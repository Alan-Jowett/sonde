<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->

# Maintenance Audit — 2026-03-30

## 1. Executive Summary

This audit applies the full D1–D13 drift detection taxonomy across all five
Sonde components: Gateway, Node, Protocol, Modem, and Hardware.

**Key metrics:**

| Component | REQ count | Req→Design | Req→Validation | Req→Code | Critical findings |
|-----------|-----------|-----------|----------------|----------|-------------------|
| Gateway   | 101       | 99%       | 83% (focus)    | 96%      | 7                 |
| Node      | 76        | 95%       | 93%            | 99%      | 5                 |
| Protocol  | 62 (TC)   | 100%      | 100%           | 100%     | 2                 |
| Modem     | 42        | 98%       | 93%            | 100%     | 3                 |
| Hardware  | 35        | 100%      | N/A            | 42%      | 8                 |

**Overall assessment:** Gateway and Node show strong implementation fidelity
but have **documentation lag** — recently added requirements (GW-1307,
ND-1015, ND-1016) are implemented in code but lack design sections and
validation test cases. Protocol and Modem are in good shape with spot-check
gaps in newer observability requirements. Hardware has the widest drift —
many `Must`-priority requirements for the PCB pipeline and contract system
remain unimplemented.

**Finding count by severity:**

| Severity | Count |
|----------|-------|
| Critical | 5     |
| High     | 13    |
| Medium   | 10    |
| Low      | 4     |
| **Total**| **32**|

---

## 2. Problem Statement

Periodic maintenance audit to detect specification drift across all
artifact layers — requirements, design, validation plans, source code,
and test code. This is Phase 1 of the maintain workflow: systematic
drift detection before human classification.

Focus areas specified by the operator:
- **Gateway**: handler config (GW-1401–1406), error observability (GW-1307),
  service logging (GW-1306), installer (GW-1500–1503)
- **Node**: boot visibility (ND-1015, ND-1016)
- **Modem & Protocol**: spot check
- **Hardware**: new component — full review

---

## 3. Investigation Scope

### Source documents consulted

| Document | Purpose |
|----------|---------|
| `docs/gateway-requirements.md` | Gateway REQ-IDs and acceptance criteria |
| `docs/gateway-design.md` | Gateway design traceability |
| `docs/gateway-validation.md` | Gateway test case definitions |
| `docs/node-requirements.md` | Node REQ-IDs and acceptance criteria |
| `docs/node-design.md` | Node design traceability |
| `docs/node-validation.md` | Node test case definitions |
| `docs/modem-requirements.md` | Modem REQ-IDs |
| `docs/modem-design.md` | Modem design traceability |
| `docs/modem-validation.md` | Modem test case definitions |
| `docs/protocol-crate-design.md` | Protocol design sections |
| `docs/protocol-crate-validation.md` | Protocol test case definitions |
| `docs/hw-requirements.md` | Hardware REQ-IDs |
| `docs/hw-design.md` | Hardware generation tool design |
| `docs/hw-schematic-design.md` | Hardware circuit design |

### Crates examined

| Crate | Source dir | Test locations |
|-------|-----------|----------------|
| `sonde-gateway` | `crates/sonde-gateway/src/` | `crates/sonde-gateway/tests/` |
| `sonde-node` | `crates/sonde-node/src/` | In-source `#[cfg(test)]` modules |
| `sonde-protocol` | `crates/sonde-protocol/src/` | `crates/sonde-protocol/tests/` |
| `sonde-modem` | `crates/sonde-modem/src/` | `crates/sonde-modem/src/` (in-source) |
| Hardware | `hw/` | N/A |

### Method

- `grep` with REQ-ID patterns to verify forward/backward traceability
- `glob` to inventory source and test files
- Targeted file reads for acceptance criteria cross-checking
- Spot-check sampling for Protocol and Modem (10–15 REQ-IDs each)

### Excluded

- **BLE Pairing** (`sonde-pair`, `sonde-pair-ui`): not in the operator's
  component map for this audit cycle.
- **BPF Interpreter** (`sonde-bpf`): not in scope.
- **E2E tests** (`sonde-e2e`): not in scope.
- Non-focus gateway requirements (GW-0100–GW-0808) received only automated
  traceability checks, not deep acceptance-criteria review.

---

## 4. Findings

### F-001 — GW-1307 not traced in design

- **Severity:** High
- **Category:** `D1_UNTRACED_REQUIREMENT`
- **Location:** `docs/gateway-requirements.md` (GW-1307) ↔ `docs/gateway-design.md`
- **Description:** GW-1307 (Error diagnostic observability) is a `Must`-priority
  requirement with 4 acceptance criteria. It has zero references in
  `gateway-design.md`. No design section describes how error diagnostic
  context is realized.
