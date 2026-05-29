<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Handler Requirements Specification

> **Document status:** Draft
> **Source:** Azure handler discovery review, `GW-0811`/`GW-0812`/`GW-0813`,
> and the implemented Azure companion bridge architecture.
> **Scope:** This document covers the Azure cloud-side handler hosted in the
> Azure Function App provisioned for the Sonde Azure integration. It owns the
> Azure Table state used for node reconciliation, consumes upstream connector
> traffic from Storage Queue, emits downstream `GW-0811` desired-state messages,
> and stores `GW-0813` application-data messages in the `SensorData` table. It
> does not cover the gateway-local Azure companion bridge or Azure resource
> provisioning beyond the handler's runtime dependencies.
> **Related:** [gateway-companion-api.md](gateway-companion-api.md),
> [gateway-requirements.md](gateway-requirements.md),
> [azure-handler-design.md](azure-handler-design.md),
> [azure-handler-validation.md](azure-handler-validation.md),
> [azure-provisioning-requirements.md](azure-provisioning-requirements.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure handler** | The Azure-hosted control-plane process that consumes upstream Sonde connector traffic from Storage Queue, produces downstream desired-state messages, and stores application-data messages in the `SensorData` table. |
| **Actual state row** | One append-only Azure Table row that records a received node-scoped `GW-0812` observation for a Sonde `node_id`. |
| **Desired state row** | One append-only Azure Table row that records a requested desired state for a Sonde `node_id`. Desired rows are authored by admin/control-plane surfaces, not by the Azure handler reconciliation path. |
| ~~**Program route row**~~ | _Retired._ Previously mapped `program_hash` to a handler queue for `GW-0813` delivery. Superseded by direct `SensorData` table storage (AZH-0500). |
| **Observed fields** | The subset of node state reported by `GW-0812` and copied into an actual state row, including current program state, observed schedule as reported by the gateway, firmware data, battery, and check-in time. |
| **Desired fields** | The cloud-authored fields stored in a desired state row and used to build a complete `GW-0811` `DESIRED_STATE` payload for the node. In v1 this document defines `assigned_program_hash` and `schedule_interval_s`. |
| **Reverse-tick key** | A row-key prefix derived from `u64::MAX - timestamp_ms`, so newer timestamps sort before older timestamps for `Top(1)` queries within one node partition. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`AZH-XXXX`).
- **Title** — Short name.
- **Description** — What the Azure handler must do.
- **Acceptance criteria** — Observable, testable conditions that confirm the requirement is met.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Discovery decision, connector specification, or downstream dependency that motivates the requirement.

---

## 3  Storage Queue integration

### AZH-0100  Upstream connector queue consumption

**Priority:** Must
**Source:** GW-0812, GW-0813, Azure handler discovery review

**Description:**
The Azure handler MUST consume raw Sonde connector payloads from the configured
upstream Azure Storage Queue. It MUST decode the connector `msg_type`
enough to distinguish node-scoped `GW-0812` actual-state messages from
`GW-0813` application-data messages and route them to the appropriate handler
logic.

**Acceptance criteria:**

1. The Azure handler accepts raw connector payload bytes from the configured upstream queue.
2. A node-scoped `GW-0812` message is routed to node-state reconciliation logic.
3. A `GW-0813` message is routed to `SensorData` table storage logic.
4. Unsupported or out-of-scope connector messages do not mutate actual-state, desired-state, or sensor-data tables.

---

### AZH-0101  Downstream desired-state publication

**Priority:** Must
**Source:** GW-0811, Azure handler discovery review

**Description:**
When node-state reconciliation detects divergence, the Azure handler MUST emit a
complete node-scoped `GW-0811` `DESIRED_STATE` message to the configured
downstream queue. The emitted desired state replaces the gateway's previous
desired-state view for that node.

**Acceptance criteria:**

1. Each emitted message is encoded as one complete node-scoped `GW-0811` desired-state payload.
2. The payload targets exactly one node by `node_id`.
3. The payload includes the desired fields owned by the latest desired-state row.
4. The Azure handler does not emit imperative gateway commands outside the `GW-0811` desired-state contract.

