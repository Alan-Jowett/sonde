<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# BLE Pairing Tool Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design of the Sonde BLE pairing tool (`sonde-pair` crate).  
> **Audience:** Implementers (human or LLM agent) building the pairing tool.  
> **Related:** [ble-pairing-tool-requirements.md](ble-pairing-tool-requirements.md), [ble-pairing-tool-validation.md](ble-pairing-tool-validation.md), [ble-pairing-protocol.md](ble-pairing-protocol.md), [security.md](security.md), [gateway-design.md](gateway-design.md)

---

## 1  Overview

The BLE pairing tool is a cross-platform application that provisions Sonde nodes over Bluetooth Low Energy.  It implements two protocol phases:

1. **Phase 1 — Gateway pairing** (one-time): the tool connects to a gateway's BLE service, authenticates via BLE LESC, registers as a pairing agent, and receives a phone PSK over the secure BLE link.
2. **Phase 2 — Node provisioning** (per node): the tool generates a node PSK, constructs an encrypted pairing payload, and writes it to a node's BLE service.  The node stores the payload and relays it to the gateway over ESP-NOW on next boot.

The tool is a Rust-first application following a Tauri-style architecture: all protocol logic, cryptography, and persistence live in a shared Rust crate (`sonde-pair`), with a thin UI shell invoking Rust commands.  The core crate has no platform dependencies and is testable with mocked BLE transport and storage (PT-0101, PT-0102, PT-0104).

---

## 2  Technology choices

| Decision | Choice | Rationale |
|---|---|---|
| Language | Rust | Memory safety, strong typing, zeroize support for key material, consistent with rest of Sonde |
| UI framework | Tauri v2 | Cross-platform desktop + mobile, Rust backend, WebView frontend, Android support via Tauri Mobile |
| Protocol crate | `sonde-protocol` (shared) | CBOR encoding, AES-256-GCM, SHA-256, key_hint derivation — reuses existing workspace crate (PT-0103) |
| BLE library (desktop) | `btleplug` | Cross-platform BLE (Windows WinRT, macOS CoreBluetooth, Linux BlueZ); active maintenance |
| BLE library (Android) | Android BLE API via JNI | `btleplug` does not support Android; Tauri Mobile provides JNI bridge |
| AES-256-GCM | `aes-gcm` | RustCrypto AES-GCM, pure Rust, no OpenSSL dependency — sole frame authentication mechanism (PT-1100) |
| SHA-256 | `sha2` (via `sonde-protocol::Sha256Provider`) | Reuses protocol crate trait for key_hint derivation (PT-0402) |
| CSPRNG | `getrandom` | OS-level CSPRNG, no `rand::rng()` dependency (PT-0901) |
| Key zeroing | `zeroize` | Wraps ephemeral keys, shared secrets, derived AES keys in `Zeroizing<[u8; N]>` (PT-0304, PT-0408) |
| CBOR | `ciborium` (via `sonde-protocol`) | Deterministic encoding for PairingRequest (PT-1103) |
| Logging | `tracing` | Structured logging with level filtering, consistent with other Sonde crates |
| Persistence (desktop) | JSON file in `%APPDATA%\sonde\` (Windows) or `~/.config/sonde/` (Linux/macOS) | Simple, human-debuggable, restricted file permissions (PT-0801) |
| Persistence (Android) | Encrypted SharedPreferences via Android Keystore | Platform-appropriate secure storage (PT-0801) |

---

## 3  Crate structure

The pairing tool is implemented as a new workspace crate `sonde-pair` with a clear four-layer separation (PT-0104):

```
crates/sonde-pair/
├── Cargo.toml
└── src/
    ├── lib.rs                  # Public API surface, module declarations
    ├── types.rs                # Shared data types (GatewayIdentity, PairingArtifacts, NodeProvisionResult)
    ├── error.rs                # PairingError enum (device/transport/protocol categories)
    ├── discovery.rs            # BLE scan logic, device filtering, scan lifecycle
    ├── phase1.rs               # Phase 1 state machine (gateway pairing)
    ├── phase2.rs               # Phase 2 state machine (node provisioning)
    ├── crypto.rs               # Ed25519 verification, AES-GCM, SHA-256
    ├── envelope.rs             # BLE message envelope (TYPE + LEN + BODY) encode/decode
    ├── cbor.rs                 # PairingRequest CBOR construction (deterministic encoding)
    ├── validation.rs           # Input validation (node_id, rf_channel, label, payload size)
    ├── transport.rs            # BleTransport trait definition
    ├── store.rs                # PairingStore trait definition
    └── rng.rs                  # RngProvider trait (injectable CSPRNG for testing)
```

### 3.1  Dependency rules

| Dependency | Allowed |
|---|---|
| `sonde-protocol` | ✅ CBOR, AES-256-GCM, SHA-256, key_hint, constants |
| `sonde-gateway` | ❌ No gateway dependency (PT-0103) |
| `sonde-node` | ❌ No node dependency (PT-0103) |
| `sonde-modem` | ❌ No modem dependency (PT-0103) |
| Platform BLE APIs | ❌ Not in `sonde-pair` — injected via `BleTransport` trait |
| Platform storage APIs | ❌ Not in `sonde-pair` — injected via `PairingStore` trait |

### 3.2  Cargo.toml dependencies

```toml
[dependencies]
sonde-protocol = { path = "../sonde-protocol" }
ed25519-dalek = { version = "2", features = ["zeroize"] }
sha2 = "0.10"
aes-gcm = "0.10"
getrandom = "0.3"
zeroize = { version = "1", features = ["derive"] }
ciborium = "0.2"
tracing = "0.1"
thiserror = "2"

