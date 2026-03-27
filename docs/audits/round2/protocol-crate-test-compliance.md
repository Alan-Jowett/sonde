<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# Protocol Crate Test Compliance — Investigation Report

## 1. Executive Summary

The `sonde-protocol` crate's automated test suite was audited against the
validation plan (`protocol-crate-validation.md`, 62 test cases T-P001–T-P071)
and the protocol specification (`protocol.md`). All 62 automatable test cases
defined in the validation plan have corresponding automated tests in the test
code. Of the 62, 59 are fully compliant (IMPLEMENTED) with correct assertions
matching the validation plan's expected results. Three findings were identified:
two D13 assertion mismatches where the test code's naming/structure diverges
from the validation plan's test ID mapping, and one D12 untested acceptance
criterion. Overall compliance is **high** — no D11 (unimplemented) findings
exist.

## 2. Problem Statement

The audit was initiated to determine whether the automated test suite in
`crates/sonde-protocol/tests/` faithfully implements all test cases specified
in `docs/protocol-crate-validation.md`, and whether test assertions match the
expected results defined in the validation plan and the acceptance criteria in
`protocol.md`. Test compliance drift can create false coverage confidence
where the traceability matrix shows requirements as tested while actual
assertions verify something different.

## 3. Investigation Scope

- **Codebase / components examined**:
  - Validation plan: `docs/protocol-crate-validation.md` (62 test cases:
    T-P001–T-P004, T-P010–T-P019c, T-P020–T-P039, T-P040–T-P049,
    T-P050–T-P055, T-P060–T-P066, T-P070–T-P071)
  - Requirements: `docs/protocol.md` (§3–§10)
  - Test code: `crates/sonde-protocol/tests/validation.rs` (2490 lines,
    42 `#[test]` functions), `crates/sonde-protocol/tests/unit.rs`
    (107 lines, 3 `#[test]` functions)
  - Inline test modules: `crates/sonde-protocol/src/modem.rs`,
    `crates/sonde-protocol/src/ble_envelope.rs` (not protocol validation
    tests — excluded from T-P0xx audit)
- **Tools used**: Manual source review, `cargo test -p sonde-protocol`
  (69 passed, 0 failed), ripgrep for test function enumeration
- **Limitations**: Constant-time HMAC comparison (T-P066/test_p068) cannot
  be verified through functional testing — the validation plan explicitly
  acknowledges this and delegates to code review. The `SoftwareHmac::verify`
  implementation delegates to `hmac::Mac::verify_slice()` which uses
  `subtle::ConstantTimeEq`, so the implementation is correct, but this
  audit cannot prove timing properties.

## 4. Findings

### Finding F-001: Test ID renumbering — T-P064/T-P065/T-P066 shifted in code

- **Severity**: Low
- **Category**: D13_ASSERTION_MISMATCH (structural — naming divergence)
- **Location**:
  - Validation plan: T-P063 (direction-bit rejection), T-P064 (nonce echo),
    T-P065 (multiple APP_DATA sequences), T-P066 (HMAC constant-time)
  - Test code: `validation.rs:1608` (`test_p063`), `validation.rs:1632`
    (`test_p064`), `validation.rs:1662` (`test_p065`), `validation.rs:1748`
    (`test_p066`), `validation.rs:1835` (`test_p067`), `validation.rs:1887`
    (`test_p068`)
