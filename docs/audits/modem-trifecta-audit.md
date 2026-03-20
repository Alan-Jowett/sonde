<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Specification Trifecta Audit — Investigation Report

## 1. Executive Summary

This report documents a systematic traceability audit of the modem specification trifecta: requirements (`modem-requirements.md`), design (`modem-design.md`), and validation (`modem-validation.md`). The audit uncovered **13 findings** spanning all seven defect categories.

The dominant issue is a **complete absence of BLE pairing relay coverage in the design document**. Fourteen BLE-related requirements (MD-0400 through MD-0414, excluding superseded MD-0406) have no design specification whatsoever — no architecture, no module decomposition, no data flow, and no error handling. The design document's overview, architecture diagram, and module table all describe the modem as a USB-CDC ↔ ESP-NOW bridge, while the requirements and validation documents define a tri-directional bridge that also includes BLE GATT relay.

Beyond the BLE gap, the audit found two untested requirements, three test cases with broken requirement traceability, several acceptance criteria not fully exercised by tests, and terminology conflicts between the design overview and the BLE requirements.

**Key metrics:**
- Requirements → Design traceability: **51.6%** (16/31 fully traced)
- Requirements → Test coverage: **93.5%** (29/31 tested)
- Test → Requirement backward traceability: **93.3%** (42/45 trace to MD-XXXX IDs)

---

## 2. Problem Statement

The modem specification comprises three interdependent artifacts that must remain consistent as the system evolves. Requirements define *what* the modem must do, design defines *how* it does it, and validation defines *how we prove it works*. A gap or conflict between any pair undermines the integrity of the system: untested requirements may hide bugs, orphaned design decisions may implement undocumented behavior, and misaligned acceptance criteria may give false confidence.

This audit evaluates bidirectional traceability across all three documents and checks for semantic consistency, numbering integrity, and assumption drift.

---

## 3. Investigation Scope

| Artifact | File | Version |
|----------|------|---------|
| Requirements | `docs/modem-requirements.md` | Draft |
| Design | `docs/modem-design.md` | Draft |
| Validation | `docs/modem-validation.md` | Draft |

**In scope:** All requirement IDs (MD-XXXX), test case IDs (T-XXXX), design module mappings, acceptance criteria, and cross-references.

**Out of scope:** The modem protocol specification (`modem-protocol.md`), BLE pairing protocol (`ble-pairing-protocol.md`), source code implementation, and CI/CD pipeline validation.

**Methodology:** Full-text extraction of all IDs and cross-references, followed by forward traceability (requirements → design, requirements → tests), backward traceability (tests → requirements, design → requirements), and semantic consistency checks.

---

## 4. Findings

### F-001 — BLE requirement block entirely absent from design document

| Field | Value |
|-------|-------|
| **Severity** | Critical |
| **Category** | D1_UNTRACED_REQUIREMENT |
| **Location** | Requirements §7 (MD-0400–MD-0414); Design doc (entire document) |

**Description:**
Fourteen active BLE pairing relay requirements have zero representation in the design document. The design document contains no BLE module, no BLE architecture, no BLE data flow, no BLE error handling, and no BLE-related entries in the module responsibility table (§3.1).

**Evidence:**
The design §3.1 module table maps six modules to requirements. Every mapped requirement is in the MD-01xx, MD-02xx, or MD-03xx range. The following requirement IDs appear nowhere in the design document:

| Untraced Requirement | Title |
|----------------------|-------|
| MD-0400 | Gateway Pairing Service |
| MD-0401 | BLE ↔ USB-CDC message relay |
| MD-0402 | ATT MTU negotiation |
| MD-0403 | Indication fragmentation |
| MD-0404 | BLE LESC pairing |
| MD-0405 | BLE connection lifecycle |
| MD-0407 | BLE advertising |
| MD-0408 | BLE_INDICATE relay |
| MD-0409 | BLE_RECV forwarding |
| MD-0410 | BLE_CONNECTED notification |
| MD-0411 | BLE_DISCONNECTED notification |
| MD-0412 | BLE advertising default off |
| MD-0413 | BLE_ENABLE / BLE_DISABLE commands |
| MD-0414 | Numeric Comparison pin relay |

**Root Cause:**
The design document was authored when the modem scope was limited to ESP-NOW bridging. The BLE pairing relay requirements (§7) were added to the requirements document later, but the design document was never updated to match.

