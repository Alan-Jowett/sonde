<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Handler Design Specification

> **Document status:** Draft
> **Scope:** Internal design for the Azure cloud-side handler hosted in the
> Azure Function App. Covers upstream connector message intake, Azure Table
> schemas, node-state reconciliation, downstream `GW-0811` publication, and
> `GW-0813` queue routing.
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
   row and publish a complete node-scoped `GW-0811` when they diverge, and
4. route `GW-0813` application-data messages to handler queues by `program_hash`.

The handler does not replace the gateway's reconciler model. It expresses cloud
intent only by publishing `GW-0811` desired-state messages.

---

## 2  Runtime topology

> **Requirements:** AZH-0100, AZH-0101, AZH-0303, AZH-0400

The Azure handler runs inside the Azure Function App provisioned by the Bicep
stack. The Function App uses a system-assigned managed identity with:

1. receive permission on the upstream queue,
2. send permission on the downstream queue,
3. append permission on `ActualNodeState`, read permission on
   `DesiredNodeState`, and read permission on `ProgramRoute`, and
4. send permission on each pre-provisioned handler queue referenced by the
   program route table.

The final permission is an external dependency when a mapped handler queue is
not provisioned by the Sonde Bicep stack.

### 2.1  Trigger model

The Function App uses a Storage Queue-triggered entrypoint for upstream connector
messages. Each invocation receives one raw connector payload from the upstream
queue and performs the following dispatch:

1. decode the top-level connector `msg_type`,
2. if `msg_type = ACTUAL_STATE` and `entity_kind = "node"`, invoke node-state
   reconciliation,
3. if `msg_type = APP_DATA`, invoke program-route delivery, and
4. otherwise log the unsupported or out-of-scope message and complete without
   mutating handler-owned tables.

The handler does not require a second inbound trigger for downstream traffic
because it publishes `GW-0811` by calling the downstream queue sender directly.

---

## 3  Connector message interpretation

> **Requirements:** AZH-0100, AZH-0201, AZH-0202, AZH-0203, AZH-0301

### 3.1  `GW-0812` fields consumed by the handler

The handler decodes the node-scoped `ACTUAL_STATE` connector payload using the
schema defined in [gateway-companion-api.md](gateway-companion-api.md). For
node reconciliation it consumes:

1. `entity_id` as `node_id`,
2. `current_program_hash`,
3. `assigned_program_hash`,
4. `battery_mv`,
5. `firmware_abi_version`,
6. `firmware_version`,
7. `timestamp_ms` as last check-in time, and
8. `schedule_interval_s`.

Gateway-scoped `ACTUAL_STATE` messages are outside the node-table ownership of
this document and are therefore logged and ignored by the reconciliation path.

### 3.2  `GW-0813` fields consumed by the handler

For `APP_DATA`, the handler decodes:

1. `program_hash` for route lookup,
2. `node_id`, `timestamp_ms`, raw `blob`, and optional `readings` (key 16)
   for `SensorData` table storage (§6.1), and
3. the raw connector payload bytes for transparent delivery to the handler
   queue.

Fields beyond `program_hash` were previously opaque to the handler; the
`SensorData` feature (AZH-0500) extends the handler's parsing scope.

---

## 4  Azure Table schemas

> **Requirements:** AZH-0200, AZH-0205, AZH-0206, AZH-0300

The design uses three Azure Tables:

1. **`ActualNodeState`** — append-only actual-state history keyed for latest-per-node queries.
2. **`DesiredNodeState`** — append-only desired-state history keyed for latest-per-node queries.
3. **`ProgramRoute`** — `program_hash` to handler queue mapping.

### 4.1  `ActualNodeState` schema

Each row uses:

- `PartitionKey = "n:" + lowercase hex-encoded SHA-256(node_id UTF-8 bytes)`
- `RowKey = <reverse_tick_ms as fixed-width lowercase hex> + ":" + <implementation-defined suffix that preserves append uniqueness and orders later appends first within the same timestamp for one handler process lifetime>`

The row contains the following logical columns:

| Column | Purpose |
|--------|---------|
| `node_id` | Original opaque node identifier used by gateway and handlers. |
| `observed_current_program_hash` | Node-reported current resident program hash, nullable. |
| `observed_assigned_program_hash` | Gateway-reported assigned resident program hash, nullable. |
| `observed_schedule_interval_s` | Gateway-reported node schedule interval, nullable. |
| `battery_mv` | Latest battery reading from `GW-0812`, nullable. |
| `firmware_abi_version` | Latest firmware ABI version, nullable. |
| `firmware_version` | Latest firmware version, nullable. |
| `timestamp_ms` | Check-in time carried by the source `GW-0812`. |

The node-scoped `PartitionKey` keeps each node's history in one queryable
partition. The reverse-tick `RowKey` prefix makes newer timestamps sort first.
The suffix preserves append-only behavior when multiple deliveries share the
same `timestamp_ms`, and it orders later appends before earlier appends only
within one handler process lifetime. Across restarts or concurrent handler
instances, equal-timestamp row ordering is intentionally unspecified, so the
reconciliation path must not depend on `Top(1)` returning the most recently
appended equal-timestamp row.

### 4.2  `DesiredNodeState` schema

Each row uses:

- `PartitionKey = "n:" + lowercase hex-encoded SHA-256(node_id UTF-8 bytes)`
- `RowKey = <reverse_tick_ms as fixed-width lowercase hex> + ":" + <implementation-defined suffix that preserves append uniqueness and orders later appends first within the same timestamp for one handler process lifetime>`

The row contains:

| Column | Purpose |
|--------|---------|
| `node_id` | Original opaque node identifier used by gateway and handlers. |
| `desired_assigned_program_hash` | Cloud-authored desired resident program hash, nullable. |
| `desired_schedule_interval_s` | Cloud-authored desired schedule interval, nullable. |
| `timestamp_ms` | Time associated with the desired-state request. |

The Azure handler reads this table but does not write it. Admin/control-plane
surfaces append desired-state rows when requested state changes.

### 4.3  `ProgramRoute` schema

Each row uses:

- `PartitionKey = "program"`
- `RowKey = <lowercase hex-encoded program_hash>`

The row contains:

| Column | Purpose |
|--------|---------|
| `handler_queue` | Storage Queue name for `GW-0813` delivery. |

The table stores only queue references. It does not own queue creation or queue
policy lifecycle.

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
      does not already have the program locally. Programs must be re-ingested
      through `ProgramIngest` to populate the `elf_image` column.
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

## 6  `GW-0813` routing algorithm

> **Requirements:** AZH-0300, AZH-0301, AZH-0302, AZH-0303

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
   would collide,
3. look up the `ProgramRoute` row for that hash,
4. if the row exists, send the original raw connector payload bytes unchanged to
   the queue named by `handler_queue`, and
5. if the row is missing, log the missing mapping and fail the invocation so the
   upstream message is not reported as successfully handled.

The design does not route unmapped application-data messages to a default queue.
It also does not attempt to create the mapped queue if it does not already
exist.

### 6.1  SensorData table storage (AZH-0500)

In addition to routing `GW-0813` to the handler queue, the Azure handler MUST
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
as JSON into `decoded_readings`. Otherwise `decoded_readings` is `""`.

The `PartitionKey` and `RowKey` follow the same patterns as `ActualNodeState`
(§4.1) — hashed partition key for safe table keys, reverse-tick plus uniqueness
suffix for chronological ordering and append uniqueness.

`SensorData` writes are independent of `ProgramRoute` routing — the table is
populated even if no handler queue is configured for the program hash.

---

## 7  Failure handling

> **Requirements:** AZH-0302, AZH-0400

The handler follows a fail-closed rule for all Azure Table and Storage Queue
operations that determine externally visible control-plane behavior:

1. Table read/write failure aborts the invocation.
2. Downstream `GW-0811` publish failure aborts the invocation.
3. Handler-queue publish failure aborts the invocation.
4. Missing `ProgramRoute` rows are treated as failures, not soft drops.

This failure model preserves at-least-once retry behavior from the Azure
Function runtime instead of silently pretending that state was reconciled or
application data was delivered.
