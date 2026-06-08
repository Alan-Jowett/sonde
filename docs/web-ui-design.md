<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Design

> **Document status:** Draft  
> **Scope:** Architecture and implementation design for the Sonde Web UI.  
> **Audience:** Implementers (human or LLM agent) building the web UI and supporting Azure infrastructure.  
> **Related:** [web-ui-requirements.md](web-ui-requirements.md), [web-ui-validation.md](web-ui-validation.md), [gateway-design.md](gateway-design.md)

---

## 1. Overview

Static SPA hosted on GitHub Pages (deployed via GitHub Actions using `actions/deploy-pages`). Vanilla HTML/JS/CSS with zero build step. Communicates directly with Azure Storage Tables via REST API using MSAL.js bearer tokens. Program ingestion is delegated to an HTTP-triggered Azure Function that runs Prevail verification server-side. Environment configuration (Azure backend connection details) is managed at runtime via `localStorage` — no deploy-time configuration file is needed.

---

## 2. Component Architecture

```
Browser (SPA)
├── Dashboard (read actualstate table)
├── Desired State (read/write desiredstate table)
├── Program Upload (POST ELF to ProgramIngest function)
├── Program List (read programs table)
└── Sensor Data (read sensordata table, time-series graph)
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
- Silent token renewal via `acquireTokenSilent`; falls back to `acquireTokenPopup` on failure.
- `redirectUri` explicitly set to `window.location.origin` plus the normalized
  directory path (stripping any filename component like `index.html`) so the
  registered redirect URIs (`https://alan-jowett.github.io/sonde/` and
  `https://sondeplatform.com/`) match regardless of which hostname or base path
  the user accesses. This is necessary for GitHub Pages project sites where the
  origin alone (`https://alan-jowett.github.io`) does not match the registered URI.
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
(`sondeplatform.com`) can be configured via the repository's GitHub Pages
settings (Settings → Pages → Custom domain). GitHub manages the DNS verification
and TLS certificate automatically.

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
registered via CLI (`az rest PATCH`) during bootstrap.

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
   `https://alan-jowett.github.io` and `https://sondeplatform.com`
   (via `corsAllowedOrigins` parameter — origins only, no path component).

2. **SPA redirect URIs** on the Entra app (configured via CLI/Graph API
   during bootstrap):
   `https://alan-jowett.github.io/sonde/` and `https://sondeplatform.com/`
   (full URL with trailing slash).

Both CORS origins are parameterized via `githubPagesOrigin` and
`customDomainOrigin` parameters in `main.bicep`, with Sonde-specific defaults.
SPA redirect URIs are configured via CLI using `SONDE_AZURE_GITHUB_PAGES_ORIGIN`,
`SONDE_AZURE_GITHUB_PAGES_PATH`, and `SONDE_AZURE_CUSTOM_DOMAIN_ORIGIN`
environment variables. `SONDE_AZURE_CUSTOM_DOMAIN_ORIGIN` defaults to
`https://sondeplatform.com` (matching the Bicep `customDomainOrigin` parameter
default); set to the empty string to omit the custom domain redirect URI.

---

## 10. Sensor Data (WEB-0700)

> **Requirements:** WEB-0700, WEB-0701, WEB-0702, WEB-0703, WEB-0704

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
- Time range selector: Last 1h, 24h, 7d.
- Auto-refresh toggle with configurable interval (default 30s).
- Hover tooltip: timestamp, node ID, reading name, value.
- Series selector: checkboxes to choose which (node, program, reading)
  combinations to display.

**Scale constraints:**
- Maximum 20 concurrent lines on the graph. If more combinations exist,
  the admin selects which to display via the series selector.
- Maximum 1000 rows per query (`$top=1000`). For longer time ranges,
  the SPA downsamples client-side: fetch up to 1000 rows for the
  requested window, then downsample to a maximum of 500 points per
  series (e.g., divide the time range into 500 equal buckets and
  pick one representative point per bucket).
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

### 10.5  Sensor data export (WEB-0704)

The Sensor Data tab includes a separate export panel for downloading sensor
rows over a custom start/end time range without changing the graph/table
display state.

**Controls:**
- Start time and end time inputs (`datetime-local`)
- Format selector with `.jsonl` and `.csv`
- Export button

**Behavior:**
- The export range is validated client-side before querying. Missing values or
  `start > end` are rejected with an inline error message and no network call.
- Export scope is **all sensor-data rows in the chosen export range** across
  all known node partitions. It does not depend on the graph view mode, graph
  preset range, or series picker selection.
- Export queries reuse the same authenticated Azure Tables access model as the
  rest of the SPA. For each node partition, the SPA issues a filtered range
  query and follows Azure Table continuation tokens until the partition's
  matching rows are exhausted.
