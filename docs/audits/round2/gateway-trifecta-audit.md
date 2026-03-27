<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# Gateway Specification Trifecta Audit — Investigation Report

## 1. Executive Summary

A systematic traceability audit of the sonde gateway specification set
(requirements, design, validation) across all 83 requirements (GW-0100
through GW-1304, including GW-0601a/GW-0601b) identified **10 findings**
of specification drift. Two are **High** severity: an untested
security-relevant requirement (GW-1304, build-type–aware log levels) and
illusory test coverage for the REBOOT command priority (T-0205 vs
GW-0204). Five are **Medium** severity, covering untested modem logging
(GW-1301), four requirements whose REQ-IDs are absent from the design
document despite their behavior being addressed, and a systemic absence
of documented assumptions. Three are **Low** severity, comprising minor
traceability labeling gaps and a stale internal cross-reference. Overall
specification integrity is **high** — no constraint violations (D6) were
found.

## 2. Problem Statement

The sonde gateway specification spans three documents — requirements,
design, and validation — authored over multiple development phases.
This audit checks whether every requirement traces forward into design
and validation, whether every design element and test case traces
backward to a requirement, and whether shared concepts, constraints,
and assumptions are consistent across all three documents.

**Expected behavior:** Complete bidirectional traceability with no
gaps, conflicts, or divergence.

**Impact:** Undetected drift can cause untested security requirements,
illusory test coverage, and scope creep.

## 3. Investigation Scope

- **Codebase / components examined:**
  - `docs/gateway-requirements.md` — 83 REQ-IDs (GW-0100 through GW-1304, including GW-0601a/GW-0601b)
  - `docs/gateway-design.md` — 19 sections, 10 modules, 17 design decisions
  - `docs/gateway-validation.md` — ~130 test cases (T-0100 through T-1304, including variants)
- **Time period:** Audit conducted against current HEAD of each document.
- **Tools used:** Manual cross-document traceability analysis; full-text
  search for every GW-XXXX identifier across all three documents.
- **Limitations:**
  - Source code compliance (D8–D10) and test code compliance (D11–D13)
    were NOT examined — those categories are reserved for future audits.
  - Quality of individual requirements, design decisions, or test
    procedures was NOT assessed — only cross-document consistency.
  - The requirements document contains no formal assumptions (ASM-NNN),
    constraints (CON-NNN), or dependencies (DEP-NNN); assumption
    analysis is therefore limited to design-stated assumptions.

## 4. Findings

### Finding F-001: GW-1304 has no test case

- **Severity**: High
- **Category**: D2_UNTESTED_REQUIREMENT
- **Location**:
  - Requirements: GW-1304 "Build-type–aware log levels" (§11, operational logging)
  - Validation: traceability matrix (§15) — GW-1304 absent
- **Description**: GW-1304 specifies six acceptance criteria governing
  compile-time trace-level stripping and runtime default `EnvFilter`
  behavior in debug vs release builds. No test case in the validation
  plan references GW-1304. The closest test (T-1304) validates GW-1303
  (build metadata / `--version` output), not GW-1304.
- **Evidence**: `grep -n "GW-1304" gateway-validation.md` returns zero
  matches. The traceability matrix (line 2029+) jumps from GW-1303 →
  T-1304 to the end of the table without a GW-1304 entry.
- **Root Cause**: GW-1304 was added after T-1304 was assigned to
  GW-1303, and no new test case was created for the new requirement.
- **Impact**: The requirement that `trace!` and `debug!` call-sites
  become no-ops in release builds (criterion 2) is unverified. If a
  Cargo feature flag is misconfigured, sensitive debug traces could
  leak in production binaries with no automated detection.
- **Remediation**: Add test cases for GW-1304. At minimum:
  (a) A build-system test asserting `tracing` dependency includes
  `max_level_trace` and `release_max_level_info` features.
  (b) A runtime test asserting the default `EnvFilter` matches
  `sonde_gateway=info` in debug and `sonde_gateway=warn` in release.
- **Confidence**: High — verified by full-text search of validation
  document.

---

### Finding F-002: T-0205 does not verify REBOOT priority

- **Severity**: High
- **Category**: D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location**:
  - Validation: T-0205 "Command priority ordering" (§4, line 209)
  - Requirements: GW-0204 "REBOOT command" (§4)
  - Traceability matrix: GW-0204 → T-0204, T-0205 (line 1957)
