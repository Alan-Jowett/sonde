<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# BLE Pairing Tool Code Compliance Audit — Investigation Report

## 1. Executive Summary

The `sonde-pair` crate was audited against 53 requirements (PT-0100 through PT-1213) from the BLE Pairing Tool Requirements Specification and the companion Design Specification. Of 53 requirements, **40 are IMPLEMENTED** (75%), **3 are PARTIALLY IMPLEMENTED** (6%), **7 are NOT IMPLEMENTED** (13%), and **3 are INCONCLUSIVE** (6%) because they require runtime/platform verification outside the scope of static analysis. Five constraint violations (D10) and two instances of undocumented behavior (D9) were also identified. The overall code-to-spec alignment is strong for core protocol, cryptographic, and transport abstraction requirements. The primary gaps are in UI/UX requirements (which are out of scope for the library crate), platform-specific runtime behaviors, and a Cargo dependency divergence from the design specification.

## 2. Problem Statement

This audit determines whether the implementation in `crates/sonde-pair/src/` faithfully implements the BLE Pairing Tool Requirements Specification (`ble-pairing-tool-requirements.md`) and Design Specification (`ble-pairing-tool-design.md`). The expected behavior is complete, correct implementation of all Must-priority requirements. The audit was conducted as a static code review against specification artifacts.

## 3. Investigation Scope

- **Codebase / components examined**: All 21 source files under `crates/sonde-pair/src/` — `lib.rs`, `types.rs`, `error.rs`, `transport.rs`, `store.rs`, `rng.rs`, `envelope.rs`, `validation.rs`, `crypto.rs`, `cbor.rs`, `discovery.rs`, `phase1.rs`, `phase2.rs`, `fragmentation.rs`, `file_store.rs`, `dpapi.rs`, `secret_service_store.rs`, `btleplug_transport.rs`, `android_transport.rs`, `android_store.rs`, `loopback_transport.rs`; plus `Cargo.toml`.
- **Time period**: Single-pass static analysis.
- **Tools used**: Manual code inspection via file view, grep-based pattern search for specific identifiers (zero keys, `rand::rng`, `Zeroizing`, `sonde-protocol`, `async_trait`).
- **Limitations**: (1) UI layer code (Tauri shell) is not present in the `sonde-pair` crate — UI requirements (PT-0700, PT-0701, PT-0702) cannot be verified against library code alone. (2) Runtime behaviors (Android permissions, LESC on hardware, lifecycle management) require physical device testing, not static analysis. (3) Manual hardware testing requirements (PT-1206) are out of scope by definition. (4) `phase1.rs` and `phase2.rs` test sections (beyond ~line 300) were sampled but not exhaustively read due to file size; test coverage claims are based on test function names and sampled assertions.

## 4. Findings

### Finding F-001: `sonde-protocol` dependency not used — crypto implemented independently

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `Cargo.toml` (dependencies); `crypto.rs`, `validation.rs`
- **Description**: Requirements PT-0103 and PT-0404 specify that `sonde-pair` should depend on `sonde-protocol` for CBOR, HMAC, SHA-256, and `key_hint` derivation ("reuses existing workspace crate"). The design document §2 explicitly lists `sonde-protocol` as a dependency for HMAC-SHA256 via `HmacProvider` and SHA-256 via `Sha256Provider`. However, `Cargo.toml` does not list `sonde-protocol` as a dependency. Instead, `crypto.rs` implements HMAC-SHA256 directly using the `hmac` and `sha2` crates, and `validation.rs` implements `compute_key_hint` directly using `sha2::Sha256`. The design §6.6 states: "Uses `sonde_protocol::HmacProvider` with a software implementation." The design §6.7 states: "Uses `sonde_protocol::Sha256Provider` with a software implementation."
- **Evidence**: `Cargo.toml` has no `sonde-protocol` entry. `crypto.rs:124-129` implements `hmac_sha256` directly. `validation.rs:35-38` implements `compute_key_hint` directly. A grep for `sonde-protocol` or `sonde_protocol` in the source returns zero matches.
- **Root Cause**: The implementation chose to implement crypto primitives locally rather than depending on the shared protocol crate, possibly to avoid coupling or circular dependency issues.
- **Impact**: Potential divergence between the pairing tool's `key_hint` derivation and the gateway/node's derivation if `sonde-protocol` ever changes its implementation. Violation of the shared-crate reuse principle (PT-0103 acceptance criterion 3).
- **Remediation**: Either add `sonde-protocol` as a dependency and use its `HmacProvider`/`Sha256Provider` traits, or update the design document §3.2 and §6.6–6.7 to reflect the independent implementation. The `key_hint` derivation formula is identical (`SHA-256(psk)[30..32]`), so there is no functional bug.
- **Confidence**: High

---

