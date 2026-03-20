<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Protocol Crate Validation Specification (`sonde-protocol`)

> **Document status:** Draft  
> **Scope:** Test plan for the `sonde-protocol` shared crate.  
> **Audience:** Implementers (human or LLM agent) writing protocol crate tests.  
> **Related:** [protocol-crate-design.md](protocol-crate-design.md), [protocol.md](protocol.md)

---

## 1  Overview

All tests in this document are pure Rust `#[test]` cases — no hardware, no async runtime, no mocks. The protocol crate is fully testable in isolation using a software `HmacProvider` and `Sha256Provider`. There are 60 test cases total.

### Test HMAC/SHA providers

```rust
struct SoftwareHmac;
impl HmacProvider for SoftwareHmac { /* RustCrypto hmac+sha2 */ }

struct SoftwareSha256;
impl Sha256Provider for SoftwareSha256 { /* RustCrypto sha2 */ }
```

---

## 2  Frame header tests

### T-P001  Header round-trip

**Procedure:**
1. Create `FrameHeader { key_hint: 0x1234, msg_type: 0x01, nonce: 0xDEADBEEFCAFEBABE }`.
2. Serialize to bytes.
3. Deserialize back.
4. Assert: all fields match the original.

---

### T-P002  Header byte layout

**Procedure:**
1. Create header with known values.
2. Serialize.
3. Assert: bytes[0..2] = key_hint big-endian.
4. Assert: bytes[2] = msg_type.
5. Assert: bytes[3..11] = nonce big-endian.
6. Assert: total length = 11.

---

### T-P003  Header zero values

**Procedure:**
1. Create header with all fields = 0.
2. Serialize, deserialize.
3. Assert: round-trip succeeds, all fields are 0.

---

### T-P004  Header max values

**Procedure:**
1. Create header with `key_hint = 0xFFFF`, `msg_type = 0xFF`, `nonce = u64::MAX`.
2. Serialize, deserialize.
3. Assert: round-trip succeeds.

---

## 3  Frame codec tests

### T-P010  Encode and decode round-trip

**Procedure:**
1. Create a header and CBOR payload.
2. Encode with `encode_frame()`.
3. Decode with `decode_frame()`.
4. Assert: header matches, payload matches, HMAC matches.

---

### T-P011  HMAC verification — valid

**Procedure:**
1. Encode a frame with PSK_A.
2. Decode and verify with PSK_A.
3. Assert: `verify_frame()` returns true.

---

### T-P012  HMAC verification — wrong key

**Procedure:**
1. Encode a frame with PSK_A.
2. Decode and verify with PSK_B.
3. Assert: `verify_frame()` returns false.

---

### T-P013  HMAC verification — tampered payload

**Procedure:**
1. Encode a frame.
2. Flip one bit in the payload portion of the raw bytes.
3. Decode and verify with the correct PSK.
4. Assert: `verify_frame()` returns false.

---

### T-P014  HMAC verification — tampered header

**Procedure:**
1. Encode a frame.
2. Flip one bit in the header portion (e.g., msg_type).
3. Decode and verify.
4. Assert: `verify_frame()` returns false.

---

### T-P015  HMAC verification — tampered HMAC

**Procedure:**
1. Encode a frame.
2. Flip one bit in the HMAC trailer.
3. Decode and verify.
4. Assert: `verify_frame()` returns false.

---

### T-P016  Frame too short

**Procedure:**
1. Call `decode_frame()` with 42 bytes (less than MIN_FRAME_SIZE).
2. Assert: `DecodeError::TooShort`.

---

### T-P017  Frame exactly minimum size

**Procedure:**
1. Encode a frame with empty payload.
2. Assert: total length = 43 (11 header + 0 payload + 32 HMAC).
3. Decode succeeds.

---

### T-P018  Frame too large

**Procedure:**
1. Call `encode_frame()` with a payload that would make the total exceed 250 bytes.
2. Assert: `EncodeError::FrameTooLarge`.

---

### T-P019  Frame exactly max size

