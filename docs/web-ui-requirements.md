<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->
# Sonde Web UI — Requirements

> **Document status:** Draft
> **Source:** Extracted from [web-ui-design.md](web-ui-design.md), `deploy/web-ui/app.js`, and [web-ui-validation.md](web-ui-validation.md).
> **Scope:** The Sonde Web UI — a static SPA hosted on GitHub Pages that provides an operator dashboard for monitoring and managing Sonde sensor nodes via Azure Storage Tables and Azure Functions.
> **Related:** [web-ui-design.md](web-ui-design.md), [web-ui-validation.md](web-ui-validation.md), [azure-handler-requirements.md](azure-handler-requirements.md)

---

## 1  Overview

The Sonde Web UI is a zero-build, vanilla HTML/JS/CSS single-page application
(SPA) hosted on GitHub Pages. It provides operators with a browser-based
interface to:

- Monitor node fleet status and detect configuration divergence
- Set desired state (schedule interval, program assignment) per node
- Upload BPF program ELF binaries for server-side verification and storage
- Visualize sensor data as time-series graphs and tabular views
- Manage multiple Azure backend environments without redeployment

The SPA communicates directly with Azure Storage Tables via REST API (using
MSAL.js bearer tokens) and with an Azure Function (`ProgramIngest`) for
program ingestion.

---

## 2  Scope

### In scope

- SPA front-end behavior (all tabs: Dashboard, Desired State, Programs, Sensor Data)
- Key Management (gateway status, master key rotation)
- Environment manager (runtime configuration via `localStorage`)
- Authentication via MSAL.js (Entra ID)
- Program upload flow (SPA → ProgramIngest Azure Function)
- Infrastructure requirements (Bicep provisioning, CORS, EasyAuth, GitHub Pages deployment)

### Out of scope

- Azure handler function internals beyond ProgramIngest's externally observable contracts (covered by [azure-handler-requirements.md](azure-handler-requirements.md))
- Gateway, node, or modem firmware behavior
- BPF program compilation or Prevail verifier internals
- Azure Table schema design (owned by the handler)

---

## 3  Definitions and Glossary

| Term | Definition |
|------|------------|
| **SPA** | Single-page application — the web UI served from `deploy/web-ui/`. |
| **Actual state** | The latest telemetry row a node has reported, stored in the `actualstate` Azure Table. |
| **BIP-39 fingerprint** | Six-word fingerprint string displayed in the UI so the operator can verify the gateway identity against the modem. |
| **Desired state** | An operator-specified target configuration for a node, stored in the `desiredstate` Azure Table. |
| **Divergence** | A mismatch between a node's actual state and its desired state (program hash or schedule interval). |
| **Environment** | A named set of Azure backend connection details (client ID, tenant ID, storage account, function app) stored in `localStorage`. |
| **Gateway status** | The gateway ACTUAL_STATE summary shown in a dedicated dashboard card, read from the `actualstate` Azure Table with `PartitionKey` starting with `"g:"`. The SPA selects the latest row per gateway partition using `latestByPartition` (lexicographically smallest reverse-timestamp `RowKey`). |
| **ProgramIngest** | An HTTP-triggered Azure Function that accepts ELF uploads, runs Prevail verification, and stores verified program images. |
| **Reverse-timestamp RowKey** | `{(u64::MAX - timestamp_ms):016x}:{(u64::MAX - sequence):016x}:{random_nonce:016x}` — ensures newest rows sort first in Azure Tables. |
| **Rotation code** | A six-character operator-entered confirmation code normalized to uppercase `[A-Z0-9]` and included in the encrypted rotation payload. |
| **Rotation payload** | The encrypted `RotationPayloadV1` binary blob written to the gateway's `desiredstate` row to request master key rotation. |
| **Series** | A unique `(NodeId, ProgramHash, ReadingName)` tuple in sensor data, rendered as one line on the time-series chart. |

---

## 4  Requirement Format

Each requirement uses the following fields:

- **ID** — Unique identifier. Functional requirements use `WEB-XXYY` (e.g., `WEB-0101`); cross-cutting requirements use `WEB-CC-NN` (e.g., `WEB-CC-01`).
- **Title** — Short name.
- **Description** — What the SPA must do, using RFC 2119 keywords.
- **Acceptance criteria** — Observable, testable conditions (AC-1, AC-2, …).
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Design doc section, code reference, or user request.
- **Confidence** — **High** (directly evidenced), **Medium** (inferred from patterns), **Low** (needs confirmation).

---

## 5  Node Dashboard (WEB-0100)

### WEB-0101  Node State Table

**Priority:** Must
**Source:** web-ui-design.md §4, app.js `renderDashboard()`
**Confidence:** High

**Description:**
The SPA MUST render a table of node states by querying the `actualstate` Azure
Table. Each node is represented by the most recent row per `PartitionKey`
(determined by smallest `RowKey` due to reverse-timestamp ordering).

**Acceptance criteria:**

1. The Dashboard tab queries the `actualstate` table on load.
2. Nodes are deduplicated to one row per `PartitionKey` (latest only).
3. Nodes are sorted alphabetically by `node_id`.
4. An empty table displays "No node state found."

---

### WEB-0102  Dashboard Columns

**Priority:** Must
**Source:** web-ui-design.md §4, app.js `renderDashboard()`
**Confidence:** High

**Description:**
The dashboard table MUST display the following columns: Node ID, Battery (mV),
Firmware, ABI Version, Schedule (s), Current Program Hash, Assigned Program
Hash, Last Seen, Status.

**Acceptance criteria:**

1. All nine columns are present in the table header.
2. Program hashes are displayed as truncated 8-character hex with a full-hash tooltip.
3. "Last Seen" displays a relative time string (e.g., "5m ago").
4. Missing values display "—".

---

### WEB-0103  Dashboard Auto-Refresh

**Priority:** Must
**Source:** web-ui-design.md §4, app.js `setAutoRefresh()`
**Confidence:** High

**Description:**
The dashboard MUST auto-refresh at a configurable interval (default 30 seconds).

**Acceptance criteria:**

1. After initial render, the dashboard re-fetches and re-renders every 30 seconds.
2. Auto-refresh is cancelled when navigating to a different tab.

