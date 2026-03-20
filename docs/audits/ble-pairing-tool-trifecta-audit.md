<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# BLE Pairing Tool Specification Trifecta Audit — Investigation Report

## 1. Executive Summary

This audit examines forward and backward traceability across the BLE pairing
tool specification trifecta: requirements (`ble-pairing-tool-requirements.md`),
design (`ble-pairing-tool-design.md`), and validation
(`ble-pairing-tool-validation.md`).

**Key metrics:**

| Metric | Value |
|---|---|
| Total requirements | 55 |
| Total test cases | 57 |
| Requirements → Design coverage | 55/55 (100.0%) |
| Requirements → Test coverage (direct traceability) | 35/55 (63.6%) |
| Functional requirements → Test coverage (excl. structural + meta) | 35/43 (81.4%) |
| CI-testable functional coverage | 35/39 (89.7%) |
| Orphaned test cases (D4) | 0 |
| Orphaned design decisions (D3) | 0 |
| Total findings | 15 |
| High severity | 2 |
| Medium severity | 4 |
| Low severity | 6 |
| Informational | 3 |

The specification trifecta is well-structured with strong design traceability
(100%).  The most significant issues are a **traceability mislabel** that masks
coverage of a Must-priority security requirement (PT-0902), a **missing test**
for the no-implicit-retries requirement (PT-1003), and **design omissions** for
the BLE connection timeout mechanism and the gateway Just Works LESC fallback.

---

## 2. Problem Statement

Before implementation begins on the `sonde-pair` crate, we need confidence that
every requirement is addressed by the design, every design decision traces back
to a requirement, and every testable requirement has concrete validation
coverage.  Gaps discovered after implementation are an order of magnitude more
expensive to remediate than gaps discovered during specification review.

---

## 3. Investigation Scope

### Documents audited

| Document | Path | Role |
|---|---|---|
| Requirements | `docs/ble-pairing-tool-requirements.md` | 55 requirements (PT-0100 … PT-1206) |
| Design | `docs/ble-pairing-tool-design.md` | 14 sections, §14 traceability matrix |
| Validation | `docs/ble-pairing-tool-validation.md` | 57 test cases (T-PT-100 … T-PT-903), Appendix A traceability |

### ID patterns

- Requirements: `PT-XXXX` (section-based hundreds: §3→01xx, §4→02xx, … §14→12xx)
- Test cases: `T-PT-NNN` (numeric, with one exception: `T-PT-208a`)
- Design references: explicit `PT-XXXX` mentions + `PT-XXXX–PT-YYYY` range patterns

### Methodology

1. Extracted all requirement IDs (55), test case IDs (57), and "Validates:" mappings.
2. Expanded all range references in the design traceability table (§14).
3. Computed forward traceability (requirements → design, requirements → tests).
4. Computed backward traceability (tests → requirements, design → requirements).
5. Performed cross-document consistency checks on terminology, timeouts, crypto
   parameters, and protocol references.

---

## 4. Findings

### F-001 — T-PT-309 traceability mislabel masks PT-0902 coverage

**Severity:** High  
**Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH  
**Location:** Validation doc §5 (T-PT-309), Appendix A  
**Confidence:** Definite

**Description:**  
Test case T-PT-309 ("Ed25519 → X25519 low-order point rejection") declares
`Validates: PT-0405` (Gateway public key encryption).  However, its procedure
tests the scenario described by PT-0902 (Ed25519 ↔ X25519 conversion safety):
constructing an Ed25519 key that maps to a low-order X25519 point, attempting
the conversion, and asserting that it returns an error with message "invalid
gateway public key."  This exactly matches PT-0902's acceptance criteria, not
PT-0405's primary concern (ECDH + HKDF + AES-GCM encryption).

**Evidence:**
- T-PT-309 Validates field: `PT-0405` (validation doc line 449)
- T-PT-309 procedure: "Construct an Ed25519 public key that maps to a low-order
  X25519 point.  Attempt the Ed25519 → X25519 conversion.  Assert: conversion
  returns an error." (validation doc lines 451–455)
- PT-0902 AC1: "Conversion of an Ed25519 key that maps to a low-order X25519
  point returns an error." (requirements doc line 655)