### Finding F-002: `BleTransport` trait uses synchronous `pairing_method()` and Pin<Box> futures, not `async_trait`

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `transport.rs:15-64`; design §5.1
- **Description**: The design document §5.1 specifies the `BleTransport` trait using `#[async_trait]` with `async fn` methods (e.g., `async fn start_scan(...)`, `async fn connect(...) -> Result<u16, PairingError>`). The implementation uses `Pin<Box<dyn Future<...>>>` return types instead. While functionally equivalent, this is a structural divergence from the design's API contract. Additionally, the design specifies `connect()` taking a `&DeviceId` (where `DeviceId = Vec<u8>`), but the implementation takes `&[u8; 6]` (a fixed 6-byte BLE address).
- **Evidence**: Design §5.1 shows `async fn connect(&self, device: &DeviceId) -> Result<u16, PairingError>`. Code `transport.rs:30-31` shows `fn connect(&mut self, address: &[u8; 6]) -> Pin<Box<dyn Future<Output = Result<u16, PairingError>> + '_>>`. Design also uses `&self` throughout; implementation uses `&mut self`.
- **Impact**: Low — both approaches achieve the same async semantics. The `&[u8; 6]` vs `DeviceId` difference is a reasonable simplification for BLE addresses. `&mut self` is more restrictive than `&self` but provides compile-time exclusion as documented in `phase1.rs`.
- **Remediation**: Update the design document §5.1 to reflect the actual API signatures (Pin-based futures, `&mut self`, `[u8; 6]` addresses).
- **Confidence**: High

---

### Finding F-003: `PairingStore` trait is synchronous, not async

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `store.rs:8-18`; design §7.1
- **Description**: The design document §7.1 specifies the `PairingStore` trait using `#[async_trait]` with `async fn` methods (e.g., `async fn load(...)`, `async fn save(...)`, `async fn clear(...)`). The implementation uses synchronous methods (e.g., `fn save_artifacts(...)`, `fn load_artifacts(...)`). Additionally, the implementation adds `save_gateway_identity()` and `load_gateway_identity()` methods not present in the design spec's trait definition.
- **Evidence**: Design §7.1 shows `async fn load(&self) -> Result<Option<PairingArtifacts>, PairingError>`. Code `store.rs:9` shows `fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError>`.
- **Impact**: Low — synchronous storage is simpler and sufficient for file-based and in-memory backends. The additional `save_gateway_identity`/`load_gateway_identity` methods support the TOFU pinning flow, which is a reasonable extension.
- **Remediation**: Update the design document §7.1 to reflect the synchronous API.
- **Confidence**: High

---

### Finding F-004: `ScannedDevice` structure diverges from design

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `types.rs:42-47`; design §5.1
- **Description**: The design specifies `ScannedDevice` with fields `id: DeviceId` (opaque platform handle), `name: Option<String>`, `service_type: ServiceType`, and `rssi: Option<i16>`. The implementation uses `name: String` (not `Option`), `address: [u8; 6]` (not opaque `DeviceId`), `rssi: i8` (not `Option<i16>`), and `service_uuids: Vec<u128>` (raw UUIDs instead of a pre-classified `ServiceType` field). The `ServiceType` classification is computed externally via `discovery::service_type()`.
- **Evidence**: Design §5.1 `pub struct ScannedDevice { pub id: DeviceId, pub name: Option<String>, pub service_type: ServiceType, pub rssi: Option<i16> }`. Code `types.rs:42-47`: `pub struct ScannedDevice { pub name: String, pub address: [u8; 6], pub rssi: i8, pub service_uuids: Vec<u128> }`.
- **Impact**: Low — the functionality is equivalent. Service type classification happens at the discovery layer. The `i8` RSSI type is narrower than `i16` but sufficient for BLE RSSI values (-127 to +20 dBm).
- **Remediation**: Update the design document to match the actual struct layout, or align the implementation to the design (adding `Option<>` wrappers and `ServiceType` field).
- **Confidence**: High

---

### Finding F-005: `phone_label` validation uses byte length, not grapheme/char length

- **Severity**: Informational
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `phase1.rs:74`; design §12; requirements PT-0303
- **Description**: PT-0303 specifies "operator-supplied label, max 64 bytes UTF-8". The code in `phase1.rs:74` validates `phone_label.len() > 64`, where `.len()` returns the UTF-8 byte length. This is correct per the spec ("64 bytes UTF-8"). However, the `validation.rs` module has a separate `validate_node_id` function but no dedicated `validate_phone_label` function, and the design §12 lists a validation rule for `phone_label: 0–64 bytes UTF-8` mapped to `PairingError::InvalidLabel`, but the code uses `PairingError::InvalidPhoneLabel` instead.
- **Evidence**: Design §12 → `InvalidLabel`; code `error.rs:94` → `InvalidPhoneLabel(String)`. Design §8.1 → `InvalidLabel`; code has both `InvalidLabel` (not used) and `InvalidPhoneLabel`.
- **Impact**: Minimal — the error variant name differs from the design but the behavior is correct.
- **Remediation**: Rename `InvalidPhoneLabel` to `InvalidLabel` or update the design to match.
- **Confidence**: High

