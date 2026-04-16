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
| PT-1202 | Phase 1 error paths | T-PT-209, T-PT-210, T-PT-211 |
| PT-1203 | Phase 2 happy path | T-PT-311 |
| PT-1204 | Phase 2 error paths | T-PT-300, T-PT-310, T-PT-312, T-PT-313, T-PT-314 |
| PT-1205 | Input validation | T-PT-303, T-PT-304, T-PT-305, T-PT-306 |
| PT-1206 | Manual testing on physical hardware | Manual test procedures in §10 (T-PT-800–T-PT-807) |

**Cryptographic primitive coverage (PT-1100):** PT-1100 requires the cryptographic primitives to be implemented.  Rather than a single aggregate test, coverage is provided structurally by the test suite's dependency on each primitive:

| Primitive | Test(s) exercising it |
|---|---|
| AES-256-GCM | T-PT-307, T-PT-308, T-PT-902 |
| SHA-256 | T-PT-303 |
| CSPRNG | T-PT-302, T-PT-702 |

**Test ID convention:** Test IDs follow the numeric pattern `T-PT-NNN`. When a test case is added after initial numbering to cover a gap between two adjacent IDs, an alphabetic suffix is used (e.g., `T-PT-208a` for a test inserted between T-PT-208 and T-PT-209).

---

## 2  Test environment

### 2.1  Mock BLE transport

An in-process `BleTransport` implementation that:

- Simulates BLE scan results (service UUIDs, advertising names, RSSI).
- Simulates GATT connections with configurable MTU negotiation.
- Queues indication responses (e.g., `PHONE_REGISTERED`, `NODE_ACK`).
- Captures outbound GATT writes (for assertion).
- Supports error injection: connection failure, timeout, malformed indication, mid-operation disconnect.

### 2.2  Mock pairing store

An in-memory `PairingStore` implementation that:

- Stores and retrieves pairing artifacts (`phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`).
- Supports clear/reset operations.
- Can be pre-loaded with test data or left empty.
- Can be configured to simulate corruption (return errors on read).

### 2.3  Test key material

All tests use clearly non-zero keys to avoid normalizing insecure patterns:

```
TEST_PSK:        [0x42u8; 32]
TEST_NODE_PSK:   [0x55u8; 32]
```

### 2.4  Test gateway helper

A helper that constructs valid BLE indication payloads:

