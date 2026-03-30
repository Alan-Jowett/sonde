<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Protocol Crate Validation Specification (`sonde-protocol`)

> **Document status:** Draft  
> **Scope:** Test plan for the `sonde-protocol` shared crate.  
> **Audience:** Implementers (human or LLM agent) writing protocol crate tests.  
> **Related:** [protocol-crate-design.md](protocol-crate-design.md), [protocol.md](protocol.md)

---

## 1  Overview

All tests in this document are pure Rust `#[test]` cases — no hardware, no async runtime, no mocks. The protocol crate is fully testable in isolation using a software `HmacProvider` and `Sha256Provider`. There are 59 test cases total.

### Traceability note

The protocol specification (`protocol.md`) uses prose-based assertions without formal requirement IDs (e.g., `[PR-NNNN]`). Test cases in this document reference `protocol.md` section numbers for traceability (e.g., `**Validates:** protocol.md §3.1`). A future pass should add formal requirement identifiers to `protocol.md` to enable precise requirement-to-test mapping.

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

**Validates:** protocol.md §3.1 (Header fields — fixed binary layout)

**Procedure:**
1. Create `FrameHeader { key_hint: 0x1234, msg_type: 0x01, nonce: 0xDEADBEEFCAFEBABE }`.
2. Serialize to bytes.
3. Deserialize back.
4. Assert: all fields match the original.

---

### T-P002  Header byte layout

**Validates:** protocol.md §3.1 (Header encoding — fixed byte offsets, big-endian)

**Procedure:**
1. Create header with known values.
2. Serialize.
3. Assert: bytes[0..2] = key_hint big-endian.
4. Assert: bytes[2] = msg_type.
5. Assert: bytes[3..11] = nonce big-endian.
6. Assert: total length = 11.

---

### T-P003  Header zero values

**Validates:** protocol.md §3.1 (Header fields — boundary values)

**Procedure:**
1. Create header with all fields = 0.
2. Serialize, deserialize.
3. Assert: round-trip succeeds, all fields are 0.

---

### T-P004  Header max values

**Validates:** protocol.md §3.1 (Header fields — boundary values)

**Procedure:**
1. Create header with `key_hint = 0xFFFF`, `msg_type = 0xFF`, `nonce = u64::MAX`.
2. Serialize, deserialize.
3. Assert: round-trip succeeds.

---

## 3  Frame codec tests

### T-P010  Encode and decode round-trip

**Validates:** protocol.md §3 (Frame format — header ∥ payload ∥ HMAC layout)

**Procedure:**
1. Create a header and CBOR payload.
2. Encode with `encode_frame()`.
3. Decode with `decode_frame()`.
4. Assert: header matches, payload matches, HMAC matches.

---

### T-P011  HMAC verification — valid

**Validates:** protocol.md §7.1 (HMAC computation)

**Procedure:**
1. Encode a frame with PSK_A.
2. Decode and verify with PSK_A.
3. Assert: `verify_frame()` returns true.

---

### T-P012  HMAC verification — wrong key

**Validates:** protocol.md §7.1 (HMAC computation — key mismatch)

**Procedure:**
1. Encode a frame with PSK_A.
2. Decode and verify with PSK_B.
3. Assert: `verify_frame()` returns false.

---

### T-P013  HMAC verification — tampered payload

**Validates:** protocol.md §3.2 (HMAC covers header + payload), §7.1

**Procedure:**
1. Encode a frame.
2. Flip one bit in the payload portion of the raw bytes.
3. Decode and verify with the correct PSK.
4. Assert: `verify_frame()` returns false.

---

### T-P014  HMAC verification — tampered header

**Validates:** protocol.md §3.2 (HMAC covers header + payload), §7.1

**Procedure:**
1. Encode a frame.
2. Flip one bit in the header portion (e.g., msg_type).
3. Decode and verify.
4. Assert: `verify_frame()` returns false.

---

### T-P015  HMAC verification — tampered HMAC

**Validates:** protocol.md §7.1 (HMAC computation — tag integrity)

**Procedure:**
1. Encode a frame.
2. Flip one bit in the HMAC trailer.
3. Decode and verify.
4. Assert: `verify_frame()` returns false.

