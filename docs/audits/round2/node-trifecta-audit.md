<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Node Firmware Trifecta Audit — Investigation Report

## 1. Executive Summary

A systematic traceability audit of the node firmware specification set (72 requirements in `node-requirements.md`, 17 design sections in `node-design.md`, and 120+ test cases in `node-validation.md`) found **15 findings** across 4 severity levels. The most impactful is a **D6 constraint violation** where the design specifies response timeout and retry delay values (50 ms / 100 ms) that directly conflict with the requirements (200 ms / 400 ms), which would cause the system to be non-functional on the reference USB-CDC modem bridge hardware. Six **D7 acceptance-criteria mismatches** create false coverage — test cases are linked to requirements but do not actually verify specific acceptance criteria. Three **D2 untested requirements** and four **D1 untraced requirements** round out the gaps. Overall forward traceability is strong (95.2% requirements→design, 95.2% requirements→validation), but the acceptance-criteria–level coverage has systemic blind spots in constraint enforcement, edge cases, and logging.

## 2. Problem Statement

The node firmware specification set (requirements, design, validation) must be internally consistent to ensure the implementation meets all stated requirements. This audit was conducted to detect specification drift — gaps, conflicts, and divergence — across the three documents before implementation proceeds further. The audit covers all 72 requirements (ND-0100 through ND-1012).

## 3. Investigation Scope

- **Documents examined:**
  - `docs/node-requirements.md` — 72 requirements (68 Must, 4 Should)
  - `docs/node-design.md` — 17 major sections, 11 functional modules
  - `docs/node-validation.md` — 120+ test cases, traceability matrix (Appendix A)
- **Tools used:** Manual cross-document traceability analysis; systematic enumeration of REQ-IDs, test case IDs, and design section references.
- **Limitations:**
  - Source code was NOT examined — this audit covers specification documents only (D1–D7 taxonomy). Code compliance (D8–D10) and test compliance (D11–D13) audits are out of scope.
  - Semantic correctness of individual requirements, design decisions, and test procedures was not assessed — only cross-document consistency.

## 4. Findings

### Finding F-001: Design timeout/retry values conflict with requirements

- **Severity:** High
- **Category:** D6_CONSTRAINT_VIOLATION
- **Location:**
  - Requirements: ND-0700 (§9, line ~627), ND-0701 (§9, line ~641), ND-0702 (§9, line ~656)
  - Design: §4.2 step 4 (line ~134), §4.3 (line ~153)
- **Description:** The requirements specify a 200 ms response timeout and 400 ms inter-retry delay for both WAKE and chunk transfer exchanges. The design specifies 50 ms timeout and 100 ms between retries, a 4× discrepancy on both values.
- **Evidence:**
  - ND-0702 AC1: "the node uses a response timeout of 200 ms"
  - ND-0700 AC2: "The delay between retries is 400 ms"
  - ND-0701 AC2 (implicit via ND-0700 reference): 400 ms delay
  - Design §4.2: "Wait up to 50 ms. If no response, retry (up to 3 times, 100 ms between)"
  - Design §4.3: "50 ms timeout, up to 3 retries per chunk"
  - Validation T-N201: "retries up to 3 times (400 ms apart)" — matches requirements
  - Validation T-N702: "delays response by 300 ms (>200 ms timeout)" — matches requirements
- **Root Cause:** The design was likely written before ND-0702 was updated to reflect the USB-CDC modem bridge latency budget. The validation plan was updated but the design was not.
- **Impact:** If implemented per the design, the node will time out before any response arrives through the USB-CDC modem bridge (physical round-trip exceeds 50 ms). The node would never successfully complete a WAKE/COMMAND exchange on the reference hardware, rendering it non-functional.
- **Remediation:** Update design §4.2 step 4 and §4.3 to use 200 ms response timeout and 400 ms inter-retry delay, matching ND-0700, ND-0701, and ND-0702.
- **Confidence:** High — the conflict is explicit in the text of both documents.

---

### Finding F-002: ND-0500 AC4 — program image size cap enforcement not tested

- **Severity:** High
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Requirements: ND-0500 AC4 (§7, line ~347)
  - Validation: T-N500 (§7, line ~396)