---

### WEB-0104  Divergence Indicator

**Priority:** Must
**Source:** web-ui-design.md §4, app.js `renderDashboard()` divergence logic
**Confidence:** High

**Description:**
The dashboard MUST display a divergence indicator for each node by
cross-referencing the `desiredstate` table.

**Acceptance criteria:**

1. When a desired-state row exists for a node and the desired program hash differs from the actual current program hash, the node shows "Diverged."
2. When a desired-state row exists with an empty/missing `desired_assigned_program_hash` and the node still reports a current program, the node shows "Diverged."
3. When no desired-state row exists for a node, the node shows "Aligned" (unmanaged node).
4. When `desired_schedule_interval_s` is set and differs from `observed_schedule_interval_s`, the node shows "Diverged."
5. The Schedule column tooltip shows both observed and desired values.

---

## 6  Desired State Management (WEB-0200)

### WEB-0201  Set Desired Schedule

**Priority:** Must
**Source:** web-ui-design.md §5, app.js `renderDesiredState()` submit handler
**Confidence:** High

**Description:**
The SPA MUST allow operators to set a desired schedule interval for a node by
writing a row to the `desiredstate` Azure Table.

**Acceptance criteria:**

1. The Desired State form includes a "Schedule Interval (s)" number input (min=1, step=1).
2. On submit, the entity includes `desired_schedule_interval_s` as `Edm.Int32`.
3. An empty schedule field omits `desired_schedule_interval_s` from the entity.

---

### WEB-0202  Assign Program Hash

**Priority:** Must
**Source:** web-ui-design.md §5, app.js `renderDesiredState()` submit handler
**Confidence:** High

**Description:**
The SPA MUST allow operators to assign a program hash to a node by writing a
row to the `desiredstate` Azure Table. The program hash is selected from a
dropdown populated from the `programs` table.

**Acceptance criteria:**

1. The Program Hash field is a `<select>` dropdown with an "No program target" default option.
2. Options are populated from the `programs` table (`PartitionKey eq 'program'`).
3. On submit, `desired_assigned_program_hash` is stored as a lowercase hex string.
4. Selecting "No program target" omits `desired_assigned_program_hash` from the entity.

---

### WEB-0203  RowKey Format

**Priority:** Must
**Source:** web-ui-design.md §5, app.js `desiredRowKey()`
**Confidence:** High

**Description:**
Desired-state entity `RowKey` MUST use the reverse-timestamp format:
`{(u64::MAX - timestamp_ms):016x}:{(u64::MAX - sequence):016x}:{random_nonce:016x}`.

**Acceptance criteria:**

1. `RowKey` is a string of three colon-separated 16-character hex segments.
2. The first segment is the bitwise complement of the current `timestamp_ms`.
3. The third segment is 16 hex characters of cryptographic randomness.

---

### WEB-0204  PartitionKey Derivation

**Priority:** Must
**Source:** web-ui-design.md §5, app.js `sha256hex()`
**Confidence:** High

**Description:**
Desired-state entity `PartitionKey` MUST be `n:{SHA-256(node_id).hex()}`,
computed using the Web Crypto API (`SubtleCrypto.digest`).

**Acceptance criteria:**

1. `PartitionKey` starts with `n:` followed by a 64-character lowercase hex SHA-256 hash.
2. The hash input is the UTF-8 encoding of the `node_id` string.

---

### WEB-0205  Timestamp Storage

**Priority:** Must
**Source:** web-ui-design.md §5, app.js submit handler
**Confidence:** High

**Description:**
The `timestamp_ms` field MUST be stored as `Edm.Int64` in the Azure Table entity.

**Acceptance criteria:**

1. The entity includes `timestamp_ms` as a string value.
2. The entity includes `timestamp_ms@odata.type` set to `Edm.Int64`.

---

### WEB-0206  Node ID Dropdown

**Priority:** Must
**Source:** web-ui-design.md §5 (WEB-0206), app.js `renderDesiredState()`
**Confidence:** High

**Description:**
The Node ID field MUST be a dropdown-only `<select>` control populated from
nodes that have reported actual state. Arbitrary node IDs MUST NOT be accepted.

**Acceptance criteria:**

1. The Node ID field is a `<select>` element (not a text input).
2. Options are populated from the `actualstate` table, deduplicated to one per `PartitionKey`.
3. A placeholder option ("Select a node…") is shown by default and is not submittable.
4. Free-text entry is not supported.

---

### WEB-0207  Auto-Populate on Node Selection

**Priority:** Must
**Source:** web-ui-design.md §5 (WEB-0207), app.js `nodeSelect` change handler
**Confidence:** High

**Description:**
When the operator selects a node in the Desired State form, the Schedule
Interval and Program Hash fields MUST be pre-populated using a desired-over-actual
fallback strategy.

**Acceptance criteria:**

1. Schedule Interval is populated from `desired_schedule_interval_s` (if desired-state row exists), else `observed_schedule_interval_s`, else empty.
2. Program Hash is populated from `desired_assigned_program_hash` (if desired-state row exists), else `observed_assigned_program_hash`, else "No program target."
3. If the pre-populated program hash is not in the dropdown options, the field defaults to "No program target."

---

## 7  Program Ingest (WEB-0300)

### WEB-0301  ProgramIngest HTTP Endpoint

**Priority:** Must
**Source:** web-ui-design.md §6.1, azure-handler code
**Confidence:** High

**Description:**
The ProgramIngest Azure Function MUST accept `POST` requests at
`/api/programs/ingest` with a JSON body containing an `elf` field
(base64-encoded ELF binary) and optional metadata fields.

**Acceptance criteria:**

1. The endpoint accepts `POST` requests with `Content-Type: application/json`.
2. Request body fields: `elf` (string, required), `source_filename` (string, optional), `abi_version` (integer, optional), `verification_profile` (string, optional: `"resident"` or `"ephemeral"`).
3. Missing `elf` field returns HTTP 400.

---

### WEB-0302  Prevail Verification

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
The ProgramIngest function MUST run Prevail verification on the uploaded ELF
binary. Invalid programs MUST be rejected.

**Acceptance criteria:**

1. Valid ELF passes Prevail verification and is stored.
2. Invalid ELF returns HTTP 422 with diagnostic information.