[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros", "time"] }
tracing-test = "0.2"
```

---

## 4  Architecture

### 4.1  Layer separation

```
┌──────────────────────────────────────────────────────────┐
│                     UI Shell (Tauri)                      │
│  Scan toggle, device list, pair button, status display    │
│  (no protocol logic — invokes Rust commands only)         │
├──────────────────────────────────────────────────────────┤
│                   Protocol Logic Layer                    │
│  phase1.rs   phase2.rs   crypto.rs   cbor.rs             │
│  envelope.rs   validation.rs   discovery.rs              │
├──────────────────────────────────────────────────────────┤
│                   Transport Layer                         │
│  BleTransport trait (platform-specific implementations)   │
├──────────────────────────────────────────────────────────┤
│                   Persistence Layer                       │
│  PairingStore trait (platform-specific implementations)   │
└──────────────────────────────────────────────────────────┘
```

### 4.2  Phase 1 state machine — Gateway pairing

The Phase 1 state machine drives the gateway pairing flow defined in [ble-pairing-protocol.md §5](ble-pairing-protocol.md).  It is implemented as an async function that takes a `BleTransport`, `PairingStore`, and `RngProvider` and returns `Result<PairingArtifacts, PairingError>`.

```
┌─────────┐
│  Idle   │
└────┬────┘
     │ operator selects gateway device
     ▼
┌─────────────────┐
│  Connecting     │──── MTU < 247 ────► Error("MTU too low")
└────┬────────────┘                      disconnect
     │ MTU ≥ 247
     ▼
┌─────────────────────────┐
│ Authenticating          │
│ write REQUEST_GW_INFO   │
│ (32-byte challenge)     │
│ wait GW_INFO_RESPONSE   │──── timeout 45s ───► Error("timeout")
│                         │──── bad signature ──► Error("auth failed")
└────┬────────────────────┘                      disconnect
     │ signature valid
     ▼
┌─────────────────┐
│ TOFU Check      │──── key mismatch ──► Error("public key mismatch")
└────┬────────────┘                      disconnect
     │ first use → pin key; or key matches stored key
     ▼
┌─────────────────────────┐
│ Registering             │
│ generate ephemeral X25519│
│ write REGISTER_PHONE    │
│ wait response           │──── timeout 30s ────► Error("timeout")
│                         │──── ERROR(0x02) ────► Error("window closed")
│                         │──── ERROR(0x03) ────► Error("already paired")
│                         │──── bad GCM tag ────► Error("decrypt failed")
└────┬────────────────────┘                      disconnect
     │ decrypt PHONE_REGISTERED
     │ extract phone_psk, phone_key_hint, rf_channel
     │ zero ephemeral key, shared secret, AES key
     ▼
┌─────────────────┐
│  Persist        │ persist all artifacts to PairingStore
│  Disconnect     │
│  Success        │
└─────────────────┘
```

**Key design decisions:**

- TOFU check occurs *after* the challenge–response exchange.  The tool first authenticates the gateway via Ed25519 signature verification, then compares the received `gw_public_key` against any previously pinned key in the store.  On first use, the key is pinned; on subsequent connections, a mismatch is rejected.  This ordering ensures the gateway is live and holds the claimed private key before the TOFU decision is made (PT-0302).
- No artifacts are persisted until the entire flow succeeds.  On any error, the BLE connection is released and the store is left unchanged (PT-0502).
- Already-paired detection: if the store contains a `gw_public_key`, the tool warns the operator before starting Phase 1 and offers to proceed or cancel (PT-0601).

### 4.3  Phase 2 state machine — Node provisioning

The Phase 2 state machine implements the node provisioning flow from [ble-pairing-protocol.md §6](ble-pairing-protocol.md).  It takes a `BleTransport`, `PairingStore`, `RngProvider`, and operator-supplied `node_id` and returns `Result<NodeProvisionResult, PairingError>`.

```
┌─────────────┐
│ Prerequisite│──── no phone_psk ──► Error("complete Phase 1 first")
│ Check       │
└────┬────────┘
     │ phone_psk present
     ▼
┌─────────────────┐
│  Connecting     │──── MTU < 247 ────► Error("MTU too low")
└────┬────────────┘                      disconnect
     │ MTU ≥ 247
     ▼
┌─────────────────────────────┐
│ Build NODE_PROVISION        │
│ 1. Validate node_id, channel│──── invalid ────► Error (before BLE)
│ 2. Generate node_psk (CSPRNG)│
│ 3. Derive node_key_hint     │
│ 4. Build PairingRequest CBOR│
│ 5. Encrypt with phone_psk   │
│ 6. Encrypt with gw_public_key│
│ 7. Check payload ≤ 202 bytes│──── too large ──► Error (before BLE)
│ 8. Assemble NODE_PROVISION   │
└────┬────────────────────────┘
     │
     ▼
┌─────────────────────────┐
│ Provisioning            │
│ write NODE_PROVISION    │
│ wait NODE_ACK           │──── timeout 5s ────► Error("no response")
│                         │──── ACK(0x01) ─────► Error("already paired")
│                         │──── ACK(0x02) ─────► Error("storage error")
└────┬────────────────────┘                      disconnect
     │ ACK(0x00)
     │ zero node_psk, ephemeral keys, shared secret, AES key
     ▼
┌─────────────────┐
│  Disconnect     │
│  Success        │ return node_id, node_key_hint, rf_channel
└─────────────────┘
```

**Key design decisions:**

- All validation and payload construction happen *before* the BLE write.  The tool rejects invalid inputs (empty `node_id`, `rf_channel` out of range, payload > 202 bytes) without touching BLE (PT-0403, PT-0406).
- `node_psk` is never persisted to disk.  It exists only in memory during provisioning and is zeroed via `Zeroizing` after the `NODE_PROVISION` write succeeds (PT-0408, PT-0804).
- A fresh ephemeral X25519 keypair is generated for each provisioning attempt (PT-0405).

### 4.1  NODE_PROVISION body wire format

```
Offset  Size           Field
──────  ─────────────  ──────────────────────────────────────────
0       2              node_key_hint     (BE u16)
2       32             node_psk          (256-bit PSK)
34      1              rf_channel        (1–13)
35      2              payload_len       (BE u16, encrypted payload length)
37      payload_len    encrypted_payload (opaque blob for gateway)
37+N    remaining      pin_config_cbor   (optional, CBOR map — see below)
```

**Pin config (ND-0608):** If the NODE_PROVISION body is longer than `37 + payload_len`, the remaining bytes are a deterministic CBOR map (RFC 8949 §4.2) of board-specific pin assignments:

| CBOR key | Field | Type | Default |
|----------|-------|------|---------|
| 1 | `i2c0_sda` | uint | 0 |
| 2 | `i2c0_scl` | uint | 1 |

The node persists these to NVS. If the map is absent (older pairing tool), the node uses compiled-in defaults. Future keys (SPI pins, pairing button GPIO) may be added without breaking backward compatibility.

**Pin config source:** The pairing tool obtains `i2c0_sda` and `i2c0_scl` values from the bundle manifest's `hardware.pins` section (see [bundle-format.md](bundle-format.md) §4.5).  When the bundle declares I2C sensors for a node, the `pins` section is required and the pairing tool passes the values through to `provision_node` as an optional `PinConfig` parameter.  This keeps the bundle self-contained — no separate board profile management is needed.

---

## 5  BLE transport layer

### 5.1  `BleTransport` trait

All platform-specific BLE operations are abstracted behind the `BleTransport` trait (PT-0102).  The core `sonde-pair` crate calls only this trait — no platform BLE APIs appear in protocol logic.

```rust
/// A discovered BLE device.
pub struct ScannedDevice {
    /// Opaque platform handle for connecting to this device.
    pub id: DeviceId,
    /// BLE advertising name (e.g., "sonde-ABCD").
    pub name: Option<String>,
    /// Which Sonde service this device advertises.
    pub service_type: ServiceType,
    /// Signal strength in dBm, if available.
    pub rssi: Option<i16>,
}

pub enum ServiceType {
    GatewayPairing,
    NodeProvisioning,
}

/// Opaque device identifier — platform-specific.
pub type DeviceId = Vec<u8>;

#[async_trait]
pub trait BleTransport: Send + Sync {
    /// Start scanning for Sonde BLE services.
    /// Returns a stream of discovered devices.
    /// Filters to Gateway Pairing Service (0000FE60-…)
    /// and Node Provisioning Service (0000FE50-…) UUIDs.
    async fn start_scan(&self) -> Result<(), PairingError>;

    /// Stop an active scan.
    async fn stop_scan(&self) -> Result<(), PairingError>;

    /// Get the current list of discovered devices.
    async fn get_scan_results(&self) -> Result<Vec<ScannedDevice>, PairingError>;

    /// Connect to a device. Returns the negotiated ATT MTU.
    /// The implementation handles LESC pairing: Numeric Comparison
    /// is required for gateway connections (PT-0300) — a Just Works
    /// fallback MUST be treated as a connection failure.  Just Works
    /// is acceptable only for node provisioning connections.
    /// Connection establishment MUST time out after 10 seconds (PT-1002).
    async fn connect(&self, device: &DeviceId) -> Result<u16, PairingError>;

    /// Disconnect from the currently connected device.
    async fn disconnect(&self) -> Result<(), PairingError>;

    /// Write data to the characteristic (Gateway Command or Node Command).
    /// The implementation selects the correct characteristic UUID
    /// based on the connected device's service type.
    /// Handles Write Long fragmentation if data exceeds (MTU - 3).
    async fn write(&self, data: &[u8]) -> Result<(), PairingError>;

    /// Wait for an indication from the connected device.
    /// Handles reassembly of multi-indication messages per §3.4.
    /// Returns the complete reassembled envelope.
    async fn read_indication(&self, timeout: Duration) -> Result<Vec<u8>, PairingError>;
}
```

### 5.2  GATT service discovery

The transport implementation discovers services and characteristics during `connect()`:

- **Gateway device:** Look for Gateway Pairing Service (`0000FE60-0000-1000-8000-00805F9B34FB`), then Gateway Command characteristic (`0000FE61-0000-1000-8000-00805F9B34FB`).  Subscribe to indications on the characteristic.
- **Node device:** Look for Node Provisioning Service (`0000FE50-0000-1000-8000-00805F9B34FB`), then Node Command characteristic (`0000FE51-0000-1000-8000-00805F9B34FB`).  Subscribe to indications.

If the expected service or characteristic is not found after connection, `connect()` returns a transport-level error (PT-0500).

### 5.3  MTU negotiation

The transport requests ATT MTU ≥ 247 during connection.  The actual negotiated MTU is returned from `connect()`.  The protocol layer checks the returned MTU and disconnects if < 247 (PT-0300, PT-0401).

### 5.4  Indication reassembly

Per [ble-pairing-protocol.md §3.4](ble-pairing-protocol.md), indications may be fragmented across multiple ATT Handle Value Indications.  The transport implementation:

1. Receives the first indication chunk.  Parses the envelope header (TYPE + LEN) to determine the expected total body length.
2. Buffers subsequent indication chunks until accumulated body bytes equal `LEN`.
3. Returns the complete envelope (TYPE + LEN + full BODY) to the caller.
4. If no further chunks arrive within the timeout, returns a timeout error.

### 5.5  Connection lifecycle

The transport guarantees cleanup on all paths:

- On successful disconnect: releases GATT subscription, closes connection.
- On error (connection drop, timeout, protocol error): the protocol layer calls `disconnect()`.  The transport releases resources even if the BLE connection is already lost (PT-1001).
- Stale device eviction: during scanning, devices that stop advertising for > 10 s are removed from scan results (PT-0202).

### 5.6  Mock BLE transport

A `MockBleTransport` is provided for testing (PT-1200).  It implements the `BleTransport` trait with:

- **Configurable scan results:** injected `ScannedDevice` entries.
- **Configurable MTU:** set the MTU returned by `connect()`.
- **Indication queue:** pre-loaded indication responses that are returned in order by `read_indication()`.
- **Write capture:** all `write()` calls are recorded and can be inspected by test assertions.
- **Error injection:** connection failure, indication timeout, malformed indication, mid-operation disconnect.

```rust
pub struct MockBleTransport {
    scan_results: Vec<ScannedDevice>,
    mtu: u16,
    indication_queue: VecDeque<Result<Vec<u8>, PairingError>>,
    writes: Vec<Vec<u8>>,
    connected: bool,
}
```

---

## 6  Cryptographic operations

All cryptographic operations are implemented in `crypto.rs`.  Key material is wrapped in `zeroize::Zeroizing` throughout (PT-0304, PT-0408).

### 6.1  Ed25519 signature verification (Phase 1)

Used to authenticate the gateway's `GW_INFO_RESPONSE` (PT-0301).

```rust
/// Verify Ed25519 signature over (challenge ‖ gateway_id).
pub fn verify_gateway_signature(
    gw_public_key: &[u8; 32],
    challenge: &[u8; 32],
    gateway_id: &[u8; 16],
    signature: &[u8; 64],
) -> Result<(), PairingError>
```

Uses `ed25519_dalek::VerifyingKey::verify_strict()` to reject non-canonical signatures.

### 6.2  Ed25519 → X25519 conversion

Used in both Phase 1 (decrypting `PHONE_REGISTERED`) and Phase 2 (encrypting pairing payload) (PT-0902).

```rust
/// Convert an Ed25519 public key to X25519 public key.
/// Rejects low-order points (returns error).
pub fn ed25519_to_x25519_public(
    ed_public: &[u8; 32],
) -> Result<x25519_dalek::PublicKey, PairingError>
```

Uses `curve25519_dalek::edwards::CompressedEdwardsY` → `to_montgomery()` conversion.  After conversion, checks that the resulting X25519 public key is not a low-order point (all-zero or small-order Curve25519 points).  Returns `PairingError::InvalidGatewayPublicKey` on failure.

### 6.3  AES-256-GCM encryption/decryption — RETIRED (§6.3 renumbered)

> **RETIRED (issue #628).** X25519 ECDH key agreement and HKDF key derivation are no longer used. AES-256-GCM with pre-shared keys (PSK-direct) replaces all asymmetric cryptography in the pairing flow.

### 6.4  AES-256-GCM encryption/decryption

Used for Phase 1 (decrypt `PHONE_REGISTERED`) and Phase 2 (encrypt pairing payload) (PT-1102).

```rust
/// Decrypt AES-256-GCM ciphertext.
/// AAD = gateway_id.  Nonce is extracted from the first 12 bytes.
pub fn aes_gcm_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8; 16],
) -> Result<Vec<u8>, PairingError>