**Procedure:**
1. Encode a frame with payload exactly 207 bytes.
2. Assert: total length = 250.
3. Decode succeeds.

---

### T-P019a  decode_frame with >250 raw bytes

**Procedure:**
1. Construct a 251-byte buffer.
2. Call `decode_frame()`.
3. Assert: returns `DecodeError::TooLong`.

---

### T-P019b  Invalid CBOR payload

**Procedure:**
1. Build a frame with valid header and HMAC but payload bytes `[0xFF, 0xFF]`.
2. Call message decode.
3. Assert: returns `DecodeError::CborError`.

---

### T-P019c  Type-mismatched CBOR field

**Procedure:**
1. Build CBOR where a field expected to be uint is instead a text string (e.g., set `KEY_BATTERY_MV` to `"hello"` in a Wake message).
2. Decode with `NodeMessage::decode()`.
3. Assert: returns `DecodeError::InvalidFieldType(KEY_BATTERY_MV)`.

---

## 4  Message encoding tests

### T-P020  Wake encode/decode round-trip

**Procedure:**
1. Create `NodeMessage::Wake { firmware_abi_version: 1, program_hash: vec![0xAA; 32], battery_mv: 3300 }`.
2. Encode to CBOR.
3. Decode with `msg_type = MSG_WAKE`.
4. Assert: all fields match.

---

### T-P021  Wake with empty program hash

**Procedure:**
1. Create Wake with `program_hash: vec![]` (no program installed).
2. Round-trip.
3. Assert: `program_hash` is empty.

---

### T-P022  Command NOP round-trip

**Procedure:**
1. Create `GatewayMessage::Command { command_type: CMD_NOP, starting_seq: 42, timestamp_ms: 1710000000000, payload: CommandPayload::Nop }`.
2. Round-trip.
3. Assert: all fields match.

---

### T-P023  Command UPDATE_PROGRAM round-trip

**Procedure:**
1. Create Command with `CommandPayload::UpdateProgram { program_hash, program_size: 4000, chunk_size: 190, chunk_count: 22 }`.
2. Round-trip.
3. Assert: all fields match.

---

### T-P024  Command UPDATE_SCHEDULE round-trip

**Procedure:**
1. Create Command with `CommandPayload::UpdateSchedule { interval_s: 300 }`.
2. Round-trip.
3. Assert: `interval_s = 300`.

---

### T-P025  GetChunk round-trip

**Procedure:**
1. Create `NodeMessage::GetChunk { chunk_index: 7 }`.
2. Round-trip.
3. Assert: `chunk_index = 7`.

---

### T-P026  Chunk round-trip

**Procedure:**
1. Create `GatewayMessage::Chunk { chunk_index: 7, chunk_data: vec![0x55; 190] }`.
2. Round-trip.
3. Assert: fields match, data length = 190.

---

### T-P027  AppData round-trip

**Procedure:**
1. Create `NodeMessage::AppData { blob: vec![1, 2, 3, 4, 5] }`.
2. Round-trip.
3. Assert: blob matches.

---

### T-P028  AppDataReply round-trip

**Procedure:**
1. Create `GatewayMessage::AppDataReply { blob: vec![0xAA, 0xBB] }`.
2. Round-trip.
3. Assert: blob matches.

---

### T-P029  Unknown CBOR keys ignored

**Procedure:**
1. Encode a Wake message.
2. Manually inject an extra CBOR key (e.g., key 99 with value "unknown").
3. Decode.
4. Assert: decoding succeeds, extra key is ignored.

---

### T-P030  Missing required field

**Procedure:**
1. Manually construct CBOR for a Wake with `battery_mv` omitted.
2. Decode.
3. Assert: `DecodeError::MissingField(KEY_BATTERY_MV)`.

---

### T-P031  Invalid msg_type

**Procedure:**
1. Call `NodeMessage::decode(0xFF, &valid_cbor)`.
2. Assert: `DecodeError::InvalidMsgType(0xFF)`.

---

### T-P032  CBOR integer keys used on wire

