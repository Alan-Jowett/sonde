<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Handler Requirements Specification

> **Document status:** Draft
> **Source:** Azure handler discovery review, `GW-0811`/`GW-0812`/`GW-0813`,
> and the implemented Azure companion bridge architecture.
> **Scope:** This document covers the Azure cloud-side handler hosted in the
> Azure Function App provisioned for the Sonde Azure integration. It owns the
> Azure Table state used for node reconciliation, consumes upstream connector
> traffic from Service Bus, emits downstream `GW-0811` desired-state messages,
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
| **Azure handler** | The Azure-hosted control-plane process that consumes upstream Sonde connector traffic from Service Bus and produces downstream desired-state messages or handler-queue deliveries. |
| **Node state row** | One Azure Table row keyed by Sonde `node_id` that contains both desired fields and the latest observed fields derived from `GW-0812`. |
| **Program route row** | One Azure Table row keyed by `program_hash` that names the Azure Service Bus queue that should receive `GW-0813` application-data messages for that program. |
| **Observed fields** | The subset of node state reported by `GW-0812` and copied into the node state row, including current program state, observed schedule as reported by the gateway, firmware data, battery, and last check-in time. |
| **Desired fields** | The cloud-authored fields stored in the node state row and used to build a complete `GW-0811` `DESIRED_STATE` payload for the node. In v1 this document defines `assigned_program_hash` and `schedule_interval_s`. |
| **Last check-in time** | The most recent `timestamp_ms` carried by a node-scoped `GW-0812` message accepted by the Azure handler for that node. |

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

## 3  Service Bus integration

### AZH-0100  Upstream connector queue consumption

**Priority:** Must
**Source:** GW-0812, GW-0813, Azure handler discovery review

**Description:**
The Azure handler MUST consume raw Sonde connector payloads from the configured
upstream Azure Service Bus queue. It MUST decode the connector `msg_type`
enough to distinguish node-scoped `GW-0812` actual-state messages from
`GW-0813` application-data messages and route them to the appropriate handler
logic.

**Acceptance criteria:**

1. The Azure handler accepts raw connector payload bytes from the configured upstream queue.
2. A node-scoped `GW-0812` message is routed to node-state reconciliation logic.
3. A `GW-0813` message is routed to application-data delivery logic.
4. Unsupported or out-of-scope connector messages do not mutate node-state or program-route tables.

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
3. The payload includes the desired fields owned by the node-state row.
4. The Azure handler does not emit imperative gateway commands outside the `GW-0811` desired-state contract.

---

## 4  Node-state table ownership and reconciliation

### AZH-0200  Combined desired and observed node-state rows

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
The Azure handler MUST own an Azure Table that stores one node state row per
`node_id`. Each row MUST contain both desired fields and the latest observed
fields derived from node-scoped `GW-0812` messages. In v1, the desired fields
owned by this document are `assigned_program_hash` and `schedule_interval_s`.
`ephemeral_program_hash` remains out of scope for this document.

**Acceptance criteria:**

1. The table stores one row per `node_id`.
2. Each row contains desired `assigned_program_hash` and desired `schedule_interval_s` fields.
3. Each row contains observed `current_program_hash`, observed gateway-assigned program hash, observed `schedule_interval_s`, `battery_mv`, firmware ABI/version fields, and last check-in time.
4. The row shape distinguishes desired fields from observed fields rather than collapsing them into one set of columns.

---

### AZH-0201  Seed missing rows from `GW-0812`

**Priority:** Must
**Source:** Azure handler discovery review, GW-0812

**Description:**
If the node-state table does not contain a row for the `node_id` named by a
node-scoped `GW-0812` message, the Azure handler MUST create one using values
from that message. The seed operation MUST initialize the desired resident
program from the node's observed current program when present, otherwise from
the gateway-assigned program hash. It MUST initialize the desired schedule from
the observed `schedule_interval_s`. The seed operation MUST NOT emit `GW-0811`
for that first-seen node.