- PT-0902 AC2: "The error is reported to the operator as 'invalid gateway
  public key'." (requirements doc line 656)

**Root Cause:**  
PT-0405 acceptance criterion #2 also mentions low-order point rejection ("Ed25519
→ X25519 conversion rejects low-order points"), creating ambiguity about which
requirement T-PT-309 should trace to.  The author chose PT-0405 because the
criterion appeared there, but PT-0902 is the dedicated requirement for this
exact behavior.

**Impact:**  
PT-0902 (a Must-priority security requirement) shows zero direct test coverage
in the traceability matrix, which would cause it to be flagged as untested
during implementation reviews.

**Remediation:**  
Change T-PT-309 `Validates:` to `PT-0902` (or `PT-0902, PT-0405`).  Update
Appendix A accordingly.

---

### F-002 — PT-0902 (Ed25519 ↔ X25519 conversion safety) has no direct test traceability

**Severity:** High  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §11 (PT-0902), Validation doc Appendix A  
**Confidence:** Definite

**Description:**  
Requirement PT-0902 ("Ed25519 ↔ X25519 conversion safety," Must priority) does
not appear in any test case's `Validates:` field.  No row in the validation
doc's Appendix A traceability table references PT-0902.

**Evidence:**
- PT-0902 is defined at requirements doc line 647.
- Appendix A (validation doc lines 796–855): PT-0902 is absent from the
  "Requirement" column.
- T-PT-309 substantively covers PT-0902's scenario but maps to PT-0405 (see
  F-001).

**Root Cause:**  
Direct consequence of the mislabel in F-001.

**Impact:**  
An implementer relying solely on the traceability matrix would conclude that
PT-0902 has no test coverage and either write a duplicate test or defer the
requirement.

**Remediation:**  
Resolve F-001.  Once T-PT-309 maps to PT-0902, this finding is automatically
closed.

---

### F-003 — PT-1003 (No implicit retries) has no test case

**Severity:** Medium  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §12 (PT-1003)  
**Confidence:** Definite

**Description:**  
Requirement PT-1003 ("No implicit retries," Must priority) states that the tool
MUST NOT silently retry failed protocol operations.  No test case validates this
behavior.  The design addresses it in §8.3, confirming no retries, but the
validation doc contains no test that asserts the absence of retries after a
failure.

**Evidence:**
- PT-1003 defined at requirements doc line 724.
- Design §8.3 (design doc line 665): "The tool does not silently retry failed
  protocol operations (PT-1003)."
- No `Validates: PT-1003` in any test case.
- Appendix A does not include PT-1003.

**Root Cause:**  
The validation doc focuses on positive error-handling behavior (correct error
messages, clean disconnects) but omits the negative behavioral assertion that no
automatic retry occurs.

**Impact:**  
An implementation could introduce automatic retries (e.g., for transient BLE
failures) without any test catching the deviation from the specification.

**Remediation:**  
Add a test case (e.g., T-PT-803) that injects a protocol failure (e.g.,
`GW_INFO_RESPONSE` timeout), asserts the error is reported to the operator, and
asserts that only one `REQUEST_GW_INFO` write was captured by the mock transport
(i.e., no retry occurred).

---

### F-004 — Requirement overlap between PT-0405 AC2 and PT-0902

**Severity:** Medium  
**Category:** D6_CONSTRAINT_VIOLATION  
**Location:** Requirements doc §6 (PT-0405) and §11 (PT-0902)  
**Confidence:** Definite

**Description:**  
PT-0405 acceptance criterion #2 states "Ed25519 → X25519 conversion rejects
low-order points."  PT-0902 is an entire dedicated requirement for "Ed25519 ↔
X25519 conversion safety" with acceptance criteria that describe the same
behavior.  This duplication creates traceability confusion (demonstrated by
F-001) and ambiguity about which requirement an implementer should reference
when testing low-order point rejection.

**Evidence:**
- PT-0405 AC2 (requirements doc line 352): "Ed25519 → X25519 conversion rejects
  low-order points."
- PT-0902 AC1 (requirements doc line 655): "Conversion of an Ed25519 key that
  maps to a low-order X25519 point returns an error."