---

### T-P016  Frame too short

**Validates:** protocol.md §3.3 (Frame size budget — minimum frame size)

**Procedure:**
1. Call `decode_frame()` with 42 bytes (less than MIN_FRAME_SIZE).
2. Assert: `DecodeError::TooShort`.

---

### T-P017  Frame exactly minimum size

**Validates:** protocol.md §3.3 (Frame size budget — minimum frame size)

**Procedure:**
1. Encode a frame with empty payload.
2. Assert: total length = 43 (11 header + 0 payload + 32 HMAC).
3. Decode succeeds.

---

### T-P018  Frame too large

**Validates:** protocol.md §3.3 (Frame size budget — 250-byte maximum)

**Procedure:**
1. Call `encode_frame()` with a payload that would make the total exceed 250 bytes.
2. Assert: `EncodeError::FrameTooLarge`.

---

### T-P019  Frame exactly max size

**Validates:** protocol.md §3.3 (Frame size budget — 250-byte maximum, 207-byte payload)

**Procedure:**
1. Encode a frame with payload exactly 207 bytes.
2. Assert: total length = 250.
3. Decode succeeds.

---

### T-P019a  decode_frame with >250 raw bytes

**Validates:** protocol.md §3.3 (Frame size budget — 250-byte maximum)

**Procedure:**
1. Construct a 251-byte buffer.
2. Call `decode_frame()`.
3. Assert: returns `DecodeError::TooLong`.

---

### T-P019b  Invalid CBOR payload

**Validates:** protocol.md §8 (Error handling — malformed CBOR)

**Procedure:**
1. Construct an invalid CBOR payload (e.g., raw bytes `[0xFF, 0xFF]`).
2. Build a frame for a WAKE message (`msg_type = MSG_WAKE`) with a valid header and a valid HMAC computed over the header + these invalid CBOR bytes, so that `decode_frame()` succeeds and yields `MSG_WAKE` and the payload unchanged.
3. Call `NodeMessage::decode(MSG_WAKE, &payload)`.
4. Assert: returns `DecodeError::CborError`.

---

### T-P019c  Type-mismatched CBOR field

**Validates:** protocol.md §8 (Error handling — malformed CBOR)

**Procedure:**
1. Build CBOR where a field expected to be uint is instead a text string (e.g., set `KEY_BATTERY_MV` to `"hello"` in a Wake message).
2. Decode with `NodeMessage::decode()`.
3. Assert: returns `DecodeError::InvalidFieldType(KEY_BATTERY_MV)`.

---

## 4  Message encoding tests

### T-P020  Wake encode/decode round-trip

**Validates:** protocol.md §5.1 (WAKE message fields)

**Procedure:**
1. Create `NodeMessage::Wake { firmware_abi_version: 1, program_hash: vec![0xAA; 32], battery_mv: 3300 }`.
2. Encode to CBOR.
3. Decode with `msg_type = MSG_WAKE`.
4. Assert: all fields match.

---

### T-P021  Wake with empty program hash

**Validates:** protocol.md §5.1 (WAKE — zero-length `program_hash` when no program installed)

**Procedure:**
1. Create Wake with `program_hash: vec![]` (no program installed).
2. Round-trip.
3. Assert: `program_hash` is empty.

---

### T-P022  Command NOP round-trip

**Validates:** protocol.md §5.2 (COMMAND — NOP command type)

**Procedure:**
1. Create a COMMAND with `CommandPayload::Nop`, `starting_seq: 42`, `timestamp_ms: 1710000000000`.
2. Round-trip.
3. Assert: all fields match.

---

### T-P023  Command UPDATE_PROGRAM round-trip

**Validates:** protocol.md §5.2.1 (UPDATE_PROGRAM payload fields)

**Procedure:**
1. Create Command with `CommandPayload::UpdateProgram { program_hash, program_size: 4000, chunk_size: 190, chunk_count: 22 }`.
2. Round-trip.
3. Assert: all fields match.

---

### T-P024  Command UPDATE_SCHEDULE round-trip

