<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Protocol Crate Design Specification (`sonde-protocol`)

> **Document status:** Draft  
> **Scope:** Architecture and API of the shared `sonde-protocol` Rust crate.  
> **Audience:** Implementers (human or LLM agent) building the protocol crate.  
> **Related:** [protocol.md](protocol.md), [gateway-design.md](gateway-design.md), [node-design.md](node-design.md)

---

## 1  Overview

`sonde-protocol` is a `no_std`-compatible Rust crate that encapsulates all wire-format logic for the Sonde protocol. It is the single source of truth for frame encoding, decoding, message types, and constants — shared by the gateway, node firmware, and test harnesses.

The crate has **no platform dependencies**. All platform-specific behavior (HMAC computation, transport I/O) is injected via traits.

---

## 2  Crate metadata

```toml
[package]
name = "sonde-protocol"
edition = "2021"

[features]
default = ["alloc"]
alloc = []       # enables Vec<u8> in message types
std = ["alloc"]  # enables std-dependent features (if any)

[dependencies]
ciborium = { version = "0.2", default-features = false, features = ["alloc"] }
```

The crate is `#![no_std]` by default, with `alloc` for heap types (`Vec<u8>`). Both the gateway (`std`) and node (ESP-IDF `std`) enable the `alloc` feature.

---

## 3  Constants

```rust
// Frame structure
pub const HEADER_SIZE: usize = 11;
pub const HMAC_SIZE: usize = 32;
pub const MIN_FRAME_SIZE: usize = HEADER_SIZE + HMAC_SIZE; // 43
pub const MAX_FRAME_SIZE: usize = 250;  // ESP-NOW reference
pub const MAX_PAYLOAD_SIZE: usize = MAX_FRAME_SIZE - HEADER_SIZE - HMAC_SIZE; // 207

// Header offsets
pub const OFFSET_KEY_HINT: usize = 0;
pub const OFFSET_MSG_TYPE: usize = 2;
pub const OFFSET_NONCE: usize = 3;

// msg_type codes (node → gateway)
pub const MSG_WAKE: u8 = 0x01;
pub const MSG_GET_CHUNK: u8 = 0x02;
pub const MSG_PROGRAM_ACK: u8 = 0x03;
pub const MSG_APP_DATA: u8 = 0x04;

// msg_type codes (gateway → node)
pub const MSG_COMMAND: u8 = 0x81;
pub const MSG_CHUNK: u8 = 0x82;
pub const MSG_APP_DATA_REPLY: u8 = 0x83;

// Command codes
pub const CMD_NOP: u8 = 0x00;
pub const CMD_UPDATE_PROGRAM: u8 = 0x01;
pub const CMD_RUN_EPHEMERAL: u8 = 0x02;
pub const CMD_UPDATE_SCHEDULE: u8 = 0x03;
pub const CMD_REBOOT: u8 = 0x04;

// CBOR integer keys (protocol messages)
pub const KEY_FIRMWARE_ABI_VERSION: u64 = 1;
pub const KEY_PROGRAM_HASH: u64 = 2;
pub const KEY_BATTERY_MV: u64 = 3;
pub const KEY_COMMAND_TYPE: u64 = 4;
pub const KEY_PAYLOAD: u64 = 5;
pub const KEY_PROGRAM_SIZE: u64 = 6;
pub const KEY_CHUNK_SIZE: u64 = 7;
pub const KEY_CHUNK_COUNT: u64 = 8;
pub const KEY_INTERVAL_S: u64 = 9;
pub const KEY_BLOB: u64 = 10;
pub const KEY_CHUNK_INDEX: u64 = 11;
pub const KEY_CHUNK_DATA: u64 = 12;
pub const KEY_STARTING_SEQ: u64 = 13;
pub const KEY_TIMESTAMP_MS: u64 = 14;

// CBOR integer keys (program image — separate keyspace)
pub const IMG_KEY_BYTECODE: u64 = 1;
pub const IMG_KEY_MAPS: u64 = 2;
pub const MAP_KEY_TYPE: u64 = 1;
pub const MAP_KEY_KEY_SIZE: u64 = 2;
pub const MAP_KEY_VALUE_SIZE: u64 = 3;
pub const MAP_KEY_MAX_ENTRIES: u64 = 4;
```

---

## 4  Frame header