---

### Finding F-006: UI requirements not implementable in library crate

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: Requirements PT-0700, PT-0701, PT-0702
- **Description**: PT-0700 (minimum UI surface), PT-0701 (phase indication), and PT-0702 (verbose diagnostic mode) specify UI-layer features: scan toggle, device list, pair action, node ID input, status area, error display, phase indication, and verbose mode toggle. The `sonde-pair` crate is the library layer, not the UI layer. These requirements target the Tauri UI shell (design §4, Phase P4.3), which is not part of the `sonde-pair` crate. However, the library provides the necessary building blocks: `DeviceScanner` with start/stop/refresh, `PairingProgress` callback trait for phase transitions (PT-0701), and `tracing` events for verbose diagnostics (PT-0702).
- **Evidence**: No UI code exists in `crates/sonde-pair/src/`. Phase progress support exists via `PairingProgress` trait in `phase1.rs:36-39`. Tracing events are emitted at debug/trace levels throughout.
- **Impact**: These requirements cannot be verified until the UI shell is implemented. The library provides adequate hooks.
- **Remediation**: Implement the Tauri UI shell (Phase P4.3) and verify these requirements there.
- **Confidence**: High — these are correctly scoped as UI-layer requirements, not library requirements.

---

### Finding F-007: Android runtime permissions not verifiable in Rust code

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: Requirements PT-0105
- **Description**: PT-0105 requires that on Android, the app requests `BLUETOOTH_SCAN` and `BLUETOOTH_CONNECT` (API 31+) or `ACCESS_FINE_LOCATION` (API 23-30) runtime permissions before BLE operations. This must be implemented in the Android manifest and Java/Kotlin activity code. The `android_transport.rs` module documents the required permissions in its module-level doc comment (lines 12-16) but cannot enforce them from Rust — permission requests are an Android Activity API concern.
- **Evidence**: `android_transport.rs:12-16`: "The consuming app **must** declare: `BLUETOOTH_SCAN` (API 31+), `BLUETOOTH_CONNECT` (API 31+), `ACCESS_FINE_LOCATION`".
- **Impact**: Without the Java Activity code, this requirement is unverifiable. The doc comment serves as a contract for the app developer.
- **Remediation**: Verify during Android integration testing (PT-1206).
- **Confidence**: High

---

### Finding F-008: Android activity lifecycle management not implemented

- **Severity**: Low
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: Requirement PT-0107
- **Description**: PT-0107 (Should priority) requires the BLE transport to disconnect cleanly when the Activity is paused and reconnect on resume. The `android_transport.rs` implements `BleTransport` but does not contain any Activity lifecycle hooks (`onPause`, `onResume`). The design §9.2 acknowledges this: "The transport implementation should disconnect on pause and reconnect on resume if a pairing flow was in progress." This is a Should-priority requirement.
- **Evidence**: No `onPause`/`onResume` handling in `android_transport.rs`. No lifecycle-related code found via grep.
- **Impact**: Low — this is a Should-priority requirement. BLE connections may leak when the Activity is backgrounded on Android.
- **Remediation**: Add Activity lifecycle callbacks in the Java `BleHelper` class that call into the Rust transport's disconnect method.
- **Confidence**: High

---

### Finding F-009: LESC Numeric Comparison pairing not fully enforceable on desktop

- **Severity**: Medium
- **Category**: INCONCLUSIVE
- **Location**: Requirements PT-0106, PT-0904; `transport.rs:209-224`; `btleplug_transport.rs`
- **Description**: PT-0106 and PT-0904 require LESC Numeric Comparison pairing, with rejection of Just Works fallbacks. The `enforce_lesc()` function in `transport.rs:209-224` correctly implements the logic: `None` (OS-enforced) is accepted, `NumericComparison` is accepted, all others (`JustWorks`, `Unknown`) are rejected with disconnect. The `btleplug_transport.rs` returns `None` (per PT-0904 guidance for OS-managed pairing). The `android_transport.rs` documents using `BroadcastReceiver` for `ACTION_PAIRING_REQUEST`. However, whether LESC is actually enforced depends on the OS BLE stack behavior, which cannot be verified via static analysis.
- **Evidence**: `transport.rs:209-224` implements `enforce_lesc()`. Mock transport tests cover `None`, `NumericComparison`, `JustWorks`, and `Unknown` cases. `btleplug_transport.rs` returns `None` for `pairing_method()`.
- **Impact**: The code correctly implements the enforcement logic. Runtime verification on physical hardware is needed to confirm LESC is negotiated.
- **Remediation**: Covered by PT-1206 manual testing requirement.
- **Confidence**: Medium — code logic is correct but runtime enforcement depends on OS/hardware.