```
TestGateway {
    fn register_ack(status: u8, phone_key_hint: u16, rf_channel: u8) -> Vec<u8>
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

**Validates:** PT-0700, PT-1217  
**Type:** Manual / platform test

**Procedure:**
1. Launch the pairing tool.
2. Assert: the UI shows a multi-page wizard flow with 6 pages.
3. Navigate through all pages and assert: scan toggle, device list, device select, pair action, node ID input, board selector, status area, and error display are present on the appropriate pages.
4. Assert: the UI does not include management, monitoring, or telemetry features.

---

### T-PT-111  Phase indication updates

**Validates:** PT-0701, PT-1218

**Procedure:**
1. Launch the pairing tool.
2. Assert: a stepper bar is visible with three steps: Gateway, Node, Done.
3. Assert: Gateway step is active on pages 1–3.
4. Navigate to page 4.
5. Assert: Gateway step is marked done; Node step is active.
6. Navigate to page 6.
7. Assert: Gateway and Node steps are marked done; Done step is active.
8. Assert: stepper steps are not clickable.

---

### T-PT-112  Verbose diagnostic mode

**Validates:** PT-0702

**Procedure:**
1. Enable verbose diagnostic mode (toggle or flag).
2. Run a complete Phase 1 pairing flow with mock transport.
3. Assert: verbose output includes raw BLE event names, message types, and timing.
4. Assert: verbose output does NOT include key material (PSKs, private keys, AES keys).
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

> **T-PT-202 through T-PT-207 — RETIRED (issue #495).** Gateway Ed25519 authentication (T-PT-202, T-PT-203), `GW_INFO_RESPONSE` timeout (T-PT-204), and TOFU key pinning (T-PT-205, T-PT-206, T-PT-207) were removed when the pairing protocol was simplified to use BLE LESC Numeric Comparison for mutual authentication.

---

### T-PT-208  Phone registration happy path

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport with a `TestGateway` that returns a valid `PHONE_REGISTERED (0x82)` message with status `0x00`, `phone_key_hint`, and `rf_channel`.
2. Initiate Phase 1. After connection, proceed to registration.
3. Assert: tool generates a 32-byte `phone_psk` via the injectable RNG provider.
4. Assert: tool writes `REGISTER_PHONE` containing the phone-generated `phone_psk` and operator label.
5. Assert: tool receives `PHONE_REGISTERED` with status `0x00`.
6. Assert: `phone_psk`, `phone_key_hint`, and `rf_channel` are persisted.

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
2. Initiate Phase 1.
3. Assert: tool disconnects.
4. Assert: error message contains "registration window not open".

---

### T-PT-210  ERROR(0x03) — already paired

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport to respond with `ERROR(0x03)` after `REGISTER_PHONE`.
2. Initiate Phase 1.
3. Assert: tool disconnects.
4. Assert: error message contains "already paired".

---

### T-PT-211  PHONE_REGISTERED timeout (30 s)

**Validates:** PT-0303

**Procedure:**
1. Configure mock transport to never send `PHONE_REGISTERED`.
2. Initiate Phase 1 and write `REGISTER_PHONE`.
3. Assert: after 30 s, the tool disconnects.
4. Assert: error message indicates timeout on phone registration.

---

### T-PT-212  Decryption failure (bad GCM tag)

> **RETIRED (issue #495).** The simplified registration flow returns a plaintext status ACK; there is no encrypted `PHONE_REGISTERED` response to decrypt.

---

### T-PT-213  Key material zeroing after use

**Validates:** PT-0304

**Procedure:**
1. Complete a successful Phase 1 flow.
2. Assert: `phone_psk` is wrapped in `Zeroizing` during the registration flow.
3. Assert: after Phase 1 completes and the PSK is persisted, the in-memory copy is dropped (verified structurally by type signatures using `Zeroizing<[u8; N]>`).

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

### T-PT-307  Phone PSK authentication (AES-256-GCM)

**Validates:** PT-1102

**Procedure:**
1. Construct a PairingRequest CBOR with known fields and `phone_psk = [0x42u8; 32]`.
2. Encrypt with AES-256-GCM using `phone_psk` as the key and a 12-byte random nonce.
3. Assert: the encrypted payload format is `nonce[12] ‖ ciphertext_with_tag`.
4. Assert: the 16-byte GCM authentication tag is appended to the ciphertext.
5. Decrypt with the same `phone_psk` and nonce.
6. Assert: decrypted plaintext matches the original CBOR bytes.

---

### T-PT-308  Payload encryption (AES-256-GCM with phone_psk)

**Validates:** PT-0407, PT-1102

**Procedure:**
1. Use `phone_psk = [0x42u8; 32]` and a known PairingRequest CBOR payload.
2. Encrypt the payload using AES-256-GCM with `phone_psk` as the key.
3. Assert: output format is `nonce[12] ‖ ciphertext_with_tag`.
4. Assert: nonce is 12 bytes generated via the injectable RNG provider.
5. Assert: the 16-byte GCM tag provides both confidentiality and authenticity.
6. Decrypt using the same `phone_psk` and verify round-trip.

---

### T-PT-309  Ed25519 → X25519 low-order point rejection

> **RETIRED (issue #495).** Ed25519 → X25519 key conversion removed; the simplified pairing flow uses `phone_psk` directly as the AES-256-GCM key.

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
4. Assert: tool writes `NODE_PROVISION` as `node_key_hint[2] ‖ node_psk[32] ‖ rf_channel[1] ‖ payload_len[2, BE u16] ‖ encrypted_payload` (37-byte prefix + payload).
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
3. Assert: all AES-256-GCM keys and nonces from Phase 2 encryption are also zeroed (verified structurally by type signatures).

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
1. Trigger each of the following errors: BLE adapter disabled, MTU too low, `ERROR(0x02)`, `ERROR(0x03)`, `NODE_ACK(0x01)`, timeout.
2. Assert: every error message includes at least one actionable sentence (e.g., "enable Bluetooth", "ask operator to open registration window").
3. Assert: no error message consists solely of a code or internal identifier.

---

### T-PT-402  No partial state persisted on failure

**Validates:** PT-0502

**Procedure:**
1. Start with an empty pairing store.
2. Initiate Phase 1 and inject a failure before registration completes (e.g., timeout on `PHONE_REGISTERED`).
3. Assert: pairing store is unchanged (no `phone_psk` persisted).
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
1. Pre-load pairing store with a `phone_psk`.
2. Initiate Phase 1.
3. Assert: tool warns that a gateway pairing is already stored.
4. Simulate operator choosing to proceed.
5. Assert: Phase 1 continues normally.

---

## 8  Persistence tests

### T-PT-600  Pairing store contents round-trip

**Validates:** PT-0800

**Procedure:**
1. Write test artifacts to the mock pairing store: `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`.
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
2. Search captured log output for any occurrence of `phone_psk`, `node_psk`, or AES key bytes.
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
3. Assert: all random values (node PSKs, nonces) are sourced from the mock provider.
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
1. Initiate Phase 1 and inject a BLE disconnect after `REGISTER_PHONE` is written but before `PHONE_REGISTERED` arrives.
2. Assert: tool returns to idle/scanning state without crash.
3. Assert: operator can start a new scan and retry without restarting the application.

---

### T-PT-801  No resource leaks on failure

**Validates:** PT-1001

**Procedure:**
1. Run 10 consecutive Phase 1 attempts that fail at different stages (connection failure, timeout, registration error).
2. Assert: after each failure, mock transport reports no open connections and no active GATT subscriptions.

---

### T-PT-802  Timeout values match spec

**Validates:** PT-1002

**Procedure:**
1. Inspect the timeout constants used by the pairing state machine.
2. Assert: `PHONE_REGISTERED` timeout = 30 s.
3. Assert: `NODE_ACK` timeout = 5 s.
4. Assert: BLE scan default timeout = 30 s.
5. Assert: BLE connection establishment timeout = 30 s.

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
3. Assert: transport rejects the connection before proceeding to `REGISTER_PHONE`.
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

> **T-PT-900, T-PT-901 — RETIRED (issue #495).** HKDF key derivation tests removed; the simplified pairing flow uses `phone_psk` directly as the AES-256-GCM key.

---

### T-PT-902  AES-256-GCM with phone_psk round-trip

**Validates:** PT-1102

**Procedure:**
1. Encrypt a test payload using AES-256-GCM with `phone_psk = [0x42u8; 32]` and a 12-byte nonce.
2. Decrypt using the same `phone_psk` and nonce.
3. Assert: decryption succeeds and plaintext matches.
4. Attempt decryption with a different key `[0x43u8; 32]`.
5. Assert: decryption fails (GCM authentication tag mismatch).

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
5. Assert: no log event contains raw PSK or private key bytes.

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
1. Configure a mock transport to cause a `PHONE_REGISTERED` timeout (30 s).
2. Run Phase 1, capture the error, and capture tracing output (e.g., with `#[traced_test]`).
3. Assert: the error is `PairingError::IndicationTimeout` and the captured logs include an event for the `PHONE_REGISTERED` timeout with fields for the operation name and timeout duration (30 s).
4. Configure a mock transport to return an `ERROR` response with status `0x02`.
5. Run Phase 1 and capture the error.
6. Assert: the error includes the status code in its display output.

