<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Node Specification Trifecta Audit — Investigation Report

> **Note:** This report is a pre-remediation snapshot. Several findings have been resolved by subsequent changes to the specification documents. See the PR that introduced this report for the resolution status of each finding.

## 1. Executive Summary

The node specification trifecta (requirements, design, validation) is well-structured and largely consistent, with **93.0% of requirements covered by concrete test cases** and **100% backward traceability** (no orphaned test references). However, the audit found **18 requirements not explicitly traced in the design document** (mostly BLE pairing/registration requirements ND-0905–ND-0918), **4 requirements lacking concrete test cases**, **1 numbering gap** (ND-0401), and **2 phantom test IDs** (T-N402, T-N403) that appear in Appendix B but have no corresponding test case definitions. Remediation is low-effort and primarily involves adding explicit ND-reference annotations to the design document's BLE pairing section and writing deferred test case definitions for ND-0403/ND-0403a.

## 2. Problem Statement

This audit examines the internal consistency and cross-document traceability of the three node firmware specification documents: `node-requirements.md` (57 requirements), `node-design.md` (16 sections), and `node-validation.md` (78 test cases). The goal is to identify gaps, conflicts, and divergences that could lead to implementation defects or untested code paths.

## 3. Investigation Scope

- **Documents examined:**
  - `docs/node-requirements.md` — 57 requirements (ND-0100 through ND-0918), 1032 lines
  - `docs/node-design.md` — 16 sections covering architecture through BLE pairing, 597 lines
  - `docs/node-validation.md` — 78 test cases (T-N100 through T-N918), 1158 lines
- **Tools used:** Regex extraction of requirement IDs (ND-XXXX), test case IDs (T-NNNN), Validates: field parsing, range pattern expansion (e.g., `ND-0200–0203`), cross-reference matrix
- **Limitations:**
  - The design doc uses range patterns (e.g., `ND-0200–0203`) in module tables; these were expanded programmatically but are less auditable than explicit per-requirement references.
  - "Should"-priority requirements (ND-0403, ND-0403a) are intentionally deferred with italicized deferral notes; these are flagged but are not defects.
  - The audit does not verify whether test *implementations* (Appendix B) match test *specifications* (main body); it only checks specification-level traceability.

## 4. Findings

### F-001 — Design doc missing explicit traces for 18 requirements

**Severity:** Medium  
**Category:** D1_UNTRACED_REQUIREMENT  
**Location:** `node-design.md` §3.1 module table, §15 BLE pairing mode  
**Description:** 18 of 57 requirements have no explicit ND-reference in the design document, even after expanding range patterns. The untraced requirements are:

| ID | Title | Notes |
|---|---|---|
| ND-0403 | Secure boot support | "Should" priority; no design section |
| ND-0403a | Flash encryption support | "Should" priority; no design section |
| ND-0800 | Malformed CBOR handling | Behavior described in §12 error table but no ND-reference |
| ND-0801 | Unexpected message type handling | Same as above |
| ND-0802 | Chunk index validation | Same as above |
| ND-0905 | NODE_PROVISION handling | Partially covered by §15.5–15.6 but without ND-tag |
| ND-0906 | NODE_PROVISION NVS persistence | Same — design §15.6 discusses handler but lacks ND-ref |
| ND-0908 | NODE_PROVISION NVS write failure | Not mentioned in design |
| ND-0909 | PEER_REQUEST frame construction | Entirely absent from design doc |
| ND-0910 | PEER_REQUEST retransmission | Entirely absent from design doc |
| ND-0911 | PEER_ACK listen timeout | Entirely absent from design doc |
| ND-0912 | PEER_ACK verification | Entirely absent from design doc |
| ND-0913 | Registration completion | Entirely absent from design doc |
| ND-0914 | Deferred payload erasure | Entirely absent from design doc |
| ND-0915 | Self-healing on WAKE failure | Entirely absent from design doc |
| ND-0916 | NVS layout for BLE pairing artifacts | Entirely absent from design doc |
| ND-0917 | Factory reset via BLE | Partially covered by §6.2 factory reset but without ND-tag |
| ND-0918 | Main task stack size | Not mentioned in design doc |