**Impact:**
Implementers have no design guidance for 45% of the active requirements. This forces them to derive architecture from the requirements and protocol documents directly, increasing the risk of inconsistent implementation across modules (e.g., BLE ↔ serial codec integration, BLE + ESP-NOW concurrency model, BLE error handling strategy).

**Remediation:**
Add new design sections covering: BLE GATT driver initialization, BLE ↔ serial codec message types (`BLE_RECV`, `BLE_INDICATE`, `BLE_CONNECTED`, `BLE_DISCONNECTED`, `BLE_ENABLE`, `BLE_DISABLE`, `BLE_PAIRING_CONFIRM`, `BLE_PAIRING_CONFIRM_REPLY`), indication fragmentation logic, LESC pairing state machine, BLE connection lifecycle management, BLE + ESP-NOW concurrency model, and BLE error handling. Update the §3.1 module table with BLE module entries mapping to MD-0400–MD-0414. Update the architecture diagram in §1 to include BLE.

**Confidence:** Certain — verified by exhaustive text search for all 14 requirement IDs and the terms "BLE", "GATT", "pairing", "advertis" across the design document.

---

### F-002 — Design overview contradicts BLE requirements

| Field | Value |
|-------|-------|
| **Severity** | High |
| **Category** | D6_CONSTRAINT_VIOLATION |
| **Location** | Design §1 (line 14), §1 (line 32); Requirements §7 (MD-0400–MD-0414) |

**Description:**
The design document's §1 overview makes two claims that conflict with the BLE requirements:

1. *"The modem firmware is a simple bidirectional bridge between USB-CDC and ESP-NOW."* (line 14) — The BLE requirements make the modem a **tri-directional** bridge: USB-CDC ↔ ESP-NOW and USB-CDC ↔ BLE GATT.

2. *"The firmware is intentionally minimal — no crypto, no CBOR parsing, no sessions, no OTA updates."* (line 32) — MD-0404 requires BLE LESC pairing, which involves LE Secure Connections (ECDH-based cryptographic key exchange). While the BLE stack handles the cryptography, the firmware must orchestrate the pairing flow (passkey relay, confirm/reject, timeout). MD-0405 requires BLE connection lifecycle state management, which is session-like behavior.

**Evidence:**
- Design §1 line 14: `"The modem firmware is a simple bidirectional bridge between USB-CDC and ESP-NOW."`
- Design §1 line 32: `"The firmware is intentionally minimal — no crypto, no CBOR parsing, no sessions, no OTA updates."`
- MD-0404: `"The modem MUST support BLE LESC Numeric Comparison pairing"`
- MD-0405: `"The modem MUST support one BLE connection at a time [...] clean up all GATT state"`
- MD-0414: `"the modem MUST send BLE_PAIRING_CONFIRM [...] and wait for BLE_PAIRING_CONFIRM_REPLY"`

**Root Cause:**
Same as F-001 — the overview was written for the ESP-NOW-only scope and not updated when BLE requirements were added.

**Impact:**
Implementers reading the design document will form incorrect mental models of the firmware's scope and constraints. The "no crypto" and "no sessions" claims may lead implementers to avoid patterns needed for BLE (e.g., pairing state machine, connection tracking).

**Remediation:**
Update the overview to describe the modem as a tri-directional bridge (USB-CDC ↔ ESP-NOW + USB-CDC ↔ BLE GATT). Revise the "intentionally minimal" statement to clarify that BLE LESC pairing is delegated to the BLE stack but the firmware orchestrates the pairing flow and manages one BLE connection at a time.

**Confidence:** Certain — direct textual contradiction.

---

### F-003 — Design architecture diagram omits BLE module

| Field | Value |
|-------|-------|
| **Severity** | High |
| **Category** | D5_ASSUMPTION_DRIFT |
| **Location** | Design §1 (lines 17–29); Requirements §7 |

**Description:**
The ASCII architecture diagram in design §1 shows six internal modules: USB-CDC Driver, Serial Codec, Bridge Logic, ESP-NOW Driver, Counters & Status, and Peer Table. There is no BLE Driver, BLE GATT module, or BLE state manager in the diagram.

**Evidence:**
The diagram (lines 17–29) enumerates exactly six boxes. None mentions BLE.

**Root Cause:**
Same as F-001 — the diagram predates BLE requirements.