**Root Cause:**  
PT-0405 covers the full Phase 2 encryption flow and embeds a cross-cutting
security concern (conversion safety) as one of five acceptance criteria.
PT-0902 was later added as a standalone security requirement for the same
concern.

**Impact:**  
Implementers and reviewers may disagree about where to trace conversion-safety
tests, leading to coverage confusion.

**Remediation:**  
In PT-0405, replace AC2 with a reference: "Ed25519 → X25519 conversion safety
per PT-0902."  This eliminates the duplication while preserving the reminder
that the conversion step is part of the Phase 2 flow.

---

### F-005 — BLE connection establishment timeout not specified in design

**Severity:** Medium  
**Category:** D5_ASSUMPTION_DRIFT  
**Location:** Requirements doc §12 (PT-1002), Design doc §5.1, Validation doc
§10 (T-PT-802)  
**Confidence:** Definite

**Description:**  
PT-1002 requires a 10-second BLE connection establishment timeout.  T-PT-802
asserts this value.  However, the design's `BleTransport::connect()` method
signature (`async fn connect(&self, device: &DeviceId) -> Result<u16,
PairingError>`) includes no timeout parameter and §5.3 does not specify the
10-second bound.  The requirement and test agree, but the design that bridges
them is silent.

**Evidence:**
- PT-1002 (requirements doc line 712): "BLE connection establishment 10 s."
- T-PT-802 step 6 (validation doc line 738): "Assert: BLE connection
  establishment timeout = 10 s."
- Design §5.1 `connect()` (design doc line 279): no timeout parameter.
- Design §5.3 (design doc lines 307–308): discusses MTU negotiation but not
  connection timeout.

**Root Cause:**  
The design treats `connect()` timeouts as a transport-implementation detail,
but the 10-second value is a protocol-level requirement that should be
specified at the trait level or documented as a constant.

**Impact:**  
Platform implementers of `BleTransport` may use inconsistent timeout values.

**Remediation:**  
Add either a timeout parameter to `BleTransport::connect()` or document the
10-second constant in §5.3 as a protocol requirement that all implementations
must honor.

---

### F-006 — Gateway Just Works LESC fallback omitted from design

**Severity:** Medium  
**Category:** D5_ASSUMPTION_DRIFT  
**Location:** Requirements doc §5 (PT-0300), Design doc §5.1  
**Confidence:** Probable

**Description:**  
PT-0300 states: "Numeric Comparison is the default method for the gateway
pairing service. … Just Works is available as a fallback when no operator is
present."  The design's `BleTransport::connect()` comment says "Numeric
Comparison for gateway, Just Works for node" — implying Just Works is *only*
for nodes.  The gateway Just Works fallback path is not mentioned in the
transport design or state machine.

**Evidence:**
- PT-0300 (requirements doc line 180): "Just Works is available as a fallback
  when no operator is present."
- Design §5.1 `connect()` (design doc lines 277–278): "The implementation
  handles LESC pairing (Numeric Comparison for gateway, Just Works for node)."
- Design §9.1 (design doc line 675): Windows section mentions only Numeric
  Comparison passkey display for gateway.
- Design §9.2 (design doc lines 684): Android section mentions both methods but
  in a generic context, not specifically for gateway fallback.

**Root Cause:**  
The design partitioned pairing methods by device type (gateway = Numeric
Comparison, node = Just Works) rather than by scenario (operator present vs.
absent), simplifying the model but losing the fallback requirement.

**Impact:**  
Implementations may reject or fail gateway connections in headless/automated
scenarios where no operator is available to confirm a Numeric Comparison
passkey.

**Remediation:**  
Update the `BleTransport::connect()` documentation and §9.1/§9.2 to
acknowledge that gateway connections support both Numeric Comparison (default)
and Just Works (fallback when no operator is present).

---

### F-007 — UI requirements (PT-0700, PT-0701, PT-0702) have no test cases

**Severity:** Low  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §9  
**Confidence:** Definite

**Description:**  
Three Must-priority UI requirements have no test cases in the validation doc:
PT-0700 (Minimum UI surface), PT-0701 (Phase indication), and PT-0702 (Verbose
diagnostic mode).

