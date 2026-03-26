<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# BLE Pairing Tool Validation Specification

> **Document status:** Draft  
> **Scope:** Test plan for the Sonde BLE pairing tool.  
> **Audience:** Implementers (human or LLM agent) writing pairing tool tests.  
> **Related:** [ble-pairing-tool-requirements.md](ble-pairing-tool-requirements.md), [ble-pairing-protocol.md](ble-pairing-protocol.md)

---

## 1  Overview

This document defines test cases that validate the BLE pairing tool against the requirements in [ble-pairing-tool-requirements.md](ble-pairing-tool-requirements.md). Each test case is traceable to one or more requirements.

**Scope:** These are integration tests that exercise the pairing state machine through its external interfaces (BLE transport and persistence). Unit tests for internal modules are expected but are not specified here.

**Test harness:** All CI tests use a **mock BLE transport** (in-process implementation of the `BleTransport` trait) and a **mock pairing store** (in-memory implementation of the `PairingStore` trait). No real BLE hardware is needed. Manual tests against physical hardware are specified separately.

**Architecture requirements:** PT-0100 (supported platforms), PT-0101 (Rust-first implementation), PT-0102 (platform isolation), PT-0103 (crate placement), and PT-0104 (separation of concerns) are structural constraints validated by CI build targets (Android `aarch64-linux-android`, Windows `x86_64-pc-windows-msvc`) and code review of `Cargo.toml` dependency graphs.  They do not have runtime test cases in this document.  PT-1004 (reusable core) is validated by T-PT-1004, which asserts the crate builds without platform features.

**Testing meta-requirement traceability:** The following mapping shows how requirements PT-1000–PT-1206 are satisfied by the test suites, structural coverage, and supporting CI/build checks described in this document:

| Meta-requirement | Description | Satisfied by |
|---|---|---|
| PT-1000 | Transient BLE failure tolerance | T-PT-300, T-PT-310, T-PT-800 |
| PT-1001 | No resource leaks on failure | T-PT-801 |
| PT-1002 | Deterministic timeouts | T-PT-310, T-PT-312, T-PT-313, T-PT-314, T-PT-802 |
| PT-1003 | No implicit retries | T-PT-803 |
| PT-1004 | Reusable core | T-PT-1004 |
| PT-1100 | Required cryptographic primitives | Structural coverage (see table below) |
| PT-1200 | Mocked BLE transport for CI | Test harness infrastructure (§2); all CI tests use `MockBleTransport` |
| PT-1201 | Phase 1 happy path | T-PT-208 |
| PT-1202 | Phase 1 error paths | T-PT-203, T-PT-204, T-PT-206, T-PT-209, T-PT-210, T-PT-211, T-PT-212 |
| PT-1203 | Phase 2 happy path | T-PT-311 |
| PT-1204 | Phase 2 error paths | T-PT-300, T-PT-310, T-PT-312, T-PT-313, T-PT-314 |
| PT-1205 | Input validation | T-PT-303, T-PT-304, T-PT-305, T-PT-306 |
| PT-1206 | Manual testing on physical hardware | Manual test procedures in §10 (T-PT-800–T-PT-807) |

**Cryptographic primitive coverage (PT-1100):** PT-1100 requires all eight cryptographic primitives to be implemented.  Rather than a single aggregate test, coverage is provided structurally by the test suite's dependency on each primitive:

| Primitive | Test(s) exercising it |
|---|---|
| Ed25519 signature verification | T-PT-202, T-PT-203 |
| X25519 ECDH | T-PT-208, T-PT-308 |
| Ed25519 → X25519 conversion | T-PT-309 |
| HKDF-SHA256 | T-PT-900, T-PT-901 |
| AES-256-GCM | T-PT-212, T-PT-902 |
| HMAC-SHA256 | T-PT-307 |
| SHA-256 | T-PT-303 |
| CSPRNG | T-PT-302, T-PT-702 |

**Test ID convention:** Test IDs follow the numeric pattern `T-PT-NNN`. When a test case is added after initial numbering to cover a gap between two adjacent IDs, an alphabetic suffix is used (e.g., `T-PT-208a` for a test inserted between T-PT-208 and T-PT-209).

---

## 2  Test environment

### 2.1  Mock BLE transport

An in-process `BleTransport` implementation that:

- Simulates BLE scan results (service UUIDs, advertising names, RSSI).
- Simulates GATT connections with configurable MTU negotiation.
- Queues indication responses (e.g., `GW_INFO_RESPONSE`, `PHONE_REGISTERED`, `NODE_ACK`).
- Captures outbound GATT writes (for assertion).
- Supports error injection: connection failure, timeout, malformed indication, mid-operation disconnect.

### 2.2  Mock pairing store

An in-memory `PairingStore` implementation that:

- Stores and retrieves pairing artifacts (`gw_public_key`, `gateway_id`, `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`).
- Supports clear/reset operations.
- Can be pre-loaded with test data or left empty.
- Can be configured to simulate corruption (return errors on read).

### 2.3  Test key material

All tests use clearly non-zero keys to avoid normalizing insecure patterns:

```
TEST_PSK:        [0x42u8; 32]
TEST_GATEWAY_ID: [0xAAu8; 16]
TEST_CHALLENGE:  [0xBBu8; 32]
TEST_NODE_PSK:   [0x55u8; 32]
```

Ed25519 and X25519 keypairs are generated from fixed seeds for reproducibility.

### 2.4  Test gateway helper

A helper that constructs valid BLE indication payloads:

```
TestGateway {
    signing_key: Ed25519SigningKey,
    gateway_id: [u8; 16],
    eph_keypair: X25519EphemeralKeyPair,

    fn gw_info_response(challenge: &[u8; 32]) -> Vec<u8>
    fn phone_registered(eph_public: &[u8; 32], phone_psk: &[u8; 32],
                        phone_key_hint: u16, rf_channel: u8) -> Vec<u8>
    fn error_response(code: u8) -> Vec<u8>
}
```

### 2.5  Test node helper

A helper that constructs valid node BLE indication payloads:

```
TestNode {
    fn node_ack(status: u8) -> Vec<u8>
}
```

---

## 3  Device discovery tests

### T-PT-100  BLE scan discovers gateway service UUID

**Validates:** PT-0200

**Procedure:**
1. Configure mock transport with two advertised devices: one with Gateway Pairing Service UUID (`0000FE60-…`), one with an unrelated UUID.
2. Start a scan.
3. Assert: scan results contain the gateway device.
4. Assert: scan results do not contain the unrelated device.

---

### T-PT-101  BLE scan discovers node service UUID