**Impact:**
The architecture diagram is the first thing implementers see. Its absence of BLE establishes a misleading frame for the entire design document.

**Remediation:**
Add BLE Driver, BLE GATT Service, and BLE Pairing State modules to the architecture diagram. Show data flow: BLE Driver ↔ Serial Codec (via Bridge Logic) for `BLE_RECV`/`BLE_INDICATE` messages, and BLE Driver ↔ Bridge Logic for lifecycle events (`BLE_CONNECTED`, `BLE_DISCONNECTED`, `BLE_ENABLE`, `BLE_DISABLE`).

**Confidence:** Certain.

---

### F-004 — MD-0200 (ESP-NOW initialization) has no dedicated test case

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D2_UNTESTED_REQUIREMENT |
| **Location** | Requirements §4 MD-0200; Validation Appendix A |

**Description:**
No test case lists MD-0200 in its "Validates:" field. MD-0200 requires the modem to initialize ESP-NOW in WiFi station mode on the configured channel (default: channel 1), with acceptance criteria that the modem can receive and transmit ESP-NOW frames after `MODEM_READY`.

**Evidence:**
A search for "MD-0200" in the validation document returns zero matches in any "Validates:" field or in Appendix A.

**Root Cause:**
MD-0200 is implicitly validated by T-0200 (frame forwarding) and T-0201 (frame transmission), which both require working ESP-NOW. A dedicated test was likely deemed redundant.

**Impact:**
Low — the requirement is implicitly covered. However, if ESP-NOW initialization regresses in isolation (e.g., wrong default channel, station mode not set), the root cause would be harder to diagnose from the higher-level frame forwarding tests.

**Remediation:**
Either (a) add a dedicated T-0200a test that verifies ESP-NOW is initialized on channel 1 after boot (e.g., by sending a frame on channel 1 and receiving it without a prior `SET_CHANNEL`), or (b) add MD-0200 to the "Validates:" field of T-0200 and T-0201 to establish explicit traceability.

**Confidence:** Certain — exhaustive search of validation document.

---

### F-005 — MD-0302 (Watchdog timer) untested

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D2_UNTESTED_REQUIREMENT |
| **Location** | Requirements §5 MD-0302; Validation (entire document) |

**Description:**
MD-0302 (Watchdog timer, priority: Should) has no test case. The requirement specifies a 10-second watchdog timeout that triggers a hardware reset if the main loop stalls. The acceptance criterion is: "A deliberate infinite loop in test firmware triggers a watchdog reset within 10 seconds."

**Evidence:**
A search for "MD-0302" and "watchdog" in validation document "Validates:" fields returns zero matches. The design document §11 describes the watchdog implementation but no test exercises it.

**Root Cause:**
Testing a watchdog requires either: (a) a special test firmware build with a deliberate stall, or (b) a fault injection mechanism. Neither is mentioned in the test harness description (validation §2). The test was likely deferred due to infrastructure complexity.

**Impact:**
Medium — the watchdog is a reliability safety net. If misconfigured (wrong timeout, wrong task registered, `trigger_panic` not set), it silently fails to protect against firmware hangs. This is a "Should" priority requirement, so it's not blocking, but it's the only reliability mechanism besides RESET.

**Remediation:**
Add a test case (e.g., T-0304) using a dedicated test firmware build that enters an infinite loop after a trigger command. Assert: modem reboots and sends `MODEM_READY` within 15 seconds.

**Confidence:** Certain.

---

### F-006 — Error handling behaviors lack requirement IDs

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D3_ORPHANED_DESIGN_DECISION |
| **Location** | Design §13 (error handling table); Validation T-0400, T-0401 |

**Description:**
The design document §13 specifies error handling behaviors that are tested (T-0400, T-0401) but have no corresponding requirement IDs:

1. **`SEND_FRAME` with body < 7 bytes** — Design §13: "Silently discard (codec returns `BodyTooShort`, bridge continues)." Tested by T-0400, which references `modem-protocol.md §6.1` instead of a requirement ID.

2. **`SET_CHANNEL` with invalid channel** — Design §13: "Send `ERROR(CHANNEL_SET_FAILED)` to gateway." Tested by T-0401, which references `modem-protocol.md §6.1` instead of a requirement ID.

3. **ESP-NOW init failure / WiFi init failure** — Design §13: "Panic → automatic reboot." No test, no requirement.

