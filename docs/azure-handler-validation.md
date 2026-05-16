<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Handler Validation Plan

> **Document status:** Draft
> **Scope:** Validation for the Azure Function App that owns Sonde cloud-side
> node reconciliation and `GW-0813` sensor data storage.
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
1. Start the Azure handler against test doubles for Azure Tables and Storage Queue senders.
2. Deliver one node-scoped `GW-0812`, one `GW-0813`, and one unsupported connector payload through the upstream trigger path.
3. Assert: the `GW-0812` is routed to node-state reconciliation.
4. Assert: the `GW-0813` is routed to sensor data storage.
5. Assert: the unsupported message does not mutate the actual-state, desired-state, or sensor-data tables.

---

### T-AZH-0101  Divergence emits a complete downstream `GW-0811`

**Validates:** AZH-0101, AZH-0203

**Procedure:**
1. Seed a latest desired-state row whose desired resident program hash and desired schedule differ from the observed values that will be carried by the test `GW-0812`.
2. Deliver that node-scoped `GW-0812` through the upstream trigger path.
3. Assert: exactly one downstream message is published.
4. Assert: the published payload is a node-scoped `GW-0811` `DESIRED_STATE` message.
5. Assert: the payload contains the row's desired `assigned_program_hash` and desired `schedule_interval_s`.
6. Assert: the payload does not invent a non-null `ephemeral_program_hash`.

---

### T-AZH-0200  Actual-state and desired-state tables are separate append-only histories

**Validates:** AZH-0200

**Procedure:**
1. Trigger the reconciliation path for one node-scoped `GW-0812`.
2. Inspect the actual-state table and desired-state table through test doubles.
3. Assert: the actual-state table receives an appended row for the node.
4. Assert: the desired-state table is not mutated by the handler.
5. Assert: actual-state rows store observed current program, observed assigned program, observed schedule, battery, firmware data, and `timestamp_ms`.
6. Assert: desired-state rows, when pre-seeded by the test, store desired resident program and desired schedule separately from actual-state rows.

---

### T-AZH-0201  First `GW-0812` appends actual-state history without publishing `GW-0811`

**Validates:** AZH-0201

**Procedure:**
1. Start with no actual-state rows and no desired-state rows for a test node.
2. Deliver one node-scoped `GW-0812` containing current program, assigned program, schedule, battery, firmware data, and timestamp.
3. Assert: exactly one new actual-state row is created for that node.
4. Assert: no desired-state row is created for that node.
5. Assert: no downstream `GW-0811` message is published.

---

### T-AZH-0202  Every `GW-0812` appends a new actual-state row

**Validates:** AZH-0202

**Procedure:**
1. Deliver two node-scoped `GW-0812` messages for the same node.
2. Assert: two distinct actual-state rows exist for that node afterward.
3. Assert: each row retains the battery, firmware, schedule, and timestamp values from its source message.
4. Deliver the same logical `GW-0812` again.
5. Assert: a third actual-state row is appended rather than merged into an existing row.

---

### T-AZH-0203  Program mismatch causes one downstream `GW-0811`

**Validates:** AZH-0203

**Procedure:**
1. Seed a latest desired-state row whose desired resident program hash differs from the current program hash that will be reported by the next `GW-0812`.
2. Deliver the node-scoped `GW-0812`.
3. Assert: exactly one downstream `GW-0811` message is published.
4. Assert: the emitted message targets the correct `node_id`.

---

### T-AZH-0204  Aligned rows do not publish downstream desired state

**Validates:** AZH-0204

**Procedure:**
1. Seed a latest desired-state row whose desired resident program and desired schedule match the observed values that will be carried by the next `GW-0812`.
2. Deliver that node-scoped `GW-0812`.
3. Assert: a new actual-state row is appended with the latest observed battery, firmware, and timestamp values.
4. Assert: no downstream `GW-0811` message is published.

---

### T-AZH-0205  Schedule mismatch causes one downstream `GW-0811`

**Validates:** AZH-0203

**Procedure:**
1. Seed a latest desired-state row whose desired schedule differs from the observed schedule that will be carried by the next `GW-0812`, while the desired and observed resident program hashes match.
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
1. Seed a latest desired-state row whose desired resident program hash and desired schedule are both `null`, while the observed values that will be carried by the next `GW-0812` are non-null.
2. Deliver that node-scoped `GW-0812`.
3. Assert: the handler appends a new actual-state row.
4. Assert: no downstream `GW-0811` message is published solely because the desired field is `null`.

---

### T-AZH-0207  Missing desired-state history suppresses divergence publication

**Validates:** AZH-0201, AZH-0203, AZH-0206

**Procedure:**
1. Start with no desired-state row for a test node.
2. Deliver a node-scoped `GW-0812` with non-null observed program and schedule values.
3. Assert: an actual-state row is appended.
4. Assert: no downstream `GW-0811` message is published.

---

### T-AZH-0208  Reverse-tick row keys return the newest row with `Top(1)`

**Validates:** AZH-0205

**Procedure:**
1. Seed multiple actual-state rows for one node with different `timestamp_ms` values.
2. Query the node partition with `Top(1)`.
3. Assert: the returned row is the one with the greatest `timestamp_ms`.
4. Repeat for desired-state rows.
5. Assert: repeated timestamps can coexist without overwriting one another.
6. Assert: when two rows share the same `timestamp_ms` within one handler process lifetime, the later-appended row sorts ahead of the earlier one for `Top(1)`.

---

### T-AZH-0209  Out-of-order `GW-0812` is retained but does not drive publication

**Validates:** AZH-0202, AZH-0203, AZH-0207