/// Encrypt plaintext with AES-256-GCM.
/// AAD = gateway_id.  Nonce is 12 random bytes from rng.
/// Returns (nonce, ciphertext_with_tag).
pub fn aes_gcm_encrypt(
    key: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8; 16],
    rng: &dyn RngProvider,
) -> Result<([u8; 12], Vec<u8>), PairingError>
```

### 6.5  SHA-256 key_hint derivation

Derives `key_hint` from a PSK (PT-0402).  Uses `sonde_protocol::Sha256Provider` with a software implementation.

```rust
/// Derive key_hint = u16::from_be_bytes(SHA-256(psk)[30..32]).
pub fn derive_key_hint(psk: &[u8; 32]) -> u16
```

This function reuses the `sonde_protocol` key_hint convention used by the radio protocol.

### 6.8  Cryptographic material lifecycle

All intermediate cryptographic material is explicitly zeroed after use:

| Material | Lifetime | Zeroed when |
|---|---|---|
| Node PSK (Phase 2) | Generated → `NODE_PROVISION` written | After `NODE_ACK(0x00)` received |
| Phone PSK (Phase 1) | Received over BLE LESC → persisted | After write to PairingStore completes |

All values above are wrapped in `Zeroizing<[u8; N]>` to ensure zeroing on drop even in error paths.

---

## 7  Persistence

### 7.1  `PairingStore` trait

All storage operations are behind the `PairingStore` trait (PT-0802).  This enables platform-specific secure storage backends and in-memory mocks for testing.

```rust
/// Pairing artifacts stored after successful Phase 1.
pub struct PairingArtifacts {
    pub gw_public_key: [u8; 32],
    pub gateway_id: [u8; 16],
    pub phone_psk: [u8; 32],
    pub phone_key_hint: u16,
    pub rf_channel: u8,
    pub phone_label: String,
}

