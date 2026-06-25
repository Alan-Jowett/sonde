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
- When the dashboard performs a broad initial node-discovery read, it follows
  Azure Table continuation tokens until the requested discovery scope is
  exhausted before deduplicating node partitions.
- Groups entities by `PartitionKey` (one per node), displays most recent row (smallest `RowKey` due to reverse-timestamp ordering).
- Cross-references `desiredstate` table to compute divergence indicators:
  - **Program divergence**: When a desired-state row exists for the node, divergence is flagged if the desired program hash differs from the actual current program hash. Missing, null, or empty `desired_assigned_program_hash` is treated as "no program desired" — so a node that still reports a current program hash is diverged until it confirms clearing. When no desired-state row exists at all, program divergence is not flagged (node is unmanaged).
  - **Schedule divergence**: Flagged when `desired_schedule_interval_s` is set and differs from `observed_schedule_interval_s`.
- Auto-refresh every 30s by default (configurable).
- Columns: Node ID, Battery (mV), RSSI, Firmware, ABI Version, Schedule (s), Current Program Hash, Assigned Program Hash, Last Seen, Status (aligned/diverged).

### 4.1  Dashboard device-data export (WEB-0105)

The Dashboard includes a separate export panel for downloading historical
device-data rows from the append-only `actualstate` table. This is an
export-only diagnostics feature; it does not add a second dashboard view or a
new Azure table.

**Controls:**
- Start time and end time inputs (`datetime-local`)
- Format selector with `.jsonl` and `.csv`
- Export button

**Behavior:**
- The export range is validated client-side before querying. Missing values or
  `start > end` are rejected with an inline error message and no network call.
- Export scope is **all matching historical actual-state rows in the chosen
  export range** across all known node partitions. It is not limited to the
  latest row per node shown in the dashboard table.
- Export queries reuse the same authenticated Azure Tables access model as the
  rest of the SPA. For each node partition, the SPA issues a filtered range
  query against `actualstate` and follows Azure Table continuation tokens until
  the partition's matching rows are exhausted.
- The export action surfaces success/failure status in the Dashboard view.
- Dashboard auto-refresh continues to own the latest-state table only. The
  export operation snapshots the operator-selected time range and does not
  derive its result set from the deduplicated dashboard rows.

**File formats:**
- **CSV:** Header row
  `timestamp_ms,node_id,battery_mv,wake_rssi_dbm,firmware_version,firmware_abi_version,observed_schedule_interval_s,observed_current_program_hash,observed_assigned_program_hash`.
  Missing optional fields are written as empty fields.
- **JSONL:** One JSON object per line with keys `timestamp_ms`, `node_id`,
  `battery_mv`, `wake_rssi_dbm`, `firmware_version`, `firmware_abi_version`,
  `observed_schedule_interval_s`, `observed_current_program_hash`, and
  `observed_assigned_program_hash`. Missing optional fields are written as
  `null`.

---

## 5. Desired State Management (WEB-0200)

- Form: Node ID (dropdown), Schedule Interval (number, seconds), Program Hash (dropdown from `programs` table).
- **Node ID dropdown (WEB-0206):** The Node ID field is a `<select>` populated from nodes that have reported actual state (i.e., rows in the `actualstate` table, deduplicated via `latestByPartition`). A placeholder `<option>` prompts the operator to select a node. Free-text entry is not supported — only nodes known to the gateway appear; arbitrary node IDs cannot be entered or submitted.
- The backing `actualstate` discovery read for the dropdown follows Azure Table
  continuation tokens until the active discovery scope is exhausted before the
  options are derived, so later pages cannot silently hide live nodes.
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

Overrides are stored in the active environment's `sensorData.seriesOverrides`
map as a JSON object keyed by series key
(`partitionKey|programHash|readingName`). Each entry has the shape:

```json
{ "displayName": "string", "scaleDivisor": number, "unitSuffix": "string" }
```

Overrides survive page reloads and browser restarts. They are scoped to the
active environment and round-trip through the environment import/export schema.

### 10.4b  Sensor Data preference persistence (WEB-0705)

The Sensor Data tab persists operator preferences as part of the active
environment record rather than in a global Sensor Data storage bucket.