- **Evidence:** `grep -c "GW-1307" docs/gateway-design.md` → 0 matches.
- **Root cause:** Requirement was added after the design document was last
  updated. Code was written directly from the requirement.
- **Impact:** No design baseline for error observability architecture.
  Reviewers cannot trace implementation decisions to a design rationale.
- **Confidence:** High — verified by grep.
- **Remediation:** Add a design section to `gateway-design.md` describing
  the error context enrichment strategy.

### F-002 — GW-1306 has no validation test cases

- **Severity:** High
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/gateway-requirements.md` (GW-1306) ↔
  `docs/gateway-validation.md`
- **Description:** GW-1306 (Service logging) has 5 acceptance criteria.
  Only AC5 (graceful log file failure) has a test (`t1306_ac5` in code).
  AC1–AC4 (file path derivation, default filter level, ETW registration,
  runtime reload) have no test cases in the validation plan.
- **Evidence:** `grep -c "GW-1306" docs/gateway-validation.md` → 0 matches.
  Code test `t1306_ac5` exists in `tests/logging.rs` but covers only one
  of five acceptance criteria.
- **Root cause:** Validation plan was not updated when GW-1306 was added.
- **Impact:** 4 of 5 acceptance criteria are untested. Service logging
  behavior can regress silently.
- **Confidence:** High
- **Remediation:** Add T-1306a through T-1306d test cases to
  `gateway-validation.md` covering AC1–AC4. Implement corresponding tests.

### F-003 — GW-1307 has no validation test cases

- **Severity:** High
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/gateway-requirements.md` (GW-1307) ↔
  `docs/gateway-validation.md`
- **Description:** GW-1307 has no T-xxxx test case entries in the validation
  plan. Nine test functions (`t1307a`–`t1307i`) exist in
  `tests/error_observability.rs` but without validation plan backing.
- **Evidence:** `grep -c "GW-1307" docs/gateway-validation.md` → 0 matches.
  Nine test functions exist in code referencing GW-1307.
- **Root cause:** Tests were written directly from the requirement without
  updating the validation document.
- **Impact:** Error observability tests are orphaned — not tracked in the
  validation plan, not counted in coverage metrics.
- **Confidence:** High
- **Remediation:** Add T-1307a through T-1307i test case definitions to
  `gateway-validation.md` with GW-1307 traceability.

### F-004 — GW-1308 phantom requirement

- **Severity:** High
- **Category:** `D4_ORPHANED_TEST_CASE` / `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-gateway/tests/logging.rs:628` ↔
  `docs/gateway-requirements.md`
- **Description:** Test function `t1308_app_data_handler_pipeline_logging`
  references requirement GW-1308 with 5 acceptance criteria assertions.
  **GW-1308 does not exist** in `gateway-requirements.md`.
- **Evidence:** `grep -c "GW-1308" docs/gateway-requirements.md` → 0 matches.
  Test code at `tests/logging.rs:628–759` validates 5 ACs against a
  non-existent requirement.
- **Root cause:** Requirement ID was either never added to the requirements
  document, or was planned but not formalized before tests were written.
- **Impact:** Test coverage metric is inflated. Behavior is tested but has
  no specification baseline — it cannot be verified for correctness because
  the acceptance criteria exist only in the test code.
- **Confidence:** High — verified by grep.
- **Remediation:** Either add GW-1308 to `gateway-requirements.md` with the
  acceptance criteria reflected in the test code, or re-number the test to
  an existing requirement.

### F-005 — T-1403, T-1404 live-reload tests not implemented

