<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Handler Validation Plan

> **Document status:** Draft
> **Scope:** Validation for the Azure Function App that owns Sonde cloud-side
> node reconciliation and `GW-0813` routing.
> **Audience:** Implementers and reviewers validating the Azure handler against
> the connector contract.
> **Related:** [azure-handler-requirements.md](azure-handler-requirements.md),
> [azure-handler-design.md](azure-handler-design.md),
> [gateway-companion-api.md](gateway-companion-api.md),
> [azure-provisioning-validation.md](azure-provisioning-validation.md)

---

## 1  Test cases

### T-AZH-0100  Upstream queue messages are classified by connector `msg_type`

**Validates:** AZH-0100

**Procedure:**
1. Start the Azure handler against test doubles for Azure Tables and Service Bus senders.
2. Deliver one node-scoped `GW-0812`, one `GW-0813`, and one unsupported connector payload through the upstream trigger path.
3. Assert: the `GW-0812` is routed to node-state reconciliation.
4. Assert: the `GW-0813` is routed to application-data delivery.
5. Assert: the unsupported message does not mutate the node-state or program-route tables.

---

### T-AZH-0101  Divergence emits a complete downstream `GW-0811`

**Validates:** AZH-0101, AZH-0203

**Procedure:**
1. Seed an existing `NodeState` row whose desired resident program hash and desired schedule differ from the observed values that will be carried by the test `GW-0812`.
2. Deliver that node-scoped `GW-0812` through the upstream trigger path.
3. Assert: exactly one downstream message is published.
4. Assert: the published payload is a node-scoped `GW-0811` `DESIRED_STATE` message.
5. Assert: the payload contains the row's desired `assigned_program_hash` and desired `schedule_interval_s`.
6. Assert: the payload does not invent a non-null `ephemeral_program_hash`.

---

### T-AZH-0200  NodeState table stores separate desired and observed columns

**Validates:** AZH-0200

**Procedure:**
1. Create or update a `NodeState` row through the handler's reconciliation path.
2. Inspect the stored row.
3. Assert: desired resident program and desired schedule fields are stored separately from the observed fields.
4. Assert: the row also stores observed current program, observed assigned program, observed schedule, battery, firmware data, and last check-in time.

---

### T-AZH-0201  First `GW-0812` seeds a missing row without publishing `GW-0811`

**Validates:** AZH-0201

**Procedure:**
1. Start with no `NodeState` row for a test node.
2. Deliver one node-scoped `GW-0812` containing current program, assigned program, schedule, battery, firmware data, and timestamp.
3. Assert: exactly one new row is created for that node.
4. Assert: the row's desired resident program is initialized from current program when present, otherwise from assigned program.
5. Assert: the row's desired schedule is initialized from the observed schedule.
6. Assert: no downstream `GW-0811` message is published.

---

### T-AZH-0202  Existing rows refresh observed state before divergence evaluation

**Validates:** AZH-0202

**Procedure:**
1. Seed a `NodeState` row with stale observed values.
2. Deliver a later node-scoped `GW-0812` for the same node with different battery, firmware, schedule, and timestamp values.
3. Assert: the stored observed values are replaced by the new values from the message.
4. Assert: the divergence decision for that invocation uses the new observed values.

---

### T-AZH-0203  Program mismatch causes one downstream `GW-0811`

**Validates:** AZH-0203

**Procedure:**
1. Seed a `NodeState` row whose desired resident program hash differs from the current program hash that will be reported by the next `GW-0812`.
2. Deliver the node-scoped `GW-0812`.
3. Assert: exactly one downstream `GW-0811` message is published.
4. Assert: the emitted message targets the correct `node_id`.

---

### T-AZH-0204  Aligned rows do not publish downstream desired state

**Validates:** AZH-0204

**Procedure:**
1. Seed a `NodeState` row whose desired resident program and desired schedule match the observed values that will be carried by the next `GW-0812`.
2. Deliver that node-scoped `GW-0812`.
3. Assert: the row is updated with the latest observed battery, firmware, and timestamp values.
4. Assert: no downstream `GW-0811` message is published.

---

### T-AZH-0205  Schedule mismatch causes one downstream `GW-0811`

**Validates:** AZH-0203

**Procedure:**
1. Seed a `NodeState` row whose desired schedule differs from the observed schedule that will be carried by the next `GW-0812`, while the desired and observed resident program hashes match.
2. Deliver the node-scoped `GW-0812`.
3. Assert: exactly one downstream `GW-0811` message is published.
4. Assert: the emitted payload carries the row's desired `schedule_interval_s`.

Combined resident-program and schedule divergence is covered by
`T-AZH-0101`, which asserts that one complete downstream `GW-0811` is
published when both desired fields diverge simultaneously.

---

### T-AZH-0206  Null desired fields suppress divergence for that field

**Validates:** AZH-0203, AZH-0204

**Procedure:**
1. Seed a `NodeState` row whose desired resident program hash and desired schedule are both `null`, while the observed values that will be carried by the next `GW-0812` are non-null.
2. Deliver that node-scoped `GW-0812`.
3. Assert: the handler refreshes the stored observed fields.
4. Assert: no downstream `GW-0811` message is published solely because the desired field is `null`.

---

### T-AZH-0300  ProgramRoute rows map `program_hash` to handler queue name

**Validates:** AZH-0300

**Procedure:**
1. Seed a `ProgramRoute` row for a known `program_hash`.
2. Deliver a `GW-0813` carrying that hash.
3. Assert: the handler performs a route lookup by `program_hash`.
4. Assert: the lookup resolves to the expected queue name.

---

### T-AZH-0301  `GW-0813` payloads are forwarded unchanged to the mapped queue

**Validates:** AZH-0301

**Procedure:**
1. Seed a `ProgramRoute` row mapping a test `program_hash` to a test queue.
2. Deliver a representative `GW-0813` payload through the upstream trigger path.
3. Assert: exactly one queue message is sent to the mapped queue.
4. Assert: the queue message body matches the original raw `GW-0813` connector payload bytes byte-for-byte.

---

### T-AZH-0302  Missing program routes fail closed

**Validates:** AZH-0302

**Procedure:**
1. Ensure no `ProgramRoute` row exists for a test `program_hash`.
2. Deliver a `GW-0813` carrying that hash.
3. Assert: the handler reports failure through logging, function failure, or both.
4. Assert: no fallback or default queue delivery occurs.

---

### T-AZH-0303  Mapped queues are treated as external dependencies

**Validates:** AZH-0303

**Procedure:**
1. Configure a `ProgramRoute` row that references a queue name.
2. Inspect the route storage, handler behavior, and deployment documentation.
3. Assert: the route stores only the queue reference.
4. Assert: the handler does not attempt queue creation when processing the route.
5. Assert: the deployment documentation identifies mapped handler queues as pre-provisioned dependencies.

---

### T-AZH-0400  Storage and broker failures are surfaced

**Validates:** AZH-0400

**Procedure:**
1. Inject one table write failure, one downstream `GW-0811` send failure, and one mapped handler-queue send failure in separate sub-cases.
2. Trigger the corresponding handler path in each sub-case.
3. Assert: each failure is surfaced through logging, function failure, or both.
4. Assert: the handler does not report success for the failed message path.

For downstream `GW-0811` publication failures, repeat delivery of the same
`GW-0812` with the same `timestamp_ms` and assert that the handler retries the
divergence publication instead of treating that redelivery as permanently stale.
