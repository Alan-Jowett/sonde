# Sonde Code Compliance Audit — Investigation Report

**Crate under audit:** `sonde-pair` (BLE pairing library)
**Audit date:** 2025-07-03

---

## 1. Executive Summary

The `sonde-pair` crate was audited against its requirements (51 REQ-IDs), design, and validation documents. Of 51 requirements, 47 are fully implemented, 1 is partially implemented, and 3 are not directly verifiable from library code (UI-only requirements). The audit identified **10 findings**: 2 constraint violations (D10), 1 partial implementation (D8), and 7 instances of undocumented behavior (D9). The highest-severity finding is the `GW_INFO_RESPONSE` timeout set to 45 seconds instead of the specified 5 seconds (D10, High), which directly contradicts PT-0301, PT-1002, and validation test T-PT-802. Recommended action: update the timeout to match the spec (or update the spec to reflect the operational rationale documented in the code comment).

---

## 2. Problem Statement

This audit performs static code-to-specification traceability analysis on the `sonde-pair` crate. The objective is to verify that the implementation matches the specification in both directions: every requirement is implemented (forward traceability), and every significant code behavior traces to a requirement (backward traceability). The audit was initiated as a routine compliance check. The primary concern is finding code behavior not covered by the specification (D9 findings).

---

## 3. Investigation Scope

- **Codebase / components examined:**
  - `crates/sonde-pair/src/` — all 21 source files: `lib.rs`, `types.rs`, `error.rs`, `crypto.rs`, `rng.rs`, `cbor.rs`, `validation.rs`, `transport.rs`, `envelope.rs`, `fragmentation.rs`, `discovery.rs`, `phase1.rs`, `phase2.rs`, `store.rs`, `file_store.rs`, `dpapi.rs`, `secret_service_store.rs`, `android_store.rs`, `android_transport.rs`, `btleplug_transport.rs`, `loopback_transport.rs`
- **Specification documents:**
  - Requirements: `docs/ble-pairing-tool-requirements.md` (51 REQ-IDs: PT-0100 through PT-1206)
  - Design: `docs/ble-pairing-tool-design.md`
  - Validation: `docs/ble-pairing-tool-validation.md` (66 test cases: T-PT-100 through T-PT-1004)
- **Tools used:** Static analysis via source code reading, `grep` pattern search, cross-document traceability mapping
- **Limitations:**
  - No runtime execution or test execution performed (static analysis only).
  - UI-layer requirements (PT-0700, PT-0701 partial, PT-0702 partial) are out of scope for the library crate — they apply to the Tauri shell which is not yet built.
  - PT-1206 (manual hardware testing) cannot be verified from code.
  - Android JNI behavior (PT-0105, PT-0107, PT-0108) can only be structurally verified — actual device testing is out of scope.

---

## 4. Findings

### Finding F-001: `GW_INFO_RESPONSE` Timeout Is 45 s, Not 5 s

- **Severity**: High
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Spec Location**: PT-0301 §5 ("`GW_INFO_RESPONSE` timeout: 5s"), PT-1002 §12 ("all timeouts MUST match protocol specification"), validation T-PT-802 step 2 ("Assert: `GW_INFO_RESPONSE` timeout = 5 s")
- **Code Location**: `crates/sonde-pair/src/phase1.rs:147`
- **Description**: The code uses a 45-second timeout for the `GW_INFO_RESPONSE` indication, which is 9× the specified 5-second value. The code comment at lines 140–141 explains the rationale: *"The timeout must be long enough to cover the operator passkey confirmation window (up to 30 s) plus gateway processing time."* This is a deliberate implementation choice to accommodate the LESC Numeric Comparison dialog, but it contradicts the specification.
- **Evidence**:
  - **Spec says** (PT-0301): "wait for `GW_INFO_RESPONSE` (timeout: 5s)"
  - **Code does** (phase1.rs:145–147):
    ```rust
    trace!("waiting for GW_INFO_RESPONSE indication (45 s timeout)");
    let response = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 45_000)
        .await?;
    ```
  - **Validation** (T-PT-802 step 2): "Assert: `GW_INFO_RESPONSE` timeout = 5 s" — test would fail if it inspected the actual constant.
