# Sonde Code Compliance Audit — Investigation Report

**Crate:** `sonde-node`
**Date:** 2025-07-13

---

## 1. Executive Summary

The `sonde-node` crate was audited against its 55 requirements (ND-0100–ND-0918), the node design document, and the node validation plan. Of 53 applicable "Must" requirements, **49 are fully implemented** with traceable code evidence. The audit found **1 constraint violation** (D10) where self-healing logic narrows ND-0915 by adding an undocumented precondition, **2 partially-unimplemented requirements** (D8), and **7 undocumented behaviors** (D9) — primarily defense-in-depth constants and clamping logic that exist in the code but lack specification backing. Overall code-to-spec alignment is strong (92% full implementation), but the undocumented caps on BPF helper parameters and silent clamping behaviors should be formalized in the requirements to close traceability gaps.

---

## 2. Problem Statement

This audit was initiated to verify backward traceability (Code → Specification) of the `sonde-node` crate. The primary concern is discovering code behavior not accounted for by the requirements or design documents. Forward traceability (Spec → Code) and constraint compliance are also checked.

**Impact of gaps:** Undocumented behavior in firmware is a security and reliability risk — code that isn't specified is also untested against any acceptance criteria, making regressions undetectable.

---

## 3. Investigation Scope

- **Codebase / components examined:**
  - `crates/sonde-node/src/` — all 20 source files (~4,800 lines)
  - `crates/sonde-node/Cargo.toml` — dependencies and features
  - `crates/sonde-node/sdkconfig.defaults` — ESP-IDF configuration (65 lines)
  - Key files: `wake_cycle.rs`, `bpf_dispatch.rs`, `bpf_helpers.rs`, `sonde_bpf_adapter.rs`, `map_storage.rs`, `program_store.rs`, `ble_pairing.rs`, `peer_request.rs`, `key_store.rs`, `sleep.rs`, `crypto.rs`, `traits.rs`, `error.rs`, `hal.rs`, `lib.rs`, `bin/node.rs`, and ESP-specific modules
- **Specification documents:**
  - `docs/node-requirements.md` — 55 requirements (53 Must, 2 Should)
  - `docs/node-design.md` — architecture, module design, API contracts
  - `docs/node-validation.md` — test case catalog (T-N100–T-N929+)
- **Tools used:** Static analysis via grep/glob pattern search, manual code review, cross-reference tracing
- **Limitations:**
  - ESP-specific modules (`esp_ble_pairing.rs`, `esp_hal.rs`, `esp_sleep.rs`, `esp_storage.rs`, `esp_transport.rs`) were reviewed for interface compliance but not for low-level hardware correctness (no hardware-in-the-loop verification).
  - Runtime behavior (timing accuracy, memory consumption) was not verified — this is static analysis only.
  - The `sonde-bpf` crate (BPF interpreter backend) was not audited; its compliance is assumed based on the adapter layer.

---

## 4. Findings

### Finding F-001: Self-healing adds undocumented precondition to ND-0915

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Spec Location**: ND-0915 (node-requirements.md line 926): "If WAKE fails after `reg_complete` is set, the node MUST clear the `reg_complete` flag"
- **Code Location**: `crates/sonde-node/src/wake_cycle.rs:197`
- **Description**: ND-0915 states unconditionally that a WAKE failure when `reg_complete` is set MUST clear the flag. The code adds a second condition — `storage.has_peer_payload()` — so `reg_complete` is only cleared when `peer_payload` is still present. If `peer_payload` was erased after a prior successful WAKE/COMMAND (per ND-0914), subsequent transient WAKE failures do NOT trigger self-healing.
- **Evidence**:
  - Spec (line 926): "If WAKE fails … after `reg_complete` is set, the node MUST clear the `reg_complete` flag"
  - Code (line 197): `if storage.read_reg_complete() && storage.has_peer_payload() {`
  - The design doc (line 625) also states the simpler condition without the `has_peer_payload` guard.
- **Impact**: Without the extra guard, clearing `reg_complete` after `peer_payload` erasure would put the node in the PEER_REQUEST path with no payload — an infinite no-op retry loop. The code is arguably *more correct* than the spec, but violates the literal "MUST" wording.
- **Remediation**: Update ND-0915 acceptance criteria to add: "Self-healing applies only while `peer_payload` is still present. After `peer_payload` has been erased (ND-0914), transient WAKE failures do not revert to the PEER_REQUEST path." Alternatively, update the design doc to document this refinement.
- **Confidence**: High — code path verified at line 197, spec text verified at line 926.