- **Severity:** High
- **Category:** `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `docs/gateway-validation.md` (T-1403, T-1404) ↔
  `crates/sonde-gateway/tests/`
- **Description:** Validation plan specifies T-1403 (live reload — handler
  add) and T-1404 (live reload — handler remove) for GW-1404. No
  corresponding test functions exist in the test suite.
- **Evidence:** `grep "t1403\|t1404" crates/sonde-gateway/tests/` → 0 matches.
- **Root cause:** Live-reload testing may require a more complex test
  harness (running gateway, dynamic config change); deferred during
  implementation.
- **Impact:** Handler hot-reload behavior is untested. Regressions in
  add/remove-while-running paths will not be caught by CI.
- **Confidence:** High
- **Remediation:** Implement `t1403` and `t1404` test functions.

### F-006 — GW-1306 AC1–AC4 untested acceptance criteria

- **Severity:** High
- **Category:** `D12_UNTESTED_ACCEPTANCE_CRITERION`
- **Location:** `docs/gateway-requirements.md` (GW-1306 AC1–AC4) ↔
  `crates/sonde-gateway/tests/`
- **Description:** GW-1306 has 5 acceptance criteria. Only AC5 (graceful
  failure on log file error) has a test (`t1306_ac5`). Four criteria remain
  untested: file path derivation (AC1), default filter level (AC2), ETW
  registration (AC3), and runtime filter reload (AC4).
- **Evidence:** Only `t1306_ac5` exists in test code. No `t1306_ac1` through
  `t1306_ac4` found.
- **Root cause:** Partial test implementation.
- **Impact:** Service logging path derivation, filter levels, and ETW
  integration can regress without detection.
- **Confidence:** High
- **Remediation:** Implement tests for AC1–AC4.

### F-007 — GW-1308 assertion mismatch (phantom requirement)

- **Severity:** High
- **Category:** `D13_ASSERTION_MISMATCH`
- **Location:** `crates/sonde-gateway/tests/logging.rs:628` ↔
  `docs/gateway-validation.md`
- **Description:** Test `t1308_app_data_handler_pipeline_logging` asserts
  against 5 acceptance criteria (AC1–AC5) that belong to GW-1308, a
  requirement that does not exist. The assertions cannot be validated
  against any specification.
- **Evidence:** Test at line 628 of `logging.rs` references GW-1308 ACs.
  No GW-1308 in requirements doc. No T-1308 in validation plan (though
  the validation plan entry T-1308 exists, it references the phantom
  GW-1308).
- **Root cause:** Same as F-004.
- **Impact:** False coverage — the test passes but validates against
  no authoritative specification.
- **Confidence:** High
- **Remediation:** Resolve jointly with F-004.

### F-008 — T-1405a (invalid YAML bootstrap) not implemented

- **Severity:** Medium
- **Category:** `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `docs/gateway-validation.md` (T-1405a) ↔
  `crates/sonde-gateway/tests/`
- **Description:** Validation plan specifies T-1405a (bootstrap with invalid
  YAML returns error). No corresponding test function exists.
- **Evidence:** `grep "t1405a" crates/sonde-gateway/tests/` → 0 matches.
- **Root cause:** Negative test case deferred during implementation.
- **Impact:** Error path for invalid YAML config is untested.
- **Confidence:** High
- **Remediation:** Implement `t1405a` test function.

### F-009 — GW-1307 AC4 serial port errors partially unimplemented

- **Severity:** Medium
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/gateway-requirements.md` (GW-1307 AC4) ↔
  `crates/sonde-gateway/src/`
- **Description:** GW-1307 AC4 requires serial port errors to include port
  name and OS error code. Error observability tests (`t1307a`–`t1307i`)
  cover key file, SQLite, and config errors, but **no serial port error
  test exists** and no enriched serial error context was found in the modem
  transport code.
- **Evidence:** Tests cover `t1307c`–`t1307f` for key files and SQLite.
  No serial port error enrichment found in gateway source.
- **Root cause:** Serial port errors flow through the modem transport trait
  and may not surface with enriched context.
- **Impact:** Serial port errors may lack diagnostic context, making field
  debugging harder.
- **Confidence:** Medium — serial port errors may be enriched at a different
  layer (needs domain check).
- **Remediation:** Verify serial port error paths include port name and OS
  error. Add a test if missing.

### F-010 — ND-1015 not traced in design

- **Severity:** High
- **Category:** `D1_UNTRACED_REQUIREMENT`
- **Location:** `docs/node-requirements.md` (ND-1015) ↔ `docs/node-design.md`
- **Description:** ND-1015 (Boot version visibility) has no reference in
  `node-design.md`. The §17.2 log-point table lists all ND-10xx entries
  but omits ND-1015.
- **Evidence:** `grep -c "ND-1015" docs/node-design.md` → 0 matches.
  Code implementation exists at `bin/node.rs:50–51`.
- **Root cause:** Requirement added after design document's log table was
  written.
- **Impact:** Design document is stale for boot diagnostics.
- **Confidence:** High
- **Remediation:** Add ND-1015 row to `node-design.md` §17.2 table.

### F-011 — ND-1016 not traced in design

- **Severity:** High
- **Category:** `D1_UNTRACED_REQUIREMENT`
- **Location:** `docs/node-requirements.md` (ND-1016) ↔ `docs/node-design.md`
- **Description:** ND-1016 (ESP-NOW channel logging at boot) has no reference
  in `node-design.md`. Same gap as F-010.
- **Evidence:** `grep -c "ND-1016" docs/node-design.md` → 0 matches.
  Code implementation exists at `bin/node.rs:163`.
- **Root cause:** Same as F-010.
- **Impact:** Design document is stale.
- **Confidence:** High
- **Remediation:** Add ND-1016 row to `node-design.md` §17.2 table.

### F-012 — ND-1015, ND-1016 have no validation test cases

- **Severity:** High
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/node-requirements.md` (ND-1015, ND-1016) ↔
  `docs/node-validation.md`