**Persisted fields:**
- `viewMode` — `graph` or `table`
- `timeRange` — one of the supported preset ranges (`1h`, `24h`, `7d`)
- `selectedSeries` — array of series keys
  (`partitionKey|programHash|readingName`)
- `selectedSeriesInitialized` — internal local-storage-only boolean that
  distinguishes "no explicit series selection has been saved yet" from
  "the operator intentionally saved an empty selection"
- `seriesOverrides` — the WEB-0703 per-series override map

**Behavior:**
- Sensor Data preference changes are saved back into the active environment's
  object in `sonde_environments`.
- When the SPA loads or the user switches environments, the Sensor Data tab
  restores the active environment's saved preferences before rendering.
- Export/import distinguishes between omitted and empty `selectedSeries`:
  omitting the field means "no explicit selection has been saved yet, so use the
  default initial auto-selection behavior", while `selectedSeries: []` means
  "preserve an intentionally empty graph selection".
- If a saved `selectedSeries` entry does not correspond to any currently
  available series, the renderer prunes it without error.
- Export-form fields (`start`, `end`, `format`), status banners, and other
  transient Sensor Data UI state are not persisted.

**Legacy migration:**
- On the first run after this feature ships, if the active environment has no
  `sensorData.seriesOverrides` data but the legacy global
  `sonde_series_overrides` key exists, the SPA copies those overrides into the
  active environment and then stops using the legacy global key for normal
  reads/writes.

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

### 10.6 Session telemetry cache (WEB-0706)

Normal rendering paths share a session-scoped in-memory telemetry cache. The
cache is an optimization layer over Azure Tables; it does not change rendered
semantics, does not become a new source of truth, and is not persisted to
`localStorage`.

**Scope:**
- Applies to normal rendering and discovery paths that read `actualstate` or
  `sensordata`:
  - Dashboard latest-state rendering
  - Desired State node discovery / node dropdown population
  - Sensor Data graph/table rendering
  - Dashboards variable reading-type discovery and metric evaluation
- Does **not** apply to historical export actions. Device-data export and
  sensor-data export continue to issue direct completeness-first Azure Table
  queries for the operator-selected range.

**Environment isolation:**
- Cache entries are scoped to the active environment.
- Switching environments discards or fully isolates cached telemetry from the
  previous environment before the new environment renders.

**ActualState cache model:**
- Maintain an in-memory row map keyed by `PartitionKey + "|" + RowKey`.
- Maintain a derived latest-row map keyed by `PartitionKey` for fast node and
  gateway lookups.
- Maintain a global watermark representing the newest `actualstate` history row
  already incorporated into the cache.
- Maintain an in-flight request registry keyed by the active environment and
  normalized `actualstate` request scope (initial full scan vs. bounded delta
  refresh window).
- Initial session hydration may query `actualstate` broadly enough to discover
  the current node set.
- While that broad hydration is in progress, follow Azure Table continuation
  tokens until the requested discovery scope is complete before marking the
  cache loaded or deriving the global latest-by-partition view.
- If another consumer requests the same `actualstate` scope while that fetch is
  still running, return the existing in-flight promise instead of starting a
  second Azure Table request.
- Subsequent refreshes issue a global `actualstate` delta query bounded to rows
  newer than the watermark, merge returned rows into the row map, update the
  per-partition latest-row view, and surface newly seen node partitions.
- Remove the in-flight entry when the shared request settles so later refreshes
  can observe newer bounds or retry failures.

**SensorData cache model:**
- Maintain per-partition row maps keyed by `PartitionKey + "|" + RowKey`.
- Track covered time bounds per partition so the SPA knows whether a requested
  range is already satisfied from cache.
- Maintain an in-flight request registry keyed by active environment plus the
  normalized `sensordata` request identity (`partitionKey`, `startMs`, `endMs`,
  and any query options that affect completeness, such as paging limits).
- When a render requests a time range fully covered by cache, reuse cached rows.
- When the request extends to newer data, fetch only the uncovered newer tail
  when possible, merge by row identity, and extend coverage.
- When the request expands to older historical data, fetch only the uncovered
  older interval and merge it with existing cached rows.
- If another consumer requests the same uncovered `sensordata` scope while the
  first request is still in flight, return the existing in-flight promise for
  that request identity instead of issuing a duplicate query.