---

### WEB-0303  Program Hash Computation

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
The ProgramIngest function MUST compute the program hash as SHA-256 of the
deterministic CBOR-encoded program image, matching the gateway's computation.

**Acceptance criteria:**

1. The returned `program_hash` matches the SHA-256 of the CBOR image.
2. Re-ingesting the same ELF produces the same hash (idempotent).

---

### WEB-0304  Program Storage

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
Verified programs MUST be stored in the `programs` Azure Table with all
required fields.

**Acceptance criteria:**

1. Entity stored with `PartitionKey="program"`, `RowKey=hex(program_hash)`.
2. Fields include: `source_filename`, `abi_version` (`Edm.Int32`), `cbor_image` (base64), `elf_image` (base64), `size_bytes` (`Edm.Int32`), `verification_profile`, `created_at` (ISO 8601 UTC).

---

### WEB-0305  Ingest Response Format

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
The ProgramIngest function MUST return a JSON response with program metadata
on success, or diagnostics on failure.

**Acceptance criteria:**

1. Success (HTTP 200): `{"program_hash": "hex", "size": N, "abi_version": N, "source_filename": "name"}`.
2. Failure: JSON error body with diagnostic information and appropriate HTTP status code.

---

### WEB-0306  Oversized Program Rejection

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
The ProgramIngest function MUST reject ELF binaries that exceed the 1 MB
size limit (defense-in-depth before verification).

**Acceptance criteria:**

1. Decoded ELF exceeding 1 MB returns HTTP 413.

---

### WEB-0307  Invalid ELF Rejection

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
The ProgramIngest function MUST reject empty ELFs and multi-program ELFs.

**Acceptance criteria:**

1. Empty ELF (0 bytes) returns an error response.
2. ELF containing multiple BPF programs returns an error response.

---

### WEB-0308  Source Filename Normalization

**Priority:** Must
**Source:** web-ui-design.md §6.1
**Confidence:** High

**Description:**
The ProgramIngest function MUST normalize `source_filename` to its basename
(strip path components) using `normalize_display_filename()`.

**Acceptance criteria:**

1. A `source_filename` of `../../etc/passwd` is stored as `passwd`.
2. A `source_filename` of `C:\Users\test\program.o` is stored as `program.o`.

---

### WEB-0309  Inline ELF in DESIRED_STATE

**Priority:** Must
**Source:** web-ui-design.md §6.2
**Confidence:** High

**Description:**
When the handler publishes a `DESIRED_STATE` message due to program divergence,
it MUST fetch the original ELF binary from the `programs` table and embed it
at CBOR key 5.

**Acceptance criteria:**

1. `DESIRED_STATE` message includes key 5 with ELF bytes when program divergence exists.
2. The gateway receives the ELF and runs full Prevail verification.

---

### WEB-0310  DESIRED_STATE Metadata Keys

**Priority:** Must
**Source:** web-ui-design.md §6.2
**Confidence:** High

**Description:**
`DESIRED_STATE` messages with inline ELF MUST carry metadata at CBOR keys 6–8.

**Acceptance criteria:**

1. Key 6 carries the verification profile.
2. Key 7 carries the source filename.
3. Key 8 carries the ABI version.

---

## 8  Program List (WEB-0400)

### WEB-0401  Program List Display

**Priority:** Must
**Source:** web-ui-design.md §7, app.js `renderPrograms()`
**Confidence:** High

**Description:**
The SPA MUST display a table of programs from the `programs` Azure Table.

**Acceptance criteria:**

1. Programs are queried with filter `PartitionKey eq 'program'`.
2. Table columns: Hash (truncated), Filename, ABI, Size, Created.
3. Programs are sorted by `created_at` descending (newest first).
4. An empty table displays "No programs found."

---

## 9  Authentication (WEB-0500)

### WEB-0501  MSAL.js Login Flow

**Priority:** Must
**Source:** web-ui-design.md §8, app.js `initMsal()`, `login()`
**Confidence:** High

**Description:**
The SPA MUST authenticate users via MSAL.js 2.x using the authorization code
flow with PKCE.

**Acceptance criteria:**

1. Login uses `loginPopup()` with `STORAGE_SCOPES`.
2. Token cache uses `sessionStorage`.
3. The active account is tracked and displayed in the header.

---

### WEB-0502  Storage API Bearer Token

**Priority:** Must
**Source:** web-ui-design.md §8, app.js `getToken()`
**Confidence:** High

**Description:**
All Azure Storage Table REST API calls MUST include a bearer token with
the `https://storage.azure.com/.default` scope.

**Acceptance criteria:**

1. `acquireTokenSilent` is attempted first; falls back to `acquireTokenPopup`.
2. The `Authorization: Bearer <token>` header is included on every table query.

---

### WEB-0503  ProgramIngest Authentication (EasyAuth)

**Priority:** Must
**Source:** web-ui-design.md §9.4
**Confidence:** High

**Description:**
The ProgramIngest endpoint MUST reject unauthenticated requests via EasyAuth
(returns HTTP 401).

**Acceptance criteria:**

1. A request without a bearer token receives HTTP 401.

---

### WEB-0504  Function App Token Scope

**Priority:** Must
**Source:** web-ui-design.md §8, app.js `getFunctionToken()`
**Confidence:** High

**Description:**
The SPA MUST acquire a separate token scoped to
`api://<companionClientId>/user_impersonation` for ProgramIngest calls.

**Acceptance criteria:**

1. The SPA calls `acquireTokenSilent` (popup fallback) with the Function App scope.
2. The token is sent as a `Bearer` header on `POST /api/programs/ingest`.

---

### WEB-0505  Wrong-Audience Token Rejection

**Priority:** Must
**Source:** web-ui-design.md §9.4
**Confidence:** High

**Description:**
The ProgramIngest endpoint MUST reject tokens scoped to
`https://storage.azure.com/.default` (wrong audience).

**Acceptance criteria:**

1. A Storage-scoped token sent to ProgramIngest is rejected by EasyAuth.

---

### WEB-0506  Expired Token Rejection

**Priority:** Must
**Source:** web-ui-design.md §9.4
**Confidence:** High