- **Impact**: Operators will wait up to 45 seconds instead of 5 seconds for an unresponsive gateway before seeing a timeout error. This changes the user experience and may mask connectivity issues. The test T-PT-802 is expected to fail against this code if it checks the actual timeout value.
- **Remediation**: Either (a) update the code to use 5000 ms per spec, or (b) update PT-0301, PT-1002, and T-PT-802 to specify 45 s with documented rationale (LESC pairing dialog window).
- **Confidence**: High — exact code location and spec text confirmed.

---

### Finding F-002: btleplug Transport Retries GATT Writes Up to 6 Times

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Spec Location**: PT-1003 §12 ("Tool MUST NOT silently retry failed protocol operations"), validation T-PT-803 ("no automatic retries; `write_characteristic` called exactly once")
- **Code Location**: `crates/sonde-pair/src/btleplug_transport.rs:361–380`
- **Description**: When a GATT write fails with "authentication" or "0x80650005" error (WinRT pairing dialog trigger), the btleplug transport retries the write up to 6 times with 5-second delays (total 30 seconds). This is functionally a workaround for the OS pairing dialog flow on WinRT, but it constitutes an implicit retry of a write operation at the transport layer.
- **Evidence**:
  - **Spec says** (PT-1003): "Tool MUST NOT silently retry failed protocol operations. If write or indication times out, tool reports failure."
  - **Code does** (btleplug_transport.rs:361–368):
    ```rust
    if msg.contains("authentication") || msg.contains("0x80650005") {
        debug!("GATT write requires auth — waiting for OS pairing dialog");
        for attempt in 1..=6 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            debug!(attempt, "retrying GATT write after pairing");
            // ... retry write ...
        }
    }
    ```
  - **Mitigation**: PT-1003 notes "BLE-level connection retries by platform stack acceptable." This retry handles OS-initiated pairing which is arguably BLE-level behavior, but it is implemented in application code, not the platform stack itself.
- **Impact**: On WinRT, the operator sees an implicit 30-second retry window during pairing dialog acceptance. If the operator dismisses the dialog, the tool silently retries instead of failing fast. Phase-level tests (T-PT-803) use mock transport and would not observe this behavior.
- **Remediation**: Either (a) document this as an exception to PT-1003 specific to WinRT BLE pairing dialog handling, or (b) refactor to fail after the first write error and let the caller retry, or (c) add a specific "awaiting OS pairing" state that is visible to the operator. Option (a) is recommended as the behavior is practically necessary for WinRT.
- **Confidence**: High — exact code location and retry count confirmed.

---

### Finding F-003: Android Transport `pairing_method()` Always Returns `None`

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: PT-0904 §11 ("Transport abstraction MUST expose explicit, observable signal indicating which pairing method was actually negotiated with OS BLE stack")
- **Code Location**: `crates/sonde-pair/src/android_transport.rs:570–576`
- **Description**: PT-0904 specifies that the transport must expose the actual pairing method. The requirement explicitly distinguishes desktop platforms (where `None` meaning "OS-enforced" is acceptable) from Android (where `onBondStateChanged` can observe the method). The Android transport always returns `None` with a TODO comment indicating the JNI callback is not yet wired up.
- **Evidence**:
  - **Spec says** (PT-0904): "Transport abstraction MUST expose explicit, observable signal indicating which pairing method was actually negotiated"
  - **Code does** (android_transport.rs:570–576):
    ```rust
    /// Android can observe the pairing method via `onBondStateChanged`.
    /// TODO: Wire up JNI callback to report the actual negotiated method.
    fn pairing_method(&self) -> Option<PairingMethod> {
        None
    }
    ```
  - **Functional impact**: Because `enforce_lesc()` treats `None` as acceptable (OS-enforced), the pairing flow still works. However, on Android, a Just Works fallback would NOT be detected — the tool would proceed with PSK-bearing GATT operations without MITM protection.