**Evidence:**
- PT-0700 (requirements doc line 489), PT-0701 (line 505), PT-0702 (line 519).
- None appear in Appendix A (validation doc lines 796–855).

**Root Cause:**  
The validation doc explicitly scopes itself to integration tests against the
pairing state machine (validation doc line 15: "integration tests that exercise
the pairing state machine through its external interfaces").  UI behavior is
outside that scope.

**Impact:**  
Low — UI requirements are typically validated by manual testing (PT-1206) and
Tauri-specific test frameworks.  However, PT-0702 (verbose mode) has a testable
aspect: T-PT-700/T-PT-701 test that key material doesn't appear in logs, but no
test verifies the verbose toggle itself or that verbose mode is off by default.

**Remediation:**  
Consider adding a test case for PT-0702's toggle behavior (verbose mode
disabled by default, enabled by explicit flag).  PT-0700 and PT-0701 can remain
manual-only with a note in the validation doc acknowledging the deferral.

---

### F-008 — PT-1100 (Required primitives) has no explicit test

**Severity:** Low  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §13 (PT-1100)  
**Confidence:** Definite

**Description:**  
PT-1100 enumerates all eight cryptographic primitives the tool must implement.
No test case explicitly validates that all eight are present and used correctly.

**Evidence:**
- PT-1100 (requirements doc line 756): lists Ed25519 verification, X25519 ECDH,
  Ed25519 → X25519 conversion, HKDF-SHA256, AES-256-GCM, HMAC-SHA256, SHA-256,
  and CSPRNG.
- Individual primitives are tested by T-PT-202 (Ed25519), T-PT-308 (X25519,
  HKDF, AES-GCM), T-PT-307 (HMAC), T-PT-303 (SHA-256), T-PT-702 (CSPRNG),
  T-PT-309 (Ed25519 → X25519) — but no single test validates all eight as a
  checklist.

**Root Cause:**  
PT-1100 is a meta-requirement about the crypto suite.  Individual primitives are
tested via protocol-flow tests, providing implicit coverage.

**Impact:**  
Low — all eight primitives are exercised individually.  The gap is formal, not
substantive.

**Remediation:**  
Add a note to PT-1100 listing the test cases that collectively cover each
primitive (cross-reference table).  No new test is strictly needed.

---

### F-009 — Architecture requirements (PT-0100–PT-0104, PT-1004) have no automated tests

**Severity:** Low  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §3, §12  
**Confidence:** Definite

**Description:**  
Six structural/architecture requirements have no automated test cases:
PT-0100 (Supported platforms), PT-0101 (Rust-first implementation), PT-0102
(Platform isolation), PT-0103 (Crate placement), PT-0104 (Separation of
concerns), and PT-1004 (Reusable core).

**Evidence:**
- None of these IDs appear in Appendix A.
- Design doc §3 traces all of them, and §14 lists them explicitly.

**Root Cause:**  
These are architectural constraints validated by build success (cross-platform
CI), dependency graph inspection, and code review — not behavioral test cases.

**Impact:**  
Low — CI build targets and `Cargo.toml` review provide adequate validation.

**Remediation:**  
Document in the validation doc's §1 (Overview) that PT-0100–PT-0104 and
PT-1004 are validated by CI build targets and code review, not by test cases.

---

### F-010 — Testing meta-requirements (PT-1200–PT-1206) have no direct test traceability

**Severity:** Low  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §14  
**Confidence:** Definite

**Description:**  
Seven requirements in §14 (Testing) describe what the test plan should contain
(e.g., "A test MUST exercise the complete Phase 1 flow").  These
meta-requirements are satisfied by the test cases that exist in the validation
doc, but no test case's `Validates:` field references any PT-12xx ID.

**Evidence:**
- PT-1201 requires a Phase 1 happy-path test → satisfied by T-PT-208.
- PT-1202 requires Phase 1 error-path tests → satisfied by T-PT-203, T-PT-204,
  T-PT-206, T-PT-209–T-PT-212.
- PT-1203 requires a Phase 2 happy-path test → satisfied by T-PT-311.
- PT-1204 requires Phase 2 error-path tests → satisfied by T-PT-300,
  T-PT-310, T-PT-312–T-PT-314.