**Evidence:**
- T-0400 "Validates:" field reads `modem-protocol.md §6.1` (not an MD-XXXX ID).
- T-0401 "Validates:" field reads `modem-protocol.md §6.1` (not an MD-XXXX ID).
- Design §13 error handling table lists these behaviors without requirement IDs.

**Root Cause:**
These behaviors are defined in the modem protocol specification (`modem-protocol.md §6.1`) but were not elevated to formal requirements in `modem-requirements.md`. The design and test documents reference the protocol doc directly, bypassing the requirement layer.

**Impact:**
The requirements document presents an incomplete picture of mandatory modem behaviors. A reader of `modem-requirements.md` alone would not know that `SEND_FRAME` body validation or `SET_CHANNEL` error responses are specified behaviors.

**Remediation:**
Create requirements for these behaviors (e.g., MD-0208 for `SEND_FRAME` body validation, MD-0209 for `SET_CHANNEL` error response). Update T-0400 and T-0401 "Validates:" fields to reference the new requirement IDs.

**Confidence:** Certain.

---

### F-007 — MD-0104 timing constraint not traced in design

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D1_UNTRACED_REQUIREMENT |
| **Location** | Requirements §3 MD-0104; Design §10 |

**Description:**
MD-0104 requires `MODEM_READY` within 2 seconds of USB enumeration. The design document §10 (Reset behavior) describes sending `MODEM_READY` as step 7 of the reset sequence, and §4.4 mentions `MODEM_READY` on USB reconnection, but neither section states the 2-second timing constraint. MD-0104 is also absent from the §3.1 module responsibility table.

**Evidence:**
- MD-0104: "The modem firmware MUST send `MODEM_READY` within 2 seconds of USB enumeration."
- Design §10 step 7: "Send `MODEM_READY`." (no timing mentioned)
- Design §3.1: MD-0104 not listed in any module's "Requirements covered" column.

**Root Cause:**
The design document describes the functional behavior (sending `MODEM_READY`) but omits the performance constraint (2-second deadline). Performance requirements are often overlooked in design documents that focus on functional decomposition.

**Impact:**
Low — the timing constraint is clear in the requirements and tested by T-0101 and T-0303. However, an implementer reading only the design document would not know about the deadline.

**Remediation:**
Add the 2-second timing constraint to design §10 and/or §4.4. Add MD-0104 to the Serial Codec or Bridge Logic module in §3.1.

**Confidence:** Certain.

---

### F-008 — MD-0300 and MD-0302 not traced in design module table

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D1_UNTRACED_REQUIREMENT |
| **Location** | Requirements §5 (MD-0300, MD-0302); Design §3.1, §10, §11 |

**Description:**
MD-0300 (Reset command) and MD-0302 (Watchdog timer) are described in the design document by content (§10 and §11 respectively) but are not traced by ID in the §3.1 module responsibility table. This means the module table's "Requirements covered" column presents an incomplete picture.

**Evidence:**
- Design §3.1: Six modules listed. No module claims MD-0300 or MD-0302.
- Design §10: Describes reset behavior matching MD-0300 but does not cite the requirement ID.
- Design §11: Describes watchdog behavior matching MD-0302 but does not cite the requirement ID.

**Root Cause:**
The module table focuses on the codec/bridge/driver modules. Cross-cutting concerns (reset, watchdog) are described in standalone sections but not linked back to requirement IDs.

**Impact:**
Low — the behaviors are clearly described in dedicated design sections. The gap is purely a traceability bookkeeping issue.

**Remediation:**
Add a "Cross-cutting" row to the §3.1 table mapping Reset behavior to MD-0300 and Watchdog to MD-0302. Alternatively, assign these to the Bridge Logic module.

**Confidence:** Certain.

---

### F-009 — T-0400, T-0401, T-0500 reference non-requirement sources

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D4_ORPHANED_TEST_CASE |
| **Location** | Validation §6 (T-0400, T-0401), §7 (T-0500) |

**Description:**
Three test cases have "Validates:" fields that reference source documents or document sections instead of MD-XXXX requirement IDs:

| Test | Validates field | Issue |
|------|----------------|-------|
| T-0400 | `modem-protocol.md §6.1` | Not a requirement ID |
| T-0401 | `modem-protocol.md §6.1` | Not a requirement ID |
| T-0500 | `§6 Non-requirements` | References a section header |

