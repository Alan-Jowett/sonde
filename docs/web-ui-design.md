<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Design

> **Document status:** Draft  
> **Scope:** Architecture and implementation design for the Sonde Web UI.  
> **Audience:** Implementers (human or LLM agent) building the web UI and supporting Azure infrastructure.  
> **Related:** [gateway-design.md](gateway-design.md), [gateway-validation.md](gateway-validation.md)

---

## 1. Overview

Static SPA hosted on GitHub Pages (deployed from the `gh-pages` branch of the sonde repository). Vanilla HTML/JS/CSS with zero build step. Communicates directly with Azure Storage Tables via REST API using MSAL.js bearer tokens. Program ingestion is delegated to an HTTP-triggered Azure Function that runs Prevail verification server-side. Environment configuration (Azure backend connection details) is managed at runtime via `localStorage` — no deploy-time configuration file is needed.

---

## 2. Component Architecture

```
Browser (SPA)
├── Dashboard (read actualstate table)
├── Desired State (read/write desiredstate table)
├── Program Upload (POST ELF to ProgramIngest function)
├── Program List (read programs + programroute tables)
└── Sensor Data (read SensorData table, time-series graph)
     │
     │ MSAL.js Bearer Token
     ▼
Azure Storage Tables + ProgramIngest Azure Function
```

---

## 3. File Structure

```
deploy/web-ui/
  index.html                  — single-page app shell
  app.js                      — application logic (MSAL, table queries, UI rendering, environment manager)
  style.css                   — minimal styling
```

---

## 4. Node Dashboard (WEB-0100)

- Queries `actualstate` Azure Table via REST API.
- Groups entities by `PartitionKey` (one per node), displays most recent row (smallest `RowKey` due to reverse-timestamp ordering).
- Cross-references `desiredstate` table to compute divergence indicators:
  - **Program divergence**: When a desired-state row exists for the node, divergence is flagged if the desired program hash differs from the actual current program hash. Missing, null, or empty `desired_assigned_program_hash` is treated as "no program desired" — so a node that still reports a current program hash is diverged until it confirms clearing. When no desired-state row exists at all, program divergence is not flagged (node is unmanaged).
  - **Schedule divergence**: Flagged when `desired_schedule_interval_s` is set and differs from `observed_schedule_interval_s`.
- Auto-refresh every 30s by default (configurable).
- Columns: Node ID, Battery (mV), Firmware, ABI Version, Schedule (s), Current Program Hash, Assigned Program Hash, Last Seen, Status (aligned/diverged).

---

## 5. Desired State Management (WEB-0200)

- Form: Node ID (dropdown), Schedule Interval (number, seconds), Program Hash (dropdown from `programs` table).
- **Node ID dropdown (WEB-0206):** The Node ID field is a `<select>` populated from nodes that have reported actual state (i.e., rows in the `actualstate` table, deduplicated via `latestByPartition`). A placeholder `<option>` prompts the operator to select a node. Free-text entry is not supported — only nodes known to the gateway appear; arbitrary node IDs cannot be entered or submitted.
- **Auto-populate on selection (WEB-0206, WEB-0207):** When the operator selects a node, the Schedule Interval and Program Hash fields are pre-populated. The latest row per node is used (same `latestByPartition` dedup as the dashboard). Priority order:
  1. **Existing desired state** for the selected node (latest row from `desiredstate` table) — `desired_schedule_interval_s` and `desired_assigned_program_hash`.
  2. **Last reported actual state** — `observed_schedule_interval_s` and `observed_assigned_program_hash`.
  3. **Empty** — if neither source has a value for a given field.
- If the pre-populated program hash is not present in the Program Hash dropdown (i.e., the program was deleted from the `programs` table), the Program Hash field is left at the default "No program target" option.
- On submit:
  - `PartitionKey`: `"n:" + SHA-256(node_id).hex()` using `SubtleCrypto`
  - `RowKey`: reverse-timestamp format `{(u64::MAX - timestamp_ms):016x}:{(u64::MAX - sequence):016x}:{random_nonce:016x}`
  - `timestamp_ms` stored as `Edm.Int64`
  - `POST`s entity to `desiredstate` table via Azure Tables REST API

---

## 6. Program Ingest (WEB-0300)

### 6.1 Azure Function: ProgramIngest