- **Description:** Neither ND-1015 nor ND-1016 has a test case defined in
  `node-validation.md`. No T-N test IDs exist for these requirements.
  Both are also missing from the Appendix A traceability table.
- **Evidence:** `grep -c "ND-1015\|ND-1016" docs/node-validation.md` → 0.
- **Root cause:** Validation plan not updated when requirements were added.
- **Impact:** Boot visibility diagnostics are unverified by the validation
  plan.
- **Confidence:** High
- **Remediation:** Add T-N test cases for ND-1015 and ND-1016 to
  `node-validation.md`. Add both to the Appendix A traceability table.

### F-013 — ND-1011, ND-1014 not traced in design

- **Severity:** Medium
- **Category:** `D1_UNTRACED_REQUIREMENT`
- **Location:** `docs/node-requirements.md` (ND-1011, ND-1014) ↔
  `docs/node-design.md`
- **Description:** ND-1011 (Chunk transfer logging) and ND-1014 (Error
  diagnostic observability) have no references in the design document.
  ND-1011 is implemented in code with `(ND-1011)` markers in log strings
  at `wake_cycle.rs:839,856`. ND-1014 is a quality attribute applied
  across error paths.
- **Evidence:** 0 grep matches for each in design doc. Code markers exist.
- **Root cause:** Added after design document last updated.
- **Impact:** Design baseline incomplete for observability requirements.
- **Confidence:** High
- **Remediation:** Add design entries for ND-1011 and ND-1014.

### F-014 — ND-1011, ND-1012, ND-1014 have no validation test cases

- **Severity:** Medium
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/node-requirements.md` ↔ `docs/node-validation.md`
- **Description:** Three observability requirements have no test cases in
  the validation plan: ND-1011 (chunk transfer logging), ND-1012
  (build-type-aware log levels), ND-1014 (error diagnostic observability).
- **Evidence:** No T-N IDs found for these requirements in validation doc.
- **Root cause:** Observability requirements added without validation plan
  updates.
- **Impact:** Observability features can regress without CI detection.
- **Confidence:** High
- **Remediation:** Add T-N test cases for ND-1011, ND-1012, ND-1014.

### F-015 — 16 ND-10xx requirements missing from Appendix A traceability table

- **Severity:** Medium
- **Category:** `D7_ACCEPTANCE_CRITERIA_MISMATCH`
- **Location:** `docs/node-validation.md` Appendix A
- **Description:** All ND-10xx observability requirements (ND-1000 through
  ND-1016) and ND-0607 are missing from the Appendix A traceability
  matrix. While many have inline T-N test cases in the validation body,
  the summary table does not reference them.
- **Evidence:** Appendix A lists ND-0100 through ND-0918 but omits the
  entire ND-10xx block.
- **Root cause:** Appendix A was not updated when ND-10xx requirements
  were added.
- **Impact:** Traceability matrix gives a false picture of coverage. Anyone
  consulting only Appendix A will conclude these requirements are untested.
- **Confidence:** High
- **Remediation:** Add all ND-10xx entries (and ND-0607) to Appendix A.

### F-016 — MD-0506 complete traceability gap

- **Severity:** High
- **Category:** `D1_UNTRACED_REQUIREMENT` / `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/modem-requirements.md` (MD-0506) ↔
  `docs/modem-design.md` ↔ `docs/modem-validation.md`
- **Description:** MD-0506 (Error diagnostic observability) is a
  `Must`-priority requirement with no design section and no validation
  test case. Complete traceability gap across all downstream artifacts.
- **Evidence:** `grep -c "MD-0506" docs/modem-design.md` → 0.
  `grep -c "MD-0506" docs/modem-validation.md` → 0.
- **Root cause:** Requirement added to all three components (GW, ND, MD)
  but design and validation updated only for some.
- **Impact:** Error diagnostic observability for modem is completely
  untraced.
- **Confidence:** High
- **Remediation:** Add MD-0506 design section and validation test cases.

### F-017 — MD-0504, MD-0505 have no validation test cases

- **Severity:** Medium
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/modem-requirements.md` (MD-0504, MD-0505) ↔
  `docs/modem-validation.md`