**Validates:** PT-0200

**Procedure:**
1. Configure mock transport with two advertised devices: one with Node Provisioning Service UUID (`0000FE50-…`), one with an unrelated UUID.
2. Start a scan.
3. Assert: scan results contain the node device.
4. Assert: scan results do not contain the unrelated device.

---

### T-PT-102  Non-Sonde devices filtered from results

**Validates:** PT-0200

**Procedure:**
1. Configure mock transport with three devices: one gateway, one node, one generic BLE peripheral (heart rate sensor UUID).
2. Start a scan.
3. Assert: only the gateway and node appear in results.

---

### T-PT-103  Device presentation (name, type, RSSI)

**Validates:** PT-0201

**Procedure:**
1. Configure mock transport with a gateway (name `"sonde-gw-01"`, RSSI −55 dBm) and a node (name `"sonde-ABCD"`, RSSI −70 dBm).
2. Start a scan.
3. Assert: each result includes the advertising name, service type (gateway vs. node), and RSSI.
4. Assert: gateway and node are distinguishable by service type.

---

### T-PT-104  Scan timeout and stale device eviction

**Validates:** PT-0202

**Procedure:**
1. Configure mock transport with one gateway device.
2. Start a scan with timeout = 15 s.
3. After 1 s, stop advertising the gateway in the mock.
4. Assert: after 10 s of no advertisements, the device is removed from results.
5. Assert: scan stops automatically after the configured timeout (15 s).

---

### T-PT-105  BLE permission dialog shown on Android

**Validates:** PT-0105  
**Type:** Manual / platform test (requires Android device)

**Procedure:**
1. Install the app on an Android 12+ device with BLE permissions **not** pre-granted (fresh install or permissions revoked via Settings).
2. Launch the app.
3. Assert: the system permission dialog appears requesting `BLUETOOTH_SCAN` and `BLUETOOTH_CONNECT`.
4. Grant the permissions.
5. Initiate a BLE scan.
6. Assert: scan starts successfully with no permission errors.

---

### T-PT-106  BLE permission denial produces actionable error

**Validates:** PT-0105  
**Type:** Manual / platform test (requires Android device)

**Procedure:**
1. Install the app on an Android 12+ device with BLE permissions not pre-granted.
2. Launch the app.
3. When the system permission dialog appears, **deny** the permissions.
4. Initiate a BLE scan.
5. Assert: the UI displays an actionable error indicating BLE permissions are required.
6. Assert: the app does not crash or silently fail.

---

### T-PT-107  BLE permissions on Android 6–11 (location)

**Validates:** PT-0105  
**Type:** Manual / platform test (requires Android 6–11 device)

**Procedure:**
1. Install the app on an Android 6–11 (API 23–30) device with location permissions not pre-granted.
2. Launch the app.
3. Assert: the system permission dialog appears requesting `ACCESS_FINE_LOCATION`.
4. Grant the permission.
5. Initiate a BLE scan.
6. Assert: scan starts successfully with no permission errors.

---

### T-PT-108  LESC Numeric Comparison pairing used

**Validates:** PT-0106, PT-0904  
**Type:** Manual / platform test (runs against real BLE hardware)

**Procedure:**
1. Connect the pairing tool to a modem (or test peripheral) configured for LESC with `DisplayYesNo` I/O capability.
2. Initiate Phase 1 gateway pairing from the pairing tool UI.
3. Observe the platform pairing UX and/or system logs:
   - Assert: LESC pairing is established — either the platform initiates bonding (e.g., Android `createBond()`) or the modem's server-initiated SMP Security Request triggers the OS pairing dialog (e.g. WinRT, CoreBluetooth).
   - Assert: the pairing method negotiated is Numeric Comparison (a 6-digit comparison dialog is shown, not an implicit "Just Works" pairing).
4. Accept the Numeric Comparison dialog on both sides (host and peripheral, as applicable).
5. Assert: pairing completes successfully (bond is created at the OS level) and subsequent GATT operations from the pairing tool proceed without additional pairing prompts.

---

### T-PT-109  Just Works fallback rejected

**Validates:** PT-0106, PT-0904  
**Type:** CI / mock transport test

**Procedure:**
1. Configure the mock `BleTransport` to simulate a peripheral that only supports Just Works pairing (no `DisplayYesNo` I/O capability), and to fail the secure connection / bonding attempt (for example, make `connect()` or the platform-specific bond API return an error) **before** any GATT characteristic writes are permitted.
2. Initiate Phase 1 gateway pairing.
3. Assert: the connection fails with an actionable error indicating that Numeric Comparison is required.
4. Assert at the transport mock that no PSK-bearing GATT operations occurred before the failure (for example, verify that `write_characteristic` was never called).

### T-PT-109a  OS-enforced pairing (`None`) accepted by `enforce_lesc`

**Validates:** PT-0904 (criterion 3)  
**Type:** CI / mock transport test

**Procedure:**
1. Configure the mock `BleTransport` with `pairing_method()` returning `None` (simulating a desktop transport where the OS manages LESC pairing, e.g. btleplug on WinRT).
2. Call `enforce_lesc` on the connected transport.
3. Assert: `enforce_lesc` returns `Ok(())` — the connection is not terminated.
4. Assert: the transport remains connected.

### T-PT-109b  Unknown pairing method rejected by `enforce_lesc`

**Validates:** PT-0904 (criterion 4)  
**Type:** CI / mock transport test

**Procedure:**
1. Configure the mock `BleTransport` with `pairing_method()` returning `Some(Unknown)`.
2. Call `enforce_lesc` on the connected transport.
3. Assert: `enforce_lesc` returns `Err(InsecurePairingMethod)`.
4. Assert: the transport is disconnected.

---

### T-PT-110  Minimum UI elements present

**Validates:** PT-0700  
**Type:** Manual / platform test

**Procedure:**
1. Launch the pairing tool.
2. Assert: all required UI elements are present: scan toggle, device list, device select, pair action, node ID input, status area, error display.
3. Assert: the UI does not include management, monitoring, or telemetry features.

---

### T-PT-111  Phase indication updates

**Validates:** PT-0701

**Procedure:**
1. Start a mock BLE scan.
2. Assert: status shows "Scanning".
3. Select a gateway device and initiate pairing.
4. Assert: status transitions through "Connecting" → "Authenticating" → "Registering" → "Complete" (or "Error" on failure).
5. Assert: phase transitions are immediate and unambiguous.

---

### T-PT-112  Verbose diagnostic mode

**Validates:** PT-0702