**Procedure:**
1. Seed a newer actual-state row and a latest desired-state row for a test node.
2. Deliver an older node-scoped `GW-0812` whose timestamp is less than the latest actual-state row.
3. Assert: the older delivery is appended as a new actual-state row.
4. Assert: the newest actual-state row used for control decisions remains the pre-existing newer row.
5. Assert: the older delivery does not trigger downstream publication by itself.

---

### T-AZH-0210  Equal-timestamp deliveries still evaluate the current append

**Validates:** AZH-0203, AZH-0205, AZH-0207

**Procedure:**
1. Seed a latest desired-state row for a test node.
2. Arrange the latest-row lookup to return a different actual-state history row with the same `timestamp_ms` as the delivery being tested.
3. Deliver a node-scoped `GW-0812` whose observed state diverges from the desired state.
4. Assert: the handler still evaluates the current appended delivery rather than suppressing it as stale solely because another equal-timestamp row sorts first.
5. Assert: downstream `GW-0811` publication still occurs when the current delivery diverges.

---

### T-AZH-0211  Handler never writes desired-state history during reconciliation

**Validates:** AZH-0206

**Procedure:**
1. Start the Azure handler against test doubles that record writes separately for actual-state and desired-state tables.
2. Deliver one or more node-scoped `GW-0812` messages.
3. Assert: actual-state writes occur as expected.
4. Assert: no desired-state write is attempted by the reconciliation path.

---

### T-AZH-0212  Mismatched desired-state `node_id` fails closed

**Validates:** AZH-0203

**Procedure:**
1. Arrange a desired-state lookup for node `A` that returns a row from node `A`'s partition but with a stored `node_id` payload of node `B`.
2. Deliver a node-scoped `GW-0812` for node `A` that would otherwise diverge from the desired row.
3. Assert: the handler surfaces an error instead of publishing a downstream `GW-0811`.
4. Assert: no downstream desired-state message is sent for either node.

---

### ~~T-AZH-0300 through T-AZH-0303~~ (Retired)

> Retired. These test cases validated ProgramRoute-based queue delivery of
> `GW-0813` messages, which has been superseded by direct `SensorData` table
> storage (AZH-0500).

---

### T-AZH-0400  Storage and broker failures are surfaced

**Validates:** AZH-0400

**Procedure:**
1. Inject one actual-state table append failure, one desired-state table read failure, one `SensorData` table append failure, and one downstream `GW-0811` send failure in separate sub-cases.
2. Trigger the corresponding handler path in each sub-case.
3. Assert: each failure is surfaced through logging, function failure, or both.
4. Assert: the handler does not report success for the failed message path.

For downstream `GW-0811` publication failures, repeat delivery of the same
`GW-0812` with the same `timestamp_ms` and assert that the handler retries the
divergence publication instead of treating that redelivery as permanently stale.

---

### T-AZH-0500  SensorData table row creation

**Validates:** AZH-0500

**Procedure:**
1. Deliver a `GW-0813` app-data message through the handler with a known
   `node_id` and `program_hash`.
2. Assert: a row is created in the `SensorData` table with correct
   `PartitionKey` (`"n:" + lowercase-hex-encoded SHA-256(node_id UTF-8 bytes)`),
   `node_id`, `program_hash`, and `raw_payload` (base64 of original blob).
3. Assert: `RowKey` is a reverse-tick key with uniqueness suffix.

---

### T-AZH-0500a  Duplicate-timestamp writes use unique RowKey suffixes

**Validates:** AZH-0500

**Procedure:**
1. Deliver two `GW-0813` messages for the same node with identical
   `timestamp_ms`.
2. Assert: both messages produce distinct rows (different `RowKey` suffixes).
3. Assert: no overwrite or conflict.

---

### T-AZH-0500b  Same upstream message retried produces single row

**Validates:** AZH-0500

**Procedure:**
1. Deliver the same `GW-0813` message twice (same upstream message ID /
   envelope sequence number).
2. Assert: only one `SensorData` row exists (the retry overwrites, not
   appends).
3. Assert: row content matches the message payload.

---

### T-AZH-0501  Decoded readings stored as JSON

**Validates:** AZH-0501

**Procedure:**
1. Deliver a `GW-0813` message whose CBOR contains a `readings` map
   (key 16) with `{ "temperature_mc": 25125, "humidity_pct": 4500 }`.
2. Assert: parsing the `decoded_readings` column as JSON yields an object
   containing `temperature_mc` = `25125` and `humidity_pct` = `4500`
   (comparison is on parsed values, not exact string representation).

---

### T-AZH-0501a  Missing readings stored as empty string

**Validates:** AZH-0501

**Procedure:**
1. Deliver a `GW-0813` message without a `readings` key.
2. Assert: `decoded_readings` is `""` (empty string).

---

### T-AZH-0501b  Large int64 values encoded as JSON strings

**Validates:** AZH-0501 (AC-5)

**Procedure:**
1. Deliver a `GW-0813` message whose CBOR `readings` map contains
   `{ "big_value": 9007199254740993 }` (one above `Number.MAX_SAFE_INTEGER`).
2. Assert: parsing the `decoded_readings` column as JSON yields
   `big_value` encoded as a JSON string `"9007199254740993"`, not a number.
3. Deliver a second message with `{ "small_value": 42 }`.
4. Assert: `small_value` is encoded as a JSON number `42`.

---

### T-AZH-0502  SensorData queryable by node and time range

**Validates:** AZH-0502

**Procedure:**
1. Insert 10 `SensorData` rows for the same node at 1-second intervals.
2. Query with `PartitionKey` filter and `RowKey` range covering the middle
   5 rows.
3. Assert: exactly 5 rows returned in reverse-chronological order.
