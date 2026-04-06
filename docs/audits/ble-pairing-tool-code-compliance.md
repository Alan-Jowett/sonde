<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->

# BLE Pairing Tool — Code Compliance Audit (D8 / D9 / D10)

> **Audit date:** 2026-03-20
> **Snapshot:** HEAD of working tree at audit time
> **Scope:** `crates/sonde-pair/src/`, `crates/sonde-pair-ui/src-tauri/src/`, `crates/sonde-pair/java/`
> **Specification:** `docs/ble-pairing-tool-requirements.md` (PT-0100 – PT-1206)
> **Design:** `docs/ble-pairing-tool-design.md`

---

## Legend

| Code | Meaning |
|------|---------|
| **D8** | Requirement exists in spec but is **not implemented** in code |
| **D9** | Code implements behaviour that is **not documented** in spec |
| **D10** | Code **violates a constraint** stated in the spec |
| ✅ | Requirement fully satisfied |
| ⚠️ | Partially satisfied — noted inline |

---

## 1  Forward Traceability (Spec → Code)

### 1.1  Platform and Architecture (PT-0100 – PT-0108)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0100 | Supported platforms | ✅ | Desktop: `btleplug_transport.rs` (Windows/Linux/macOS). Android: `android_transport.rs` + JNI `BleHelper.java`. No iOS-specific APIs used. |
| PT-0101 | Rust-first implementation | ✅ | All protocol logic, crypto, validation, and persistence are in Rust. UI layer (`lib.rs` in `sonde-pair-ui`) is a thin Tauri shell. |
| PT-0102 | Platform isolation | ✅ | `BleTransport` trait in `transport.rs`; `PairingStore` trait in `store.rs`. Core crate has no platform deps. Mock implementations exist for both. |
| PT-0103 | Crate placement | ✅ | `sonde-pair` is a workspace member. `Cargo.toml` has no dependency on `sonde-gateway`, `sonde-node`, or `sonde-modem`. |
| PT-0104 | Separation of concerns | ⚠️ **D10** | See **D10-001**. UI layer directly calls `phase1::pair_with_gateway()` and `phase2::provision_node()`. While protocol *logic* is in Rust core, the UI command handlers instantiate transports, stores, and RNG, then pass mutable references. A service/controller abstraction is absent. The spec requires "No BLE platform calls appear in protocol logic modules" (satisfied) and "No UI code appears in protocol or transport modules" (satisfied), but the spirit of the separation (UI as a thin shell over a service layer) is stretched. |
| PT-0105 | Android BLE runtime permissions | ✅ | `BleHelper.java` `requireBlePermissions()` checks `BLUETOOTH_SCAN` + `BLUETOOTH_CONNECT` (API 31+) and `ACCESS_FINE_LOCATION` (API 23–30). Throws descriptive exception if denied. |
| PT-0106 | LESC Numeric Comparison | ⚠️ **D8** | See **D8-001**. `BleHelper.java` calls `createBond()` (LESC bonding), but no programmatic check rejects a Just Works fallback. The `BleTransport` trait has no `PairingMethod` signal. Desktop `btleplug` has no bonding API at all. |
| PT-0107 | Android activity lifecycle | ⚠️ | `AndroidBleTransport::on_pause()` calls `stopScan()` only; no disconnect on pause. Partial coverage. |
| PT-0108 | JNI classloader caching | ✅ | `android_transport.rs`: `cache_vm()` + `cache_helper_class()` called from `JNI_OnLoad`. `android_store.rs`: analogous `cache_vm()` + `cache_store_class()`. `GlobalRef` used for class references. |

### 1.2  Device Discovery (PT-0200 – PT-0202)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0200 | BLE scanning | ✅ | `discovery.rs` scans for both `GATEWAY_SERVICE_UUID` and `NODE_SERVICE_UUID`. `DeviceScanner::start()` passes both UUIDs. `service_type()` classifies devices. |
| PT-0201 | Device presentation | ✅ | `ScannedDevice` struct exposes `name`, `address`, `rssi`, `service_uuids`. UI `DeviceInfo` serializes name, service type, and RSSI. |
| PT-0202 | Scan lifecycle | ✅ | Start/stop via `DeviceScanner`. Default scan timeout 30 s. Stale eviction at 10 s. Tests cover all. |