**Validates:** protocol.md §5.2.2 (UPDATE_SCHEDULE payload fields)

**Procedure:**
1. Create Command with `CommandPayload::UpdateSchedule { interval_s: 300 }`.
2. Round-trip.
3. Assert: `interval_s = 300`.

---

### T-P025  GetChunk round-trip

**Validates:** protocol.md §5.3 (GET_CHUNK message fields)

**Procedure:**
1. Create `NodeMessage::GetChunk { chunk_index: 7 }`.
2. Round-trip.
3. Assert: `chunk_index = 7`.

---

### T-P026  Chunk round-trip

**Validates:** protocol.md §5.4 (CHUNK message fields)

**Procedure:**
1. Create `GatewayMessage::Chunk { chunk_index: 7, chunk_data: vec![0x55; 190] }`.
2. Round-trip.
3. Assert: fields match, data length = 190.

---

### T-P027  AppData round-trip

**Validates:** protocol.md §5.6 (APP_DATA message fields)

**Procedure:**
1. Create `NodeMessage::AppData { blob: vec![1, 2, 3, 4, 5] }`.
2. Round-trip.
3. Assert: blob matches.

---

### T-P028  AppDataReply round-trip

**Validates:** protocol.md §5.7 (APP_DATA_REPLY message fields)

**Procedure:**
1. Create `GatewayMessage::AppDataReply { blob: vec![0xAA, 0xBB] }`.
2. Round-trip.
3. Assert: blob matches.

---

### T-P029  Unknown CBOR keys ignored

**Validates:** protocol.md §5 (CBOR key mapping — forward compatibility)

**Procedure:**
1. Encode a Wake message.
2. Manually inject an extra CBOR key (e.g., key 99 with value "unknown").
3. Decode.
4. Assert: decoding succeeds, extra key is ignored.

---

### T-P030  Missing required field

**Validates:** protocol.md §5.1 (WAKE — required fields)

**Procedure:**
1. Manually construct CBOR for a Wake with `battery_mv` omitted.
2. Decode.
3. Assert: `DecodeError::MissingField(KEY_BATTERY_MV)`.

---

### T-P031  Invalid msg_type

**Validates:** protocol.md §4 (Message types — direction-bit discriminator)

**Procedure:**
1. Call `NodeMessage::decode(0xFF, &valid_cbor)`.
2. Assert: `DecodeError::InvalidMsgType(0xFF)`.

---

### T-P032  CBOR integer keys used on wire

**Validates:** protocol.md §5 (CBOR key mapping — integer keys for compactness)

**Procedure:**
1. Encode a Wake message.
2. Manually inspect the CBOR bytes.
3. Assert: map keys are small positive integers (1, 2, 3), not text strings.

---

### T-P033  ProgramAck round-trip

**Validates:** protocol.md §5.5 (PROGRAM_ACK message fields)

**Procedure:**
1. Choose a fixed 32-byte test hash: `let program_hash = vec![0xABu8; 32];`.
2. Create `NodeMessage::ProgramAck { program_hash: program_hash.clone() }`.
3. Encode to CBOR.
4. Decode back with `msg_type = MSG_PROGRAM_ACK`.
5. Assert: decoded `program_hash` field exactly matches the original bytes.

---

### T-P034  Cmd(RunEphemeral) round-trip

**Validates:** protocol.md §5.2.1 (RUN_EPHEMERAL payload fields)

**Procedure:**
1. Create `GatewayMessage::Command { starting_seq: 100, timestamp_ms: 1_710_000_000_000, payload: CommandPayload::RunEphemeral { program_hash: vec![0xBBu8; 32], program_size: 4000, chunk_size: 190, chunk_count: 22 } }`.
2. Encode to CBOR.
3. Decode back with `msg_type = MSG_COMMAND`.
4. Assert: decoded payload variant is `RunEphemeral` and all fields (`program_hash`, `program_size`, `chunk_size`, `chunk_count`, `starting_seq`, `timestamp_ms`) match.

---

### T-P035  Cmd(Reboot) round-trip

**Validates:** protocol.md §5.2 (COMMAND — REBOOT command type, key 5 omitted)

