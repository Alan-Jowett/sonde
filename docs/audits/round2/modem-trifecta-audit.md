<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Trifecta Audit — Investigation Report

## 1. Executive Summary

A cross-document traceability audit of the modem specification set (requirements, design, validation) covering all 39 active requirements (MD-0100 through MD-0505, excluding superseded MD-0406) found **8 findings**: 2 High, 4 Medium, and 2 Low. Two requirements (MD-0504, MD-0505) have **no test cases at all**, representing 9 untested acceptance criteria. The watchdog timeout value diverges across all three documents (requirements: 10 s, design: 35 s, validation: assumes 10 s). The serial codec dispatch table in the design omits all BLE inbound message types, creating an internal design inconsistency. Forward traceability to design is 100%; forward traceability to validation is 94.9% (37/39). Acceptance criteria coverage is 91.1% (112/123). Immediate action items are adding test cases for MD-0504 and MD-0505, and reconciling the watchdog timeout across all three documents.

## 2. Problem Statement

This audit evaluates the internal consistency and completeness of the modem specification triad:

- **Requirements:** `docs/modem-requirements.md` — 39 active requirements (MD-0100 – MD-0505)
- **Design:** `docs/modem-design.md` — architecture and internal design of the ESP32-S3 modem firmware
- **Validation:** `docs/modem-validation.md` — integration and system-level test plan

The goal is to identify specification drift — gaps, conflicts, and divergence — using the D1–D7 taxonomy before implementation proceeds further.

## 3. Investigation Scope

- **Documents examined:**
  - `docs/modem-requirements.md` (39 active requirements, 123 acceptance criteria)
  - `docs/modem-design.md` (15 sections including module architecture, BLE pairing relay, diagnostics)
  - `docs/modem-validation.md` (62 active test cases across 9 sections)
- **Time period:** Current document versions as of audit date
- **Tools used:** Manual cross-document traceability analysis using the D1–D7 specification-drift taxonomy
- **Limitations:**
  - Source code was not examined (D8–D13 reserved for code compliance audits).
  - The referenced upstream protocol documents (`modem-protocol.md`, `ble-pairing-protocol.md`) were not audited; consistency with those documents is out of scope.
  - MD-0406 is superseded by MD-0410/MD-0411 and was excluded from analysis.

### Artifact Inventory

**Requirements:** 39 active requirement IDs, 123 total acceptance criteria across 8 sections (USB-CDC, ESP-NOW, reliability/reset, non-requirements, BLE pairing relay, operational logging).

**Design:** 10 modules in the responsibility table (§3.1) with explicit requirement mappings. Additional coverage for MD-0104 in §10 and MD-0500–MD-0505 in §14.

**Validation:** 62 active test cases (T-0100 through T-0704, excluding removed/subsumed T-0610, T-0617, T-0618). Traceability matrix in Appendix A.

## 4. Findings

### Finding F-001: MD-0504 has no test case

- **Severity:** High
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:** Requirements: MD-0504 (modem-requirements.md §8); Validation: Appendix A test index (modem-validation.md, lines 967–1038)
- **Description:** MD-0504 (BLE pairing event logging) defines 3 acceptance criteria requiring INFO/WARN-level diagnostic UART output for BLE pairing events: (1) server-initiated LESC pairing trigger logged with connection handle, (2) authentication success logged at INFO, (3) authentication failure logged at WARN with failure reason. No test case in the validation plan references MD-0504. The test index in Appendix A has no entry for MD-0504.
- **Evidence:** Searching the validation document for "MD-0504" yields zero matches in the test index Validates column. The closest test, T-0703 (BLE lifecycle events logged), validates MD-0501 but does not cover pairing-specific events. T-0607b step 4 checks a buffered-write log message but is mapped to MD-0404/MD-0409, not MD-0504.
- **Root Cause:** The logging test section (§9, T-0700–T-0704) covers MD-0500 through MD-0503 but stops at MD-0503; MD-0504 was likely added after the test section was written and the corresponding test case was never created.
- **Impact:** BLE pairing event logging will not be verified. Operators debugging pairing failures in the field will have no assurance that the expected diagnostic output is present.
- **Remediation:** Add a test case (e.g., T-0705) that validates all 3 acceptance criteria of MD-0504: trigger `ble_gap_security_initiate` → assert INFO log with connection handle; complete authentication → assert INFO log; induce authentication failure → assert WARN log with reason.
- **Confidence:** High

---

### Finding F-002: MD-0505 has no test case