### 4.1  Types

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct FrameHeader {
    pub key_hint: u16,
    pub msg_type: u8,
    pub nonce: u64,
}
```

### 4.2  Serialization

```rust
impl FrameHeader {
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] { ... }
    pub fn from_bytes(bytes: &[u8; HEADER_SIZE]) -> Self { ... }
}
```

All fields are big-endian. Parsing is at fixed offsets — no branching or variable-length decoding.

---

## 5  Frame codec

### 5.1  HMAC trait

```rust
pub trait HmacProvider {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32];
    
    /// Verify HMAC tag. Implementations MUST use constant-time comparison
    /// to prevent timing side-channel attacks.
    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool;
}
```

Implementations MUST use constant-time comparison to prevent timing side-channel attacks.

### 5.2  Encoding

```rust
pub fn encode_frame(
    header: &FrameHeader,
    payload_cbor: &[u8],
    psk: &[u8],
    hmac: &impl HmacProvider,
) -> Result<Vec<u8>, EncodeError>
```

1. Serialize header to 11 bytes.
2. Concatenate header + payload.
3. Compute HMAC over header + payload.
4. Return `header || payload || hmac` (total ≤ `MAX_FRAME_SIZE`).

Returns `EncodeError::FrameTooLarge` if the result exceeds `MAX_FRAME_SIZE`.

### 5.3  Decoding

```rust
#[derive(Debug)]
pub struct DecodedFrame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,  // raw CBOR bytes, not yet deserialized
    pub hmac: [u8; 32],
}

pub fn decode_frame(raw: &[u8]) -> Result<DecodedFrame, DecodeError>
```

1. Validate `raw.len() >= MIN_FRAME_SIZE`, otherwise return `DecodeError::TooShort`.
2. Validate `raw.len() <= MAX_FRAME_SIZE`, otherwise return `DecodeError::TooLong`.
3. Split into header (11), payload (middle), HMAC (last 32).
4. Parse header.
5. Return `DecodedFrame`. Payload is **not** CBOR-decoded — caller does that after HMAC verification.

### 5.4  HMAC verification helper

```rust
pub fn verify_frame(
    frame: &DecodedFrame,
    psk: &[u8],
    hmac: &impl HmacProvider,
) -> bool
```

Recomputes HMAC over the header + payload bytes and compares with `frame.hmac`.

### 5.5  `key_hint` derivation

```rust
/// Derive the 2-byte key hint from a PSK.
/// key_hint = u16::from_be_bytes(SHA-256(PSK)[30..32])
pub fn key_hint_from_psk(psk: &[u8; 32], sha: &impl Sha256Provider) -> u16 {
    let hash = sha.hash(psk);
    u16::from_be_bytes([hash[30], hash[31]])
}
```

This consolidates the `key_hint` derivation formula that the gateway and node otherwise implement independently. The derivation takes the **lower** 16 bits (bytes 30–31) of the SHA-256 hash of the PSK, interpreted as a big-endian `u16`. See protocol.md §3.1.1 for `key_hint` semantics.

---

## 6  Message types

### 6.1  Node → Gateway

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum NodeMessage {
    Wake {
        firmware_abi_version: u32,
        program_hash: Vec<u8>,
        battery_mv: u32,
    },
    GetChunk {
        chunk_index: u32,
    },
    ProgramAck {
        program_hash: Vec<u8>,
    },
    AppData {
        blob: Vec<u8>,
    },
}

impl NodeMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> { ... }
    pub fn decode(msg_type: u8, cbor: &[u8]) -> Result<Self, DecodeError> { ... }
}
```

### 6.2  Gateway → Node

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum CommandPayload {
    Nop,
    UpdateProgram {
        program_hash: Vec<u8>,
        program_size: u32,
        chunk_size: u32,
        chunk_count: u32,
    },
    RunEphemeral {
        program_hash: Vec<u8>,
        program_size: u32,
        chunk_size: u32,
        chunk_count: u32,
    },
    UpdateSchedule {
        interval_s: u32,
    },
    Reboot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GatewayMessage {
    Command {
        command_type: u8,
        starting_seq: u64,
        timestamp_ms: u64,
        payload: CommandPayload,
    },
    Chunk {
        chunk_index: u32,
        chunk_data: Vec<u8>,
    },
    AppDataReply {
        blob: Vec<u8>,
    },
}

impl GatewayMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> { ... }
    pub fn decode(msg_type: u8, cbor: &[u8]) -> Result<Self, DecodeError> { ... }
}
```

### 6.3  `command_type` / `CommandPayload` consistency invariant

The `command_type` field (CBOR key 4) in the COMMAND payload is the authoritative wire-format discriminator. It MUST match the `CommandPayload` variant in `GatewayMessage::Command`:

| `command_type` | `CommandPayload` variant |
|---|---|
| `CMD_NOP` (0x00) | `Nop` |
| `CMD_UPDATE_PROGRAM` (0x01) | `UpdateProgram { .. }` |
| `CMD_RUN_EPHEMERAL` (0x02) | `RunEphemeral { .. }` |
| `CMD_UPDATE_SCHEDULE` (0x03) | `UpdateSchedule { .. }` |
| `CMD_REBOOT` (0x04) | `Reboot` |

- **`encode()`** derives `command_type` from the `CommandPayload` variant — callers never set it manually.
- **`decode()`** reads `command_type` from the CBOR map, selects the corresponding `CommandPayload` variant, and validates that the nested `payload` (key 5) structure is consistent (e.g., `CMD_NOP` and `CMD_REBOOT` must not contain key 5; `CMD_UPDATE_PROGRAM` must contain key 5 with the required sub-fields).

Because `command_type` is fully determined by the `CommandPayload` variant, the public `GatewayMessage::Command` API is defined in terms of the payload only; implementations may cache the derived `command_type` internally for pattern matching and logging, but callers do not read or write it directly.

### 6.4  CBOR encoding rules

- All payloads are CBOR maps with integer keys (§3 constants).
- Unknown keys in inbound messages are ignored (forward compatibility).
- Missing required keys produce `DecodeError::MissingField`.

---

## 7  Program image

### 7.1  Types

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct MapDef {
    pub map_type: u32,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgramImage {
    pub bytecode: Vec<u8>,
    pub maps: Vec<MapDef>,
}
```