- **Description:** ND-0500 AC4 requires the node to reject resident program images exceeding 4096 bytes and ephemeral program images exceeding 2048 bytes. T-N500 (the only test linked to ND-0500) tests a normal 4-chunk transfer and verifies ordering, sequence numbers, and hash — but never tests size cap enforcement.
- **Evidence:**
  - ND-0500 AC4: "Resident program images MUST NOT exceed 4096 bytes. Ephemeral program images MUST NOT exceed 2048 bytes. The node MUST reject any program image that exceeds the applicable size cap."
  - T-N500 procedure: tests 4-chunk transfer with correct data, asserts PROGRAM_ACK — no oversized image tested.
- **Root Cause:** The size cap was added to ND-0500 as AC4 but no corresponding test case was created.
- **Impact:** The traceability matrix shows ND-0500 as tested, but the size cap enforcement — a security boundary preventing oversized programs from being installed — has no verification. An implementation could omit the check without being caught.
- **Remediation:** Add test cases for (a) a resident program image slightly exceeding 4096 bytes — assert rejected, and (b) an ephemeral program image slightly exceeding 2048 bytes — assert rejected.
- **Confidence:** High — the AC is explicit and no test addresses it.

---

### Finding F-003: ND-0203 AC4 — 1-second minimum sleep interval not tested

- **Severity:** High
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Requirements: ND-0203 AC4 (§4, line ~180)
  - Validation: T-N208, T-N209, T-N923 (§4, lines ~219–291)
- **Description:** ND-0203 AC4 requires the node to clamp any computed sleep duration below 1 second to 1 second. The linked tests (T-N208, T-N209, T-N923) test `set_next_wake()` clamping against the base interval but none test the 1-second floor.
- **Evidence:**
  - ND-0203 AC4: "The node MUST enforce a minimum sleep interval of 1 second. Any computed sleep duration below 1 second MUST be clamped to 1 second."
  - T-N208: tests `set_next_wake(10)` with base 300s — asserts 10s sleep
  - T-N209: tests `set_next_wake(600)` with base 60s — asserts 60s sleep
  - T-N923: tests `set_next_wake(5000)` with base 60s — asserts 5s sleep
  - No test uses a sub-1-second value (e.g., `set_next_wake(0)` or a 0-second base interval).
- **Root Cause:** AC4 was added after the initial test cases were written.
- **Impact:** Without a test for the 1-second floor, an implementation could allow 0-second sleep intervals, causing the node to busy-loop and drain its battery.
- **Remediation:** Add a test: set base interval to 1s, call `set_next_wake(0)`, assert node sleeps for 1 second (not 0).
- **Confidence:** High.

---

### Finding F-004: ND-0503 AC4 — ephemeral programs declaring maps not tested for rejection

- **Severity:** High
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Requirements: ND-0503 AC4 (§7, line ~412)
  - Validation: T-N505 (§7, line ~456)
- **Description:** ND-0503 AC4 requires that ephemeral program images declaring maps MUST be rejected (they are read-only and must not allocate map storage). T-N505 tests that an ephemeral program runs from RAM, executes, and is freed — but never tests rejection of a map-declaring ephemeral image.
- **Evidence:**
  - ND-0503 AC4: "Ephemeral program images that declare maps MUST be rejected."
  - T-N505: asserts RAM storage, execution, cleanup, resident unaffected — no map-declaring ephemeral image tested.
- **Root Cause:** AC4 was added as a security constraint after T-N505 was written.
- **Impact:** An implementation could silently accept ephemeral programs with map declarations, potentially corrupting persistent map storage.
- **Remediation:** Add a test: transfer an ephemeral program image with map definitions, assert the node rejects it (no execution, no map allocation).
- **Confidence:** High.

---

### Finding F-005: ND-1009 AC4–AC5 — program install and chunk transfer failure logging not tested

- **Severity:** High
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Requirements: ND-1009 AC4–AC5 (§10, lines ~1188–1190)
  - Validation: T-N1009, T-N1010, T-N1011 (§11, lines ~1165–1195)