---

### Finding F-002: `DEFAULT_INSTRUCTION_BUDGET` (100,000) has no spec-level value

- **Severity**: Medium
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0605 (line 557) says "an instruction budget" but does not specify the numeric value.
  Searched: ND-0605 acceptance criteria, node-design.md §12, node-validation.md test cases.
- **Code Location**: `crates/sonde-node/src/wake_cycle.rs:33`
- **Description**: The firmware uses `DEFAULT_INSTRUCTION_BUDGET = 100_000` as the BPF execution cap. ND-0605 requires "an instruction budget" and that programs exceeding it are terminated, but never specifies what the budget value is. The design doc (line 332) mentions "bounded mode" but also omits the concrete number.
- **Evidence**: `const DEFAULT_INSTRUCTION_BUDGET: u64 = 100_000;` (wake_cycle.rs:33). Applied at line 448: `interpreter.execute(ctx_ptr, DEFAULT_INSTRUCTION_BUDGET)`.
- **Impact**: BPF program authors have no spec to reference for maximum program complexity. A firmware update could silently change this value, breaking existing programs.
- **Remediation**: Add the instruction budget value to ND-0605 acceptance criteria or a new ND requirement: "The default instruction budget MUST be at least 100,000 instructions."
- **Confidence**: High

---

### Finding F-003: `MAX_SEND_RECV_TIMEOUT_MS` (5,000 ms) silent clamping is unspecified

- **Severity**: Medium
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0602 and ND-0702 specify the *default* response timeout (50 ms) but not a maximum user-provided timeout for `send_recv()`.
  Searched: ND-0602, ND-0604, ND-0702, node-design.md §12.
- **Code Location**: `crates/sonde-node/src/bpf_dispatch.rs:40,443-447`
- **Description**: When a BPF program calls `send_recv()` with a non-zero r5 (timeout), the firmware silently clamps it to `MAX_SEND_RECV_TIMEOUT_MS = 5000` ms. No error is returned for values above 5 seconds — the request succeeds with a shorter timeout than requested. This differs from `delay_us()` which returns an error for values above its cap.
- **Evidence**:
  ```rust
  const MAX_SEND_RECV_TIMEOUT_MS: u32 = 5000; // line 40
  let timeout_ms = if r5 == 0 {
      SEND_RECV_TIMEOUT_MS        // default 50ms
  } else {
      (r5 as u32).min(MAX_SEND_RECV_TIMEOUT_MS)  // line 446: silent clamp
  };
  ```
- **Impact**: BPF programs may silently receive shorter timeouts than requested, causing unexpected `send_recv()` failures. The asymmetry with `delay_us()` error-return behavior creates an inconsistent helper API contract.
- **Remediation**: Either (a) add a requirement specifying the maximum `send_recv` timeout and the clamping behavior, or (b) return an error (like `delay_us`) when the requested timeout exceeds the cap.
- **Confidence**: High

---

### Finding F-004: `MAX_BUS_TRANSFER_LEN` (4,096 bytes) is unspecified

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0601 describes bus helpers but does not constrain transfer sizes.
  Searched: ND-0601 acceptance criteria, node-design.md §12.
- **Code Location**: `crates/sonde-node/src/bpf_dispatch.rs:37,281,301,321-326,347`
- **Description**: All I2C and SPI helpers cap individual transfers at `MAX_BUS_TRANSFER_LEN = 4096` bytes. Requests exceeding this return -1. This is documented in code as "defence-in-depth" but has no requirements backing.
- **Evidence**: `const MAX_BUS_TRANSFER_LEN: usize = 4096;` (line 37). Checked in `helper_i2c_read` (line 281), `helper_i2c_write` (line 301), `helper_i2c_write_read` (lines 321-326), `helper_spi_transfer` (line 347).
- **Impact**: Low — this is reasonable defense-in-depth. BPF programs attempting transfers > 4 KB would get silent failures with no spec to consult for the limit.
- **Remediation**: Add the cap to ND-0601 acceptance criteria: "Bus helpers MUST reject transfers exceeding 4096 bytes."
- **Confidence**: High