**Procedure:**
1. Enable verbose diagnostic mode (toggle or flag).
2. Run a complete Phase 1 pairing flow with mock transport.
3. Assert: verbose output includes raw BLE event names, message types, and timing.
4. Assert: verbose output does NOT include key material (PSKs, private keys, shared secrets).
5. Disable verbose mode and repeat.
6. Assert: no diagnostic output appears in default mode.

---

### T-PT-113  Android activity lifecycle disconnect

**Validates:** PT-0107  
**Type:** Manual / platform test (requires Android device)

**Procedure:**
1. Initiate Phase 1 on Android and establish a BLE connection.
2. Simulate an Activity pause event (user switches to another app).
3. Assert: the BLE connection is released within 5 s.
4. Simulate an Activity resume event (user returns to the app).
5. Assert: the app surfaces an error or returns to a scannable state (no crash, no hang).
6. Assert: no orphaned GATT connections remain.

---

### T-PT-114  JNI classloader caching on background threads

**Validates:** PT-0108  
**Type:** Manual / platform test (requires Android device or emulator)

**Procedure:**
1. On Android, invoke `AndroidBleTransport::from_cached_vm()` from a tokio background thread (not the main thread).
2. Assert: `BleHelper` is instantiated successfully (no `ClassNotFoundException`).
3. Invoke `AndroidPairingStore::from_cached_vm()` from a different tokio background thread.
4. Assert: `SecureStore` is instantiated successfully.

---

## 4  Phase 1 — Gateway pairing tests

### T-PT-200  MTU negotiation ≥ 247

**Validates:** PT-0300

**Procedure:**
1. Configure mock transport to negotiate MTU = 247.
2. Select the gateway device and initiate connection.
3. Assert: connection succeeds and proceeds to authentication.

---

### T-PT-201  MTU < 247 → disconnect + error

**Validates:** PT-0300

**Procedure:**
1. Configure mock transport to negotiate MTU = 185.
2. Select the gateway device and initiate connection.
3. Assert: tool disconnects immediately.
4. Assert: error message contains "MTU too low".

---

### T-PT-202  Gateway authentication happy path (signature verification)

**Validates:** PT-0301

**Procedure:**
1. Configure mock transport with a `TestGateway` that returns a valid `GW_INFO_RESPONSE` (correct Ed25519 signature over `challenge ‖ gateway_id`).
2. Initiate Phase 1.
3. Assert: tool writes `REQUEST_GW_INFO` containing a 32-byte challenge.
4. Assert: signature verification succeeds.
5. Assert: tool proceeds to phone registration.

---

### T-PT-203  Gateway authentication failure (bad signature)

**Validates:** PT-0301

**Procedure:**
1. Configure mock transport with a `GW_INFO_RESPONSE` containing an invalid signature (corrupted last byte).
2. Initiate Phase 1.
3. Assert: tool disconnects.
4. Assert: error message contains "gateway authentication failed".

---

### T-PT-204  GW_INFO_RESPONSE timeout (45 s)

**Validates:** PT-0301

**Procedure:**
1. Configure mock transport to never send `GW_INFO_RESPONSE`.
2. Initiate Phase 1.
3. Assert: after 45 s, the tool disconnects.
4. Assert: error message indicates timeout on gateway authentication.

---

### T-PT-205  TOFU — first connection persists public key

**Validates:** PT-0302

**Procedure:**
1. Start with an empty pairing store.
2. Complete a successful Phase 1 (phone registration) flow through to `PHONE_REGISTERED`.
3. Assert: `gw_public_key` and `gateway_id` are persisted in the pairing store.

---

### T-PT-206  TOFU — mismatched public key rejected

**Validates:** PT-0302

**Procedure:**
1. Pre-load pairing store with a `gw_public_key` from a previous successful pairing.
2. Configure mock transport with a gateway presenting a **different** public key.
3. Initiate Phase 1.
4. Assert: tool rejects the connection before proceeding to registration.
5. Assert: error message indicates public key mismatch.

---

### T-PT-207  TOFU — operator can clear pinned identity

**Validates:** PT-0302

**Procedure:**
1. Pre-load pairing store with a `gw_public_key`.
2. Invoke the clear/reset operation on the pairing store.
3. Assert: `gw_public_key` and `gateway_id` are removed.
4. Assert: a subsequent Phase 1 with a new gateway succeeds (TOFU accepts the new key).

---