- PT-1205 requires input validation tests → satisfied by T-PT-303–T-PT-306.
- PT-1200 requires a mock BLE transport → the test harness exists.
- PT-1206 requires manual hardware testing → outside automated scope.

**Root Cause:**  
Self-referential requirements: they define what the test plan should contain.
The validation doc satisfies them by existing, but doesn't trace back to them.

**Impact:**  
Low — a future audit could mistake these for untested requirements.

**Remediation:**  
Add a paragraph in the validation doc §1 (Overview) listing which test cases
satisfy each PT-12xx requirement.

---

### F-011 — PT-0801 (Platform-appropriate secure storage) has no test case

**Severity:** Low  
**Category:** D2_UNTESTED_REQUIREMENT  
**Location:** Requirements doc §10 (PT-0801)  
**Confidence:** Definite

**Description:**  
PT-0801 ("Platform-appropriate secure storage," Should priority) requires
Android Keystore and Windows DPAPI-protected storage.  No test case validates
these platform-specific mechanisms.

**Evidence:**
- PT-0801 (requirements doc line 555).
- Design §7.3 describes `FilePairingStore` (Windows) and
  `AndroidPairingStore` implementations.
- No test in Appendix A targets PT-0801.

**Root Cause:**  
Platform-specific secure storage cannot be tested in CI with mock transports.
It requires manual testing on each platform.

**Impact:**  
Low — Should-priority requirement; covered by manual hardware testing
(PT-1206).

**Remediation:**  
Add a note to PT-1206's manual test checklist to verify secure storage on
each platform.

---

### F-012 — Protocol doc section reference discrepancy

**Severity:** Low  
**Category:** D5_ASSUMPTION_DRIFT  
**Location:** Requirements doc §13 (PT-1101), Design doc §6.4  
**Confidence:** Probable

**Description:**  
PT-1101's source field cites "ble-pairing-protocol.md §5.5, §6.3" for the
HKDF parameters.  The design doc §6.4 table cites "ble-pairing-protocol.md
§6.4" for the Phase 2 HKDF info string.  The section numbers do not match
(§6.3 vs. §6.4).

**Evidence:**
- PT-1101 (requirements doc line 779): "Source: ble-pairing-protocol.md §5.5,
  §6.3"
- Design §6.4 HKDF table (design doc line 414): "ble-pairing-protocol.md §6.4"

**Root Cause:**  
The protocol specification was likely restructured (section added or renumbered)
after the requirements were written, and one of the two documents was not
updated.

**Impact:**  
Low — the actual HKDF parameters (`gateway_id` salt, `"sonde-node-pair-v1"`
info) are consistent across all three documents.  Only the source cross-reference
is stale.

**Remediation:**  
Verify the current section numbering in `ble-pairing-protocol.md` and update
whichever document has the stale reference.

---

### F-013 — T-PT-205 procedure wording is ambiguous

**Severity:** Informational  
**Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH  
**Location:** Validation doc §4 (T-PT-205)  
**Confidence:** Probable

**Description:**  
T-PT-205 step 2 says "Complete a successful gateway authentication" and step 3
asserts `gw_public_key` and `gateway_id` are persisted.  In the design (§4.2),
artifacts are persisted only at the end of the full Phase 1 flow (after
registration), not after authentication alone.  If the test stops after
authentication, persistence would not have occurred.

**Evidence:**
- T-PT-205 step 2 (validation doc line 215): "Complete a successful gateway
  authentication."
- Design §4.2 (design doc lines 169–173): Persist step occurs after
  registration and decryption of `PHONE_REGISTERED`.

**Root Cause:**  
Imprecise wording.  The test likely intends "Complete a successful Phase 1 flow"
(which includes authentication) but uses narrower language.

**Impact:**  
Informational — an implementer following the procedure literally might write a
test that fails because persistence hasn't occurred yet.

**Remediation:**  
Reword T-PT-205 step 2 to "Complete a successful Phase 1 flow" or add a note
that authentication alone does not trigger persistence.

---

### F-014 — T-PT-213 and T-PT-315 verify key zeroing structurally, not at runtime

