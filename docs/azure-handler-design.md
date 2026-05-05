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
adapter between the gateway's local connector socket and Azure Service Bus. The
Azure handler owns the first Sonde-aware cloud logic:

1. consume upstream connector messages from the upstream queue,
2. reconcile node-scoped `GW-0812` actual-state messages into a combined
   desired/observed Azure Table row,
3. publish a complete node-scoped `GW-0811` desired-state message when desired
   and observed node state diverge, and
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
3. read/write permission on the Azure Tables used by the handler, and
4. send permission on each pre-provisioned handler queue referenced by the
   program route table.

The final permission is an external dependency when a mapped handler queue is
not provisioned by the Sonde Bicep stack.

### 2.1  Trigger model

The Function App uses a Service Bus-triggered entrypoint for upstream connector
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

1. `program_hash` for route lookup, and
2. the raw connector payload bytes for transparent delivery.

The handler does not reinterpret or normalize the opaque application payload
inside the `GW-0813` message body.

---

## 4  Azure Table schemas

> **Requirements:** AZH-0200, AZH-0300

The design uses two Azure Tables:

1. **`NodeState`** — combined desired/observed state keyed by `node_id`.
2. **`ProgramRoute`** — `program_hash` to handler queue mapping.

### 4.1  `NodeState` schema

Each row uses:

- `PartitionKey = "node"`
- `RowKey = <node_id>`

The row contains the following logical columns:

| Column | Purpose |
|--------|---------|
| `desired_assigned_program_hash` | Cloud-authored desired resident program hash, nullable. |
| `desired_schedule_interval_s` | Cloud-authored desired schedule interval, nullable. |
| `observed_current_program_hash` | Node-reported current resident program hash, nullable. |
| `observed_assigned_program_hash` | Gateway-reported assigned resident program hash, nullable. |
| `observed_schedule_interval_s` | Gateway-reported node schedule interval, nullable. |
| `battery_mv` | Latest battery reading from `GW-0812`, nullable. |
| `firmware_abi_version` | Latest firmware ABI version, nullable. |
| `firmware_version` | Latest firmware version, nullable. |
| `last_checkin_ms` | Most recent `timestamp_ms` accepted for the node. |

The row deliberately separates desired and observed columns so that Azure-side
control-plane intent is not overwritten by the next `GW-0812`.

### 4.2  `ProgramRoute` schema

Each row uses:

- `PartitionKey = "program"`
- `RowKey = <lowercase hex-encoded program_hash>`

The row contains:

| Column | Purpose |
|--------|---------|
| `handler_queue` | Azure Service Bus queue name for `GW-0813` delivery. |

The table stores only queue references. It does not own queue creation or queue
policy lifecycle.

---

## 5  Node-state reconciliation algorithm

> **Requirements:** AZH-0201, AZH-0202, AZH-0203, AZH-0204

For each node-scoped `GW-0812`, the handler performs the following sequence:

1. Load the `NodeState` row for `node_id`.
2. If the row is missing:
   1. create a new row,
   2. copy the observed fields from `GW-0812`,
   3. seed `desired_assigned_program_hash` from `current_program_hash` when
      present, otherwise from `assigned_program_hash`, and
   4. seed `desired_schedule_interval_s` from `schedule_interval_s`.
3. If the row exists:
   1. update all observed fields from `GW-0812`,
   2. compare `desired_assigned_program_hash` to
      `observed_current_program_hash`, and
   3. compare `desired_schedule_interval_s` to
      `observed_schedule_interval_s`.
4. If neither comparison diverges, complete the invocation with no downstream
   publication.
5. If either comparison diverges, build one complete `GW-0811`
   `DESIRED_STATE` payload using:
   1. `entity_kind = "node"`,
   2. `entity_id = node_id`,
   3. `assigned_program_hash = desired_assigned_program_hash`, and
   4. `schedule_interval_s = desired_schedule_interval_s`.
6. Publish that payload to the downstream queue.

`ephemeral_program_hash` is intentionally omitted in v1. The gateway connector
schema already defines it, but this design does not add Azure-side ownership or
comparison rules for it yet.

---

## 6  `GW-0813` routing algorithm

> **Requirements:** AZH-0300, AZH-0301, AZH-0302, AZH-0303

For each `GW-0813` invocation:

1. decode the top-level connector payload enough to extract `program_hash`,
2. look up the `ProgramRoute` row for that hash,
3. if the row exists, send the original raw connector payload bytes unchanged to
   the queue named by `handler_queue`, and
4. if the row is missing, log the missing mapping and fail the invocation so the
   upstream message is not reported as successfully handled.

The design does not route unmapped application-data messages to a default queue.
It also does not attempt to create the mapped queue if it does not already
exist.

---

## 7  Failure handling

> **Requirements:** AZH-0302, AZH-0400

The handler follows a fail-closed rule for all Azure Table and Service Bus
operations that determine externally visible control-plane behavior:

1. Table read/write failure aborts the invocation.
2. Downstream `GW-0811` publish failure aborts the invocation.
3. Handler-queue publish failure aborts the invocation.
4. Missing `ProgramRoute` rows are treated as failures, not soft drops.

This failure model preserves at-least-once retry behavior from the Azure
Function runtime instead of silently pretending that state was reconciled or
application data was delivered.