**Evidence:**
- Design §3.1 module table covers: Transport (ND-0100, ND-0103), Protocol Codec (ND-0101, ND-0102), Wake Cycle Engine (ND-0200–0203, ND-0700–0702), Key Store (ND-0400–0402), Program Store (ND-0500–0503, ND-0501a), BPF Runtime (ND-0504–0506, ND-0600–0606), Map Storage (ND-0603, ND-0606), HAL (ND-0601), Sleep Manager (ND-0203), Auth (ND-0300–0304).
- The Key Store range `ND-0400–0402` expands to ND-0400, ND-0401, ND-0402 — but ND-0401 does not exist in the requirements doc (see F-004).
- Design §15 "BLE pairing mode" references ND-0900, ND-0901, ND-0902, ND-0903, ND-0904, ND-0907 inline but omits ND-0905, ND-0906, ND-0908–ND-0918.
- Design §12 "Error handling" covers behaviors for ND-0800/0801/0802 in its error table but never mentions the ND-IDs.

**Root Cause:** The design doc was written before the BLE pairing requirements (ND-09xx) were fully elaborated; §15 covers the NimBLE/GATT design but stops short of the PEER_REQUEST/PEER_ACK registration sub-protocol and the NVS layout. The error handling section (§12) describes behaviors without linking to the requirement IDs.

**Impact:** Implementers working from the design doc alone would miss the PEER_REQUEST/PEER_ACK registration flow, NVS layout requirements, self-healing logic, and main task stack sizing.

**Remediation:**
1. Add a `§15.7 PEER_REQUEST / Registration` section to the design doc covering ND-0909–ND-0916.
2. Add ND-references to the §12 error handling table (ND-0800, ND-0801, ND-0802).
3. Add a `§15.8 BLE provisioning handler` section with explicit ND-0905, ND-0906, ND-0908 references.
4. Add a note on ND-0918 (main task stack size) in §2 or §14.
5. Add a note in §6 or a new section for ND-0403/ND-0403a (secure boot / flash encryption) even if they are "Should" priority.

**Confidence:** High

---

### F-002 — Four requirements lack concrete test cases

**Severity:** Low–Medium  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** `node-validation.md` Appendix A  
**Description:** Four requirements have no concrete test case (T-NNNN) assigned. Two of these have explicit deferral notes (italicized text) which is acceptable for "Should"-priority requirements; two do not.

| ID | Priority | Appendix A entry | Status |
|---|---|---|---|
| ND-0403 | Should | `*(verified by secure boot platform tests)*` | Deferred — acceptable |
| ND-0403a | Should | `*(verified by flash encryption platform tests)*` | Deferred — acceptable |
| ND-0600 | Must | `*(validated by automated helper ABI conformance test...)*` | Deferred — but "Must" priority |
| ND-0918 | Must | `*(verified by sdkconfig.defaults setting)*` | Deferred — but "Must" priority |

**Evidence:**
- `node-validation.md` Appendix A, lines 1021–1022 (ND-0403, ND-0403a): italicized deferral text.
- `node-validation.md` Appendix A, line 1031 (ND-0600): deferred to "automated helper ABI conformance test" — no T-NNNN defined.
- `node-validation.md` Appendix A, line 1062 (ND-0918): deferred to sdkconfig check — no T-NNNN defined.

**Root Cause:** ND-0403/ND-0403a are platform-level concerns (ESP-IDF secure boot/flash encryption) appropriately deferred. ND-0600 describes a cross-version ABI stability check that doesn't fit the single-wake-cycle test model. ND-0918 is a build configuration check rather than a runtime behavior.