---

## 4  Node-state table ownership and reconciliation

### AZH-0200  Append-only actual-state and desired-state history tables

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
The Azure handler integration MUST use separate append-only Azure Tables for
node actual-state history and node desired-state history. `ActualNodeState`
records every received node-scoped `GW-0812`. `DesiredNodeState` records each
requested node desired state. In v1, the desired fields owned by this document
are `assigned_program_hash` and `schedule_interval_s`.
`ephemeral_program_hash` remains out of scope for this document.

**Acceptance criteria:**

1. `ActualNodeState` stores append-only actual-state rows for each `node_id`.
2. `DesiredNodeState` stores append-only desired-state rows for each `node_id`.
3. Actual-state rows contain observed `current_program_hash`, observed gateway-assigned program hash, observed `schedule_interval_s`, `battery_mv`, `wake_rssi_dbm`, firmware ABI/version fields, and `timestamp_ms`.
4. Desired-state rows contain desired `assigned_program_hash` and desired `schedule_interval_s`.
5. The schema distinguishes actual-state history from desired-state history rather than collapsing them into one mutable row shape.

---

### AZH-0201  First `GW-0812` appends actual-state history without seeding desired state

**Priority:** Must
**Source:** Azure handler discovery review, GW-0812

**Description:**
If the handler receives a node-scoped `GW-0812` for a `node_id` that has no
prior actual-state history, it MUST append one `ActualNodeState` row using the
message fields. It MUST NOT synthesize or append a `DesiredNodeState` row for
that first-seen node. It MUST NOT emit `GW-0811` solely because the node has no
desired-state history yet.

**Acceptance criteria:**

1. The first node-scoped `GW-0812` for an unseen `node_id` creates exactly one new actual-state row.
2. The new actual-state row copies the observed fields from the message.
3. The handler does not create or mutate any desired-state row on that path.
4. The first-seen path does not emit a downstream `GW-0811` message solely because desired state is absent.

---

### AZH-0202  Append-only actual-state recording on every `GW-0812`

**Priority:** Must
**Source:** Azure handler discovery review, GW-0812

**Description:**
For every node-scoped `GW-0812` message, the Azure handler MUST append a new
actual-state history row before evaluating divergence. The appended row MUST
capture the message's check-in time, battery, firmware data, observed current
program state, observed gateway-assigned program state, and observed schedule.
The handler MUST retain repeated deliveries and older deliveries as history
rather than overwriting or discarding them at write time.

**Acceptance criteria:**

1. Each node-scoped `GW-0812` appends exactly one new actual-state row.
2. The appended row's `timestamp_ms`, `battery_mv`, `wake_rssi_dbm`, firmware ABI/version fields, current program, assigned program, and schedule fields match the message.
3. Repeated delivery of the same logical check-in results in multiple history rows rather than in-place replacement.
4. Older deliveries may still be appended for diagnostics and audit history.

---

### AZH-0203  Divergence detection and `GW-0811` emission

**Priority:** Must
**Source:** Azure handler discovery review, GW-0811, GW-0812

**Description:**
After appending an actual-state row from `GW-0812`, the Azure handler MUST load
the latest desired-state row for that `node_id` and compare the latest eligible
actual-state row against it. In v1, divergence is present when desired
`assigned_program_hash` is non-null and differs from the observed current
program hash, or when desired `schedule_interval_s` is non-null and differs
from the observed schedule interval. A null desired field means the Azure
handler is not asserting a cloud target for that field and therefore MUST NOT
treat that field as divergent by itself. If no desired-state row exists for the
node, the handler MUST NOT emit `GW-0811`. When either active comparison
diverges, the Azure handler MUST emit one complete `GW-0811` `DESIRED_STATE`
message for that node using the latest desired-state row's fields.

**Acceptance criteria:**