- **Severity:** High
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:** Requirements: MD-0505 (modem-requirements.md §8); Validation: Appendix A test index (modem-validation.md, lines 967–1038)
- **Description:** MD-0505 (Build-type–aware log levels) defines 6 acceptance criteria governing compile-time and runtime log level policies across 3 build variants (debug, release quiet, release verbose), plus a mutual-exclusivity `compile_error!` check. No test case in the validation plan references MD-0505. This is the most acceptance-criteria-dense requirement in the document (6 ACs) and it is entirely untested.
- **Evidence:** The string "MD-0505" does not appear in the validation document's test index. The design document addresses MD-0505 thoroughly in §14.2a, confirming the feature is specified at the design level. No validation coverage exists.
- **Root Cause:** MD-0505 requires build-system-level testing (compiling under different feature/profile combinations and asserting compile-time log level filtering), which does not fit the integration test harness model used by the validation plan. The gap appears structural rather than an oversight.
- **Impact:** Build-type log level policies will not be verified. A misconfigured `Cargo.toml` could ship release firmware with `info!` call-sites compiled in (wasting flash and CPU) or strip `warn!` messages needed for diagnostics, without detection.
- **Remediation:** Add test cases (e.g., T-0706a through T-0706c) that build the modem firmware under each variant and assert: (a) compile-time max level (check that `info!("probe")` is present/absent in binary), (b) runtime default level (check log output at boot), (c) `compile_error!` when both `quiet` and `verbose` features are enabled. These may be CI-only tests rather than hardware integration tests.
- **Confidence:** High

---

### Finding F-003: Watchdog timeout diverges across all three documents

- **Severity:** Medium
- **Category:** D6_CONSTRAINT_VIOLATION
- **Location:** Requirements: MD-0302 (modem-requirements.md §5); Design: §11 (modem-design.md, line 273); Validation: T-0304 note (modem-validation.md, line 315)
- **Description:** MD-0302 specifies a 10-second watchdog timeout. The design (§11) sets the effective timeout to 35 seconds via `CONFIG_ESP_TASK_WDT_TIMEOUT_S=35` in `crates/sonde-modem/sdkconfig.defaults`, which overrides the root `sdkconfig.defaults.esp32s3` value of 10. The validation test T-0304 assumes a 10-second timeout, with a maximum wait of 15 seconds ("The 10-second watchdog timeout plus reboot time should complete within 15 seconds"). With the design's 35-second timeout, T-0304 would time out and fail.
- **Evidence:**
  - Requirements: "10 second timeout" (MD-0302 description)
  - Design: "Timeout: 35 seconds (set via `CONFIG_ESP_TASK_WDT_TIMEOUT_S=35`)" (§11, line 273)
  - Validation: "The 10-second watchdog timeout plus reboot time should complete within 15 seconds" (T-0304 note, line 315)