**Procedure:**
1. Encode a Wake message.
2. Manually inspect the CBOR bytes.
3. Assert: map keys are small positive integers (1, 2, 3), not text strings.

---

### T-P033  ProgramAck round-trip

**Procedure:**
1. Choose a fixed 32-byte test hash: `let program_hash = vec![0xABu8; 32];`.
2. Create `NodeMessage::ProgramAck { program_hash: program_hash.clone() }`.
3. Encode to CBOR.
4. Decode back with `msg_type = MSG_PROGRAM_ACK`.
5. Assert: decoded `program_hash` field exactly matches the original bytes.

---

### T-P034  Cmd(RunEphemeral) round-trip

**Procedure:**
1. Create `GatewayMessage::Command { starting_seq: 100, timestamp_ms: 1700000000, payload: CommandPayload::RunEphemeral { program_hash: vec![0xBBu8; 32], program_size: 4000, chunk_size: 190, chunk_count: 22 } }`.
2. Encode to CBOR.
3. Decode back with `msg_type = MSG_COMMAND`.
4. Assert: decoded payload variant is `RunEphemeral` and all fields (`program_hash`, `program_size`, `chunk_size`, `chunk_count`, `starting_seq`, `timestamp_ms`) match.

---

### T-P035  Cmd(Reboot) round-trip

**Procedure:**
1. Create `GatewayMessage::Command { starting_seq: 1, timestamp_ms: 1700000000, payload: CommandPayload::Reboot }`.
2. Encode to CBOR.
3. Inspect raw CBOR bytes: assert `KEY_COMMAND_TYPE` is present with value `0x04` and no `KEY_PAYLOAD` key exists.
4. Decode back with `msg_type = MSG_COMMAND`.
5. Assert: decoded payload variant is `Reboot`.

---

### T-P036  Missing-field detection for non-Wake types

**Procedure:**
1. For each of Command, GetChunk, Chunk, ProgramAck, AppData, AppDataReply: encode valid CBOR.
2. For each message type, remove one required field (e.g., remove `KEY_STARTING_SEQ` from Command, `KEY_PROGRAM_HASH` from ProgramAck, `KEY_BLOB` from AppData).
3. Decode each.
4. Assert: returns `DecodeError::MissingField(key)` where `key` matches the removed field's CBOR key constant.

---

### T-P037  Unknown CBOR keys ignored in non-Wake messages

**Procedure:**
1. For each of Command, GetChunk, Chunk, ProgramAck, AppData, AppDataReply: add an extra CBOR key (e.g., key 99) to valid encoded bytes.
2. Decode each.
3. Assert: decoding succeeds and the unknown key is silently ignored.

---

### T-P038  COMMAND nested payload CBOR byte inspection

**Procedure:**
1. Encode a `GatewayMessage::Command` with `CommandPayload::UpdateProgram`.
2. Inspect raw CBOR bytes.
3. Assert: the command envelope and payload are structured as nested maps (not flattened), matching the wire format in protocol.md §5.2.

---

### T-P039  Large u64 values round-trip

**Procedure:**
1. Encode a Wake with `battery_mv = u32::MAX`.
2. Encode a Command with `starting_seq = u64::MAX` and `timestamp_ms = u64::MAX`.
3. Decode both.
4. Assert: values round-trip without truncation.
5. Inspect CBOR bytes to confirm 8-byte integer encoding is used.

---

## 5  Program image tests

### T-P040  ProgramImage encode/decode round-trip

**Procedure:**
1. Create `ProgramImage { bytecode: vec![0x18, 0x01, ...], maps: vec![MapDef { map_type: 1, key_size: 4, value_size: 64, max_entries: 16 }] }`.
2. Encode with `encode_deterministic()`.
3. Decode.
4. Assert: all fields match.

---

### T-P041  ProgramImage empty maps

**Procedure:**
1. Create image with `maps: vec![]`.
2. Round-trip.
3. Assert: maps is empty.

---

### T-P042  ProgramImage deterministic encoding

**Procedure:**
1. Create the same ProgramImage twice (independent construction).
2. Encode both with `encode_deterministic()`.
3. Assert: byte-for-byte identical.