- **Impact**: On Android devices where the BLE stack silently degrades to Just Works, the pairing tool would not detect the insecure pairing mode. PSK material could be intercepted by a MITM attacker.
- **Remediation**: Wire up the `onBondStateChanged` JNI callback to report the actual negotiated pairing method, as indicated by the TODO. Until implemented, document the security risk in the Android deployment notes.
- **Confidence**: High — TODO comment in code explicitly acknowledges the gap.

---

### Finding F-004: Plaintext-to-Encrypted PSK Migration Not in Requirements

- **Severity**: Medium
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — no matching requirement identified. Searched: PT-0800 (pairing store contents), PT-0801 (secure storage), PT-0803 (corruption handling). None mention migration from plaintext to encrypted storage.
- **Code Location**: `crates/sonde-pair/src/file_store.rs:240–278`
- **Description**: When `FilePairingStore` loads a JSON file containing a plaintext `phone_psk` field (legacy format) and a `PskProtector` is attached, it emits a `tracing::warn!` and transparently loads the plaintext PSK. On the next `save()`, the PSK is re-encrypted using the protector, replacing the plaintext field with `phone_psk_protected`. This automatic migration behavior is not specified in any requirement.
- **Evidence**:
  - **Code does** (file_store.rs:270–278, load path):
    ```rust
    } else if let Some(ref psk_hex) = s.phone_psk {
        if self.protector.is_some() {
            tracing::warn!("phone_psk stored in plaintext — will be encrypted on next save");
        }
    ```
  - Module doc (lines 25–27): *"Files written without a protector store `phone_psk` as plaintext hex and are transparently read by stores with a protector (backward compatibility)."*
  - No requirement in PT-0800, PT-0801, or PT-0803 mentions backward compatibility or migration between plaintext and encrypted PSK storage.
- **Impact**: The migration path works correctly and improves security by encrypting previously plaintext PSKs. However, it is untested against any specification — changes to this code have no acceptance criteria to verify against. The `tracing::warn!` path also means plaintext PSK appears briefly in the code's memory during the read-then-encrypt cycle.
- **Remediation**: Add a requirement (e.g., PT-0805) covering backward-compatible PSK encryption migration: "When loading a plaintext `phone_psk` with a protector attached, the store MUST re-encrypt on next save and emit a warning." Alternatively, if legacy migration is not desired, remove the plaintext fallback path.
- **Confidence**: High — code path confirmed, no matching requirement found.

---

### Finding F-005: Linux Secret Service PSK Protector Not in Requirements

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — no matching requirement identified. Searched: PT-0801 (secure storage) which mentions only "Android Keystore or encrypted SharedPreferences (Android)" and "DPAPI-protected storage or user-profile directory with restricted permissions (Windows)." PT-0100 (supported platforms) lists Windows and Android only.
- **Code Location**: `crates/sonde-pair/src/secret_service_store.rs` (entire file, 239 lines)
- **Description**: A complete Linux Secret Service (D-Bus) PSK protector implementation exists, using GNOME Keyring / KWallet for PSK-at-rest encryption. This is a full `PskProtector` trait implementation with store, load, and delete operations. Requirements PT-0801 and PT-0100 do not mention Linux as a target platform.
- **Evidence**:
  - **Requirements** (PT-0801): "Android Keystore or encrypted SharedPreferences (Android); DPAPI-protected storage (Windows)"
  - **Requirements** (PT-0100): "Windows (desktop) and Android (physical devices)"
  - **Code provides**: `SecretServicePskProtector` at `secret_service_store.rs:28` implementing `PskProtector` trait
- **Impact**: Benign scope expansion. The implementation provides value for Linux development/testing but has no acceptance criteria. This is a reasonable anticipatory feature (supporting developers on Linux), not a security concern.
- **Remediation**: Either (a) add Linux as a supported platform in PT-0100 and add "Linux Secret Service keyring" to PT-0801, or (b) document it as a developer convenience feature outside the formal requirement scope.
- **Confidence**: High.