- **Description**: T-0205 is linked to GW-0200–0204, claiming to
  validate the priority ordering of all five command types. However,
  its procedure only queues three conditions (ephemeral + schedule
  change + program update) and asserts four outcomes:
  1. WAKE 1 → RUN_EPHEMERAL
  2. WAKE 2 → UPDATE_PROGRAM
  3. WAKE 3 → UPDATE_SCHEDULE
  4. WAKE 4 → NOP

  REBOOT is entirely absent from both the setup and assertions. The
  design (§6.4) defines REBOOT at priority 4 (between UPDATE_SCHEDULE
  and NOP), but T-0205 never tests this position.
- **Evidence**: T-0205 procedure (lines 209–219): no step queues a
  reboot; no assertion checks REBOOT's position between
  UPDATE_SCHEDULE and NOP. The design's command selection table (§6.4)
  explicitly places REBOOT at priority 4.
- **Root Cause**: T-0205 was likely written before REBOOT was added to
  the priority table, or REBOOT was considered fully covered by T-0204
  (which tests that REBOOT can be issued, but not its relative
  priority).
- **Impact**: Illusory coverage. The traceability matrix shows GW-0204
  covered by T-0205, but a priority ordering bug (e.g., REBOOT firing
  before UPDATE_SCHEDULE) would pass T-0205 undetected.
- **Remediation**: Extend T-0205 to also queue a pending reboot and
  assert REBOOT appears after UPDATE_SCHEDULE and before NOP. Updated
  expected sequence: RUN_EPHEMERAL → UPDATE_PROGRAM →
  UPDATE_SCHEDULE → REBOOT → NOP.
- **Confidence**: High — verified by reading T-0205 procedure verbatim.

---

### Finding F-003: GW-1301 has no automated test case

- **Severity**: Medium
- **Category**: D2_UNTESTED_REQUIREMENT
- **Location**:
  - Requirements: GW-1301 "Operational logging — modem transport state" (§11)
  - Validation: traceability matrix (line 2029) — entry reads
    `*(verified by integration/manual testing)*`
- **Description**: GW-1301 specifies that modem transport state
  transitions (`connected`, `ready`, `disconnecting`, `reconnecting`)
  are logged at INFO level with specific fields (e.g., `backoff_s`).
  The traceability matrix explicitly defers this to manual or
  integration testing with no automated test case ID.
- **Evidence**: Line 2029 of `gateway-validation.md`:
  `| GW-1301 | *(verified by integration/manual testing)* |`
- **Root Cause**: Modem transport state logging requires serial
  port lifecycle simulation, which may be difficult to automate
  in the test harness.
- **Impact**: Regression in modem state logging (e.g., missing
  `backoff_s` field in reconnection log) will not be caught by CI.
  Operational visibility of modem health depends on these logs.
- **Remediation**: Add test cases using the PTY-based mock modem
  (already used by T-1104a/T-1104b) to verify INFO log output for
  each transition. T-1104a already exercises reconnection with
  backoff; extending it to assert log content would close this gap.
- **Confidence**: High — verified by reading traceability matrix entry.

---

### Finding F-004: GW-0807 not traced in design document

- **Severity**: Medium
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**:
  - Requirements: GW-0807 "Admin API — modem management" (§9A)
  - Design: §3.1 module responsibilities table (line 78) — Admin API
    row lists GW-0800–0806, omits GW-0807. §13 Admin API header
    (line 749) — same omission.
- **Description**: GW-0807 defines four acceptance criteria for modem
  management operations (`GetModemStatus`, `SetModemChannel`,
  `ScanModemChannels`, and corresponding CLI commands). The design
  document DOES include all three RPCs in the gRPC service definition
  (§13.1, lines 782–784), the operations table (§13.2, lines 810–812),
  and the CLI reference (lines 845–847). However, the REQ-ID GW-0807
  is never cited in any design section's requirements list.
- **Evidence**: `grep -n "GW-0807" gateway-design.md` returns zero
  matches. The Admin API module row (line 78) ends at GW-0806. The
  §13 header (line 749) ends at GW-0806.
- **Root Cause**: GW-0807 was added to the requirements after the
  design document's traceability annotations were written, and the
  modem RPCs were added to the gRPC definition without back-linking.
- **Impact**: Low functional risk (behavior is fully designed), but
  traceability is broken — an audit querying "which design section
  addresses GW-0807?" would return no result.
- **Remediation**: Add GW-0807 to the Admin API module row in §3.1
  and to the §13 requirements header.
- **Confidence**: High — verified by full-text search.

---

### Finding F-005: GW-1216 not traced in design document