- HTTP trigger, `POST`, route `api/programs/ingest`
- Request: `application/json` with fields:
  - `elf` (string, required): Base64-encoded ELF binary
  - `source_filename` (string, optional): Original filename for display
  - `abi_version` (integer, optional): ABI version the program targets
  - `verification_profile` (string, optional): `"resident"` (default) or `"ephemeral"`
- Processing (reuses `sonde-gateway` `ProgramLibrary`):
  1. Parse JSON body from the Azure Functions HTTP trigger envelope (`Data.req.Body`)
  2. Base64-decode the `elf` field; reject if missing, empty, or invalid base64
  3. Reject requests where decoded ELF exceeds 1 MB (defense-in-depth before verification)
  4. Parse `verification_profile`; default to `Resident` if absent
  5. Call `ProgramLibrary::ingest_elf(elf_bytes, profile)` — this verifies with Prevail, extracts bytecode + maps, encodes deterministic CBOR, and computes SHA-256
  6. Normalize `source_filename` via `normalize_display_filename()`
  7. Store in `programs` Azure Table via `HandlerStore::store_program_image()`
  8. Return JSON: `{"program_hash": "hex", "size": N, "abi_version": N, "source_filename": "name"}`
  9. On failure: return JSON error with diagnostics
- The uploaded ELF is verified and transformed; the cloud stores both the deterministic CBOR program image (for hash computation and node delivery) and the original ELF binary (for embedding in `DESIRED_STATE` messages so gateways can re-verify locally)
- `programs` table schema: `PartitionKey="program"`, `RowKey=hex(program_hash)`, `source_filename`, `abi_version` (`Edm.Int32`), `cbor_image` (base64-encoded CBOR program image), `elf_image` (base64-encoded original ELF binary), `size_bytes` (`Edm.Int32`, CBOR image byte length), `verification_profile`, `created_at` (ISO 8601 UTC string)
- Idempotent: re-ingesting the same ELF produces the same program hash; all metadata fields (`source_filename`, `abi_version`, `verification_profile`, `created_at`) are overwritten on re-ingest (last-writer-wins)
- Error responses use HTTP status codes: 400 (malformed JSON, missing `elf` field, invalid base64), 413 (ELF exceeds size limit), 422 (Prevail verification failure, invalid ELF), 500 (storage/internal error)

#### 6.1.1 Custom Handler Routing

The Azure Functions custom handler (`main.rs`) routes `/ProgramIngest` to a
dedicated `handle_program_ingest` handler, separate from the catch-all
`/{*path}` route used for queue-triggered connector messages. The handler:

1. Extracts the HTTP trigger envelope from the Azure Functions invocation request
2. Reads `Data.req.Body` (JSON string)
3. Parses the JSON body to extract `elf`, `source_filename`, `abi_version`, `verification_profile`
4. Processes the program through `ProgramLibrary`
5. Returns an Azure Functions HTTP output binding response:
   ```json
   {
     "Outputs": {
       "res": {
         "statusCode": 200,
         "headers": {"Content-Type": "application/json"},
         "body": "{\"program_hash\":\"hex\",\"size\":N,\"abi_version\":N,\"source_filename\":\"name\"}"
       }
     }
   }
   ```

#### 6.1.2 HandlerStore Expansion

The `HandlerStore` trait gains:

```rust
async fn store_program_image(&self, row: &ProgramImageRow) -> Result<(), HandlerError>;
```

`ProgramImageRow` contains: `program_hash` (`Vec<u8>`), `cbor_image` (`Vec<u8>`),
`elf_image` (`Vec<u8>`), `source_filename` (`Option<String>`), `abi_version` (`Option<u32>`), `size_bytes` (`u32`),
`verification_profile` (`String`), `created_at` (`String`, ISO 8601 UTC).

`AzureTablesStore` implements this as an upsert to the `programs` table, encoding
`cbor_image` and `elf_image` as base64 and `program_hash` as hex for the `RowKey`.

### 6.2 Inline Program ELF in DESIRED_STATE (WEB-0309, WEB-0310)

When the handler publishes a `DESIRED_STATE` message due to program divergence,
it fetches the original ELF binary from the `programs` table and embeds it at
CBOR key 5 (`assigned_program_elf`, `bstr`). Keys 6–8 carry the verification
profile, source filename, and ABI version metadata. The companion forwards this
opaque payload to the gateway, which runs full Prevail verification via
`ProgramLibrary::ingest_elf()` and stores the resulting verified program in its
local `ProgramLibrary`.