- **Description:** MD-0504 (BLE pairing event logging) and MD-0505
  (Build-type-aware log levels) have design coverage but no validation
  test cases.
- **Evidence:** Both have design sections but 0 T-xxxx entries in
  validation doc.
- **Root cause:** Validation plan not updated for new logging requirements.
- **Impact:** Logging behavior untested.
- **Confidence:** High
- **Remediation:** Add T-xxxx test cases for MD-0504 and MD-0505.

### F-018 — Protocol §10 (modem serial codec) has no validation tests

- **Severity:** Medium
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/protocol-crate-design.md` §10 ↔
  `docs/protocol-crate-validation.md`
- **Description:** Design §10 describes the modem serial codec
  (`modem.rs`). Code exists. No T-P test cases cover this section in the
  validation plan.
- **Evidence:** No T-P IDs reference §10 or the modem codec in the
  validation plan.
- **Root cause:** Modem codec was added to the protocol crate after the
  validation plan was written.
- **Impact:** Modem serial codec can regress without CI detection.
- **Confidence:** High
- **Remediation:** Add T-P test cases for §10 modem serial codec.

### F-019 — 5 protocol test functions without validation plan entries

- **Severity:** Low
- **Category:** `D4_ORPHANED_TEST_CASE`
- **Location:** `crates/sonde-protocol/tests/validation.rs` ↔
  `docs/protocol-crate-validation.md`
- **Description:** Five test functions exist in code but have no
  corresponding T-P entry in the validation plan: `test_p067` (line 1835),
  `test_p068` (line 1887), `test_p069` (line 2059), `test_p072`
  (line 2475), `test_p090` (line 1953).
- **Evidence:** Tests exist at the cited lines. No T-P067, T-P068,
  T-P069, T-P072, or T-P090 in validation doc.
- **Root cause:** Tests added during implementation without updating
  validation plan.
- **Impact:** Low — tests exist and provide coverage, but are not tracked
  in the validation plan.
- **Confidence:** High
- **Remediation:** Add T-P067, T-P068, T-P069, T-P072, T-P090 to
  `protocol-crate-validation.md`.

### F-020 — Protocol §11 (BLE envelope) tests not in validation plan

- **Severity:** Low
- **Category:** `D4_ORPHANED_TEST_CASE`
- **Location:** `crates/sonde-protocol/src/ble_envelope.rs` ↔
  `docs/protocol-crate-validation.md`
- **Description:** Five inline tests exist for the BLE envelope codec but
  are not formalized in the validation plan.
- **Evidence:** 5 `#[test]` functions in `ble_envelope.rs`. No T-P IDs for
  BLE envelope in validation doc.
- **Root cause:** Same pattern as F-019.
- **Impact:** Low — inline tests exist.
- **Confidence:** High
- **Remediation:** Formalize in validation plan.

### F-021 — HW-0701 only 1 of 3 required configs exists

- **Severity:** Critical
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/hw-requirements.md` (HW-0701) ↔ `hw/configs/`
- **Description:** HW-0701 requires 3 example configurations: `minimal.yaml`,
  `soil-monitor.yaml`, `environmental.yaml`. Only `minimal-qwiic.yaml`
  exists, which is explicitly a supplement — not the canonical
  `minimal.yaml`.
- **Evidence:** `hw/configs/` contains only `minimal-qwiic.yaml`.
- **Root cause:** Tool development focused on the generation pipeline; the
  three canonical configs have not been authored yet.
- **Impact:** Users cannot generate any of the three required board
  variants. The tool's primary use case is blocked.
- **Confidence:** High
- **Remediation:** Create the 3 required config files.

### F-022 — HW-0801 PCB layout generation unimplemented

- **Severity:** Critical
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/hw-requirements.md` (HW-0801) ↔ `hw/sonde_hw/`
- **Description:** HW-0801 requires KiCad PCB layout output. No `.kicad_pcb`
  generation code exists. The tool generates schematics and BOMs only.
- **Evidence:** No PCB-related Python modules in `hw/sonde_hw/`. No
  `.kicad_pcb` files in `hw/output/`.
- **Root cause:** PCB layout generation is significantly more complex than
  schematic generation and has not been implemented yet.
- **Impact:** The pipeline stops at schematic → BOM. DRC (HW-1001) and
  Gerber export (HW-0802) are also blocked.
- **Confidence:** High
- **Remediation:** Implement PCB layout generation or document this as a
  known limitation with a roadmap.