**Procedure:**
1. Create `GatewayMessage::Command { starting_seq: 1, timestamp_ms: 1_710_000_000_000, payload: CommandPayload::Reboot }`.
2. Encode to CBOR.
3. Inspect raw CBOR bytes: assert `KEY_COMMAND_TYPE` is present with value `0x04` and no `KEY_PAYLOAD` key exists.
4. Decode back with `msg_type = MSG_COMMAND`.
5. Assert: decoded payload variant is `Reboot`.

---

### T-P036  Missing-field detection for non-Wake types

**Validates:** protocol.md §5.2–§5.7 (Required fields across all message types)

**Procedure:**
1. For each of Command, GetChunk, Chunk, ProgramAck, AppData, AppDataReply: encode valid CBOR.
2. For each message type, remove one required field (e.g., remove `KEY_STARTING_SEQ` from Command, `KEY_PROGRAM_HASH` from ProgramAck, `KEY_BLOB` from AppData).
3. Decode each.
4. Assert: returns `DecodeError::MissingField(key)` where `key` matches the removed field's CBOR key constant.

---

### T-P037  Unknown CBOR keys ignored in non-Wake messages

**Validates:** protocol.md §5 (CBOR key mapping — forward compatibility across all message types)

**Procedure:**
1. For each of Command, GetChunk, Chunk, ProgramAck, AppData, AppDataReply: add an extra CBOR key (e.g., key 99) to valid encoded bytes.
2. Decode each.
3. Assert: decoding succeeds and the unknown key is silently ignored.

---

### T-P038  COMMAND nested payload CBOR byte inspection

**Validates:** protocol.md §5.2 (COMMAND structure — nested payload map)

**Procedure:**
1. Encode a `GatewayMessage::Command` with `CommandPayload::UpdateProgram`.
2. Inspect raw CBOR bytes.
3. Assert: the top-level CBOR map contains keys {4, 5, 13, 14} (`command_type`, `payload`, `starting_seq`, `timestamp_ms`).
4. Assert: key 5 (`payload`) contains a nested CBOR map with the `UpdateProgram` sub-fields (keys 2, 6, 7, 8).
5. Encode a `GatewayMessage::Command` with `CommandPayload::Nop`.
6. Inspect raw CBOR bytes.
7. Assert: the top-level CBOR map contains keys {4, 13, 14} only — key 5 (`payload`) is absent.
8. Encode a `GatewayMessage::Command` with `CommandPayload::Reboot`.
9. Inspect raw CBOR bytes.
10. Assert: the top-level CBOR map contains keys {4, 13, 14} only — key 5 (`payload`) is absent.

---

### T-P039  Large u64 values round-trip

**Validates:** protocol.md §5 (CBOR encoding — integer types)

**Procedure:**
1. Encode a Wake with `battery_mv = u32::MAX`.
2. Encode a Command with `starting_seq = u64::MAX` and `timestamp_ms = u64::MAX`.
3. Decode both.
4. Assert: values round-trip without truncation.
5. Inspect CBOR bytes and assert:
   - `battery_mv` (`u32::MAX`) is encoded as a 4-byte unsigned integer (major type 0, additional info 26).
   - `starting_seq` and `timestamp_ms` (`u64::MAX`) are encoded as 8-byte unsigned integers (major type 0, additional info 27).

---

## 5  Program image tests

### T-P040  ProgramImage encode/decode round-trip

**Validates:** protocol.md §5 (Program image format — CBOR structure)

**Procedure:**
1. Create `ProgramImage { bytecode: vec![0x18, 0x01, ...], maps: vec![MapDef { map_type: 1, key_size: 4, value_size: 64, max_entries: 16 }] }`.
2. Encode with `encode_deterministic()`.
3. Decode.
4. Assert: all fields match.

---

### T-P041  ProgramImage empty maps

**Validates:** protocol.md §5 (Program image format — empty maps array)

**Procedure:**
1. Create image with `maps: vec![]`.
2. Round-trip.
3. Assert: maps is empty.

---

### T-P042  ProgramImage deterministic encoding

**Validates:** protocol.md §5 (Program image format — deterministic encoding per RFC 8949 §4.2)