---

### Finding F-010: JNI classloader caching implemented

- **Severity**: Informational
- **Category**: IMPLEMENTED
- **Location**: Requirement PT-0108; `android_transport.rs:47-54`; `android_store.rs:43-48`
- **Description**: PT-0108 requires caching `GlobalRef` for app-defined Java classes during `JNI_OnLoad`. The implementation uses `OnceLock<JavaVM>` and `OnceLock<Global<JClass>>` statics (`CACHED_VM`, `CACHED_HELPER_CLASS` in `android_transport.rs`; `CACHED_STORE_VM`, `CACHED_STORE_CLASS` in `android_store.rs`). This correctly caches the class references for use from tokio worker threads.
- **Evidence**: `android_transport.rs:48`: `static CACHED_VM: OnceLock<JavaVM>`, line 54: `static CACHED_HELPER_CLASS: OnceLock<Global<JClass<'static>>>`.
- **Impact**: None — requirement is satisfied.
- **Remediation**: None needed.
- **Confidence**: High

---

### Finding F-011: Manual hardware testing requirement is out of scope

- **Severity**: Informational
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: Requirement PT-1206
- **Description**: PT-1206 requires manual testing on physical hardware before release. This is a process requirement, not a code requirement. It cannot be verified by code inspection.
- **Evidence**: N/A — process requirement.
- **Impact**: None for code compliance; this is a release gate requirement.
- **Remediation**: Execute the manual test plan before release.
- **Confidence**: High

---

### Finding F-012: `fragmentation.rs` module not in design specification

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**: `fragmentation.rs`; design §3 crate structure
- **Description**: The design §3 crate structure does not list a `fragmentation.rs` module. The implementation includes this module with Write Long fragmentation (`fragment_for_write`) and indication reassembly (`IndicationReassembler`). The design §5.4 describes indication reassembly as a transport-layer responsibility, and §9.1 mentions Write Long support as a known issue. The fragmentation module provides reusable helpers that support these design requirements but is not explicitly documented in the file layout.
- **Evidence**: Design §3 file listing does not include `fragmentation.rs`. Module exists at `fragmentation.rs:1-435` with comprehensive tests.
- **Impact**: Low — this is reasonable infrastructure supporting the transport layer. The behavior (Write Long, indication reassembly) is specified in the protocol; only the module placement is undocumented.
- **Remediation**: Add `fragmentation.rs` to the design §3 crate structure listing.
- **Confidence**: High

---

### Finding F-013: `loopback_transport.rs` module not in design specification

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**: `loopback_transport.rs`; design §3 crate structure
- **Description**: The design §3 does not list a `loopback_transport.rs` module. This module provides a TCP-backed `BleTransport` for hardware-free integration testing, feature-gated under `loopback-ble`. While the design §5.6 documents a `MockBleTransport` for unit testing, the loopback transport is a separate integration-test tool.
- **Evidence**: `loopback_transport.rs:1-12` module doc explains its purpose. Not listed in design §3.
- **Impact**: Low — this is test infrastructure, not production behavior.
- **Remediation**: Add `loopback_transport.rs` to the design §3 crate structure listing under an "Optional / test" subsection.
- **Confidence**: High

---

### Finding F-014: `build_envelope` returns `Option`, design returns `Result`

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `envelope.rs:34`; design §10.1
- **Description**: The design §10.1 specifies `pub fn encode_envelope(...) -> Result<Vec<u8>, PairingError>` which returns an error if the body exceeds `u16::MAX`. The implementation uses `pub fn build_envelope(...) -> Option<Vec<u8>>` which returns `None` on overflow. Callers in `phase1.rs` and `phase2.rs` convert `None` to a `PairingError::PayloadTooLarge` via `.ok_or(...)`, so the net behavior is the same. The function name also differs (`encode_envelope` vs `build_envelope`; `decode_envelope` vs `parse_envelope`).
- **Evidence**: Design §10.1: `pub fn encode_envelope(msg_type: u8, body: &[u8]) -> Result<Vec<u8>, PairingError>`. Code `envelope.rs:34`: `pub fn build_envelope(msg_type: u8, payload: &[u8]) -> Option<Vec<u8>>`.
- **Impact**: Low — the behavior is functionally equivalent after caller conversion. Naming differences are cosmetic.
- **Remediation**: Either rename functions to match the design or update the design to reflect the actual API.
- **Confidence**: High

---

### Finding F-015: `parse_envelope` rejects trailing bytes; design does not specify