### 1.3  Phase 1 — Gateway Pairing (PT-0300 – PT-0304)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0300 | BLE connection and MTU | ✅ | `phase1.rs` checks `mtu < BLE_MTU_MIN (247)`, disconnects with `MtuTooLow` on failure. |
| PT-0301 | Gateway authentication | ✅ | 32-byte challenge via `RngProvider`, writes `REQUEST_GW_INFO`, reads with 5 s timeout, verifies Ed25519 signature over `(challenge ‖ gateway_id)`. Tests cover timeout and signature failure. |
| PT-0302 | TOFU | ✅ | `phase1.rs` checks stored gateway identity; rejects on public key mismatch (`PublicKeyMismatch`); pins on first contact. `save_gateway_identity()` called before full artifact save. |
| PT-0303 | Phone registration | ✅ | Ephemeral X25519 keypair generated per attempt. `REGISTER_PHONE` written. 30 s timeout. `PHONE_REGISTERED` decrypted via ECDH + HKDF + AES-256-GCM. `phone_psk`, `phone_key_hint`, `rf_channel` extracted. Error mapping: `0x02` → `RegistrationWindowClosed`, `0x03` → `GatewayAlreadyPaired`. All fields persisted via `save_artifacts()`. |
| PT-0304 | Ephemeral key zeroing | ✅ | Ephemeral X25519 private key: `Zeroizing<[u8; 32]>`. Shared secret: `Zeroizing`. AES key: `Zeroizing`. All dropped/zeroed after use. |

### 1.4  Phase 2 — Node Provisioning (PT-0400 – PT-0408)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0400 | Phase 1 prerequisite check | ✅ | `phase2.rs` line 49: `store.load_artifacts()?.ok_or(PairingError::NotPaired)?`. Error message: "not paired — run Phase 1 (gateway pairing) first". |
| PT-0401 | Node BLE connection and MTU | ✅ | Same MTU ≥ 247 check as Phase 1. |
| PT-0402 | Node PSK generation | ✅ | 32-byte node PSK from `RngProvider`. `node_key_hint = compute_key_hint()` = `u16::from_be_bytes(SHA-256(psk)[30..32])`. |
| PT-0403 | PairingRequest CBOR | ✅ | `cbor.rs` encodes deterministic CBOR with integer keys 1–6 in ascending order. `node_id` validated 1–64 bytes. `rf_channel` validated 1–13. Tests confirm deterministic encoding. |
| PT-0404 | Phone HMAC authentication | ✅ | `phase2.rs` lines 72–77: `hmac_sha256(phone_psk, cbor)`, prepends `phone_key_hint[2]`, appends HMAC[32]. |
| PT-0405 | Gateway public key encryption | ✅ | Fresh ephemeral X25519 per attempt. Ed25519→X25519 with low-order rejection. HKDF salt = `gateway_id`, info = `"sonde-node-pair-v1"`. AES-256-GCM AAD = `gateway_id`. Payload = `eph_public ‖ nonce ‖ ciphertext`. |
| PT-0406 | Encrypted payload size validation | ✅ | `phase2.rs` line 145: checks `total_encrypted_len > PEER_PAYLOAD_MAX_LEN (202)`, returns `PayloadTooLarge { size, max }`. Checked **before** BLE write. |
| PT-0407 | NODE_PROVISION transmission | ✅ | Assembles `node_key_hint[2] ‖ node_psk[32] ‖ rf_channel[1] ‖ payload_len[2 BE] ‖ encrypted_payload`. 5 s timeout on `NODE_ACK`. Status mapping: `0x00` → Success, `0x01` → AlreadyPaired, `0x02` → StorageError. |
| PT-0408 | Node PSK zeroing | ✅ | `node_psk: Zeroizing<[u8; 32]>`, dropped after function returns. Ephemeral keys also zeroed. |