**Impact:** ND-0600 (helper API stability) is a "Must" requirement with no formal test specification — the deferral note references a test that doesn't exist as a T-NNNN case. An ABI regression could go undetected. ND-0918 is similarly a "Must" requirement verified only by inspection.

**Remediation:**
1. Define a `T-N6xx` test case for ND-0600 that asserts helper IDs and signatures match the spec (even if the test itself is described as automated/generated).
2. Define a `T-N9xx` test case for ND-0918 that asserts sdkconfig.defaults contains the required setting.

**Confidence:** High

---

### F-003 — Phantom test IDs T-N402 and T-N403 in Appendix B

**Severity:** Low  
**Category:** D3_ORPHANED_DESIGN_DECISION  
**Location:** `node-validation.md` Appendix B, lines 1099–1100  
**Description:** Appendix B ("Test ID to test function traceability") maps T-N402 and T-N403 to implemented test functions, but neither T-N402 nor T-N403 has a corresponding `### T-N4xx` section in the main body of the validation document. These IDs appear to be implementation artifacts rather than specified test cases.

**Evidence:**
- Line 1099: `| T-N402 | \`t_e2e_064_onboarding_to_wake\` | e2e_tests.rs |`
- Line 1100: `| T-N403 | \`t_n905_same_session_reprovision\` | ble_pairing.rs |`
- No `### T-N402` or `### T-N403` heading exists anywhere in the validation document's main body (sections 3–11).
- These IDs fall in the "Key storage and provisioning" test series (T-N4xx), which jumps from T-N401 to T-N404, creating gap numbers that happen to match.

**Root Cause:** Test implementations were added to Appendix B to track implemented tests that map to requirement areas but were never formalized with full test case specifications in the main body. Alternatively, the test functions were created for requirements ND-0402 (factory reset via onboarding) and ND-0905 (re-provision) but placed under improvised IDs.

**Impact:** Low — the tests exist in code and are mapped to requirement coverage. But the spec-level traceability is incomplete: someone reading only the main body would not know T-N402 and T-N403 exist.

**Remediation:** Either add `### T-N402` and `### T-N403` sections to the validation document's main body, or renumber the Appendix B entries to use existing test IDs if they are duplicates.

**Confidence:** High

---

### F-004 — Numbering gap: ND-0401 missing from requirements

**Severity:** Low  
**Category:** D5_ASSUMPTION_DRIFT  
**Location:** `node-requirements.md` §6 (Key storage and provisioning)  
**Description:** The requirement numbering jumps from ND-0400 (PSK storage) to ND-0402 (Factory reset), skipping ND-0401. However, the design document's module table (§3.1, line 73) lists Key Store as covering `ND-0400–0402`, which expands to include the non-existent ND-0401.

**Evidence:**
- `node-requirements.md`: Headings in §6 are `### ND-0400`, `### ND-0402`, `### ND-0403`, `### ND-0403a` (lines 265, 282, 298, 313).
- `node-design.md` line 73: `| **Key Store** | PSK storage ... | ND-0400–0402 |` — range includes ND-0401.
- No requirement titled ND-0401 exists in the requirements document or the Appendix A index (lines 989–990 jump from ND-0400 to ND-0402).

**Root Cause:** ND-0401 was likely a "BLE provisioning" or "PSK write" requirement that was either removed, merged into ND-0400, or split into the ND-09xx BLE pairing series during a requirements refactoring. The design document's range pattern was not updated.

**Impact:** The design doc claims to cover a requirement that does not exist. This is a minor documentation inconsistency but could confuse auditors or implementers searching for ND-0401.

**Remediation:** Either (a) update the design doc's Key Store row to `ND-0400, ND-0402` (explicit list instead of range), or (b) add a note in the requirements doc explaining the gap (e.g., "ND-0401 was merged into ND-09xx").

**Confidence:** High

---

### F-005 — Design doc lacks PEER_REQUEST/PEER_ACK registration sub-protocol