- **Root Cause:** The design chose a longer timeout (35 s) to accommodate legitimate slow operations (e.g., channel scanning can block for several seconds) but did not propagate the change back to the requirements or validation documents.
- **Impact:** T-0304 would fail if implemented against the design. The three documents disagree on a concrete, testable value, making it unclear which is authoritative. MD-0302 is "Should" priority, which softens the constraint, but the explicit value of 10 seconds is still stated.
- **Remediation:** Reconcile the timeout value. If 35 s is correct, update MD-0302 and T-0304 to reflect 35 s (and adjust T-0304's max wait to ~40 s). If 10 s is correct, update the design's sdkconfig to use 10 and document why channel scanning does not cause false watchdog triggers.
- **Confidence:** High

---

### Finding F-004: HCI disconnect reason code cannot be provided as required

- **Severity:** Medium
- **Category:** D6_CONSTRAINT_VIOLATION
- **Location:** Requirements: MD-0411 (modem-requirements.md §7); Design: §15.4 / D10-4 (modem-design.md, lines 456–458)
- **Description:** MD-0411 requires `BLE_DISCONNECTED` to contain "the peer BLE address and HCI disconnect reason code." The design (§15.4, note D10-4) states that NimBLE's Rust binding does not expose the raw HCI reason code. The modem maps `Ok(())` → `0x16` and `Err(_)` → `0x13` as fixed approximations. The requirement implies the actual HCI reason code; the design can only provide a best-effort default.
- **Evidence:**
  - Requirements: "HCI disconnect reason code" (MD-0411 description and AC2)
  - Design: "the exact HCI reason code reported in `BLE_DISCONNECTED` may not match the actual reason" (D10-4, line 458)
  - Validation: T-0615 step 3 asserts "peer address and reason code" without verifying accuracy, so the test passes with approximated codes
- **Root Cause:** NimBLE Rust binding limitation (`BLEError` does not expose a public accessor for the raw HCI reason code).
- **Impact:** The gateway receives an inaccurate HCI reason code for most disconnections. This limits the gateway's ability to distinguish between remote user disconnection, link loss, and other BLE disconnect causes, potentially complicating pairing failure diagnostics.
- **Remediation:** Update MD-0411 to acknowledge the NimBLE limitation and specify the two-value approximation as acceptable behavior, or gate the requirement on a future NimBLE version that exposes raw reason codes. Consider adding a design note to T-0615 explaining the approximation.
- **Confidence:** High

---

### Finding F-005: T-0636 does not verify advertising resumption after idle timeout

- **Severity:** Medium
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:** Requirements: MD-0415 AC3 (modem-requirements.md §7); Validation: T-0636 (modem-validation.md §8, lines 896–903)
- **Description:** MD-0415 defines 3 acceptance criteria. AC3 states: "If BLE is still enabled, advertising resumes after the idle disconnect." T-0636 validates the 60-second idle timeout disconnect (AC1) and `BLE_DISCONNECTED` notification (AC2) but does not include an assertion that advertising resumes after the idle-timeout disconnect. The traceability matrix shows T-0636 → MD-0415, suggesting full coverage, but AC3 is not exercised.
- **Evidence:**
  - MD-0415 AC3: "If BLE is still enabled, advertising resumes after the idle disconnect."
  - T-0636 procedure (4 steps): step 3 asserts disconnect, step 4 asserts `BLE_DISCONNECTED`. No step asserts advertising resumption.
  - Compare T-0600 (Gateway Pairing Service lifecycle), which does assert post-disconnect advertising resumption (steps 7–9) for the general disconnect case.
- **Root Cause:** T-0636 was written to focus on the idle-timeout path and omitted the advertising-resumption assertion that T-0600 covers for the normal disconnect path.
- **Impact:** Advertising resumption after idle-timeout disconnect is untested. If the idle-timeout code path has a different disconnect flow than normal disconnection, advertising might not resume, blocking subsequent pairing attempts.
- **Remediation:** Add a step 5 to T-0636: "Scan for BLE advertisements. Assert: Gateway Pairing Service UUID is advertised (advertising resumed after idle disconnect)."
- **Confidence:** High

---

### Finding F-006: Serial codec dispatch table omits BLE inbound message types

- **Severity:** Medium
- **Category:** D5_ASSUMPTION_DRIFT
- **Location:** Design: §5.2 dispatch table (modem-design.md, lines 111–119); Design: §15.6, §15.7 (modem-design.md, lines 469–498)
- **Description:** The serial codec inbound dispatch table (§5.2) lists 5 gateway→modem message types: `RESET` (0x01), `SEND_FRAME` (0x02), `SET_CHANNEL` (0x03), `GET_STATUS` (0x04), `SCAN_CHANNELS` (0x05), plus "Unknown → silently discard." However, the BLE sections of the same design document (§15) describe at least 4 additional inbound message types required by requirements: `BLE_INDICATE` (0x20, per MD-0408), `BLE_ENABLE` (per MD-0413), `BLE_DISABLE` (per MD-0413), and `BLE_PAIRING_CONFIRM_REPLY` (per MD-0414). Taken literally, §5.2's dispatch would silently discard all BLE messages as unknown types.
- **Evidence:**
  - §5.2 table lists types 0x01–0x05 only (lines 111–119)
  - §15.6 references `BLE_INDICATE` (0x20) as an inbound type (line 474)
  - §15.7 references `BLE_ENABLE` and `BLE_DISABLE` as inbound types (lines 483–484)
  - §15.2 step 4 references `BLE_PAIRING_CONFIRM_REPLY` as inbound (line 427)
- **Root Cause:** The dispatch table in §5.2 was written for the initial ESP-NOW-only modem and was not updated when BLE message types were added in §15.
- **Impact:** An implementer reading §5.2 as the authoritative dispatch specification would not implement BLE message handling, breaking all BLE functionality. The conflict is resolvable by reading §15, but the internal inconsistency creates ambiguity.
- **Remediation:** Update the §5.2 dispatch table to include all gateway→modem message types: `BLE_INDICATE` (0x20), `BLE_ENABLE`, `BLE_DISABLE`, `BLE_PAIRING_CONFIRM_REPLY`, and any other inbound BLE types.
- **Confidence:** High

---

### Finding F-007: T-0704 does not verify buffered or flushed GATT write logging

- **Severity:** Low
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:** Requirements: MD-0502 ACs 2–3 (modem-requirements.md §8); Validation: T-0704 (modem-validation.md §9, lines 955–963)
- **Description:** MD-0502 defines 3 acceptance criteria for BLE GATT write logging: (AC1) authenticated write logged at INFO with payload length, (AC2) buffered write logged at INFO with payload length and authentication state, (AC3) flushed write logged at INFO with payload length. T-0704 only covers AC1 — it connects an authenticated client and writes 20 bytes. ACs 2 and 3 require testing the pre-authentication buffering and post-authentication flush scenarios. T-0607b step 4 partially covers AC2 (asserting a log message about buffered writes) but is mapped to MD-0404/MD-0409, not MD-0502. AC3 (flush logging) has no explicit log assertion in any test case.
- **Evidence:**
  - T-0704 procedure: "1. Connect and authenticate a BLE client. 2. Write 20 bytes via GATT. 3. Assert: INFO log indicating GATT write with payload length 20." — covers AC1 only.
  - MD-0502 AC2: "When a GATT write is buffered (awaiting authentication), the modem logs at INFO level: payload length and authentication state."
  - MD-0502 AC3: "When a buffered GATT write is flushed after authentication, the modem logs at INFO level: payload length."
- **Root Cause:** T-0704 tests the happy path (authenticated write) but does not exercise the pre-auth buffering scenario that triggers AC2 and AC3.
- **Impact:** Log output for the buffering/flushing code path is not verified. Missing log messages in this path would hinder debugging of pre-auth GATT write issues in the field.
- **Remediation:** Extend T-0704 (or add T-0704a) to: connect without completing pairing, write to characteristic (triggers buffering → assert INFO log with auth state), complete pairing (triggers flush → assert INFO log with payload length).
- **Confidence:** High

---

### Finding F-008: Design module responsibility table omits 7 requirements

- **Severity:** Low
- **Category:** D5_ASSUMPTION_DRIFT
- **Location:** Design: §3.1 module table (modem-design.md, lines 49–62)
- **Description:** The module responsibility table in §3.1 maps 10 modules to their covered requirements. However, 7 requirements are absent from this table: MD-0104 (Ready notification timing), MD-0500 (ESP-NOW frame logging), MD-0501 (BLE lifecycle logging), MD-0502 (BLE GATT write logging), MD-0503 (USB-CDC message logging), MD-0504 (BLE pairing event logging), and MD-0505 (Build-type–aware log levels). These requirements are addressed elsewhere in the design: MD-0104 in §10, MD-0500–MD-0504 in §14.3, and MD-0505 in §14.2a. The module table is structurally incomplete as a traceability artifact.
- **Evidence:**
  - §3.1 table lists 32 unique requirement IDs across 10 modules (lines 51–62)
  - MD-0104 is not mentioned in any module row; it is addressed in §10 (line 264)
  - MD-0500–MD-0505 are not mentioned in any module row; they are addressed in §14.2a–§14.3 (lines 337–377)
- **Root Cause:** The module table was likely written before the logging requirements (§8) and build-type policy (MD-0505) were added to the requirements document. MD-0104 may have been considered part of reset behavior (§10) rather than a module-level responsibility.
- **Impact:** Low. An auditor relying solely on §3.1 for forward traceability would incorrectly conclude these 7 requirements are unaddressed in the design. The requirements are in fact addressed, just not indexed in the table.
- **Remediation:** Add a "Diagnostics / Logging" row to the §3.1 table covering MD-0500–MD-0505. Add MD-0104 to the Bridge logic or USB-CDC driver row, or add a cross-cutting "Boot / Reset" row.
- **Confidence:** High

---

## 5. Coverage Metrics

### Forward Traceability

| Direction | Traced | Total | Rate |
|-----------|--------|-------|------|
| Requirements → Design | 39 | 39 | **100%** |
| Requirements → Validation | 37 | 39 | **94.9%** |

Untraced to validation: MD-0504, MD-0505.

### Backward Traceability

| Direction | Traced | Total | Rate |
|-----------|--------|-------|------|
| Design modules → Requirements | 32 | 32 | 100% (table only; 7 reqs addressed outside table) |
| Test cases → Requirements | 62 | 62 | **100%** (no orphaned test cases) |

### Acceptance Criteria Coverage

| Category | Count |
|----------|-------|
| Total acceptance criteria | 123 |
| Verified by linked test cases | 112 |
| Untested (MD-0504: 3, MD-0505: 6) | 9 |
| Partially untested (MD-0415 AC3, MD-0502 AC3) | 2 |
| **Coverage rate** | **91.1%** |

### Assumption Consistency

| Status | Count | Details |
|--------|-------|---------|
| Aligned | 37 | Requirements, design, and validation agree |
| Conflicting | 2 | Watchdog timeout (F-003), HCI reason code (F-004) |
| Design-only notes | 5 | D9-1 through D9-6, D10-4, D10-5 — implementation details documented in design with no requirement or test impact |

### Findings by Drift Category

| Category | Count | Finding IDs |
|----------|-------|-------------|
| D2_UNTESTED_REQUIREMENT | 2 | F-001, F-002 |
| D5_ASSUMPTION_DRIFT | 2 | F-006, F-008 |
| D6_CONSTRAINT_VIOLATION | 2 | F-003, F-004 |
| D7_ACCEPTANCE_CRITERIA_MISMATCH | 2 | F-005, F-007 |
| D1, D3, D4 | 0 | — |

### Overall Assessment

**Moderate confidence** — the specification set is well-aligned for ESP-NOW and USB-CDC functionality (sections 3–6 of the requirements). BLE pairing relay coverage is thorough. The two untested requirements (MD-0504, MD-0505) and the watchdog timeout three-way inconsistency are the most significant gaps. No orphaned design decisions or orphaned test cases were found. No requirements are untraced to the design. The specification set is in good shape for implementation with the remediations below.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Add T-0705 for MD-0504: test BLE pairing event logging (LESC trigger, auth success, auth failure) | S | Low |
| 2 | F-002 | Add T-0706a–c for MD-0505: test build-type log levels across debug/quiet/verbose variants; may require CI-only tests | M | Low |
| 3 | F-003 | Reconcile watchdog timeout: pick 10 s or 35 s, update all three documents and sdkconfig to match | S | Medium — changing timeout may cause false watchdog resets (if lowered) or delayed recovery (if raised) |
| 4 | F-004 | Update MD-0411 to document NimBLE reason-code approximation as accepted limitation; add note to T-0615 | S | Low |
| 5 | F-005 | Add step 5 to T-0636: assert advertising resumes after idle-timeout disconnect | S | Low |
| 6 | F-006 | Update §5.2 dispatch table to include BLE_INDICATE, BLE_ENABLE, BLE_DISABLE, BLE_PAIRING_CONFIRM_REPLY | S | Low |
| 7 | F-007 | Extend T-0704 or add T-0704a for pre-auth buffered write and post-auth flush log assertions | S | Low |
| 8 | F-008 | Add Diagnostics/Logging row to §3.1 module table covering MD-0104, MD-0500–MD-0505 | S | Low |

## 7. Prevention

- **Checklist item for requirement additions:** When adding a new requirement ID, verify that (1) a corresponding test case is created in the validation plan, (2) the requirement appears in the design module table, and (3) the validation Appendix A index is updated.
- **Dispatch table as source of truth:** Treat the serial codec dispatch table (§5.2) as the canonical list of inbound message types. Any new inbound type added elsewhere in the design must also be added to this table.
- **Concrete values require cross-document grep:** When a requirement specifies a numeric value (timeout, size, count), grep all three documents for that value to ensure consistency before merging.

## 8. Open Questions

1. **Watchdog timeout authority:** Which value is correct — 10 s (requirements) or 35 s (design)? The design notes that the crate-specific sdkconfig overrides the root value, suggesting 35 s was a deliberate choice, but the rationale for 35 s over 10 s is not documented. The 35-second timeout would allow a stalled main loop to block the modem for 35 seconds before recovery, which is a long outage for a radio bridge. Resolve by determining the longest legitimate blocking operation in the main loop (e.g., `esp_wifi_scan_start()` in blocking mode) and setting the timeout to cover that plus margin.

2. **NimBLE reason code exposure:** Will a future NimBLE Rust binding version expose the raw HCI disconnect reason code? If so, MD-0411 should remain as-is and the approximation should be treated as temporary. If not, the requirement should be permanently amended.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-26 | Specification Analyst (Copilot) | Initial trifecta audit — 8 findings across D2/D5/D6/D7 |