---

## 7. Program List (WEB-0400)

- Queries `programs` table (`PartitionKey eq 'program'`), displays table with hash, filename, `abi_version`, size, upload time.

---

## 8. Authentication (WEB-0500)

- MSAL.js 2.x with authorization code flow + PKCE.
- Token caching in browser session storage.
- Silent token renewal; redirect to login on expiry.
- `redirectUri` explicitly set to `window.location.origin` so the registered
  redirect URIs (`https://alan-jowett.github.io/sonde/` and
  `https://sondeplatform.com`) match regardless of which hostname the user
  accesses.
- Configuration is loaded from the active environment in `localStorage`
  (see §11). `msalAuthority` is derived as
  `https://login.microsoftonline.com/<tenantId>`.
- Two token scopes, acquired separately:
  - `https://storage.azure.com/.default` — for Azure Table REST API calls
    (dashboard, desired state, program list, sensor data).
  - `api://<companionClientId>/user_impersonation` — for `ProgramIngest` Function
    App calls. This token is validated by EasyAuth on the Function App (see §9.4).
- The SPA calls `acquireTokenSilent` (with popup fallback) for the Function App
  scope before each `ProgramIngest` request and sends the token as a
  `Bearer` header.

---

## 9. Infrastructure (WEB-0600)

### 9.1 GitHub Pages Deployment (WEB-0900)

The SPA is deployed to GitHub Pages from the sonde repository. A GitHub Actions
workflow (`.github/workflows/web-ui.yml`) publishes the contents of
`deploy/web-ui/` to GitHub Pages on pushes to `main` that modify
`deploy/web-ui/**`.

The well-known URL is `https://alan-jowett.github.io/sonde/`.  A custom domain
(`sondeplatform.com`) can be configured via GitHub Pages settings and a `CNAME`
file in the deployment artifact.

No deploy-time configuration is needed — environment configuration is managed at
runtime via `localStorage` (see §11 Environment Manager).

### 9.2 Modified Bicep Modules

- `storage.bicep`: add `programs` table.
- `function-rbac.bicep`: add Storage Table Data Contributor on `programs` table.
- `stack.bicep`: wire new modules.
- `main.bicep`: add outputs.

### 9.3 Function App Changes

- `ProgramIngest/function.json` defines the HTTP trigger (`authLevel: anonymous`,
  route `programs/ingest`). Authentication is handled by EasyAuth (see §9.4),
  not by the Azure Functions runtime key mechanism.
- `main.rs` routes `/ProgramIngest` to a dedicated handler that parses the
  Azure Functions HTTP trigger envelope and delegates to
  `AzureHandler::handle_program_ingest()`. The catch-all `/{*path}` route
  continues to handle queue-triggered connector messages.
- `lib.rs` implements `handle_program_ingest()` which reuses
  `ProgramLibrary::ingest_elf()` from `sonde-gateway` for Prevail verification,
  CBOR encoding, and SHA-256 hashing.
- The HTTP trigger response uses the Azure Functions `res` output binding
  envelope format with `statusCode`, `headers`, and `body` fields.

### 9.4 EasyAuth Configuration (WEB-0606)

The Function App is configured with Azure App Service Authentication
(EasyAuth / `authSettingsV2`) to validate Entra ID bearer tokens on HTTP
routes. This replaces the previous `authLevel: "function"` API key
mechanism for the `ProgramIngest` endpoint.

**Bicep resource** (`function-placeholder.bicep`):

A `Microsoft.Web/sites/config@2024-04-01` resource named `authsettingsV2` is
added to the Function App with the following configuration:

- `platform.enabled: true`
- `globalValidation.unauthenticatedClientAction: 'Return401'` — rejects
  unauthenticated requests with HTTP 401 instead of redirecting to a login page.
- `identityProviders.azureActiveDirectory.enabled: true`
- `identityProviders.azureActiveDirectory.registration.clientId` — set to the
  companion Entra app registration's client ID (passed as a Bicep parameter).
- `identityProviders.azureActiveDirectory.registration.openIdIssuer` — set to
  `${environment().authentication.loginEndpoint}<tenantId>/v2.0` using the Bicep
  `environment()` function so the template works in sovereign clouds (Azure
  Government, Azure China, etc.).