- **Description:** ND-1009 has 5 acceptance criteria. Tests T-N1009 (RNG failure), T-N1010 (WAKE retries exhausted), and T-N1011 (HMAC mismatch) cover AC1–AC3. No test covers AC4 (WARN log on program install failure) or AC5 (WARN log on chunk transfer failure).
- **Evidence:**
  - ND-1009 AC4: "A WARN log is emitted when a program install fails (hash mismatch, decode error, or map budget exceeded), including the error description."
  - ND-1009 AC5: "A WARN log is emitted when a chunk transfer fails (timeout, size mismatch), including the error description."
  - T-N1009/T-N1010/T-N1011: validate AC1, AC2, AC3 respectively. No test for AC4 or AC5.
- **Root Cause:** Tests were written for the original 3 acceptance criteria; AC4 and AC5 were added later.
- **Impact:** Program install failures and chunk transfer failures could silently occur without diagnostic output, making field debugging difficult.
- **Remediation:** Add two tests: (a) trigger a hash-mismatch program install failure, assert WARN log with error description; (b) trigger a chunk transfer timeout, assert WARN log with error description.
- **Confidence:** High.

---

### Finding F-006: ND-0915 AC3 — WAKE failure after `peer_payload` erasure not tested

- **Severity:** High
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Requirements: ND-0915 AC3 (§11, line ~969)
  - Validation: T-N917 (§11, line ~1032)
- **Description:** ND-0915 AC3 requires that a WAKE failure when `reg_complete` is set but `peer_payload` has already been erased does NOT clear the `reg_complete` flag (the node cannot self-heal and must continue normal WAKE retries). T-N917 tests only the positive case (AC1–AC2: WAKE failure with `peer_payload` present → clear flag).
- **Evidence:**
  - ND-0915 AC3: "A WAKE failure when `reg_complete` is set but `peer_payload` has been erased (ND-0914) does not clear the flag; the node continues normal WAKE retries."
  - T-N917: only asserts "`reg_complete` flag is cleared" — does not test the negative case where `peer_payload` is absent.
- **Root Cause:** T-N917 was written for the common case; the negative-path edge case (AC3) was added later in requirements.
- **Impact:** An implementation could clear `reg_complete` on every WAKE failure regardless of `peer_payload` state, causing the node to enter an unrecoverable PEER_REQUEST loop when the gateway is temporarily unavailable after normal registration.
- **Remediation:** Add a test: complete registration, erase `peer_payload` (simulate ND-0914 completion), then trigger WAKE failure. Assert `reg_complete` remains set; assert node retries WAKE, not PEER_REQUEST.
- **Confidence:** High.

---

### Finding F-007: ND-0910 AC3 — malformed `peer_payload` handling not tested

- **Severity:** High
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Requirements: ND-0910 AC3 (§11, line ~892)
  - Validation: T-N910 (§11, line ~949)
- **Description:** ND-0910 AC3 requires that if `peer_payload` is malformed, the node attempts to erase it from NVS to break retry loops, and if erasure fails, treats it as permanently invalid. T-N910 tests only the normal retransmission case (AC1–AC2).
- **Evidence:**
  - ND-0910 AC3: "If the `peer_payload` is malformed…the node MUST attempt to erase the `peer_payload` from NVS to break retry loops; if the erase fails, the node MUST treat the stored `peer_payload` as permanently invalid and MUST NOT continue retransmitting it."
  - T-N910: provisions node, allows 2 wake cycles without PEER_ACK, asserts retransmission — does not test malformed payload.
- **Root Cause:** AC3 is a defensive robustness criterion added after the initial test was written.
- **Impact:** A malformed `peer_payload` could cause infinite retry loops, draining the battery as the node repeatedly attempts to send an invalid PEER_REQUEST.
- **Remediation:** Add test: write a malformed blob to NVS key `peer_payload`, boot node into PEER_REQUEST path. Assert: node attempts to erase `peer_payload` and does not retransmit on subsequent wake cycles.
- **Confidence:** High.

---

### Finding F-008: ND-0607 (Initial map data) — untested requirement

- **Severity:** High
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:**
  - Requirements: ND-0607 (§8, line ~583) — Priority: Must
  - Validation: Appendix A traceability matrix — ND-0607 absent