### 1.5  Error Handling (PT-0500 – PT-0502)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0500 | Error classification | ✅ | `PairingError` enum (32 variants) groups errors by: device-level (`AdapterNotFound`, `BluetoothDisabled`, `DeviceOutOfRange`), transport-level (`ConnectionFailed`, `MtuTooLow`, `GattWriteFailed`, `IndicationTimeout`), protocol-level (`SignatureVerificationFailed`, `DecryptionFailed`, `RegistrationWindowClosed`). |
| PT-0501 | Actionable error messages | ⚠️ **D10** | See **D10-002**. Most error messages are descriptive but several lack suggested operator actions. E.g., `IndicationTimeout` says "indication not received before timeout" with no next step. `SignatureVerificationFailed` says "Ed25519 signature verification failed" but omits "possible impersonation" guidance. The spec requires *every* message to include an actionable sentence. |
| PT-0502 | No partial state on failure | ✅ | Phase 1: gateway identity pinned only after full success. `do_pair_with_gateway` is an inner function; `pair_with_gateway` always disconnects on any exit path. Phase 2: store is read-only (`&dyn PairingStore`), no writes possible. Tests verify no partial state. |

### 1.6  Idempotency and Safety (PT-0600 – PT-0601)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0600 | Re-run safety | ✅ | Tests exist (`t_pt_600_repeated_phase1_does_not_corrupt_state`). Phase 2 returns `AlreadyPaired` if node is configured. |
| PT-0601 | Already-paired detection | ⚠️ | `phase1.rs` emits `tracing::warn!` if gateway identity exists. However the UI layer (`sonde-pair-ui`) does **not** call `is_already_paired()` before launching Phase 1, so the operator never sees a confirmation prompt. The warning only appears in the log capture. See **D8-002**. |

### 1.7  User Experience (PT-0700 – PT-0702)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0700 | Minimum UI surface | ✅ | Tauri commands cover: `start_scan`, `stop_scan`, `get_devices`, `pair_gateway`, `provision_node`, `get_phase`, `get_pairing_status`, `clear_pairing`. `provision_node` accepts `node_id`. |
| PT-0701 | Phase indication | ⚠️ | `AppState.phase` tracks "Idle", "Scanning", "Pairing", "Complete", "Error: …". Missing intermediate states: "Connecting", "Authenticating", "Registering", "Provisioning". See **D8-003**. |
| PT-0702 | Verbose diagnostic mode | ⚠️ **D8** | See **D8-004**. `tracing` is configured at build time with `sonde_pair=debug`. There is a `get_logs()` command that captures trace output. However, there is **no runtime toggle** to enable/disable verbose mode — the log level is baked in. The spec requires an opt-in toggle that is disabled by default. |