---

### T-PT-1215a  Connection error includes device address

**Validates:** PT-1215 (AC 1, AC 2)

**Procedure:**
1. Configure a `MockBleTransport` so `connect()` returns `ConnectionFailed` with `device: Some("AA:BB:CC:DD:EE:FF")`.
2. Run `pair_with_gateway` with device address `[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]`.
3. Assert: the error display string contains `"AA:BB:CC:DD:EE:FF"`.
4. Assert: the error display string contains the failed operation and reason.

---

### T-PT-1215b  MTU error includes device address

**Validates:** PT-1215 (AC 1, AC 2)

**Procedure:**
1. Configure a `MockBleTransport` with MTU 100 (below `BLE_MTU_MIN`).
2. Run `pair_with_gateway` with device address `[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]`.
3. Assert: the error is `PairingError::MtuTooLow { .. }`.
4. Assert: the error display string contains `"11:22:33:44:55:66"`.
5. Assert: the error display string contains `100` (negotiated) and `247` (required).

---

### T-PT-1215c  Connection dropped includes stale pairing hint

**Validates:** PT-1215 (AC 3)

**Procedure:**
1. Construct `PairingError::ConnectionDropped { device: Some("AA:BB:CC:DD:EE:FF".into()) }`.
2. Assert: the error display string contains `"AA:BB:CC:DD:EE:FF"`.
3. Assert: the error display string contains `"stale"` or `"Bluetooth pairing"`.