### F-023 — HW-0802 Gerber/CPL generation unimplemented

- **Severity:** Critical
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/hw-requirements.md` (HW-0802) ↔ `hw/sonde_hw/`
- **Description:** HW-0802 requires Gerber, BOM, and CPL outputs. BOM is
  generated, but Gerber and CPL are not — they depend on PCB layout
  (HW-0801) which is also unimplemented.
- **Evidence:** `bom.py` exists. No Gerber or CPL generation code.
- **Root cause:** Blocked by F-022.
- **Impact:** Cannot produce manufacturing files.
- **Confidence:** High
- **Remediation:** Implement after HW-0801.

### F-024 — HW-1100–1104 contract system unimplemented

- **Severity:** Critical
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/hw-requirements.md` (HW-1100–HW-1104) ↔ `hw/`
- **Description:** The entire hardware contract system (5 `Must`-priority
  requirements) is unimplemented: no contract schema (`contract-schema.json`),
  no contract generation code, no power/IO contract output, no invariant
  checking.
- **Evidence:** No `contract` files or modules anywhere in `hw/`.
- **Root cause:** Contract system is a newer design addition that has not
  been built yet.
- **Impact:** No formal power budget or pin ownership verification.
  firmware/hardware integration issues will not be caught automatically.
- **Confidence:** High
- **Remediation:** Implement the contract system or defer with explicit
  documentation.

### F-025 — HW-1001 DRC impossible without PCB

- **Severity:** Critical
- **Category:** `D10_CONSTRAINT_VIOLATION_IN_CODE`
- **Location:** `docs/hw-requirements.md` (HW-1001) ↔ `hw/`
- **Description:** HW-1001 requires DRC with zero errors. Without PCB
  layout generation (F-022), DRC cannot run. No `.kicad_dru` rule files
  exist.
- **Evidence:** No DRC rules or PCB files anywhere in `hw/`.
- **Root cause:** Blocked by F-022.
- **Impact:** Physical board errors will not be caught pre-fabrication.
- **Confidence:** High
- **Remediation:** Implement after HW-0801.

### F-026 — No hw-validation.md exists

- **Severity:** Medium
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/` (expected: `hw-validation.md`)
- **Description:** Unlike all other components, the hardware component has
  no validation document. There are no formalized test procedures mapping
  HW-xxxx requirements to acceptance test cases.
- **Evidence:** No `hw-validation.md` file exists in `docs/`.
- **Root cause:** Hardware component is newer; validation plan has not
  been authored.
- **Impact:** All 35 HW requirements lack formal validation procedures.
- **Confidence:** High
- **Remediation:** Author `hw-validation.md` defining test cases for at
  least the `Must`-priority requirements.

### F-027 — SPICE subsystem undocumented in hw-design.md

- **Severity:** Medium
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `hw/sonde_hw/spice/` ↔ `docs/hw-design.md`
- **Description:** A complete SPICE simulation subsystem exists (5 models,
  3 test specifications, 4 Python modules) but is not mentioned in
  `hw-design.md`. HW-1003 (SPICE simulation) is addressed by this code
  but the design document doesn't describe the implementation approach.
- **Evidence:** `hw/sonde_hw/spice/` directory with `assertions.py`,
  `deck.py`, `netlist.py`, `runner.py`, models, and test specs.
  Zero references to SPICE in `hw-design.md`.
- **Root cause:** SPICE subsystem was built without updating the design doc.
- **Impact:** Undocumented implementation — maintainers cannot understand
  the design intent without reading the code.
- **Confidence:** High
- **Remediation:** Add a SPICE simulation section to `hw-design.md`.

### F-028 — hw-design.md tool entry point stale

- **Severity:** Low
- **Category:** `D5_ASSUMPTION_DRIFT`
- **Location:** `docs/hw-design.md` §2.3 ↔ `hw/sonde_hw/`
- **Description:** Design specifies `sonde-hw.py` as the tool entry point.
  Actual implementation uses a Python package (`hw/sonde_hw/`) with
  `__main__.py` and `cli.py`.
- **Evidence:** No `sonde-hw.py` file exists. `cli.py` is the actual entry
  point.
- **Root cause:** Implementation improved to a proper package structure
  without updating the design doc.
- **Impact:** Minor — design doc misleads about invocation.
- **Confidence:** High
- **Remediation:** Update `hw-design.md` §2.3 to reflect package structure.

### F-029 — hw-design.md directory structure stale

- **Severity:** Medium
- **Category:** `D5_ASSUMPTION_DRIFT`
- **Location:** `docs/hw-design.md` §2.3 ↔ `hw/`
- **Description:** Design specifies `templates/` (KiCad `.kicad_sch` files),
  `footprints/`, and `rules/` directories. None exist. Templates are
  Python modules in `sonde_hw/templates/`, not KiCad files.
- **Evidence:** No `templates/`, `footprints/`, or `rules/` directories
  at `hw/` level. Python template modules exist at `hw/sonde_hw/templates/`.
- **Root cause:** Implementation approach changed from KiCad template files
  to programmatic Python templates.
- **Impact:** Design document does not match actual architecture. New
  contributors will be confused.
- **Confidence:** High
- **Remediation:** Update `hw-design.md` §2.3 to reflect the Python
  template approach.

### F-030 — Gateway handler `working_dir` undocumented

- **Severity:** Low
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-gateway/src/handler.rs:325` ↔
  `docs/gateway-requirements.md` (GW-1401–GW-1406)