- **Severity**: Medium
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**:
  - Requirements: GW-1216 "Node ID duplicate handling" (§12)
  - Design: §17.5 PEER_REQUEST Processing pipeline, step 6 — labels
    the duplicate-handling logic as "(GW-1218 AC4)" not GW-1216. §17
    requirements header (line 936) lists GW-1200–GW-1222 but the
    per-stage annotations skip GW-1216.
- **Description**: GW-1216 specifies three acceptance criteria for the
  duplicate-node-ID decision point during pairing: (1) new node_id
  proceeds, (2) matching PSK skips re-registration but sends PEER_ACK,
  (3) different PSK is silently discarded. The design addresses this
  exact behavior in §17.5 step 6 but attributes it to "GW-1218 AC4"
  (which overlaps but is a different requirement — GW-1218 focuses on
  the registration record, not the decision point).
- **Evidence**: `grep -n "GW-1216" gateway-design.md` returns zero
  matches. Design §17.5 step 6 (line ~900): "Node ID duplicate
  handling (GW-1218 AC4)".
- **Root Cause**: GW-1216 and GW-1218 AC4/AC5 have overlapping scope.
  The design author cited the more specific GW-1218 criteria rather
  than the standalone GW-1216 requirement.
- **Impact**: GW-1216 appears untraced in a systematic audit, even
  though its behavior is fully realized by the same design section.
- **Remediation**: Add an explicit GW-1216 reference alongside
  GW-1218 AC4 in §17.5 step 6 and in the §17 requirements header.
- **Confidence**: High — verified by full-text search.

---

### Finding F-006: GW-1223 and GW-1224 not traced in design document

- **Severity**: Medium
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**:
  - Requirements: GW-1223 "Admin API — phone listing" (§12),
    GW-1224 "Admin API — phone revocation" (§12)
  - Design: §17 BLE Pairing Handler header (line 936) ends range at
    GW-1222. §13 Admin API header (line 749) ends at GW-0806.
- **Description**: GW-1223 requires `ListPhones` with metadata
  and CLI command. GW-1224 requires `RevokePhone` with CLI command.
  The design includes both RPCs in the gRPC definition (lines
  790–791), the operations table (lines 816–817), the CLI reference
  (lines 852–853), and the admin session section (§17.7, line 979).
  However, neither GW-1223 nor GW-1224 is cited by REQ-ID anywhere.
  The admin session section (line 979) attributes ListPhones and
  RevokePhone to "(GW-1222)" instead.
- **Evidence**: `grep -n "GW-1223\|GW-1224" gateway-design.md` returns
  zero matches. BLE Pairing Handler scope: "GW-1200–GW-1222" (line
  79, 936).
- **Root Cause**: GW-1223 and GW-1224 were split from GW-1222 after
  the design's traceability annotations were written.
- **Impact**: Same as F-004 — traceability gap, not a functional gap.
- **Remediation**: Extend the BLE Pairing Handler range to GW-1224 in
  §3.1 and §17, and add explicit GW-1223/GW-1224 citations in §17.7
  alongside GW-1222.
- **Confidence**: High — verified by full-text search.

---

### Finding F-007: Design assumptions not documented in requirements

- **Severity**: Medium
- **Category**: D5_ASSUMPTION_DRIFT
- **Location**:
  - Requirements: no ASM-NNN, CON-NNN, or DEP-NNN sections exist
  - Design: 9 explicit assumptions documented across §§4, 6, 9, 10,
    10a, 17
- **Description**: The design document states nine explicit assumptions
  (radio unreliability, predictable wake schedules, master-key
  stability, storage atomicity, handler locality, operator trust
  boundary, modem response timeliness, clock monotonicity, failover
  group manual coordination). None of these are acknowledged in the
  requirements document, which has no assumptions, constraints, or
  dependencies section at all.
- **Evidence**: The requirements document summary (end of file) states:
  "No assumptions (ASM-NNN), constraints (CON-NNN), or dependencies
  (DEP-NNN) were defined in this requirements document."
  Design §4 (transport): "Frames are not reliably delivered."
  Design §10a.1 (master key): "Key remains constant across restarts;
  no key rotation defined."
  Design §10 (storage): "`upsert_node`, `store_program` operations
  assumed atomic."
  Design §17.5 (timestamp): "Assumes reasonable clock" for ±86 400 s
  validation.
- **Root Cause**: The requirements document was structured without a
  formal assumptions section. Design assumptions were documented
  locally but never propagated back to requirements.