**Procedure:**
1. Create the same ProgramImage twice (independent construction).
2. Encode both with `encode_deterministic()`.
3. Assert: byte-for-byte identical.

---

### T-P043  ProgramImage hash stability

**Validates:** protocol.md §5 (Program image format — `program_hash` = SHA-256 of CBOR image)

**Procedure:**
1. Create a ProgramImage.
2. Encode and hash.
3. Repeat 100 times.
4. Assert: all hashes are identical.

---

### T-P044  Different maps produce different hashes

**Validates:** protocol.md §5 (Program image format — hash covers both bytecode and map definitions)

**Procedure:**
1. Create image A with `max_entries: 16`.
2. Create image B with identical bytecode but `max_entries: 32`.
3. Encode and hash both.
4. Assert: hashes differ.

---

### T-P045  Different bytecode produces different hashes

**Validates:** protocol.md §5 (Program image format — hash covers both bytecode and map definitions)

**Procedure:**
1. Create two images with different bytecode but same maps.
2. Hash both.
3. Assert: hashes differ.

---

### T-P046  ProgramImage deterministic encoding — key ordering

**Validates:** protocol.md §5 (Program image format — deterministic encoding, RFC 8949 §4.2 key ordering)

**Procedure:**
1. Encode a ProgramImage.
2. Inspect CBOR bytes.
3. Assert: map keys are in ascending numeric order (RFC 8949 §4.2 deterministic encoding).

---

### T-P047  ProgramImage with empty bytecode

**Validates:** protocol.md §5 (Program image format — boundary: empty bytecode)

**Procedure:**
1. Create `ProgramImage { bytecode: vec![], maps: vec![] }`.
2. Encode.
3. Assert: encoding succeeds.
4. Decode.
5. Assert: decoded `bytecode` is empty.

---

### T-P048  Deterministic CBOR minimal-length integer encoding

**Validates:** protocol.md §5 (Program image format — deterministic encoding, RFC 8949 §4.2 minimal-length integers)

**Procedure:**
1. Encode a `ProgramImage` with a map having `max_entries = 23` (fits in 1-byte CBOR int) and another with `max_entries = 256` (requires 2-byte CBOR int).
2. Inspect raw bytes.
3. Assert: 23 is encoded as single byte `0x17`, 256 is encoded as `0x19 0x01 0x00` (minimal-length per RFC 8949 §4.2).

---

### T-P049  ProgramImage::decode() with malformed CBOR

**Validates:** protocol.md §8 (Error handling — malformed CBOR)

**Procedure:**
1. Feed truncated CBOR bytes (e.g., first half of a valid encoding) to `ProgramImage::decode()`. Assert: returns an error (not panic).
2. Feed CBOR with missing `bytecode` field. Assert: returns an error.
3. Feed CBOR with `bytecode` as a text string instead of byte string. Assert: returns an error.

---

## 6  Chunking helper tests

### T-P050  chunk_count calculation

**Validates:** protocol.md §5.2.1 (UPDATE_PROGRAM — `chunk_count` derivation from `program_size` and `chunk_size`)

**Procedure:**
1. `chunk_count(4000, 190)` → assert `Some(22)`.
2. `chunk_count(190, 190)` → assert `Some(1)`.
3. `chunk_count(0, 190)` → assert `Some(0)`.
4. `chunk_count(1, 190)` → assert `Some(1)`.
5. `chunk_count(380, 190)` → assert `Some(2)` (exact multiple).
6. `chunk_count(100, 0)` → assert `None` (invalid chunk size).

---

### T-P051  get_chunk — valid indices

**Validates:** protocol.md §5.4 (CHUNK — chunked transfer data retrieval)

**Procedure:**
1. Create a 400-byte image, chunk_size = 190.
2. `get_chunk(image, 0, 190)` → first 190 bytes.
3. `get_chunk(image, 1, 190)` → next 190 bytes.
4. `get_chunk(image, 2, 190)` → last 20 bytes.

---

### T-P052  get_chunk — out of range

**Validates:** protocol.md §8 (Error handling — `chunk_index` out of range)