**Description:**
The ProgramIngest endpoint MUST reject expired bearer tokens.

**Acceptance criteria:**

1. An expired token sent to ProgramIngest receives HTTP 401.

---

### WEB-0507  Valid Token Acceptance

**Priority:** Must
**Source:** web-ui-design.md §9.4
**Confidence:** High

**Description:**
The ProgramIngest endpoint MUST accept valid tokens with the
`api://<clientId>/user_impersonation` audience.

**Acceptance criteria:**

1. A valid, non-expired token with the correct audience is accepted.

---

### WEB-0508  Redirect URI Configuration

**Priority:** Must
**Source:** web-ui-design.md §8, app.js `initMsal()` `basePath` logic
**Confidence:** High

**Description:**
The MSAL `redirectUri` MUST be set to `window.location.origin` plus the
normalized directory path (stripping filename) so authentication works on both
GitHub Pages project sites and custom domain hostnames.

**Acceptance criteria:**

1. On `https://alan-jowett.github.io/sonde/`, the redirect URI is `https://alan-jowett.github.io/sonde/`.
2. On `https://sondeplatform.com/`, the redirect URI is `https://sondeplatform.com/`.
3. On `https://example.com/sonde/index.html`, the redirect URI is `https://example.com/sonde/`.

---

## 10  Infrastructure (WEB-0600)

### WEB-0602  Programs Table Provisioning

**Priority:** Must
**Source:** web-ui-design.md §9.2
**Confidence:** High

**Description:**
The Bicep deployment MUST provision a `programs` table in the Azure Storage account.

**Acceptance criteria:**

1. The `programs` table exists after deployment.

---

### WEB-0603  ProgramIngest Co-Deployment

**Priority:** Must
**Source:** web-ui-design.md §9.3
**Confidence:** High

**Description:**
The ProgramIngest HTTP trigger MUST be deployed alongside the
`UpstreamConnector` queue trigger in the same Azure Function App.

**Acceptance criteria:**

1. Both triggers are available in the same Function App deployment.

---

### WEB-0604  CORS Configuration

**Priority:** Must
**Source:** web-ui-design.md §9.5
**Confidence:** High

**Description:**
The Function App MUST configure CORS to allow requests from the GitHub Pages
origin and the custom domain origin.

**Acceptance criteria:**

1. CORS allows `https://alan-jowett.github.io`.
2. CORS allows `https://sondeplatform.com`.
3. Origins are parameterized via Bicep parameters.

---

### WEB-0605  Function Identity RBAC

**Priority:** Must
**Source:** web-ui-design.md §9.2
**Confidence:** High

**Description:**
The Function App's managed identity MUST have Storage Table Data Contributor
role on the `programs` table.

**Acceptance criteria:**

1. The function identity can read and write to the `programs` table.

---

### WEB-0606  EasyAuth Configuration

**Priority:** Must
**Source:** web-ui-design.md §9.4
**Confidence:** High

**Description:**
The Function App MUST be configured with Azure App Service Authentication
(EasyAuth) using the Entra ID provider.

**Acceptance criteria:**

1. `platform.enabled` is `true`.
2. `unauthenticatedClientAction` is `Return401`.
3. `azureActiveDirectory.enabled` is `true`.
4. `allowedAudiences` includes both `api://<clientId>` and the bare `<clientId>`.

---

### WEB-0607  ProgramIngest Auth Level

**Priority:** Must
**Source:** web-ui-design.md §9.3
**Confidence:** High

**Description:**
The ProgramIngest function's `authLevel` MUST be `anonymous` — authentication
is delegated to EasyAuth, not Azure Functions runtime keys.

**Acceptance criteria:**

1. `crates/sonde-azure-handler/function-app/ProgramIngest/function.json` specifies `authLevel: "anonymous"`.

---

## 11  Sensor Data (WEB-0700)

### WEB-0700  Sensor Data Tab

**Priority:** Must
**Source:** web-ui-design.md §10, app.js `renderSensorData()`
**Confidence:** High

**Description:**
The SPA MUST provide a Sensor Data tab that loads data from the `sensordata`
Azure Table.

> **Note:** This document uses `sensordata` (lowercase) to match the SPA
> default config and Bicep provisioning. Azure Table names are
> case-insensitive; other docs (e.g., `azure-handler-requirements.md`) may
> use `SensorData` for readability.

**Acceptance criteria:**

1. A "Sensor Data" tab appears in the tab bar.
2. The tab queries `sensordata` table using per-node partition queries.
3. The tab displays a loading indicator while fetching.

---

### WEB-0701  Time-Series Graph

**Priority:** Must
**Source:** web-ui-design.md §10.2, app.js `renderSensorChart()`
**Confidence:** High

**Description:**
The SPA MUST render a time-series graph where each unique
`(NodeId, ProgramHash, ReadingName)` tuple is a separate line.

**Acceptance criteria:**

1. The X-axis is time (derived from `timestamp_ms`); the Y-axis is the reading value.
2. Time range selector: Last 1h, 24h (default), 7d.
3. Maximum 20 concurrent lines; excess series are selectable via series picker.
4. Data is downsampled client-side to a maximum of 500 points per series.
5. Hover tooltip shows timestamp, series label, and value.
6. String-encoded int64 values above `Number.MAX_SAFE_INTEGER` are displayed as strings; in-range values render as numbers.

---

### WEB-0702  Sensor Data Table View

**Priority:** Must
**Source:** web-ui-design.md §10.3, app.js `renderSensorTable()`
**Confidence:** High

**Description:**
The SPA MUST provide a table view for sensor data, togglable with the graph view.

**Acceptance criteria:**

1. Table columns: Timestamp, Node ID, Program Hash, Decoded Readings, Raw Payload.
2. Rows are sorted by timestamp descending.
3. Rows with empty `decoded_readings` display "—".
4. Raw Payload is truncated to 40 characters with a full-value tooltip.

---

### WEB-0703  Series Display Customization

**Priority:** Must
**Source:** web-ui-design.md §10.4, app.js series override functions
**Confidence:** High

**Description:**
The SPA MUST allow operators to customize how each series is displayed via a
modal dialog.

**Acceptance criteria:**