### 7.2  Encoding

```rust
impl ProgramImage {
    pub fn encode_deterministic(&self) -> Vec<u8> { ... }
}
```

Uses CBOR deterministic encoding (RFC 8949 §4.2): integer keys sorted in canonical order, minimal-length integer encoding. This ensures identical programs produce identical bytes on any platform.

### 7.3  Decoding

```rust
impl ProgramImage {
    pub fn decode(cbor: &[u8]) -> Result<Self, DecodeError> { ... }
}
```

### 7.4  Hashing

```rust
pub fn program_hash(image_cbor: &[u8]) -> [u8; 32] {
    // SHA-256 of the raw CBOR bytes
}
```

The crate provides this as a convenience function. The SHA-256 implementation is **not** included in the crate (to avoid pulling in a crypto dependency for all platforms). The platform provides a `Sha256Provider`:

```rust
pub trait Sha256Provider {
    fn hash(&self, data: &[u8]) -> [u8; 32];
}

pub fn program_hash(image_cbor: &[u8], sha: &impl Sha256Provider) -> [u8; 32] {
    sha.hash(image_cbor)
}
```

Gateway uses RustCrypto `sha2`; node uses ESP-IDF hardware SHA.

---

## 8  Error types

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum EncodeError {
    FrameTooLarge,
    CborError(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    TooShort,
    TooLong,
    InvalidMsgType(u8),
    MissingField(u64),     // CBOR key that was expected
    InvalidFieldType(u64), // CBOR key with wrong type
    CborError(String),
}
```

---

## 9  Chunking helpers

```rust
pub fn chunk_count(image_size: usize, chunk_size: usize) -> Option<u32> {
    if image_size == 0 {
        return Some(0);
    }
    if chunk_size == 0 {
        return None;
    }
    Some(((image_size + chunk_size - 1) / chunk_size) as u32)
}

pub fn get_chunk(image: &[u8], chunk_index: u32, chunk_size: u32) -> Option<&[u8]> {
    let start = (chunk_index as usize) * (chunk_size as usize);
    if start >= image.len() { return None; }
    let end = core::cmp::min(start + chunk_size as usize, image.len());
    Some(&image[start..end])
}
```

These are pure functions — no state, no allocation. Used by the gateway for serving and by tests for verification.

---

## 10  Modem serial codec (`modem.rs`)

Implements the length-prefixed framing protocol between the gateway and a USB-attached ESP-NOW radio modem, as defined in `modem-protocol.md`. The module is `no_std`-compatible and shared between `sonde-gateway` and `sonde-modem` to guarantee wire-format compatibility.

**Public API:**

- Constants: `SERIAL_LEN_SIZE`, `SERIAL_MAX_LEN`, `SERIAL_MAX_FRAME_SIZE`, `MAC_SIZE`, and message-type constants (`MODEM_MSG_*`).
- `ModemFrame` — typed enum covering all gateway↔modem serial messages.
- `ModemFrame::encode() -> Vec<u8>` / `ModemFrame::decode(&[u8]) -> Result` — encode and decode individual frames.
- `SerialDecoder` — streaming decoder that buffers partial reads and yields complete `ModemFrame`s via `feed(&[u8])`.

---

## 11  BLE envelope codec (`ble_envelope.rs`)

Implements a minimal Type-Length-Value envelope used for BLE GATT messages in the pairing protocol (see `ble-pairing-protocol.md` §4).

**Wire format:** `TYPE (1 byte) | LEN (2 bytes, big-endian) | BODY (LEN bytes)`.

**Public API:**

- `parse_ble_envelope(&[u8]) -> Option<(u8, &[u8])>` — parse a complete envelope, returning `(msg_type, body)`. Rejects truncated or trailing-byte inputs.
- `encode_ble_envelope(msg_type: u8, body: &[u8]) -> Option<Vec<u8>>` — encode a BLE envelope. Returns `None` if `body` exceeds `u16::MAX`.