- **Impact**: Test validity may depend on unstated assumptions. For
  example, GW-1215 (timestamp validation ±86 400 s) assumes monotonic,
  roughly-accurate clocks — but the validation plan does not state
  this as a test precondition. If a test environment uses mocked time,
  the assumption is satisfied implicitly; if run on hardware with
  drifted clocks, test results could be unreliable.
- **Remediation**: Add an Assumptions section to
  `gateway-requirements.md` documenting the nine design assumptions
  or explicitly classifying each as in-scope or out-of-scope.
- **Confidence**: High — direct comparison of document structures.

---

### Finding F-008: GW-0103 AC5 references non-existent GW-0205

- **Severity**: Low
- **Category**: D5_ASSUMPTION_DRIFT
- **Location**:
  - Requirements: GW-0103 acceptance criterion 5 (line 106):
    "The `command_type` field is one of the defined command types
    (see GW-0200–GW-0205)."
  - Requirements: §4 Command Set — defines GW-0200 through GW-0204
    only. GW-0205 does not exist.
- **Description**: GW-0103 AC5 cross-references "GW-0200–GW-0205",
  but the command set only defines five commands: GW-0200 (NOP),
  GW-0201 (UPDATE_PROGRAM), GW-0202 (RUN_EPHEMERAL), GW-0203
  (UPDATE_SCHEDULE), GW-0204 (REBOOT). GW-0205 is not assigned.
  This is an internal stale reference within the requirements
  document — likely left from a consolidation that removed or
  renumbered a sixth command type.
- **Evidence**: Line 106 of `gateway-requirements.md`:
  `5. The command_type field is one of the defined command types
  (see GW-0200–GW-0205).`
  No requirement with ID GW-0205 exists in the document.
- **Root Cause**: A command type was removed or renumbered during
  requirements consolidation, and the cross-reference in GW-0103 AC5
  was not updated.
- **Impact**: Low. The stale reference does not cause design or
  validation drift — both correctly use GW-0200–0204. A reader may
  search for GW-0205 and find nothing, causing confusion.
- **Remediation**: Update GW-0103 AC5 to read "GW-0200–GW-0204".
- **Confidence**: High — verified by full-text search.

---

### Finding F-009: GW-0404 absent from Program Library module row

- **Severity**: Low
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**:
  - Requirements: GW-0404 "Sonde-specific verifier platform" (§6)
  - Design: §3.1 module responsibilities table (line 74) — Program
    Library row lists GW-0300–0302, GW-0400–0403; omits GW-0404.
    However, §8.2 (line 389) and §8.2.1 (line 398) DO reference
    GW-0404 explicitly in the ingestion pipeline and SondePlatform
    subsection.
- **Description**: The summary module table omits GW-0404 from the
  Program Library module's requirement list, even though the detailed
  design sections for that module address GW-0404 thoroughly (custom
  `SondePlatform`, helper prototype table, rationale for wrapping
  `LinuxPlatform`).
- **Evidence**: Line 74: `| **Program Library** | ... |
  GW-0300–0302, GW-0400–0403, GW-1004 |` — GW-0404 not listed.
  Line 389: "Verify with `prevail-rust` using `SondePlatform`
  ... (GW-0401, GW-0404)." Line 398: "The gateway uses a custom
  Prevail platform (`SondePlatform`) ... (GW-0404)."
- **Root Cause**: The module table was written before GW-0404 was
  split from GW-0401, and the table was not updated to include the
  new REQ-ID.
- **Impact**: Minimal — GW-0404 IS traced in the body of §8. Only
  the summary table is incomplete.
- **Remediation**: Append GW-0404 to the Program Library row in the
  module responsibilities table (change `GW-0400–0403` to
  `GW-0400–0404`).
- **Confidence**: High — verified by full-text search.

---

### Finding F-010: GW-0705 absent from Node Registry module row

- **Severity**: Low
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**:
  - Requirements: GW-0705 "Factory reset support" (§9)
  - Design: §3.1 module responsibilities table (line 73) — Node
    Registry row lists GW-0601, GW-0700–0703; omits GW-0705.
    However, §7.3 (line 361) DOES reference GW-0705 explicitly:
    "The registry supports adding and removing nodes (GW-0601,
    GW-0705)."
- **Description**: Same pattern as F-009. The summary module table
  omits GW-0705 from the Node Registry module, but the detailed
  section addresses it.
- **Evidence**: Line 73: `| **Node Registry** | ... |
  GW-0601, GW-0700, GW-0701, GW-0702, GW-0703 |` — GW-0705 not
  listed. Line 361: "... (GW-0601, GW-0705)."
- **Root Cause**: Table not updated after GW-0705 was added.
- **Impact**: Minimal — same as F-009.
- **Remediation**: Add GW-0705 to the Node Registry row in the
  module responsibilities table.