---

### Finding F-006: Node ID Whitespace Trimming Before Validation

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — PT-0403 §6 says "node_id (1–64 bytes UTF-8)" without mentioning trimming. Searched: PT-0403, PT-1205.
- **Code Location**: `crates/sonde-pair/src/validation.rs:10–23`
- **Description**: The `validate_node_id()` function trims whitespace from the input before checking length and emptiness. A node ID consisting solely of whitespace (e.g., `"    "`, 4 bytes of valid UTF-8) is rejected as "empty" even though it meets the literal "1–64 bytes UTF-8" constraint. The doc comment acknowledges this: *"non-empty after trimming."*
- **Evidence**:
  - **Spec says** (PT-0403): "`node_id` validated: empty strings and strings >64 bytes rejected"
  - **Code does** (validation.rs:10–16):
    ```rust
    pub fn validate_node_id(id: &str) -> Result<(), PairingError> {
        let trimmed = id.trim();
        if trimmed.is_empty() { ... }
        if trimmed.len() > 64 { ... }
    }
    ```
  - A 4-space string is 4 bytes of valid UTF-8, passes the literal spec constraint (1–64 bytes, non-empty), but fails the code's trimming check.
- **Impact**: Low. Whitespace-only node IDs are not a practical use case. The trimming behavior is arguably defensive and desirable, but is not specified.
- **Remediation**: Update PT-0403 acceptance criteria to state: "empty strings, whitespace-only strings, and strings >64 bytes (after trimming) rejected."
- **Confidence**: High.

---

### Finding F-007: `GatewayIdMismatch` as Separate TOFU Check

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — PT-0302 §5 mentions only "gateway presenting different public key." Searched: PT-0302 acceptance criteria (3 items), PT-0502.
- **Code Location**: `crates/sonde-pair/src/phase1.rs:194–201`
- **Description**: The TOFU check validates both `gw_public_key` AND `gateway_id` independently. PT-0302 acceptance criteria only mention "subsequent connection to gateway with different public key is rejected." The code adds a second check: if the public key matches but `gateway_id` differs, it returns `PairingError::GatewayIdMismatch`. A dedicated test (`t_pt_209_tofu_gateway_id_mismatch`) validates this behavior.
- **Evidence**:
  - **Spec says** (PT-0302): "reject any gateway presenting different public key"
  - **Code does** (phase1.rs:194–201):
    ```rust
    if stored.gateway_id != gw_info.gateway_id {
        warn!("gateway identity mismatch: stored gateway_id does not match");
        return Err(PairingError::GatewayIdMismatch);
    }
    ```
- **Impact**: Positive security enhancement. A gateway presenting the same public key with a different `gateway_id` is suspicious (possible key reuse attack). The additional check strengthens TOFU enforcement.
- **Remediation**: Add `gateway_id` mismatch rejection to PT-0302 acceptance criteria: "Subsequent connection with different public key OR different gateway_id is rejected."
- **Confidence**: High.

---

### Finding F-008: btleplug Transport Reports Hardcoded MTU (247)

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — PT-0300 §5 says "negotiate ATT MTU ≥247." The code does not negotiate; it reports a conservative default. Searched: PT-0300, PT-0401, PT-1002, design doc §6.1.
- **Code Location**: `crates/sonde-pair/src/btleplug_transport.rs:48,317–320`
- **Description**: The btleplug BLE library (v0.11) does not expose an API to query the negotiated ATT MTU. The transport always returns `BLE_MTU_MIN` (247) as a hardcoded conservative default. The actual OS-negotiated MTU is almost certainly higher (512+ on modern hardware), but is not reported.
- **Evidence**:
  - **Spec says** (PT-0300): "negotiate ATT MTU ≥247"
  - **Code does** (btleplug_transport.rs:48,320):
    ```rust
    const DEFAULT_REPORTED_MTU: u16 = BLE_MTU_MIN; // 247
    // ...
    Ok(DEFAULT_REPORTED_MTU) // returned from connect()
    ```
  - Module doc explains: *"btleplug 0.11 does not expose an API to query or request the ATT MTU."*