**Severity:** Informational  
**Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH  
**Location:** Validation doc §4 (T-PT-213), §5 (T-PT-315)  
**Confidence:** Definite

**Description:**  
Both test cases verify ephemeral key zeroing by asserting that values are
"wrapped in `Zeroizing`" and "verified structurally by type signatures."  This
confirms the `Zeroizing` wrapper is used but does not verify that key material
is actually zeroed in memory at runtime.  The corresponding requirements
(PT-0304 AC2, PT-0408 AC2) use stronger language: "no copies of these values
remain in memory."

**Evidence:**
- T-PT-213 step 3 (validation doc line 327): "verified structurally by type
  signatures using `Zeroizing<[u8; N]>`."
- PT-0304 AC2 (requirements doc line 255): "no copies of these values remain in
  memory."

**Root Cause:**  
Runtime memory verification is impractical in safe Rust without debug tools.
The `Zeroizing` type guarantee is the strongest practical assertion available.

**Impact:**  
Informational — this is an inherent limitation of the testing approach, not a
specification defect.  The `zeroize` crate is well-audited and provides the
guarantee at the type level.

**Remediation:**  
No action required.  Consider adding a comment to PT-0304 and PT-0408 noting
that structural verification via `Zeroizing` type signatures is the accepted
test methodology.

---

### F-015 — Test ID T-PT-208a breaks numeric convention

**Severity:** Informational  
**Category:** D6_CONSTRAINT_VIOLATION  
**Location:** Validation doc §4 (T-PT-208a)  
**Confidence:** Definite