- **Description:** ND-0607 (Initial map data) is a Must-priority requirement with zero test coverage. No test case in the validation plan references ND-0607 or verifies that map definitions with `initial_data` (CBOR key 5) are correctly decoded and applied.
- **Evidence:**
  - ND-0607 does not appear in the Appendix A traceability matrix.
  - Full-text search of validation document: no test case header contains "ND-0607" or "initial_data" or "Initial map data."
- **Root Cause:** ND-0607 was added to the requirements after the validation plan was established.
- **Impact:** The mechanism for carrying ELF `.rodata`/`.data` content (compile-time constants) into BPF program maps is unverified. Programs relying on initial map data could silently receive zero-filled maps.
- **Remediation:** Add test cases covering ND-0607 AC1–AC4: (1) program image with `initial_data` accepted, (2) entry 0 populated with initial bytes, (3) maps without initial data remain zero-filled, (4) mismatched `initial_data` length silently ignored.
- **Confidence:** High.

---

### Finding F-009: ND-0607 (Initial map data) — untraced in design

- **Severity:** High
- **Category:** D1_UNTRACED_REQUIREMENT
- **Location:**
  - Requirements: ND-0607 (§8, line ~583) — Priority: Must
  - Design: §7.2 `MapDef` struct (line ~262), §9.2 Map allocation (line ~406)
- **Description:** ND-0607 requires optional `initial_data` support in the program image format (CBOR key 5). The design's `MapDef` struct does not include an `initial_data` field, and the map allocation procedure (§9.2) only describes zero-initialization, with no mention of writing initial data to entry 0.
- **Evidence:**
  - ND-0607: "The program image format MUST support optional initial data for each map definition."
  - Design §7.2 `MapDef`: fields are `map_type`, `key_size`, `value_size`, `max_entries` — no `initial_data`.
  - Design §9.2 step 4: "Zero-initialize all map storage." — no conditional for initial data.
  - Design §3.1 module table: BPF Runtime covers "ND-0600–0606" — ND-0607 is excluded from the range.
- **Root Cause:** ND-0607 was added to the requirements after the design was finalized. The design data model and allocation procedure were not updated.
- **Impact:** An implementer following only the design document will not know to add `initial_data` to `MapDef`, and will not implement the initial data write logic. Programs relying on compile-time constants in maps will fail.
- **Remediation:** (1) Add `initial_data: Option<Vec<u8>>` to the `MapDef` struct in §7.2. (2) Update §9.2 map allocation to include a step after zero-init: "For each map with non-empty `initial_data`, write the bytes to entry 0." (3) Add ND-0607 to the module responsibility table in §3.1.
- **Confidence:** High.

---

### Finding F-010: ND-1011 (Chunk transfer logging) — untested requirement

- **Severity:** Medium
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:**
  - Requirements: ND-1011 (§10, line ~1194) — Priority: Must
  - Validation: no test case references ND-1011
- **Description:** ND-1011 requires DEBUG-level logging for `GET_CHUNK` requests and `CHUNK` responses including chunk index and attempt number. No test case validates this.
- **Evidence:**
  - ND-1011 does not appear in the Appendix A traceability matrix.
  - No test case header in the validation plan references ND-1011.
- **Root Cause:** Logging tests were added for INFO-level events (T-N1000–T-N1015) but DEBUG-level chunk transfer logging was omitted.
- **Impact:** Chunk transfer logging may not be implemented or may use incorrect log levels, reducing diagnostic capability during program update failures.
- **Remediation:** Add a test: initiate a chunked transfer, assert DEBUG logs are emitted for each GET_CHUNK (with `chunk_index`, `attempt`) and each CHUNK response (with `chunk_index`, data length).
- **Confidence:** High.

---

### Finding F-011: ND-1012 (Build-type–aware log levels) — untested requirement

- **Severity:** Medium
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:**
  - Requirements: ND-1012 (§10, line ~1209) — Priority: Must
  - Validation: no test case references ND-1012