**Procedure:**
1. 400-byte image, chunk_size = 190.
2. `get_chunk(image, 3, 190)` → None.
3. `get_chunk(image, 100, 190)` → None.

---

### T-P053  Reassembled chunks match original

**Validates:** protocol.md §6.2 (Program update — chunked transfer reassembly and hash verification)

**Procedure:**
1. Create a program image, encode it.
2. Split into chunks using `get_chunk()`.
3. Reassemble by concatenating all chunks.
4. Assert: reassembled bytes == original CBOR image.
5. Hash reassembled bytes.
6. Assert: hash matches the original image hash.

---

### T-P054  get_chunk with chunk_size = 0

**Validates:** protocol.md §5.2.1 (UPDATE_PROGRAM — `chunk_size` boundary)

**Procedure:**
1. Call `get_chunk(data, 0, 0)` with non-empty data.
2. Assert: returns `None` (not an empty slice or panic).

---

### T-P055  chunk_count arithmetic overflow

**Validates:** protocol.md §5.2.1 (UPDATE_PROGRAM — `chunk_count` arithmetic safety)

**Procedure:**
1. Call `chunk_count(usize::MAX, 1)`. Assert: returns `None` because the required chunk count does not fit in `u32` (no panic).
2. Call `chunk_count(u32::MAX as usize, 1)`. Assert: returns `Some(u32::MAX)` (maximum chunk count that still fits in `u32`).
3. Call `chunk_count(usize::MAX, usize::MAX)`. Assert: returns `Some(1)`.

---

## 7  Full integration tests

### T-P060  Complete frame encode → verify → decode message

**Validates:** protocol.md §3 (Frame format), §7.1 (HMAC computation), §5.1 (WAKE)

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

**Validates:** protocol.md §5.2 (COMMAND), §5.2.1 (UPDATE_PROGRAM payload), §7.1 (HMAC)

**Procedure:**
1. Create a `GatewayMessage::Command` with `UpdateProgram` payload.
2. Encode CBOR, build frame, encode frame.
3. Decode frame, verify HMAC, decode message.
4. Assert: all fields match including `starting_seq`, `timestamp_ms`, `program_hash`, `chunk_size`, `chunk_count`.

---

### T-P062  Program image → chunk → reassemble → hash → decode

**Validates:** protocol.md §5 (Program image format), §6.2 (Program update — chunked transfer)

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

**Validates:** protocol.md §4 (Message types — direction bit 0x01–0x7F vs 0x80–0xFF)

**Procedure:**
1. Encode a `NodeMessage::Wake` to CBOR.
2. Pass the CBOR bytes and `msg_type = MSG_WAKE` (0x01, node→gateway range) to `GatewayMessage::decode()`.
3. Assert: returns an error (msg_type 0x01 is outside the gateway message range 0x80–0xFF).
4. Encode a `GatewayMessage::Command` to CBOR.
5. Pass the CBOR bytes and `msg_type = MSG_COMMAND` (0x81, gateway→node range) to `NodeMessage::decode()`.
6. Assert: returns an error (msg_type 0x81 is outside the node message range 0x01–0x7F).

---

### T-P064  Nonce echo verification in request-response pair

**Validates:** protocol.md §7.3 (Verification procedure — nonce echo matching)

**Procedure:**
1. Build a `FrameHeader` with `nonce = 0x1234567890ABCDEF`. Encode a WAKE frame.
2. Call `decode_frame()` on the result. Assert: decoded header `nonce` is `0x1234567890ABCDEF`.
3. Build a COMMAND frame reusing the same `nonce` value. Decode it.
4. Assert: decoded header `nonce` matches `0x1234567890ABCDEF`.
5. Build a COMMAND frame with a different `nonce` (e.g., `0xFFFFFFFFFFFFFFFF`). Decode it.
6. Assert: decoded nonce is `0xFFFFFFFFFFFFFFFF` (different from the WAKE nonce — mismatch is detectable by comparing decoded header fields).

---

### T-P065  Multiple APP_DATA with incrementing sequences

**Validates:** protocol.md §5.6 (APP_DATA — incrementing sequence numbers), §7.4 (Replay protection)