These tests are not orphaned in the traditional sense (they don't reference non-existent MD-XXXX IDs), but they break the requirement traceability chain. An automated traceability report filtering on MD-XXXX patterns would miss these tests entirely.

**Evidence:**
- Validation Appendix A rows for T-0400, T-0401, T-0500 confirm the non-standard "Validates" values.

**Root Cause:**
For T-0400 and T-0401: the underlying behaviors have no requirement IDs (see F-006). For T-0500: "non-requirements" are negative constraints documented in §6 of the requirements doc but don't have formal IDs.

**Impact:**
Low — the tests are valid and valuable. The issue is that traceability tooling and manual audits may miss them.

**Remediation:**
For T-0400 and T-0401: create requirement IDs per F-006 and update "Validates:" fields. For T-0500: either assign a requirement ID to the non-requirements section (e.g., MD-0304: "Opaque transport — no content inspection") or leave as-is with a note that T-0500 validates negative constraints.

**Confidence:** Certain.

---

### F-010 — `uptime_s` reset behavior not tested

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D7_ACCEPTANCE_CRITERIA_MISMATCH |
| **Location** | Requirements §5 MD-0303 AC5; Validation §5 T-0300, T-0302 |

**Description:**
MD-0303 acceptance criterion 5 states: *"`uptime_s` reflects seconds since last boot or `RESET`."* This implies `uptime_s` should reset to zero (or near-zero) on `RESET`. However:

- T-0300 (RESET clears state) asserts `tx_count` = 0, `rx_count` = 0, `tx_fail_count` = 0, and `channel` = 1 after RESET — but does **not** assert `uptime_s` is near zero.
- T-0302 (Status counter accuracy) asserts `uptime_s` > 0 but does not test its behavior across a RESET.

**Evidence:**
- T-0300 procedure step 7: "Assert: `tx_count` = 0, `rx_count` = 0, `tx_fail_count` = 0." (no `uptime_s` assertion)
- T-0300 procedure step 8: "Assert: `channel` = 1" (no `uptime_s` assertion)
- MD-0303 AC5: "`uptime_s` reflects seconds since last boot or `RESET`."
- Design §8: "All counters reset to zero on boot and on `RESET`."

**Root Cause:**
T-0300 was likely written to cover the most critical state (counters, channel) and `uptime_s` was overlooked.

**Impact:**
Low — `uptime_s` is a diagnostic counter, not a functional one. However, if the implementation doesn't reset `uptime_s` on `RESET`, the counter would reflect total uptime rather than session uptime, which could mislead diagnostics.

**Remediation:**
Add an assertion to T-0300: "Assert: `uptime_s` < 3" (allowing for the time taken by the RESET sequence and GET_STATUS round-trip).

**Confidence:** Certain.

---

### F-011 — Peer table cleared after `SET_CHANNEL` not directly tested

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D7_ACCEPTANCE_CRITERIA_MISMATCH |
| **Location** | Requirements §4 MD-0206 AC3; Validation §4 T-0205 |

**Description:**
MD-0206 acceptance criterion 3 states: *"The peer table is empty after a channel change."* T-0205 tests that a channel change works (sends/receives on the new channel and doesn't receive on the old channel) but does not explicitly verify peer table emptiness.

**Evidence:**
- MD-0206 AC3: "The peer table is empty after a channel change."
- T-0205 procedure: Steps 1–7 verify radio behavior on old/new channels. No step checks peer table state.

**Root Cause:**
The peer table is an internal data structure not directly observable via the serial protocol. The `STATUS` message does not report peer table size. Verifying emptiness would require either: (a) sending to a previously-registered peer after channel change and confirming re-registration occurs, or (b) adding a peer count to the `STATUS` response.

**Impact:**
Low — peer table clearing is an internal invariant. If the modem fails to clear peers on channel change, the practical effect is stale peers consuming table slots, which would eventually cause LRU eviction (tested separately by T-0203).

**Remediation:**
Add a step to T-0205 (or create T-0205a): After channel change, send `SEND_FRAME` to a MAC that was previously registered on the old channel. If the modem re-registers it (observable via ESP-NOW add_peer log on UART), the table was cleared. Alternatively, add `peer_count` to the `STATUS` message.

**Confidence:** High — the gap is clear, but the practical impact is debatable.

---

### F-012 — Write Long reassembly (MD-0409 AC2) not explicitly tested

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D7_ACCEPTANCE_CRITERIA_MISMATCH |
| **Location** | Requirements §7 MD-0409 AC2; Validation §8 T-0613 |

**Description:**
MD-0409 acceptance criterion 2 states: *"Write Long payloads are reassembled before forwarding."* BLE Write Long (ATT Prepare Write + Execute Write) is a multi-step GATT write mechanism for payloads exceeding (MTU − 3) bytes. No test case explicitly exercises Write Long behavior. T-0613 tests a regular GATT write but does not specify payload sizes that would trigger Write Long.

**Evidence:**
- MD-0409 AC2: "Write Long payloads are reassembled before forwarding."
- T-0613 procedure: "Phone writes a BLE envelope to the Gateway Command characteristic." (no size specified, no mention of Write Long)

**Root Cause:**
Write Long is a BLE stack-level mechanism. With MTU ≥ 247 (MD-0402), payloads up to 244 bytes can be sent in a single ATT write, which covers the expected BLE pairing envelope sizes. Write Long may only be needed for edge cases.

**Impact:**
Low — if the BLE stack handles Write Long reassembly transparently (which most stacks do), the modem firmware doesn't need explicit handling. However, the acceptance criterion explicitly calls it out, so it should be tested.

**Remediation:**
Add a test (e.g., T-0613c) that writes a payload > (MTU − 3) bytes via BLE Write Long and asserts the modem reassembles and forwards it as a single `BLE_RECV` message.

**Confidence:** High.

---

### F-013 — Numbering continuity intact; superseded/removed IDs properly annotated

| Field | Value |
|-------|-------|
| **Severity** | Informational |
| **Category** | N/A (positive finding) |
| **Location** | Requirements Appendix A; Validation Appendix A |

**Description:**
The requirement numbering (MD-0100–MD-0414) and test numbering (T-0100–T-0622) have no unexplained gaps. All gaps are accounted for:

- **MD-0406** is explicitly marked *"Superseded by MD-0410/MD-0411"* with a note explaining the supersession.
- **T-0610** is marked *"Removed — superseded by T-0614 and T-0615."*
- **T-0617** and **T-0618** are marked *"Subsumed by T-0600."*

The validation Appendix A accurately reflects the body text for all "Validates:" mappings — no discrepancies were found between inline references and the index table.

**Evidence:**
- Requirements Appendix A: MD-0406 row shows priority "—" and title "*(Superseded by MD-0410/MD-0411)*".
- Validation Appendix A: T-0610 row shows "*(Removed — superseded by T-0614/T-0615)*".
- All 45 test index entries match their body "Validates:" fields.

**Confidence:** Certain.

---

## 5. Root Cause Analysis

All 12 defect findings trace to two root causes:

### Root Cause 1: BLE requirements added without design document update

Findings F-001, F-002, F-003 all stem from the same event: the requirements document was extended with §7 (BLE pairing relay, MD-0400–MD-0414) but the design document was not updated. This is the classic "document drift" problem in iterative specification development. The requirements and validation documents evolved together (BLE requirements have comprehensive test coverage), but the design document was left behind.

**Contributing factor:** The design document's opening statements ("bidirectional bridge", "no crypto", "no sessions") create a strong framing that may have discouraged additions — updating the overview to include BLE would require reconceptualizing the modem's identity, not just appending a section.

### Root Cause 2: Protocol-level behaviors not elevated to requirements

Findings F-006 and F-009 reflect a pattern where behaviors defined in `modem-protocol.md` are implemented in the design and tested in validation, but bypass the requirements document entirely. The traceability chain should be: protocol spec → requirement → design → test. Instead, the chain is: protocol spec → design + test (skipping requirements).

### Coverage Metrics

| Metric | Value | Detail |
|--------|-------|--------|
| **Total active requirements** | 31 | MD-0100–MD-0414, excluding superseded MD-0406 |
| **Requirements → Design (by ID)** | 14/31 (45.2%) | Traced in §3.1 module table or inline reference |
| **Requirements → Design (by content)** | 2/31 (6.5%) | MD-0300 (§10), MD-0302 (§11) — described but not cited by ID |
| **Requirements → Design (partial)** | 1/31 (3.2%) | MD-0104 — behavior described, timing constraint absent |
| **Requirements → Design (untraced)** | 14/31 (45.2%) | All BLE requirements (MD-04xx except MD-0406) |
| **Requirements → Design (effective)** | 16/31 (51.6%) | By-ID + by-content |
| **Requirements → Test coverage** | 29/31 (93.5%) | MD-0200, MD-0302 untested |
| **Active test cases** | 45 | Excluding removed T-0610 |
| **Tests → Requirement (MD-XXXX)** | 42/45 (93.3%) | T-0400, T-0401, T-0500 reference protocol/section |
| **Orphaned tests (invalid MD-XXXX)** | 0/45 (0%) | No test references a non-existent requirement ID |
| **Numbering gaps (unexplained)** | 0 | All gaps have explicit supersession/removal notes |

---

## 6. Remediation Plan

| Priority | Finding | Action | Effort |
|----------|---------|--------|--------|
| **P0** | F-001 | Add BLE design sections (architecture, modules, data flow, error handling, state machines) covering MD-0400–MD-0414 | Large |
| **P0** | F-002 | Update design §1 overview to describe tri-directional bridge; revise "no crypto/sessions" claims | Small |
| **P0** | F-003 | Update architecture diagram to include BLE modules | Small |
| **P1** | F-005 | Add watchdog test case (T-0304) using test firmware with deliberate stall | Medium |
| **P1** | F-006 | Create MD-0208 (SEND_FRAME body validation) and MD-0209 (SET_CHANNEL error response); update T-0400, T-0401 | Small |
| **P2** | F-004 | Add MD-0200 to "Validates:" field of T-0200 and T-0201 | Trivial |
| **P2** | F-007 | Add 2-second timing constraint to design §10 and/or §4.4 | Trivial |
| **P2** | F-008 | Add MD-0300, MD-0302 to §3.1 module table | Trivial |
| **P2** | F-009 | Update T-0400, T-0401, T-0500 "Validates:" fields (depends on F-006) | Trivial |
| **P3** | F-010 | Add `uptime_s` assertion to T-0300 | Trivial |
| **P3** | F-011 | Add peer table verification step to T-0205 or create T-0205a | Small |
| **P3** | F-012 | Add Write Long test (T-0613c) | Small |

---

## 7. Prevention

To prevent recurrence of these issues:

1. **Mandatory trifecta update rule:** Any PR that adds or modifies a requirement MUST include corresponding updates to both the design and validation documents. CI could enforce this by checking that PRs touching `modem-requirements.md` also touch `modem-design.md` and `modem-validation.md`.

2. **Automated traceability checks:** A CI script should:
   - Extract all MD-XXXX IDs from `modem-requirements.md`.
   - Verify each appears in at least one "Requirements covered" cell in `modem-design.md`.
   - Verify each appears in at least one "Validates:" field in `modem-validation.md`.
   - Flag any "Validates:" field that does not contain a valid MD-XXXX ID.

3. **Design module table as traceability matrix:** Treat the §3.1 table as the single source of truth for requirement → design traceability. Every active requirement ID must appear in at least one module's "Requirements covered" column.

4. **Review checklist item:** Add "Are all new requirement IDs traced in the design module table and validated by at least one test case?" to the PR review checklist for specification changes.

---

## 8. Open Questions

1. **Was the BLE design deliberately deferred?** If BLE design is being developed in a separate branch or document, F-001/F-002/F-003 may be expected work-in-progress rather than defects. Clarification from the specification owner is needed.

2. **Should protocol-level error behaviors have formal requirement IDs?** The current pattern (T-0400/T-0401 referencing `modem-protocol.md §6.1` directly) may be intentional to avoid requirement proliferation. If so, the traceability convention should be documented.

3. **Is the watchdog testable in the current test harness?** MD-0302 may require a dedicated test firmware build with a controllable stall mechanism. If the test infrastructure cannot support this, the requirement should be annotated as "validated by code inspection" rather than left silently untested.

4. **Should `STATUS` include `peer_count`?** F-011 identifies that peer table state is not directly observable. Adding a `peer_count` field to the `STATUS` response would enable testing of peer table invariants (cleared after channel change, LRU eviction threshold) without relying on log inspection.

---

## 9. Revision History

| Version | Date | Author | Description |
|---------|------|--------|-------------|
| 1.0 | 2026-03-20 | Copilot (specification analyst) | Initial audit |