### 1.8  Persistence and Storage (PT-0800 – PT-0804)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0800 | Pairing store contents | ✅ | `PairingArtifacts` struct stores all six fields: `gw_public_key`, `gateway_id`, `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`. Verified in `file_store.rs`, `android_store.rs`. |
| PT-0801 | Platform-appropriate secure storage | ✅ | Android: `EncryptedSharedPreferences` (AES-256-GCM + Android Keystore). Windows: `DpapiPskProtector` (DPAPI user-scope). Linux: `SecretServicePskProtector` (D-Bus keyring). Desktop fallback: `%APPDATA%\sonde\` with restricted file permissions. |
| PT-0802 | Storage abstraction | ✅ | `PairingStore` trait with `save_artifacts`, `load_artifacts`, `clear`, `load_gateway_identity`, `save_gateway_identity`. `MemoryPairingStore` for tests. Platform impls exist. |
| PT-0803 | Corruption handling | ⚠️ | `StoreCorrupted` error returned for invalid JSON. Error message includes `"delete or fix {path}"`. However the UI does **not** offer a "reset store" option in response to corruption — it only has `clear_pairing`. See **D8-005**. |
| PT-0804 | No node PSK persistence | ✅ | `node_psk` never appears in any store implementation. Test `no_node_psk_in_json()` verifies absence. `android_store.rs` doc comment explicitly notes `node_psk` is never persisted. |

### 1.9  Security (PT-0900 – PT-0904)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-0900 | No key material in logs | ✅ | `PairingArtifacts` Debug impl redacts `phone_psk` as "[REDACTED]". `tracing` calls log operation outcomes and lengths, never key bytes. `Zeroizing` types don't implement `Display`. |
| PT-0901 | CSPRNG for all randomness | ✅ | `OsRng` uses `getrandom::fill()`. No `rand::rng()` calls anywhere. All random values (challenges, ephemeral keys, PSKs, nonces) flow through `RngProvider`. |
| PT-0902 | Ed25519→X25519 conversion safety | ✅ | `crypto.rs` `ed25519_to_x25519_public()`: decompresses to Edwards, converts to Montgomery, rejects all-zero result (identity/low-order point). Returns `InvalidPublicKey("low-order point produces all-zero X25519 key")`. |
| PT-0903 | Clearly non-zero test keys | ✅ | All tests use `[0x42u8; 32]` for PSKs and keys. No `[0u8; 32]` used as a test key value (only as zero-initialization buffers that are immediately filled). |
| PT-0904 | BLE pairing mode enforcement | **D8** | See **D8-001**. The `BleTransport` trait has no pairing-method signal. No `PairingMethod` enum exists. Android `BleHelper.java` calls `createBond()` but does not programmatically verify that LESC Numeric Comparison (not Just Works) was negotiated. Desktop `btleplug` has no bonding support. No tests cover this requirement. |

### 1.10  Non-Functional (PT-1000 – PT-1004)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-1000 | Transient BLE failure tolerance | ✅ | After any error, phase returns to "Error: …". UI allows restarting scan. Transport always disconnects on error paths. |
| PT-1001 | No resource leaks on failure | ✅ | `pair_with_gateway` and `provision_node` both call `transport.disconnect().await.ok()` on every exit. `BtleplugTransport::Drop` attempts disconnect. GATT subscriptions cleaned up on disconnect. |
| PT-1002 | Deterministic timeouts | ✅ | **D10-003 RESOLVED** (issue #655). `GW_INFO_RESPONSE`: 45 s ✅. `PHONE_REGISTERED`: 30 s ✅. `NODE_ACK`: 5 s ✅. Scan: 30 s ✅. BLE connection: both platforms use 30 s ✅ (spec updated from 10 s to 30 s). |
| PT-1003 | No implicit retries | ✅ | No protocol-level retry logic found. All failures are immediately reported. |
| PT-1004 | Reusable core | ✅ | `sonde-pair` crate has no UI or platform deps. Used by `sonde-pair-ui` (Tauri frontend) and by the built-in test suite (148+ tests across modules). |

### 1.11  Cryptographic Requirements (PT-1100 – PT-1103)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-1100 | Required primitives | ✅ | All eight primitives implemented: Ed25519 (`ed25519-dalek`), X25519 (`curve25519-dalek`), Ed→X conversion, HKDF-SHA256 (`hkdf`), AES-256-GCM (`aes-gcm`), HMAC-SHA256 (`hmac`), SHA-256 (`sha2`), CSPRNG (`getrandom`). |
| PT-1101 | HKDF parameters | ✅ | Phase 1: info = `b"sonde-phone-reg-v1"`, salt = `gateway_id`. Phase 2: info = `b"sonde-node-pair-v1"`, salt = `gateway_id`. Both produce 32-byte keys. |
| PT-1102 | AES-GCM AAD | ✅ | Phase 1 decrypt and Phase 2 encrypt both use `gateway_id` as AAD. |
| PT-1103 | Deterministic CBOR | ✅ | `cbor.rs` encodes integer keys in ascending order (1–6). Test `deterministic_encoding_same_twice` verifies byte-identical output. `ciborium` used with manual key ordering. |

### 1.12  Testing (PT-1200 – PT-1206)

| Req | Title | Status | Evidence / Finding |
|-----|-------|--------|-------------------|
| PT-1200 | Mocked BLE transport for CI | ✅ | `MockBleTransport` in `transport.rs` implements `BleTransport`. Supports queued responses and error injection (`IndicationTimeout`, connection failure via `PairingError`). All unit tests run against mock. |
| PT-1201 | Phase 1 happy path | ✅ | `t_pt_200_phase1_happy_path` (or equivalent named test in `phase1.rs`). Verified: signature, decryption, artifact persistence. |
| PT-1202 | Phase 1 error paths | ✅ | Tests cover: signature failure, `ERROR(0x02)` window closed, `ERROR(0x03)` already paired, decryption failure, TOFU rejection, GW_INFO timeout (5 s), PHONE_REGISTERED timeout (30 s). |
| PT-1203 | Phase 2 happy path | ✅ | Test in `phase2.rs`. NODE_ACK(0x00) handled, success output verified. |
| PT-1204 | Phase 2 error paths | ✅ | Tests cover: AlreadyPaired, StorageError, timeout, NotPaired, payload too large. |
| PT-1205 | Input validation | ✅ | `validation.rs` tests: empty node_id, >64 bytes, rf_channel 0, rf_channel >13, key_hint derivation. `cbor.rs` tests: deterministic CBOR round-trip. |
| PT-1206 | Manual hardware testing | **D8** | See **D8-006**. No test log or evidence of physical hardware testing. This is expected for a pre-release codebase. |

---

## 2  D8 Findings — Spec Requirements Not Implemented

### D8-001: LESC Numeric Comparison enforcement not observable (PT-0106, PT-0904)

**Severity:** High
**Requirements:** PT-0106, PT-0904

The spec requires the `BleTransport` trait to expose an observable signal indicating which BLE pairing method was negotiated (`NumericComparison` vs `JustWorks`). The protocol logic must reject Just Works as a connection failure.

**Current state:**
- The `BleTransport` trait has no `PairingMethod` return value or callback.
- `BleHelper.java` calls `createBond()` but does not verify the actual pairing method used.
- Desktop `btleplug` has no bonding API; LESC pairing is delegated entirely to the OS stack with no verification.
- `MockBleTransport` has no `PairingMethod` field, so no tests can cover acceptance/rejection of pairing methods.

**Recommendation:** Add a `pairing_method()` accessor or modify `connect()` to return a `ConnectionInfo { mtu: u16, pairing_method: PairingMethod }`. Add `PairingMethod::NumericComparison` and `PairingMethod::JustWorks` enum. Verify on Android that bonded transport was LESC, reject otherwise.

---

### D8-002: UI does not prompt on already-paired re-pairing (PT-0601)

**Severity:** Low
**Requirement:** PT-0601

The spec says the tool SHOULD warn the operator and offer proceed/cancel when a gateway identity is already stored. Currently, `phase1.rs` emits a `tracing::warn!()` but the UI never calls `is_already_paired()` to present a confirmation dialog.

**Recommendation:** Add an `is_paired` check in the `pair_gateway` Tauri command and return a distinguishable result (e.g., a `"confirm_overwrite"` status) so the frontend can prompt before proceeding.

---

### D8-003: Phase indication lacks intermediate states (PT-0701)

**Severity:** Low
**Requirement:** PT-0701

The spec requires distinct states: Idle, Scanning, Connecting, Authenticating, Registering, Provisioning, Complete, Error. The UI only tracks "Idle", "Scanning", "Pairing", "Complete", and "Error: …". Missing: Connecting, Authenticating, Registering, Provisioning.

**Recommendation:** Have `phase1::pair_with_gateway` and `phase2::provision_node` accept a callback or channel to report sub-phase transitions. Update `AppState.phase` accordingly.

---

### D8-004: No runtime toggle for verbose diagnostic mode (PT-0702)

**Severity:** Medium
**Requirement:** PT-0702

The spec requires an opt-in verbose mode activated by an explicit toggle, disabled by default. Currently, `tracing` is initialized at `sonde_pair=debug` level unconditionally. The `get_logs()` command returns captured output, but there is no way for the operator to toggle verbosity at runtime.

**Recommendation:** Initialize tracing at `info` level by default. Add a `set_verbose(enabled: bool)` Tauri command that swaps the `EnvFilter` to `sonde_pair=trace` dynamically using `tracing_subscriber::reload`.

---

### D8-005: Corruption recovery UX incomplete (PT-0803)

**Severity:** Low
**Requirement:** PT-0803

The spec says the tool must offer to reset the store on corruption. `FilePairingStore` returns `StoreCorrupted` with a message suggesting to "delete or fix" the file, but the UI does not present a reset prompt. The `clear_pairing` command exists but is not triggered automatically on corruption.

**Recommendation:** In the UI, catch `StoreCorrupted` errors and present a "Reset pairing store?" confirmation dialog before calling `clear_pairing`.

---

### D8-006: No recorded hardware test log (PT-1206)

**Severity:** Informational
**Requirement:** PT-1206

This is a pre-release gate requirement. No hardware test log was found in the repository. Expected for current development stage; must be completed before release.

---

## 3  D9 Findings — Code Behaviour Not in Spec

### D9-001: Loopback TCP transport

**Location:** `loopback_transport.rs`
**Feature flag:** `loopback-ble`

A TCP-based BLE transport emulation exists for hardware-free integration testing. It creates a synthetic "Sonde-GW-Loopback" device and tunnels GATT envelopes over TCP. This is a useful test utility but is not documented in any requirement or design document.

**Impact:** None (test-only). No action required, but consider documenting in the design doc for completeness.

---

### D9-002: BLE helper bond removal via reflection

**Location:** `BleHelper.java` `removeBond()`
**Behaviour:** Before each `connect()`, the Java code removes any existing Bluetooth bond using the hidden `removeBond()` API via Java reflection.

The requirements and design documents do not mention this pre-connect bond removal strategy. It is motivated by the modem not persisting bonds across reboots, but this is an implementation detail with potential side effects (removes bonds from other BLE profiles).

**Impact:** Low. Behaviour is correct for the modem's limitations but should be documented.

---

### D9-003: File store backward compatibility (plaintext → encrypted migration)

**Location:** `file_store.rs`
**Behaviour:** When a `PskProtector` is present and the store file contains a plaintext `phone_psk` (legacy format), the code reads it transparently and re-encrypts on next save. This silent migration path is not documented in requirements.

**Impact:** Low. Defensive and correct, but should be noted in the design document.

---

### D9-004: `get_logs()` command for log capture

**Location:** `sonde-pair-ui/src-tauri/src/lib.rs`
**Behaviour:** The UI backend captures all `tracing` output into an `Arc<Mutex<Vec<String>>>` and exposes it via the `get_logs()` Tauri command. The frontend can poll this to display diagnostic output.

This mechanism is not described in the requirements (PT-0702 describes a "verbose mode toggle", not a log-capture buffer).

**Impact:** Low. Useful feature, but the toggle mechanism is missing (see D8-004).

---

### D9-005: `PskProtector` trait and `SecretServicePskProtector`

**Location:** `file_store.rs`, `secret_service_store.rs`
**Behaviour:** A `PskProtector` trait abstracts PSK encryption at rest. A Linux-specific `SecretServicePskProtector` uses the D-Bus Secret Service API (GNOME Keyring / KWallet). The requirements mention Android Keystore and DPAPI but not Linux Secret Service.

**Impact:** None. Adds value beyond spec. Consider updating PT-0801 to mention Linux support.

---

## 4  D10 Findings — Constraint Violations

### D10-001: UI layer directly orchestrates protocol (PT-0104 — partial)

**Severity:** Low
**Requirement:** PT-0104 acceptance criterion 3: "No UI code appears in protocol or transport modules."

This criterion is **satisfied** (no UI code leaks into the core). However, the converse architectural intent — that the UI is a "thin shell over Rust commands" — is stretched. The Tauri command handlers directly instantiate platform transports, create RNG instances, and pass `&mut` references to `phase1`/`phase2` functions. A service-layer abstraction would better isolate the UI from protocol orchestration details.

**Status:** Borderline. The literal acceptance criteria pass, but the design intent of a clean service boundary is not fully realized. Flagged as informational D10.

---

### D10-002: Error messages missing actionable guidance (PT-0501)

**Severity:** Medium
**Requirement:** PT-0501: "Every error message MUST include a suggested operator action."

Several `PairingError` variants lack actionable guidance:

| Error variant | Current message | Missing guidance |
|---------------|----------------|------------------|
| `IndicationTimeout` | "indication not received before timeout" | Should say *which* operation timed out and suggest moving closer / retrying |
| `SignatureVerificationFailed` | "Ed25519 signature verification failed" | Should say "possible impersonation — do not proceed" |
| `ConnectionDropped` | "BLE connection dropped unexpectedly" | Should suggest "move closer and retry" |
| `DeviceNotFound` | "target device not found during scan" | Should suggest "start a new scan" |
| `DeviceOutOfRange` | "target device is out of BLE range" | Should suggest "move closer to the device" |
| `InvalidKeyHint` | "invalid key hint" | Should explain implications |

**Recommendation:** Enhance `#[error(...)]` strings to include a suggested action after the diagnostic. E.g., `"Ed25519 signature verification failed — possible impersonation, do not proceed"`.