1. A resident-program mismatch between the latest eligible actual-state row and the latest desired-state row causes one downstream `GW-0811` message for that evaluation.
2. A schedule mismatch between the latest eligible actual-state row and the latest desired-state row causes one downstream `GW-0811` message for that evaluation.
3. When both resident-program and schedule mismatch, the Azure handler still emits exactly one complete `GW-0811` message for that evaluation.
4. The emitted desired-state payload includes the latest desired-state row's `assigned_program_hash` and `schedule_interval_s`.
5. The emitted desired-state payload does not invent `ephemeral_program_hash` state owned by this document.
6. A null desired resident-program or null desired schedule field does not, by itself, trigger divergence or downstream publication for that field.
7. Absence of any desired-state row for a node suppresses downstream publication for that node.
8. If the latest desired-state row returned for a node partition carries a different `node_id` payload, the handler fails the reconciliation attempt rather than publishing a `GW-0811` for either node.

---

### AZH-0204  No downstream publication when desired and observed state align

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
If the latest desired-state row for a node matches the latest eligible
actual-state row derived from `GW-0812`, the Azure handler MUST retain the
appended actual-state history row and MUST NOT emit a downstream `GW-0811`
message for that event.

**Acceptance criteria:**

1. An aligned `GW-0812` appends an actual-state row.
2. An aligned `GW-0812` does not enqueue a downstream desired-state message.

---

### AZH-0205  Reverse-tick latest-row lookup

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
The actual-state and desired-state history tables MUST optimize the common query
pattern "latest row for one node". Each table MUST use a node-scoped partition
key and a reverse-tick row-key prefix derived from `u64::MAX - timestamp_ms`,
so that the newest row for a node sorts first for `Top(1)` retrieval.

**Acceptance criteria:**

1. Actual-state and desired-state rows for one node can be queried by a node-scoped partition key.
2. Within one node partition, newer timestamps sort before older timestamps by row key.
3. A `Top(1)` query scoped to one node returns that node's newest row without a full partition scan.
4. The row key format permits multiple rows with the same `timestamp_ms` without overwriting history.
5. For rows that share the same `timestamp_ms`, the row key still provides deterministic uniqueness, and within one handler process lifetime later appends sort before earlier appends. Reconciliation correctness MUST NOT depend on a globally newest-first order across restarts or concurrent handler instances.

---

### AZH-0206  Desired-state history is admin-authored

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
The Azure handler reconciliation path MUST treat `DesiredNodeState` as an
admin-authored control-plane history surface. It MUST read the latest desired
row for a node when evaluating divergence, but it MUST NOT create, replace, or
append desired-state rows while processing `GW-0812`.

**Acceptance criteria:**

1. Node reconciliation reads desired-state history but does not write it.
2. First-seen nodes do not cause synthetic desired-state rows to be created.
3. Desired-state table mutations are owned by external admin or control-plane surfaces.

---

### AZH-0207  Out-of-order actual-state retention without stale control decisions

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
If a node-scoped `GW-0812` arrives with `timestamp_ms` older than the latest
actual-state row already stored for that node, the Azure handler MUST still
append the older actual-state row for diagnostics. However, it MUST NOT let
that out-of-order row displace the latest eligible actual state for divergence
evaluation or downstream `GW-0811` publication.

**Acceptance criteria:**

1. An older `GW-0812` still produces an appended actual-state history row.
2. The latest actual-state row used for control decisions remains the newest row by timestamp for that node.
3. An out-of-order row does not trigger downstream publication solely because it differs from the latest desired state.

---

## 5  ~~Program-hash routing for `GW-0813`~~ (Retired)

> **Status:** Retired. Program-route-based queue delivery of `GW-0813` messages
> has been superseded by direct `SensorData` table storage (AZH-0500). The
> `ProgramRoute` table, handler queue delivery, and related fail-closed
> semantics are no longer part of the handler. Requirements AZH-0300 through
> AZH-0303 are retired.

---

## 6  Failure handling and observability

### AZH-0400  Storage and broker failures surface and fail closed

**Priority:** Must
**Source:** Azure handler discovery review, GW-0815