**Acceptance criteria:**

1. The first node-scoped `GW-0812` for an unseen `node_id` creates exactly one new row.
2. The new row copies the observed fields from the message.
3. The new row initializes desired `assigned_program_hash` from observed `current_program_hash` when non-null, otherwise from observed gateway-assigned program hash.
4. The new row initializes desired `schedule_interval_s` from the observed `schedule_interval_s`.
5. The seed path does not emit a downstream `GW-0811` message.

---

### AZH-0202  Observed-state refresh on every `GW-0812`

**Priority:** Must
**Source:** Azure handler discovery review, GW-0812

**Description:**
For every node-scoped `GW-0812` message, the Azure handler MUST refresh the
row's observed fields from the message before evaluating divergence. The update
must include the latest check-in time, battery, firmware data, observed current
program state, observed gateway-assigned program state, and observed schedule.

**Acceptance criteria:**

1. Existing rows are updated in place when a later `GW-0812` for that node arrives.
2. The row's last check-in time matches the message timestamp after the update.
3. The row's observed `battery_mv`, firmware ABI/version fields, current program, assigned program, and schedule fields match the message after the update.
4. Divergence evaluation uses the refreshed observed values rather than stale values from an older row version.

---

### AZH-0203  Divergence detection and `GW-0811` emission

**Priority:** Must
**Source:** Azure handler discovery review, GW-0811, GW-0812

**Description:**
After refreshing an existing node-state row from `GW-0812`, the Azure handler
MUST compare the row's desired fields against the observed fields reported by
that message. In v1, divergence is present when desired `assigned_program_hash`
differs from the observed current program hash, or when desired
`schedule_interval_s` differs from the observed schedule interval. When either
comparison diverges, the Azure handler MUST emit one complete `GW-0811`
`DESIRED_STATE` message for that node using the row's desired fields.

**Acceptance criteria:**

1. A resident-program mismatch causes one downstream `GW-0811` message for that `GW-0812`.
2. A schedule mismatch causes one downstream `GW-0811` message for that `GW-0812`.
3. When both resident-program and schedule mismatch, the Azure handler still emits exactly one complete `GW-0811` message for that `GW-0812`.
4. The emitted desired-state payload includes the row's desired `assigned_program_hash` and desired `schedule_interval_s`.
5. The emitted desired-state payload does not invent `ephemeral_program_hash` state owned by this document.

---

### AZH-0204  No downstream publication when desired and observed state align

**Priority:** Must
**Source:** Azure handler discovery review

**Description:**
If the desired fields stored in a node-state row match the corresponding
observed fields reported by `GW-0812`, the Azure handler MUST update the row and
MUST NOT emit a downstream `GW-0811` message for that event.

**Acceptance criteria:**

1. An aligned `GW-0812` updates the node-state row.
2. An aligned `GW-0812` does not enqueue a downstream desired-state message.

---

## 5  Program-hash routing for `GW-0813`

### AZH-0300  Program route mapping table

**Priority:** Must
**Source:** Azure handler discovery review, GW-0813

**Description:**
The Azure handler MUST own an Azure Table that stores one program route row per
`program_hash`. Each row maps the Sonde program hash to the Azure Service Bus
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
program route row, it MUST deliver that message to the mapped Azure Service Bus
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
If the Azure handler cannot read or write the node-state table, cannot read the
program route table, cannot publish `GW-0811`, or cannot publish a mapped
`GW-0813`, it MUST surface the failure and fail closed for the affected message.

**Acceptance criteria:**

1. Table read/write failures are surfaced through logging, function failure, or both.
2. Downstream `GW-0811` publish failures are surfaced through logging, function failure, or both.
3. Mapped handler-queue publish failures are surfaced through logging, function failure, or both.
4. The Azure handler does not silently claim success after a detected storage or broker failure.