### T-PT-208  Phone registration happy path (ECDH + decrypt)

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport with a `TestGateway` that returns a valid `PHONE_REGISTERED` (encrypted with the tool's ephemeral public key).
2. Complete gateway authentication, then proceed to registration.
3. Assert: tool writes `REGISTER_PHONE` with an ephemeral X25519 public key and operator label.
4. Assert: tool successfully decrypts `PHONE_REGISTERED`.
5. Assert: `phone_psk`, `phone_key_hint`, `rf_channel`, `gw_public_key`, and `gateway_id` are persisted.

---

### T-PT-208a  Phone label validation

**Validates:** PT-0303

**Procedure:**
1. Attempt REGISTER_PHONE with a label of exactly 64 bytes UTF-8.
2. Assert: the label is accepted and included in the GATT write.
3. Attempt REGISTER_PHONE with a label of 65 bytes UTF-8.
4. Assert: the tool rejects the label before BLE transmission with an error.
5. Attempt REGISTER_PHONE with an empty label (0 bytes).
6. Assert: the empty label is accepted.

---

### T-PT-209  ERROR(0x02) — registration window closed

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport to respond with `ERROR(0x02)` after `REGISTER_PHONE`.
2. Initiate Phase 1 through authentication.
3. Assert: tool disconnects.
4. Assert: error message contains "registration window not open".

---

### T-PT-210  ERROR(0x03) — already paired

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport to respond with `ERROR(0x03)` after `REGISTER_PHONE`.
2. Initiate Phase 1 through authentication.
3. Assert: tool disconnects.
4. Assert: error message contains "already paired".

---

### T-PT-211  PHONE_REGISTERED timeout (30 s)

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport to never send `PHONE_REGISTERED`.
2. Initiate Phase 1 through authentication and write `REGISTER_PHONE`.
3. Assert: after 30 s, the tool disconnects.
4. Assert: error message indicates timeout on phone registration.

---

### T-PT-212  Decryption failure (bad GCM tag)

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport to return a `PHONE_REGISTERED` payload with a corrupted GCM tag (flip last byte of ciphertext).
2. Initiate Phase 1 through authentication and write `REGISTER_PHONE`.
3. Assert: tool disconnects.
4. Assert: error message contains "ephemeral key mismatch" or "decryption failed".

---

### T-PT-213  Ephemeral key zeroing after use

**Validates:** PT-0304

**Procedure:**
1. Complete a successful Phase 1 flow.
2. Assert: the ephemeral X25519 private key, ECDH shared secret, and derived AES key are wrapped in `Zeroizing`.
3. Assert: after Phase 1 completes, these values are dropped (verified structurally by type signatures using `Zeroizing<[u8; N]>`).

---

## 5  Phase 2 — Node provisioning tests

### T-PT-300  Phase 1 prerequisite check (no phone PSK → error)

**Validates:** PT-0400

**Procedure:**
1. Start with an empty pairing store (no prior Phase 1).
2. Attempt to initiate Phase 2 (node provisioning).
3. Assert: tool refuses to proceed.
4. Assert: error message contains "no gateway pairing found" or "complete Phase 1 first".

---

### T-PT-301  Node MTU negotiation

**Validates:** PT-0401

**Procedure:**
1. Pre-load pairing store with valid Phase 1 artifacts.
2. Configure mock transport to negotiate MTU = 247 for a node device.
3. Select the node and initiate connection.
4. Assert: connection succeeds.
5. Configure mock transport to negotiate MTU = 100 for a second attempt.
6. Assert: tool disconnects and reports "MTU too low".

---

### T-PT-302  Node PSK generation (32 bytes, CSPRNG)

**Validates:** PT-0402

**Procedure:**
1. Initiate Phase 2 with valid Phase 1 artifacts.
2. Capture the generated `node_psk`.
3. Assert: `node_psk` is exactly 32 bytes.
4. Assert: `node_psk` is generated via the injectable RNG provider. In CI, inject a mock RNG and assert it was called with the correct buffer size (32 bytes). In production builds, the provider delegates to `getrandom::fill()`.

---

### T-PT-303  key_hint derivation matches SHA-256(psk)[30..32]

**Validates:** PT-0402

**Procedure:**
1. Generate a node PSK = `[0x55u8; 32]`.
2. Compute `expected_hint = u16::from_be_bytes(SHA-256([0x55u8; 32])[30..32])`.
3. Assert: the derived `node_key_hint` equals `expected_hint`.

---

### T-PT-304  PairingRequest CBOR deterministic encoding

**Validates:** PT-0403

**Procedure:**
1. Construct a PairingRequest with known fields: `node_id = "test-node"`, `node_key_hint = 0x1234`, `node_psk = [0x55u8; 32]`, `rf_channel = 1`, `sensors = []`, `timestamp = 1700000000`.
2. Encode to CBOR twice.
3. Assert: both encodings produce identical bytes.
4. Assert: integer keys appear in ascending order (1, 2, 3, 4, 5, 6).

---

### T-PT-305  node_id validation (empty rejected, >64 bytes rejected)

**Validates:** PT-0403

**Procedure:**
1. Attempt to construct a PairingRequest with `node_id = ""`.
2. Assert: rejected with an error before any BLE operation.
3. Attempt to construct a PairingRequest with `node_id` = 65-byte UTF-8 string.
4. Assert: rejected with an error before any BLE operation.
5. Attempt with `node_id` = 64-byte string.
6. Assert: accepted.

---

### T-PT-306  rf_channel validation (0 rejected, 14+ rejected)

**Validates:** PT-0403

**Procedure:**
1. Attempt to construct a PairingRequest with `rf_channel = 0`.
2. Assert: rejected with an error.
3. Attempt with `rf_channel = 14`.
4. Assert: rejected with an error.
5. Attempt with `rf_channel = 1` and `rf_channel = 13`.
6. Assert: both accepted.

---

### T-PT-307  Phone HMAC authentication

**Validates:** PT-0404

**Procedure:**
1. Construct a PairingRequest CBOR with known fields and `phone_psk = [0x42u8; 32]`.
2. Compute expected HMAC = `HMAC-SHA256([0x42u8; 32], cbor_bytes)`.
3. Assert: the authenticated request is `phone_key_hint[2] ‖ cbor_bytes ‖ hmac[32]`.
4. Assert: `phone_key_hint` matches `u16::from_be_bytes(SHA-256([0x42u8; 32])[30..32])`.

---

### T-PT-308  Gateway public key encryption (ECDH + HKDF + AES-GCM)

**Validates:** PT-0405

**Procedure:**
1. Use a known Ed25519 public key (from `TestGateway`) and `gateway_id = [0xAAu8; 16]`.
2. Encrypt the authenticated request using the Phase 2 encryption flow.
3. Assert: output format is `eph_public[32] ‖ nonce[12] ‖ ciphertext`.
4. Assert: HKDF uses salt = `gateway_id`, info = `"sonde-node-pair-v1"`, output = 32 bytes.
5. Assert: AES-256-GCM AAD = `gateway_id`.
6. Decrypt using the `TestGateway`'s private key and verify round-trip.

---

### T-PT-309  Ed25519 → X25519 low-order point rejection

**Validates:** PT-0405, PT-0902

**Procedure:**
1. Construct an Ed25519 public key that maps to a low-order X25519 point.
2. Attempt the Ed25519 → X25519 conversion.
3. Assert: conversion returns an error.
4. Assert: error message contains "invalid gateway public key".

---

### T-PT-310  Payload size > 202 bytes rejected before BLE write

**Validates:** PT-0406

**Procedure:**
1. Construct a PairingRequest with a large `node_id` (64 bytes) and `sensors` array sized to push the encrypted payload above 202 bytes.
2. Attempt Phase 2.
3. Assert: tool rejects the request before writing to BLE.
4. Assert: error message includes the current size and the 202-byte limit.

---

### T-PT-311  NODE_PROVISION happy path → NODE_ACK(0x00)

**Validates:** PT-0407

**Procedure:**
1. Pre-load pairing store with valid Phase 1 artifacts.
2. Configure mock transport to return `NODE_ACK(0x00)` after `NODE_PROVISION` write.
3. Initiate Phase 2 with `node_id = "sensor-01"`.
4. Assert: tool writes `NODE_PROVISION` as `node_key_hint[2] ‖ node_psk[32] ‖ rf_channel[1] ‖ payload_len[2, BE u16] ‖ encrypted_payload`.
5. Assert: success output includes `node_id`, `node_key_hint`, and `rf_channel`.

---

### T-PT-312  NODE_ACK(0x01) — already paired

**Validates:** PT-0407

**Procedure:**
1. Configure mock transport to return `NODE_ACK(0x01)`.
2. Initiate Phase 2.
3. Assert: error message contains "node already paired" and suggests factory reset.

---

### T-PT-313  NODE_ACK(0x02) — storage error

**Validates:** PT-0407

**Procedure:**
1. Configure mock transport to return `NODE_ACK(0x02)`.
2. Initiate Phase 2.
3. Assert: error message contains "node storage error".

---

### T-PT-314  NODE_ACK timeout (5 s)

**Validates:** PT-0407

**Procedure:**
1. Configure mock transport to never send `NODE_ACK`.
2. Initiate Phase 2 and write `NODE_PROVISION`.
3. Assert: after 5 s, the tool disconnects.
4. Assert: error message contains "node did not respond".

---

### T-PT-315  Node PSK zeroing after success

**Validates:** PT-0408

**Procedure:**
1. Complete a successful Phase 2 flow.
2. Assert: `node_psk` is wrapped in `Zeroizing` and dropped after the `NODE_PROVISION` write succeeds.
3. Assert: all ephemeral keys, shared secrets, and derived AES keys from Phase 2 encryption are also zeroed (verified structurally by type signatures).

---

## 6  Error handling tests

### T-PT-400  Error classification (device/transport/protocol)

**Validates:** PT-0500

**Procedure:**
1. Trigger a device-level error: configure mock transport to report "BLE adapter not found".
2. Assert: error message identifies the category as device-level and suggests enabling Bluetooth.
3. Trigger a transport-level error: configure mock transport to drop connection mid-write.
4. Assert: error message identifies the category as transport-level.
5. Trigger a protocol-level error: inject `ERROR(0x02)` response.
6. Assert: error message identifies the category as protocol-level.

---

### T-PT-401  Actionable error messages include next steps

**Validates:** PT-0501

**Procedure:**
1. Trigger each of the following errors: BLE adapter disabled, MTU too low, signature verification failure, `ERROR(0x02)`, `ERROR(0x03)`, `NODE_ACK(0x01)`, timeout.
2. Assert: every error message includes at least one actionable sentence (e.g., "enable Bluetooth", "ask operator to open registration window").
3. Assert: no error message consists solely of a code or internal identifier.

---

### T-PT-402  No partial state persisted on failure

**Validates:** PT-0502

**Procedure:**
1. Start with an empty pairing store.
2. Initiate Phase 1 and inject a failure after gateway authentication but before registration completes (e.g., timeout on `PHONE_REGISTERED`).
3. Assert: pairing store is unchanged (no `phone_psk`, no `gw_public_key` persisted).
4. Pre-load pairing store with Phase 1 artifacts. Initiate Phase 2 and inject a failure (e.g., `NODE_ACK(0x02)`).
5. Assert: pairing store is unchanged (no node-related data added).

---

## 7  Idempotency and safety tests

### T-PT-500  Re-run Phase 1 does not corrupt state

**Validates:** PT-0600

**Procedure:**
1. Complete Phase 1 successfully and persist state.
2. Re-run Phase 1 against the same gateway (registration window open).
3. Assert: Phase 1 succeeds or fails with `ERROR(0x03)`.
4. Assert: local pairing state remains valid in either case.

---

### T-PT-501  Re-provision already-paired node → ACK(0x01)

**Validates:** PT-0600

**Procedure:**
1. Complete Phase 2 successfully for a node.
2. Attempt Phase 2 again for the same node (mock returns `NODE_ACK(0x01)`).
3. Assert: error message indicates node is already paired.
4. Assert: no state change on either side.

---

### T-PT-502  Already-paired detection and operator choice

**Validates:** PT-0601

**Procedure:**
1. Pre-load pairing store with a `gw_public_key`.
2. Initiate Phase 1.
3. Assert: tool warns that a gateway identity is already stored.
4. Simulate operator choosing to proceed.
5. Assert: Phase 1 continues normally.

---

## 8  Persistence tests

### T-PT-600  Pairing store contents round-trip

**Validates:** PT-0800

**Procedure:**
1. Write test artifacts to the mock pairing store: `gw_public_key`, `gateway_id`, `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`.
2. Read all fields back.
3. Assert: every field matches the written value exactly.

---

### T-PT-601  Storage abstraction (mock store works)

**Validates:** PT-0802

**Procedure:**
1. Run the full Phase 1 flow against `MockPairingStore`.
2. Assert: all persistence operations succeed.
3. Assert: `MockPairingStore` implements the same `PairingStore` trait as the platform implementations.

---

### T-PT-602  Corrupted store → error + reset offer

**Validates:** PT-0803

**Procedure:**
1. Configure mock pairing store to return a corruption error on read.
2. Attempt to load pairing state.
3. Assert: tool reports a clear error message (not a panic).
4. Assert: tool offers to reset the store.

---

### T-PT-603  No node PSK persisted after provisioning

**Validates:** PT-0804

**Procedure:**
1. Complete a successful Phase 2 flow.
2. Inspect the pairing store contents.
3. Assert: no `node_psk` value is present in the store.
4. Assert: no `node_psk` value appears in any log output captured during the test.

---

### T-PT-604  Android secure storage uses EncryptedSharedPreferences

**Validates:** PT-0801  
**Type:** Manual / platform test (Android instrumentation test)

**Procedure:**
1. On Android (instrumentation test running inside the app process), instantiate `SecureStore` and write a test PSK `[0x42u8; 32]` under key `"phone_psk"`.
2. Assert: `SecureStore` constructor calls `MasterKeys.getOrCreate()` (verifies Android Keystore integration).
3. Assert: the backing `SharedPreferences` filename is `"sonde_pairing_store"` and that `SecureStore` obtains it via `EncryptedSharedPreferences.create("sonde_pairing_store", …)` with the expected key and value encryption schemes.
4. Assert (e.g., via dependency injection, mocking, or a test double) that the concrete `SharedPreferences` instance used by `SecureStore` is the implementation returned by `androidx.security.crypto.EncryptedSharedPreferences` rather than a plain-text `SharedPreferences` implementation.
5. Assert: the PSK round-trips correctly via the `SecureStore` API (read returns the original `[0x42u8; 32]`), and that the PSK value does not appear verbatim in any log output or error messages captured during the test.

---

### T-PT-605  Windows secure storage uses restricted file permissions

**Validates:** PT-0801  
**Type:** Manual / platform test (requires Windows)

**Procedure:**
1. On Windows, instantiate the Windows `PairingStore` and save test artifacts.
2. Assert: the pairing file is written to `%APPDATA%\sonde\pairing.json`.
3. Query the file ACL via `GetFileSecurity` / `icacls`.
4. Assert: the ACL does **not** grant read/write access to broad principals such as `Everyone`, `Users`, `Authenticated Users`, or similar world/group entries; access is limited to the owning user and expected privileged accounts (for example `SYSTEM` and `Administrators`).

---

### T-PT-606  Windows DPAPI PSK protect/unprotect semantics

**Validates:** PT-0801
**Type:** Automated (requires Windows + `--features dpapi`)

**Procedure:**
1. Instantiate `DpapiPskProtector` and protect a 32-byte test PSK.
2. Assert: the protected blob differs from the plaintext and is larger than 32 bytes.
3. Unprotect the blob and assert the recovered value equals the original PSK.
4. Tamper with the protected blob (flip the last byte) and assert `unprotect` returns an error.
5. Protect two different PSKs and assert the blobs differ.

---

## 9  Security tests

### T-PT-700  No key material in default logs

**Validates:** PT-0900

**Procedure:**
1. Complete a full Phase 1 + Phase 2 flow with tracing output captured at default level.
2. Search captured log output for any occurrence of `phone_psk`, `node_psk`, ephemeral private key bytes, shared secret bytes, or AES key bytes.
3. Assert: no key material appears in the captured output.

---

### T-PT-701  No key material in verbose logs

**Validates:** PT-0900

**Procedure:**
1. Enable verbose/debug logging mode.
2. Complete a full Phase 1 + Phase 2 flow with tracing output captured.
3. Search captured log output for key material bytes (hex-encoded `TEST_PSK`, `TEST_NODE_PSK`, etc.).
4. Assert: no key material appears. Operation outcomes and key lengths are acceptable.

---

### T-PT-702  All randomness from injectable RNG provider

**Validates:** PT-0901

**Procedure:**
1. The pairing core MUST accept an injectable RNG provider trait.
2. In CI, inject a mock RNG provider and run the full Phase 1 + Phase 2 flows.
3. Assert: all random values (challenges, ephemeral keys, node PSKs, nonces) are sourced from the mock provider.
4. Assert: no direct calls to `rand::rng()` exist in the pairing crate (enforced via a `#![deny(clippy::disallowed_methods)]` or equivalent CI lint rule).

---

### T-PT-703  Non-zero test keys used

**Validates:** PT-0903

**Procedure:**
1. Search all test files in the pairing crate for PSK or key declarations.
2. Assert: no test uses `[0u8; 32]` as a PSK, key, or secret value.
3. Assert: tests use clearly non-zero values (e.g., `[0x42u8; 32]`).

---

## 10  Non-functional tests

### T-PT-800  Recovery from BLE disconnect mid-pairing

**Validates:** PT-1000

**Procedure:**
1. Initiate Phase 1 and inject a BLE disconnect after `REQUEST_GW_INFO` is written but before `GW_INFO_RESPONSE` arrives.
2. Assert: tool returns to idle/scanning state without crash.
3. Assert: operator can start a new scan and retry without restarting the application.

---

### T-PT-801  No resource leaks on failure

**Validates:** PT-1001

**Procedure:**
1. Run 10 consecutive Phase 1 attempts that fail at different stages (connection failure, timeout, signature failure, decryption failure).
2. Assert: after each failure, mock transport reports no open connections and no active GATT subscriptions.

---

### T-PT-802  Timeout values match spec

**Validates:** PT-1002

**Procedure:**
1. Inspect the timeout constants used by the pairing state machine.
2. Assert: `GW_INFO_RESPONSE` timeout = 45 s.
3. Assert: `PHONE_REGISTERED` timeout = 30 s.
4. Assert: `NODE_ACK` timeout = 5 s.
5. Assert: BLE scan default timeout = 30 s.
6. Assert: BLE connection establishment timeout = 10 s.

---

### T-PT-803  No implicit retries on protocol failure

**Validates:** PT-1003

**Procedure:**
1. Configure the mock transport to inject a timeout error on its next `write_characteristic` call (for example, via a queued error mechanism on the mock).
2. Initiate Phase 1.
3. Assert: the error is returned immediately to the caller — no automatic retry of the write.
4. Assert: mock transport's `write_characteristic` was called exactly **once** (not retried).
5. Configure mock transport to inject a timeout error on `read_indication` using the same error-injection mechanism.
6. Initiate Phase 1 again.
7. Assert: the timeout error is surfaced immediately — no automatic retry of the read.

---

### T-PT-804  LESC Numeric Comparison enforced

**Validates:** PT-0106, PT-0904

**Procedure:**
1. Configure the mock BLE transport to report Numeric Comparison as the pairing method.
2. Run Phase 1 gateway pairing.
3. Assert: pairing proceeds normally and Phase 1 completes using Numeric Comparison.

---

### T-PT-805  Just Works fallback rejected

**Validates:** PT-0904

**Procedure:**
1. Configure the mock BLE transport to silently fall back to Just Works (no passkey displayed).
2. Attempt Phase 1 gateway pairing.
3. Assert: transport rejects the connection before proceeding to `REQUEST_GW_INFO`.
4. Assert: error message indicates the pairing mode is insecure.

---

### T-PT-806  Android lifecycle pause/resume during pairing

**Validates:** PT-0107

**Procedure:**
1. Start Phase 1 gateway pairing on the mock transport.
2. Simulate an Android activity pause event after BLE connection is established.
3. Assert: BLE connection is cleanly disconnected.
4. Simulate an Android activity resume event.
5. Assert: transport automatically attempts to reconnect the BLE connection.
6. If reconnection fails, assert: operator is presented with a clear "pairing interrupted — retry?" prompt.
7. Assert: no GATT client resource leaks.

---

### T-PT-807  JNI classloader caching on background thread

**Validates:** PT-0108

**Procedure:**
1. Spawn a new thread (simulating a tokio worker thread, not the main/JNI_OnLoad thread).
2. From that thread, attempt to instantiate the BLE helper class using the cached `GlobalRef`.
3. Assert: instantiation succeeds without `ClassNotFoundException`.
4. Assert: BLE operations can be invoked from the background thread.

---

## 11  Cryptographic tests

### T-PT-900  HKDF parameters correct for Phase 1

**Validates:** PT-1101

**Procedure:**
1. Using known inputs (`gateway_id = [0xAAu8; 16]`, a fixed ECDH shared secret), derive the AES key for Phase 1 decryption.
2. Assert: HKDF salt = `gateway_id`.
3. Assert: HKDF info = `"sonde-phone-reg-v1"`.
4. Assert: output length = 32 bytes.
5. Assert: derived key matches a precomputed expected value.

---

### T-PT-901  HKDF parameters correct for Phase 2

**Validates:** PT-1101

**Procedure:**
1. Using known inputs (`gateway_id = [0xAAu8; 16]`, a fixed ECDH shared secret), derive the AES key for Phase 2 encryption.
2. Assert: HKDF salt = `gateway_id`.
3. Assert: HKDF info = `"sonde-node-pair-v1"`.
4. Assert: output length = 32 bytes.
5. Assert: derived key matches a precomputed expected value.

---

### T-PT-902  AES-GCM AAD = gateway_id

**Validates:** PT-1102

**Procedure:**
1. Encrypt a test payload using the Phase 2 encryption flow with `gateway_id = [0xAAu8; 16]`.
2. Decrypt using the same key and `AAD = [0xAAu8; 16]`.
3. Assert: decryption succeeds and plaintext matches.
4. Attempt decryption with `AAD = [0xBBu8; 16]`.
5. Assert: decryption fails (GCM tag mismatch).

---

### T-PT-903  CBOR deterministic encoding (known test vector)

**Validates:** PT-1103

**Procedure:**
1. Construct a PairingRequest with known fields: `node_id = "n1"`, `node_key_hint = 0x0001`, `node_psk = [0x55u8; 32]`, `rf_channel = 1`, `sensors = []`, `timestamp = 1700000000`.
2. Encode to CBOR.
3. Assert: output bytes match a precomputed reference vector (integer keys in ascending order, minimal-length encoding, definite-length containers).
4. Assert: encoding is identical across repeated runs.

---

### T-PT-1004  Core crate builds and works without platform features

**Validates:** PT-1004

**Procedure:**
1. Build the `sonde-pair` crate without any platform features enabled (`--no-default-features`).
2. Assert: the build succeeds on all CI targets (Android `aarch64-linux-android`, Windows `x86_64-pc-windows-msvc`, Linux `x86_64-unknown-linux-gnu`).
3. Assert: `Cargo.toml` dependency graph shows platform-specific dependencies are gated behind feature flags, not unconditionally required.

---

## 12  Diagnostic logging tests

### T-PT-1207  BLE scan events logged

**Validates:** PT-1207

**Procedure:**
1. Configure a `MockBleTransport` with two target devices and one unrelated device.
2. Initialise a `tracing-test` subscriber with `#[traced_test]`.
3. Create a `DeviceScanner`, call `start()`, then `refresh()`.
4. Assert: captured logs contain a `debug` event with text "scan started" and the UUID filter list.
5. Call `stop()`.
6. Assert: captured logs contain a `debug` event with text "scan stopped".
7. Assert: captured logs contain `debug` events for each discovered target device with `name`, `address`, `rssi`.

---

### T-PT-1208  Connection lifecycle events logged

**Validates:** PT-1208

**Procedure:**
1. Configure a `MockBleTransport` with MTU 247.
2. Initialise a `tracing-test` subscriber with `#[traced_test]`.
3. Run `pair_with_gateway` or `provision_node` with mock data.
4. Assert: captured logs contain a `debug` or `info` event with "connecting".
5. Assert: captured logs contain a `debug` event with `mtu` field.
6. Assert: captured logs contain a `debug` event with "disconnected".

---

### T-PT-1209  GATT write and indication events logged

**Validates:** PT-1209

**Procedure:**
1. Run a Phase 1 happy path with mock transport and `#[traced_test]`.
2. Assert: captured logs contain `trace` events for each `BLE write` with `msg` type name and `len`.
3. Assert: captured logs contain `trace` events for each `BLE indication received` with `msg_type` and `len` fields.
4. Assert: transport-level `debug` events for `GATT write complete` include `characteristic` and `len`.
5. Assert: no log event contains raw PSK, private key, or shared secret bytes.

---

### T-PT-1210  Phase transition events logged

**Validates:** PT-1210

**Procedure:**
1. Run a Phase 1 happy path with mock transport, progress callback, and `#[traced_test]`.
2. Assert: captured logs contain `info` events for "connecting to gateway" and "Phase 1 complete".
3. Assert: the completion log includes `phone_key_hint` and `rf_channel` fields.
4. Run a Phase 2 happy path.
5. Assert: captured logs contain `info` events for "connecting to node" and "Phase 2 complete".

---

### T-PT-1211  LESC pairing method logged

**Validates:** PT-1211

**Procedure:**
1. Run a Phase 1 happy path with a mock transport that reports `PairingMethod::NumericComparison`.
2. Capture tracing output with `#[traced_test]`.
3. Assert: captured logs contain a `debug` event with `pairing_method` field.

---

### T-PT-1212  Error context in log output

**Validates:** PT-1212

**Procedure:**
1. Configure a mock transport to cause a `GW_INFO_RESPONSE` timeout (45 s).
2. Run Phase 1, capture the error, and capture tracing output (e.g., with `#[traced_test]`).
3. Assert: the error is `PairingError::IndicationTimeout` and the captured logs include an event for the `GW_INFO_RESPONSE` timeout with fields for the operation name and timeout duration (45 s).
4. Configure a mock transport to return an `ERROR` response with status `0x02`.
5. Run Phase 1 and capture the error.
6. Assert: the error includes the status code in its display output.

---

### T-PT-1214a  Pin config included in NODE_PROVISION when provided

**Validates:** PT-1214 (AC 1, 2, 4)  
**Status:** Deferred — `sonde-pair` does not yet implement pin config encoding.

**Procedure:**
1. Call `provision_node(...)` with pin config `Some(PinConfig { i2c0_sda: 4, i2c0_scl: 5 })`.
2. Capture the NODE_PROVISION message body written to the mock BLE transport.
3. Assert: the body contains the encrypted payload followed by a deterministic CBOR map.
4. Decode the trailing CBOR map and assert: integer key 1 = 4 (`i2c0_sda`), integer key 2 = 5 (`i2c0_scl`).
5. Assert: the CBOR map uses deterministic encoding (RFC 8949 §4.2).

---

### T-PT-1214b  No pin config in NODE_PROVISION — backward compatible

**Validates:** PT-1214 (AC 1, 3)  
**Status:** Deferred — `sonde-pair` does not yet implement pin config encoding.

**Procedure:**
1. Call `provision_node(...)` with pin config `None`.
2. Capture the NODE_PROVISION message body written to the mock BLE transport.
3. Assert: the body is identical to the existing format (encrypted payload only, no trailing bytes).
4. Assert: provisioning completes successfully (NODE_ACK received).

---

## Appendix A  Test-to-requirement traceability

| Test ID | Requirement | Title |
|---|---|---|
| T-PT-100 | PT-0200 | BLE scan discovers gateway service UUID |
| T-PT-101 | PT-0200 | BLE scan discovers node service UUID |
| T-PT-102 | PT-0200 | Non-Sonde devices filtered from results |
| T-PT-103 | PT-0201 | Device presentation (name, type, RSSI) |
| T-PT-104 | PT-0202 | Scan timeout and stale device eviction |
| T-PT-105 | PT-0105 | BLE permission dialog shown on Android |
| T-PT-106 | PT-0105 | BLE permission denial produces actionable error |
| T-PT-107 | PT-0105 | BLE permissions on Android 6–11 (location) |
| T-PT-108 | PT-0106, PT-0904 | LESC Numeric Comparison pairing used |
| T-PT-109 | PT-0106, PT-0904 | Just Works fallback rejected |
| T-PT-109a | PT-0904 | OS-enforced pairing (`None`) accepted by `enforce_lesc` |
| T-PT-109b | PT-0904 | Unknown pairing method rejected by `enforce_lesc` |
| T-PT-110 | PT-0700 | Minimum UI elements present |
| T-PT-111 | PT-0701 | Phase indication updates |
| T-PT-112 | PT-0702 | Verbose diagnostic mode |
| T-PT-113 | PT-0107 | Android activity lifecycle disconnect |
| T-PT-114 | PT-0108 | JNI classloader caching on background threads |
| T-PT-200 | PT-0300 | MTU negotiation ≥ 247 |
| T-PT-201 | PT-0300 | MTU < 247 → disconnect + error |
| T-PT-202 | PT-0301 | Gateway authentication happy path |
| T-PT-203 | PT-0301 | Gateway authentication failure (bad signature) |
| T-PT-204 | PT-0301 | GW_INFO_RESPONSE timeout (45 s) |
| T-PT-205 | PT-0302 | TOFU — first connection persists public key |
| T-PT-206 | PT-0302 | TOFU — mismatched public key rejected |
| T-PT-207 | PT-0302 | TOFU — operator can clear pinned identity |
| T-PT-208 | PT-0303 | Phone registration happy path |
| T-PT-208a | PT-0303 | Phone label validation |
| T-PT-209 | PT-0303 | ERROR(0x02) — registration window closed |
| T-PT-210 | PT-0303 | ERROR(0x03) — already paired |
| T-PT-211 | PT-0303 | PHONE_REGISTERED timeout (30 s) |
| T-PT-212 | PT-0303 | Decryption failure (bad GCM tag) |
| T-PT-213 | PT-0304 | Ephemeral key zeroing after use |
| T-PT-300 | PT-0400 | Phase 1 prerequisite check |
| T-PT-301 | PT-0401 | Node MTU negotiation |
| T-PT-302 | PT-0402 | Node PSK generation (32 bytes, CSPRNG) |
| T-PT-303 | PT-0402 | key_hint derivation matches SHA-256(psk)[30..32] |
| T-PT-304 | PT-0403 | PairingRequest CBOR deterministic encoding |
| T-PT-305 | PT-0403 | node_id validation |
| T-PT-306 | PT-0403 | rf_channel validation |
| T-PT-307 | PT-0404 | Phone HMAC authentication |
| T-PT-308 | PT-0405 | Gateway public key encryption |
| T-PT-309 | PT-0405, PT-0902 | Ed25519 → X25519 low-order point rejection |
| T-PT-310 | PT-0406 | Payload size > 202 bytes rejected |
| T-PT-311 | PT-0407 | NODE_PROVISION happy path → NODE_ACK(0x00) |
| T-PT-312 | PT-0407 | NODE_ACK(0x01) — already paired |
| T-PT-313 | PT-0407 | NODE_ACK(0x02) — storage error |
| T-PT-314 | PT-0407 | NODE_ACK timeout (5 s) |
| T-PT-315 | PT-0408 | Node PSK zeroing after success |
| T-PT-400 | PT-0500 | Error classification |
| T-PT-401 | PT-0501 | Actionable error messages |
| T-PT-402 | PT-0502 | No partial state persisted on failure |
| T-PT-500 | PT-0600 | Re-run Phase 1 does not corrupt state |
| T-PT-501 | PT-0600 | Re-provision already-paired node |
| T-PT-502 | PT-0601 | Already-paired detection and operator choice |
| T-PT-600 | PT-0800 | Pairing store contents round-trip |
| T-PT-601 | PT-0802 | Storage abstraction (mock store works) |
| T-PT-602 | PT-0803 | Corrupted store → error + reset offer |
| T-PT-603 | PT-0804 | No node PSK persisted after provisioning |
| T-PT-604 | PT-0801 | Android secure storage uses EncryptedSharedPreferences |
| T-PT-605 | PT-0801 | Windows secure storage uses restricted file permissions |
| T-PT-606 | PT-0801 | Windows DPAPI PSK protect/unprotect semantics |
| T-PT-700 | PT-0900 | No key material in default logs |
| T-PT-701 | PT-0900 | No key material in verbose logs |
| T-PT-702 | PT-0901 | All randomness from injectable RNG provider |
| T-PT-703 | PT-0903 | Non-zero test keys used |
| T-PT-800 | PT-1000 | Recovery from BLE disconnect mid-pairing |
| T-PT-801 | PT-1001 | No resource leaks on failure |
| T-PT-802 | PT-1002 | Timeout values match spec |
| T-PT-803 | PT-1003 | No implicit retries on protocol failure |
| T-PT-804 | PT-0106, PT-0904 | LESC Numeric Comparison enforced |
| T-PT-805 | PT-0904 | Just Works fallback rejected |
| T-PT-806 | PT-0107 | Android lifecycle pause/resume during pairing |
| T-PT-807 | PT-0108 | JNI classloader caching on background thread |
| T-PT-900 | PT-1101 | HKDF parameters correct for Phase 1 |
| T-PT-901 | PT-1101 | HKDF parameters correct for Phase 2 |
| T-PT-902 | PT-1102 | AES-GCM AAD = gateway_id |
| T-PT-903 | PT-1103 | CBOR deterministic encoding (known test vector) |
| T-PT-1004 | PT-1004 | Core crate builds and works without platform features |
| T-PT-1207 | PT-1207 | BLE scan events logged |
| T-PT-1208 | PT-1208 | Connection lifecycle events logged |
| T-PT-1209 | PT-1209 | GATT write and indication events logged |
| T-PT-1210 | PT-1210 | Phase transition events logged |
| T-PT-1211 | PT-1211 | LESC pairing method logged |
| T-PT-1212 | PT-1212 | Error context in log output |
| T-PT-1214a | PT-1214 | Pin config included in NODE_PROVISION when provided |
| T-PT-1214b | PT-1214 | No pin config in NODE_PROVISION — backward compatible |