**Description:**
If the Azure handler cannot append to the actual-state table, cannot read the
desired-state table, cannot publish `GW-0811`, or cannot append to the
`SensorData` table, it MUST surface the failure and fail closed for the affected
message.

**Acceptance criteria:**

1. Actual-state table append failures are surfaced through logging, function failure, or both.
2. Desired-state table read failures are surfaced through logging, function failure, or both.
3. Downstream `GW-0811` publish failures are surfaced through logging, function failure, or both.
4. `SensorData` table append failures are surfaced through logging, function failure, or both.
5. The Azure handler does not silently claim success after a detected storage or broker failure.

---

## 7  Sensor data storage

### AZH-0500  SensorData table storage

**Priority:** Must
**Source:** User request (GW-1903 enrichment)

**Description:**
The Azure handler MUST store enriched APP_DATA messages in an Azure Storage
Table named `SensorData`. Each `GW-0813` app-data message results in one row.
The table uses the same safety patterns as existing tables: hashed partition
keys and uniqueness-suffixed row keys.

**Table schema:**

| Column | Type | Description |
|--------|------|-------------|
| `PartitionKey` | `String` | `"n:" + lowercase-hex-encoded SHA-256(node_id UTF-8 bytes)` |
| `RowKey` | `String` | Reverse-tick key + `":"` + uniqueness suffix |
| `node_id` | `String` | Originating node identifier (display) |
| `timestamp_ms` | `Edm.Int64` | Message timestamp in milliseconds |
| `program_hash` | `String` | BPF program hash (hex) |
| `raw_payload` | `String` | Base64-encoded raw APP_DATA blob |
| `decoded_readings` | `String` | JSON string of `readings` map, or `""` |

**Acceptance criteria:**

1. Every `GW-0813` app-data message results in a row in the `SensorData` table.
2. Rows are queryable by node ID (partition key) and time range (row key).
3. Multiple messages within the same millisecond produce distinct rows (uniqueness suffix).
4. The `SensorData` table is pre-provisioned (added to Bicep/provisioning).

---

### AZH-0501  SensorData decoded readings column

**Priority:** Must
**Source:** User request (GW-1903 enrichment)

**Description:**
The `decoded_readings` column in the `SensorData` table MUST store the
`readings` map from enriched CBOR as a JSON string. The JSON format is
`{ "reading_name": value, ... }` where values are JSON numbers for int64
values within JavaScript's `Number.MAX_SAFE_INTEGER` (2^53 - 1), or JSON
strings for values exceeding that threshold (see AC-5). If no `readings`
key is present in the upstream message (no decoder configured or decoder
failure), `decoded_readings` is an empty string.

**Acceptance criteria:**

1. A reading emitted as `emit_reading("temperature_mc", 25125)` produces
   `decoded_readings` containing `{"temperature_mc":25125}`.
2. Multiple readings produce a single JSON object with all key-value pairs.
3. The column is an `Edm.String` Azure Table property.
4. Empty readings (no decoder) result in `""` (empty string), not `null`.
5. Int64 values within JavaScript's `Number.MAX_SAFE_INTEGER` (2^53 - 1) are encoded as JSON numbers. Values exceeding this threshold are encoded as JSON strings (e.g., `"9007199254740993"`) to preserve precision for SPA consumers that use `JSON.parse`.

---

### AZH-0502  SensorData query support

**Priority:** Must
**Source:** User request (WEB-0700 visualization)
**Scope:** SPA + provisioning (not the Azure handler function)

**Description:**
The `SensorData` table MUST be queryable by the SPA via Azure Table Storage
REST API using the logged-in user's bearer token. The table MUST support
queries by:
- Node ID (partition key filter)
- Time range (row key range, using reverse-timestamp convention)
- Program hash (property filter)

No additional API endpoint in the Azure handler is required — the SPA queries
Azure Table Storage directly. The handler's responsibility is limited to
writing rows (AZH-0500). The provisioning stack must grant the SPA's Entra
identity read access to the `SensorData` table (see
[azure-provisioning-requirements.md](azure-provisioning-requirements.md)).