---

### T-PT-1215d  Indication timeout includes device address

**Validates:** PT-1215 (AC 1, AC 2)

**Procedure:**
1. Configure a `MockBleTransport` with no queued responses (causes `IndicationTimeout`).
2. Run `pair_with_gateway` with device address `[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]`.
3. Assert: the error is `PairingError::IndicationTimeout { .. }`.
4. Assert: the error display string contains `"AA:BB:CC:DD:EE:FF"`.

---

### T-PT-1215e  format_device_address produces canonical format

**Validates:** PT-1215 (AC 2)

**Procedure:**
1. Call `format_device_address(&[0x00, 0x0A, 0xFF, 0x10, 0x0B, 0xAC])`.
2. Assert: result is `"00:0A:FF:10:0B:AC"`.

---

### T-PT-1214a  Pin config included in NODE_PROVISION when provided

**Validates:** PT-1214 (AC 1, 2, 4)

**Procedure:**
1. Call `provision_node(...)` with pin config `Some(PinConfig { i2c0_sda: 4, i2c0_scl: 5 })`.
2. Capture the NODE_PROVISION message body written to the mock BLE transport.
3. Assert: the body contains the encrypted payload followed by a deterministic CBOR map.
4. Decode the trailing CBOR map and assert: integer key 1 = 4 (`i2c0_sda`), integer key 2 = 5 (`i2c0_scl`).
5. Assert: the CBOR map uses deterministic encoding (RFC 8949 §4.2).

---

### T-PT-1214c  Pin config with out-of-range GPIO rejected

**Validates:** PT-1214 (AC 5)

**Procedure:**
1. Call `provision_node(...)` with pin config `Some(PinConfig { i2c0_sda: 22, i2c0_scl: 5 })`.
2. Assert: the call returns an error indicating GPIO number out of range (0–21).
3. Assert: no NODE_PROVISION message is written to the BLE transport.

---

### T-PT-1214d  Pin config with SDA equal to SCL rejected

**Validates:** PT-1214 (AC 6)

**Procedure:**
1. Call `provision_node(...)` with pin config `Some(PinConfig { i2c0_sda: 4, i2c0_scl: 4 })`.
2. Assert: the call returns an error indicating SDA and SCL must be different pins.
3. Assert: no NODE_PROVISION message is written to the BLE transport.

---

### T-PT-1214b  No pin config in NODE_PROVISION — backward compatible

**Validates:** PT-1214 (AC 1, 3)

**Procedure:**
1. Call `provision_node(...)` with pin config `None`.
2. Capture the NODE_PROVISION message body written to the mock BLE transport.
3. Assert: the body is identical to the existing format (encrypted payload only, no trailing bytes).
4. Assert: provisioning completes successfully (NODE_ACK received).