1. Each series in the picker has a ✏️ edit button that opens a modal dialog.
2. The dialog allows setting: display name, scale divisor, and unit suffix.
3. Custom display name replaces the default label in series picker, chart legend, and tooltip.
4. Scale divisor transforms plotted values (e.g., 1000 converts 21500 → 21.5).
5. Unit suffix appears in tooltip values and Y-axis title (when all series share the same suffix).
6. Overrides persist in `localStorage` under `sonde_series_overrides`.
7. Reset to Default clears overrides and restores original label/scale.
8. The dialog has focus trapping and closes on Escape.

---

### WEB-0704  Sensor Data Export

**Priority:** Must
**Source:** User request (2026-06-08), web-ui-design.md §10.5
**Confidence:** High

**Description:**
The SPA MUST allow operators to export sensor-data rows from a custom
start/end time range as either `.jsonl` or `.csv`.

**Acceptance criteria:**

1. The Sensor Data tab provides export controls with a start time, end time,
   format selector, and export action.
2. The export range is independent of the graph/table view toggle, graph
   time-range selector, and series selection state.
3. Export queries all matching sensor-data rows across all known node
   partitions for the selected time range.
4. Export follows Azure Table continuation tokens so rows beyond the first
   page are included in the downloaded file.
5. CSV export writes one header row and one data row per sensor-data entity
   with columns `timestamp_ms`, `node_id`, `program_hash`, `raw_payload`, and
   `decoded_readings_json`.
6. JSONL export writes one JSON object per line with fields `timestamp_ms`,
   `node_id`, `program_hash`, `raw_payload`, and `decoded_readings`.
7. Rows with empty `decoded_readings` export as an empty `decoded_readings_json`
   CSV column and `decoded_readings: null` in JSONL.
8. The export range is validated before querying; invalid or inverted ranges
   are rejected with operator-visible feedback.

---

## 12  Environment Manager (WEB-0800)

### WEB-0800  Runtime Environment Configuration

**Priority:** Must
**Source:** web-ui-design.md §11, app.js environment functions
**Confidence:** High

**Description:**
The SPA MUST support runtime configuration of Azure backend environments
without redeployment. Environments are stored in `localStorage`.

**Acceptance criteria:**

1. No deploy-time configuration file is required for normal operation.
2. Each environment stores: name, clientId, tenantId, storageAccount, functionAppName.
3. Environments are persisted in `localStorage` under `sonde_environments`.

---

### WEB-0801  Environment Persistence

**Priority:** Must
**Source:** web-ui-design.md §11.2, app.js `saveEnvironments()`
**Confidence:** High

**Description:**
Adding an environment MUST persist all fields to `localStorage` under the
`sonde_environments` key.

**Acceptance criteria:**

1. All five fields (name, clientId, tenantId, storageAccount, functionAppName) are stored.
2. The environment is retrievable after page reload.

---

### WEB-0802  Environment Validation

**Priority:** Must
**Source:** app.js `showEnvironmentForm()` validation logic
**Confidence:** High

**Description:**
The environment form MUST validate all fields before saving.

**Acceptance criteria:**

1. All five fields are required; empty fields show an error.
2. Client ID and Tenant ID must be valid GUIDs (hex with hyphens, case-insensitive).
3. Storage Account must be 3–24 lowercase alphanumeric characters.
4. Function App Name must be 2–60 alphanumeric characters with optional hyphens.
5. Duplicate environment names are rejected on add.

---

### WEB-0803  First-Load Setup Modal

**Priority:** Must
**Source:** web-ui-design.md §11.4, app.js `init()`
**Confidence:** High

**Description:**
On first load with no environments configured, the SPA MUST show a full-screen
setup modal. The main app UI MUST be inaccessible until at least one environment
is configured. The modal MUST offer both manual entry and file import.

**Acceptance criteria:**

1. With no environments in `localStorage`, the setup modal is shown.
2. The modal cannot be closed without adding an environment (no "Close" button).
3. Tab bar and content area are not interactive.
4. The setup modal includes an Import button that accepts a `.json` environment file (see WEB-0807).

---

### WEB-0804  Edit, Export, and Delete Environments

**Priority:** Must
**Source:** web-ui-design.md §11.4, app.js `showEnvironmentManager()`
**Confidence:** High

**Description:**
The SPA MUST support editing, exporting, and deleting existing environments.

**Acceptance criteria:**

1. Editing opens a form pre-filled with the environment's current values.
2. The name field is read-only during edit.
3. Deleting an environment removes it from `localStorage`.
4. Deleting the active environment switches to the next available environment or shows the setup modal.
5. Each environment row shows Use (for non-active), Export, Edit, and Delete buttons.

---

### WEB-0807  Environment Import from JSON File