- The export action surfaces success/failure status in the Sensor Data view.

**File formats:**
- **CSV:** Header row
  `timestamp_ms,node_id,program_hash,raw_payload,decoded_readings_json`. The
  `decoded_readings_json` column contains the original JSON string from the
  table row, or the empty string when the source row has no decoded readings.
- **JSONL:** One JSON object per line with keys `timestamp_ms`, `node_id`,
  `program_hash`, `raw_payload`, and `decoded_readings`. The
  `decoded_readings` value is the parsed JSON object when present, otherwise
  `null`.

---

## 11. Environment Manager (WEB-0800)

> **Requirements:** WEB-0800, WEB-0801, WEB-0802, WEB-0803, WEB-0804, WEB-0805, WEB-0806, WEB-0807, WEB-0808

### 11.1 Overview

The environment manager replaces the deploy-time `config.json` with a runtime
configuration system. Users define named environments (e.g., "production",
"staging", "dev") with the Azure backend connection details needed by the SPA.
A single SPA instance can connect to any environment without redeployment.
Environments can be imported from JSON files (e.g., the `web-ui-environment.json`
emitted by Azure companion bootstrap) and exported for backup or sharing.

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

### 11.2b Import/Export File Schema (WEB-0807, WEB-0808)

The import/export file uses a single-object JSON schema with a `version` field
for forward compatibility:

```json
{
  "version": 1,
  "name": "production",
  "clientId": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "tenantId": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "storageAccount": "mystorageaccount",
  "functionAppName": "sonde-decoder-xxxx"
}
```

The `version` field MUST be integer `1`. Files with any other version value are
rejected. Extra properties beyond the defined schema are silently ignored to
allow forward-compatible extensions.

The Azure companion bootstrap emits this file as `web-ui-environment.json` with
`name` set to empty string — the SPA prompts the user for a name during import.

### 11.3 Authority Derivation

`msalAuthority` is derived from the tenant ID as:
`https://login.microsoftonline.com/<tenantId>`

This targets Azure public cloud. Sovereign cloud support is out of scope.

### 11.4 UI Design

**First load (no environments):** A full-screen modal prompts the user to add
their first environment. The main app UI (tabs, dashboard) is inaccessible until
at least one environment is configured. The modal cannot be closed without adding
an environment. The modal includes both "Add Environment" and "Import" buttons.

**Environment list modal:** A full-screen modal displaying all configured
environments in a table (Name, Storage Account, Function App), with action
buttons: Use (switch to this environment), Export, Edit, Delete. The active
environment is marked with a badge. "Add Environment" and "Import" buttons are
shown below the table.

**Add/edit form:** A stacked form with fields for Name (read-only on edit),
Client ID, Tenant ID, Storage Account, Function App Name. All fields are
required. Duplicate names are rejected on add.

**Import flow (WEB-0807):** Clicking Import opens a browser file picker
accepting `.json` files. After selection:

1. Parse the file as JSON. Reject non-JSON or non-object content with an error.
2. Validate `version === 1`. Reject other values with "Unsupported environment
   file version" error.
3. Validate the four data fields using the same rules as the manual form
   (WEB-0802).
4. If `name` is blank/missing, show a name input prompt before saving.
5. If `name` matches an existing environment, show a conflict dialog offering
   "Overwrite" or "Rename" options.
6. If overwriting the active environment, trigger the full re-initialization
   sequence (WEB-0806): clear MSAL state, re-create MSAL instance, re-render.
7. Save the environment to `localStorage` and refresh the environment list.

**Export (WEB-0808):** Each row's Export button serializes the environment to a
JSON file using the import/export schema (§11.2b) and triggers a browser
download. The filename is derived from the environment name with characters
unsafe for filesystems (slashes, colons, control characters) replaced by
hyphens. If the sanitized name is empty, the fallback `sonde-environment.json`
is used.

**Header indicator:** The active environment name is displayed in the top bar
next to a ⚙ gear button that opens the environment manager modal.

### 11.5 Environment Switching (WEB-0806)

When the user switches to a different environment:

1. The auto-refresh timer is cleared
2. `CONFIG` fields are updated from the selected environment
3. The MSAL `PublicClientApplication` instance is discarded
4. The active MSAL account is cleared
5. MSAL-related `sessionStorage` keys are cleared (keys starting with `msal.` or containing `.login.` or `.acquireToken.`) — other session data is preserved
6. A new MSAL instance is initialized with the new environment's credentials
7. The active tab is re-rendered

---

## 12. Cross-Cutting Concerns

### 12.1 HTML Output Escaping (WEB-CC-02)

All user-supplied and server-sourced values rendered into HTML MUST pass
through an `escapeHtml()` function before insertion. The function replaces
the following five characters with their HTML entity equivalents:

| Character | Entity |
|-----------|--------|
| `&` | `&amp;` |
| `<` | `&lt;` |
| `>` | `&gt;` |
| `"` | `&quot;` |
| `'` | `&#39;` |

Replacement MUST be applied in the order shown (ampersand first) to avoid
double-encoding. Every code path that builds HTML from dynamic values —
including table cells, tooltips, modal content, and status indicators —
MUST route through `escapeHtml()`. Raw string interpolation into `innerHTML`
is prohibited for any value originating from Azure Table responses, URL
parameters, or user input fields.

### 12.2 MSAL Hash Routing Compatibility (WEB-CC-03)

The SPA uses URL hash fragments for tab routing (e.g., `#dashboard`,
`#programs`). MSAL.js also uses hash fragments for authorization code
responses (e.g., `#code=…`). To prevent conflicts:

1. **Before MSAL init:** If the current URL hash does not contain
   MSAL-related tokens (`code=`, `error=`, `access_token=`), the hash is
   temporarily saved and cleared via `history.replaceState()` so MSAL does
   not misinterpret a routing hash as an auth response.
2. **After MSAL processes the redirect:** The saved routing hash is restored
   via `history.replaceState()` (not `window.location.hash`, which would
   add a spurious history entry).
3. **Auth hashes** (containing `code=`, `error=`, or `access_token=`) are
   left intact for MSAL to consume.

### 12.3 Popup Window Detection (WEB-CC-04)

When MSAL.js opens a popup for interactive login, the SPA is loaded again
inside the popup window. To avoid unnecessary API calls and duplicate
rendering in the popup:

- At `DOMContentLoaded`, the SPA checks `window.opener && window.opener !== window`.
- If the check is true, the SPA skips calling `init()` entirely — no MSAL
  initialization, no table queries, no UI rendering.
- The popup page exists solely to receive the auth redirect and relay the
  result back to the parent window via MSAL's internal messaging.

---

## 13. Key Management (WEB-1000)

### 13.1 Gateway Status Card

- Reads the `actualstate` table for rows whose `PartitionKey` starts with `g:`.
- Cross-references the `desiredstate` table for gateway rows (`PartitionKey`
  starting with `g:`) to compute a convergence badge (see §13.1.1).
- Renders a dedicated gateway status card above the node table, showing the
  BIP-39 fingerprint, `master_key_epoch`, `master_key_id`, salt status,
  `rotation_in_progress`, `gateway_version`, `modem_firmware_version`,
  `channel`, and an **Aligned / Diverged** convergence badge.
- **Fingerprint computation is local:** The SPA MUST compute the 6-word BIP-39
  fingerprint from `x25519_public_key` using SHA-256 + 66-bit extraction +
  BIP-39 wordlist lookup. The SPA MUST NOT use the `fingerprint_words` field
  stored in Azure — a compromised Azure could substitute a rogue public key
  with pre-matched fingerprint words, defeating the verification. The admin
  compares the SPA-computed fingerprint against the modem display.
- The BIP-39 English wordlist (2048 words) is embedded in the SPA or loaded
  from CDN. The fingerprint algorithm: `SHA-256(x25519_public_key)` →
  take first 66 bits → split into 6 × 11-bit indices → map to BIP-39 words.
- Uses the same MSAL.js bearer token acquisition path as node `actualstate`
  queries.
- The `renderGatewayStatusCard` function receives both gateway actual-state
  rows and gateway desired-state rows (filtered via `filterGatewayRows`).

#### 13.1.1 Gateway Convergence Rules

Gateway convergence mirrors the node divergence pattern (§4) but uses
gateway-specific fields. The convergence badge is computed per gateway by
comparing the latest gateway desired-state row (from `desiredstate`, filtered
by `PartitionKey` starting with `g:`, deduplicated via `latestByPartition`)
against the gateway actual-state row.

**Divergence conditions** (any true → "Diverged"):

1. **Rotation payload pending:** The desired row has a non-null
   `rotation_payload` AND the actual `rotation_in_progress` is not `true`
   AND `actual.master_key_epoch <= desired.submitted_epoch`.
   - The `submitted_epoch` field is stored in the desired-state row at
     submission time (see §13.4). This enables precise comparison without
     timestamp heuristics.
   - Once `rotation_in_progress` becomes `true` or
     `actual.master_key_epoch > desired.submitted_epoch`, the rotation is
     considered consumed and this condition clears.
2. **Channel mismatch:** Desired `channel` is non-null and differs from
   actual `channel`.
3. **Salt pending adoption:** Desired `salt` is non-null AND actual `salt`
   is null/absent. Once the gateway has a salt, it is immutable except via
   rotation payload — a mismatched desired `salt` against an existing
   actual `salt` is a no-op by gateway design (set-if-absent semantics)
   and does NOT flag divergence.