- **Description:** ND-1012 has 6 acceptance criteria governing compile-time log stripping, runtime defaults, and mutual exclusivity of `quiet`/`verbose` features. No test case validates any of these criteria.
- **Evidence:**
  - ND-1012 does not appear in the Appendix A traceability matrix.
  - No test case header references ND-1012.
  - The design (§17.1a) describes the mechanism in detail, but validation coverage is missing.
- **Root Cause:** ND-1012 is a build-system/configuration requirement; no test framework was established for build-level validation.
- **Impact:** Build configuration drift could cause release firmware to emit verbose logs (performance overhead) or verbose firmware to suppress diagnostic output.
- **Remediation:** Add tests: (1) build with `quiet` feature, assert `info!` calls are no-ops (compile check or binary inspection); (2) build with `verbose` feature, assert INFO output appears; (3) build with both features, assert compile error; (4) verify runtime default levels per build type.
- **Confidence:** High.

---

### Finding F-012: ND-0203 AC4 (minimum sleep) — untraced in design

- **Severity:** Medium
- **Category:** D1_UNTRACED_REQUIREMENT
- **Location:**
  - Requirements: ND-0203 AC4 (§4, line ~180)
  - Design: §11.1 Sleep entry (line ~471)
- **Description:** ND-0203 AC4 requires a 1-second minimum sleep interval. The design's sleep entry procedure (§11.1) calculates sleep as `min(set_next_wake_value, base_interval)` but does not describe a 1-second floor clamp.
- **Evidence:**
  - ND-0203 AC4: "The node MUST enforce a minimum sleep interval of 1 second."
  - Design §11.1: "Calculate sleep duration: `min(set_next_wake_value, base_interval)`." — no mention of minimum clamp.
- **Root Cause:** AC4 was added to the requirements after the design was finalized.
- **Impact:** An implementer following the design would not apply the 1-second floor.
- **Remediation:** Update design §11.1 step 1 to: "Calculate sleep duration: `max(1, min(set_next_wake_value, base_interval))`."
- **Confidence:** High.

---

### Finding F-013: ND-0503 AC4 (ephemeral map rejection) — untraced in design

- **Severity:** Medium
- **Category:** D1_UNTRACED_REQUIREMENT
- **Location:**
  - Requirements: ND-0503 AC4 (§7, line ~412)
  - Design: §8.3 Ephemeral restrictions (line ~359), §7.4 Ephemeral programs (line ~279)
- **Description:** ND-0503 AC4 requires rejection of ephemeral programs that declare maps. The design §8.3 only restricts helpers 11 and 15 at runtime for ephemeral programs. The design §7.4 describes ephemeral programs as RAM-stored and executed, with no mention of rejecting map-declaring images at load time.
- **Evidence:**
  - ND-0503 AC4: "Ephemeral program images that declare maps MUST be rejected."
  - Design §8.3: "For ephemeral programs, helpers 11 (`map_update_elem`) and 15 (`set_next_wake`) return an error code." — runtime restriction, not load-time rejection.
  - Design §7.4: "Ephemeral programs are stored in RAM…executed immediately, then the allocation is freed." — no mention of map check.
- **Root Cause:** AC4 was added as a load-time security constraint after the design described only runtime restrictions.
- **Impact:** An implementer following the design would enforce map restrictions at runtime only, potentially allowing ephemeral programs to allocate (and corrupt) persistent map storage before the first `map_update_elem` call is rejected.
- **Remediation:** Add to design §7.4: "Before storing an ephemeral program, verify it declares zero maps. If any map definitions are present, reject the image."
- **Confidence:** High.

---

### Finding F-014: ND-0500 AC4 (program image size caps) — not explicit in design

- **Severity:** Medium
- **Category:** D1_UNTRACED_REQUIREMENT
- **Location:**
  - Requirements: ND-0500 AC4 (§7, line ~347)
  - Design: §13 Memory budget (line ~511)
- **Description:** ND-0500 AC4 requires the node to actively reject program images exceeding 4096 bytes (resident) or 2048 bytes (ephemeral). The design §13 lists partition sizes (4 KB per partition, ≤ 2 KB ephemeral) as physical constraints but does not describe validation/rejection logic during transfer or install.
- **Evidence:**
  - ND-0500 AC4: "The node MUST reject any program image that exceeds the applicable size cap."
  - Design §13: "Flash (program) | 4 KB per program image", "Ephemeral program | Allocated from heap (≤ 2 KB)" — capacity listing, not enforcement logic.