**Severity:** Medium  
**Category:** D1_UNTRACED_REQUIREMENT  
**Location:** `node-design.md` §15 (BLE pairing mode)  
**Description:** The design document's BLE pairing section (§15) covers NimBLE initialization, GATT service registration, advertising, and the NODE_PROVISION/NODE_ACK flow, but entirely omits the PEER_REQUEST/PEER_ACK registration sub-protocol. Requirements ND-0909 through ND-0915 describe a multi-boot registration handshake that is not mentioned anywhere in the design document.

**Evidence:**
- `node-design.md` §15.5 event flow (lines 547–555) ends at "phone disconnects → return → reboot (ND-0907)". There is no description of what happens on the *next* boot when PSK is stored but `reg_complete` is not set.
- The keywords `PEER_REQUEST`, `PEER_ACK`, `reg_complete`, and `peer_payload` do not appear anywhere in the design document.
- The design doc's §14 boot sequence (lines 490–503) describes: check magic → load PSK → enter wake cycle. It does not mention the three-way boot priority (BLE / PEER_REQUEST / WAKE) defined in ND-0900.

**Root Cause:** The PEER_REQUEST/PEER_ACK registration protocol was added to the requirements after the design doc was written. The design doc's boot sequence and BLE pairing sections were not updated to reflect the full three-state boot flow.

**Impact:** An implementer using only the design doc would build a firmware that goes directly from BLE provisioning to WAKE, skipping the PEER_REQUEST registration step entirely. This would break the pairing flow with the modem.

**Remediation:** Add §15.7 "Post-provisioning registration" and §14 boot sequence update covering the `reg_complete` flag check, PEER_REQUEST transmission, PEER_ACK verification, self-healing logic, and deferred payload erasure.

**Confidence:** High

---

### F-006 — Design doc §14 boot sequence does not reflect ND-0900 boot priority

**Severity:** Medium  
**Category:** D6_CONSTRAINT_VIOLATION  
**Location:** `node-design.md` §14 (Boot sequence), lines 490–503  
**Description:** The design doc's boot sequence describes a two-path flow: "No magic → unpaired → sleep indefinitely" vs. "Magic present → load PSK → enter wake cycle." This contradicts ND-0900, which defines a three-path boot priority: (1) no PSK or button held → BLE pairing, (2) PSK + no reg_complete → PEER_REQUEST, (3) PSK + reg_complete → normal WAKE.

**Evidence:**
- Design §14, step 2 (line 493–494): "No magic → unpaired. Log. Sleep indefinitely." — ND-0900 requires BLE pairing mode, not indefinite sleep.
- Design §14 has no step for `reg_complete` flag checking.
- ND-0900 (lines 676–687) explicitly requires three-way branching.

**Root Cause:** Design §14 predates the BLE pairing requirements. The "sleep indefinitely" behavior was the pre-BLE design; with BLE pairing, the node must enter BLE mode instead.

**Impact:** Direct contradiction — implementing §14 as written produces incorrect behavior (sleeping instead of entering BLE pairing mode when unpaired).

**Remediation:** Rewrite §14 to match the ND-0900 three-way boot priority: (1) BLE pairing, (2) PEER_REQUEST, (3) WAKE cycle. Reference ND-0900 explicitly.

**Confidence:** High

---

### F-007 — Design doc omits main task stack size (ND-0918)

**Severity:** Low  
**Category:** D1_UNTRACED_REQUIREMENT  
**Location:** `node-design.md` §2, §13, §14  
**Description:** ND-0918 requires `CONFIG_ESP_MAIN_TASK_STACK_SIZE=16384` (at least 16 KB). The design document does not mention this requirement or the stack size constraint. The design doc §15.1 mentions `CONFIG_BT_NIMBLE_HOST_TASK_STACK_SIZE=7000` but not the main task stack.

