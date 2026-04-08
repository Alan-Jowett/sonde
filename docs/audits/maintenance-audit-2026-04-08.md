<!-- SPDX-License-Identifier: MIT -->
<!-- Copyright (c) 2026 sonde contributors -->

# Maintenance Audit — 2026-04-08

## 1. Executive Summary

This audit applies the full D1–D13 drift detection taxonomy across all
Sonde components: Protocol, Gateway, Node, Modem, BLE Pairing, BPF
Interpreter, Bundle, and E2E. It succeeds the 2026-03-30 audit (32
findings, 27 resolved in PRs #667–#670).

**Key metrics:**

| Component    | Findings | Critical | High | Medium | Low |
|-------------|----------|----------|------|--------|-----|
| Protocol     | 3        | 0*       | 1    | 1      | 1   |
| Gateway      | 3        | 0        | 2    | 0      | 1   |
| Node         | 3        | 0        | 0    | 2      | 1   |
| Modem        | 9        | 0        | 1    | 5      | 3   |
| BLE Pairing  | 6        | 0        | 1    | 3      | 2   |
| BPF          | 2        | 0        | 2    | 0      | 0   |
| E2E/Bundle   | 0        | 0        | 0    | 0      | 0   |
| Hardware     | 1        | 0        | 0    | 0      | 1   |
| **Total**    | **27**   | **0**    | **7**| **11** | **9**|

*F-001 (PeerRequest/PeerAck missing from codec) was initially Critical but
reclassified to Low after investigation showed these messages intentionally
bypass the `NodeMessage`/`GatewayMessage` codec — they use the lower-level
AEAD frame codec directly.*

**Overall assessment:** The codebase is in good shape. The `firmware_version`
feature (PR #690) was correctly implemented across protocol, gateway, node,
and modem, but **test assertions lagged** — gateway tests construct the
field but don't assert its persistence. The BPF interpreter's `Context`
write-silencing behavior and `Memory` region tag need spec updates. Modem
BLE pairing state machine lacks unit test coverage.

**Previous audit residuals:** 27 of 32 findings resolved. F-028/F-029/F-030
silently fixed. F-031 (T-1304/T-1305 tests, now F-021) and F-032 (HW CI
workflow, now F-022) remain open and carried forward.

---

## 2. Investigation Scope

### Source documents consulted

| Document | Purpose |
|----------|---------|
| `docs/protocol-crate-design.md` | Protocol design (§6.1–6.3 PeerRequest/PeerAck) |
| `docs/protocol-crate-validation.md` | Protocol test case definitions |
| `docs/protocol.md` | Wire format reference |
| `docs/gateway-requirements.md` | Gateway REQ-IDs |
| `docs/gateway-design.md` | Gateway design |
| `docs/gateway-validation.md` | Gateway test cases (T-0103, T-0104) |
| `docs/node-requirements.md` | Node REQ-IDs (ND-0505, ND-0608, ND-1013) |
| `docs/node-design.md` | Node design |
| `docs/node-validation.md` | Node test cases |
| `docs/modem-requirements.md` | Modem REQ-IDs |
| `docs/modem-design.md` | Modem design |
| `docs/modem-validation.md` | Modem test cases |
| `docs/ble-pairing-tool-requirements.md` | BLE pairing REQ-IDs (PT-1215) |
| `docs/ble-pairing-tool-design.md` | BLE pairing design |
| `docs/ble-pairing-tool-validation.md` | BLE pairing test cases |
| `docs/ble-pairing-protocol.md` | BLE pairing protocol spec |
| `docs/safe-bpf-interpreter.md` | BPF interpreter design (§2.2, §3.2, §6) |
| `docs/safe-bpf-interpreter-validation.md` | BPF test cases |
| `docs/bpf-environment.md` | BPF helper/environment spec |
| `docs/bundle-format.md` | Bundle format spec |
| `docs/bundle-tool-design.md` | Bundle tool design |
| `docs/bundle-tool-validation.md` | Bundle test cases |
| `docs/e2e-validation.md` | E2E test cases |

### Crates examined

| Crate | Source dir | Test locations |
|-------|-----------|----------------|
| `sonde-protocol` | `crates/sonde-protocol/src/` | `crates/sonde-protocol/tests/` |
| `sonde-gateway` | `crates/sonde-gateway/src/` | `crates/sonde-gateway/tests/` |
| `sonde-node` | `crates/sonde-node/src/` | In-source `#[cfg(test)]` |
| `sonde-modem` | `crates/sonde-modem/src/` | `crates/sonde-modem/tests/` |
| `sonde-pair` | `crates/sonde-pair/src/` | In-source `#[cfg(test)]` |
| `sonde-pair-ui` | `crates/sonde-pair-ui/src-tauri/src/` | In-source |
| `sonde-bpf` | `crates/sonde-bpf/src/` | `crates/sonde-bpf/tests/` |
| `sonde-bundle` | `crates/sonde-bundle/src/` | In-source |
| `sonde-e2e` | `crates/sonde-e2e/tests/` | Integration tests |

### Method

- 6 parallel explore agents for spec-vs-code comparison
- `grep`/`glob`/`view` verification of all Critical/High findings
- Residual check of previous audit findings F-028 through F-032
- Additional deep-dive into PeerRequest/PeerAck data flow (reclassified F-001)

### Excluded

- Hardware (`hw/`): excluded except for residual CI finding (F-022)
- Handler crates (`sonde-tmp102-handler`, `sonde-sht40-handler`): external
  process handlers, no spec artifacts

---

## 3. Findings

### F-001 — PeerRequest/PeerAck message variants not in codec enums

- **Severity:** Low (reclassified from Critical)
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT` (intentional deferral)
- **Location:** `crates/sonde-protocol/src/messages.rs` ↔
  `docs/protocol-crate-design.md` §6.1–6.2
- **Description:** Design spec defines `NodeMessage::PeerRequest` and
  `GatewayMessage::PeerAck` enum variants. Constants (`MSG_PEER_REQUEST`
  = 0x05, `MSG_PEER_ACK` = 0x84) and CBOR keys are defined in
  `constants.rs`. However, the enum variants are absent from
  `messages.rs`. Investigation confirmed these messages **intentionally
  bypass** the `NodeMessage`/`GatewayMessage` decode path — the gateway
  processes them directly via `decode_frame()` (lower-level AEAD codec),
  and the node builds them via `encode_frame()` directly. The system works
  correctly without the high-level enum variants.
- **Evidence:** `grep PeerRequest messages.rs` → 0. Gateway `engine.rs`
  handles `MSG_PEER_REQUEST` via raw `msg_type` check. Node
  `peer_request.rs` builds frames with `encode_frame()` directly.
- **Disposition:** No action — intentional design. Previously documented
  in round-2 code compliance audit (F-004, F-005).
- **Confidence:** High
- **Tracking:** N/A

---

### F-002 — Gateway T-0103 test missing firmware_version assertion

- **Severity:** High
- **Category:** `D7_ACCEPTANCE_CRITERIA_MISMATCH`
- **Location:** `crates/sonde-gateway/tests/phase2b.rs:284-286` ↔
  `docs/gateway-validation.md:101-109`
- **Description:** T-0103 spec requires asserting `firmware_version` is
  updated in the node registry. Test constructs WAKE with
  `firmware_version: "0.4.0"` (line 68) but assertion block (lines
  284-286) only checks `firmware_abi_version` and `last_battery_mv`.
- **Evidence:** Missing assertion:
  `assert_eq!(updated.firmware_version, Some("0.4.0".into()))`.
- **Disposition:** Fix — add the missing assertion.
- **Confidence:** High
- **Tracking:** #691

---

### F-003 — Gateway T-0104 test conflates two missing-field variants

- **Severity:** High
- **Category:** `D12_UNTESTED_ACCEPTANCE_CRITERION`
- **Location:** `crates/sonde-gateway/tests/phase2b.rs:289-331` ↔
  `docs/gateway-validation.md:112-121`
- **Description:** T-0104 spec requires testing two rejection variants:
  WAKE missing `battery_mv` and WAKE missing `firmware_version`. Test
  builds CBOR missing BOTH fields, conflating the two cases.
- **Evidence:** `phase2b.rs:300-310` builds CBOR with only
  `KEY_FIRMWARE_ABI_VERSION` and `KEY_PROGRAM_HASH`.
- **Disposition:** Fix — split into two sub-tests.
- **Confidence:** High
- **Tracking:** #691

---

### F-004 — No encode/decode tests for PeerRequest/PeerAck

- **Severity:** Low (reclassified with F-001)
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `crates/sonde-protocol/tests/` ↔
  `docs/protocol-crate-validation.md`
- **Description:** No round-trip tests for PeerRequest/PeerAck. Follows
  from F-001 — these messages bypass the codec enum.
- **Disposition:** No action — deferred with F-001.
- **Confidence:** High
- **Tracking:** N/A

---

### F-005 — BPF Context write behavior contradicts spec

- **Severity:** High
- **Category:** `D5_CROSS_DOCUMENT_CONTRADICTION`
- **Location:** `crates/sonde-bpf/src/interpreter.rs:382-384` ↔
  `docs/safe-bpf-interpreter.md:175-177`
- **Description:** BPF spec §3.2 says Context writes return
  `Err(BpfError::ReadOnlyWrite)`. Code silently ignores them per ND-0505
  AC6. Code is correct; spec is stale.
- **Evidence:** Spec: `return Err(BpfError::ReadOnlyWrite { pc })`.
  Code: `return Ok(())`. Requirement ND-0505 AC6 mandates the code's
  behavior.
- **Disposition:** Fix — update BPF spec to match ND-0505 AC6.
- **Confidence:** High
- **Tracking:** #692

---

### F-006 — BPF RegionTag::Memory not in spec

- **Severity:** High
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-bpf/src/interpreter.rs:95-96` ↔
  `docs/safe-bpf-interpreter.md:63-72`
- **Description:** Code has `RegionTag::Memory` ("Writable input memory
  — same as Context but allows stores") not defined in spec §2.2. Spec
  line 589 treats mutable input regions as future work.
- **Evidence:** Code line 96: `Memory,`. Spec §2.2: only Stack, Context,
  MapValue, MapDescriptor.
- **Disposition:** Fix — add Memory to spec.
- **Confidence:** High
- **Tracking:** #692

---

### F-007 — Modem BLE pairing state machine lacks unit tests

- **Severity:** High
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `crates/sonde-modem/src/ble.rs` ↔
  `docs/modem-requirements.md` MD-0414, MD-0416
- **Description:** Pairing state transitions (authenticated flag,
  pre-auth write buffering, timeouts) only tested via real hardware.
  `NoBle` mock prevents BLE event injection in unit tests.
- **Disposition:** Fix — add mock BLE event injection.
- **Confidence:** High
- **Tracking:** #693

---

### F-008 — PT-1215 error diagnostic observability incomplete

- **Severity:** High
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `crates/sonde-pair/src/error.rs` ↔
  `docs/ble-pairing-tool-requirements.md` PT-1215
- **Description:** `PairingError` variants lack device context per
  PT-1215 AC1. `ConnectionFailed(String)` has no device address;
  `MtuTooLow` has no peer info.
- **Disposition:** Fix — add device context fields.
- **Confidence:** High
- **Tracking:** #694

---

### F-009 — Protocol validation test count stale

- **Severity:** Medium
- **Category:** `D11_STALE_DOCUMENTATION`
- **Location:** `docs/protocol-crate-validation.md`
- **Description:** 87 `### T-P` headers vs ~67 actual test
  implementations. Discrepancy makes coverage metrics unreliable.
- **Disposition:** Fix — reconcile.
- **Confidence:** Medium
- **Tracking:** #695

---

### F-010 — Modem MODEM_READY timing not validated in test

- **Severity:** Medium
- **Category:** `D7_ACCEPTANCE_CRITERIA_MISMATCH`
- **Location:** `crates/sonde-modem/tests/device_tests.rs:146` ↔
  `docs/modem-requirements.md` MD-0104
- **Description:** T-0101 waits for MODEM_READY but doesn't assert the
  2-second deadline.
- **Disposition:** Fix — add timing assertion.
- **Confidence:** High
- **Tracking:** #696

---

### F-011 — Modem pre-auth write buffer queue depth undocumented

- **Severity:** Medium
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-modem/src/ble.rs` ↔
  `docs/modem-requirements.md` MD-0409
- **Description:** `pending_write: Option<Vec<u8>>` (single slot).
  Second pre-auth writes silently replace the first. Not documented in
  requirements.
- **Disposition:** Fix — document in MD-0409.
- **Confidence:** High
- **Tracking:** #696

---

### F-012 — Modem BLE event queue drop undocumented in requirements

- **Severity:** Medium
- **Category:** `D3_ORPHAN_DESIGN_ELEMENT` / `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-modem/src/ble.rs:54` ↔
  `docs/modem-design.md` D9-3
- **Description:** `MAX_BLE_EVENT_QUEUE=32`. Drop behavior documented in
  design note D9-3 but no requirement or test covers it.
- **Disposition:** Fix — add requirement or acceptance criterion.
- **Confidence:** High
- **Tracking:** #696

---

### F-013 — Modem indication fragment limit untested

- **Severity:** Medium
- **Category:** `D2_UNTESTED_REQUIREMENT` / `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `crates/sonde-modem/src/ble.rs:57` ↔
  `docs/modem-validation.md` T-0605
- **Description:** `MAX_INDICATION_CHUNKS=64` boundary untested. T-0605
  tests fragmentation but not the rejection path at 64+1 chunks.
- **Disposition:** Fix — add boundary test.
- **Confidence:** High
- **Tracking:** #696

---

### F-014 — Modem tentative accept deferral lacks unit test

- **Severity:** Medium
- **Category:** `D7_ACCEPTANCE_CRITERIA_MISMATCH`
- **Location:** `crates/sonde-modem/src/bridge.rs` ↔
  `docs/modem-requirements.md` MD-0416 AC1
- **Description:** `BleEvent::Connected` deferral until operator accepts
  is untestable with current `NoBle` mock.
- **Disposition:** Fix — address jointly with F-007.
- **Confidence:** Medium
- **Tracking:** #693

---

### F-015 — BLE pairing: retired HMAC references in protocol doc

- **Severity:** Medium
- **Category:** `D5_CROSS_DOCUMENT_CONTRADICTION`
- **Location:** `docs/ble-pairing-protocol.md` ↔
  `docs/ble-pairing-tool-requirements.md` (PT-0404 RETIRED)
- **Description:** Vestigial HMAC references remain after PT-0404/0405
  retirement. Confusing for contributors.
- **Disposition:** Fix — clean up retired references.
- **Confidence:** High
- **Tracking:** #697

---

### F-016 — T-PT-1214e resolve_pin_config test missing

- **Severity:** Medium
- **Category:** `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `crates/sonde-pair-ui/src-tauri/src/lib.rs` ↔
  `docs/ble-pairing-tool-validation.md` T-PT-1214e
- **Description:** `resolve_pin_config(Some(5), None)` rejection case
  specified but not implemented as unit test.
- **Disposition:** Fix — add unit test.
- **Confidence:** High
- **Tracking:** #697

---

### F-017 — Pin config CBOR determinism not tested

- **Severity:** Medium
- **Category:** `D6_DESIGN_VIOLATES_CONSTRAINT`
- **Location:** `crates/sonde-pair/src/phase2.rs` ↔
  `docs/ble-pairing-tool-requirements.md` PT-1214 AC2
- **Description:** Pin config CBOR encoding is manually constructed
  correctly but no test verifies deterministic byte output.
- **Disposition:** Fix — add determinism test.
- **Confidence:** Medium
- **Tracking:** #697

---

### F-018 — Node WAKE retry language ambiguity

- **Severity:** Medium
- **Category:** `D5_CROSS_DOCUMENT_CONTRADICTION`
- **Location:** `docs/node-requirements.md` ND-0700 ↔
  `docs/node-validation.md` T-N700
- **Description:** ND-0700 AC1: "retries up to 3 times." T-N700 AC2:
  "sends exactly 4 WAKE frames (1 initial + 3 retries)." Consistent but
  ambiguous in isolation.
- **Disposition:** Fix — clarify wording.
- **Confidence:** Medium
- **Tracking:** #698

---

### F-019 — Node initial map data skip not logged

- **Severity:** Medium
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-node/src/map_storage.rs:523-534`
- **Description:** `apply_initial_data()` silently skips mismatched
  sizes per ND-0607 AC4. Adding `debug!()` would aid field debugging.
- **Disposition:** Fix — add debug log.
- **Confidence:** Low
- **Tracking:** #698

---

### F-020 — Modem USB disconnect frame drop incomplete

- **Severity:** Medium
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `crates/sonde-modem/src/usb_cdc.rs` ↔
  `docs/modem-requirements.md` MD-0301 AC3
- **Description:** Frame drop relies on reactive I/O-failure detection,
  which may lag physical disconnection. Small window where frames could
  be queued.
- **Disposition:** Fix — document as known limitation.
- **Confidence:** Medium
- **Tracking:** #696

---

### F-021 — Gateway T-1304/T-1305 tests still not implemented

- **Severity:** Low
- **Category:** `D11_UNIMPLEMENTED_TEST_CASE`
- **Location:** `docs/gateway-validation.md` (T-1304, T-1305a/b) ↔
  `crates/sonde-gateway/tests/`
- **Description:** Residual from previous audit F-031. Three validation
  tests remain unimplemented.
- **Disposition:** Fix — implement.
- **Confidence:** High
- **Tracking:** #699

---

### F-022 — Hardware CI workflow still missing

- **Severity:** Low
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `docs/hw-requirements.md` HW-0902 ↔ `.github/workflows/`
- **Description:** Residual from previous audit F-032. No CI workflow for
  hardware tool.
- **Disposition:** Fix — defer until tool matures.
- **Confidence:** High
- **Tracking:** #700

---

### F-023 — Node orphan test naming T-N1014/T-N1015

- **Severity:** Low
- **Category:** `D4_ORPHANED_TEST_CASE`
- **Location:** `docs/node-validation.md` §11
- **Description:** T-N1014/T-N1015 validate ND-1006/ND-1010 but use
  non-standard naming.
- **Disposition:** Fix — add cross-reference.
- **Confidence:** High
- **Tracking:** #698

---

### F-024 — BLE pairing: magic number timeouts

- **Severity:** Low
- **Category:** `D9_UNDOCUMENTED_BEHAVIOR`
- **Location:** `crates/sonde-pair/src/phase1.rs`,
  `crates/sonde-pair/src/phase2.rs`
- **Description:** Timeout values (30s, 5s) hard-coded as magic numbers
  instead of named constants traceable to PT-1002.
- **Disposition:** Fix — extract constants.
- **Confidence:** High
- **Tracking:** #697

---

### F-025 — PT-0601 already-paired detection not implemented

- **Severity:** Low
- **Category:** `D8_UNIMPLEMENTED_REQUIREMENT`
- **Location:** `crates/sonde-pair/src/phase1.rs` ↔
  `docs/ble-pairing-tool-requirements.md` PT-0601
- **Description:** Tool unconditionally generates new PSK without
  checking for existing pairing. PT-0601 is `Should` priority.
- **Disposition:** Fix — add pre-pairing check.
- **Confidence:** Medium
- **Tracking:** #697

---

### F-026 — Modem watchdog timer lacks unit test

- **Severity:** Low
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/modem-requirements.md` MD-0302 ↔
  `crates/sonde-modem/tests/`
- **Description:** T-0304 requires special test firmware. Cannot unit
  test hardware watchdog.
- **Disposition:** Document as hardware-only test.
- **Confidence:** Low
- **Tracking:** #696

---

### F-027 — Modem logging requirements lack systematic test coverage

- **Severity:** Low
- **Category:** `D2_UNTESTED_REQUIREMENT`
- **Location:** `docs/modem-requirements.md` MD-0500–MD-0506
- **Description:** Logging requirements (format, level, completeness)
  lack automated verification.
- **Disposition:** Consider `tracing-test` for representative subset.
- **Confidence:** Low
- **Tracking:** #696

---

## 4. Root Cause Analysis

### Pattern 1: Test assertion lag after feature additions (F-002, F-003)

PR #690 added `firmware_version` across the stack. The engine correctly
stores the field, but existing tests were not extended with assertions for
the new field. The test constructs the right payload but doesn't verify
the outcome. **Prevention:** PR checklist item — "All new fields are
asserted in existing tests."

### Pattern 2: Spec lag after behavioral changes (F-005, F-006)

The BPF interpreter evolved with Context write-silencing (per ND-0505
AC6) and a new `Memory` region tag, but the BPF spec document wasn't
updated. The code follows the requirements correctly, but the BPF spec
describes the old behavior. **Prevention:** Update spec documents in the
same PR that changes behavior.

### Pattern 3: Mock gaps preventing unit testing (F-007, F-014)

The modem's `NoBle` mock returns `None` from `drain_event()`, preventing
BLE state machine testing in CI. Device tests exist but require real
hardware. **Prevention:** Design mocks to support event injection from
the start.

### Pattern 4: Residual deferrals (F-021, F-022)

Two findings from the March 2026 audit remain open due to lower priority.
**Prevention:** Track residuals in issues and revisit each audit cycle.

---

## 5. Issue Tracking

| Issue | Findings | Component | Severity |
|-------|----------|-----------|----------|
| #691 | F-002, F-003 | Gateway | High |
| #692 | F-005, F-006 | BPF | High |
| #693 | F-007, F-014 | Modem | High + Medium |
| #694 | F-008 | BLE Pairing | High |
| #695 | F-009 | Protocol | Medium |
| #696 | F-010–F-013, F-020, F-026, F-027 | Modem | Medium + Low |
| #697 | F-015–F-017, F-024, F-025 | BLE Pairing | Medium + Low |
| #698 | F-018, F-019, F-023 | Node | Medium + Low |
| #699 | F-021 | Gateway | Low |
| #700 | F-022 | Hardware | Low |

---

## 6. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-04-08 | Copilot (maintenance audit) | Full audit — D1–D13 across 8 components. 27 findings (0 Critical after reclassification, 7 High, 11 Medium, 9 Low). 10 issues filed (#691–#700). 2 residuals carried from 2026-03-30 audit. |