- **Description:** Handler configuration includes a `working_dir` field
  that is not mentioned in the GW-1401–GW-1406 requirements.
- **Evidence:** Field exists in code at `handler.rs:325`. Not in any
  GW-14xx requirement.
- **Root cause:** Implementation addition not reflected in requirements.
- **Impact:** Low — benign feature, but undocumented.
- **Confidence:** High
- **Remediation:** Add `working_dir` to GW-1401 or document as an
  implementation detail.

### F-031 — T-1304, T-1305a/b build metadata and verification tests missing

- **Severity:** Medium
- **Category:** `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `docs/gateway-validation.md` (T-1304, T-1305a, T-1305b) ↔
  `crates/sonde-gateway/tests/`
- **Description:** Three validation plan test cases have no implementing
  test functions: T-1304 (build metadata `--version`), T-1305a and T-1305b
  (verification diagnostics).
- **Evidence:** `grep "t1304\|t1305" crates/sonde-gateway/tests/` → 0.
- **Root cause:** Deferred during implementation.
- **Impact:** Build metadata and verification diagnostics are untested.
- **Confidence:** High
- **Remediation:** Implement the test functions.

### F-032 — HW-0902 no CI workflow for hardware

- **Severity:** Low
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/hw-requirements.md` (HW-0902) ↔ `.github/workflows/`
- **Description:** HW-0902 (`Should` priority) specifies CI integration via
  GitHub Actions for the hardware tool. No workflow exists.
- **Evidence:** No hardware-related workflow in `.github/workflows/`.
- **Root cause:** Lower priority (`Should`) — deferred.
- **Impact:** Hardware tool regressions won't be caught by CI.
- **Confidence:** High
- **Remediation:** Create a GH Actions workflow for `hw/` when the tool
  reaches sufficient maturity.

---

## 5. Root Cause Analysis

Three systemic patterns account for the majority of findings:

### Pattern 1: Documentation lag after code changes

**Findings:** F-001, F-002, F-003, F-010, F-011, F-012, F-013, F-014,
F-015, F-016, F-017

**Description:** Requirements are added to `*-requirements.md`, code is
implemented correctly, but the intermediate artifacts — design sections
and validation test case definitions — are not updated. This creates a
"specification sandwich" where the top (requirements) and bottom (code)
are aligned but the middle layers (design, validation plan) are stale.

**Root cause:** The spec-first development workflow is followed for the
initial requirement, but downstream artifact updates are not enforced as
part of the code review/merge process.

### Pattern 2: Phantom and orphaned identifiers

**Findings:** F-004, F-007, F-019, F-020

**Description:** Test code references requirement IDs or test case IDs
that don't exist in the authoritative documents (GW-1308 phantom,
orphaned T-P067/068/069/072/090). This creates false traceability.

**Root cause:** Tests written directly from developer intent without
cross-checking the requirements document. No automated validation that
REQ-IDs referenced in test comments actually exist.

### Pattern 3: Hardware pipeline incomplete

**Findings:** F-021, F-022, F-023, F-024, F-025, F-026, F-027, F-028,
F-029, F-032

**Description:** The hardware component has strong documentation
(requirements and design) but the implementation pipeline is only
partially built. The schematic generation works, but PCB layout, Gerber
export, and the contract system are unimplemented.

**Root cause:** Hardware tooling is a newer addition to the project.
The design documents describe the target architecture, but implementation
has only reached the schematic/BOM stage.

---

## 6. Remediation Plan

### Priority 1 — Critical (resolve first)