#[async_trait]
pub trait PairingStore: Send + Sync {
    /// Load pairing artifacts from storage.
    /// Returns None if no pairing exists.
    /// Returns Err on corruption or I/O failure.
    async fn load(&self) -> Result<Option<PairingArtifacts>, PairingError>;

    /// Persist pairing artifacts after successful Phase 1.
    /// Overwrites any existing artifacts.
    async fn save(&self, artifacts: &PairingArtifacts) -> Result<(), PairingError>;

    /// Clear all pairing artifacts (operator-initiated reset).
    async fn clear(&self) -> Result<(), PairingError>;
}
```

### 7.2  Stored artifacts

After successful Phase 1, the following artifacts are persisted (PT-0800):

| Field | Size | Source |
|-------|------|--------|
| `gw_public_key` | 32 bytes | `GW_INFO_RESPONSE` |
| `gateway_id` | 16 bytes | `GW_INFO_RESPONSE` |
| `phone_psk` | 32 bytes | Decrypted from `PHONE_REGISTERED` |
| `phone_key_hint` | 2 bytes | Decrypted from `PHONE_REGISTERED` |
| `rf_channel` | 1 byte | Decrypted from `PHONE_REGISTERED` |
| `phone_label` | Variable (max 64 bytes UTF-8) | Operator-supplied |

**No node PSK is ever persisted** (PT-0804).  Node PSKs exist only in memory during Phase 2 and are zeroed after provisioning.

### 7.3  Platform storage backends

#### Windows (`FilePairingStore`)

- Location: `%APPDATA%\sonde\pairing.json`
- Format: JSON-serialized `PairingArtifacts` (keys as hex strings)
- PSK bytes are hex-encoded in the JSON; the file is created with restricted permissions (user-only read/write via `SetFileSecurityW`)
- On corruption (invalid JSON, missing fields): returns `PairingError::StoreCorrupted` with a clear message and offers to reset (PT-0803)

#### Linux (`FilePairingStore` + `SecretServicePskProtector`)

- Location: `~/.config/sonde/pairing.json`
- Format: same JSON layout as Windows, but the `phone_psk` field is protected by the Linux Secret Service keyring rather than stored in plaintext
- The `SecretServicePskProtector` (enabled via the `secret-service-store` Cargo feature) stores and retrieves the 32-byte PSK through D-Bus using GNOME Keyring, KWallet, or any other freedesktop.org Secret Service-compatible backend
- The PSK is stored as a binary secret under attributes `service = "sonde-pair"` and `account = "sonde-pair-phone-psk"` (configurable label)
- On `protect()`, the PSK is written to the keyring and the label is returned as opaque bytes for the JSON file; on `unprotect()`, the label is used to look up the PSK from the keyring
- When the `secret-service-store` feature is enabled but no Secret Service provider is available or the keyring cannot be accessed/unlocked at runtime, pairing operations that need the `phone_psk` return an error rather than silently falling back to plaintext storage

#### Plaintext-to-encrypted storage migration

When a `PskProtector` is configured (e.g. the DPAPI protector on Windows or the Secret Service protector on Linux), the `FilePairingStore` transparently migrates legacy plaintext PSK data on load:

1. On `load_artifacts()`, if the JSON contains a plaintext `phone_psk` field but no `phone_psk_protected` field, the PSK is read from plaintext and a `tracing::warn!` is emitted: *"phone_psk stored in plaintext — will be encrypted on next save"*.
2. The next `save_artifacts()` call writes the PSK through the configured protector, producing a `phone_psk_protected` field and omitting the plaintext `phone_psk` field.
3. This migration is idempotent and requires no operator action.  The warning log provides visibility into the one-time upgrade.

#### Android (`AndroidPairingStore`)

- Backend: Android `EncryptedSharedPreferences` backed by the Android Keystore
- Keys: `gw_public_key`, `gateway_id`, `phone_psk`, `phone_key_hint`, `rf_channel`, `phone_label`
- Accessed via JNI bridge from Rust (Tauri Mobile provides the JNI environment)
- On corruption: clears the corrupted preferences and returns `PairingError::StoreCorrupted`

#### Test (`MockPairingStore`)

- In-memory `Arc<Mutex<Option<PairingArtifacts>>>` for testing
- Supports pre-loading with test data
- Can be configured to simulate corruption (return errors on `load()`)

### 7.4  Atomic persistence

The `save()` operation is atomic: write to a temporary file, then rename over the target file (on platforms supporting atomic rename).  This prevents a crash during write from corrupting the store (PT-0502).

---

## 8  Error handling

### 8.1  Error categories

The `PairingError` enum distinguishes three categories (PT-0500):

```rust
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    // ── Device-level errors ──
    #[error("BLE adapter not found — enable Bluetooth in system settings")]
    AdapterNotFound,

    #[error("Bluetooth is disabled — enable Bluetooth in system settings")]
    BluetoothDisabled,

    #[error("device out of range — move closer and retry")]
    DeviceOutOfRange,

    // ── Transport-level errors ──
    #[error("BLE connection dropped — retry the operation")]
    ConnectionDropped,

    #[error("MTU too low ({actual}) — device requires MTU ≥ 247")]
    MtuTooLow { actual: u16 },

    #[error("GATT write failed — retry the operation")]
    GattWriteFailed,

    #[error("{operation} timed out after {timeout_secs}s — {suggestion}")]
    Timeout {
        operation: &'static str,
        timeout_secs: u64,
        suggestion: &'static str,
    },

    // ── Protocol-level errors ──
    #[error("gateway authentication failed — possible impersonation")]
    GatewayAuthFailed,

    #[error("gateway public key mismatch — a different gateway was expected; \
             clear the pinned identity to pair with a new gateway")]
    PublicKeyMismatch,

    #[error("registration window not open — ask the operator to hold the \
             gateway button for 2 seconds")]
    RegistrationWindowClosed,

    #[error("already paired with this gateway")]
    AlreadyPaired,

    #[error("decryption failed — ephemeral key mismatch")]
    DecryptionFailed,

    #[error("node already paired — hold the pairing button during boot \
             to factory reset before re-pairing")]
    NodeAlreadyPaired,

    #[error("node storage error — the node's NVS write failed")]
    NodeStorageError,

    #[error("no gateway pairing found — complete Phase 1 first")]
    NoGatewayPairing,

    #[error("encrypted payload too large ({size} bytes, max {max} bytes) — \
             reduce node_id length or sensor metadata")]
    PayloadTooLarge { size: usize, max: usize },

    #[error("invalid gateway public key — Ed25519 to X25519 conversion failed")]
    InvalidGatewayPublicKey,

    #[error("invalid node_id — must be 1–64 bytes UTF-8")]
    InvalidNodeId,

    #[error("invalid rf_channel ({channel}) — must be 1–13")]
    InvalidRfChannel { channel: u8 },

    #[error("invalid phone label — must be 0–64 bytes UTF-8")]
    InvalidLabel,

    // ── Storage errors ──
    #[error("pairing store corrupted — reset the store to continue")]
    StoreCorrupted,

    #[error("pairing store I/O error: {0}")]
    StoreIo(String),

    // ── Internal errors ──
    #[error("malformed BLE envelope — incomplete or invalid header")]
    MalformedEnvelope,

    #[error("random number generation failed — OS CSPRNG unavailable")]
    RngFailed,
}
```

Every variant includes an actionable message for the operator (PT-0501).  No error message consists solely of a code or internal identifier.

### 8.2  Error recovery

On any error after a BLE connection is established:

1. Call `transport.disconnect()` to release the BLE connection (PT-1001).
2. Return the error to the caller.  Do **not** persist any partial state (PT-0502).
3. The tool returns to the idle/scanning state.  The operator can retry without restarting the application (PT-1000).

### 8.3  No implicit retries

The tool does not silently retry failed protocol operations (PT-1003).  BLE-level connection retries by the platform stack are acceptable, but protocol-level operations (GATT writes, indication waits) are single-attempt.  The operator decides whether to retry.

---

## 9  Platform-specific considerations

### 9.1  Windows (WinRT Bluetooth stack)

- **BLE library:** `btleplug` uses the WinRT Bluetooth API via `windows` crate bindings.
- **Scan filter:** WinRT's `BluetoothLEAdvertisementWatcher` does not reliably match 16-bit BLE service UUIDs passed as expanded 128-bit UUIDs in the `ScanFilter`.  The `BtleplugTransport` scans with an empty filter and relies on the `DeviceScanner` application layer for UUID-based filtering.
- **MTU negotiation:** WinRT handles ATT MTU exchange during connection.  The negotiated MTU is available via `BluetoothLEDevice.MaxPduSize`.  Note that WinRT may negotiate a lower MTU than requested; the protocol layer handles the < 247 rejection.
- **Numeric Comparison:** The modem initiates LESC pairing server-side via `ble_gap_security_initiate` (MD-0404 criterion 5).  WinRT responds to the SMP Security Request by presenting the OS pairing dialog.  `btleplug` does not expose the negotiated pairing method to user-space, so `BtleplugTransport::pairing_method()` returns `None` to indicate OS-enforced security (PT-0904).  A Just Works fallback for gateway connections MUST be treated as a connection failure (PT-0300).
- **Pre-connect scan:** When `pair_gateway` creates a fresh `BtleplugTransport`, the adapter has no cached peripherals.  The `connect()` method runs a short 3-second scan if the target is not found in the cache.
- **Storage:** `%APPDATA%\sonde\pairing.json` with restricted file permissions (ACL: user-only read/write).
- **Known issues:** Some Windows BLE drivers have limited Write Long support.  The transport should fall back to standard writes if the payload fits within (MTU − 3) bytes and only use Write Long for larger messages.
- **GATT write retry (WinRT auth errors):** On Windows, a GATT write issued before WinRT has completed its internal authentication handshake fails with `HRESULT 0x80650005`.  `BtleplugTransport` retries the write up to 6 times with a 5-second delay between attempts, allowing the OS pairing dialog and LESC handshake to complete.  If all retries are exhausted, the write is reported as failed.

### 9.2  Android (Android BLE API)

- **BLE library:** Android BLE API accessed via JNI bridge (Tauri Mobile).  `btleplug` does not support Android, so the `BleTransport` trait implementation calls the Android `BluetoothGatt` API through JNI.
- **Permissions:** The Android manifest must declare `BLUETOOTH_SCAN` and `BLUETOOTH_CONNECT` for Android 12+ (API 31+) BLE scanning.  For pre-31 devices, BLE scanning requires a location permission such as `ACCESS_FINE_LOCATION`/`ACCESS_COARSE_LOCATION`.  The app must request the relevant runtime permissions before starting a scan (PT-0105).

  **Runtime permission request mechanism:** Because the Tauri runtime owns the main `Activity` and Android's `requestPermissions()` requires an `Activity` context with an `onRequestPermissionsResult()` callback, a lightweight helper `PermissionActivity` is used.  `BleHelper.requestBlePermissions()` determines the missing permissions based on API level, launches a transparent `PermissionActivity` via `Intent` with `FLAG_ACTIVITY_NEW_TASK`, and blocks on a `CompletableFuture` until the user responds to the system dialog.  `PermissionActivity` calls `requestPermissions()` in `onCreate()`, receives the result in `onRequestPermissionsResult()`, completes the future, and finishes itself.  `BleHelper.startScan()` and `BleHelper.connect()` call `requestBlePermissions()` before checking permissions, so the system consent dialog appears automatically on first BLE use — no separate permission step is required from the frontend.
- **MTU negotiation:** Call `BluetoothGatt.requestMtu(247)` after connection.  The actual negotiated MTU is reported via `onMtuChanged()`.
- **Just Works / Numeric Comparison:** Android handles LESC pairing via the system pairing dialog.  Numeric Comparison displays a 6-digit passkey for user confirmation.  Just Works proceeds without user interaction.  The app must verify that LESC Numeric Comparison was used; a Just Works fallback must be treated as a connection failure (PT-0106, PT-0904).
- **Storage:** `EncryptedSharedPreferences` backed by the Android Keystore for PSK protection.
- **Lifecycle:** The BLE connection must be managed carefully around Android activity lifecycle events (pause/resume).  The transport implementation should disconnect on pause and reconnect on resume if a pairing flow was in progress (PT-0107).
- **JNI classloader caching:** App-defined Java classes (`BleHelper`, `SecureStore`) must be resolved and cached as `GlobalRef` from `JNI_OnLoad` or another Java-attached thread that uses the application classloader.  Tokio worker threads use the system classloader, which cannot find app-defined classes via `FindClass` (PT-0108).

### 9.3  Cross-platform considerations

- **MTU defaults:** If the platform does not support explicit MTU negotiation, the transport reports the platform-default MTU.  Most modern BLE 5.0+ stacks negotiate 247+ by default.
- **BLE adapter availability:** The transport checks for BLE adapter presence before scanning.  Returns `PairingError::AdapterNotFound` or `PairingError::BluetoothDisabled` as appropriate.
- **Future iOS support:** The `BleTransport` trait is designed to be implementable on iOS via CoreBluetooth.  No iOS-specific APIs are assumed by the core crate (PT-0100).

---

## 10  BLE message envelope

### 10.1  Envelope codec

The `envelope.rs` module handles encoding and decoding the BLE message envelope used on both Gateway Command and Node Command characteristics ([ble-pairing-protocol.md §4](ble-pairing-protocol.md)):

```
┌──────────┬──────────┬───────────────────────────┐
│ TYPE (1B)│ LEN (2B) │ BODY (0..65535 bytes)      │
│          │ BE u16   │                             │
└──────────┴──────────┴───────────────────────────┘
```

```rust
/// Encode a BLE message envelope.
/// Returns an error if body exceeds 65535 bytes (u16::MAX).
pub fn encode_envelope(msg_type: u8, body: &[u8]) -> Result<Vec<u8>, PairingError> {
    if body.len() > u16::MAX as usize {
        return Err(PairingError::PayloadTooLarge {
            size: body.len(),
            max: u16::MAX as usize,
        });
    }
    let mut buf = Vec::with_capacity(3 + body.len());
    buf.push(msg_type);
    buf.extend_from_slice(&(body.len() as u16).to_be_bytes());
    buf.extend_from_slice(body);
    Ok(buf)
}