**Procedure:**
1. Encode 3 `NodeMessage::AppData { blob: ... }` messages with distinct payloads.
2. Frame each with `encode_frame()` using `FrameHeader { nonce: 1, ... }`, `nonce: 2`, `nonce: 3` respectively.
3. Decode each frame with `decode_frame()`.
4. Assert: each decoded `FrameHeader.nonce` matches its expected sequence (1, 2, 3).
5. Assert: each decoded `AppData.blob` matches its original payload.

---

### T-P066  HMAC constant-time comparison behavior

**Validates:** protocol.md §7.1 (HMAC computation — constant-time comparison)

**Procedure:**
1. Construct a message and compute its HMAC tag using `SoftwareHmac`.
2. Call `SoftwareHmac::verify()` with the correct tag and assert that verification **succeeds**.
3. Call `SoftwareHmac::verify()` with an incorrect tag (e.g., flip one bit in the tag) and assert that verification **fails**.

**Implementation requirement (non-test):** `SoftwareHmac::verify()` must internally use a constant-time comparison primitive (e.g., delegate to `hmac::Mac::verify_slice()` or `subtle::ConstantTimeEq`) and must not compare HMAC digests using `==`, `PartialEq`, or `[u8]::eq()`. This requirement is enforced via code review, not automated tests.

---

### T-P070  ProgramImage initial data round-trip

**Validates:** protocol.md §6 (Program image format — key 5 `initial_data`)

**Procedure:**
1. Create a `ProgramImage` with one map definition and `map_initial_data[0]` set to non-empty bytes (e.g., `[0xDE, 0xAD, 0xBE, 0xEF]`) whose length equals `value_size`.
2. Encode with `encode_deterministic()`.
3. Decode with `ProgramImage::decode()`.
4. Assert: `decoded.map_initial_data[0]` equals the original bytes.
5. Assert: the CBOR contains key 5 in the map definition entry.

---

### T-P071  ProgramImage initial data absent when empty

**Validates:** protocol.md §6 (Program image format — key 5 `initial_data` omission)

**Procedure:**
1. Create a `ProgramImage` with one map definition and `map_initial_data[0]` set to an empty `Vec`.
2. Encode with `encode_deterministic()`.
3. Decode the raw CBOR and inspect the map definition entry.
4. Assert: key 5 (`initial_data`) is **not present** in the CBOR map entry.
5. Assert: `decoded.map_initial_data[0]` is empty after round-trip.

---

## 8  Modem serial codec tests

### T-P080  ModemMessage round-trip — RESET

**Validates:** protocol-crate-design.md §10 (Modem serial codec)

**Procedure:**
1. Encode `ModemMessage::Reset` via `encode_modem_frame`.
2. Decode the result with `decode_modem_frame`.
3. Assert: decoded message equals `ModemMessage::Reset`.

### T-P081  ModemMessage round-trip — SEND_FRAME

**Validates:** protocol-crate-design.md §10

**Procedure:**
1. Encode `ModemMessage::SendFrame(SendFrame { peer_mac, frame_data })` with a known MAC and payload.
2. Decode with `decode_modem_frame`.
3. Assert: peer MAC and frame data match.

### T-P082  ModemMessage round-trip — all message types

**Validates:** protocol-crate-design.md §10

**Procedure:**
1. For each `ModemMessage` variant (`Reset`, `SendFrame`, `SetChannel`, `GetStatus`, `ScanChannels`, `ModemReady`, `RecvFrame`, `SetChannelAck`, `Status`, `ScanResult`, `Error`, `BleIndicate`, `BleEnable`, `BleDisable`, `BlePairingConfirmReply`, `BleRecv`, `BleConnected`, `BleDisconnected`, `BlePairingConfirm`, `Unknown { .. }`), encode and decode.
2. Assert: round-trip preserves all fields.

### T-P083  Frame envelope structure — LEN + TYPE + BODY

**Validates:** protocol-crate-design.md §10 (Frame format — length-prefixed)

**Procedure:**
1. Encode a `ModemReady` message.
2. Inspect the raw bytes: first 2 bytes are big-endian length, next byte is message type, remainder is body.
3. Assert: length field equals `body.len() + 1` (type byte + body).