---

### T-PT-1216a  Board preset passes correct pin config

**Validates:** PT-1216 (AC 1, 3, 6), PT-1214 (AC 1, 2)

**Procedure:**
1. Call `phase2::provision_node(...)` with `MockBleTransport` and pin config `Some(PinConfig { i2c0_sda: 5, i2c0_scl: 6 })` (SparkFun preset values).
2. Capture the NODE_PROVISION message body written to the mock BLE transport.
3. Assert: the trailing CBOR map contains integer key 1 = 5 (`i2c0_sda`), integer key 2 = 6 (`i2c0_scl`).

---

### T-PT-1216b  Default preset is Espressif DevKitM-1

**Validates:** PT-1216 (AC 2, 3)

**Procedure:**
1. Open the provisioning UI and verify the initial board selector value is "Espressif ESP32-C3 DevKitM-1".
2. Without changing the board selection or manually overriding the I2C pins, trigger provisioning.
3. Capture the NODE_PROVISION message body written to the mock BLE transport.
4. Assert: the trailing CBOR map contains integer key 1 = 0 (`i2c0_sda`), integer key 2 = 1 (`i2c0_scl`).

---

### T-PT-1216c  Custom board with valid GPIOs

**Validates:** PT-1216 (AC 4, 5, 6)

**Procedure:**
1. Call `phase2::provision_node(...)` with `MockBleTransport` and pin config `Some(PinConfig { i2c0_sda: 8, i2c0_scl: 9 })` (custom values).
2. Capture the NODE_PROVISION message body written to the mock BLE transport.
3. Assert: the trailing CBOR map contains integer key 1 = 8 (`i2c0_sda`), integer key 2 = 9 (`i2c0_scl`).

---

### T-PT-1216d  Custom board with invalid GPIOs rejected

**Validates:** PT-1216 (AC 5), PT-0409

**Procedure:**
1. Call `phase2::provision_node(...)` with `MockBleTransport` and pin config `Some(PinConfig { i2c0_sda: 25, i2c0_scl: 6 })`.
2. Assert: the call returns `Err(PairingError::InvalidPinConfig(_))`.
3. Assert: no NODE_PROVISION message is written to the mock BLE transport.

---

### T-PT-1216e  Provision with only one pin parameter rejected

**Validates:** PT-1216 (AC 7)

**Procedure:**
1. Call `resolve_pin_config(Some(5), None)`.
2. Assert: the call returns an error indicating both pins must be provided.

---

## 13  Multi-page wizard navigation tests

### T-PT-1217a  Six pages rendered and only one visible at a time

**Validates:** PT-1217 (AC 1, 2)  
**Type:** Manual / platform test

**Procedure:**
1. Launch the pairing tool.
2. Assert: 6 `<section>` elements with IDs `page-welcome`, `page-gateway-scan`, `page-gateway-done`, `page-node-scan`, `page-node-provision`, `page-done` exist in the DOM.
3. Assert: exactly one page is visible; the other 5 are hidden.

---

### T-PT-1217b  Forward navigation through all pages

**Validates:** PT-1217 (AC 3, 4)  
**Type:** Manual / platform test

**Procedure:**
1. Launch the pairing tool on page 1.
2. Complete gateway pairing (or simulate via mock) and navigate forward through each page.
3. Assert: each page becomes visible in order (1 → 2 → 3 → 4 → 5 → 6).
4. Assert: the previous page is hidden when the next page becomes visible.

---

### T-PT-1217c  Existing functionality works through wizard flow

**Validates:** PT-1217 (AC 5)  
**Type:** Manual / platform test

**Procedure:**
1. Launch the pairing tool.
2. Complete a full workflow: check status (page 1) → scan and pair gateway (page 2) → confirm pairing (page 3) → scan and select node (page 4) → provision node (page 5) → view success (page 6).
3. Assert: all Tauri commands (`start_scan`, `pair_gateway`, `provision_node`, `get_pairing_status`) are invoked correctly and produce the expected results.