/// Decode a BLE message envelope.
/// Returns (msg_type, body).
pub fn decode_envelope(data: &[u8]) -> Result<(u8, &[u8]), PairingError> {
    if data.len() < 3 {
        return Err(PairingError::MalformedEnvelope);
    }
    let msg_type = data[0];
    let len = u16::from_be_bytes([data[1], data[2]]) as usize;
    if data.len() < 3 + len {
        return Err(PairingError::MalformedEnvelope);
    }
    Ok((msg_type, &data[3..3 + len]))
}
```

### 10.2  Message type constants

| Constant | Value | Direction | Service |
|----------|-------|-----------|---------|
| `REQUEST_GW_INFO` | `0x01` | Phone → GW | Gateway Command |
| `REGISTER_PHONE` | `0x02` | Phone → GW | Gateway Command |
| `GW_INFO_RESPONSE` | `0x81` | GW → Phone | Gateway Command |
| `PHONE_REGISTERED` | `0x82` | GW → Phone | Gateway Command |
| `NODE_PROVISION` | `0x01` | Phone → Node | Node Command |
| `NODE_ACK` | `0x81` | Node → Phone | Node Command |
| `ERROR` | `0xFF` | Either | Either |

---

## 11  RNG provider

All randomness is injectable via the `RngProvider` trait (PT-0901).  This enables deterministic testing with mock RNG while using OS CSPRNG in production.

```rust
/// Injectable RNG provider for all cryptographic randomness.
pub trait RngProvider: Send + Sync {
    /// Fill the buffer with random bytes.
    fn fill(&self, buf: &mut [u8]) -> Result<(), PairingError>;
}