### T-P084  Decode empty frame rejected

**Validates:** protocol-crate-design.md §10 (Error handling)

**Procedure:**
1. Construct a frame whose on-wire length prefix is zero (e.g., a 2-byte buffer containing `LEN = 0x0000` and no type/body bytes).
2. Call `decode_modem_frame` with this buffer.
3. Assert: returns a protocol error indicating an empty frame (i.e., the `LEN=0` case, not the empty-input/`Incomplete` case).

### T-P085  Decode oversized frame rejected

**Validates:** protocol-crate-design.md §10 (Error handling — `SERIAL_MAX_LEN`)

**Procedure:**
1. Call `decode_modem_frame` with a frame exceeding `SERIAL_MAX_LEN` (512).
2. Assert: returns an error.

### T-P086  FrameDecoder streaming — byte-by-byte assembly

**Validates:** protocol-crate-design.md §10 (Streaming decoder)

**Procedure:**
1. Create a `FrameDecoder`.
2. Push a valid frame one byte at a time via `push(&[byte])`.
3. Call `decode()` after each push.
4. Assert: `decode()` returns `None` until the full frame is available, then returns the complete message.

### T-P087  FrameDecoder streaming — multiple consecutive frames

**Validates:** protocol-crate-design.md §10 (Streaming decoder)

**Procedure:**
1. Encode two different messages and concatenate the raw frames.
2. Push the concatenated bytes into a `FrameDecoder` in one call.
3. Assert: two successive `decode()` calls return the two messages in order.

### T-P088  RecvFrame preserves negative RSSI

**Validates:** protocol-crate-design.md §10 (RSSI sign preservation)

**Procedure:**
1. Encode `ModemMessage::RecvFrame` with RSSI = −90.
2. Decode and assert: RSSI is −90 (not 166 or any unsigned reinterpretation).

### T-P089  Status max counter values

**Validates:** protocol-crate-design.md §10 (Boundary values)

**Procedure:**
1. Encode a `Status` message with all counters set to `u32::MAX`.
2. Decode and assert: all counters round-trip to `u32::MAX`.

### T-P090  Unknown message type decoded as Unknown

**Validates:** protocol-crate-design.md §10 (Unknown type handling)

**Procedure:**
1. Construct a frame with message type `0x7F` (undefined).
2. Decode with `decode_modem_frame`.
3. Assert: result matches `ModemMessage::Unknown { msg_type: 0x7F, body }` (or equivalent), and `body` equals the original payload bytes.

---

## 9  BLE envelope codec tests

### T-P100  BLE envelope round-trip

**Validates:** protocol-crate-design.md §11 (BLE envelope codec)

**Procedure:**
1. Encode a BLE envelope with `msg_type = 0x01` and a 10-byte body.
2. Parse the encoded bytes with `parse_ble_envelope`.
3. Assert: returned `msg_type` is `0x01` and body matches the original 10 bytes.

### T-P101  BLE envelope with empty body

**Validates:** protocol-crate-design.md §11

**Procedure:**
1. Encode a BLE envelope with `msg_type = 0x02` and an empty body.
2. Parse with `parse_ble_envelope`.
3. Assert: returned body is empty.

### T-P102  BLE envelope too short rejected

**Validates:** protocol-crate-design.md §11 (Error handling)

**Procedure:**
1. Call `parse_ble_envelope` with a 2-byte buffer (less than the 3-byte header).
2. Assert: returns `None`.

### T-P103  BLE envelope truncated body rejected

**Validates:** protocol-crate-design.md §11 (Error handling)

**Procedure:**
1. Construct a BLE envelope header with `LEN = 10` but provide only 5 body bytes.
2. Call `parse_ble_envelope`.
3. Assert: returns `None`.

### T-P104  BLE envelope trailing bytes rejected

**Validates:** protocol-crate-design.md §11 (Strict parsing)

**Procedure:**
1. Encode a valid BLE envelope, then append 2 extra bytes.
2. Call `parse_ble_envelope`.
3. Assert: returns `None` (rejects trailing data).