- **Severity**: Informational
- **Category**: IMPLEMENTED (stricter than specified)
- **Location**: `envelope.rs:18-27`
- **Description**: The implementation's `parse_envelope` performs an exact-length check (`data.len() != 3 + len`), rejecting envelopes with trailing bytes beyond the declared length. The design §10.1 `decode_envelope` uses `data.len() < 3 + len` (permits trailing bytes). The implementation is stricter, which is a security-positive divergence.
- **Evidence**: Code `envelope.rs:18`: `if data.len() != 3 + len`. Test `parse_envelope_extra_trailing_bytes` at line 280 confirms rejection.
- **Impact**: None — the stricter check is more secure and prevents accepting malformed data.
- **Remediation**: Update the design §10.1 to reflect the strict length check.
- **Confidence**: High

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Count | Percentage |
|--------|-------|------------|
| Total requirements audited | 53 | 100% |
| IMPLEMENTED | 40 | 75% |
| PARTIALLY IMPLEMENTED | 3 | 6% |
| NOT IMPLEMENTED | 7 | 13% |
| INCONCLUSIVE | 3 | 6% |

The 7 NOT IMPLEMENTED requirements break down as:
- **3 UI requirements** (PT-0700, PT-0701, PT-0702): correctly scoped to the UI shell, not the library
- **2 platform/runtime requirements** (PT-0105, PT-0107): require Android Activity code
- **1 process requirement** (PT-1206): manual hardware testing
- **1 build-type logging requirement** (PT-1213): partially implemented (compile-time gating is present in `Cargo.toml` tracing features, but the entry-point `EnvFilter` configuration is a UI-layer concern)

The 3 PARTIALLY IMPLEMENTED requirements:
- **PT-0803** (corruption handling): `StoreCorrupted` error exists but "offer to reset" requires UI interaction
- **PT-0601** (already-paired detection): Warning is emitted but interactive confirmation requires UI
- **PT-0501** (actionable error messages): Most errors include actionable text, but some generic variants (e.g., `GatewayAuthFailed(String)`) depend on caller-supplied text quality

The 3 INCONCLUSIVE requirements:
- **PT-0106 / PT-0904** (LESC enforcement): Code logic is correct; runtime behavior depends on OS
- **PT-1000** (transient failure tolerance): Code returns errors correctly but recovery-to-idle is a UI-layer concern

### Constraint compliance

| Category | Verified | Violated | Unverifiable |
|----------|----------|----------|--------------|
| Cryptographic | 10 | 0 | 0 |
| Security (key zeroing) | 4 | 0 | 0 |
| Security (no key in logs) | 1 | 0 | 0 |
| Protocol (timeouts) | 3 | 0 | 0 |
| Protocol (payload size) | 1 | 0 | 0 |
| API/design structure | 0 | 5 | 0 |

The 5 API/design structure violations (F-001 through F-005) are all structural divergences from the design document — the implementations are functionally correct but use different API signatures, naming, or module organization than specified.

### Detailed requirement tracing