---

### D10-003: Android BLE connection timeout is 30 s, spec says 10 s (PT-1002)

**Severity:** Low — **RESOLVED** (issue #655)
**Requirement:** PT-1002: "BLE connection establishment 30 s."

`android_transport.rs` uses `CONNECT_TIMEOUT_MS = 30_000` (30 s), documented as including LESC bonding time. PT-1002 has been updated from 10 s to 30 s to accommodate the LESC Numeric Comparison bonding flow, which requires human confirmation. Both `android_transport.rs` and `btleplug_transport.rs` now use 30 s consistently.

---

### D10-004: No constant-time comparison for TOFU public key check

**Severity:** Low (mitigated by protocol design)
**Requirement:** Implicit from security.md and PT-0302

`phase1.rs` TOFU check uses standard `!=` comparison for the 32-byte `public_key` arrays. This is not constant-time. While timing side-channels are unlikely to be exploitable over BLE (the comparison is local, not used in a tight authentication loop), the spec's security conventions prefer constant-time crypto operations.

**Recommendation:** Use `subtle::ConstantTimeEq` for public key comparisons. Add `subtle` as a dependency.

---

## 5  Coverage Summary

### Requirements coverage

| Category | Total | ✅ Implemented | ⚠️ Partial | ❌ D8 (Missing) |
|----------|-------|---------------|------------|-----------------|
| Platform / Arch (PT-01xx) | 9 | 6 | 2 | 1 |
| Discovery (PT-02xx) | 3 | 3 | 0 | 0 |
| Phase 1 (PT-03xx) | 5 | 5 | 0 | 0 |
| Phase 2 (PT-04xx) | 9 | 9 | 0 | 0 |
| Error handling (PT-05xx) | 3 | 2 | 1 | 0 |
| Idempotency (PT-06xx) | 2 | 1 | 1 | 0 |
| UX (PT-07xx) | 3 | 1 | 1 | 1 |
| Storage (PT-08xx) | 5 | 4 | 1 | 0 |
| Security (PT-09xx) | 5 | 4 | 0 | 1 |
| Non-functional (PT-10xx) | 5 | 4 | 1 | 0 |
| Crypto (PT-11xx) | 4 | 4 | 0 | 0 |
| Testing (PT-12xx) | 7 | 6 | 0 | 1 |
| **Totals** | **60** | **49** | **7** | **4** |

- **Full compliance:** 49 / 60 (82%)
- **Partial compliance:** 7 / 60 (12%)
- **Not implemented:** 4 / 60 (7%)

### Finding counts

| Type | Count | High | Medium | Low | Info |
|------|-------|------|--------|-----|------|
| D8 (not implemented) | 6 | 1 | 1 | 3 | 1 |
| D9 (undocumented) | 5 | 0 | 0 | 5 | 0 |
| D10 (constraint violation) | 4 | 0 | 1 | 3 | 0 |
| **Total** | **15** | **1** | **2** | **11** | **1** |

---

## 6  Prioritised Action Items

| Priority | Finding | Action |
|----------|---------|--------|
| **P0** | D8-001 (LESC enforcement) | Add `PairingMethod` to `BleTransport` trait; verify on Android; reject Just Works |
| **P1** | D10-002 (actionable errors) | Add operator guidance to all error message strings |
| **P1** | D8-004 (verbose toggle) | Add runtime log-level toggle via `tracing_subscriber::reload` |
| ~~P2~~ | ~~D10-003 (Android timeout)~~ | ~~Reconcile spec timeout (10 s) with LESC bonding budget (30 s)~~ — **RESOLVED** (issue #655) |
| **P2** | D8-002 (already-paired prompt) | Add `is_paired` check in UI before Phase 1 |
| **P2** | D8-003 (phase granularity) | Add sub-phase callbacks to protocol functions |
| **P3** | D8-005 (corruption UX) | Auto-offer store reset on corruption in UI |
| **P3** | D10-004 (constant-time compare) | Use `subtle::ConstantTimeEq` for TOFU key comparison |
| **P3** | D10-001 (service layer) | Introduce a `PairingService` abstraction between UI and core |
| **—** | D8-006 (hardware testing) | Pre-release gate — schedule before v1.0 |
| **—** | D9-001–005 | Document in design doc; no code changes needed |
