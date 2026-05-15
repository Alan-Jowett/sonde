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
> and routes `GW-0813` application-data messages to handler queues. It does not
> cover the gateway-local Azure companion bridge or Azure resource provisioning
> beyond the handler's runtime dependencies.
> **Related:** [gateway-companion-api.md](gateway-companion-api.md),
> [gateway-requirements.md](gateway-requirements.md),
> [azure-handler-design.md](azure-handler-design.md),
> [azure-handler-validation.md](azure-handler-validation.md),
> [azure-provisioning-requirements.md](azure-provisioning-requirements.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Azure handler** | The Azure-hosted control-plane process that consumes upstream Sonde connector traffic from Storage Queue and produces downstream desired-state messages or handler-queue deliveries. |
| **Actual state row** | One append-only Azure Table row that records a received node-scoped `GW-0812` observation for a Sonde `node_id`. |
| **Desired state row** | One append-only Azure Table row that records a requested desired state for a Sonde `node_id`. Desired rows are authored by admin/control-plane surfaces, not by the Azure handler reconciliation path. |
| **Program route row** | One Azure Table row keyed by `program_hash` that names the Storage Queue that should receive `GW-0813` application-data messages for that program. |
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
3. A `GW-0813` message is routed to application-data delivery logic.
4. Unsupported or out-of-scope connector messages do not mutate actual-state, desired-state, or program-route tables.

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
3. Actual-state rows contain observed `current_program_hash`, observed gateway-assigned program hash, observed `schedule_interval_s`, `battery_mv`, firmware ABI/version fields, and `timestamp_ms`.
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
2. The appended row's `timestamp_ms`, `battery_mv`, firmware ABI/version fields, current program, assigned program, and schedule fields match the message.
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

## 5  Program-hash routing for `GW-0813`

### AZH-0300  Program route mapping table

**Priority:** Must
**Source:** Azure handler discovery review, GW-0813

**Description:**
The Azure handler MUST own an Azure Table that stores one program route row per
`program_hash`. Each row maps the Sonde program hash to the Azure Storage Queue
queue that should receive `GW-0813` messages for that program.

**Acceptance criteria:**

1. The table stores one route row per `program_hash`.
2. Each route row identifies the handler queue name for that program.
3. The Azure handler can look up a route row using the `program_hash` carried by `GW-0813`.

---

### AZH-0301  Queue delivery of `GW-0813` messages

**Priority:** Must
**Source:** Azure handler discovery review, GW-0813

**Description:**
When the Azure handler receives a `GW-0813` message whose `program_hash` has a
program route row, it MUST deliver that message to the mapped Azure Storage Queue
queue. The delivered message body MUST preserve the raw `GW-0813` connector
payload bytes unchanged.

**Acceptance criteria:**

1. A mapped `GW-0813` is forwarded to the queue named by the corresponding route row.
2. The queue message body contains the raw `GW-0813` connector payload bytes unchanged.
3. The Azure handler uses the `program_hash` from the message rather than a node-local default route.

---

### AZH-0302  Missing program route rows fail closed

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
If a `GW-0813` message arrives for a `program_hash` that has no program route
row, the Azure handler MUST log the condition and fail closed. It MUST NOT drop
the message silently and MUST NOT reroute the message to a default queue.

**Acceptance criteria:**

1. Missing route rows are surfaced through logging, function failure, or both.
2. The Azure handler does not route an unmapped `GW-0813` message to a shared default queue.
3. The Azure handler does not report success for an unmapped `GW-0813` message.

---

### AZH-0303  Pre-provisioned handler queue boundary

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
The Azure handler MUST treat mapped handler queues as pre-provisioned external
dependencies. The program route table references those queues, but this
document's scope does not include queue creation or lifecycle management.

**Acceptance criteria:**

1. The program route table stores queue names rather than queue-creation directives.
2. The handler design and deployment docs identify mapped handler queues as pre-provisioned dependencies.
3. Queue provisioning failure is not hidden by silently creating a replacement queue.

---

## 6  Failure handling and observability

### AZH-0400  Storage and broker failures surface and fail closed

**Priority:** Must
**Source:** Azure handler discovery review, GW-0815

**Description:**
If the Azure handler cannot append to the actual-state table, cannot read the
desired-state table, cannot read the program route table, cannot publish
`GW-0811`, or cannot publish a mapped `GW-0813`, it MUST surface the failure
and fail closed for the affected message.

**Acceptance criteria:**

1. Actual-state table append failures are surfaced through logging, function failure, or both.
2. Desired-state table read failures are surfaced through logging, function failure, or both.
3. Program route table read failures are surfaced through logging, function failure, or both.
4. Downstream `GW-0811` publish failures are surfaced through logging, function failure, or both.
5. Mapped handler-queue publish failures are surfaced through logging, function failure, or both.
6. The Azure handler does not silently claim success after a detected storage or broker failure.

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
`{ "reading_name": value, ... }` where values are integers. If no `readings`
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

**Description:**
The `SensorData` table MUST be queryable by the SPA via Azure Table Storage
REST API using the logged-in user's bearer token. The table MUST support
queries by:
- Node ID (partition key filter)
- Time range (row key range, using reverse-timestamp convention)
- Program hash (property filter)

No additional API endpoint is required — the SPA queries Azure Tables directly.

**Acceptance criteria:**

1. The SPA can query `SensorData` rows for a specific node within a time range.
2. The SPA can query all `SensorData` rows across nodes for a specific program hash.
3. Query performance is acceptable for time-series visualization (< 2 seconds for 1000 rows).