---

### T-P043  ProgramImage hash stability

**Procedure:**
1. Create a ProgramImage.
2. Encode and hash.
3. Repeat 100 times.
4. Assert: all hashes are identical.

---

### T-P044  Different maps produce different hashes

**Procedure:**
1. Create image A with `max_entries: 16`.
2. Create image B with identical bytecode but `max_entries: 32`.
3. Encode and hash both.
4. Assert: hashes differ.

---

### T-P045  Different bytecode produces different hashes

**Procedure:**
1. Create two images with different bytecode but same maps.
2. Hash both.
3. Assert: hashes differ.

---

### T-P046  ProgramImage deterministic encoding — key ordering

**Procedure:**
1. Encode a ProgramImage.
2. Inspect CBOR bytes.
3. Assert: map keys are in ascending numeric order (RFC 8949 §4.2 deterministic encoding).

---

### T-P047  ProgramImage with empty bytecode

**Procedure:**
1. Create `ProgramImage { bytecode: vec![], maps: vec![] }`.
2. Encode.
3. Assert: encoding succeeds.
4. Decode.
5. Assert: decoded `bytecode` is empty.

---

### T-P048  Deterministic CBOR minimal-length integer encoding

**Procedure:**
1. Encode a `ProgramImage` with a map having `max_entries = 23` (fits in 1-byte CBOR int) and another with `max_entries = 256` (requires 2-byte CBOR int).
2. Inspect raw bytes.
3. Assert: 23 is encoded as single byte `0x17`, 256 is encoded as `0x19 0x01 0x00` (minimal-length per RFC 8949 §4.2).

---

### T-P049  ProgramImage::decode() with malformed CBOR

**Procedure:**
1. Feed truncated CBOR bytes (e.g., first half of a valid encoding) to `ProgramImage::decode()`. Assert: returns an error (not panic).
2. Feed CBOR with missing `bytecode` field. Assert: returns an error.
3. Feed CBOR with `bytecode` as a text string instead of byte string. Assert: returns an error.

---

## 6  Chunking helper tests

### T-P050  chunk_count calculation

**Procedure:**
1. `chunk_count(4000, 190)` → assert `Some(22)`.
2. `chunk_count(190, 190)` → assert `Some(1)`.
3. `chunk_count(0, 190)` → assert `Some(0)`.
4. `chunk_count(1, 190)` → assert `Some(1)`.
5. `chunk_count(380, 190)` → assert `Some(2)` (exact multiple).
6. `chunk_count(100, 0)` → assert `None` (invalid chunk size).

---

### T-P051  get_chunk — valid indices

**Procedure:**
1. Create a 400-byte image, chunk_size = 190.
2. `get_chunk(image, 0, 190)` → first 190 bytes.
3. `get_chunk(image, 1, 190)` → next 190 bytes.
4. `get_chunk(image, 2, 190)` → last 20 bytes.

---

### T-P052  get_chunk — out of range

**Procedure:**
1. 400-byte image, chunk_size = 190.
2. `get_chunk(image, 3, 190)` → None.
3. `get_chunk(image, 100, 190)` → None.

---

### T-P053  Reassembled chunks match original

**Procedure:**
1. Create a program image, encode it.
2. Split into chunks using `get_chunk()`.
3. Reassemble by concatenating all chunks.
4. Assert: reassembled bytes == original CBOR image.
5. Hash reassembled bytes.
6. Assert: hash matches the original image hash.

---

### T-P054  get_chunk with chunk_size = 0

**Procedure:**
1. Call `get_chunk(data, 0, 0)` with non-empty data.
2. Assert: returns `None` (not an empty slice or panic).

---

### T-P055  chunk_count arithmetic overflow

**Procedure:**
1. Call `chunk_count(usize::MAX, 1)`. Assert: returns `None` because the required chunk count does not fit in `u32` (no panic).
2. Call `chunk_count(u32::MAX as usize, 1)`. Assert: returns `Some(u32::MAX)` (maximum chunk count that still fits in `u32`).
3. Call `chunk_count(usize::MAX, usize::MAX)`. Assert: returns `Some(1)`.