---

### T-PT-1218a  Stepper bar shows three phases

**Validates:** PT-1218 (AC 1, 2, 5)  
**Type:** Manual / platform test

**Procedure:**
1. Launch the pairing tool.
2. Assert: the stepper bar contains exactly three labeled steps: "Gateway", "Node", "Done".
3. Click on each stepper step.
4. Assert: no navigation occurs (stepper is not interactive).

---

### T-PT-1218b  Stepper highlights current phase

**Validates:** PT-1218 (AC 3, 4)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 1 (Welcome).
2. Assert: Gateway step has `step--active` class; Node and Done are dimmed.
3. Navigate to page 4 (Node Scan).
4. Assert: Gateway step has `step--done` class; Node step has `step--active` class; Done is dimmed.
5. Navigate to page 6 (Done).
6. Assert: Gateway and Node steps have `step--done` class; Done step has `step--active` class.

---

### T-PT-1219a  Page index persisted to localStorage

**Validates:** PT-1219 (AC 1)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 4 (Node Scan).
2. Read `localStorage.getItem('sonde-pair-page')`.
3. Assert: the value is `"3"` (0-based index for page 4).

---

### T-PT-1219b  Page restored on app restart

**Validates:** PT-1219 (AC 2, 3)  
**Type:** Manual / platform test

**Procedure:**
1. Set `localStorage.setItem('sonde-pair-page', '2')` and ensure pairing artifacts exist.
2. Reload the app.
3. Assert: page 3 (Pairing Complete) is visible.
4. Leave pairing artifacts in place and set `localStorage.setItem('sonde-pair-page', '99')`.
5. Reload the app.
6. Assert: page 4 (Node Scan) is visible (invalid index defaults to the earliest valid page for an already paired state).
7. Clear pairing artifacts and set `localStorage.setItem('sonde-pair-page', '4')`.
8. Reload the app.
9. Assert: page 1 (Welcome) is visible (prerequisites not met, redirects to earliest valid page).

---

### T-PT-1220a  Back navigation returns to previous page

**Validates:** PT-1220 (AC 1, 4)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 3 (Pairing Complete).
2. Trigger back navigation (browser back or hardware back button).
3. Assert: page 2 (Gateway Scan) is visible.
4. Assert: stepper bar still shows Gateway phase as active.

---

### T-PT-1220b  Back navigation on page 1 does nothing

**Validates:** PT-1220 (AC 2)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 1 (Welcome).
2. Trigger back navigation.
3. Assert: page 1 is still visible; the app does not exit or navigate away.

---

### T-PT-1221a  RSSI indicator shows correct quality level

**Validates:** PT-1221 (AC 1–4)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 4 (Node Scan) and start a scan.
2. Mock a device with RSSI = −50 dBm and select it.
3. Assert: RSSI indicator shows "Good" with green styling (`rssi--good` class).
4. Update mock device RSSI to −65 dBm.
5. Assert: RSSI indicator shows "Marginal" with amber styling (`rssi--marginal` class).
6. Update mock device RSSI to −80 dBm.
7. Assert: RSSI indicator shows "Bad" with red styling (`rssi--bad` class).

---

### T-PT-1221b  RSSI indicator updates on poll interval

**Validates:** PT-1221 (AC 5)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 4, start a scan, and select a device.
2. Change the mock device RSSI from −50 to −80 dBm.
3. Wait for 1 device poll cycle (≤ 1.5 s).
4. Assert: the RSSI indicator updates from "Good" to "Bad".

---

### T-PT-1221c  RSSI boundary values classified correctly

**Validates:** PT-1221 (AC 2, 3, 4)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 4, start a scan, and select a device.
2. Set mock device RSSI to exactly −60 dBm.
3. Assert: RSSI indicator shows "Good" (≥ −60 threshold is inclusive).
4. Set mock device RSSI to exactly −75 dBm.
5. Assert: RSSI indicator shows "Marginal" (−75 ≤ x < −60, −75 is inclusive).
6. Set mock device RSSI to −76 dBm.
7. Assert: RSSI indicator shows "Bad".