/// Production RNG provider using OS CSPRNG.
pub struct OsRng;

impl RngProvider for OsRng {
    fn fill(&self, buf: &mut [u8]) -> Result<(), PairingError> {
        getrandom::fill(buf).map_err(|_| PairingError::RngFailed)?;
        Ok(())
    }
}
```

The mock RNG for testing returns deterministic bytes, enabling reproducible test vectors (T-PT-702).

---

## 12  Input validation

The `validation.rs` module validates all user inputs before any BLE or cryptographic operation (PT-0403, PT-1205):

| Input | Validation rule | Error |
|-------|----------------|-------|
| `node_id` | 1–64 bytes UTF-8 | `PairingError::InvalidNodeId` |
| `rf_channel` | 1–13 inclusive | `PairingError::InvalidRfChannel` |
| `phone_label` | 0–64 bytes UTF-8 | `PairingError::InvalidLabel` |
| Encrypted payload | ≤ 202 bytes | `PairingError::PayloadTooLarge` |

All validation occurs *before* any BLE write, ensuring that invalid inputs never reach the transport layer.

---

## 13  Module-by-module implementation order

Following the pattern established in [implementation-guide.md](implementation-guide.md), the `sonde-pair` crate is built in three sub-phases.  Each step produces a testable artifact before proceeding to the next.

### Phase P1: Foundation (steps P1.1–P1.7)

Core types, traits, and standalone modules.  Each module is testable in isolation.

| Step | Module | What to build | Test with |
|---|---|---|---|
| P1.1 | `types.rs` | `PairingArtifacts`, `NodeProvisionResult`, `ScannedDevice`, `ServiceType`, `DeviceId` | Compile check |
| P1.2 | `error.rs` | `PairingError` enum with all variants and actionable messages | Compile check |
| P1.3 | `transport.rs` | `BleTransport` trait + `MockBleTransport` with scan results, MTU config, indication queue, write capture, error injection | T-PT-100 to T-PT-104 |
| P1.4 | `store.rs` | `PairingStore` trait + `MockPairingStore` (in-memory, corruption simulation) | T-PT-600 to T-PT-603 |
| P1.5 | `rng.rs` | `RngProvider` trait + `OsRng` + `MockRng` for testing | T-PT-702 |
| P1.6 | `envelope.rs` | BLE message envelope encode/decode (TYPE + LEN + BODY) | Unit tests |
| P1.7 | `validation.rs` | `node_id`, `rf_channel`, `phone_label` validation functions | T-PT-305, T-PT-306, T-PT-208a |

**Exit criteria (P1):** All foundation modules compile.  MockBleTransport, MockPairingStore, and MockRng are functional.  Validation tests pass.

### Phase P2: Cryptography and CBOR (steps P2.1–P2.4)

Cryptographic operations and CBOR construction — testable with known test vectors and no BLE dependency.

| Step | Module | What to build | Test with |
|---|---|---|---|
| P2.1 | `crypto.rs` (signature) | `verify_gateway_signature()`, `ed25519_to_x25519_public()` | T-PT-202, T-PT-203, T-PT-309 |
| P2.2 | `crypto.rs` (AES-GCM) | `aes_gcm_encrypt()`, `aes_gcm_decrypt()`, `derive_key_hint()` | T-PT-307, T-PT-308, T-PT-902, T-PT-303 |
| P2.3 | `cbor.rs` | `PairingRequest` CBOR construction with deterministic encoding (RFC 8949 §4.2) | T-PT-304, T-PT-903 |

**Exit criteria (P2):** All cryptographic operations pass known test vectors.  CBOR encoding is deterministic and matches precomputed reference vectors.  AES-GCM AAD is verified.

### Phase P3: Protocol state machines (steps P3.1–P3.4)

Connect foundation and crypto into the Phase 1 and Phase 2 state machines.

| Step | Module | What to build | Test with |
|---|---|---|---|
| P3.1 | `discovery.rs` | Scan lifecycle (start, stop, timeout, stale eviction), device filtering by service UUID | T-PT-100 to T-PT-104 |
| P3.2 | `phase1.rs` | Phase 1 state machine: connect → TOFU → authenticate → register → decrypt → persist | T-PT-200 to T-PT-213 |
| P3.3 | `phase2.rs` | Phase 2 state machine: prerequisite check → connect → build payload → provision → ACK | T-PT-300 to T-PT-315 |
| P3.4 | Integration | Error handling, idempotency, security, non-functional tests | T-PT-400 to T-PT-402, T-PT-500 to T-PT-502, T-PT-700 to T-PT-703, T-PT-800 to T-PT-802 |

**Exit criteria (P3):** `cargo test -p sonde-pair` — all validation tests pass (T-PT-100 through T-PT-903).  Full Phase 1 and Phase 2 flows execute against MockBleTransport and MockPairingStore.  No key material appears in logs.  All error paths produce actionable messages.

### Phase P4: Platform implementations and UI (steps P4.1–P4.3)

Platform-specific BLE transport and storage implementations, plus the Tauri UI shell.  These steps require platform-specific tooling and may involve manual testing on physical hardware.

| Step | Module | What to build | Test with |
|---|---|---|---|
| P4.1 | `BtleplugTransport` | `BleTransport` implementation for Windows/Linux/macOS using `btleplug` | Manual BLE hardware test |
| P4.2 | `FilePairingStore` | JSON file storage for Windows/Linux/macOS with restricted permissions | Unit tests + manual |
| P4.3 | Tauri UI | Scan toggle, device list, pair button, node_id input, status area, error display | Manual test, PT-1206 |

**Exit criteria (P4):** Phase 1 and Phase 2 work end-to-end on physical hardware (Windows, Android) against a real gateway and node.  All PT-1206 manual test scenarios pass.

---

## 14  Diagnostic logging

The pairing tool uses the `tracing` crate (§2) for structured, level-filtered diagnostic logging (PT-0702, PT-1207–PT-1212).

### 14.1  Architecture

Logging is implemented as direct `tracing` macro calls (`debug!`, `info!`, `trace!`, `warn!`) at each operational boundary.  The library crate (`sonde-pair`) emits tracing events but does **not** install a subscriber — that is the responsibility of the application entry point (Tauri shell or CLI harness).  This keeps the core crate dependency-free and allows the host to choose the output format and verbosity.

A typical entry point configures:

```rust
use tracing_subscriber::EnvFilter;