**Priority:** Should
**Source:** [issue #1074](https://github.com/Alan-Jowett/sonde/issues/1074)
**Confidence:** High

**Description:**
The environment manager and the first-load setup modal MUST offer an Import
button that reads a JSON environment file and adds the environment to
`localStorage`. The import flow validates the file schema, prompts for a name
when the `name` field is blank, and handles conflicts with existing environments.

**Acceptance criteria:**

1. An "Import" button is visible in both the environment manager panel and the first-load setup modal.
2. Clicking Import opens a file picker accepting `.json` files.
3. The file MUST contain `version` equal to integer `1`; files with missing, non-numeric, zero, or greater-than-1 version values are rejected with an error message.
4. The four data fields (`clientId`, `tenantId`, `storageAccount`, `functionAppName`) are validated using the same rules as WEB-0802.
5. If `name` is blank or missing, a name prompt is shown before saving.
6. If `name` conflicts with an existing environment, a prompt offers overwrite or rename.
7. Overwriting the active environment triggers the full re-initialization sequence defined by WEB-0806.
8. Successfully imported environments appear in the environment list and are persisted to `localStorage`.
9. Non-JSON files, files with missing required fields, and files with a top-level type other than object are rejected with a descriptive error message.
10. Extra JSON properties beyond the defined schema are ignored.

---

### WEB-0808  Per-Environment Export to JSON File

**Priority:** Should
**Source:** [issue #1074](https://github.com/Alan-Jowett/sonde/issues/1074)
**Confidence:** High

**Description:**
Each environment row in the environment manager MUST have an Export button that
downloads a JSON file containing that environment's settings in the
import-compatible schema.

**Acceptance criteria:**

1. Each environment row has an Export button alongside Use, Edit, and Delete.
2. Clicking Export triggers a browser download of a `.json` file.
3. The filename is derived from the environment name with unsafe filesystem characters replaced; if the result is empty, the fallback filename `sonde-environment.json` is used.
4. The exported file contains `version` (integer 1) and all five fields (`name`, `clientId`, `tenantId`, `storageAccount`, `functionAppName`).
5. The exported file is valid for re-import via WEB-0807.

---

### WEB-0805  Active Environment Indicator

**Priority:** Must
**Source:** web-ui-design.md §11.4, app.js `updateEnvironmentIndicator()`
**Confidence:** High

**Description:**
The active environment name MUST be displayed in the header bar.

**Acceptance criteria:**

1. The environment name is visible in the top bar next to the ⚙ gear button.
2. The indicator updates when the environment is switched.

---

### WEB-0806  Environment Switching

**Priority:** Must
**Source:** web-ui-design.md §11.5, app.js `switchEnvironment()`
**Confidence:** High

**Description:**
When the user switches to a different environment, the SPA MUST fully
re-initialize authentication and refresh the active tab.

**Acceptance criteria:**

1. Auto-refresh timer is cleared.
2. `CONFIG` fields are updated from the selected environment.
3. MSAL instance is discarded and re-created.
4. Active MSAL account is cleared.
5. MSAL-related `sessionStorage` keys are cleared (not all session storage).
6. A new MSAL instance is initialized with the new environment's credentials.
7. The active tab is re-rendered.

---

## 13  Deployment (WEB-0900)

### WEB-0901  GitHub Pages Deployment

**Priority:** Must
**Source:** web-ui-design.md §9.1, `.github/workflows/web-ui.yml`
**Confidence:** High

**Description:**
The SPA MUST be deployed to GitHub Pages via a GitHub Actions workflow that
publishes `deploy/web-ui/` on pushes to `main`.

**Acceptance criteria:**

1. The workflow triggers on pushes to `main` that modify `deploy/web-ui/**`.
2. The workflow uses `actions/upload-pages-artifact` and `actions/deploy-pages`.
3. Manual trigger (`workflow_dispatch`) is supported.

---

### WEB-0902  CORS and Redirect URI Provisioning

**Priority:** Must
**Source:** web-ui-design.md §9.5
**Confidence:** High

**Description:**
Bicep deployment MUST include GitHub Pages and `sondeplatform.com` in CORS
origins and SPA redirect URIs.

**Acceptance criteria:**

1. CORS origins include `https://alan-jowett.github.io` and `https://sondeplatform.com`.
2. SPA redirect URIs include `https://alan-jowett.github.io/sonde/` and `https://sondeplatform.com/`.
3. Origins are parameterized via Bicep parameters.
4. The bootstrap script defaults `SONDE_AZURE_CUSTOM_DOMAIN_ORIGIN` to `https://sondeplatform.com`, matching the Bicep `customDomainOrigin` default. An operator may opt out by setting it to the empty string.

---

## 14  Cross-Cutting Requirements

### WEB-CC-01  Zero-Build Architecture

**Priority:** Must
**Source:** web-ui-design.md §1, app.js (no build step)
**Confidence:** High

**Description:**
The SPA MUST be a zero-build vanilla HTML/JS/CSS application with no
compilation, bundling, or transpilation step. Browser dependencies MUST be
loaded from CDN.

**Acceptance criteria:**

1. `deploy/web-ui/` contains only `.html`, `.js`, `.css`, and `.json` files.
2. No `package.json`, build scripts, or node_modules.
3. External libraries (MSAL.js, Chart.js) are loaded via CDN `<script>` tags.

---

### WEB-CC-02  HTML Output Escaping

**Priority:** Must
**Source:** app.js `escapeHtml()` function
**Confidence:** High

**Description:**
All user-supplied and server-sourced values rendered into HTML MUST be escaped
to prevent XSS.

**Acceptance criteria:**

1. The `escapeHtml()` function escapes `&`, `<`, `>`, `"`, and `'`.
2. All dynamic values in rendered HTML pass through `escapeHtml()`.

---

### WEB-CC-03  MSAL Hash Routing Compatibility

**Priority:** Must
**Source:** app.js `initMsal()` hash handling
**Confidence:** High

**Description:**
The SPA MUST handle conflicts between MSAL.js hash-based auth responses and
the app's own hash-based tab routing.

**Acceptance criteria:**

1. Non-auth hashes are temporarily cleared before MSAL initialization.
2. Auth hashes (containing `code=`, `error=`, `access_token=`) are preserved for MSAL.
3. The routing hash is restored after MSAL processes the redirect.

---

### WEB-CC-04  Popup Window Detection

**Priority:** Must
**Source:** app.js `DOMContentLoaded` handler
**Confidence:** High

**Description:**
When the SPA is loaded inside an MSAL login popup, it MUST skip full app
initialization to avoid unnecessary API calls and rendering.

**Acceptance criteria:**

1. If `window.opener && window.opener !== window`, the `init()` function is not called.

---

## 15  Key Management (WEB-1000)

### WEB-1001  Gateway Status Display

**Priority:** Must
**Source:** Issue #962, Issue #1092
**Confidence:** High

**Description:**
The dashboard MUST display a gateway status card showing information from the
gateway's ACTUAL_STATE row in the `actualstate` Azure Table
(`PartitionKey` starting with `"g:"`). The SPA selects the latest row per
gateway partition using `latestByPartition`. The card MUST
show the BIP-39 fingerprint (6 words), `master_key_epoch`, `master_key_id`
(32-byte SHA-256 hex), `rotation_in_progress`,
`gateway_version`, `modem_firmware_version`, and `channel`.

The gateway status card MUST also display a convergence badge (Aligned /
Diverged) by cross-referencing the `desiredstate` table for gateway rows
(`PartitionKey` starting with `"g:"`). See WEB-1009 for convergence rules.

**Critical:** The BIP-39 fingerprint MUST be computed locally in the SPA
from the `x25519_public_key` field — NOT read from the Azure-stored
`fingerprint_words` field. The SPA computes SHA-256 of the public key,
extracts 66 bits, and maps to 6 BIP-39 words using the same wordlist as
the gateway. This ensures that a compromised Azure cannot substitute a
rogue public key with pre-matched fingerprint words. The admin verifies
the SPA-computed fingerprint against the modem display.

The gateway status card MUST be rendered as a separate UI element and MUST
NOT appear in the node table or node dropdown.

**Acceptance criteria:**

1. The gateway status card displays all required fields.
2. The BIP-39 fingerprint is computed locally from `x25519_public_key`, not
   read from the stored `fingerprint_words` field.
3. The gateway ACTUAL_STATE row is not shown in the node table.
4. Node dropdowns exclude gateway entities.
5. The gateway status card shows an Aligned/Diverged convergence badge.

---

### WEB-1002  Key Rotation Initiation

**Priority:** Must
**Source:** Issue #962, Issue #1092
**Confidence:** High

**Description:**
A `Rotate Key` button on the gateway status card MUST toggle an inline
collapsible rotation form within the card. The form shows (1) the BIP-39
fingerprint with instructions to verify it against the modem, (2) a rotation
code input limited to 6 characters and normalized to uppercase `[A-Z0-9]`,
(3) a masked passphrase input that requires either at least 20 characters or
6 space-separated words, (4) a deployment label input field where the admin
enters the label used to derive salt deterministically, and (5) a confirmation
action. If the browser lacks the required rotation crypto capabilities, the
button MUST be disabled with a tooltip explaining the requirement.

After submission the form displays a brief confirmation message (e.g.,
"Rotation submitted") and collapses. No inline polling is performed — the
gateway convergence badge (WEB-1009) reflects pending rotation status via
the normal dashboard auto-refresh cycle.

The dashboard auto-refresh (WEB-0103) MUST be paused while the rotation
form is expanded or a submission is in progress, to prevent DOM replacement
from destroying form state or interrupting key derivation. Auto-refresh
resumes when the form collapses.

**Acceptance criteria:**

1. The inline form validates rotation code format as 6 characters from `[A-Z0-9]`.
2. The inline form validates the passphrase length requirement.
3. Unsupported browsers show the action as disabled with an explanatory message.
4. After submission the form shows a success message and collapses.
5. Dashboard auto-refresh is paused while the rotation form is expanded.

---

### WEB-1003  Passphrase-Based Key Derivation

**Priority:** Must
**Source:** Issue #962
**Confidence:** High

**Description:**
The SPA MUST derive the new master key using Argon2id with hardcoded KDF v1
parameters (`m_cost=65536`, `t_cost=3`, `p_cost=1`, `output_len=32`) via a
WASM Argon2id implementation (e.g., `argon2-browser`). Salt is derived from
the deployment label: `SHA-256("sonde-kdf-v1:" || utf8(deployment_label))[0..16]`.
The SPA MUST NOT read KDF parameters or salt from gateway ACTUAL_STATE.
Passphrase and derived key material MUST be cleared from JavaScript variables
after use on a best-effort basis.

**Acceptance criteria:**

1. The derived key matches the gateway's expected output for the same inputs.
2. Hardcoded KDF v1 parameters are always used.
3. Key material is cleared after use on a best-effort basis.

---

### WEB-1004  Rotation Payload Construction

**Priority:** Must
**Source:** Issue #962, evolve-962 §2.6.1
**Confidence:** High

**Description:**
The SPA MUST construct a `RotationPayloadV1` binary payload with the following
format: version byte `0x01`; a fresh ephemeral X25519 keypair generated with a
CDN-loaded `noble-curves` implementation; shared secret
`X25519(ephemeral_private, gateway_x25519_public_key)`; HKDF-SHA-256 derived key
using `shared_secret`, `hkdf_salt = b"sonde-rotation-v1"`, and
`info = gateway_id_raw || current_master_key_epoch_be64`; a random 12-byte nonce
from `crypto.getRandomValues()`; CBOR plaintext map
`{1: new_master_key, 2: rotation_code}`. CBOR keys 3–5 are RESERVED and MUST
NOT be included; AES-256-GCM ciphertext using the derived key, nonce, and
`aad = gateway_id_raw || current_master_key_epoch_be64`; and final output
`version || ephemeral_public || nonce || ciphertext_and_tag`.

**Acceptance criteria:**

1. The payload format matches evolve-962 §2.6.1.
2. A fresh ephemeral keypair is generated for each rotation.
3. `gateway_id` in the AEAD AAD is the raw 16-byte value, not a hex string.

---

### WEB-1005  Rotation Submission

**Priority:** Must
**Source:** Issue #962
**Confidence:** High

**Description:**
The SPA MUST write the rotation payload into the gateway's DESIRED_STATE row in
the `desiredstate` Azure Table. The row MUST use
`PartitionKey = "g:" + gateway_id_hex`, a reverse-timestamp `RowKey` using the
same format as node desired state rows, and a `rotation_payload` entity property
encoded as binary data for the Azure Table REST API. The row MUST also include a
`submitted_epoch` property (Edm.Int64) set to the gateway's current
`master_key_epoch` at the time of submission, to enable convergence tracking
(WEB-1009).

**Acceptance criteria:**

1. A DESIRED_STATE row is created in the `desiredstate` table.
2. The `PartitionKey` uses the `g:` gateway prefix.
3. The row includes `submitted_epoch` matching the current `master_key_epoch`.

---

### WEB-1006  Rotation Status Monitoring

**Priority:** ~~Must~~ **Retired** (Issue #1092)
**Source:** Issue #962
**Confidence:** High

**Description:**
~~After submitting a rotation payload, the SPA MUST poll the latest gateway
ACTUAL_STATE row every 5 seconds for up to 120 seconds and watch for
`master_key_epoch` to increment.~~

**Retired:** Replaced by WEB-1009 (gateway convergence badge). Rotation
status is now reflected by the Aligned/Diverged badge on the gateway status
card, updated via the dashboard auto-refresh cycle (WEB-0103). No dedicated
inline polling is performed.

**Acceptance criteria:**

1. ~~Success is detected within the 120-second polling window.~~ Retired.
2. ~~Timeout is handled gracefully.~~ Retired.
3. ~~A progress indicator is shown while polling.~~ Retired.

---

### WEB-1007  Gateway ACTUAL_STATE Read

**Priority:** Must
**Source:** Issue #962
**Confidence:** High

**Description:**
The SPA MUST read the gateway ACTUAL_STATE from the `actualstate` Azure Table
by querying for rows with `PartitionKey` values that start with `g:` and
selecting the latest row per gateway partition using `latestByPartition`
(lexicographically smallest reverse-timestamp `RowKey`). The
`gateway_id` is discovered by querying for rows with `PartitionKey` values that
start with `g:`. If multiple gateways exist, the SPA MUST display all of them
and let the operator select one.

**Acceptance criteria:**

1. The gateway row is read successfully.
2. Multiple gateways are handled.
3. A missing gateway row shows `No gateway connected`.

---

### WEB-1008  Browser-Side Key Hygiene

**Priority:** Must
**Source:** Issue #962, security review
**Confidence:** High

**Description:**
The SPA MUST NOT store the passphrase, master key, or derived key material in
`localStorage`, `sessionStorage`, or cookies. The encrypted `rotation_payload`
is the only cryptographic artifact written to Azure. After rotation completes or
fails, the SPA SHOULD overwrite JavaScript variables that held key material with
zeros on a best-effort basis.

**Acceptance criteria:**

1. No key material is stored in browser storage.
2. Only `rotation_payload` is written to Azure.
3. Key variables are cleared after use on a best-effort basis.

---

### WEB-1009  Gateway Convergence Display

**Priority:** Must
**Source:** Issue #1092
**Confidence:** High

**Description:**
The gateway status card (WEB-1001) MUST display an Aligned / Diverged
convergence badge by cross-referencing gateway desired-state rows in the
`desiredstate` Azure Table (`PartitionKey` starting with `"g:"`) against
the gateway ACTUAL_STATE row.

The convergence check compares the following fields:
- **`rotation_payload`:** Diverged if the latest desired-state row has a
  non-null `rotation_payload` AND the actual `rotation_in_progress` is
  `false` AND `master_key_epoch` has not advanced past the epoch at which
  the rotation was submitted. The SPA MUST store the current
  `master_key_epoch` as `submitted_epoch` (Edm.Int64) in the desired-state
  row when submitting a rotation payload. Divergence condition:
  `desired.rotation_payload != null AND actual.rotation_in_progress !== true
  AND actual.master_key_epoch <= desired.submitted_epoch`. Once
  `rotation_in_progress` is `true` or `master_key_epoch >
  desired.submitted_epoch`, the rotation is considered consumed.
- **`channel`:** Diverged if the desired `channel` is non-null and differs
  from actual `channel`.
- **`salt`:** *(Retired — no longer tracked for convergence.)*
- **`kdf_params`:** *(Retired — no longer tracked for convergence.)*

`recovered_psks` is excluded from convergence — it is a background queue
not visible to the operator.

If no gateway desired-state row exists, the gateway is "Aligned" (no
pending changes).

The badge uses the same CSS classes as the node convergence badge (`badge
success` for Aligned, `badge warning` for Diverged).

**Acceptance criteria:**

1. Gateway with no desired-state row shows "Aligned."
2. Gateway with a pending `rotation_payload` (not yet consumed, i.e.,
   `actual.master_key_epoch <= desired.submitted_epoch` and
   `rotation_in_progress` is false) shows "Diverged."
3. Gateway where `rotation_in_progress` is `true` or
   `master_key_epoch > submitted_epoch` shows "Aligned" (rotation
   consumed).
4. Gateway with desired `channel` differing from actual shows "Diverged."
5. *(Reserved — previously salt divergence, now retired.)*
6. *(Reserved — previously salt alignment, now retired.)*
7. *(Reserved — previously KDF params divergence, now retired.)*
8. *(Reserved — previously KDF params alignment, now retired.)*
9. Badge uses the same CSS styling as the node convergence badge.

---

## 16  Dependencies

### DEP-001  MSAL.js Browser Library

MSAL.js 2.39.0 loaded from `cdn.jsdelivr.net`. Provides Entra ID
authentication (authorization code flow + PKCE).

### DEP-002  Chart.js

Chart.js 4.4.9 loaded from `cdn.jsdelivr.net` with SRI hash. Provides
time-series charting for the Sensor Data tab.

### DEP-003  Azure Storage Tables REST API

Azure Tables REST API (version `2019-02-02`) for reading/writing `actualstate`,
`desiredstate`, `programs`, and `sensordata` tables.

### DEP-004  ProgramIngest Azure Function

HTTP-triggered Azure Function that accepts ELF uploads, runs Prevail
verification, and stores verified program images in the `programs` table.

### DEP-005  Azure App Service Authentication (EasyAuth)

EasyAuth configured on the Function App to validate Entra ID bearer tokens on
HTTP routes.

---

## 17  Assumptions

### ASM-001  Azure Public Cloud Only

The SPA targets Azure public cloud. Sovereign cloud support
(`login.microsoftonline.us`, etc.) is out of scope.

### ASM-002  Modern Browser

The SPA requires a modern browser with ES2020+ support (async/await, BigInt,
nullish coalescing, optional chaining, `SubtleCrypto`).

### ASM-003  localStorage Availability

The environment manager requires `localStorage` to be available and writable.
If disabled (e.g., private browsing in some browsers), the SPA cannot store
environment configuration.

---

## 18  Risks

### RISK-001  CDN Availability

If `cdn.jsdelivr.net` is unreachable, MSAL.js and Chart.js will not load,
breaking authentication and charting respectively.

### RISK-002  Azure Table Query Limits

Sensor data queries are limited to `$top=1000` rows per partition. For
high-frequency sensors over long time ranges, data may be incomplete.

### RISK-003  localStorage Quota

Environments and series overrides are stored in `localStorage`. Browser-imposed
quota limits could prevent saving if other origins on the same domain consume
storage.

---

## 19  Revision History

| Date | Author | Description |
|------|--------|-------------|
| 2026-05-19 | Spec extraction (automated) | Initial extraction from web-ui-design.md, app.js, and web-ui-validation.md. |