- `identityProviders.azureActiveDirectory.validation.defaultAuthorizationPolicy.allowedApplications`
  — includes the companion client ID so the SPA's tokens (audience matching the
  client ID) are accepted.
- `identityProviders.azureActiveDirectory.validation.allowedAudiences` — includes
  both `api://<clientId>` and the bare `<clientId>`. The Application ID URI form
  matches v1 access tokens (the default); the bare client ID matches v2 access
  tokens. Including both makes EasyAuth robust against changes to the Entra app's
  `requestedAccessTokenVersion` setting.

**Queue-triggered invocations are unaffected.** Azure Functions queue triggers
are invoked by the runtime, not via HTTP. EasyAuth only applies to HTTP-triggered
routes.

**Entra app registration prerequisites:**

The Entra app registration (companion client ID) must expose an API scope
(`api://<clientId>/user_impersonation`). This is configured by the bootstrap
script inside the `sonde-azure-bootstrap` container. SPA redirect URIs are
registered declaratively in Bicep (`companion-identity.bicep`).

The scope and redirect URIs must exist before the SPA attempts to acquire
tokens for the Function App audience.

**CORS and preflight:**

EasyAuth applies only to authenticated HTTP methods. Browser preflight
(`OPTIONS`) requests are unauthenticated by design and are handled by the
Function App's CORS configuration (already provisioned in Bicep via
`corsAllowedOrigins`). The existing CORS setup passes through `OPTIONS`
preflight requests without requiring a bearer token. Only the actual
`POST` to `/api/programs/ingest` requires authentication.

**SPA scope derivation:**

The SPA derives the Function App API scope as
`api://${CONFIG.msalClientId}/user_impersonation` — it does not require
an additional `config.json` field. This works because the companion Entra
app registration is shared between the SPA and the Function App EasyAuth
configuration.

---

### 9.5 CORS and Redirect URI Configuration

The Bicep deployment configures:

1. **CORS origins** on the Function App (`function-placeholder.bicep`):
   `https://alan-jowett.github.io/sonde` and `https://sondeplatform.com`
   (via `corsAllowedOrigins` parameter).

2. **SPA redirect URIs** on the Entra app (`companion-identity.bicep`):
   `https://alan-jowett.github.io/sonde/` and `https://sondeplatform.com`
   (via `spaRedirectUris` parameter).

Both are parameterized via `githubPagesOrigin` and `customDomainOrigin`
parameters in `main.bicep`, with Sonde-specific defaults.

---

## 10. Sensor Data (WEB-0700)

> **Requirements:** WEB-0700, WEB-0701, WEB-0702, WEB-0703

### 10.1  Data source

The Sensor Data tab reads from the `SensorData` Azure Table (AZH-0500). Each
row contains `node_id`, `timestamp_ms`, `program_hash`, `raw_payload`, and
`decoded_readings` (JSON string), plus `PartitionKey` and `RowKey` for
querying. The tab parses `decoded_readings` JSON to extract reading names
and values for plotting. When `decoded_readings` is the empty string
(`""`), the row has no decoded readings — the SPA skips it for plotting
and shows "—" in table view. The SPA MUST check for empty string before
calling `JSON.parse`. Timestamps for the X-axis are derived from
`timestamp_ms`.

Data is queried using the same MSAL.js bearer token as other tabs — no
additional authentication is required. Queries use `PartitionKey` filters
(per node) and `RowKey` range (time window) with `$top=1000`. Cross-node
queries issue parallel per-node requests for each selected node's partition,
since Azure Tables do not efficiently support cross-partition range scans.

### 10.2  Time-series graph (WEB-0701)

Each unique `(NodeId, ProgramHash, ReadingName)` tuple is rendered as a
separate line on a time-series chart. The X-axis is time (derived from
`timestamp_ms`); the Y-axis is the reading value.

**Controls:**
- Time range selector: Last 1h, 24h, 7d, custom.
- Auto-refresh toggle with configurable interval (default 30s).
- Hover tooltip: timestamp, node ID, reading name, value.
- Series selector: checkboxes to choose which (node, program, reading)
  combinations to display.

**Scale constraints:**
- Maximum 20 concurrent lines on the graph. If more combinations exist,
  the admin selects which to display via the series selector.
- Maximum 1000 rows per query (`$top=1000`). For longer time ranges,
  the SPA downsamples client-side: fetch up to 1000 rows for the
  requested window, then thin the data points to a displayable density
  (e.g., pick one point per pixel or per time bucket).