4. **KDF params pending adoption:** Desired `kdf_params`/`kdf_params_json` is
   non-null AND actual `kdf_params_json` is null/absent. Same set-if-absent
   semantics as salt.

**No desired row:** If no gateway desired-state row exists, the gateway is
"Aligned" (no pending changes — same as unmanaged nodes in §4).

### 13.2 Rotation Form

- Inline collapsible form within the gateway status card (replaces the
  previous modal dialog):
  1. Fingerprint verification against the modem
  2. Rotation code input (`text`, 6 chars, auto-uppercase)
  3. Passphrase input (`password` field)
  4. Submit button
- The `Rotate Key` button toggles the form's visibility. When expanded, the
  form appears below the gateway status fields within the same card panel.
- Displays the current salt from gateway ACTUAL_STATE, or a `first rotation`
  indicator when no salt exists yet.
- Shows a progress indicator during Argon2id key derivation because the WASM
  KDF may take ~1–3 seconds.
- After successful submission, the form displays "Rotation submitted" and
  collapses. No inline polling is performed — the convergence badge
  (§13.1.1) reflects rotation status via the dashboard auto-refresh cycle.
- Dashboard auto-refresh (§4, WEB-0103) is paused while the rotation form
  is expanded or a submission is in progress. This prevents DOM replacement
  from destroying form state or interrupting Argon2id key derivation.
  Auto-refresh resumes when the form collapses.

### 13.3 Crypto Pipeline

- **Argon2id:** `argon2-browser` WASM loaded from CDN, e.g.
  `https://cdn.jsdelivr.net/npm/argon2-browser@1.18.0/dist/argon2-bundled.min.js`
- **X25519:** `@noble/curves` loaded from CDN, e.g.
  `https://cdn.jsdelivr.net/npm/@noble/curves@1.8.1/ed25519.js`, providing the
  X25519 operations needed for the rotation flow
- **HKDF-SHA-256:** Web Crypto `importKey` + `deriveBits` with the HKDF algorithm
- **AES-256-GCM:** Web Crypto `encrypt` with the AES-GCM algorithm
- **CBOR encoding:** Lightweight inline encoder for the plaintext map with five
  integer-keyed entries
- **Random values:** `crypto.getRandomValues()` for the nonce and
  `master_key_id`

### 13.4 Azure Table Integration

- Gateway ACTUAL_STATE read:
  `GET /actualstate?$filter=PartitionKey ge 'g:' and PartitionKey lt 'g;'`
  The SPA uses `latestByPartition` to select the latest row per gateway
  partition (lexicographically smallest reverse-timestamp `RowKey`).
  This returns all gateway history rows; `latestByPartition` deduplicates
  to one row per gateway.
- Gateway discovery query:
  `GET /actualstate?$filter=PartitionKey ge 'g:' and PartitionKey lt 'g;'`
- DESIRED_STATE write:
  `POST /desiredstate` with `rotation_payload` serialized as `Edm.Binary`
  and `submitted_epoch` as `Edm.Int64` (set to the gateway's current
  `master_key_epoch` at submission time, for convergence tracking per
  §13.1.1)
- Gateway DESIRED_STATE `PartitionKey` is `"g:" + gateway_id_hex`
- Gateway DESIRED_STATE `RowKey` uses the same reverse-timestamp format as node
  desired state rows

### 13.5 Dashboard Filtering

- Existing dashboard node-table queries and node dropdown population MUST filter
  to node entities only, either by `PartitionKey` starting with `n:` or by an
  equivalent `entity_kind = "node"` discriminator if present.
- Gateway entities are read separately for the gateway status card and never
  rendered in the node table or node dropdown.

### 13.6 Dependencies

- `argon2-browser` WASM loaded from CDN with an SRI hash
- `@noble/curves` (or an equivalent noble X25519 package) loaded from CDN with
  an SRI hash
- No build step; dependencies are referenced directly from HTML `<script>` tags

### 13.7 Browser Compatibility

- Rotation requires Web Crypto support for HKDF and AES-GCM plus WebAssembly
  support for Argon2id.
- If the required capabilities are unavailable, the `Rotate Key` button is
  disabled and a tooltip explains the missing browser requirement.

---

## 14. Revision History

| Date | Author | Description |
|------|--------|-------------|
| 2026-05-29 | Issue #1092 | Added §13.1.1 gateway convergence rules. Replaced §13.2 rotation modal with inline form. Added convergence badge to §13.1. |
| 2026-05-19 | Trifecta remediation (#1012) | Added §12 (cross-cutting concerns: HTML escaping, MSAL hash routing, popup detection). Fixed §10.2 downsample cap to 500 points per series. |