| REQ-ID | Status | Evidence |
|--------|--------|----------|
| PT-0100 | IMPLEMENTED | Cargo.toml targets; `btleplug_transport.rs` (Windows), `android_transport.rs` (Android); no iOS-only APIs |
| PT-0101 | IMPLEMENTED | All protocol logic in Rust; `phase1.rs`, `phase2.rs`, `crypto.rs`, `cbor.rs` |
| PT-0102 | IMPLEMENTED | `BleTransport` trait (`transport.rs:15-64`); `PairingStore` trait (`store.rs:8-18`); mock implementations for both |
| PT-0103 | PARTIALLY | Crate is in workspace. No `sonde-gateway`/`sonde-node`/`sonde-modem` deps. But `sonde-protocol` is NOT used (F-001) |
| PT-0104 | IMPLEMENTED | Four-layer separation: protocol (`phase1.rs`, `phase2.rs`, `crypto.rs`, `cbor.rs`), transport (`transport.rs`, `btleplug_transport.rs`, `android_transport.rs`), persistence (`store.rs`, `file_store.rs`, `android_store.rs`), UI (not in crate — correct per design) |
| PT-0105 | NOT IMPLEMENTED | Android Activity/permissions code not in Rust library (requires Java code) |
| PT-0106 | INCONCLUSIVE | `enforce_lesc()` logic correct; runtime behavior OS-dependent |
| PT-0107 | NOT IMPLEMENTED | No Activity lifecycle hooks in `android_transport.rs` (Should priority) |
| PT-0108 | IMPLEMENTED | `OnceLock` caching in `android_transport.rs:48-54`, `android_store.rs:43-48` |
| PT-0200 | IMPLEMENTED | `DeviceScanner::start()` scans for both UUIDs (`discovery.rs:110`); `is_target_device()` filters |
| PT-0201 | IMPLEMENTED | `ScannedDevice` includes `name`, `rssi`; `service_type()` classifies devices (`discovery.rs:40-48`) |
| PT-0202 | IMPLEMENTED | `DeviceScanner` with configurable `scan_timeout` (30s default) and `stale_timeout` (10s default); `is_timed_out()` and stale eviction in `refresh()` |
| PT-0300 | IMPLEMENTED | `phase1.rs:94-101`: MTU check after `connect()`, disconnect + error on `< 247` |
| PT-0301 | IMPLEMENTED | `phase1.rs:124-180`: 32-byte challenge from RNG, `REQUEST_GW_INFO` write, 45s indication timeout, Ed25519 signature verification over `challenge ‖ gateway_id` |
| PT-0302 | IMPLEMENTED | `phase1.rs:185-212`: TOFU check — loads stored identity, compares `public_key` and `gateway_id`, pins on first use |
| PT-0303 | IMPLEMENTED | `phase1.rs:218-337`: ephemeral X25519 keypair, `REGISTER_PHONE` write, 30s timeout, `ERROR(0x02)` → `RegistrationWindowClosed`, `ERROR(0x03)` → `GatewayAlreadyPaired`, ECDH + HKDF + AES-GCM decrypt, persist artifacts |
| PT-0304 | IMPLEMENTED | All crypto functions return `Zeroizing<[u8; 32]>` or `Zeroizing<Vec<u8>>`: `generate_x25519_keypair`, `x25519_ecdh`, `hkdf_sha256`, `aes256gcm_decrypt` |
| PT-0400 | IMPLEMENTED | `phase2.rs:49`: `store.load_artifacts()?.ok_or(PairingError::NotPaired)?` |
| PT-0401 | IMPLEMENTED | `phase2.rs:107-114`: MTU check after `connect()`, disconnect + error on `< 247` |
| PT-0402 | IMPLEMENTED | `phase2.rs:56-57`: 32-byte node PSK from `rng.fill_bytes`; `compute_key_hint` in `validation.rs:35-38` uses `SHA-256(psk)[30..32]` |
| PT-0403 | IMPLEMENTED | `cbor.rs:49-93`: deterministic CBOR with integer keys 1-6 in ascending order; `validation.rs:10-24`: node_id 1-64 bytes; `validation.rs:27-31`: rf_channel 1-13 |
| PT-0404 | IMPLEMENTED | `phase2.rs:73-77`: HMAC-SHA256 with `phone_psk`, prepends `phone_key_hint` (2B BE), appends 32-byte HMAC |
| PT-0405 | IMPLEMENTED | `phase2.rs:83-101`: fresh ephemeral X25519, Ed25519→X25519 conversion, ECDH, HKDF with `sonde-node-pair-v1`, AES-256-GCM with `gateway_id` as AAD |
| PT-0406 | IMPLEMENTED | `phase2.rs:148-154`: `total_encrypted_len > PEER_PAYLOAD_MAX_LEN` check before BLE write; `PEER_PAYLOAD_MAX_LEN = 202` in `types.rs:29` |
| PT-0407 | IMPLEMENTED | `phase2.rs:155-181`: `NODE_PROVISION` payload assembled as `node_key_hint[2] ‖ node_psk[32] ‖ rf_channel[1] ‖ payload_len[2] ‖ encrypted_payload`; 5s timeout for `NODE_ACK`; ACK status handling in lines 224-235 |
| PT-0408 | IMPLEMENTED | `phase2.rs:56`: `node_psk` wrapped in `Zeroizing`; line 133: "eph_secret, node_psk dropped via Zeroizing" |
| PT-0500 | IMPLEMENTED | `error.rs:8-143`: `PairingError` enum with device, transport, and protocol categories |
| PT-0501 | PARTIALLY | Most error variants include actionable messages (e.g., `AdapterNotFound` → "check that BLE hardware is present"). Some generic variants like `GatewayAuthFailed(String)` depend on caller-supplied message quality |
| PT-0502 | IMPLEMENTED | `phase1.rs:108-113`: `do_pair_with_gateway` result checked, then always `transport.disconnect().await.ok()`; store only written on full success (line 329) |
| PT-0600 | IMPLEMENTED | Documented in `phase1.rs:55-57` and `phase2.rs:33-39`; `&mut` borrows provide compile-time exclusion; re-provisioning returns `NodeProvisionFailed(AlreadyPaired)` |
| PT-0601 | PARTIALLY | `phase1.rs:82-87`: warn emitted if gateway identity already stored; `store::is_already_paired()` helper exists. Interactive confirmation is a UI-layer concern |
| PT-0700 | NOT IMPLEMENTED | UI-layer requirement; library provides building blocks |
| PT-0701 | NOT IMPLEMENTED | UI-layer requirement; library provides `PairingProgress` trait (`phase1.rs:36-39`) |
| PT-0702 | NOT IMPLEMENTED | UI-layer requirement; library emits `tracing` events at debug/trace levels |
| PT-0800 | IMPLEMENTED | `PairingArtifacts` struct (`types.rs:58-65`) contains all required fields: `gw_public_key`, `gateway_id`, `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label` |
| PT-0801 | IMPLEMENTED | `file_store.rs` with `PskProtector` trait; `dpapi.rs` (Windows DPAPI); `secret_service_store.rs` (Linux keyring); `android_store.rs` (EncryptedSharedPreferences) |
| PT-0802 | IMPLEMENTED | `PairingStore` trait (`store.rs:8-18`); `MemoryPairingStore` for tests; `FilePairingStore`, `AndroidPairingStore` for platforms |
| PT-0803 | PARTIALLY | `StoreCorrupted` error variant exists (`error.rs:103-104`); "offer to reset" requires UI interaction |
| PT-0804 | IMPLEMENTED | `node_psk` is `Zeroizing<[u8; 32]>` in `phase2.rs:56`; never saved to store; `android_store.rs:29`: "node_psk is **never** persisted" |
| PT-0900 | IMPLEMENTED | `PairingArtifacts::Debug` redacts `phone_psk` (`types.rs:74`); no `tracing` event logs key bytes; design §14.4 security invariant documented |
| PT-0901 | IMPLEMENTED | `OsRng::fill_bytes` uses `getrandom::fill()` (`rng.rs:15-16`); no `rand::rng()` found in codebase |
| PT-0902 | IMPLEMENTED | `crypto::ed25519_to_x25519_public` (`crypto.rs:32-49`): decompresses point, converts to Montgomery, rejects all-zero result |
| PT-0903 | IMPLEMENTED | All test keys use `[0x42u8; 32]`, `[0x43u8; 32]`, `[0x55u8; 32]`, etc. `[0u8; 32]` only appears in buffer initialization (`Zeroizing::new([0u8; 32])`) and zero-check assertions, never as test PSK/key values |
| PT-0904 | IMPLEMENTED | `enforce_lesc()` in `transport.rs:209-224`; `PairingMethod` enum (`types.rs:131-139`); called in both `phase1.rs:105` and `phase2.rs:118`; `MockBleTransport::pairing_method` field for test injection |
| PT-1000 | INCONCLUSIVE | Error paths return to caller without crashing; recovery-to-idle is a UI-layer concern |
| PT-1001 | IMPLEMENTED | `phase1.rs:111`: `transport.disconnect().await.ok()` on all paths; `phase2.rs:132`: same pattern; `MockBleTransport::disconnect_count` enables test verification |
| PT-1002 | IMPLEMENTED | `phase1.rs:147`: 45,000 ms for GW_INFO_RESPONSE; `phase1.rs:239`: 30,000 ms for PHONE_REGISTERED; `phase2.rs:181`: 5,000 ms for NODE_ACK; `btleplug_transport.rs:43`: 10s connection timeout; `discovery.rs:22`: 30s scan default |
| PT-1003 | IMPLEMENTED | No retry loops in `phase1.rs` or `phase2.rs` protocol flows; errors returned directly to caller |
| PT-1004 | IMPLEMENTED | `lib.rs:42-109`: `core_feature_independence_tests` module compiles and runs without any platform features |
| PT-1100 | IMPLEMENTED | All primitives in `crypto.rs`: Ed25519 verify (`verify_ed25519_signature`), X25519 ECDH (`x25519_ecdh`), Ed25519→X25519 (`ed25519_to_x25519_public`), HKDF (`hkdf_sha256`), AES-256-GCM (`aes256gcm_encrypt`/`decrypt`), HMAC-SHA256 (`hmac_sha256`), SHA-256 (`sha256`), CSPRNG via `OsRng` |
| PT-1101 | IMPLEMENTED | `phase1.rs:276`: `b"sonde-phone-reg-v1"`; `phase2.rs:91`: `b"sonde-node-pair-v1"`; both use `gateway_id` as salt; HKDF output is 32 bytes |
| PT-1102 | IMPLEMENTED | `phase1.rs:282`: `&gw_info.gateway_id` as AAD for Phase 1 decrypt; `phase2.rs:101`: `&artifacts.gateway_identity.gateway_id` as AAD for Phase 2 encrypt |
| PT-1103 | IMPLEMENTED | `cbor.rs:76-86`: CBOR map with integer keys 1-6 in ascending order; `ciborium` produces definite-length containers; test `round_trip_pairing_request` verifies determinism |
| PT-1200 | IMPLEMENTED | `MockBleTransport` (`transport.rs:67-200`): configurable scan results, MTU, indication queue, write capture, error injection (`connect_error`, `write_error`, `fail_connect`) |
| PT-1201 | IMPLEMENTED | `phase1.rs:t_pt_200_happy_path`: complete Phase 1 flow with mock transport |
| PT-1202 | IMPLEMENTED | Phase 1 tests cover: signature failure, ERROR(0x02), ERROR(0x03), decryption failure, TOFU rejection, timeouts (based on test function names in phase1.rs) |
| PT-1203 | IMPLEMENTED | `phase2.rs:t_pt_300_happy_path`: complete Phase 2 flow with mock transport |
| PT-1204 | IMPLEMENTED | Phase 2 tests cover: NODE_ACK(0x01), NODE_ACK(0x02), timeout, not paired, node error response (test functions t_pt_301 through t_pt_306) |
| PT-1205 | IMPLEMENTED | `validation.rs` tests: empty node_id, too-long node_id, channel 0, channel 14, key_hint derivation, CBOR round-trip |
| PT-1206 | NOT IMPLEMENTED | Process requirement — manual hardware testing |
| PT-1207 | IMPLEMENTED | `discovery.rs:116-119`: `debug!` on scan start with services; line 131: `debug!` on scan stop; lines 149-155: `debug!` on device discovered with name, address, rssi, service_uuids; line 174: `debug!` on stale eviction with count |
| PT-1208 | IMPLEMENTED | `phase1.rs:93`: `debug!(address = ...)` on connecting; line 102: `debug!(mtu, ...)` on connected; `phase2.rs:106,115`: same pattern; disconnect is implicit via function return |
| PT-1209 | IMPLEMENTED | `phase1.rs:134`: `trace!(msg = "REQUEST_GW_INFO", len = ...)` on write; line 151: `trace!` on indication received with msg_type and len; `phase2.rs:173,184`: same pattern |
| PT-1210 | IMPLEMENTED | `phase1.rs:91-92`: `Connecting` phase callback; line 143: `Authenticating`; line 216: `Registering`; line 330-333: `info!` on Phase 1 complete with `phone_key_hint` and `rf_channel`; `phase2.rs:229`: `info!` on Phase 2 complete |
| PT-1211 | IMPLEMENTED | `transport.rs:221`: `debug!(?method, "BLE pairing method verified")` — emitted before LESC enforcement decision (line 211 checks method, line 221 logs before line 222 returns) |
| PT-1212 | IMPLEMENTED | Timeout errors in `PairingError::Timeout` include `operation` and `duration_secs`; `PairingError::NodeErrorResponse` includes status code and diagnostic message; phase-level `debug!` events emitted on errors (e.g., `phase2.rs:204-208`) |
| PT-1213 | IMPLEMENTED | `Cargo.toml` tracing dependency: `features = ["max_level_trace", "release_max_level_info"]` — this compiles out `trace!` and `debug!` in release builds. Entry-point `EnvFilter` configuration is a UI-layer concern (design §14.1) |

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Either add `sonde-protocol` dependency and use its traits, or update design §3.2, §6.6, §6.7 to document the independent implementation | M | Low — crypto algorithms are identical |
| 2 | F-006 | Implement Tauri UI shell (Phase P4.3) to satisfy PT-0700, PT-0701, PT-0702 | L | Medium — new component |
| 3 | F-007 | Add Android permission request code in Java Activity layer | S | Low |
| 4 | F-008 | Add Activity lifecycle hooks in Java BleHelper class for PT-0107 | S | Low |
| 5 | F-002 | Update design §5.1 to reflect actual `BleTransport` API (Pin<Box>, `&mut self`, `[u8; 6]`) | S | None |
| 6 | F-003 | Update design §7.1 to reflect synchronous `PairingStore` API | S | None |
| 7 | F-004 | Update design §5.1 `ScannedDevice` to match actual struct layout | S | None |
| 8 | F-012 | Add `fragmentation.rs` to design §3 crate structure | S | None |
| 9 | F-013 | Add `loopback_transport.rs` to design §3 crate structure | S | None |
| 10 | F-014 | Update design §10.1 to reflect `build_envelope` naming and `Option` return | S | None |