- **Root Cause:** The design describes physical constraints but not the active validation step.
- **Impact:** Without explicit enforcement logic, an implementer might rely on write failures (flash full, heap OOM) rather than clean rejection with proper error handling.
- **Remediation:** Add to design §4.3 chunked transfer: after reassembly, validate `total_image_size ≤ 4096` (resident) or `≤ 2048` (ephemeral) before hash verification. Reject with discard + sleep if exceeded.
- **Confidence:** High.

---

### Finding F-015: Validation traceability matrix incomplete

- **Severity:** Low
- **Category:** D5_ASSUMPTION_DRIFT
- **Location:**
  - Validation: Appendix A traceability matrix (line ~1582)
- **Description:** The Appendix A traceability matrix — the authoritative requirement-to-test mapping — has three categories of omissions: (1) ND-1000–ND-1012 (all 13 logging requirements) are absent despite test cases T-N1000–T-N1015 existing in the document body; (2) ND-0607 (Initial map data) is absent (no test exists — see F-008); (3) several test cases are missing from their requirement's entry (e.g., T-N903a and T-N903b are not listed under ND-0904; T-N402 is not listed under ND-0900, ND-0906, ND-0909, ND-0913, ND-0914 despite its "Validates:" header referencing them).
- **Evidence:**
  - Appendix A ends at ND-0918 / ND-0608. No entries for ND-1000–ND-1012.
  - T-N903a "Validates: ND-0904 (criterion 3)" — not in matrix under ND-0904.
  - T-N903b "Validates: ND-0904 (criterion 4)" — not in matrix under ND-0904.
  - T-N402 "Validates: ND-0900, ND-0906, ND-0909, ND-0913, ND-0914" — not in matrix under those IDs.
- **Root Cause:** The traceability matrix was not updated when logging requirements (§10) and later test cases were added.
- **Impact:** Anyone relying on the matrix as the single source of coverage truth would incorrectly conclude that ND-1000–ND-1012 are untested and would miss several test-to-requirement links.
- **Remediation:** Extend Appendix A to include ND-0607 and ND-1000–ND-1012 entries. Add missing test IDs (T-N903a, T-N903b, T-N402) to their respective requirement rows.
- **Confidence:** High.

---

## 5. Root Cause Analysis

### 5.1 Causal Chain

The findings cluster into three root causes:

1. **Late-added acceptance criteria without downstream propagation (F-002, F-003, F-004, F-005, F-006, F-007, F-012, F-013, F-014).** Several requirements had acceptance criteria added after the design and validation documents were written. The new ACs were not propagated to the design or reflected in new test cases. This is the dominant pattern — 9 of 15 findings stem from this cause.

2. **Late-added requirements without document-set update (F-008, F-009, F-010, F-011).** ND-0607 (Initial map data), ND-1011 (Chunk transfer logging), and ND-1012 (Build-type log levels) were added to the requirements document but not reflected in the design or validation plan.

3. **Stale design values not synchronized with requirements updates (F-001).** The design's timing values predate the requirements' USB-CDC modem bridge latency analysis.

### 5.2 Coverage Metrics

| Metric | Value |
|--------|-------|
| **Total requirements** | 72 (68 Must, 4 Should) |
| **Requirements → Design (forward)** | 59/62 = **95.2%** (missing: ND-0403 [Should], ND-0403a [Should], ND-0607 [Must]) |
| **Requirements → Validation (forward)** | 59/62 = **95.2%** (missing: ND-0607, ND-1011, ND-1012 [all Must]) |
| **Design → Requirements (backward)** | 11/11 modules = **100%** (all modules trace to requirements) |
| **Validation → Requirements (backward)** | 120+/120+ test cases = **100%** (all test cases reference valid REQ-IDs) |
| **Acceptance criteria with test gaps** | 7 individual ACs linked to tests that do not verify them |
| **Constraint violations** | 1 (response timeout / retry delay values) |
| **Traceability matrix completeness** | Appendix A omits 13 requirements and ≥3 test-to-requirement links |