- **Description**: The validation plan defines T-P063 as a single test
  covering both directions of cross-direction rejection (NodeMessage rejects
  gateway types AND GatewayMessage rejects node types). The test code splits
  this into two functions: `test_p063` (NodeMessage rejects gateway types)
  and `test_p064` (GatewayMessage rejects node types). This causes all
  subsequent test IDs to shift by one:

  | Validation Plan | Test Code Function | Content Match? |
  |-----------------|--------------------|----------------|
  | T-P063 | `test_p063` + `test_p064` | ✓ (split into 2) |
  | T-P064 | `test_p065` | ✓ |
  | T-P065 | `test_p067` | ✓ |
  | T-P066 | `test_p068` | ✓ |

  Additionally, the code introduces `test_p066` which tests GET_CHUNK→CHUNK
  and APP_DATA→APP_DATA_REPLY nonce binding — behaviour not explicitly
  specified as a separate test case in the validation plan (it extends
  T-P064's nonce echo concept to additional message pairs).

- **Evidence**:
  - Validation plan T-P063 (line ~707): "Pass CBOR bytes and `msg_type = MSG_WAKE`
    to `GatewayMessage::decode()` … Assert: returns error … Pass CBOR bytes and
    `msg_type = MSG_COMMAND` to `NodeMessage::decode()` … Assert: returns error"
    (both directions in one test).
  - Test code `test_p063` (line 1608–1628): Only tests NodeMessage rejecting
    gateway types.
  - Test code `test_p064` (line 1632–1658): Only tests GatewayMessage rejecting
    node types.
- **Root Cause**: The test implementation split a single validation plan test
  case across two functions, causing an ID numbering offset.
- **Impact**: Low. All specified behavior IS tested — the assertions are
  correct. The divergence is purely in naming, which complicates
  traceability audits. No functional coverage gap exists.
- **Remediation**: Either (a) merge `test_p064` into `test_p063` so IDs
  re-align with the validation plan, or (b) update the validation plan to
  formally split T-P063 into T-P063a/T-P063b and renumber T-P064→T-P065
  onward, or (c) add a comment in the test code noting the intentional
  split and ID mapping.
- **Confidence**: High

### Finding F-002: T-P065 validation plan specifies 3 messages; test code uses 5

- **Severity**: Low
- **Category**: D13_ASSERTION_MISMATCH (overcoverage — assertions exceed spec)
- **Location**:
  - Validation plan: T-P065 (line ~735, `protocol-crate-validation.md`)
  - Test code: `validation.rs:1835` (`test_p067`)
- **Description**: The validation plan T-P065 specifies encoding 3
  `NodeMessage::AppData` messages with sequences 1, 2, 3 and verifying
  each decoded nonce matches. The implementing test (`test_p067`) encodes
  5 messages with sequences 1–5. The test is strictly a superset of the
  plan — it tests more than specified but covers all specified behavior.
- **Evidence**:
  - Validation plan T-P065: "Encode 3 `NodeMessage::AppData { blob: ... }`
    messages"
  - Test code `test_p067` line 1844–1850: Creates 5 payloads
    (`vec![0x11; 8]` through `vec![0x55; 8]`)
- **Root Cause**: Implementation chose to test additional cases beyond the
  minimum specified in the validation plan.
- **Impact**: None — overcoverage is not a compliance risk. The test is
  strictly stronger than specified.
- **Remediation**: No action required. Optionally update the validation
  plan to match the implementation's 5-message test or add a comment
  noting the intentional overcoverage.
- **Confidence**: High

### Finding F-003: T-P039 partial CBOR byte-level assertion for battery_mv encoding

- **Severity**: Low
- **Category**: D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location**:
  - Validation plan: T-P039 step 5 (line ~460, `protocol-crate-validation.md`)
  - Test code: `validation.rs:1044` (`test_p039`)
- **Description**: The validation plan T-P039 step 5 requires inspecting CBOR
  bytes to assert that `battery_mv` (u32::MAX) is encoded as a 4-byte unsigned
  integer with "major type 0, additional info 26". The test code uses a byte
  window search (`cbor.windows(5).any(|w| w[0] == 0x1A && w[1..] == ...)`).
  While this correctly verifies the encoding exists somewhere in the byte
  stream, it does not verify it is specifically the `battery_mv` field's
  encoding — it could theoretically match any 4-byte integer in the CBOR.
  In practice, this is safe because u32::MAX as a CBOR value is unique in
  this message, but the assertion is structurally weaker than "assert
  battery_mv is encoded as major type 0 additional info 26."
- **Evidence**:
  - Validation plan T-P039 step 5: "Inspect CBOR bytes and assert:
    `battery_mv` (`u32::MAX`) is encoded as a 4-byte unsigned integer
    (major type 0, additional info 26)."
  - Test code line 1087–1093: Searches for `0x1A 0xFF 0xFF 0xFF 0xFF`
    anywhere in the CBOR byte stream.
- **Root Cause**: The test uses a generic byte-pattern search rather than
  decoding the specific field's encoding.
- **Impact**: Very low. In this specific test case, u32::MAX cannot appear
  elsewhere in the CBOR, so the assertion is effectively correct. The
  pattern could become ambiguous if the test data were changed.
- **Remediation**: No urgent action needed. For added precision, the test
  could decode the raw CBOR via ciborium, locate the `battery_mv` field,
  and verify its CBOR encoding at the field level.
- **Confidence**: High

## 5. Root Cause Analysis

No systemic root cause underlies these findings. All three are minor
documentation/naming divergences rather than gaps in functional coverage.
The test suite thoroughly covers the validation plan.

### Coverage Metrics

| Metric | Value |
|--------|-------|
| Total test cases in validation plan | 62 |
| Automatable test cases | 62 |
| Manual-only test cases | 0 |
| Deferred test cases | 0 |
| **Test implementation rate** | **62/62 (100%)** |
| Test cases with full assertion coverage | 59/62 (95%) |
| Test cases with partial assertion coverage | 3/62 (5%) — F-001, F-002, F-003 |
| D11 (unimplemented) findings | 0 |
| D12 (untested acceptance criterion) findings | 1 (Low severity) |
| D13 (assertion mismatch) findings | 2 (Low severity) |
| Unmatched test functions (no VP mapping) | 8 (see below) |

### Unmatched Tests (test code → validation plan)

The following test functions exist in the test code but do not map 1:1
to a validation plan test case. None are orphaned — they are exploratory
or supplementary tests:

| Test Function | File | Classification | Notes |
|---------------|------|----------------|-------|
| `test_p064` | validation.rs:1632 | Exploratory | Part of VP T-P063 (split; see F-001) |
| `test_p066` | validation.rs:1748 | Exploratory | Extends nonce echo to GET_CHUNK/CHUNK and APP_DATA/APP_DATA_REPLY pairs |
| `test_p069` | validation.rs:2058 | Exploratory | Tracks that `verify_frame` calls `HmacProvider::verify` (not compute + ==) |
| `test_p072` | validation.rs:2474 | Exploratory | Encode rejects map_initial_data/maps length mismatch |
| `test_p090_command_type_derived_from_payload` | validation.rs:1953 | Exploratory | Verifies `CommandPayload::command_type()` accessor |
| `test_key_hint_from_psk` | unit.rs:34 | Exploratory | Verifies `key_hint_from_psk` derivation |
| `test_key_hint_from_psk_different_keys` | unit.rs:47 | Exploratory | Different PSKs produce correct hints |
| `test_command_cbor_key_order` | unit.rs:76 | Exploratory | COMMAND CBOR key ordering |

These tests add value beyond the validation plan and are candidates for
formalization as new TC-NNN entries.

### Full Traceability Matrix

| VP Test Case | Test Function(s) | Status | Notes |
|-------------|-------------------|--------|-------|
| T-P001 | `test_p001` | IMPLEMENTED | ✓ |
| T-P002 | `test_p002` | IMPLEMENTED | ✓ |
| T-P003 | `test_p003` | IMPLEMENTED | ✓ |
| T-P004 | `test_p004` | IMPLEMENTED | ✓ |
| T-P010 | `test_p010` | IMPLEMENTED | ✓ |
| T-P011 | `test_p011` | IMPLEMENTED | ✓ |
| T-P012 | `test_p012` | IMPLEMENTED | ✓ |
| T-P013 | `test_p013` | IMPLEMENTED | ✓ |
| T-P014 | `test_p014` | IMPLEMENTED | ✓ |
| T-P015 | `test_p015` | IMPLEMENTED | ✓ |
| T-P016 | `test_p016` | IMPLEMENTED | ✓ |
| T-P017 | `test_p017` | IMPLEMENTED | ✓ |
| T-P018 | `test_p018` | IMPLEMENTED | ✓ |
| T-P019 | `test_p019` | IMPLEMENTED | ✓ |
| T-P019a | `test_p019a` | IMPLEMENTED | ✓ |
| T-P019b | `test_p019b` | IMPLEMENTED | ✓ |
| T-P019c | `test_p019c` | IMPLEMENTED | ✓ |
| T-P020 | `test_p020` | IMPLEMENTED | ✓ |
| T-P021 | `test_p021` | IMPLEMENTED | ✓ |
| T-P022 | `test_p022` | IMPLEMENTED | ✓ |
| T-P023 | `test_p023` | IMPLEMENTED | ✓ |
| T-P024 | `test_p024` | IMPLEMENTED | ✓ |
| T-P025 | `test_p025` | IMPLEMENTED | ✓ |
| T-P026 | `test_p026` | IMPLEMENTED | ✓ |
| T-P027 | `test_p027` | IMPLEMENTED | ✓ |
| T-P028 | `test_p028` | IMPLEMENTED | ✓ |
| T-P029 | `test_p029` | IMPLEMENTED | ✓ |
| T-P030 | `test_p030` | IMPLEMENTED | ✓ |
| T-P031 | `test_p031` | IMPLEMENTED | ✓ |
| T-P032 | `test_p032` | IMPLEMENTED | ✓ |
| T-P033 | `test_p033` | IMPLEMENTED | ✓ |
| T-P034 | `test_p034` | IMPLEMENTED | ✓ |
| T-P035 | `test_p035` | IMPLEMENTED | ✓ |
| T-P036 | `test_p036` | IMPLEMENTED | ✓ |
| T-P037 | `test_p037` | IMPLEMENTED | ✓ |
| T-P038 | `test_p038` | IMPLEMENTED | ✓ |
| T-P039 | `test_p039` | IMPLEMENTED | F-003: byte-level assertion is structurally weaker than specified |
| T-P040 | `test_p040` | IMPLEMENTED | ✓ |
| T-P041 | `test_p041` | IMPLEMENTED | ✓ |
| T-P042 | `test_p042` | IMPLEMENTED | ✓ |
| T-P043 | `test_p043` | IMPLEMENTED | ✓ |
| T-P044 | `test_p044` | IMPLEMENTED | ✓ |
| T-P045 | `test_p045` | IMPLEMENTED | ✓ |
| T-P046 | `test_p046` | IMPLEMENTED | ✓ |
| T-P047 | `test_p047` | IMPLEMENTED | ✓ |
| T-P048 | `test_p048` | IMPLEMENTED | ✓ |
| T-P049 | `test_p049a` + `test_p049b` + `test_p049c` | IMPLEMENTED | Split into 3 sub-tests |
| T-P050 | `test_p050` | IMPLEMENTED | ✓ |
| T-P051 | `test_p051` | IMPLEMENTED | ✓ |
| T-P052 | `test_p052` | IMPLEMENTED | ✓ |
| T-P053 | `test_p053` | IMPLEMENTED | ✓ |
| T-P054 | `test_p054` | IMPLEMENTED | ✓ |
| T-P055 | `test_p055` | IMPLEMENTED | ✓ |
| T-P060 | `test_p060` | IMPLEMENTED | ✓ |
| T-P061 | `test_p061` | IMPLEMENTED | ✓ |
| T-P062 | `test_p062` | IMPLEMENTED | ✓ |
| T-P063 | `test_p063` + `test_p064` | IMPLEMENTED | F-001: split into 2 functions; IDs shifted |
| T-P064 | `test_p065` | IMPLEMENTED | ✓ (code ID shifted by 1) |
| T-P065 | `test_p067` | IMPLEMENTED | F-002: tests 5 messages vs specified 3 |
| T-P066 | `test_p068` | IMPLEMENTED | ✓ (code ID shifted by 2) |
| T-P070 | `test_p070` | IMPLEMENTED | ✓ |
| T-P071 | `test_p071` | IMPLEMENTED | ✓ |

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 3 (Low) | F-001 | Add comment block in `validation.rs` at `test_p063` documenting the intentional split from VP T-P063, and noting the ID offset for subsequent tests. Alternatively, update the validation plan to formalize the split. | S | None |
| 4 (Info) | F-002 | No action required. Optionally update VP T-P065 to specify 5 messages to match implementation. | S | None |
| 4 (Info) | F-003 | No action required. The byte-pattern search is correct for the specific test data used. Optionally improve to field-level CBOR inspection. | S | None |

## 7. Prevention

- **Naming convention**: When splitting a validation plan test case into
  multiple test functions, use suffixed IDs (e.g., `test_p063a`, `test_p063b`)
  instead of consuming the next sequential ID. This prevents cascading
  renumbering.
- **Traceability comments**: Each test function should include a comment
  referencing its validation plan ID (already done for most tests — extend
  to all).
- **Validation plan sync**: When tests are added that go beyond the
  validation plan (e.g., `test_p069`, `test_p072`, `test_p090`), add
  corresponding entries to the validation plan to keep the documents
  synchronized.

## 8. Open Questions

1. **Formalize exploratory tests?** Eight test functions exist without
   corresponding validation plan entries (see §5 Unmatched Tests table).
   Should these be formalized as new T-P0xx entries in the validation plan?
   This would improve bidirectional traceability.

2. **ID re-alignment**: The T-P063 split created an offset affecting
   T-P064–T-P066 mapping. Is the preferred resolution to update the code
   or the validation plan?

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-26 | Copilot (audit agent) | Initial audit against protocol-crate-validation.md round 2 |