- Int64 values exceeding `Number.MAX_SAFE_INTEGER` (2^53 - 1) are displayed
  as strings. The Azure handler encodes such values as JSON strings in the
  `decoded_readings` column (see AZH-0501 AC-5). The SPA MUST handle both
  numeric and string-encoded int64 values in `decoded_readings` JSON.

The chart library is vanilla JS (Canvas-based) or a lightweight dependency
(e.g., Chart.js loaded from CDN) — no build step required, consistent with
the existing zero-build SPA architecture.

### 10.3  Table view (WEB-0702)

A toggle switches between graph and table views. The table displays all
`SensorData` columns: Timestamp, Node ID, Program Hash, Decoded Readings,
Raw Payload (truncated). Sorted by timestamp descending (newest first).
Rows with empty `decoded_readings` display "—" in the Decoded Readings
column.

### 10.4  Series display customization (WEB-0703)

Each series in the series selector has a ✏️ edit button that opens a modal
dialog for customizing how the series is displayed:

- **Display name** — override the default
  `truncHash(nodeId) / truncHash(programHash) / readingName` label with a
  friendly name (e.g., "Office Temperature"). The original raw label is
  shown in a hover tooltip and in the edit dialog.
- **Scale divisor** — divide raw values by a constant before plotting.
  For example, a divisor of `1000` converts milli-degrees (21500) to
  degrees (21.5). A divisor of `0` or empty means no scaling.
- **Unit suffix** — a string appended to values in tooltips and, when all
  selected series share the same suffix, in the Y-axis title
  (e.g., `°C`, `%`, `hPa`).

A **Reset to Default** button clears overrides for the series.

#### Persistence

Overrides are stored in `localStorage` under the key
`sonde_series_overrides` as a JSON object keyed by series key
(`partitionKey|programHash|readingName`). Each entry has the shape:

```json
{ "displayName": "string", "scaleDivisor": number, "unitSuffix": "string" }
```

Overrides survive page reloads and browser restarts. They are scoped to
the browser origin (per localStorage rules) and are not shared across
devices.

---

## 11. Environment Manager (WEB-0800)

> **Requirements:** WEB-0800, WEB-0801, WEB-0802, WEB-0803, WEB-0804, WEB-0805, WEB-0806

### 11.1 Overview

The environment manager replaces the deploy-time `config.json` with a runtime
configuration system. Users define named environments (e.g., "production",
"staging", "dev") with the Azure backend connection details needed by the SPA.
A single SPA instance can connect to any environment without redeployment.

### 11.2 Data Model

Each environment is a JSON object stored in `localStorage`:

```json
{
  "name": "production",
  "clientId": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "tenantId": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "storageAccount": "mystorageaccount",
  "functionAppName": "sonde-decoder-xxxx"
}
```

**Storage keys:**

| Key | Value |
|-----|-------|
| `sonde_environments` | JSON array of environment objects |
| `sonde_active_environment` | Name of the currently active environment |

### 11.3 Authority Derivation

`msalAuthority` is derived from the tenant ID as:
`https://login.microsoftonline.com/<tenantId>`

This targets Azure public cloud. Sovereign cloud support is out of scope.

### 11.4 UI Design

**First load (no environments):** A full-screen modal prompts the user to add
their first environment. The main app UI (tabs, dashboard) is inaccessible until
at least one environment is configured. The modal cannot be closed without adding
an environment.

**Environment list modal:** A full-screen modal displaying all configured
environments in a table (Name, Storage Account, Function App), with action
buttons: Use (switch to this environment), Edit, Delete. The active environment
is marked with a badge. An "Add Environment" button opens the add/edit form.

**Add/edit form:** A stacked form with fields for Name (read-only on edit),
Client ID, Tenant ID, Storage Account, Function App Name. All fields are
required. Duplicate names are rejected on add.

**Header indicator:** The active environment name is displayed in the top bar
next to a ⚙ gear button that opens the environment manager modal.

### 11.5 Environment Switching (WEB-0806)

When the user switches to a different environment:

1. The auto-refresh timer is cleared
2. `CONFIG` fields are updated from the selected environment
3. The MSAL `PublicClientApplication` instance is discarded
4. The active MSAL account is cleared
5. `sessionStorage` is cleared (removes cached tokens)
6. A new MSAL instance is initialized with the new environment's credentials
7. The active tab is re-rendered