- **Impact**: The protocol logic receives 247 as the MTU and proceeds correctly (247 is the minimum). No functional failure results. However, the code does not verify the actual negotiated value — if a device negotiated an MTU below 247, the code would not detect it.
- **Remediation**: Document this limitation in PT-0300 or add a note: "On btleplug platforms, the OS negotiates MTU automatically; the library reports the minimum (247) as a conservative bound." When btleplug exposes MTU query in a future version, update the implementation.
- **Confidence**: High.

---

### Finding F-009: `LoopbackBleTransport` Not in Requirements

- **Severity**: Informational
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — PT-1200 specifies `MockBleTransport` for CI testing. Searched: PT-1200, PT-1004, PT-1206. No requirement mentions TCP-based test transport.
- **Code Location**: `crates/sonde-pair/src/loopback_transport.rs` (entire file, 242 lines)
- **Description**: A TCP-based BLE transport (`LoopbackBleTransport`) is implemented behind the `loopback-ble` feature flag. It connects to a TCP endpoint simulating a GATT peripheral, enabling hardware-free integration testing without mocks. This is supplementary to the `MockBleTransport` required by PT-1200.
- **Evidence**: Feature-gated module (`lib.rs:27–28`), reports `pairing_method() → Some(NumericComparison)`, returns hardcoded MTU of 512.
- **Impact**: None. This is reasonable test infrastructure that supplements (does not replace) the required mock transport. Feature-gated so it doesn't ship in production.
- **Remediation**: No action required. Optionally mention in PT-1200 as supplementary integration test infrastructure.
- **Confidence**: High.

---

### Finding F-010: Indication Reassembly `MAX_REASSEMBLY_SIZE` (4096 bytes) Not in Spec

- **Severity**: Informational
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — no requirement specifies a maximum reassembly buffer size. Searched: PT-0300, PT-0401, PT-1000, design doc §3.5. Reassembly itself is required by the protocol spec §3.4 but no buffer limit is specified.
- **Code Location**: `crates/sonde-pair/src/fragmentation.rs:18–23`
- **Description**: The `IndicationReassembler` enforces a 4096-byte maximum envelope size to prevent unbounded allocation from a malicious or buggy peer advertising a large `LEN` field.
- **Evidence**:
  ```rust
  /// Maximum envelope size the reassembler will accept (bytes).
  /// Prevents unbounded buffering from a malicious/buggy peer.
  const MAX_REASSEMBLY_SIZE: usize = 4096;
  ```
- **Impact**: Positive defense-in-depth. All pairing protocol messages are well under 1 KiB; 4096 provides generous headroom. No functional impact.
- **Remediation**: No action required. Optionally add to PT-1000 (transient failure tolerance) as a security hardening note.
- **Confidence**: High.

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|--------|-------|
| **Total REQ-IDs** | 51 (PT-0100 through PT-1206) |
| **Implemented in code** | 47 (92%) |
| **Partially implemented** | 1 (PT-0904 — Android `pairing_method()` returns `None`) |
| **Not applicable to library crate** | 3 (PT-0700 UI surface, PT-0105 Android permissions dialog, PT-1206 manual hardware testing) |
| **D8 findings (unimplemented)** | 1 |
| **D9 findings (undocumented behavior)** | 7 |
| **D10 findings (constraint violations)** | 2 |
| **Constraints verified compliant** | 26 of 29 documented constraints |
| **Constraints violated** | 2 (timeout value, implicit retry) |
| **Constraints unverifiable (static analysis)** | 1 (PT-1206 manual hardware testing) |

### Overall Assessment

The `sonde-pair` crate demonstrates strong specification compliance. The implementation faithfully follows the protocol specification for both Phase 1 (gateway pairing) and Phase 2 (node provisioning), with correct cryptographic operations, proper key zeroization, and comprehensive error handling.

