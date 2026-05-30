<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Handler Design Specification

> **Document status:** Draft
> **Scope:** Internal design for the Azure cloud-side handler hosted in the
> Azure Function App. Covers upstream connector message intake, Azure Table
> schemas, node-state reconciliation, downstream `GW-0811` publication,
> `GW-0813` sensor data storage, and BPF program ingestion.
> **Audience:** Implementers building the Azure Function App and reviewers
> auditing traceability to the gateway connector contract.
> **Related:** [azure-handler-requirements.md](azure-handler-requirements.md),
> [azure-handler-validation.md](azure-handler-validation.md),
> [azure-provisioning-design.md](azure-provisioning-design.md),
> [gateway-companion-api.md](gateway-companion-api.md)

---

## 1  Overview

The Azure handler is the cloud-side counterpart to the implemented
`sonde-azure-companion` bridge. The bridge remains a transparent transport
adapter between the gateway's local connector socket and Azure Storage Queue. The
Azure handler owns the first Sonde-aware cloud logic:

1. consume upstream connector messages from the upstream queue,
2. append node-scoped `GW-0812` actual-state messages to actual-state history,
3. compare the latest eligible actual-state row against the latest desired-state
   row and publish a complete node-scoped `GW-0811` when they diverge,
4. store `GW-0813` application-data messages in the `SensorData` table, and
5. accept BPF program ELF uploads via an HTTP trigger, verify and store them in
   the `Programs` table for downstream embedding.

The handler does not replace the gateway's reconciler model. It expresses cloud
intent only by publishing `GW-0811` desired-state messages.

---

## 2  Runtime topology

> **Requirements:** AZH-0100, AZH-0101, AZH-0400

The Azure handler runs inside the Azure Function App provisioned by the Bicep
stack. The Function App uses a system-assigned managed identity with:

1. receive permission on the upstream queue,
2. send permission on the downstream queue,
3. append permission on `ActualNodeState`, read permission on
   `DesiredNodeState`, and read/write permission on `SensorData`, and
4. read/write permission on `Programs` (read for ELF embedding in `GW-0811`,
   write for program ingestion via the `ProgramIngest` HTTP trigger).

The final permission set covers the handler's own tables. No external handler
queue permissions are required.

### 2.1  Trigger model

The Azure handler is deployed as an Azure Functions **Custom Handler**. Instead
of running in-process, the Functions host forwards trigger invocations as HTTP
requests to an Axum HTTP server listening on the port specified by the
`FUNCTIONS_CUSTOMHANDLER_PORT` environment variable (default `3000`).

The server exposes three routes:

| Route | Trigger type | Purpose |
|-------|-------------|---------|
| `POST /` | Storage Queue | Upstream connector message dispatch |
| `POST /{*path}` | Storage Queue (fallback) | Same as above — catches any function name path |
| `POST /ProgramIngest` | HTTP (`/api/programs/ingest`) | Program ingestion (§8) |

#### Queue trigger envelope

For Storage Queue-triggered invocations the Functions host wraps the queue
message in a JSON envelope. The handler extracts the raw connector payload by
probing the following paths in order:

1. `data.message`
2. `Data.message`
3. `Data` (top-level)
4. `Body` or `body` (top-level)
5. If `data` is an object with exactly one key, use that key's value.

The extracted value is then decoded as:

- a JSON **string** — stripped of surrounding double-quotes if present, then
  base64-decoded (falling back to raw UTF-8 bytes if base64 fails),
- a JSON **array** — interpreted as a byte array of `u8` values, or
- a JSON **object** — re-serialized as JSON bytes.

#### HTTP trigger envelope

For the `ProgramIngest` HTTP trigger the Functions host wraps the HTTP request
body in an envelope at `Data.req.Body` (case-insensitive: `data`/`Data`,
`req`/`Req`, `Body`/`body`). The handler extracts and JSON-parses the body
string.

#### Dispatch rules

After extracting the connector payload from a queue trigger, the handler
performs the following dispatch:

1. decode the top-level connector `msg_type`,
2. if `msg_type = ACTUAL_STATE` and `entity_kind = "node"`, invoke node-state
   reconciliation,