**Evidence:**
- `node-requirements.md` lines 956–969: ND-0918 specifies 16 KB main task stack.
- `node-design.md`: no mention of `CONFIG_ESP_MAIN_TASK_STACK_SIZE`, `16384`, or `16 KB` for the main task.
- Design §13 "Memory budget" (lines 478–487) lists RAM, RTC SRAM, flash, BPF stack, and ephemeral program sizes but omits FreeRTOS task stack size.

**Root Cause:** Stack size was discovered as a runtime constraint during BLE integration testing and added to requirements, but the design doc's memory budget section was not updated.

**Impact:** Low — the requirement is enforced via `sdkconfig.defaults` rather than code design, so the design doc gap doesn't directly cause implementation errors. But it could lead to stack overflows if someone modifies sdkconfig without awareness.

**Remediation:** Add `CONFIG_ESP_MAIN_TASK_STACK_SIZE` to the design doc §13 memory budget table or §2 technology choices.

**Confidence:** High

---

### F-008 — Design §14 "sleep indefinitely" contradicts ND-0900 BLE pairing

**Severity:** Medium  
**Category:** D6_CONSTRAINT_VIOLATION  
**Location:** `node-design.md` §14, line 494; `node-design.md` §4.1, lines 89–90  
**Description:** The design doc uses "sleep indefinitely" in two locations for the unpaired state, while ND-0900 requires entering BLE pairing mode. This is a duplicate observation related to F-006 but covers the §4.1 state machine as well.

**Evidence:**
- Design §4.1, line 89: "if no PSK → sleep indefinitely"
- Design §14, line 494: "No magic → unpaired. Log. Sleep indefinitely."
- ND-0900, line 680: "no PSK in NVS OR pairing button held ≥ 500 ms → enter BLE pairing mode"

Note: Design §15 (line 508) *does* say "When the node boots unpaired... the firmware enters BLE pairing mode." This contradicts §4.1 and §14 within the same document.

**Root Cause:** §4.1 and §14 were written before BLE pairing was added; §15 was added later but §4.1 and §14 were not reconciled.

**Impact:** Internal design-doc contradiction creates ambiguity for implementers.

**Remediation:** Update §4.1 state machine and §14 boot sequence to replace "sleep indefinitely" with "enter BLE pairing mode" for the unpaired state. Ensure consistency with §15.

**Confidence:** High

---

### F-009 — T-N305 sequence number test does not fully trace to acceptance criteria

**Severity:** Low  
**Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH  
**Location:** `node-validation.md` T-N305 (line 299); `node-requirements.md` ND-0303 (line 231)  
**Description:** ND-0303 acceptance criterion 3 states "No sequence state is persisted across deep sleep." T-N305 tests sequence increment correctness within a single wake cycle but does not verify cross-sleep isolation. No other test explicitly validates that sequence numbers are not persisted across deep sleep.

**Evidence:**
- ND-0303 AC3 (line 243): "No sequence state is persisted across deep sleep."
- T-N305 procedure (lines 304–306): Tests seq=1000, 1001, 1002 increment within one cycle. Does not test a second wake cycle to verify sequence numbers restart from a new `starting_seq`.

**Root Cause:** The test was designed for the primary acceptance criterion (incrementing) but not the boundary condition (cross-sleep isolation).

**Impact:** A firmware bug that persists sequence state across sleep could go undetected. This is partially mitigated by T-N201/T-N200 which exercise multiple wake cycles, but those tests don't explicitly assert sequence number independence.

**Remediation:** Add an assertion to T-N305 (or a new test) that runs two wake cycles with different `starting_seq` values and verifies the second cycle's first message uses the new `starting_seq`, not a continuation from the first cycle.

**Confidence:** Medium

---

### F-010 — Validation Appendix B test ID T-N402/T-N403 gap with main body

**Severity:** Low  
**Category:** D3_ORPHANED_DESIGN_DECISION  
**Location:** `node-validation.md` Appendix B, lines 1099–1100; main body §6  
**Description:** This is the same as F-003 but additionally notes that the test ID numbering gap in the main body (T-N401 → T-N404, skipping T-N402 and T-N403) is filled by Appendix B entries that have no specification.