---

### Finding F-005: `MAX_TRACE_ENTRIES` (64) silent drop is unspecified

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0604 AC6 says "`bpf_trace_printk()` emits a debug trace message" but does not constrain buffer size or overflow behavior.
  Searched: ND-0604, node-design.md §12.
- **Code Location**: `crates/sonde-node/src/bpf_dispatch.rs:141,618-621`
- **Description**: The trace log buffer is capped at 64 entries. After 64 `bpf_trace_printk()` calls, subsequent entries are silently dropped — no error is returned (the helper returns 0 = success). This creates a data-loss scenario invisible to the BPF program.
- **Evidence**:
  ```rust
  const MAX_TRACE_ENTRIES: usize = 64;  // line 141
  if log.len() < MAX_TRACE_ENTRIES {
      log.push(s.to_string());           // line 620: accepted
  }
  // else: silently dropped, returns 0
  ```
- **Impact**: BPF programs with heavy tracing silently lose debug output with no indication. Developers may not realize their trace output is incomplete.
- **Remediation**: Add to ND-0604: "The firmware MAY limit trace log entries per execution to a documented maximum (at least 64). Entries beyond the limit are silently dropped."
- **Confidence**: High

---

### Finding F-006: `MAX_DELAY_US` specific value (1,000,000 µs) is unspecified

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0604 AC3 says "the firmware enforces a maximum delay value" but does not state what that value is.
  Searched: ND-0604 (line 544), node-design.md.
- **Code Location**: `crates/sonde-node/src/bpf_dispatch.rs:43,571`
- **Description**: `delay_us()` enforces `MAX_DELAY_US = 1_000_000` (1 second) and returns -1 for larger values. The spec delegates the value to firmware without specifying it.
- **Evidence**: `const MAX_DELAY_US: u32 = 1_000_000;` (line 43). Check at line 571: `if r1 > MAX_DELAY_US as u64 { return (-1i64) as u64; }`.
- **Impact**: Low — the spec intentionally leaves this to firmware. However, BPF program authors need to know the actual cap.
- **Remediation**: Add the concrete value to ND-0604: "The maximum delay value MUST be at least 1,000,000 µs (1 second)."
- **Confidence**: High

---

### Finding F-007: NVS write rollback semantics in `handle_node_provision` are unspecified

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0908 AC2 says "No partial credentials remain in NVS after write failure" but does not specify the rollback strategy.
  Searched: ND-0905, ND-0906, ND-0908, node-design.md §15.
- **Code Location**: `crates/sonde-node/src/ble_pairing.rs:192-226`
- **Description**: `handle_node_provision()` implements a manual cascading rollback: if `write_peer_payload` fails, it erases `key`; if `write_reg_complete` fails, it erases both `key` and `peer_payload`; if `write_channel` fails, it erases both. Channel is written last to prevent stale-channel leakage. None of this ordering or rollback strategy is specified.
- **Evidence**:
  ```rust
  // Line 205: rollback on peer_payload failure
  let _ = storage.erase_key();
  // Lines 212-213: rollback on reg_complete failure
  let _ = storage.erase_key();
  let _ = storage.erase_peer_payload();
  // Lines 221-222: rollback on channel failure
  let _ = storage.erase_key();
  let _ = storage.erase_peer_payload();
  ```
- **Impact**: The rollback order is critical for security (preventing partial credential leakage). Without spec documentation, a code refactor could inadvertently weaken the rollback guarantees.
- **Remediation**: Add to ND-0908 or ND-0906: "The provisioning handler MUST write credentials in a specific order with cascading rollback: PSK first, then peer_payload, then reg_complete, then channel (last). On any failure, all previously-written credentials in that sequence MUST be erased."
- **Confidence**: High

---

### Finding F-008: Early-wake flag preservation on RNG health check failure is unspecified

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — ND-0304 AC3 mentions health-check failure but not its interaction with early-wake flag preservation.
  Searched: ND-0304, ND-0203, node-design.md §5.