| Finding | Action | Component |
|---------|--------|-----------|
| F-004/F-007 | Add GW-1308 to `gateway-requirements.md` or re-assign test to existing REQ | Gateway |
| F-021 | Create 3 required config files (`minimal.yaml`, `soil-monitor.yaml`, `environmental.yaml`) | Hardware |
| F-022 | Implement PCB layout generation or document as roadmap item | Hardware |
| F-023 | Implement Gerber/CPL generation (blocked by F-022) | Hardware |
| F-024 | Implement contract system or defer with documentation | Hardware |
| F-025 | Implement DRC (blocked by F-022) | Hardware |

### Priority 2 — High (resolve in this cycle)

| Finding | Action | Component |
|---------|--------|-----------|
| F-001 | Add GW-1307 design section to `gateway-design.md` | Gateway |
| F-002/F-003 | Add T-1306x and T-1307x test cases to `gateway-validation.md` | Gateway |
| F-005 | Implement `t1403` and `t1404` live-reload tests | Gateway |
| F-006 | Implement `t1306_ac1` through `t1306_ac4` | Gateway |
| F-010/F-011 | Add ND-1015/ND-1016 to `node-design.md` §17.2 table | Node |
| F-012 | Add ND-1015/ND-1016 test cases to `node-validation.md` | Node |
| F-016 | Add MD-0506 design section and validation tests | Modem |

### Priority 3 — Medium (resolve in next cycle)

| Finding | Action | Component |
|---------|--------|-----------|
| F-008 | Implement `t1405a` | Gateway |
| F-009 | Verify serial port error paths | Gateway |
| F-013/F-014 | Add ND-1011/ND-1012/ND-1014 to design and validation | Node |
| F-015 | Update Appendix A traceability table | Node |
| F-017 | Add MD-0504/MD-0505 validation tests | Modem |
| F-018 | Add T-P test cases for protocol §10 modem codec | Protocol |
| F-026 | Author `hw-validation.md` | Hardware |
| F-027 | Document SPICE subsystem in `hw-design.md` | Hardware |
| F-029 | Update `hw-design.md` directory structure | Hardware |
| F-031 | Implement T-1304/T-1305a/T-1305b | Gateway |

### Priority 4 — Low (backlog)

| Finding | Action | Component |
|---------|--------|-----------|
| F-019/F-020 | Formalize orphan protocol tests in validation plan | Protocol |
| F-028 | Update tool entry point in `hw-design.md` | Hardware |
| F-030 | Document `working_dir` field | Gateway |
| F-032 | Create CI workflow for `hw/` | Hardware |

---

## 7. Prevention

### 7.1 Enforce spec-chain completeness in PR reviews

When a PR adds or modifies a requirement:
- Require corresponding design section update in the same PR (or linked PR)
- Require validation plan test case definition in the same PR
- Consider a PR checklist item: "All new REQ-IDs have design and validation
  entries"

### 7.2 Automated REQ-ID consistency checking

Add a CI check that:
- Extracts all REQ-IDs from `*-requirements.md`
- Extracts all REQ-ID references from test code comments
- Flags any REQ-ID referenced in tests that doesn't exist in requirements
  (would have caught F-004/GW-1308)
- Flags any REQ-ID in requirements with no test code reference

### 7.3 Validation plan Appendix A auto-generation

Consider generating the Appendix A traceability table from the test case
body text rather than maintaining it manually. This prevents the table from
going stale (as in F-015).

### 7.4 Hardware roadmap documentation

For the hardware component, maintain an explicit "Implementation Status"
section in `hw-design.md` that marks each pipeline stage as Implemented,
In Progress, or Planned. This sets expectations for which `Must`
requirements are not yet available.

---

## 8. Open Questions

1. **GW-1308**: Is this an intentional requirement that was never formalized,
   or a test written speculatively? Should it become a real requirement?

2. **Hardware pipeline scope**: Are HW-0801 (PCB layout), HW-0802
   (Gerber/CPL), and HW-1100–1104 (contracts) intended for this development
   cycle, or should they be explicitly deferred in the requirements doc?

3. **ND-1015/ND-1016 test feasibility**: Boot-time `warn!()` output
   verification may require hardware-in-the-loop testing. Should these be
   classified as manual test procedures?

4. **T-1403/T-1404 (live reload)**: Is there a technical blocker preventing
   these tests from being implemented, or is it purely a prioritization
   issue?

5. **Protocol §10 (modem codec)**: Are there modem codec tests elsewhere
   (e.g., in `sonde-modem` tests) that provide coverage even though they're
   not in the protocol validation plan?

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-30 | Copilot (maintenance audit) | Initial full audit — D1–D13 across all 5 components. 32 findings. |