**Evidence:**
- Main body §6 defines: T-N400, T-N401, T-N404. Gap at T-N402, T-N403.
- Appendix B assigns: T-N402 → `t_e2e_064_onboarding_to_wake`, T-N403 → `t_n905_same_session_reprovision`.

**Root Cause:** Same as F-003.  
**Impact:** Same as F-003.  
**Remediation:** Merged with F-003. Define `### T-N402` and `### T-N403` in the main body.

**Confidence:** High

---

### F-011 — Design §15 mentions NimBLE `AuthReq::all()` including MITM but ND-0904 says Just Works

**Severity:** Low  
**Category:** D5_ASSUMPTION_DRIFT  
**Location:** `node-design.md` §15.3, line 536; `node-requirements.md` ND-0904  
**Description:** Design §15.3 specifies `AuthReq::all()` which "requests SC + Bond + MITM", then says `NoInputNoOutput` "downgrades MITM to Just Works while keeping LESC." ND-0904 only requires "LESC Just Works pairing." The design's approach of requesting MITM then downgrading it is an implementation detail that is technically correct but could confuse readers into thinking MITM is required.

**Evidence:**
- Design §15.3 (line 536): "AuthReq::all() — requests SC (Secure Connections) + Bond + MITM."
- Design §15.3 (line 537): "SecurityIOCap::NoInputNoOutput — downgrades MITM to Just Works while keeping LESC."
- ND-0904 (line 741): "MUST accept BLE LESC Just Works pairing."

**Root Cause:** The design describes the ESP-IDF NimBLE API call sequence, which uses `AuthReq::all()` as a convenience. The actual BLE pairing outcome is Just Works due to I/O capability downgrade. This is not a conflict but could cause confusion.

**Impact:** Minimal — the behavior is correct. A reader might wonder whether MITM authentication is required.

**Remediation:** Add a clarifying comment in §15.3: "The effective pairing mode is LESC Just Works (ND-0904); `AuthReq::all()` requests the maximum security level, which is then constrained by the I/O capabilities."

**Confidence:** Medium

## 5. Root Cause Analysis

### Common root causes

1. **BLE requirements added after design doc was written (F-001, F-005, F-006, F-008):** The ND-09xx BLE pairing and registration requirements were elaborated after the initial design document. The design doc's §15 was partially updated for GATT/NimBLE but the PEER_REQUEST/PEER_ACK registration sub-protocol, NVS layout, and boot priority were not added.

2. **Design doc error handling section lacks ND-references (F-001 partial):** §12 describes all the right behaviors for ND-0800/0801/0802 but never cites the requirement IDs, making automated traceability impossible.

3. **Range patterns mask gaps (F-004):** The design doc's use of `ND-0400–0402` implicitly claims coverage of ND-0401, which does not exist. Explicit ID lists would have surfaced this gap earlier.

4. **Deferral notes used for "Must" requirements (F-002):** The validation doc uses italicized deferral text instead of concrete test IDs for two "Must"-priority requirements (ND-0600, ND-0918), creating a grey area in coverage.

### Coverage Metrics