- **Code Location**: `crates/sonde-node/src/wake_cycle.rs:91-100`
- **Description**: When the RNG health check fails, the wake cycle aborts early. The code deliberately does NOT call `determine_wake_reason()` (which would consume the early-wake flag) so the flag is preserved for the next boot. This is a self-healing pattern that prevents a transient RNG failure from losing a BPF-requested early wake.
- **Evidence**: Lines 91-100: The RNG failure path returns `Sleep` without calling `determine_wake_reason()`, preserving the RTC `EARLY_WAKE_FLAG` for the next cycle.
- **Impact**: Low — this is a reasonable robustness measure. Without documentation, a refactor could inadvertently consume the flag before the RNG check.
- **Remediation**: Add a note to ND-0304: "If the hardware RNG health check fails, the node MUST preserve any pending early-wake flag for the next boot cycle."
- **Confidence**: High

---

### Finding F-009: ND-0403 (Secure Boot) not implemented

- **Severity**: Low
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: ND-0403 (node-requirements.md line 302), Priority: Should
- **Code Location**: `crates/sonde-node/sdkconfig.defaults` — no `CONFIG_SECURE_BOOT*` entries
- **Description**: ND-0403 specifies that the node SHOULD support secure boot. The `sdkconfig.defaults` file does not enable any secure boot configuration. No code in the crate references secure boot APIs.
- **Evidence**: `sdkconfig.defaults` contains 65 lines; grep for `secure_boot` and `SECURE_BOOT` returns no results.
- **Impact**: Low — this is a "Should" priority requirement, not "Must". However, without secure boot, a compromised firmware could extract the PSK from flash.
- **Remediation**: Enable `CONFIG_SECURE_BOOT_V2_ENABLED=y` in sdkconfig.defaults when production hardware supports it. Document the gap in a deployment checklist.
- **Confidence**: High

---

### Finding F-010: ND-0403a (Flash Encryption) not implemented

- **Severity**: Low
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: ND-0403a (node-requirements.md line 317), Priority: Should
- **Code Location**: `crates/sonde-node/sdkconfig.defaults` — no `CONFIG_FLASH_ENCRYPTION*` entries
- **Description**: ND-0403a specifies that the node SHOULD support flash encryption. The `sdkconfig.defaults` file does not enable flash encryption. No code references flash encryption APIs.
- **Evidence**: `sdkconfig.defaults` contains 65 lines; grep for `flash_encrypt` and `FLASH_ENCRYPTION` returns no results.
- **Impact**: Low — this is a "Should" priority requirement. Without flash encryption, PSK could be extracted from flash via physical access.
- **Remediation**: Enable `CONFIG_FLASH_ENCRYPTION_ENABLED=y` in sdkconfig.defaults when production hardware supports it. Pair with secure boot (F-009) for defense-in-depth.
- **Confidence**: High

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|--------|-------|
| **Total requirements** | 55 (53 Must, 2 Should) |
| **Fully implemented (IMPLEMENTED)** | 49 of 53 Must (92.5%) |
| **Partially implemented** | 2 of 53 Must — ND-0915 has extra condition (F-001); ND-0605 lacks numeric value (F-002) |
| **Not implemented (D8)** | 2 of 2 Should — ND-0403, ND-0403a (F-009, F-010) |
| **Undocumented behavior count (D9)** | 7 findings (F-002 through F-008) |
| **Constraint violations (D10)** | 1 finding (F-001) |
| **Constraints verified** | 15 of 16 explicitly checked constraints pass |
| **Constraints violated** | 1 — ND-0915 self-healing precondition (F-001) |
| **Constraints unverifiable** | 0 from static analysis |

### Requirements Implementation Summary

| Section | REQ-IDs | Count | Status |
|---------|---------|-------|--------|
| Protocol/Communication | ND-0100–0103 | 4 | ✅ All implemented |
| Wake Cycle | ND-0200–0203 | 4 | ✅ All implemented |
| Authentication/Replay | ND-0300–0304 | 5 | ✅ All implemented |
| Key Storage | ND-0400, 0402 | 2 | ✅ All implemented |
| Key Storage (Should) | ND-0403, 0403a | 2 | ⚠️ Not implemented (Should priority) |
| Program Transfer/Exec | ND-0500–0506 | 7 | ✅ All implemented |
| BPF Environment | ND-0600–0606 | 7 | ✅ All implemented (budget value unspecified) |
| Timing/Retries | ND-0700–0702 | 3 | ✅ All implemented |
| Error Handling | ND-0800–0802 | 3 | ✅ All implemented |
| BLE Pairing | ND-0900–0918 | 19 | ✅ All Must implemented; ND-0915 has deviation (F-001) |