#[cfg(debug_assertions)]
const DEFAULT_FILTER: &str = "sonde_pair=info,sonde_pair_ui=info";
#[cfg(not(debug_assertions))]
const DEFAULT_FILTER: &str = "sonde_pair=warn,sonde_pair_ui=warn";

tracing_subscriber::fmt()
    .with_env_filter(
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| DEFAULT_FILTER.into()),
    )
    .with_target(false)
    .init();
```

In debug builds the default is INFO; in release builds the default is WARN. `RUST_LOG` overrides the default in both cases (within compile-time limits — release builds strip DEBUG and TRACE call-sites).

For in-process log capture (e.g., displaying logs in the Tauri UI or capturing in tests), a `tracing_subscriber::fmt::Layer` writing to a ring buffer or channel can be composed alongside the stderr layer.

### 14.2  Log levels

| Level | Purpose | Example |
|---|---|---|
| `error!` | Unrecoverable failures | — (errors propagated via `Result`, not logged at `error!` by the library) |
| `warn!` | Recoverable issues requiring operator attention | Already-paired gateway overwrite (PT-0601) |
| `info!` | High-level milestones | Phase transitions, pairing complete, signature verified |
| `debug!` | Operational detail visible in verbose mode | Scan start/stop, device discovered, MTU negotiated, LESC method |
| `trace!` | Protocol-level detail for deep debugging | GATT writes, CBOR field counts, AES-GCM operations, CBOR encoding |

### 14.3  Structured fields

All log events use `tracing` structured fields so they can be machine-parsed:

- **Scan events** (PT-1207): `services` (service UUID filter), `name`, `address`, `rssi`, `evicted_count`
- **Connection events** (PT-1208): `address`, `mtu`
- **GATT events** (PT-1209): `msg`, `characteristic`, `len`
- **Phase events** (PT-1210): `phase`, `phone_key_hint`, `rf_channel`
- **LESC events** (PT-1211): `pairing_method`
- **Error context** (PT-1212): emitted as `debug!` log events with structured fields (e.g., `error_kind`, `phase`, `address`, `characteristic`) and mirrored in `PairingError` display

### 14.4  Security invariant

No log event at any level may include key material: PSKs, ephemeral private keys, shared secrets, AES keys, or raw decrypted payloads (PT-0900).  Key hints (`phone_key_hint`, `node_key_hint`) are safe to log because they are non-reversible 16-bit hashes.

---

## 15  Requirement traceability

| Section | Requirements covered |
|---|---|
| §2 Technology choices | PT-0100, PT-0101, PT-0103, PT-1100 |
| §3 Crate structure | PT-0102, PT-0103, PT-0104, PT-1004 |
| §4 Architecture | PT-0301, PT-0302, PT-0303, PT-0304, PT-0400–PT-0408, PT-0502, PT-0600, PT-0601, PT-1002 |
| §5 BLE transport | PT-0102, PT-0200–PT-0202, PT-0300, PT-0401, PT-1001, PT-1200 |
| §6 Cryptographic operations | PT-0301, PT-0304, PT-0402, PT-0404, PT-0405, PT-0408, PT-0900, PT-0901, PT-0902, PT-1100–PT-1103 |
| §7 Persistence | PT-0800–PT-0804 |
| §8 Error handling | PT-0500–PT-0502, PT-1000, PT-1003 |
| §9 Platform-specific | PT-0100, PT-0105, PT-0106, PT-0107, PT-0108, PT-0300, PT-0801, PT-0904 |
| §10 BLE message envelope | PT-0301, PT-0303, PT-0407 |
| §11 RNG provider | PT-0901, PT-0903 |
| §12 Input validation | PT-0403, PT-0406 |
| §13 Implementation order | PT-0700, PT-0701, PT-0702, PT-1200–PT-1206 |
| §14 Diagnostic logging | PT-0702, PT-0900, PT-1207–PT-1212 |