---

## 7  Full integration tests

### T-P060  Complete frame encode → verify → decode message

**Procedure:**
1. Create a `NodeMessage::Wake`.
2. Encode to CBOR.
3. Build `FrameHeader` with appropriate msg_type.
4. Call `encode_frame()` with PSK.
5. Call `decode_frame()` on the result.
6. Call `verify_frame()` with PSK → assert true.
7. Call `NodeMessage::decode()` on the payload → assert fields match.

---

### T-P061  Gateway Command full round-trip

**Procedure:**
1. Create a `GatewayMessage::Command` with `UpdateProgram` payload.
2. Encode CBOR, build frame, encode frame.
3. Decode frame, verify HMAC, decode message.
4. Assert: all fields match including `starting_seq`, `timestamp_ms`, `program_hash`, `chunk_size`, `chunk_count`.

---

### T-P062  Program image → chunk → reassemble → hash → decode

**Procedure:**
1. Create `ProgramImage` with bytecode and 3 maps.
2. Encode deterministically.
3. Compute hash.
4. Split into chunks (chunk_size = 190).
5. Reassemble from chunks.
6. Compute hash of reassembly.
7. Assert: hashes match.
8. Decode reassembled CBOR.
9. Assert: decoded `ProgramImage` matches original.

---

### T-P063  Direction-bit cross-direction rejection

**Procedure:**
1. Encode a `NodeMessage::Wake` to CBOR.
2. Pass the CBOR bytes and `msg_type = MSG_WAKE` (0x01, node→gateway range) to `GatewayMessage::decode()`.
3. Assert: returns an error (msg_type 0x01 is outside the gateway message range 0x80–0xFF).
4. Encode a `GatewayMessage::Command` to CBOR.
5. Pass the CBOR bytes and `msg_type = MSG_COMMAND` (0x80, gateway→node range) to `NodeMessage::decode()`.
6. Assert: returns an error (msg_type 0x80 is outside the node message range 0x01–0x7F).

---

### T-P064  Nonce echo verification in request-response pair

**Procedure:**
1. Build a `FrameHeader` with `nonce = 0x1234567890ABCDEF`. Encode a WAKE frame.
2. Call `decode_frame()` on the result. Assert: decoded header `nonce` is `0x1234567890ABCDEF`.
3. Build a COMMAND frame reusing the same `nonce` value. Decode it.
4. Assert: decoded header `nonce` matches `0x1234567890ABCDEF`.
5. Build a COMMAND frame with a different `nonce` (e.g., `0xFFFFFFFFFFFFFFFF`). Decode it.
6. Assert: decoded nonce is `0xFFFFFFFFFFFFFFFF` (different from the WAKE nonce — mismatch is detectable by comparing decoded header fields).

---

### T-P065  Multiple APP_DATA with incrementing sequences

**Procedure:**
1. Encode 3 `NodeMessage::AppData { blob: ... }` messages with distinct payloads.
2. Frame each with `encode_frame()` using `FrameHeader { nonce: 1, ... }`, `nonce: 2`, `nonce: 3` respectively.
3. Decode each frame with `decode_frame()`.
4. Assert: each decoded `FrameHeader.nonce` matches its expected sequence (1, 2, 3).
5. Assert: each decoded `AppData.blob` matches its original payload.

---

### T-P066  HMAC constant-time comparison structural test

**Procedure:**
1. Inspect `SoftwareHmac::verify()` implementation in the protocol crate.
2. Assert: the implementation delegates to `hmac::Mac::verify_slice()` (which uses constant-time comparison internally), or uses `subtle::ConstantTimeEq`.
3. Assert: the implementation does NOT use `==`, `PartialEq`, or `[u8]::eq()` to compare HMAC digests.
4. This may be implemented as a `#[test]` that calls `verify()` with a valid and an invalid tag and asserts correct accept/reject behavior, combined with a code-review checklist item verifying the constant-time property.