**Description:**  
All 57 test case IDs follow the numeric pattern `T-PT-NNN` except T-PT-208a,
which uses an alphabetic suffix.  This creates a minor convention inconsistency
and complicates range-based references (e.g., design doc §13 P3.2: "T-PT-200
to T-PT-213" — does that include T-PT-208a?).

**Evidence:**
- T-PT-208a (validation doc line 258).
- Design doc §13 P3.2 (design doc line 836): "T-PT-200 to T-PT-213" — range
  ambiguity.

**Root Cause:**  
T-PT-208a was likely added after the initial test plan was written (as a
supplemental test for phone label validation) without renumbering subsequent
test IDs.

**Impact:**  
Informational — no behavioral impact, but automated tooling or scripts
parsing test IDs with pure numeric patterns may miss T-PT-208a.

**Remediation:**  
Either renumber to T-PT-209 (shifting subsequent IDs) or adopt a convention
note acknowledging alphabetic suffixes for supplemental tests.

---

## 5. Root Cause Analysis

The findings cluster into three root causes:

### 5.1 Traceability bookkeeping error (F-001, F-002, F-004)

A single requirement overlap (PT-0405 AC2 duplicating PT-0902) caused T-PT-309
to be traced to the wrong requirement, leaving a Must-priority security
requirement with no formal test coverage.  **Root cause:** duplication of a
constraint across two requirements without a cross-reference.

### 5.2 Validation scope definition (F-003, F-007, F-008, F-009, F-010, F-011)

The validation doc explicitly scopes itself to "integration tests that exercise
the pairing state machine" (§1).  This implicitly excludes architectural
requirements, UI requirements, platform-specific storage, and negative
behavioral requirements (like "no retries").  **Root cause:** the validation
doc does not have a section acknowledging requirements that are intentionally
out of scope or validated by other means.

### 5.3 Design-level omissions (F-005, F-006, F-012)

Two protocol-level details (10 s connection timeout, gateway Just Works
fallback) are specified in the requirements and tested in the validation doc but
absent from the design.  A stale protocol-doc section reference adds minor
confusion.  **Root cause:** the design focused on the core state machine and
crypto flows, treating transport-level timing and pairing-mode selection as
implementation details.

### Coverage Metrics

| Category | Total | Tested | Coverage |
|---|---|---|---|
| All requirements | 55 | 35 | 63.6% |
| Functional (excl. structural + meta) | 43 | 35 | 81.4% |
| CI-testable functional (excl. UI + platform) | 39 | 35 | 89.7% |
| Design traceability | 55 | 55 | 100.0% |

| Test-to-requirement mapping | Count |
|---|---|
| Total test cases | 57 |
| Unique requirements tested | 35 |
| Orphaned test cases (invalid req ref) | 0 |
| Mislabeled test cases | 1 (T-PT-309) |

---

## 6. Remediation Plan

| Priority | Finding | Action | Effort |
|---|---|---|---|
| P1 | F-001, F-002 | Change T-PT-309 `Validates:` to `PT-0902` (or `PT-0902, PT-0405`). Update Appendix A. | 5 min |
| P1 | F-004 | Replace PT-0405 AC2 with cross-reference to PT-0902. | 5 min |
| P2 | F-003 | Add test case T-PT-803: inject failure, assert single write (no retry). | 15 min |
| P2 | F-005 | Document 10 s connection timeout in design §5.3 or add timeout param to `connect()`. | 10 min |
| P2 | F-006 | Update design §5.1 and §9.x to acknowledge gateway Just Works fallback. | 10 min |
| P3 | F-007 | Add T-PT-804 for PT-0702 verbose toggle; note PT-0700/PT-0701 as manual-only. | 15 min |
| P3 | F-010 | Add paragraph to validation §1 mapping PT-12xx to satisfying test cases. | 10 min |
| P3 | F-009 | Add paragraph to validation §1 noting PT-01xx/PT-1004 validated by CI. | 5 min |
| P3 | F-012 | Verify protocol doc sections and fix stale reference. | 5 min |
| P4 | F-008 | Add cross-reference note to PT-1100 listing covering tests. | 5 min |
| P4 | F-011 | Add PT-0801 verification to PT-1206 manual test checklist. | 5 min |
| P4 | F-013 | Reword T-PT-205 step 2 to "Complete a successful Phase 1 flow." | 2 min |
| P4 | F-014 | Add note to PT-0304/PT-0408 re structural verification methodology. | 5 min |
| P4 | F-015 | Add convention note for alphabetic suffixes or renumber. | 5 min |

**Total estimated effort:** ~1.5 hours for all remediations.

---

## 7. Prevention

To prevent similar issues in future specification trifectas:

1. **Require a "Validation scope" section** at the top of every validation doc
   that explicitly lists requirements validated by non-test means (build targets,
   code review, manual testing) with justification for each exclusion.

2. **Prohibit duplicated acceptance criteria** across requirements.  When
   multiple requirements touch the same behavior, one should own the criterion
   and others should cross-reference it (e.g., "per PT-0902").

3. **Automate traceability checks.**  A CI script should:
   - Extract all `PT-XXXX` IDs from the requirements doc.
   - Extract all `Validates: PT-XXXX` fields from the validation doc.
   - Assert every requirement ID appears in at least one test or in an explicit
     exclusion list.
   - Assert every `Validates:` target exists as a requirement ID.

4. **Include timeout constants in design trait documentation**, not just in
   implementation notes.  Protocol-level timing constraints should be visible
   at the API contract level.

5. **Use a single ID convention** for test cases.  Either allow alphabetic
   suffixes by convention or reserve gaps in the numbering scheme for addenda.

---

## 8. Open Questions

1. **Protocol doc section numbering:** Which is correct for Phase 2 HKDF —
   `ble-pairing-protocol.md §6.3` (per requirements) or `§6.4` (per design)?
   Resolving this requires inspecting the protocol doc, which is out of scope
   for this audit.

2. **Gateway Just Works fallback scope:** Does the gateway firmware actually
   support Just Works as a fallback?  If the modem only configures Numeric
   Comparison, the requirement in PT-0300 may be aspirational.  This needs
   confirmation from the `sonde-modem` BLE GATT implementation.

3. **Verbose mode scope:** Should PT-0702 (verbose diagnostic mode) be tested
   at the core crate level (tracing filter configuration) or only at the UI
   level (toggle button)?  The answer affects whether a CI test case is
   feasible.

4. **`Zeroizing` verification methodology:** Is structural verification (type
   signatures) sufficient for PT-0304/PT-0408, or should the project invest in
   a debug-mode memory scanner for CI?  This is a project-wide security testing
   policy question.

---

## 9. Revision History

| Version | Date | Author | Description |
|---|---|---|---|
| 1.0 | 2026-03-20 | Copilot (specification analyst) | Initial audit |