**Acceptance criteria:**

1. The SPA can query `SensorData` rows for a specific node within a time range via the Azure Table Storage REST API.
2. The SPA can query `SensorData` rows for a specific program hash within a single node's partition. Cross-node program-hash queries are performed as parallel per-node requests by the SPA.
3. Query performance is acceptable for time-series visualization (< 2 seconds for 1000 rows).
4. The provisioning stack grants the SPA read access to the `SensorData` table.

---

## 8  PSK escrow state storage and recovery

### AZH-0600  Gateway ACTUAL_STATE storage

**Priority:** Must
**Source:** Issue #962, [evolve-962-specification.md](evolve-962-specification.md) §4.1–§4.4

**Description:**
The handler MUST store gateway `ACTUAL_STATE` in the `actualstate` Azure Table
with `PartitionKey = "g:" + entity_id` (hex-encoded `gateway_id`) and a
reverse-timestamp `RowKey` generated by `next_history_row_key(timestamp_ms)`.
The handler MUST append a new row on each gateway state update (not overwrite a
singleton), unless the incoming message's `timestamp_ms` is older than the
previously stored latest row, in which case the handler MUST silently discard
the stale message without appending. The handler MUST store all gateway-specific
escrow and recovery fields:
`x25519_public_key`, `channel`, `master_key_id`, `master_key_epoch`, `salt`,
`kdf_params_json`, `gateway_version`, `gateway_commit`,
`modem_firmware_version`, `modem_firmware_commit`,
`missing_key_hints`, `fingerprint_words`, and `rotation_in_progress`.