---

### T-PT-1220c  Scan stopped when navigating away from scan page

**Validates:** PT-1220 (AC 5)  
**Type:** Manual / platform test

**Procedure:**
1. Navigate to page 4 (Node Scan) and start a scan.
2. Navigate back to page 3.
3. Assert: the BLE scan is stopped.
4. Assert: the selected device is cleared.
5. Navigate forward to page 4 again.
6. Assert: the page shows "No devices found" (scan does not auto-restart).

---

### T-PT-1222a  Page transition animation (forward)

**Validates:** PT-1222 (AC 1, 3)  
**Type:** Manual / platform test

**Procedure:**
1. On page 1, trigger forward navigation.
2. Observe: the new page slides in from the right.
3. Assert: the transition completes within 300 ms.

---

### T-PT-1222b  Page transition animation (back)

**Validates:** PT-1222 (AC 2)  
**Type:** Manual / platform test

**Procedure:**
1. On page 3, trigger back navigation.
2. Observe: the previous page slides in from the left.
3. Assert: the transition completes within 300 ms.

---

### T-PT-1222c  Navigation works without CSS transitions

**Validates:** PT-1222 (AC 4)  
**Type:** Manual / platform test

**Procedure:**
1. Disable CSS transitions (e.g., via `* { transition: none !important; }`).
2. Navigate forward and backward through all pages.
3. Assert: all pages display correctly; no visual glitches or stuck states.

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
| T-PT-110 | PT-0700, PT-1217 | Minimum UI elements present |
| T-PT-111 | PT-0701, PT-1218 | Phase indication updates |
| T-PT-112 | PT-0702 | Verbose diagnostic mode |
| T-PT-113 | PT-0107 | Android activity lifecycle disconnect |
| T-PT-114 | PT-0108 | JNI classloader caching on background threads |
| T-PT-200 | PT-0300 | MTU negotiation ≥ 247 |
| T-PT-201 | PT-0300 | MTU < 247 → disconnect + error |
| T-PT-202 | ~~PT-0301~~ | ~~Gateway authentication happy path~~ — RETIRED |
| T-PT-203 | ~~PT-0301~~ | ~~Gateway authentication failure (bad signature)~~ — RETIRED |
| T-PT-204 | ~~PT-0301~~ | ~~GW_INFO_RESPONSE timeout (45 s)~~ — RETIRED |
| T-PT-205 | ~~PT-0302~~ | ~~TOFU — first connection persists public key~~ — RETIRED |
| T-PT-206 | ~~PT-0302~~ | ~~TOFU — mismatched public key rejected~~ — RETIRED |
| T-PT-207 | ~~PT-0302~~ | ~~TOFU — operator can clear pinned identity~~ — RETIRED |
| T-PT-208 | PT-0303 | Phone registration happy path |
| T-PT-208a | PT-0303 | Phone label validation |
| T-PT-209 | PT-0303 | ERROR(0x02) — registration window closed |
| T-PT-210 | PT-0303 | ERROR(0x03) — already paired |
| T-PT-211 | PT-0303 | PHONE_REGISTERED timeout (30 s) |
| T-PT-212 | ~~PT-0303~~ | ~~Decryption failure (bad GCM tag)~~ — RETIRED |
| T-PT-213 | PT-0304 | Key material zeroing after use |
| T-PT-300 | PT-0400 | Phase 1 prerequisite check |
| T-PT-301 | PT-0401 | Node MTU negotiation |
| T-PT-302 | PT-0402 | Node PSK generation (32 bytes, CSPRNG) |
| T-PT-303 | PT-0402 | key_hint derivation matches SHA-256(psk)[30..32] |
| T-PT-304 | PT-0403 | PairingRequest CBOR deterministic encoding |
| T-PT-305 | PT-0403 | node_id validation |
| T-PT-306 | PT-0403 | rf_channel validation |
| T-PT-307 | PT-1102 | Phone PSK authentication (AES-256-GCM) |
| T-PT-308 | PT-0407, PT-1102 | Payload encryption (AES-256-GCM with phone_psk) |
| T-PT-309 | ~~PT-0405, PT-0902~~ | ~~Ed25519 → X25519 low-order point rejection~~ — RETIRED |
| T-PT-310 | PT-0406 | Payload size > 218 bytes rejected |
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
| T-PT-900 | ~~PT-1101~~ | ~~HKDF parameters correct for Phase 1~~ — RETIRED |
| T-PT-901 | ~~PT-1101~~ | ~~HKDF parameters correct for Phase 2~~ — RETIRED |
| T-PT-902 | PT-1102 | AES-256-GCM with phone_psk round-trip |
| T-PT-903 | PT-1103 | CBOR deterministic encoding (known test vector) |
| T-PT-1004 | PT-1004 | Core crate builds and works without platform features |
| T-PT-1207 | PT-1207 | BLE scan events logged |
| T-PT-1208 | PT-1208 | Connection lifecycle events logged |
| T-PT-1209 | PT-1209 | GATT write and indication events logged |
| T-PT-1210 | PT-1210 | Phase transition events logged |
| T-PT-1211 | PT-1211 | LESC pairing method logged |
| T-PT-1212 | PT-1212 | Error context in log output |
| T-PT-1215a | PT-1215 | Connection error includes device address |
| T-PT-1215b | PT-1215 | MTU error includes device address |
| T-PT-1215c | PT-1215 | Connection dropped includes stale pairing hint |
| T-PT-1215d | PT-1215 | Indication timeout includes device address |
| T-PT-1215e | PT-1215 | `format_device_address` produces canonical format |
| T-PT-1214a | PT-1214 | Pin config included in NODE_PROVISION when provided |
| T-PT-1214b | PT-1214 | No pin config in NODE_PROVISION — backward compatible |
| T-PT-1214c | PT-1214 | Pin config with out-of-range GPIO rejected |
| T-PT-1214d | PT-1214 | Pin config with SDA equal to SCL rejected |
| T-PT-1216a | PT-1216, PT-1214 | Board preset passes correct pin config |
| T-PT-1216b | PT-1216 | Default preset is Espressif DevKitM-1 |
| T-PT-1216c | PT-1216 | Custom board with valid GPIOs |
| T-PT-1216d | PT-1216, PT-0409 | Custom board with invalid GPIOs rejected |
| T-PT-1216e | PT-1216 | Provision with only one pin parameter rejected |
| T-PT-1217a | PT-1217 | Six pages rendered and only one visible at a time |
| T-PT-1217b | PT-1217 | Forward navigation through all pages |
| T-PT-1217c | PT-1217 | Existing functionality works through wizard flow |
| T-PT-1218a | PT-1218 | Stepper bar shows three phases |
| T-PT-1218b | PT-1218 | Stepper highlights current phase |
| T-PT-1219a | PT-1219 | Page index persisted to localStorage |
| T-PT-1219b | PT-1219 | Page restored on app restart |
| T-PT-1220a | PT-1220 | Back navigation returns to previous page |
| T-PT-1220b | PT-1220 | Back navigation on page 1 does nothing |
| T-PT-1220c | PT-1220 | Scan stopped when navigating away from scan page |
| T-PT-1221a | PT-1221 | RSSI indicator shows correct quality level |
| T-PT-1221b | PT-1221 | RSSI indicator updates on poll interval |
| T-PT-1221c | PT-1221 | RSSI boundary values classified correctly |
| T-PT-1222a | PT-1222 | Page transition animation (forward) |
| T-PT-1222b | PT-1222 | Page transition animation (back) |
| T-PT-1222c | PT-1222 | Navigation works without CSS transitions |