| Metric | Value |
|---|---|
| **Requirements → Design (explicit trace)** | 22/57 = **38.6%** |
| **Requirements → Design (with range expansion)** | 39/57 = **68.4%** |
| **Requirements → Validation (concrete test cases)** | 53/57 = **93.0%** |
| **Requirements → Validation (incl. deferral notes)** | 57/57 = **100%** |
| **Backward traceability (tests → valid requirements)** | 78/78 = **100%** |
| **Orphaned test references (tests citing non-existent reqs)** | **0** |
| **Phantom test IDs (in Appendix B but no main-body spec)** | **2** (T-N402, T-N403) |
| **Requirement numbering gaps** | **1** (ND-0401) |
| **Test ID numbering gaps (main body)** | **2** (T-N402, T-N403) |

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|---|---|---|---|---|
| **P1** | F-006, F-008 | Update design §4.1 and §14 to replace "sleep indefinitely" with BLE pairing mode; align with ND-0900 | Small | Medium — current text directly contradicts requirements |
| **P1** | F-005 | Add §15.7 to design doc covering PEER_REQUEST/PEER_ACK registration sub-protocol (ND-0909–ND-0915) | Medium | High — missing design section for entire sub-protocol |
| **P2** | F-001 (partial) | Add ND-references to design §12 error table for ND-0800, ND-0801, ND-0802 | Small | Low |
| **P2** | F-001 (partial) | Add ND-0905, ND-0906, ND-0908 references to design §15.5–15.6 | Small | Low |
| **P2** | F-001 (partial) | Add ND-0916 (NVS layout) and ND-0917 (factory reset via BLE) to design §6 or §15 | Small | Low |
| **P2** | F-004 | Replace `ND-0400–0402` range with explicit `ND-0400, ND-0402` in design §3.1 | Trivial | Low |
| **P3** | F-002 | Define T-NNNN test case for ND-0600 (helper ABI stability check) | Small | Low |
| **P3** | F-002 | Define T-NNNN test case for ND-0918 (sdkconfig stack size check) | Trivial | Low |
| **P3** | F-003, F-010 | Add `### T-N402` and `### T-N403` test case specifications to validation doc main body | Small | Low |
| **P3** | F-007 | Add `CONFIG_ESP_MAIN_TASK_STACK_SIZE` to design §13 memory budget | Trivial | Low |
| **P4** | F-009 | Add cross-sleep sequence isolation assertion to T-N305 | Trivial | Low |
| **P4** | F-011 | Add clarifying comment in design §15.3 about MITM downgrade | Trivial | Low |

## 7. Prevention

### Process recommendations
1. **Explicit ID references over ranges:** Replace range patterns like `ND-0400–0402` with comma-separated explicit IDs. This prevents phantom coverage of non-existent IDs and makes automated traceability possible.
2. **Concurrent design updates:** When adding requirements to a new section (e.g., ND-09xx BLE pairing), require a corresponding design doc update in the same PR.
3. **Deferred test tracking:** Deferral notes in the validation doc should include a tracking issue number or a "TODO" marker that can be searched. "Must"-priority requirements should not use deferral notes without justification.

### Tooling recommendations
1. **Automated traceability matrix:** Add a CI check that extracts all ND-XXXX IDs from the three docs and verifies: (a) every requirement has at least one design reference, (b) every requirement has at least one T-NNNN test case, (c) every T-NNNN references only valid ND-XXXX IDs, (d) no numbering gaps exist.
2. **Appendix B reconciliation:** Add a check that every T-NNNN ID in Appendix B also has a corresponding `### T-NNNN` heading in the main body.

### Code review recommendations
1. When reviewing PRs that add requirements, verify that the design doc and validation doc are updated in the same PR or a linked follow-up PR.
2. When reviewing the design doc, verify that new sections include explicit ND-references (not just prose descriptions of the behavior).

## 8. Open Questions

1. **Was ND-0401 intentionally removed?** If so, the design doc range `ND-0400–0402` should be updated. If not, the requirement should be restored.
2. **Should T-N402 and T-N403 be formalized?** The implementations exist (e2e and ble_pairing tests), but the specifications are missing. Were they intentionally omitted as "covered by other tests" or accidentally skipped?
3. **Is ND-0600 (helper ABI stability) testable within the current test framework?** The deferral note describes a cross-version conformance test that may require a different testing approach than the per-wake-cycle model used by all other tests.
4. **Should the design doc §14 boot sequence be split into two versions** (with-BLE and without-BLE) given the `#[cfg(feature = "esp")]` conditional compilation of BLE code?

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-20 | Copilot (specification analyst) | Initial audit |