The `load_gateway_actual_state` operation MUST return only the latest row per
gateway (the row with the lexicographically smallest `RowKey` within the
gateway's partition), consistent with the existing node actual-state history
pattern.

**Acceptance criteria:**

1. Gateway `ACTUAL_STATE` appends a new history row per state update (not upsert).
2. The gateway row uses the `PartitionKey` prefix `"g:"`, distinct from node rows.
3. `load_gateway_actual_state` returns only the latest row for a given gateway.
4. Multiple state updates for the same gateway produce distinct rows preserving
   full history.

---

### AZH-0601  Node PSK escrow storage

**Priority:** Must
**Source:** Issue #962, [evolve-962-specification.md](evolve-962-specification.md) §4.1–§4.2

**Description:**
The handler MUST store node escrow fields `encrypted_psk`, `master_key_id`, and
`key_hint` alongside other node `ACTUAL_STATE` in the existing node row. Phone
`ACTUAL_STATE` is NOT stored; phones are not escrowed.

**Acceptance criteria:**

1. Node rows include `encrypted_psk`, `master_key_id`, and `key_hint` columns.
2. No phone rows are created in the `actualstate` table.

---

### AZH-0602  Missing key_hint recovery

**Priority:** Must
**Source:** Issue #962, [evolve-962-specification.md](evolve-962-specification.md) §2.8, §4.2

**Description:**
When a gateway `ACTUAL_STATE` contains non-empty `missing_key_hints`, the
handler MUST look up matching node rows where `key_hint` matches and
`master_key_id` matches the gateway's reported `master_key_id`. Matching PSKs
MUST be included in the next gateway `DESIRED_STATE` as `recovered_psks`
(CBOR key 29 inside desired-state map key 4). The handler SHOULD latch or
enqueue this recovery work immediately because subsequent `ACTUAL_STATE`
messages may overwrite the reported hints.

**Acceptance criteria:**

1. Recovery PSKs are matched by both `key_hint` and `master_key_id`.
2. PSKs with mismatched `master_key_id` are not included in `recovered_psks`.

---

### AZH-0603  Rotation payload relay

**Priority:** Must
**Source:** Issue #962, [evolve-962-specification.md](evolve-962-specification.md) §4.4

**Description:**
The handler MUST relay rotation payloads from the SPA to the gateway via the
`DESIRED_STATE` field `rotation_payload` (CBOR key 28 inside desired-state map
key 4). The handler does not originate, inspect, or modify rotation payloads.
After the gateway reports a new `master_key_epoch` in `ACTUAL_STATE`, the
handler MUST clear `rotation_payload` by appending a new row to the
`desiredstate` table with `rotation_payload` set to `None` and a fresh
history `RowKey`. The handler MUST NOT overwrite or replace the original
SPA-written row.

**Acceptance criteria:**

1. `rotation_payload` is relayed unmodified.
2. `rotation_payload` is cleared after the gateway reports an incremented epoch.
3. Clearing appends a new row; the original SPA-written row is preserved.

---

### AZH-0604  Salt management

**Priority:** Must
**Source:** Issue #962, [evolve-962-specification.md](evolve-962-specification.md) §4.3

**Description:**
Salt arrives in gateway `ACTUAL_STATE` and is stored in the gateway row. The
gateway is authoritative for salt. The handler includes the stored salt in
gateway `DESIRED_STATE` only for gateways that report `salt = null`.

**Acceptance criteria:**

1. Gateway-reported salt is stored.
2. Salt is included in `DESIRED_STATE` only when the gateway has no local salt.

---

### AZH-0605  Gateway DESIRED_STATE construction

**Priority:** Must
**Source:** Issue #962, [evolve-962-specification.md](evolve-962-specification.md) §2.5, §4.2–§4.4

**Description:**
The handler MUST construct gateway `DESIRED_STATE` with
`entity_kind = "gateway"` and `entity_id = hex(gateway_id)`. Inside the
`desired_state` map it MUST use CBOR keys 15 for `channel`, 21 for `salt`, 22
for `kdf_params`, 28 for `rotation_payload`, and 29 for `recovered_psks`.
Gateway `DESIRED_STATE` is written to the `desiredstate` Azure Table with
`PartitionKey = "g:" + gateway_id_hex`. All handler writes to the
`desiredstate` table MUST be append-only (new rows with unique history
`RowKey`s), never upserts or replacements.

**Acceptance criteria:**

1. Gateway `DESIRED_STATE` uses the correct CBOR key numbers.
2. The `PartitionKey` uses the `"g:"` prefix.
3. Handler writes to the `desiredstate` table are append-only.

---

### AZH-0700  SDK workaround — missing `Server` response header

**Priority:** Must
**Source:** Issue #1089, azure-sdk-for-rust#4489

**Description:**
Some Azure Table Storage stamps omit the `Server` HTTP response header.
`azure_storage` 0.21.0 `CommonStorageResponseHeaders` unconditionally
requires this header, causing all table operations to fail with
`"header not found server"`. The handler MUST inject a synthetic `Server`
header into Azure Table Storage responses that lack one, using the SDK's
`Policy` pipeline extension mechanism.

**Acceptance criteria:**

1. When the `Server` header is absent, the policy injects
   `"Windows-Azure-Table/1.0 Microsoft-HTTPAPI/2.0"`.
2. When the `Server` header is already present, it is not modified.
3. The workaround is removable when the upstream SDK fix lands.

### AZH-0800  Append-only program image storage

**Priority:** Must
**Source:** Issue #1098

**Description:**
The `Programs` table MUST use insert-only (append) semantics, consistent
with the audit-trail guarantees of the other Azure Tables
(`ActualNodeState`, `DesiredNodeState`, `SensorData`). The handler MUST
NOT use insert-or-replace on the `Programs` table.

**Acceptance criteria:**

1. `store_program_image` uses `insert` (not `insert_or_replace`).
2. If a row with the same `program_hash` RowKey already exists
   (Azure Table entity-already-exists conflict), the operation succeeds
   as a no-op — the original row is preserved unchanged.
3. The `HandlerStore` trait documents insert-only, first-writer-wins
   semantics for `store_program_image`.
4. Test mocks enforce insert-only: existing entries are not overwritten.
5. Legacy rows missing `elf_image` require manual deletion before
   re-ingest can populate the column; the re-ingest-to-repair path
   is removed from the design.