- Remove the in-flight entry when the shared request settles so retries and
  wider follow-on ranges are evaluated against the updated cache state.

**Consumer behavior:**
- Consumers request telemetry from a shared cache service rather than calling
  Azure Table queries independently.
- If multiple consumers in the same session need overlapping telemetry, they
  share the same cached backing rows.
- If multiple consumers in the same session ask for an identical telemetry
  scope before the cache is warm, they also share the same in-flight request.
- Dashboards metric evaluation resolves shared variable data from the cache once
  per render/time-range context, then reuses that materialized telemetry across
  all metrics that reference the same node partitions.

**Correctness constraints:**
- Cache merges deduplicate by stable Azure Table row identity
  (`PartitionKey`, `RowKey`).
- Delta refresh must not drop newer rows that arrive after the previous render.
- Failed in-flight requests are not retained as satisfied cache entries; later
  callers must be able to retry the Azure query.
- Cache misses and fetch failures surface through the existing user-visible
  loading/error states; the cache must not silently mask network failures.

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
  "functionAppName": "sonde-decoder-xxxx",
  "sensorData": {
    "viewMode": "graph",
    "timeRange": "24h",
    "selectedSeries": [],
    "selectedSeriesInitialized": false,
    "seriesOverrides": {}
  }
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
  "functionAppName": "sonde-decoder-xxxx",
  "sensorData": {
    "viewMode": "graph",
    "timeRange": "24h",
    "selectedSeries": [],
    "seriesOverrides": {}
  }
}
```

The `version` field MUST be integer `1`. Files with any other version value are
rejected. The `sensorData` object is optional on import for backward
compatibility with previously exported files and Azure companion bootstrap
output; when omitted, the SPA uses default Sensor Data preferences. Within
`sensorData`, `selectedSeries` is also optional: omission means no explicit
selection has been saved yet, while an empty array preserves an intentional
empty selection. The local-storage-only `selectedSeriesInitialized` flag is not
serialized into the import/export file; omission versus presence of
`selectedSeries` carries that distinction on the wire. Extra properties beyond
the defined schema are silently ignored to allow forward-compatible extensions.

The Azure companion bootstrap emits this file as `web-ui-environment.json` with
`name` set to empty string and no `sensorData` object — the SPA prompts the
user for a name during import and fills in default Sensor Data preferences.

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
4. If a `sensorData` object is present, validate `viewMode`, `timeRange`,
   `selectedSeries`, and each `seriesOverrides` entry. Reject invalid
   preference shapes instead of partially importing them.
5. If `sensorData` is absent, initialize it to the default preference object.
6. If `name` is blank/missing, show a name input prompt before saving.
7. If `name` matches an existing environment, show a conflict dialog offering
   "Overwrite" or "Rename" options.
8. Import overwrite semantics are replace, not merge: the imported
   `sensorData` object replaces the destination environment's saved Sensor Data
   preferences.
9. If overwriting the active environment, trigger the full re-initialization
   sequence (WEB-0806): clear MSAL state, re-create MSAL instance, re-render.
10. Save the environment to `localStorage` and refresh the environment list.

**Export (WEB-0808):** Each row's Export button serializes the environment to a
JSON file using the import/export schema (§11.2b) and triggers a browser
download. The filename is derived from the environment name with characters
unsafe for filesystems (slashes, colons, control characters) replaced by
hyphens. If the sanitized name is empty, the fallback `sonde-environment.json`
is used. Export always includes the environment's current `sensorData`
preferences object.

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
7. The active environment's `sensorData` preferences are loaded into the Sensor
   Data view state
8. The active tab is re-rendered

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

## 14. Custom Dashboards (WEB-1100)

The Dashboards feature allows operators to create custom visualizations by
binding sensor data sources to variables and defining computed metrics using
algebraic expressions. Each environment can have multiple dashboards; each
dashboard contains multiple named charts; each chart can display multiple
metrics as overlaid time-series datasets.

### 14.1 UI Structure

```
Dashboards Section
├── Dashboard Tabs [Tab 1] [Tab 2] [+]
├── Active Dashboard View
│   ├── Variables Panel
│   │   ├── Collapsible header
│   │   ├── Variable List (name → data source mapping)
│   │   └── [+ Add Variable] button
│   ├── Charts Panel
│   │   ├── Chart 1
│   │   │   ├── Chart header (name, rename/delete actions, metrics toggle)
│   │   │   ├── Collapsible Metrics section
│   │   │   │   ├── Metric list (dataset definitions)
│   │   │   │   └── [+ Add Metric] button
│   │   │   ├── Shared chart canvas
│   │   ├── Chart 2
│   │   └── [+ Add Chart] button
│   └── Time Range Selector (shared across all charts)
└── Dashboard Management (rename, delete)
```

**Navigation:**
- Dashboard tabs appear horizontally at the top of the Dashboards section.
- "+" tab button creates a new dashboard (prompts for name).
- Active dashboard is highlighted.
- Each dashboard displays its variables and charts in a vertical layout.
- Variables Panel is expanded by default and can be collapsed from its header.
- Each chart's Metrics section is expanded by default and can be collapsed
  independently without hiding the chart canvas.

**Empty State:**
- New dashboards show: "No charts yet. Click '+ Add Chart' to get started."
- Dashboards with no variables show a prompt to add variables before creating
  metrics within charts.

**Limits (WEB-1110):**
- Soft limit: 20 dashboards per environment.
- Soft limit: 10 metrics per dashboard.
- UI shows warning when approaching/exceeding limit but allows override.

### 14.2 Data Model

#### Dashboard Schema

```javascript
{
  name: string,           // User-assigned dashboard name
  variablesCollapsed: boolean, // True when the Variables pane is collapsed
  variables: [            // Array of variable bindings
    {
      name: string,       // Variable identifier (e.g., "GTMF")
      nodeId: string,     // Source node ID (e.g., "node_7")
      readingType: string // Reading type (e.g., "temperature_millif")
    }
  ],
  charts: [               // Array of named charts
    {
      name: string,       // User-assigned chart name
      metricsCollapsed: boolean, // True when the Metrics pane is collapsed
      metrics: [          // Datasets rendered on this chart
        {
          id: string,         // Unique metric ID (UUID or timestamp-based)
          displayName: string,// User-friendly label (e.g., "Greenhouse Temp (°F)")
          expression: string, // Algebraic formula (e.g., "GTMF / 1000")
          color: string       // Dataset line color (hex, auto-assigned if omitted)
        }
      ]
    }
  ],
  timeRange: {            // Dashboard-level time window
    preset: string | null,  // "1h", "6h", "24h", "7d", or null for custom
    start: number | null,   // Unix timestamp ms (if custom range)
    end: number | null      // Unix timestamp ms (if custom range)
  }
}
```

Dashboards loaded from persisted data that omit `variablesCollapsed` or
`metricsCollapsed` are normalized to `false` so legacy dashboards start with all
configuration panes expanded.

#### localStorage Schema

Dashboards are stored as part of each environment's configuration:

```javascript
// In localStorage under "sonde_environments"
[
  {
    name: "Production",
    clientId: "...",
    tenantId: "...",
    storageAccount: "...",
    functionAppName: "...",
    sensorData: { ... },
    dashboards: [          // NEW: Array of dashboard objects
      { /* dashboard 1 with variablesCollapsed + charts[].metricsCollapsed */ },
      { /* dashboard 2 with variablesCollapsed + charts[].metricsCollapsed */ }
    ]
  }
]