### 5.3 Overall Assessment

**Moderate confidence.** The specification set has strong structural coverage at the requirement level (95%+ forward traceability in both directions, 100% backward traceability). However, seven acceptance-criteria–level gaps create false coverage (D7), and one active design-requirements conflict (D6) would cause the system to be non-functional on reference hardware. The three untested Must requirements (ND-0607, ND-1011, ND-1012) need test cases before validation can be considered complete. The traceability matrix's omissions undermine its value as a single source of truth.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Update design §4.2, §4.3 timeout/retry values to match ND-0700–0702 | S | High — current values cause hardware non-functionality |
| 2 | F-009 | Add `initial_data` to design `MapDef` struct, update §9.2 allocation | S | High — blocks correct implementation |
| 3 | F-008 | Add 4 test cases for ND-0607 (initial map data) | M | High — Must requirement with zero coverage |
| 4 | F-002 | Add 2 test cases for ND-0500 AC4 (size caps) | S | Medium — enforcement boundary |
| 5 | F-003 | Add 1 test case for ND-0203 AC4 (1-second minimum) | S | Medium — battery safety |
| 6 | F-004 | Add 1 test case for ND-0503 AC4 (ephemeral map rejection) | S | Medium — storage corruption risk |
| 7 | F-006 | Add 1 test case for ND-0915 AC3 (negative self-healing path) | S | Medium — unrecoverable state risk |
| 8 | F-007 | Add 1 test case for ND-0910 AC3 (malformed payload) | S | Medium — infinite retry risk |
| 9 | F-005 | Add 2 test cases for ND-1009 AC4–AC5 (failure logging) | S | Low — diagnostics |
| 10 | F-012 | Update design §11.1 with 1-second minimum clamp | S | Low — design completeness |
| 11 | F-013 | Update design §7.4, §8.3 with load-time map rejection | S | Low — design completeness |
| 12 | F-014 | Update design §4.3 with explicit size validation step | S | Low — design completeness |
| 13 | F-010 | Add test case for ND-1011 (chunk transfer logging) | S | Low — diagnostics |
| 14 | F-011 | Add test case(s) for ND-1012 (build-type log levels) | M | Low — build configuration |
| 15 | F-015 | Extend Appendix A traceability matrix | S | Low — documentation |

## 7. Prevention

- **Acceptance criteria checklist:** When adding an acceptance criterion to an existing requirement, update the design section and add/update the linked test case in the same commit. Add a CI check or review-checklist item that flags AC additions without corresponding downstream updates.
- **Traceability matrix automation:** Generate the Appendix A traceability matrix from the test case "Validates:" headers rather than maintaining it manually. This eliminates the drift between the test body and the matrix.
- **Cross-document review gate:** Before merging any requirements change, require a review comment confirming that the design document and validation plan have been checked for impact. A simple grep for the affected REQ-ID across all three documents would catch most gaps.

## 8. Open Questions

1. **ND-0403 / ND-0403a design coverage:** These Should-priority requirements (secure boot, flash encryption) are acknowledged in the validation plan as platform tests but are not addressed in the design document. Are they intentionally left as ESP-IDF configuration concerns, or should the design document include a section on secure boot/flash encryption configuration? *Resolution: Clarify with the maintainers whether the design should document the ESP-IDF configuration steps for these features.*

2. **ND-1012 testability:** Build-type–aware log levels (compile-time stripping, feature mutual exclusivity) may require a different testing approach than the standard integration test harness (e.g., CI matrix builds with different feature flags, binary inspection). *Resolution: Define the test methodology before writing test cases.*

3. **Design timing values provenance:** The design's 50 ms / 100 ms values (F-001) may reflect an earlier transport assumption (direct ESP-NOW without modem bridge). If the design is updated, should it retain the old values as a note for non-modem-bridge deployments, or should the modem-bridge values be the only documented values? *Resolution: Clarify with the maintainers whether multiple transport profiles need to be documented.*

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-14 | Specification Audit (Copilot) | Initial audit — 15 findings across all 72 node requirements |