The most significant finding is the `GW_INFO_RESPONSE` timeout deviation (F-001), which appears to be a deliberate operational adjustment to accommodate the BLE pairing dialog but was not reflected back into the specification. This is a common pattern: implementation learnings that don't flow back to the spec.

The D9 (undocumented behavior) findings are predominantly benign: Linux support (F-005), defensive trimming (F-006), stronger TOFU enforcement (F-007), and test infrastructure (F-009, F-010). These represent reasonable engineering decisions that should be captured in the spec for traceability completeness.

The incomplete Android `pairing_method()` (F-003) is the most security-relevant gap — it means LESC enforcement cannot be verified on Android, which is a primary deployment target.

---

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Reconcile `GW_INFO_RESPONSE` timeout: update spec to 45 s (with rationale) OR code to 5 s | S | Spec or behavioral change |
| 2 | F-003 | Wire up Android `onBondStateChanged` JNI callback to report actual pairing method | M | JNI complexity; untested until hardware available |
| 3 | F-002 | Document btleplug GATT write retry as PT-1003 exception for WinRT pairing dialog | S | None (documentation only) |
| 4 | F-004 | Add PT-0805 requirement for plaintext-to-encrypted PSK migration | S | None (documentation only) |
| 5 | F-007 | Add `gateway_id` mismatch to PT-0302 acceptance criteria | S | None (documentation only) |
| 6 | F-006 | Add whitespace trimming to PT-0403 acceptance criteria | S | None (documentation only) |
| 7 | F-005 | Add Linux Secret Service to PT-0801 or document as developer convenience | S | None (documentation only) |
| 8 | F-008 | Document btleplug MTU limitation in PT-0300 | S | None (documentation only) |
| 9 | F-009 | Optionally mention `LoopbackBleTransport` in PT-1200 | S | None |
| 10 | F-010 | Optionally document `MAX_REASSEMBLY_SIZE` in PT-1000 | S | None |

---

## 7. Prevention

1. **Spec update discipline**: When implementation deviates from spec for operational reasons (e.g., F-001 timeout change), update the spec in the same commit. Add a CI check or PR template item: "Does this change any value specified in the requirements doc?"

2. **Backward traceability in code review**: For every new module or significant code behavior, code reviewers should ask: "Which REQ-ID does this trace to?" If none, either add a requirement or flag as intentional infrastructure. The 7 D9 findings all represent reasonable code that was simply never reflected in the spec.

3. **Android security milestone**: Track F-003 (Android `pairing_method()`) as a blocking item before Android production release. Without LESC verification on Android, MITM protection cannot be confirmed on the primary mobile deployment target.

4. **Timeout constant centralization**: Extract all timeout values into named constants in a single location (e.g., `types.rs` or a `timeouts.rs`). This makes T-PT-802 verification trivial and prevents drift between spec and code.

---

## 8. Open Questions

1. **Is the 45-second `GW_INFO_RESPONSE` timeout correct for the operational model?** The spec says 5 s, but the code comment explains that LESC Numeric Comparison dialog can take up to 30 s. If the connection is already established before `REQUEST_GW_INFO` is sent, does the LESC dialog occur during connection (before the timer starts) or during the first characteristic write? If LESC pairing happens during `connect()`, the 5-second timeout may be sufficient. Resolving this requires testing on physical hardware with the modem.

2. **Design doc vs. requirements conflict on Just Works for nodes**: The design document states "Just Works acceptable only for node provisioning" (mapped to PT-0300), but PT-0904 and PT-0401 both require LESC for all connections. The code follows the requirements (rejects Just Works for nodes). Should the design doc be updated to match the requirements, or should the requirements be relaxed for node connections?

3. **btleplug MTU visibility**: When btleplug exposes MTU query in a future version, should the implementation switch to reporting the actual negotiated value? If so, what happens if a device negotiates MTU < 247 — should the connection be refused even though the library currently never detects this scenario?

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-03 | Copilot (audit agent) | Initial audit report |