- **Confidence**: High — verified by full-text search.

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|--------|-------|
| **Total requirements** | 83 |
| **Requirements → Design (traced)** | 54 of 83 (65.1 %) |
| **Requirements → Design (untraced by REQ-ID)** | GW-0807, GW-1216, GW-1223, GW-1224 (all 4 have behavior addressed but REQ-ID not cited) |
| **Requirements → Validation (traced)** | 56 of 83 (67.5 %) |
| **Requirements → Validation (untested)** | GW-1301 (deferred to manual), GW-1304 (absent) |
| **Design → Requirements (orphaned)** | 0 — all design components trace to at least one requirement |
| **Validation → Requirements (orphaned)** | 0 — all test cases reference valid REQ-IDs |
| **Acceptance criteria mismatches** | 1 (T-0205 vs GW-0204) |
| **Assumption alignment** | 0 aligned (requirements has no assumptions section), 9 design-only |
| **Constraint violations (D6)** | 0 |

### Systemic Patterns

Two root causes account for 8 of the 10 findings:

1. **Stale traceability annotations** (F-004, F-005, F-006, F-008,
   F-009, F-010): Requirements were added or renumbered during
   consolidation, but cross-references in the design document's
   module table and section headers were not updated. The detailed
   design sections address the behavior correctly; only the summary
   annotations lag behind.

2. **Missing requirements scaffolding** (F-007): The requirements
   document lacks a formal assumptions section, so design assumptions
   cannot be cross-referenced. This is a structural gap, not a
   content gap.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Add test cases for GW-1304 (compile-time trace stripping, runtime `EnvFilter` defaults) | S | Low |
| 2 | F-002 | Extend T-0205 to queue a REBOOT and verify its position between UPDATE_SCHEDULE and NOP | S | Low |
| 3 | F-003 | Add automated test for GW-1301 using PTY mock modem; assert INFO logs for state transitions | S | Low |
| 4 | F-004 | Add `GW-0807` to design §3.1 Admin API row and §13 header | S | None |
| 5 | F-005 | Add `GW-1216` reference alongside `GW-1218 AC4` in design §17.5 step 6 and §17 header | S | None |
| 6 | F-006 | Extend BLE Pairing Handler range to GW-1224 in design §3.1 and §17; cite GW-1223/GW-1224 in §17.7 | S | None |
| 7 | F-007 | Add Assumptions section to `gateway-requirements.md` documenting the 9 design assumptions | M | Low |
| 8 | F-008 | Update GW-0103 AC5 cross-reference from `GW-0200–GW-0205` to `GW-0200–GW-0204` | S | None |
| 9 | F-009 | Change Program Library row from `GW-0400–0403` to `GW-0400–0404` in design §3.1 | S | None |
| 10 | F-010 | Add GW-0705 to Node Registry row in design §3.1 | S | None |

## 7. Prevention

- **Traceability-matrix CI check:** Add a script that extracts all
  GW-XXXX identifiers from the requirements document and verifies that
  each appears at least once in both the design and validation
  documents. Run on every docs/ change in CI.

- **Module table review checklist:** When adding or renumbering a
  requirement, require that the design §3.1 module responsibilities
  table is updated in the same commit.

- **Assumptions section template:** Add an empty Assumptions /
  Constraints / Dependencies section to the requirements document
  template so future requirements authors have a place to record them.

- **Validation plan completeness gate:** Before merging a new GW-XXXX
  requirement, require that at least one T-XXXX test case is added to
  the validation plan in the same PR, or that an explicit deferral
  note is added to the traceability matrix with a tracking issue.

## 8. Open Questions

1. **GW-0205 history:** Was GW-0205 a previously defined command type
   that was removed during consolidation, or was the range in GW-0103
   AC5 always erroneous? Resolving this would determine whether
   GW-0205 needs to be formally retired (with a note like GW-0704) or
   the cross-reference simply corrected.

2. **GW-1304 test feasibility:** Criterion 2 ("in release builds, the
   compile-time maximum tracing level is INFO") requires building in
   release mode and verifying that `debug!` calls are no-ops. Is this
   testable within the existing test harness, or does it require a
   separate build-system integration test?

3. **GW-1301 manual testing status:** The traceability matrix notes
   "verified by integration/manual testing." Has this manual testing
   actually been performed and recorded, or is this aspirational?

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-04 | Specification Analyst (automated) | Initial trifecta audit — 10 findings across D1, D2, D5, D7 categories |