### Overall Assessment

The `sonde-node` crate demonstrates strong code-to-spec alignment. The implementation is well-structured with platform abstraction traits, comprehensive error handling, and defense-in-depth patterns. The 7 D9 findings are primarily constants and clamping logic that represent *reasonable engineering decisions* not yet captured in the spec — they are traceability gaps rather than defects. The single D10 finding (F-001) is a case where the implementation is arguably more correct than the spec, but the deviation from the literal "MUST" wording needs to be resolved by updating the requirement.

---

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Update ND-0915 to add `peer_payload` precondition for self-healing | S | Low — spec update only; code is correct |
| 2 | F-002 | Add instruction budget value (≥100,000) to ND-0605 | S | Low — spec update only |
| 3 | F-003 | Add max `send_recv` timeout spec or change to error-return behavior | S | Medium — if behavior changes, BPF programs may be affected |
| 4 | F-007 | Document NVS rollback ordering in ND-0908 | S | Low — spec update only |
| 5 | F-004 | Add `MAX_BUS_TRANSFER_LEN` to ND-0601 | S | Low — spec update only |
| 6 | F-005 | Add trace log cap to ND-0604 | S | Low — spec update only |
| 7 | F-006 | Add `MAX_DELAY_US` concrete value to ND-0604 | S | Low — spec update only |
| 8 | F-008 | Document early-wake flag preservation on RNG failure | S | Low — spec update only |
| 9 | F-009 | Enable secure boot in production sdkconfig | M | Medium — requires hardware support, key management |
| 10 | F-010 | Enable flash encryption in production sdkconfig | M | Medium — requires hardware support, one-time fuse burn |

---

## 7. Prevention

1. **Spec-first constant definition**: When adding defense-in-depth caps (like `MAX_BUS_TRANSFER_LEN`), create a corresponding requirement or design-doc entry in the same PR. This prevents accumulation of undocumented firmware behavior.

2. **Traceability tags in code**: The crate already uses `// ND-0xxx` comments extensively — extend this practice to all behavioral constants. Every `const` that affects BPF program behavior should cite its requirement source or be tagged `// [DESIGN-CHOICE: no requirement]`.

3. **CI-enforced traceability check**: A lint script could grep for `const.*=` in `bpf_dispatch.rs` and `wake_cycle.rs` and verify each has a corresponding `ND-` or `[DESIGN-CHOICE]` tag.

4. **BPF Helper API documentation**: Publish a "BPF Helper Reference" derived from the crate's constants and behavior, so BPF program authors know the actual caps (`MAX_DELAY_US`, `MAX_SEND_RECV_TIMEOUT_MS`, `MAX_BUS_TRANSFER_LEN`, `MAX_TRACE_ENTRIES`).

5. **Production sdkconfig checklist**: Before any production deployment, require a review of `sdkconfig.defaults` against "Should"-priority requirements (ND-0403, ND-0403a) to confirm conscious opt-out vs. accidental omission.

---

## 8. Open Questions

1. **ND-0915 intent**: Is the `has_peer_payload()` guard in F-001 an intentional refinement or an accidental narrowing? If intentional, update the spec. If accidental, the code behavior after `peer_payload` erasure + WAKE failure needs to be defined.

2. **`send_recv` timeout clamping vs. error**: Should `send_recv()` return -1 (like `delay_us`) when the requested timeout exceeds `MAX_SEND_RECV_TIMEOUT_MS`, or is silent clamping the intended behavior? The current asymmetry between helpers may confuse BPF authors.

3. **Instruction budget configurability**: Should the instruction budget be configurable per-program (via the program image metadata) rather than a firmware-global constant? The current design ties all programs to the same budget.

4. **Secure boot / flash encryption timeline**: When is production hardware expected to support ND-0403 and ND-0403a? These should be tracked as deployment prerequisites, not just code gaps.

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-13 | Code Compliance Audit (automated) | Initial report covering all 55 ND-requirements against sonde-node crate source |