// In localStorage under "sonde_active_environment"
"Production"
```

### 14.3 Variable Binding UI

**Add Variable Flow:**
1. Operator clicks "+ Add Variable" in Variables Panel.
2. Modal or inline form appears with fields:
   - **Variable Name** (text input, validated as JS identifier)
   - **Node ID** (dropdown, populated from `actualstate` table deduplication)
   - **Reading Type** (dropdown, filtered by selected node's reported readings)
3. On save:
   - Validate variable name uniqueness within dashboard.
   - Validate JS identifier format: `/^[a-zA-Z_][a-zA-Z0-9_]*$/`.
   - Add to `dashboard.variables` array.
   - Persist to `localStorage`.

**Edit/Delete Variable:**
- Edit: Opens same form with pre-filled values.
- Delete: Confirmation prompt warns if variable is used in any metric
  expression.

**Variable Display:**
- Variables Panel header includes an expand/collapse control with accessible
  expanded/collapsed state.
- Variables Panel shows a table when expanded:
  | Variable | Data Source | Actions |
  |----------|-------------|---------|
  | GTMF | Node 7, Temperature (milliF) | Edit Delete |
- When collapsed, the table and add button are hidden and the panel can be
  reopened without losing any configured variables.

### 14.4 Chart and Metric Editing

**Add Chart Flow:**
1. Operator clicks "+ Add Chart" in Charts Panel.
2. Prompt or modal appears with fields:
   - **Chart Name** (text input)
3. On save:
   - Create `chart = { name, metrics: [] }`.
   - Append to `dashboard.charts`.
   - Persist to `localStorage`.

**Edit/Delete Chart:**
- Edit: Rename chart without affecting contained metrics.
- Delete: Confirmation prompt warns that all contained metrics will be removed.

**Chart Display:**
- Each chart renders as a card containing:
  - Chart name
  - Metrics expand/collapse control in the chart header
  - Metric dataset list with edit/delete actions when expanded
  - Shared `<canvas>` for all metrics assigned to that chart
  - "+ Add Metric" button scoped to that chart when the Metrics section is
    expanded
- Collapsing the Metrics section hides metric configuration content but leaves
  the shared chart canvas and legend visible.

### 14.5 Expression Editor

**Add Metric Flow:**
1. Operator clicks "+ Add Metric" within a specific chart card.
2. Form appears with fields:
   - **Display Name** (text input)
   - **Expression** (text area with monospace font)
   - **Color** (color picker, optional, defaults to auto-assigned)
   - **Chart** (selected chart, prefilled to the chart where the action started)
3. On blur or save, expression is validated:
   - Parse using expression library (see §14.6).
   - Check for syntax errors → display inline error message.
   - Check for undefined variables → display warning list.
4. On save:
   - Add to `chart.metrics` array with unique ID.
   - Persist to `localStorage`.

**Expression Syntax Help:**
- Help text below expression field lists supported operators and functions:
  ```
  Operators: + - * / ^ (power)
  Precedence: () > ^ > * / > + - (left-to-right)
  Functions: sqrt(x), log(x), log10(x), exp(x), abs(x), min(a,b), max(a,b)
  Example: (GTMF - 273150) / 1000
  ```

**Live Preview (Optional Enhancement):**
- As operator types, show sample evaluation with current variable values.
- Example: "Expression `GTMF / 1000` with current `GTMF = 75000` → `75`"

### 14.6 Expression Evaluator Architecture

**Library Selection:**
- Use **`expr-eval`** (https://github.com/silentmatt/expr-eval) version 2.0.2+.
  - Lightweight (~15 KB minified).
  - Supports arithmetic, power, and math functions.
  - Safe: does not use `eval()` or `Function()`.
  - MIT licensed.
- Alternative: **`mathjs`** (more features but larger bundle size).
- Load from CDN (jsDelivr) with SRI hash.

**Supported Operations:**
- Arithmetic: `+`, `-`, `*`, `/`, `^` (power)
- Functions: `sqrt`, `log` (natural log), `log10`, `exp`, `abs`, `min`, `max`
- Parentheses for grouping

**Security:**
- MUST NOT use `eval()`, `new Function()`, or any dynamic code execution.
- Expression library MUST be loaded from a trusted CDN with SRI hash.
- Variable values are numbers only (no string injection).

**Evaluation Context:**
1. Fetch sensor data for all bound variables within time range.
2. For each timestamp `t` where at least one variable has data:
   - Build context object: `{ GTMF: 75000, P: 92500, H: 65.5, ... }`.
   - Evaluate expression with context.
   - Collect `(timestamp, value)` pair.
3. Render collected pairs as a dataset on the assigned chart.

**Error Handling:**
- **Parse errors**: Do not render chart; display error badge on metric.
- **Runtime errors** (e.g., `log(-5)`, division by zero): Skip that timestamp
  (gap in chart), log to browser console.
- **Missing variable at timestamp**: Skip that timestamp (gap in chart).

### 14.7 Chart Rendering

**Chart Library:**
- Reuse **Chart.js 4.4.9** (already used by Sensor Data tab).
- Each chart renders as a single `<canvas>` element.

**Layout:**
- Charts are stacked vertically within the dashboard.
- Each chart shows:
  - Chart name as card header.
  - One shared line chart with time on X-axis and a single shared Y-axis.
  - Legend entries for each metric assigned to the chart.
  - Metric expressions in the dataset editor/list, not as chart subtitles.
- Metrics use auto-assigned colors unless operator specifies a color.

**Time Range:**
- Dashboard-level time range selector (same UI as Sensor Data tab).
- All charts and metrics in the dashboard share the same time window.
- Operators can select presets (1h, 6h, 24h, 7d) or custom start/end.
- Chart X-axis tick labels adapt to the selected time window: ranges longer
  than 24 hours show date + time, while ranges of 24 hours or less show
  time-only labels.
- Variables/metrics pane state is independent from the time-range controls and
  does not affect chart evaluation.

**Data Fetching:**
- For each variable binding, resolve `sensordata` rows from the shared session
  telemetry cache when the requested range is already covered; otherwise fetch
  only the missing interval(s), merge them into cache, and then evaluate from
  the merged result.
- Evaluate each metric expression at each timestamp.
- Group computed metric series by chart membership in `dashboard.charts`.
- Render each chart with one dataset per metric assigned to that chart.
- Downsample if needed (reuse Sensor Data tab logic, max 500 points per metric dataset).

**Empty State:**
- If a chart has no metrics: show an inline prompt to add a metric.
- If no data exists for any metric on a chart: "No data in selected time range."
- If expression has errors: "Expression error: <message>."
- If variable is undefined: "Undefined variable: <name>."

**Legacy Migration:**
- When loading a persisted dashboard with a top-level `metrics` array and no
  `charts` array, create a default chart (for example `Chart 1`) and move all
  legacy metrics into that chart.

### 14.8 Persistence and Export

**localStorage Persistence:**
- Dashboards are nested under each environment in `sonde_environments`.
- Any dashboard change (add/edit/delete dashboard, chart, variable, or metric)
  triggers a `saveEnvironments()` call.
- Switching environments loads that environment's dashboards.

**Environment Export (WEB-0808 Integration):**
- When exporting an environment, include the `dashboards` array in the JSON.
- Schema:
  ```json
  {
    "version": 1,
    "name": "Production",
    "clientId": "...",
    "tenantId": "...",
    "storageAccount": "...",
    "functionAppName": "...",
    "sensorData": { ... },
    "dashboards": [
      {
        "name": "Greenhouse Monitoring",
        "variables": [ ... ],
        "charts": [ ... ],
        "timeRange": { ... }
      }
    ]
  }
  ```
- Importing an environment restores its dashboards.
- Missing `dashboards` field defaults to `[]`.
- Importing a legacy dashboard object with top-level `metrics` migrates those
  metrics into a single default chart.

### 14.9 Coexistence with Sensor Data Tab

- Both "Sensor Data" and "Dashboards" are top-level tabs in the SPA navigation.
- Each tab maintains independent state.
- Dashboards do not modify the Sensor Data tab.
- Long-term plan: deprecate Sensor Data tab once Dashboards feature matures,
  but for MVP they coexist.

---

## 15. Revision History

| Date | Author | Description |
|------|--------|-------------|
| 2026-06-18 | evolve skill | Extended §10.6 session telemetry cache design with in-flight request registries and consumer coalescing rules for duplicate cold-session Azure reads. |
| 2026-06-18 | evolve skill | Added §10.6 session telemetry cache design and updated dashboard metric data-fetching to use shared in-memory coverage-aware telemetry reuse. |
| 2026-06-17 | evolve skill | Added collapsible Variables and per-chart Metrics panes, including persisted pane state and accessibility requirements for dashboard configuration UI. |
| 2026-06-16 | evolve skill | Added §14 (Custom Dashboards) with variable binding, expression evaluation, and environment export integration. |
| 2026-05-29 | Issue #1092 | Added §13.1.1 gateway convergence rules. Replaced §13.2 rotation modal with inline form. Added convergence badge to §13.1. |
| 2026-05-19 | Trifecta remediation (#1012) | Added §12 (cross-cutting concerns: HTML escaping, MSAL hash routing, popup detection). Fixed §10.2 downsample cap to 500 points per series. |