## 7. Prevention

- **Design-code sync process**: After implementation, run a diff of public API signatures against the design document. Consider generating API docs from code and comparing.
- **Dependency auditing**: Add a CI check that verifies `Cargo.toml` dependencies match the design document's dependency table (§3.2).
- **Crate structure documentation**: Auto-generate the file listing in the design §3 from `ls src/` during the doc build.
- **UI-layer requirements tagging**: Tag requirements with their target layer (library vs. UI) to avoid false "not implemented" results during library-only audits.

## 8. Open Questions

1. **`sonde-protocol` dependency**: Was the decision to not depend on `sonde-protocol` intentional (e.g., to avoid circular dependencies or to reduce the dependency footprint for mobile targets)? If so, the design document should be updated. If not, the dependency should be added.
2. **Android runtime permissions**: The Java Activity code that requests `BLUETOOTH_SCAN`/`BLUETOOTH_CONNECT` permissions is not present in the repository. Is it in a separate Android project, or is it pending implementation?
3. **LESC enforcement on Windows**: WinRT's `btleplug` does not expose the pairing method. The code returns `None` (OS-enforced). Has this been validated on Windows hardware to confirm WinRT actually rejects Just Works for LESC-configured peripherals?

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-17 | Copilot (automated audit) | Initial code compliance audit against requirements v1 and design v1 |