3. if `msg_type = APP_DATA`, invoke sensor data storage, and
4. otherwise log the unsupported or out-of-scope message and complete without
   mutating handler-owned tables.

The handler does not require a second inbound trigger for downstream traffic
because it publishes `GW-0811` by calling the downstream queue sender directly.

---

## 3  Connector message interpretation

> **Requirements:** AZH-0100, AZH-0201, AZH-0202, AZH-0203

### 3.1  `GW-0812` fields consumed by the handler

The handler decodes the node-scoped `ACTUAL_STATE` connector payload using the
schema defined in [gateway-companion-api.md](gateway-companion-api.md). For
node reconciliation it consumes:

1. `entity_id` as `node_id`,
2. `current_program_hash`,
3. `assigned_program_hash`,
4. `battery_mv`,
5. `wake_rssi_dbm`,
6. `firmware_abi_version`,
7. `firmware_version`,
8. `timestamp_ms` as last check-in time, and
9. `schedule_interval_s`.

Gateway-scoped and phone-scoped `ACTUAL_STATE` messages (`entity_kind` values
other than `"node"`) are outside the node-table ownership of this document and
are therefore logged and ignored by the handler.

### 3.2  `GW-0813` fields consumed by the handler

For `APP_DATA`, the handler decodes:

1. `node_id`, `timestamp_ms`, raw `blob`, and optional `readings` (key 16)
   for `SensorData` table storage (§6.1), and
2. `program_hash` for the `SensorData` row's `program_hash` column.

Fields beyond `program_hash` were previously opaque to the handler; the
`SensorData` feature (AZH-0500) extends the handler's parsing scope.

---

## 4  Azure Table schemas

> **Requirements:** AZH-0200, AZH-0205, AZH-0206, AZH-0500, WEB-0304

The design uses four Azure Tables:

1. **`ActualNodeState`** — append-only actual-state history keyed for latest-per-node queries.
2. **`DesiredNodeState`** — append-only desired-state history keyed for latest-per-node queries.
3. **`SensorData`** — append-only sensor data history (§6.1).
4. **`Programs`** — BPF program image store, append-only, written by `ProgramIngest` (§8).

### 4.1  `ActualNodeState` schema

Each row uses:

- `PartitionKey = "n:" + lowercase hex-encoded SHA-256(node_id UTF-8 bytes)`
- `RowKey = <reverse_tick_hex>:<reverse_sequence_hex>:<process_nonce_hex>` (see [History RowKey format](#history-rowkey-format) below)

The row contains the following logical columns:

| Column | Purpose |
|--------|---------|
| `entity_kind` | Entity kind string. In-memory only — not persisted as an Azure Table property. Currently hard-coded to `"node"` on deserialization since the handler only processes node-scoped rows. Present in the handler's row model for dispatch routing. |
| `node_id` | Original opaque node identifier used by gateway and handlers. |
| `observed_current_program_hash` | Node-reported current resident program hash, nullable. |
| `observed_assigned_program_hash` | Gateway-reported assigned resident program hash, nullable. |
| `observed_schedule_interval_s` | Gateway-reported node schedule interval, nullable. |
| `battery_mv` | Latest battery reading from `GW-0812`, nullable. |
| `wake_rssi_dbm` | Modem-measured receive RSSI (dBm) of the WAKE frame, nullable. |
| `firmware_abi_version` | Latest firmware ABI version, nullable. |
| `firmware_version` | Latest firmware version, nullable. |
| `timestamp_ms` | Check-in time carried by the source `GW-0812`. |

The node-scoped `PartitionKey` keeps each node's history in one queryable
partition. The reverse-tick `RowKey` prefix makes newer timestamps sort first.
See [History RowKey format](#history-rowkey-format) for the complete three-part
scheme.

### 4.2  `DesiredNodeState` schema

Each row uses:

- `PartitionKey = "n:" + lowercase hex-encoded SHA-256(node_id UTF-8 bytes)`
- `RowKey = <reverse_tick_hex>:<reverse_sequence_hex>:<process_nonce_hex>` (see [History RowKey format](#history-rowkey-format))

The row contains:

| Column | Purpose |
|--------|---------|
| `node_id` | Original opaque node identifier used by gateway and handlers. |
| `desired_assigned_program_hash` | Cloud-authored desired resident program hash, nullable. |
| `desired_schedule_interval_s` | Cloud-authored desired schedule interval, nullable. |
| `timestamp_ms` | Time associated with the desired-state request. |

The Azure handler reads this table but does not write it. Admin/control-plane
surfaces append desired-state rows when requested state changes.

### 4.3  History RowKey format

All history tables (`ActualNodeState`, `DesiredNodeState`, and `SensorData`)
use the same three-part `RowKey` format:

```
{reverse_tick_hex}:{reverse_sequence_hex}:{process_nonce_hex}
```

Each component is a 16-character, zero-padded, lowercase hexadecimal `u64`:

| Component | Value | Purpose |
|-----------|-------|---------|
| `reverse_tick_hex` | `u64::MAX - timestamp_ms` | Newest timestamps sort first for `Top(1)` queries. |
| `reverse_sequence_hex` | `u64::MAX - sequence` | Monotonically incrementing per-process counter (reversed). Within one handler process lifetime, later appends sort before earlier appends when timestamps are equal. |
| `process_nonce_hex` | Random `u64` generated once at process startup | Provides probabilistic uniqueness across concurrent handler instances and restarts; collisions are negligibly unlikely. |

The `":"` separators ensure that each component is compared independently during
lexicographic ordering. Across restarts or concurrent handler instances,
equal-timestamp row ordering is intentionally unspecified, so the reconciliation
path must not depend on `Top(1)` returning the most recently appended
equal-timestamp row.

### 4.4  `Programs` table schema

The `Programs` table stores ingested BPF program images. It is written by the
`ProgramIngest` HTTP trigger (§8) and read by the reconciliation path when
embedding ELF images in downstream `GW-0811` payloads (§5 step 11).

Each row uses:

- `PartitionKey = "program"` (single partition for all programs)
- `RowKey = lowercase hex-encoded program_hash`

| Column | Type | Purpose |
|--------|------|---------|
| `cbor_image` | `String` (base64) | CBOR-encoded BPF program image extracted from ELF. |
| `elf_image` | `String` (base64) | Original uploaded ELF binary, max 1 MB. Used for inline embedding in `GW-0811`. |
| `source_filename` | `String` | Normalized source filename (basename only), nullable. |
| `abi_version` | `Edm.Int32` | Firmware ABI version the program targets, nullable. |
| `size_bytes` | `Edm.Int32` | CBOR image size in bytes. |
| `verification_profile` | `String` | `"resident"` or `"ephemeral"`. |
| `created_at` | `String` | ISO 8601 UTC timestamp of ingestion. |

Programs are inserted (append-only) keyed by `program_hash`. Re-ingesting the
same ELF is a no-op — the existing row is preserved unchanged. Since
`program_hash` is SHA-256 of the CBOR image, identical content always produces
the same hash. First-writer-wins: the response may echo the new request's
metadata, but the stored row retains original values.

---

## 5  Node-state reconciliation algorithm

> **Requirements:** AZH-0201, AZH-0202, AZH-0203, AZH-0204, AZH-0207

For each node-scoped `GW-0812`, the handler performs the following sequence:

1. Append one `ActualNodeState` row for the received `GW-0812`.
2. Query `Top(1)` from `ActualNodeState` for the node's partition.
3. If the newest actual-state row has a `timestamp_ms` greater than the
   appended row's `timestamp_ms`, treat the incoming message as out-of-order and
   complete the invocation without downstream publication.
4. Otherwise, use the appended row as the actual-state input for this
   invocation's divergence evaluation. Equal-timestamp ordering among history
   rows is retained for diagnostics, but reconciliation correctness does not
   depend on `Top(1)` choosing the just-appended row when multiple rows share
   the same `timestamp_ms`.
5. Query `Top(1)` from `DesiredNodeState` for the same node partition.
6. If no desired-state row exists, complete the invocation without downstream
   publication.
7. If the desired-state row's `node_id` payload does not exactly match the
   current `entity_id`, fail the invocation rather than publishing a command
   for potentially corrupted cross-node state.
8. If `desired_assigned_program_hash` is non-null, compare it to
   `observed_current_program_hash` from the appended row selected for
   evaluation.
9. If `desired_schedule_interval_s` is non-null, compare it to
   `observed_schedule_interval_s` from the appended row selected for
   evaluation.
10. If neither comparison diverges, complete the invocation with no downstream
   publication.
11. If either comparison diverges, build one complete `GW-0811`
   `DESIRED_STATE` payload using:
   1. `entity_kind = "node"`,
   2. `entity_id = node_id`,
   3. `assigned_program_hash = desired_assigned_program_hash` from the latest desired row,
   4. `schedule_interval_s = desired_schedule_interval_s` from the latest desired row,
   5. if `assigned_program_hash` diverges and a program row exists for that hash,
      fetch the program row and embed `elf_image` at key 5
      (`assigned_program_elf`), `verification_profile` at key 6, `source_filename`
      at key 7, and `abi_version` at key 8. If the program row exists but
      `elf_image` is absent (legacy row ingested before ELF storage was added),
      omit key 5 and log a warning; the gateway will reject the message if it
      does not already have the program locally. To populate `elf_image`,
      delete the legacy row from the `Programs` table and re-ingest the
      program.
12. Publish that payload to the downstream queue.

`ephemeral_program_hash` is intentionally omitted in v1. The gateway connector
schema already defines it, but this design does not add Azure-side ownership or
comparison rules for it yet.

A null desired program or schedule field means the cloud is intentionally not
asserting a target for that field. The reconciliation algorithm therefore skips
that comparison instead of republishing `GW-0811` forever against a non-null
observed value. Because desired-state history is admin-authored, the handler
never seeds or updates desired-state rows while processing `GW-0812`.

---

## 6  `GW-0813` sensor data storage

> **Requirements:** AZH-0500, AZH-0501, AZH-0502

For each `GW-0813` invocation:

1. decode the top-level connector payload enough to extract `program_hash`,
   `node_id`, `timestamp_ms`, raw `blob`, and optional `readings` (key 16),
2. append a `SensorData` row (§6.1) using the extracted fields. To avoid
   duplicate rows on at-least-once retries, derive the `RowKey` uniqueness
   suffix from the upstream queue message ID (or connector envelope
   sequence number). This makes the `SensorData` write idempotent — a
   retry with the same message produces the same `RowKey` and overwrites
   the existing row rather than appending a duplicate. Do NOT use a hash
   of the raw payload alone, as distinct messages with identical payloads
   would collide, and
3. complete the invocation successfully.

### 6.1  SensorData table storage (AZH-0500)

In addition to node-state reconciliation, the Azure handler MUST
append a row to the `SensorData` table for every `GW-0813` message. This
provides a queryable time-series store of sensor readings for the SPA.

**Table schema:**

| Column | Type | Description |
|--------|------|-------------|
| `PartitionKey` | `String` | `"n:" + lowercase-hex-encoded SHA-256(node_id UTF-8 bytes)` |
| `RowKey` | `String` | Reverse-tick key + `":"` + uniqueness suffix |
| `node_id` | `String` | Originating node identifier |
| `timestamp_ms` | `Edm.Int64` | Message timestamp in milliseconds |
| `program_hash` | `String` | BPF program hash (hex) |
| `raw_payload` | `String` | Base64-encoded raw APP_DATA blob |
| `decoded_readings` | `String` | JSON string of `readings` map, or `""` |

If the upstream CBOR message contains a `readings` key (CBOR key 16, added by
gateway decoder enrichment per GW-1903), the handler extracts it and serializes
as JSON into `decoded_readings`. Int64 values within JavaScript's
`Number.MAX_SAFE_INTEGER` (2^53 - 1) are encoded as JSON numbers; values
exceeding that threshold are encoded as JSON strings to preserve precision
(AZH-0501 AC-5). Otherwise `decoded_readings` is `""`.

The `PartitionKey` and `RowKey` follow the same patterns as `ActualNodeState`
— hashed partition key for safe table keys, three-part history RowKey (§4.3)
for chronological ordering and append uniqueness.

`SensorData` writes complete the `GW-0813` handling path — no further routing
or delivery is performed.

### 6.2  SensorData query boundary (AZH-0502)

The handler owns the `SensorData` table schema (§6.1) and is responsible for
writing rows that support the query patterns required by AZH-0502. The schema
choices that enable SPA queries are:

1. **Node-scoped partition key** — `"n:" + lowercase-hex-encoded SHA-256(node_id
   UTF-8 bytes)` enables `PartitionKey` equality filters for per-node queries.
2. **Reverse-tick row key** — newest-first (reverse-chronological) ordering
   enables `RowKey` range filters for time-range queries. To query a time
   window `[start_ms, end_ms]`, the SPA computes reverse-tick values
   (`u64::MAX - ts`) for each bound. Because the mapping inverts the
   ordering, the lexicographic range is lower-bounded by
   `reverse_tick(end_ms)` (inclusive) and upper-bounded by
   `reverse_tick(start_ms - 1)` (inclusive), selecting all rows whose
   reverse-tick prefix falls within that interval. The `":"` separator
   between the reverse-tick prefix and the uniqueness suffix ensures that
   suffix bytes do not interfere with prefix-based range comparisons.
3. **`program_hash` property** — stored as a top-level `Edm.String` column,
   enabling property-filter queries within a single node partition.

The handler does not expose an HTTP query endpoint. The SPA queries the Azure
Table Storage REST API directly using the logged-in user's Entra bearer token.
Cross-node program-hash queries are performed as parallel per-node requests by
the SPA (AZH-0502 AC-2).

Read access for the SPA's Entra identity and query performance requirements
(AZH-0502 AC-3, AC-4) are owned by the provisioning stack. See
[azure-provisioning-design.md](azure-provisioning-design.md) for RBAC
configuration.

---

## 7  Failure handling

> **Requirements:** AZH-0400, AZH-0700

The handler follows a fail-closed rule for all Azure Table and Storage Queue
operations that determine externally visible control-plane behavior:

1. Table read/write failure aborts the invocation.
2. Downstream `GW-0811` publish failure aborts the invocation.
3. `SensorData` table append failure aborts the invocation.

This failure model preserves at-least-once retry behavior from the Azure
Function runtime instead of silently pretending that state was reconciled or
sensor data was stored.

### 7.1  SDK workaround — missing `Server` response header

> **Requirements:** AZH-0700

Some Azure Table Storage stamps omit the `Server` HTTP response header that
`azure_storage` 0.21.0 `CommonStorageResponseHeaders` unconditionally
requires (upstream: azure-sdk-for-rust#4489). Without mitigation, all table
operations fail with `"header not found server"`.

The handler injects an `InjectServerHeaderPolicy` into the
`TableServiceClientBuilder` pipeline (via `per_call_policies`) that adds a
synthetic `Server: Windows-Azure-Table/1.0 Microsoft-HTTPAPI/2.0` header to
responses when one is missing. The policy is a no-op when the header is
already present. This uses the SDK's intended extension mechanism and is
removable when the upstream fix lands.

---

## 8  Program ingestion

> **Requirements:** WEB-0300, WEB-0301, WEB-0302, WEB-0303, WEB-0304,
> WEB-0305, WEB-0306, WEB-0307, WEB-0308
> (defined in [web-ui-requirements.md](web-ui-requirements.md))

The Azure handler hosts the `ProgramIngest` HTTP trigger endpoint used by the
SPA (and any authorized client) to upload BPF ELF binaries for verification
and storage.

### 8.1  Endpoint

| Property | Value |
|----------|-------|
| External route | `POST /api/programs/ingest` |
| Auth level | `anonymous` (authentication enforced by Function App EasyAuth; see WEB-0503, WEB-0606, WEB-0607 in [web-ui-requirements.md](web-ui-requirements.md) and [azure-provisioning-design.md](azure-provisioning-design.md)) |
| Custom Handler route | `POST /ProgramIngest` |
| Binding | `ProgramIngest/function.json` — HTTP trigger in, HTTP out (`res`) |

### 8.2  Request schema

The request body is JSON:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `elf` | `String` (base64) | Yes | Base64-encoded ELF binary. |
| `source_filename` | `String` | No | Original filename. Normalized to basename only (path components stripped). |
| `abi_version` | `Integer` | No | Firmware ABI version the program targets. Must be non-negative and ≤ `i32::MAX` (2,147,483,647). |
| `verification_profile` | `String` | No | `"resident"` (default) or `"ephemeral"`. Controls BPF verification profile and CBOR image size limits. |

### 8.3  Validation rules

1. `elf` must be present, non-empty, and valid base64.
2. Decoded ELF size must not exceed 1 MB (1,048,576 bytes). A pre-decode
   length check on the base64 string rejects obviously oversized payloads
   before allocating the decoded buffer.
3. `verification_profile`, if present, must be exactly `"resident"` or
   `"ephemeral"`.
4. `abi_version`, if present, must be a non-negative integer within
   `Edm.Int32` range.
5. The ELF binary is passed through `ProgramLibrary::ingest_elf()` for BPF
   bytecode extraction and Prevail verification. Verification failure is
   rejected with HTTP 422.

### 8.4  Response schema

On success (HTTP 200):

| Field | Type | Description |
|-------|------|-------------|
| `program_hash` | `String` | Lowercase hex SHA-256 of the CBOR program image. |
| `size` | `Integer` | CBOR image size in bytes. |
| `abi_version` | `Integer` | Echo of the supplied ABI version, if any. |
| `source_filename` | `String` | Normalized filename, if any. |

On error, the response body is `{"error": "<message>"}` with an HTTP status
code embedded in the Azure Functions output binding envelope:

| Status | Condition |
|--------|-----------|
| 400 | Missing or malformed fields, invalid base64, empty ELF. |
| 413 | ELF exceeds 1 MB size limit. |
| 422 | BPF verification/ingestion failed (invalid bytecode, size limit exceeded by verification profile). |
| 500 | Internal error (store write failure). |

### 8.5  Storage

On successful verification the handler inserts one row in the `Programs` table
(§4.4) containing both the CBOR-encoded program image and the original ELF
binary. If the row already exists (duplicate `program_hash`), the insert is
treated as a successful no-op — only an entity-already-exists conflict is
suppressed; other errors propagate. The ELF is retained so the reconciliation
path (§5 step 11) can embed it in downstream `GW-0811` messages without
requiring a separate ELF store.

---

## 9  PSK escrow state storage and recovery

> **Requirements:** AZH-0600, AZH-0601, AZH-0602, AZH-0603, AZH-0604, AZH-0605

The PSK escrow redesign extends the handler's ACTUAL_STATE and DESIRED_STATE
storage so gateway escrow state is stored alongside existing node reconciliation
data. Gateway rows use the same reverse-timestamp history keying as node rows,
not a singleton upsert.

### 9.1  ACTUAL_STATE schema extension

The `actualstate` Azure Table is extended with the following escrow-related
columns:

| Column | Type | Applicable entity_kind | Description |
|--------|------|------------------------|-------------|
| `encrypted_psk` | `Binary`/null | node | Raw encrypted PSK blob stored with node ACTUAL_STATE. |
| `master_key_id` | `Binary`/null | node, gateway | 32-byte `SHA-256(master_key)` identifier used for escrow matching. |
| `key_hint` | `Edm.Int64`/null | node | Recovery lookup hint for node PSKs. |
| `x25519_public_key` | `Binary`/null | gateway | Gateway X25519 public key. |
| `channel` | `Edm.Int64`/null | gateway | Current ESP-NOW channel. |
| `master_key_epoch` | `Edm.Int64`/null | gateway | Current gateway master-key epoch. |
| `gateway_version` | `String`/null | gateway | Gateway binary semver. |
| `gateway_commit` | `String`/null | gateway | Gateway binary git commit. |
| `modem_firmware_version` | `String`/null | gateway | Modem firmware semver. |
| `modem_firmware_commit` | `String`/null | gateway | Modem firmware git commit. |
| `missing_key_hints` | `String`/null | gateway | JSON array of missing key hints reported by the gateway. |
| `fingerprint_words` | `String`/null | gateway | JSON array of 6 BIP-39 fingerprint words. |
| `rotation_in_progress` | `Edm.Boolean`/null | gateway | `true` if a key rotation is in progress. |

Phone ACTUAL_STATE is not escrowed and is therefore not stored in this table.

Gateway rows use `PartitionKey = "g:" + gateway_id_hex` and a
reverse-timestamp history `RowKey` generated by `next_history_row_key()`, the
same keying scheme used for node rows. Each gateway state update appends a new
row unless the incoming message is stale (its `timestamp_ms` is older than the
previously stored latest row), in which case the message is silently discarded
without appending. `load_gateway_actual_state` returns only the latest row by
querying `$top=1` within the gateway partition (inverted timestamps sort
newest-first).

**Legacy migration:** Existing deployments may contain a singleton row with
`RowKey = "state"`. On the first post-migration `load_gateway_actual_state`
call, if the only row in the partition has `RowKey = "state"`, the handler
treats it as the latest row. Once a new history row is appended, the legacy
singleton row is ignored (it sorts lexicographically after all reverse-timestamp
rows and is never returned by `$top=1`).

Node row keying is unchanged from the existing design: node ACTUAL_STATE rows
remain keyed by `PartitionKey = "n:" + SHA256(node_id)` with reverse-timestamp
history `RowKey` values.

### 9.2  ACTUAL_STATE handling by entity kind

For `ACTUAL_STATE` with `entity_kind = "gateway"`, the handler:

1. loads the current latest gateway row (if any) for comparison purposes
   (e.g., `master_key_epoch` change detection in §9.4),
2. appends a new history row in `actualstate` (same reverse-timestamp keying as nodes),
3. stores all gateway escrow and recovery fields from the message, and
4. triggers recovery work if `missing_key_hints` is non-empty.

For `ACTUAL_STATE` with `entity_kind = "node"`, the handler continues storing
node observation data in the existing node row shape and additionally persists
`encrypted_psk`, `master_key_id`, and `key_hint` for later recovery queries.

For `ACTUAL_STATE` with `entity_kind = "phone"`, the handler does not create an
`actualstate` row because phones are not part of PSK escrow recovery.

### 9.3  Missing `key_hint` recovery

When a gateway row reports non-empty `missing_key_hints`, the handler should
latch or enqueue recovery work immediately because later gateway ACTUAL_STATE
messages may report different hints (the field is one-shot per emission).

Recovery processing is:

1. read the gateway row's `master_key_id` and each reported `key_hint`,
2. query node rows whose `key_hint` matches a reported hint,
3. filter matches to rows whose `master_key_id` exactly matches the gateway's
   reported `master_key_id`, and
4. construct `recovered_psks` records for the next gateway DESIRED_STATE using
   node `entity_id`, `key_hint`, `encrypted_psk`, and `master_key_id`.

This match on both `key_hint` and `master_key_id` prevents the handler from
returning PSKs encrypted under an older or newer master-key era.

### 9.4  Rotation payload relay

The SPA writes gateway rotation intent to the `desiredstate` table. The handler
reads that opaque payload and includes it in the next gateway DESIRED_STATE as
`rotation_payload` (CBOR key 28 inside map key 4) without inspecting or
rewriting the payload.

The handler clears `rotation_payload` after observing a gateway ACTUAL_STATE row
whose `master_key_epoch` has incremented relative to the previously stored
gateway row. Clearing is performed by appending a **new** row to the
`desiredstate` table (with a fresh `next_history_row_key()` RowKey and
`rotation_payload: None`) rather than overwriting the original SPA-written row.
The `load_gateway_desired_state` reader returns `$top=1` from the partition, so
the newly appended row (which sorts first due to the inverted-timestamp RowKey)
is picked up as the latest state. This preserves the original SPA-written row
for audit purposes while making the DESIRED_STATE relay one-shot.

### 9.5  Gateway DESIRED_STATE construction — KDF fields retired

KDF parameters and salt are client-side concerns only (GW-2020, GW-2021).
The handler does not store or relay salt or KDF parameters.

Gateway DESIRED_STATE uses:

- `entity_kind = "gateway"`
- `entity_id = hex(gateway_id)`
- `PartitionKey = "g:" + gateway_id_hex` in the `desiredstate` table
- CBOR key 15 for `channel`
- CBOR key 28 for `rotation_payload`
- CBOR key 29 for `recovered_psks`

Keys 21 (`salt`) and 22 (`kdf_params`) are RESERVED and not emitted.
