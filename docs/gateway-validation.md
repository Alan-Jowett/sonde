<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Validation Specification

> **Document status:** Draft  
> **Scope:** Integration and system-level test plan for the Sonde gateway.  
> **Audience:** Implementers (human or LLM agent) writing gateway tests.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md), [protocol.md](protocol.md)

---

## 1  Overview

This document defines integration test cases that validate the gateway against the requirements in [gateway-requirements.md](gateway-requirements.md). Each test case is traceable to one or more requirements.

**Scope:** These are integration tests that exercise the gateway through its external interfaces (transport and handler I/O). Unit tests for internal modules are expected but are not specified here.

**Test harness:** All tests use a **mock transport** (in-process implementation of the `Transport` trait) and a **mock storage** backend. The mock transport simulates node frames — no real radio hardware is needed. A mock or stub handler process is used for handler API tests.

---

## 2  Test environment

### 2.1  Mock transport

An in-process `Transport` implementation that:

- Queues inbound frames (simulating node transmissions).
- Captures outbound frames (for assertion).
- Provides helper methods to construct valid authenticated frames for a given node PSK.

### 2.2  Mock storage

An in-memory `Storage` implementation pre-loaded with test data (node records, program images).

### 2.3  Test node helper

A helper that constructs valid protocol frames:

```
TestNode {
    key_hint: u16,
    psk: [u8; 32],
    
    fn wake(firmware_abi_version: u32, program_hash: &[u8], battery_mv: u32) -> Frame
    fn get_chunk(seq: u64, chunk_index: u32) -> Frame
    fn program_ack(seq: u64, program_hash: &[u8]) -> Frame
    fn app_data(seq: u64, blob: &[u8]) -> Frame
}
```

The helper handles header construction, CBOR encoding, sequence numbering, and AES-256-GCM encryption.

### 2.4  Test handler

A configurable stub handler process (or in-process mock) that:

- Reads DATA messages from stdin.
- Writes DATA_REPLY messages to stdout.
- Can be configured to: reply with specific data, reply with empty data, exit with code 0, exit with non-zero, crash mid-message, or delay before replying.

---

## 3  Protocol and communication tests

### T-0100  No unsolicited transmission

**Validates:** GW-0100

**Procedure:**
1. Start the gateway with one registered node.
2. Wait 5 seconds without sending any frames.
3. Assert: zero outbound frames captured by mock transport.

---

### T-0101  Valid CBOR encoding

**Validates:** GW-0101

**Procedure:**
1. Send a valid WAKE from a registered node.
2. Capture the COMMAND response.
3. Decode the CBOR payload.
4. Assert: payload is valid CBOR (RFC 8949).
5. Assert: all fields use integer keys matching the protocol CBOR key mapping.

---

### T-0102  Malformed CBOR tolerance

**Validates:** GW-0101

**Procedure:**
1. Construct a frame with valid header and GCM tag but garbage bytes as the CBOR payload.
2. Send to gateway.
3. Assert: no response sent, no crash, event logged.

---

### T-0103  WAKE reception and field extraction

**Validates:** GW-0102

**Procedure:**
1. Send a WAKE with `firmware_abi_version=1`, `program_hash=<known_hash>`, `battery_mv=3300`.
2. Assert: gateway responds with a COMMAND.
3. Assert: the node's registry entry is updated with the received `firmware_abi_version` and `battery_mv`.

---

### T-0104  WAKE with missing fields

**Validates:** GW-0102

**Procedure:**
1. Send a WAKE missing `battery_mv` (valid AEAD, valid header).
2. Assert: gateway discards the frame (no COMMAND response).

---

### T-0105  COMMAND response structure

**Validates:** GW-0103

**Procedure:**
1. Send a valid WAKE.
2. Capture the COMMAND response.
3. Assert: response header `nonce` matches the WAKE nonce.
4. Assert: CBOR payload contains `command_type`, `starting_seq`, and `timestamp_ms`.
5. Assert: `timestamp_ms` is a reasonable UTC value (within 5 seconds of test clock).

---

### T-0106  Frame size constraint

**Validates:** GW-0104

**Procedure:**
1. Register a program with chunks that approach the frame size limit.
2. Trigger a chunked transfer.
3. Capture all outbound CHUNK frames.
4. Assert: every outbound frame ≤ 250 bytes.

---

## 4  Command set tests

### T-0200  NOP command

**Validates:** GW-0200

**Procedure:**
1. Register a node with program_hash matching the assigned program.
2. Send WAKE with matching `program_hash`.
3. Assert: COMMAND response has `command_type = 0x00` (NOP).
4. Assert: no command-specific payload beyond `starting_seq`, `timestamp_ms`, and `command_type`.

---

### T-0201  UPDATE_PROGRAM command

**Validates:** GW-0201, GW-0701

**Procedure:**
1. Register a node. Assign program A. Node reports program B hash in WAKE.
2. Send WAKE with `program_hash = hash_B`.
3. Assert: COMMAND response has `command_type = 0x01` (UPDATE_PROGRAM).
4. Assert: payload includes `program_hash`, `program_size`, `chunk_size`, `chunk_count` for program A.

---

### T-0202  RUN_EPHEMERAL command

**Validates:** GW-0202

**Procedure:**
1. Queue an ephemeral program for a node.
2. Send WAKE from that node.
3. Assert: COMMAND response has `command_type = 0x02` (RUN_EPHEMERAL).
4. Assert: payload includes correct metadata for the ephemeral program.

---

### T-0203  UPDATE_SCHEDULE command

**Validates:** GW-0203

**Procedure:**
1. Queue a schedule change (interval_s = 120) for a node.
2. Send WAKE from that node.
3. Assert: COMMAND response has `command_type = 0x03` (UPDATE_SCHEDULE).
4. Assert: payload includes `interval_s = 120`.

---

### T-0204  REBOOT command

**Validates:** GW-0204

**Procedure:**
1. Queue a reboot request for a node.
2. Send WAKE from that node.
3. Assert: COMMAND response has `command_type = 0x04` (REBOOT).
4. Assert: no command-specific payload beyond standard COMMAND fields.

---

### T-0205  Command priority ordering

**Validates:** GW-0200–0204

**Procedure:**
1. Queue an ephemeral program AND a schedule change AND a program update for the same node.
2. Send WAKE.
3. Assert: COMMAND is RUN_EPHEMERAL (highest priority).
4. On next WAKE: assert UPDATE_PROGRAM.
5. On next WAKE: assert UPDATE_SCHEDULE.
6. On next WAKE: assert NOP.

---

### T-0206  Ephemeral size budget exceeded at dispatch

**Validates:** GW-0202

**Procedure:**
1. Queue an ephemeral program whose CBOR image exceeds 2 KB for a node.
2. Send WAKE.
3. Assert: gateway does NOT issue RUN_EPHEMERAL.
4. Assert: error logged indicating size budget exceeded.
5. Assert: on next WAKE, gateway falls through to next pending command (or NOP).

---

## 5  Chunked transfer tests

### T-0300  Complete chunked transfer

**Validates:** GW-0300

**Procedure:**
1. Assign a multi-chunk program to a node (e.g., 4 chunks).
2. Send WAKE → receive UPDATE_PROGRAM with `chunk_count=4`.
3. Send GET_CHUNK {0} → receive CHUNK {0, data}.
4. Send GET_CHUNK {1} → receive CHUNK {1, data}.
5. Send GET_CHUNK {2} → receive CHUNK {2, data}.
6. Send GET_CHUNK {3} → receive CHUNK {3, data}.
7. Assert: reassembled data matches the stored CBOR program image.
8. Assert: each CHUNK response echoes the sequence number from the corresponding GET_CHUNK.

---

### T-0301  Transfer resumption from chunk 0

**Validates:** GW-0301

**Procedure:**
1. Start a chunked transfer. Request chunks 0 and 1.
2. Simulate node sleep (let session timeout).
3. Send a new WAKE → receive UPDATE_PROGRAM again.
4. Request chunks starting from 0.
5. Assert: gateway serves all chunks without error.
6. Assert: data is identical to the first transfer attempt.

---

### T-0302  Program acknowledgement

**Validates:** GW-0302

**Procedure:**
1. Complete a chunked transfer.
2. Send PROGRAM_ACK with the correct `program_hash`.
3. Assert: node's `current_program_hash` in registry is updated.
4. Send WAKE with the new hash.
5. Assert: COMMAND is NOP (no longer mismatched).

---

### T-0303  Invalid chunk_index in GET_CHUNK

**Validates:** GW-0300

**Procedure:**
1. Complete WAKE → UPDATE_PROGRAM with `chunk_count=4`.
2. Send GET_CHUNK with `chunk_index=4` (out of range).
3. Assert: gateway silently discards the frame (no CHUNK response).
4. Send GET_CHUNK with `chunk_index=3` (last valid).
5. Assert: valid CHUNK response returned.

---

## 6  BPF program management tests

### T-0400  Valid ELF ingestion

**Validates:** GW-0400

**Procedure:**
1. Submit a valid BPF ELF file for ingestion.
2. Assert: gateway accepts it, stores a CBOR program image.
3. Assert: the stored image contains bytecode and map definitions.
4. Assert: LDDW relocations are resolved to `src=1, imm=<map_index>`.

---

### T-0401  Invalid ELF rejection

**Validates:** GW-0400

**Procedure:**
1. Submit a non-ELF file (random bytes).
2. Assert: gateway rejects with a clear diagnostic.
3. Assert: no program is stored.

---

### T-0402  Prevail verification — resident pass

**Validates:** GW-0401

**Procedure:**
1. Submit a valid resident BPF program (bounded loops, valid helpers).
2. Assert: verification passes, program is stored.

---

### T-0403  Prevail verification — resident fail

**Validates:** GW-0401

**Procedure:**
1. Submit a BPF program with unbounded loops.
2. Assert: verification fails with diagnostic.
3. Assert: program is not stored.

---

### T-0404  Prevail verification — ephemeral profile

**Validates:** GW-0401

**Procedure:**
1. Submit a BPF program that calls `map_update_elem` as ephemeral.
2. Assert: verification fails (map writes not allowed in ephemeral profile).

---

### T-0405  Content hash identity

**Validates:** GW-0402

**Procedure:**
1. Ingest the same ELF file twice.
2. Assert: both produce the same `program_hash`.
3. Assert: only one program record exists in storage.

---

### T-0406  Hash covers maps

**Validates:** GW-0402

**Procedure:**
1. Create two ELF files with identical bytecode but different map definitions.
2. Ingest both.
3. Assert: they produce different `program_hash` values.

---

### T-0407  Program size enforcement

**Validates:** GW-0403

**Procedure:**
1. Submit a resident program whose CBOR image exceeds 4 KB.
2. Assert: rejected with size limit diagnostic.
3. Submit an ephemeral program whose CBOR image exceeds 2 KB.
4. Assert: rejected.

---

### T-0408  Ephemeral program with maps rejected

**Validates:** GW-0401 (criterion 5)

**Procedure:**
1. Construct a valid BPF ELF that declares one or more map definitions.
2. Submit it for ingestion with the ephemeral verification profile.
3. Assert: ingestion fails with an error indicating ephemeral programs must not declare maps.
4. Assert: no program record is stored.

---

### T-0409  Sonde verifier platform — helpers accepted

**Validates:** GW-0404

**Procedure:**
1. Submit a valid BPF ELF that calls a sonde-specific helper (e.g., `gpio_read`, helper ID 5) with correct argument types.
2. Assert: verification passes — the program is accepted and stored.

---

### T-0410  Sonde verifier platform — no LinuxPlatform

**Validates:** GW-0404

**Procedure:**
1. Confirm that `ingest_elf()` constructs a `SondePlatform` (not `LinuxPlatform`) for verification.
2. Assert: `ingest_elf()` passes `SondePlatform` (not `LinuxPlatform`) to the verifier / helper-prototype engine; any `LinuxPlatform` usage is encapsulated inside `SondePlatform` (e.g., for ELF/map parsing), not passed directly to Prevail.

---

### T-0411  ELF with .rodata produces initial data

**Validates:** GW-0405

**Procedure:**
1. Ingest a BPF ELF that contains a `.rodata` section with known content (e.g., compile-time constants).
2. Decode the resulting CBOR program image.
3. Assert: the map definition corresponding to the `.rodata` section includes `initial_data` (key 5) matching the section bytes.
4. Assert: other map definitions (explicit maps, `.bss`) have empty or absent `initial_data`.

---

### T-0412  ELF with .bss produces empty initial data

**Validates:** GW-0405

**Procedure:**
1. Ingest a BPF ELF that contains a `.bss` section (SHT_NOBITS).
2. Decode the resulting CBOR program image.
3. Assert: the map definition corresponding to the `.bss` section has empty `initial_data` (key 5 absent or empty bytes).

---

### T-0413  Multi-section ELF filters to sonde section

**Validates:** GW-0401 (criterion 6)

**Procedure:**
1. Construct a BPF ELF containing two executable sections: a `sonde` section with a valid program (`mov r0, 0; exit`) and a `.text` section with a different valid program.
2. Submit it for ingestion with the resident verification profile.
3. Assert: ingestion succeeds — exactly one program is extracted (the `sonde` section program).
4. Assert: the stored bytecode matches the `sonde` section, not the `.text` section.

---

### T-0414  Source filename round-trip

**Validates:** GW-0400 (criterion 7), GW-0402 (criterion 4)

**Procedure:**
1. Ingest a valid BPF program with `source_filename` set to `"tmp102_sensor.o"`.
2. Call `ListPrograms`.
3. Assert: the returned `ProgramInfo` for that hash includes `source_filename == "tmp102_sensor.o"`.
4. Ingest a second program **without** a `source_filename`.
5. Call `ListPrograms`.
6. Assert: the second program's `source_filename` is empty / absent.

---

## 7  Application data tests

### T-0500  APP_DATA reception and forwarding

**Validates:** GW-0500, GW-0505

**Procedure:**
1. Complete a WAKE handshake. Send APP_DATA with blob `[0x01, 0x02, 0x03]`.
2. Assert: handler receives a DATA message with correct `msg_type`, `request_id`, `node_id`, `program_hash`, `data`, and `timestamp`.
3. Assert: `data` matches the original blob.

---

### T-0501  APP_DATA_REPLY with non-zero data

**Validates:** GW-0501

**Procedure:**
1. Configure handler to reply with `data = [0xAA, 0xBB]`.
2. Send APP_DATA.
3. Assert: gateway sends APP_DATA_REPLY to the node.
4. Assert: APP_DATA_REPLY blob matches `[0xAA, 0xBB]`.
5. Assert: response header nonce echoes the APP_DATA sequence number.

---

### T-0502  APP_DATA_REPLY suppressed on zero-length data

**Validates:** GW-0501

**Procedure:**
1. Configure handler to reply with `data = []` (zero-length).
2. Send APP_DATA.
3. Assert: no APP_DATA_REPLY is sent to the node.

---

### T-0503  Multiple APP_DATA per wake cycle

**Validates:** GW-0501

**Procedure:**
1. Complete WAKE handshake.
2. Send APP_DATA (seq=S), APP_DATA (seq=S+1), APP_DATA (seq=S+2).
3. Assert: handler receives 3 DATA messages with distinct `request_id`s.
4. Assert: each gets an independent reply (or suppressed, per handler config).

---

### T-0503a  APP_DATA with valid AEAD accepted

**Validates:** GW-0600

**Procedure:**
1. Complete WAKE handshake using AES-256-GCM (AEAD).
2. Ensure the node's `current_program_hash` is set (e.g., via a prior `PROGRAM_ACK` or by pre-seeding storage).
3. Send an APP_DATA frame encrypted with AES-256-GCM using the node's PSK, with the canonical GCM nonce construction from `protocol.md` §7.1: `SHA-256(PSK)[0..3] ‖ msg_type ‖ frame_nonce.to_be_bytes()`, where `frame_nonce` is the session sequence number carried in the frame header `nonce` field.
4. Assert: gateway successfully decrypts the frame and advances the session sequence number (proving AEAD authentication and CBOR decode succeeded). Handler routing is validated separately by T-E2E-032.

---

### T-0503b  APP_DATA with invalid GCM tag rejected

**Validates:** GW-0600

**Procedure:**
1. Complete WAKE handshake using AEAD.
2. Construct an APP_DATA frame with valid header and CBOR payload, but corrupt the 16-byte GCM authentication tag (flip one bit).
3. Assert: gateway silently discards the frame — no handler invocation, no APP_DATA_REPLY, no crash.

---

### T-0503c  APP_DATA with HMAC framing rejected by AEAD gateway

**Validates:** GW-0600

**Procedure:**
1. Complete WAKE handshake using AEAD.
2. Send an APP_DATA frame authenticated with HMAC-SHA256 (old framing format: 11B header + plaintext CBOR + 32B HMAC) instead of AES-256-GCM.
3. Assert: gateway silently discards the frame — the AEAD decode/decrypt fails because the frame structure does not match the expected AEAD format (ciphertext + 16B GCM tag).

---

### T-0504  Handler transport framing

**Validates:** GW-0502

**Procedure:**
1. Send APP_DATA.
2. Inspect raw bytes written to handler stdin.
3. Assert: 4-byte big-endian length prefix followed by CBOR payload.
4. Assert: message size ≤ 1 MB.

---

### T-0505  Handler respawn on clean exit

**Validates:** GW-0503

**Procedure:**
1. Configure handler to process one message and exit with code 0.
2. Send APP_DATA → handler processes and exits.
3. Send another APP_DATA.
4. Assert: handler is respawned and processes the second message.

---

### T-0506  Handler crash — no reply to node

**Validates:** GW-0503

**Procedure:**
1. Configure handler to exit with code 1 (crash) mid-message.
2. Send APP_DATA.
3. Assert: no APP_DATA_REPLY is sent to the node.
4. Assert: error is logged.

---

### T-0506a  Handler stderr captured in gateway log

**Validates:** GW-0503

**Procedure:**
1. Configure a handler that writes a diagnostic message to stderr and exits with code 1 (e.g., a Python script with a missing import).
2. Trigger handler spawn by sending APP_DATA.
3. Capture tracing output.
4. Assert: the handler's stderr output appears in the gateway log at WARN level, tagged with the handler command (AC4).
5. Assert: the handler exit is logged at ERROR level with exit code 1.

---

### T-0507  Handler routing by program hash

**Validates:** GW-0504

**Procedure:**
1. Configure handler A for program hash X, handler B for program hash Y.
2. Node with program X sends APP_DATA.
3. Assert: handler A receives the DATA message.
4. Node with program Y sends APP_DATA.
5. Assert: handler B receives the DATA message.

---

### T-0508  Handler routing — no match, no reply

**Validates:** GW-0504

**Procedure:**
1. Configure no handler for program hash Z (and no catch-all).
2. Node with program Z sends APP_DATA.
3. Assert: no APP_DATA_REPLY sent to node, no crash.

---

### T-0509  Handler routing — catch-all

**Validates:** GW-0504

**Procedure:**
1. Configure a catch-all handler (ProgramMatcher::Any).
2. Node with any program hash sends APP_DATA.
3. Assert: catch-all handler receives the DATA message.

---

### T-0509a  Handler routing — many-to-one

**Validates:** GW-0504

**Procedure:**
1. Configure handler A for program hashes X and Y (many-to-one mapping).
2. Node with program X sends APP_DATA.
3. Assert: handler A receives the DATA message.
4. Node with program Y sends APP_DATA.
5. Assert: handler A receives the DATA message (same handler for both hashes).

---

### T-0510  Handler request_id correlation

**Validates:** GW-0506

**Procedure:**
1. Send two APP_DATA messages in quick succession.
2. Handler replies to both, echoing `request_id`.
3. Assert: each APP_DATA_REPLY is sent to the correct node frame (matched by sequence number).

---

### T-0511  Handler request_id mismatch

**Validates:** GW-0506

**Procedure:**
1. Send APP_DATA.
2. Handler replies with a `request_id` that does not match any outstanding request.
3. Assert: reply is discarded, logged.

---

### T-0512  EVENT messages

**Validates:** GW-0507

**Procedure:**
1. Send WAKE from a node.
2. Assert: handler receives an EVENT message with `event_type = "node_online"`, correct `battery_mv` and `firmware_abi_version`.
3. Complete a program update (PROGRAM_ACK).
4. Assert: handler receives EVENT `event_type = "program_updated"` with correct `program_hash`.

---

### T-0513  LOG messages from handler

**Validates:** GW-0508

**Procedure:**
1. Handler writes a LOG message (`level: "info"`, `message: "test log"`).
2. Assert: message appears in gateway log output with correct level.

---

### T-0514  Oversized handler message rejection

**Validates:** GW-0502

**Procedure:**
1. Configure a mock handler that writes a DATA_REPLY with a length prefix of 2 MB (exceeding the 1 MB limit), then closes its stdout without sending any body bytes.
2. Send APP_DATA to trigger the handler.
3. Assert: gateway detects the oversized length prefix and rejects the reply based on the length prefix alone, without attempting to read the full body.
4. Assert: no APP_DATA_REPLY sent to node.
5. Assert: error logged.

---

### T-0515  Long-running handler persistence

**Validates:** GW-0503

**Procedure:**
1. Configure a handler that stays alive across messages (long-running mode).
2. Send APP_DATA → handler replies.
3. Send another APP_DATA.
4. Assert: same handler process receives the second message (no respawn).
5. Assert: handler instance identity is stable across both messages (for example, same PID when using a subprocess, or the same test-assigned instance ID for an in-process mock).

---

### T-0516  Handler hang timeout

**Validates:** GW-0503

**Procedure:**
1. Configure a handler that reads a DATA message but never writes a reply (hangs).
2. Send APP_DATA.
3. Wait for the handler reply timeout (`handler_timeout`).
4. Assert: no APP_DATA_REPLY is sent to the node.
5. Assert: the gateway does not block processing for other nodes.

---

### T-0517  Node timeout event

**Validates:** GW-0507

**Procedure:**
1. Register a node with a known schedule (`interval_s = 10`).
2. Ensure the gateway is configured with a known `node_timeout_multiplier` (default is 3× unless overridden).
3. Send WAKE.
4. Wait for `node_timeout_multiplier × interval_s` without sending another WAKE (e.g., 30 seconds when `node_timeout_multiplier = 3`).
5. Assert: handler receives an EVENT message with `event_type = "node_timeout"`.
6. Assert: event includes `last_seen` (matching the WAKE timestamp) and `expected_interval_s = 10`.

---

## 8  Authentication and security tests

### T-0600  Valid AEAD accepted

**Validates:** GW-0600

**Procedure:**
1. Send a correctly authenticated WAKE.
2. Assert: gateway processes it and responds with COMMAND.

---

### T-0601  Invalid GCM tag rejected

**Validates:** GW-0600

**Procedure:**
1. Construct a WAKE with a valid header but corrupt the GCM authentication tag (flip one bit).
2. Send to gateway.
3. Assert: silently discarded, no response sent.

---

### T-0602  Wrong key rejected

**Validates:** GW-0600, GW-0601

**Procedure:**
1. Construct a WAKE using PSK_A but with a `key_hint` that maps to PSK_B.
2. Send to gateway.
3. Assert: AES-256-GCM decryption fails, silently discarded.

---

### T-0603  key_hint collision handling

**Validates:** GW-0601

**Procedure:**
1. Register two nodes with the same `key_hint` but different PSKs.
2. Send WAKE from node A.
3. Assert: gateway tries both PSKs, accepts the one that matches.
4. Assert: response is sent to the correct peer address.

---

### T-0603a  FileKeyProvider — happy path

**Validates:** GW-0601b

**Procedure:**
1. Write a valid 64-hex-char key to a temp file.
2. Construct `FileKeyProvider` pointing to that file.
3. Call `load_master_key()`.
4. Assert: returns `Ok` with the expected 32-byte key.

---

### T-0603b  FileKeyProvider — missing file

**Validates:** GW-0601b

**Procedure:**
1. Construct `FileKeyProvider` with a path that does not exist.
2. Call `load_master_key()`.
3. Assert: returns `Err(KeyProviderError::Io(_))`.

---

### T-0603c  FileKeyProvider — malformed content

**Validates:** GW-0601b

**Procedure:**
1. Write a non-hex string to a temp file.
2. Construct `FileKeyProvider` pointing to that file.
3. Call `load_master_key()`.
4. Assert: returns `Err(KeyProviderError::Format(_))`.

---

### T-0603d  EnvKeyProvider — happy path

**Validates:** GW-0601b

**Procedure:**
1. Set an environment variable to a valid 64-hex-char key.
2. Construct `EnvKeyProvider` for that variable name.
3. Call `load_master_key()`.
4. Assert: returns `Ok` with the expected 32-byte key.

---

### T-0603e  EnvKeyProvider — variable not set

**Validates:** GW-0601b

**Procedure:**
1. Ensure a test-specific environment variable is unset.
2. Construct `EnvKeyProvider` for that variable name.
3. Call `load_master_key()`.
4. Assert: returns `Err(KeyProviderError::Io(_))`.

---

### T-0603f  DpapiKeyProvider — round-trip (Windows only)

**Validates:** GW-0601b  
**Platforms:** Windows

**Procedure:**
1. Generate a random 32-byte key.
2. Call `protect_with_dpapi(&key, blob_path)` to write the DPAPI blob.
3. Construct `DpapiKeyProvider::new(blob_path)`.
4. Call `load_master_key()`.
5. Assert: returns `Ok` with the same 32-byte key.

---

### T-0603g  DpapiKeyProvider — unavailable on non-Windows

**Validates:** GW-0601b  
**Platforms:** Linux, macOS

**Procedure:**
1. Pass `--key-provider dpapi` on a non-Windows platform.
2. Assert: `build_key_provider()` returns an error containing `"Windows"`.

---

### T-0603h  SecretServiceKeyProvider — round-trip (Linux only)

**Validates:** GW-0601b  
**Platforms:** Linux (requires a running Secret Service daemon)

**Procedure:**
1. Generate a random 32-byte key.
2. Call `store_in_secret_service(&key, "test-sonde-master-key")`.
3. Construct `SecretServiceKeyProvider::new("test-sonde-master-key")`.
4. Call `load_master_key()`.
5. Assert: returns `Ok` with the same 32-byte key.
6. Clean up: delete the keyring item.

---

### T-0603i  SecretServiceKeyProvider — item not found

**Validates:** GW-0601b  
**Platforms:** Linux (requires a running Secret Service daemon)

**Procedure:**
1. Construct `SecretServiceKeyProvider::new("nonexistent-label-xyz")`.
2. Call `load_master_key()` (item is not in keyring).
3. Assert: returns `Err(KeyProviderError::Backend(_))`.

---

### T-0603j  SecretServiceKeyProvider — unavailable on non-Linux

**Validates:** GW-0601b  
**Platforms:** Windows, macOS

**Procedure:**
1. Pass `--key-provider secret-service` on a non-Linux platform.
2. Assert: `build_key_provider()` returns an error containing `"Linux"`.

---

### T-0603k  Wrong master key detected at startup

**Validates:** GW-0601b (fallback detection, all backends)

**Procedure:**
1. Open a `SqliteStorage` with key A and register a node (PSK is encrypted with key A).
2. Re-open `SqliteStorage` with a different key B.
3. Assert: `open()` returns an error (wrong key detected by PSK validation at startup).
4. Assert: the error is returned before any storage operations are possible.

---

### T-0604  Replay protection — sequence number enforced

**Validates:** GW-0602

**Procedure:**
1. Complete WAKE handshake (starting_seq = S).
2. Send APP_DATA with seq = S. Assert: accepted.
3. Replay the same frame (seq = S again).
4. Assert: silently discarded.

---

### T-0605  Replay protection — WAKE creates new session

**Validates:** GW-0602

**Procedure:**
1. Complete WAKE handshake (session 1, starting_seq = S1).
2. Send another WAKE (session 2, starting_seq = S2).
3. Send APP_DATA with seq = S1 (from old session).
4. Assert: rejected (old session replaced).
5. Send APP_DATA with seq = S2.
6. Assert: accepted.

---

### T-0606  Replay protection — wrong sequence number

**Validates:** GW-0602

**Procedure:**
1. Complete WAKE handshake (starting_seq = S).
2. Send APP_DATA with seq = S+5 (skipping ahead).
3. Assert: rejected (expected S, got S+5).

---

### T-0607  Replay protection — no active session

**Validates:** GW-0602

**Procedure:**
1. Without sending WAKE, send APP_DATA with arbitrary sequence number.
2. Assert: silently discarded (no active session).

### T-0607a  WAKE retry preserves ChunkedTransfer session

**Validates:** GW-0602 (criterion 5)

**Procedure:**
1. Send WAKE with nonce N. Receive COMMAND with `RunEphemeral` or `UpdateProgram` (chunked; program requires chunked transfer).
2. Assert: session is in `ChunkedTransfer` state.
3. Send a second WAKE with the same nonce N (simulating a retry).
4. Assert: the gateway does NOT create a new session — the existing `ChunkedTransfer` session is preserved.
5. Assert: the COMMAND response re-sends the same `RunEphemeral` or `UpdateProgram` (chunked) with the original program hash.
6. Send `GET_CHUNK` with the expected sequence number.
7. Assert: the gateway responds with the requested chunk data.

---

### T-0608  Frame overhead budget

**Validates:** GW-0603

**Procedure:**
1. Capture any outbound frame.
2. Assert: first 11 bytes are header (key_hint 2B + msg_type 1B + nonce 8B).
3. Assert: last 16 bytes are GCM authentication tag.
4. Assert: total frame = 11 + ciphertext_len + 16.

---

### T-0609  Unknown node — silent discard

**Validates:** GW-1002

**Procedure:**
1. Send WAKE from an unregistered node (key_hint with no matching PSK).
2. Assert: no response sent.
3. Assert: no internal state changed.
4. Assert: event logged.

---

### T-0610  Key store encryption at rest

**Validates:** GW-0601a

**Procedure:**
1. Create a `SqliteStorage` backed by a temporary file (not the in-memory mock) with a known master key.
2. Register a node with a known PSK `[0x42; 32]`.
3. Close the storage.
4. Open the SQLite database file using a direct SQL connection (bypassing the `SqliteStorage` API) and query the row for the registered node from the key-store table, selecting only the `psk` column as raw bytes.
5. Assert: the stored `psk` value is present, is not equal to the cleartext `[0x42; 32]` PSK, and matches the expected encrypted envelope shape/length (e.g., fixed-size ciphertext + metadata as defined by the key-store implementation).
6. (Optional sanity check) Read the raw SQLite file bytes and assert that neither the 32-byte raw PSK value nor its 64-char hex encoding appears as a contiguous substring in the raw file.
7. Re-open the database using `SqliteStorage` with the correct master key.
8. Assert: the PSK is correctly retrieved via the storage API and matches the original `[0x42; 32]`.
9. Attempt to open the same database with an incorrect master key and either (a) assert that opening or key lookup fails as designed, or (b) if the error is deferred to decryption time, attempt to retrieve the PSK and assert that decryption fails and does not yield the original `[0x42; 32]`.

---

## 9  Node management tests

### T-0700  Node registry persistence

**Validates:** GW-0700

**Procedure:**
1. Register a node via storage.
2. Restart the gateway (re-initialize from storage).
3. Send WAKE from the registered node.
4. Assert: gateway recognizes the node and responds.

---

### T-0701  Stale program detection

**Validates:** GW-0701

**Procedure:**
1. Assign program A to a node.
2. Send WAKE with `program_hash = hash_A` → assert NOP.
3. Reassign to program B.
4. Send WAKE with `program_hash = hash_A` → assert UPDATE_PROGRAM for B.

---

### T-0702  Battery level tracking

**Validates:** GW-0702

**Procedure:**
1. Send WAKE with `battery_mv = 3300`.
2. Assert: node registry entry `last_battery_mv = 3300`.
3. Send WAKE with `battery_mv = 2900`.
4. Assert: updated to `2900`.

---

### T-0703  Firmware ABI version tracking

**Validates:** GW-0703

**Procedure:**
1. Send WAKE with `firmware_abi_version = 2`.
2. Assert: node registry records ABI version 2.

---

### T-0704  ABI incompatibility

**Validates:** GW-0703

**Procedure:**
1. Assign a program compiled for ABI version 3 to a node with ABI version 2.
2. Send WAKE.
3. Assert: gateway does NOT issue UPDATE_PROGRAM (incompatible ABI).
4. Assert: warning logged.

---

### T-0705  Battery historical data

**Validates:** GW-0702

**Procedure:**
1. Send WAKE with `battery_mv = 3300`.
2. Send WAKE with `battery_mv = 3100`.
3. Send WAKE with `battery_mv = 2900`.
4. Assert: storage retains all three readings (not just the latest).
5. Assert: readings can be queried in chronological order for trend analysis.

---

### T-0706  Factory reset

**Validates:** GW-0705

**Procedure:**
1. Provision a node with a known PSK `K_old` and deploy a program that writes non-zero data into node persistent state (e.g., a boot counter or stored configuration value).
2. Assert (pre-reset): the gateway registry contains the node with PSK `K_old` and the assigned program. The node can successfully authenticate (WAKE accepted). Application data reflects non-default persistent state.
3. Trigger a factory reset for this node via the admin API (e.g., `RemoveNode` plus any gateway action that causes the node to perform a factory reset on next contact, per design).
4. Assert (gateway-side): the node's PSK and program assignment are removed from the gateway registry. No further commands or program updates are queued for the node.
5. After the reset has completed on the node, send WAKE using the pre-reset credentials (`K_old`).
6. Assert: WAKE frames using `K_old` are silently discarded (unknown/unauthenticated node). No authenticated session is established.
7. Re-provision the same hardware as a new node via the normal pairing/provisioning flow.
8. Assert (post-reset, after re-provisioning): the newly assigned PSK `K_new` differs from `K_old`. Any program assigned after re-provisioning must be explicitly (re)deployed; the previous program image is not implicitly restored. Application data that exposes persistent state (e.g., boot counter) has returned to its factory-default value, demonstrating that node-side persistent state was erased.

---

## 9A  Admin API tests

### T-0800  gRPC API availability

**Validates:** GW-0800

**Procedure:**
1. Start the gateway.
2. Connect to the gRPC admin API on the configured address.
3. Assert: connection succeeds and a defined admin RPC (e.g., `ListNodes`) can be called successfully.

---

### T-0801  Node registration via gRPC

**Validates:** GW-0801

**Procedure:**
1. Call `RegisterNode` with key_hint, PSK, and admin node_id.
2. Assert: success response.
3. Call `ListNodes`.
4. Assert: new node appears in the list with correct metadata.
5. Send WAKE from the registered node.
6. Assert: gateway recognizes the node and responds.

---

### T-0802  Node removal via gRPC

**Validates:** GW-0801

**Procedure:**
1. Register a node.
2. Call `RemoveNode`.
3. Assert: node no longer appears in `ListNodes`.
4. Send WAKE from the removed node.
5. Assert: silently discarded (unknown node).

---

### T-0803  Program ingestion via gRPC

**Validates:** GW-0802

**Procedure:**
1. Call `IngestProgram` with a valid ELF binary and `resident` profile.
2. Assert: success response with program hash.
3. Call `ListPrograms`.
4. Assert: program appears with correct hash, size, and profile.

---

### T-0804  Program ingestion failure via gRPC

**Validates:** GW-0802

**Procedure:**
1. Call `IngestProgram` with an invalid ELF (random bytes).
2. Assert: error response with diagnostic message.
3. Assert: no program stored.

---

### T-0805  Program assignment via gRPC

**Validates:** GW-0802, GW-0803

**Procedure:**
1. Ingest a program. Register a node.
2. Call `AssignProgram` with the node and program hash.
3. Send WAKE with a different `program_hash`.
4. Assert: COMMAND is UPDATE_PROGRAM for the assigned program.

---

### T-0806  Schedule change via gRPC

**Validates:** GW-0803

**Procedure:**
1. Register a node.
2. Call `SetSchedule` with node_id and interval_s = 300.
3. Send WAKE.
4. Assert: COMMAND is UPDATE_SCHEDULE with `interval_s = 300`.

---

### T-0807  Queue reboot via gRPC

**Validates:** GW-0803

**Procedure:**
1. Register a node.
2. Call `QueueReboot` with node_id.
3. Send WAKE.
4. Assert: COMMAND is REBOOT.

---

### T-0808  Queue ephemeral via gRPC

**Validates:** GW-0803

**Procedure:**
1. Ingest an ephemeral program. Register a node.
2. Call `QueueEphemeral` with node_id and program hash.
3. Send WAKE.
4. Assert: COMMAND is RUN_EPHEMERAL with correct program metadata.

---

### T-0809  Node status

**Validates:** GW-0804

**Procedure:**
1. Register a node.
2. Send WAKE with `battery_mv = 3100`, `firmware_abi_version = 2`.
3. Call `GetNodeStatus`.
4. Assert: status reflects battery 3100, ABI 2, recent `last_seen`.

---

### T-0810  State export and import via gRPC

**Validates:** GW-0805

**Procedure:**
1. Register nodes and ingest programs.
2. Call `ExportState` → save response bytes.
3. Start a fresh gateway.
4. Call `ImportState` with the saved bytes.
5. Call `ListNodes` and `ListPrograms`.
6. Assert: all nodes and programs are restored.

---

### T-0811  Admin API local-only binding

**Validates:** GW-0800

**Procedure:**
1. Start the gateway.
2. Assert: the admin API is bound to a local-only transport (Unix domain socket or Windows named pipe).
3. Assert: no TCP listener is opened on any network interface.
4. On Linux: verify the socket path exists as a UDS file.
5. On Windows: verify the named pipe `\\.\pipe\sonde-admin` is created.

---

### T-0812  Admin CLI integration

**Validates:** GW-0806

**Procedure:**
1. Start a gateway instance (using the default admin socket, or pass `--socket PATH` consistently to both the gateway and `sonde-admin` if overriding).
2. Run `sonde-admin --format json node list` against the admin socket.
3. Assert: command exits successfully with valid JSON output.
4. Register a node via `sonde-admin node register NODE_ID KEY_HINT PSK_HEX`, for example:
   `sonde-admin node register node-0001 1 4242424242424242424242424242424242424242424242424242424242424242`
5. Assert: command exits successfully.
6. Run `sonde-admin --format json node list`.
7. Assert: the new node `NODE_ID` appears in the output.
8. Run `sonde-admin node remove NODE_ID`, for example:
   `sonde-admin node remove node-0001`
9. Assert: command exits successfully.
10. Run `sonde-admin --format json node list`.
11. Assert: the node `NODE_ID` is no longer listed.

---

### T-0813  Modem status via admin API

**Validates:** GW-0807

**Procedure:**
1. Start gateway with modem connected.
2. Call `GetModemStatus`.
3. Assert: response contains radio channel, counters, and uptime.

---

### T-0814  Modem channel change via admin API

**Validates:** GW-0807

**Procedure:**
1. Call `SetModemChannel` with channel 6.
2. Assert: success response.
3. Call `GetModemStatus`.
4. Assert: reported channel is 6.

---

### T-0815  Modem channel scan via admin API

**Validates:** GW-0807

**Procedure:**
1. Call `ScanModemChannels`.
2. Assert: response contains, for each scanned channel, an AP count and a strongest RSSI value.

---

### T-0815a  Channel persisted after SetModemChannel

**Validates:** GW-0808

**Procedure:**
1. Open a gateway with an in-memory or temporary database; CLI `--channel 1`.
2. Call `SetModemChannel(7)`.
3. Read the `espnow_channel` config value from the database.
4. Assert: the persisted value is `"7"`.

---

### T-0815b  Modem reconnect restores persisted channel

**Validates:** GW-0808, GW-1103

**Procedure:**
1. Start gateway with `--channel 1`.
2. Call `SetModemChannel(7)` — channel 7 is persisted.
3. Simulate a modem disconnect and reconnect.
4. Assert: the reconnect startup sequence sends `SET_CHANNEL(7)`, not `SET_CHANNEL(1)`.

---

### T-0815c  BLE pairing uses persisted channel

**Validates:** GW-0808

**Procedure:**
1. Start gateway with `--channel 1`.
2. Call `SetModemChannel(7)`.
3. Trigger a `REGISTER_PHONE` BLE pairing flow.
4. Assert: the encrypted response contains `rf_channel = 7`, not `1`.

---

### T-0815d  CLI --channel seeds database on first startup

**Validates:** GW-0808

**Procedure:**
1. Start gateway with `--channel 3` and a fresh (empty) database.
2. Assert: the database `espnow_channel` config value is `"3"`.
3. Assert: modem startup sends `SET_CHANNEL(3)`.

---

### T-0815e  Persisted channel overrides CLI --channel

**Validates:** GW-0808

**Procedure:**
1. Pre-populate a database with `espnow_channel = "7"`.
2. Start gateway with `--channel 3`.
3. Assert: modem startup sends `SET_CHANNEL(7)` (database wins).

---

### T-0816  Admin CLI JSON output

**Validates:** GW-0806

**Procedure:**
1. Register a node and ingest a program.
2. Run `sonde-admin node list --format json`.
3. Assert: output is valid JSON containing the registered node.
4. Run `sonde-admin program list --format json`.
5. Assert: output is valid JSON containing the ingested program.
6. Run `sonde-admin status <node-id> --format json`.
7. Assert: output is valid JSON with expected status fields.

---

### T-0817  Admin CLI error handling

**Validates:** GW-0806

**Procedure:**
1. Run `sonde-admin node get nonexistent-node`.
2. Assert: non-zero exit code and meaningful error message.
3. Run `sonde-admin program assign <node-id> 0000000000000000000000000000000000000000000000000000000000000000`.
4. Assert: non-zero exit code indicating program not found.

---

## 10  Operational tests

### T-1000  Gateway failover

**Validates:** GW-1000

**Procedure:**
1. Start gateway instance A with a node registry.
2. Complete a WAKE handshake with a node.
3. Export state from A.
4. Start gateway instance B, import state.
5. Send WAKE from the same node to B.
6. Assert: B recognizes the node and responds correctly.

---

### T-1001  Program hash consistency

**Validates:** GW-1004

**Procedure:**
1. Ingest the same ELF on two gateway instances.
2. Request the same chunk (same hash, same index) from both.
3. Assert: chunk data is byte-identical.

---

### T-1002  Export/import round-trip

**Validates:** GW-1001

**Procedure:**
1. Register nodes and programs.
2. Export state.
3. Create a fresh gateway, import state.
4. Assert: all nodes and programs are present with identical data.

---

### T-1003  Concurrent node handling

**Validates:** GW-1003

**Procedure:**
1. Register 10 nodes.
2. Send WAKE from all 10 simultaneously (parallel injection into mock transport).
3. Assert: all 10 receive COMMAND responses.
4. Assert: no cross-contamination of per-node state.

---

### T-1004  Session timeout and cleanup

**Validates:** GW-0602

**Procedure:**
1. Send WAKE, receive COMMAND (session created).
2. Wait for session timeout (configurable, default 30s).
3. Send APP_DATA with the session's sequence number.
4. Assert: rejected (session expired).

---

### T-1005  Export plaintext key leakage

**Validates:** GW-1001

**Procedure:**
1. Register nodes with known PSKs.
2. Call `ExportState` with a known export passphrase (e.g., `test-export-passphrase`).
3. Inspect the raw export bytes (encrypted bundle).
4. Assert: no PSK value appears as a contiguous substring in the export payload.
5. Attempt to import or use the export without the correct passphrase (e.g., omit the passphrase or supply an incorrect one). Assert: import is rejected with an authentication/invalid-passphrase error and the gateway state is unchanged (registered nodes are not restored and WAKE from those nodes is not accepted).
6. Import the export into a fresh gateway using the correct export passphrase.
7. Assert: nodes are restored and PSKs are functional (WAKE from registered node is accepted).

---

### T-1005b  Import restores phone PSKs and handler configs

**Validates:** GW-0805, GW-1001

**Procedure:**
1. Start a gateway, register nodes, ingest programs, register phone PSKs, and configure handler routing entries.
2. Call `ExportState` with an export passphrase.
3. Start a fresh gateway with no pre-existing state.
4. Call `ImportState` with the exported bytes and passphrase.
5. Assert: all phone PSKs are restored with correct `phone_key_hint`, PSK value, label, `issued_at`, and status.
6. Assert: handler configs are restored with correct command, args, and `reply_timeout`.
7. Assert: nodes and programs are also restored (full-state round-trip).

---

## 11  Modem transport adapter tests

### T-1100  UsbEspNowTransport — recv delivers RECV_FRAME

**Validates:** GW-1100

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup (RESET → MODEM_READY → SET_CHANNEL → SET_CHANNEL_ACK).
3. Inject a `RECV_FRAME` message from the mock modem with known `peer_mac`, `rssi`, and `frame_data`.
4. Call `Transport::recv()`.
5. Assert: returns `(frame_data, peer_mac)` matching the injected values.

---

### T-1101  UsbEspNowTransport — send produces SEND_FRAME

**Validates:** GW-1100

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup.
3. Call `Transport::send(frame, peer_mac)`.
4. Assert: the mock modem receives a well-formed `SEND_FRAME` message with the correct `peer_mac` and `frame_data`.
5. Assert: `send()` does not wait for any modem response or RF delivery acknowledgement before completing (fire-and-forget).

---

### T-1102  UsbEspNowTransport — internal message demux

**Validates:** GW-1100

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup.
3. Inject a `STATUS` message from the mock modem.
4. Inject a `RECV_FRAME` message from the mock modem.
5. Call `Transport::recv()`.
6. Assert: returns the `RECV_FRAME` data (the `STATUS` was handled internally, not surfaced).

---

### T-1103  Startup — RESET then MODEM_READY then SET_CHANNEL

**Validates:** GW-1101

**Procedure:**
1. Create a `UsbEspNowTransport` with a PTY-based `MockModem` configured for channel 6.
2. Assert: mock modem receives `RESET` as the first command.
3. Mock modem sends `MODEM_READY` with a known firmware version and MAC.
4. Assert: mock modem receives `SET_CHANNEL(6)`.
5. Mock modem sends `SET_CHANNEL_ACK(6)`.
6. Assert: startup completes successfully.
7. Assert: modem MAC address is logged.

---

### T-1104  Startup — MODEM_READY timeout

**Validates:** GW-1101

**Procedure:**
1. Create a `UsbEspNowTransport` with a PTY-based `MockModem` that does not send `MODEM_READY`.
2. Assert: startup returns an error after the configured timeout (5 seconds).

---

### T-1104a  Serial disconnect — reconnection with backoff

**Validates:** GW-1103 (criteria 3–5)

**Procedure:**
1. Create a `UsbEspNowTransport` with a PTY-based `MockModem`. Complete startup.
2. Close the mock modem's PTY slave fd to simulate a USB-CDC disconnect.
3. Assert: the serial reader logs a warning (not an error exit).
4. Assert: the transport enters a reconnection loop with exponential backoff.
5. Reopen the PTY slave fd (simulating modem reboot and USB-CDC re-enumeration).
6. Mock modem sends `MODEM_READY`.
7. Assert: the transport re-executes the startup sequence (`RESET` → `MODEM_READY` → `SET_CHANNEL`).
8. Send a `RECV_FRAME` from the mock modem.
9. Assert: `transport.recv()` returns the frame — the gateway did not exit.

### T-1104b  Serial disconnect — frame loop survives reconnection

**Validates:** GW-1103 (criterion 5)

**Procedure:**
1. Start a full gateway instance with a PTY-based `MockModem`.
2. Simulate a modem disconnect by closing the PTY slave fd.
3. Assert: the frame processing loop and BLE event loop do **not** exit.
4. Reconnect the mock modem (reopen PTY, send `MODEM_READY`).
5. Assert: the gateway resumes processing frames normally.

---

### T-1104c  Health poll — sustained failures trigger reconnect

**Validates:** GW-1103 (criterion 6)

**Procedure:**
1. Create a `UsbEspNowTransport` and wrap it in `Arc`. Complete startup.
2. Spawn the health monitor with a short interval (10 ms), `max_consecutive_failures = 3`, and a `Weak` reference to the transport.
3. Drop the server side of the serial connection so that every `poll_status` call fails.
4. Await the health monitor `JoinHandle`.
5. Assert: the monitor returns `true` (reconnect needed).

---

**Validates:** GW-1102

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup.
3. Trigger a health poll (or wait for the periodic interval).
4. Mock modem responds to `GET_STATUS` with `tx_fail_count = 0`.
5. Trigger a second health poll.
6. Mock modem responds to `GET_STATUS` with `tx_fail_count = 5`.
7. Assert: a warning is logged indicating 5 new send failures.

---

### T-1106  Health monitoring — uptime reset detection

**Validates:** GW-1102

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup.
3. First `GET_STATUS` response: `uptime_s = 120`.
4. Second `GET_STATUS` response: `uptime_s = 3`.
5. Assert: a modem reboot event is logged.

---

### T-1107  Modem ERROR handling

**Validates:** GW-1103

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup.
3. Inject an `ERROR(ESPNOW_INIT_FAILED, "test error")` message from the mock modem.
4. Assert: the error code and message are logged.

---

### T-1108  End-to-end wake cycle over PTY

**Validates:** GW-1100, GW-1101

**Procedure:**
1. Create a full gateway instance with `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete modem startup.
3. Register a test node in the gateway.
4. Inject a `RECV_FRAME` containing a valid WAKE from the test node.
5. Assert: the mock modem receives a `SEND_FRAME` containing a valid COMMAND response.
6. Decode the COMMAND and verify it contains a valid `starting_seq` and `timestamp_ms`.

---

### T-1109  RESET recovery after ERROR

**Validates:** GW-1103

**Procedure:**
1. Complete modem startup.
2. Inject an `ERROR(ESPNOW_INIT_FAILED, "test")` message.
3. Assert: error is logged.
4. Mock modem: expect to receive a `RESET` command.
5. Send `MODEM_READY` in response.
6. Mock modem: expect `SET_CHANNEL`.
7. Send `SET_CHANNEL_ACK`.
8. Assert: modem transport is operational again (inject `RECV_FRAME`, call `recv()`, assert frame delivered).

---

## 12  BLE pairing tests

### T-1200  Ed25519 keypair generation on first startup

> **RETIRED (issue #495).** The gateway no longer generates an Ed25519 identity keypair. Phone registration uses a direct PSK exchange; no asymmetric cryptography is required.

---

### T-1201  Gateway ID generation and persistence

> **RETIRED (issue #495).** The gateway no longer generates or persists a `gateway_id`. Identity is established through phone PSKs issued during BLE pairing.

---

### T-1202  Ed25519 to X25519 conversion and low-order rejection

> **RETIRED (issue #495).** X25519 / ECDH key agreement is no longer used. The phone generates the PSK directly and transmits it over the authenticated BLE channel.

---

### T-1203  REQUEST_GW_INFO happy path

> **RETIRED (issue #495).** The `REQUEST_GW_INFO` / `GW_INFO_RESPONSE` exchange has been removed. The simplified BLE pairing flow uses `REGISTER_PHONE` / `PHONE_REGISTERED` only.

---

### T-1204  GW_INFO_RESPONSE signature fails with wrong challenge

> **RETIRED (issue #495).** `GW_INFO_RESPONSE` and Ed25519 signatures have been removed from the BLE pairing flow.

---

### T-1205  REGISTER_PHONE rejected when window closed

**Validates:** GW-1207

**Procedure:**
1. Ensure the registration window is closed.
2. Send `REGISTER_PHONE`.
3. Assert: response is `ERROR` with code `0x02`.

---

### T-1206  Registration window open and auto-close

**Validates:** GW-1208

**Procedure:**
1. Open the registration window via the admin API with a short timeout (e.g., 2 s).
2. Assert: `REGISTER_PHONE` is accepted while the window is open.
3. Wait for the timeout to expire.
4. Assert: `REGISTER_PHONE` now returns `ERROR(0x02)`.

---

### T-1207  REGISTER_PHONE happy path

**Validates:** GW-1209

**Procedure:**
1. Open the registration window.
2. Send `REGISTER_PHONE` containing a phone-generated 256-bit PSK.
3. Assert: response is `PHONE_REGISTERED` with `phone_key_hint` matching `SHA-256(psk)[30..32]`.
4. Assert: the phone PSK is stored with active status.

---

### T-1208  Phone PSK storage, labelling, and revocation

**Validates:** GW-1210

**Procedure:**
1. Register a phone and record the issued PSK.
2. Assert: the PSK is stored with a label, issuance timestamp, and active status.
3. Revoke the phone PSK via operator action.
4. Assert: the PSK status is revoked.
5. Submit a `PEER_REQUEST` authenticated with the revoked PSK.
6. Assert: the request is silently discarded.

---

### T-1209  PEER_REQUEST bypasses key-hint fast-path

**Validates:** GW-1211

**Procedure:**
1. Construct a valid `PEER_REQUEST` frame (`msg_type` `0x05`) with a `key_hint` not in the node registry.
2. Submit the frame to the gateway.
3. Assert: the gateway does not reject the frame at the key-hint lookup stage.
4. Assert: the gateway proceeds to CBOR parsing and decryption.

---

### T-1210  PEER_REQUEST decryption happy path

**Validates:** GW-1212

**Procedure:**
1. Construct a `PEER_REQUEST` with a correctly encrypted `encrypted_payload` (AES-256-GCM using phone PSK, nonce from frame header).
2. Submit the frame.
3. Assert: the gateway successfully decrypts the payload and proceeds to verification steps.

---

### T-1211  PEER_REQUEST with bad GCM tag

**Validates:** GW-1212

**Procedure:**
1. Construct a `PEER_REQUEST` with a valid ciphertext but a corrupted GCM authentication tag.
2. Submit the frame.
3. Assert: the gateway silently discards the frame (no response sent).

---

### T-1212  Phone AEAD with multiple candidates

**Validates:** GW-1211

**Procedure:**
1. Register two phones whose PSKs produce the same `phone_key_hint`.
2. Construct a `PEER_REQUEST` with `encrypted_payload` encrypted using the second phone's PSK.
3. Submit the frame.
4. Assert: the gateway tries both candidate PSKs for AES-256-GCM decryption and accepts the valid one.

---

### T-1213  Phone AEAD with revoked PSK

**Validates:** GW-1211

**Procedure:**
1. Register a phone and then revoke its PSK.
2. Construct a `PEER_REQUEST` with `encrypted_payload` encrypted using the revoked PSK.
3. Submit the frame.
4. Assert: the gateway silently discards the frame (revoked PSK not tried for decryption).

---

### T-1214  PEER_REQUEST frame AEAD verification

**Validates:** GW-0600

**Procedure:**
1. Construct a valid `PEER_REQUEST` with correct AES-256-GCM frame encryption (keyed with `phone_psk`).
2. Submit the frame.
3. Assert: AEAD decryption passes and processing continues.
4. Corrupt the GCM authentication tag.
5. Resubmit.
6. Assert: the gateway silently discards the frame.

---

### T-1215  Timestamp outside ±86 400 s range

**Validates:** GW-1215

**Procedure:**
1. Construct a `PEER_REQUEST` with a timestamp 86 401 s in the past.
2. Submit the frame.
3. Assert: the gateway silently discards the frame.
4. Repeat with a timestamp 86 401 s in the future.
5. Assert: the gateway silently discards the frame.
6. Submit with a timestamp within ±86 400 s.
7. Assert: processing continues.

---

### T-1216  Duplicate node_id handling

**Validates:** GW-1216

**Procedure:**
1. Successfully pair a node with `node_id` X and `node_psk` P.
2. Construct a new `PEER_REQUEST` with the same `node_id` X and matching `node_psk` P.
3. Submit the frame.
4. Assert: the gateway returns a valid `PEER_ACK(0x00)` (duplicate with matching PSK — GW-1216 AC2).
5. Construct a new `PEER_REQUEST` with the same `node_id` X but a **different** `node_psk`.
6. Submit the frame.
7. Assert: the gateway silently discards the frame (different PSK — GW-1216 AC3).

---

### T-1217  Key hint mismatch rejected

**Validates:** GW-1217

**Procedure:**
1. Construct a `PEER_REQUEST` where the frame header `key_hint` differs from the `node_key_hint` in the CBOR payload.
2. Submit the frame.
3. Assert: the gateway silently discards the frame.

---

### T-1218  Node registration stores correct fields

**Validates:** GW-1218

**Procedure:**
1. Successfully process a `PEER_REQUEST` from a known phone.
2. Query the node registry for the new node.
3. Assert: the record contains `node_id`, `node_key_hint`, `node_psk`, `rf_channel`, `sensors`, and `registered_by` set to the phone's stable identifier (not `phone_key_hint`).

### T-1218a  Duplicate PEER_REQUEST with matching PSK sends PEER_ACK

**Validates:** GW-1218 (criterion 4)

**Procedure:**
1. Successfully process a `PEER_REQUEST` — node is registered, PEER_ACK sent.
2. Submit a second `PEER_REQUEST` with the same `node_id` and `node_psk` but a different nonce.
3. Assert: a `PEER_ACK(0x00)` is returned.
4. Assert: the `nonce` in the PEER_ACK header matches the second request's nonce.
5. Assert: the node registry still contains exactly one record for the node (no duplicate).

### T-1218b  Duplicate PEER_REQUEST with different PSK is discarded

**Validates:** GW-1218 (criterion 5)

**Procedure:**
1. Successfully process a `PEER_REQUEST` — node is registered.
2. Submit a second `PEER_REQUEST` with the same `node_id` but a **different** `node_psk`.
3. Assert: no `PEER_ACK` is sent (silent discard).
4. Assert: the existing node record is unchanged.

---

### T-1219  PEER_ACK happy path

**Validates:** GW-1219

**Procedure:**
1. Submit a valid `PEER_REQUEST` with nonce N.
2. Receive the `PEER_ACK` response.
3. Assert: the `PEER_ACK` CBOR is `{1: 0}` (status code only, no `registration_proof`).
4. Assert: the frame is AES-256-GCM encrypted under `node_psk` with the nonce from the frame header.
5. Assert: the `nonce` in the `PEER_ACK` header equals N.

---

### T-1220  PEER_REQUEST/PEER_ACK use random nonces

**Validates:** GW-1220, GW-1221

> **Note:** This test also validates GW-1220 (silent-discard error model) by asserting that the gateway never sends an error response for any malformed or invalid `PEER_REQUEST` — only valid requests produce a `PEER_ACK`. Individual pipeline-stage discards are exercised by T-1210 through T-1219.

**Procedure:**
1. Submit a `PEER_REQUEST` with a random nonce (not a sequential number).
2. Assert: the gateway does not reject the frame for sequence-number violations.
3. Assert: the `PEER_ACK` echoes the random nonce, not a gateway-assigned sequence number.

---

### T-1221  Admin BLE pairing session

**Validates:** GW-1222

**Procedure:**
1. Call `OpenBlePairing` via admin API.
2. Assert: registration window is open.
3. Assert: `BLE_ENABLE` sent to modem.
4. Wait for window timeout.
5. Assert: `BLE_DISABLE` sent to modem.
6. Assert: registration window is closed.

---

### T-1222  Numeric Comparison passkey display

**Validates:** GW-1222

**Procedure:**
1. Start a BLE pairing session via admin API (`OpenBlePairing`).
2. Connect phone via BLE. Modem sends `BLE_PAIRING_CONFIRM(passkey=123456)`.
3. Assert: gateway forwards the passkey to the admin API client (e.g., as a streaming gRPC event or CLI prompt).
4. Admin client accepts. Assert: gateway sends `BLE_PAIRING_CONFIRM_REPLY(0x01)` to modem.

> **Note:** In automated integration tests, run `sonde-admin pairing start` against a mock modem that injects `BLE_PAIRING_CONFIRM`, capture stdout, and assert the passkey appears. Operator confirmation is simulated by piping `y` to stdin.

---

### T-1223  Ed25519 seed replication

> **RETIRED (issue #495).** Ed25519 identity and `gateway_id` have been removed. State replication is covered by T-1002 (export/import round-trip) and T-1005b.

---

### T-1224  BLE GATT server via modem relay

**Validates:** GW-1204

**Procedure:**
1. Complete modem startup.
2. Using a BLE test client, scan for the modem and connect to its GATT server.
3. Discover services and assert: the Gateway Pairing Service UUID matches the value specified for GW-1204 in `ble-pairing-protocol.md`.
4. Within the Gateway Pairing Service, discover characteristics and assert: the request/command and indication/response characteristic UUIDs match the values specified for GW-1204.
5. Open a BLE pairing session via the admin API.
6. Mock modem: inject a `BLE_RECV` message containing a `REGISTER_PHONE` command on the request characteristic.
7. Assert: gateway processes the command and sends a `BLE_INDICATE` message to the modem on the indication characteristic containing a valid `PHONE_REGISTERED` response.
8. Decode the indication payload and verify it contains `phone_key_hint`.

---

### T-1225  ATT MTU and fragmentation via modem relay

**Validates:** GW-1205

**Procedure:**
1. Complete modem startup.
2. Open BLE pairing session.
3. Assert: when the gateway sends a `BLE_INDICATE` message, the payload is a complete BLE envelope (the modem handles fragmentation per MD-0403).
4. Arrange for the gateway to emit a BLE envelope whose payload exceeds `(ATT_MTU - 3)` bytes (for example, more than 244 bytes when the negotiated ATT MTU is 247), using either (a) a variable-length message type (for example, an `ERROR` with a long diagnostic string) or (b) a test-only response that includes explicit padding bytes for this validation.
5. Assert: the gateway sends the oversized envelope in a single `BLE_INDICATE` message to the modem (delegation model — modem fragments, not gateway).

---

### T-1226  BLE_ENABLE/BLE_DISABLE signals on window open/close

**Validates:** GW-1208

**Procedure:**
1. Open the registration window via admin API.
2. Assert: mock modem receives a `BLE_ENABLE` message.
3. Close the registration window explicitly via admin API.
4. Assert: mock modem receives a `BLE_DISABLE` message.
5. Open the window again with a 2s timeout.
6. Wait for auto-close.
7. Assert: mock modem receives `BLE_ENABLE` then `BLE_DISABLE` in order.

---

### T-1227  Phone listing via admin API

**Validates:** GW-1223

**Procedure:**
1. Register two phones via the BLE pairing flow.
2. Call `ListPhones` via admin API.
3. Assert: both phones appear with correct metadata (phone ID, key hint, label, issue time).
4. Revoke one phone.
5. Call `ListPhones` again.
6. Assert: revoked phone shows revoked status.

---

### T-1228  Phone revocation via admin API

**Validates:** GW-1224

**Procedure:**
1. Register a phone via the BLE pairing flow.
2. Call `RevokePhone` with the phone's ID.
3. Assert: success response.
4. Submit a `PEER_REQUEST` with `encrypted_payload` encrypted using the revoked phone PSK.
5. Assert: gateway silently discards the request (AEAD decryption fails — revoked PSK not tried, per GW-1211).

---

## 13  Operational logging tests

### T-1300  WAKE lifecycle logging

**Validates:** GW-1300

**Procedure:**
1. Configure a gateway with `tracing-test` / `#[traced_test]`.
2. Register a test node.
3. Submit a valid WAKE frame for the node.
4. Assert: an `INFO`-level log entry is emitted containing the node's `node_id`, `seq` (starting sequence number), and `battery_mv`.
5. Assert: an `INFO`-level log entry is emitted for session creation with the node's `node_id`.
6. Assert: an `INFO`-level log entry is emitted for COMMAND selected with the node's `node_id` and `command_type`.

---

### T-1301  Session expiry logging

**Validates:** GW-1300

**Procedure:**
1. Configure a gateway with a very short session timeout (e.g., 1 ms) and `#[traced_test]`, and run the test under a deterministic clock (for example, using `tokio::time::pause()` + `tokio::time::advance()` or an injected fake clock).
2. Register a test node and submit a valid WAKE to create a session.
3. Advance the test clock until the session timeout has elapsed (e.g., by at least the configured timeout plus a small delta) so that the session is considered expired.
4. Call `reap_expired()` on the session manager.
5. Assert: an `INFO`-level log entry is emitted for session expiry with the node's `node_id`.

---

### T-1302  PEER_REQUEST logging

**Validates:** GW-1300

**Procedure:**
1. Configure a gateway with `#[traced_test]`.
2. Set up phone trust for BLE pairing.
3. Submit a valid `PEER_REQUEST` frame.
4. Assert: an `INFO`-level log entry is emitted with `node_id`, `key_hint`, and `result` = `"registered"`.
5. Assert: an `INFO`-level log entry is emitted for PEER_ACK frame encoded with `node_id`.

---

### T-1303  Modem frame debug logging

**Validates:** GW-1302

**Procedure:**
1. Configure a `UsbEspNowTransport` with `#[traced_test]` at `DEBUG` level.
2. Inject a `RECV_FRAME` from the mock modem.
3. Assert: a `DEBUG`-level log entry is emitted with fields `msg_type`, `peer_mac`, and `len`.
4. Call `Transport::send(frame, peer_mac)`.
5. Assert: a `DEBUG`-level log entry is emitted with fields `msg_type`, `peer_mac`, and `len`.

---

### T-1304  Build metadata in `--version` output

**Validates:** GW-1303

**Procedure:**
1. Build `sonde-gateway` and `sonde-admin` from a git checkout.
2. Run `sonde-gateway --version`.
3. Assert: output matches the pattern `sonde-gateway <semver> (<7-char-hash>)`.
4. Run `sonde-admin --version`.
5. Assert: output matches the pattern `sonde-admin <semver> (<7-char-hash>)`.
6. Assert: the hash portion is a valid 7-character hex string (or `unknown` when built outside a git repo).

---

### T-1305a  Verification failure includes instruction-level diagnostics

**Validates:** GW-1305

**Procedure:**
1. Ingest a BPF ELF that triggers a Prevail forward-analysis failure (e.g. an invalid helper call or type violation).
2. Assert: the gRPC error message contains at least one instruction-level diagnostic line from the verifier.
3. Assert: the diagnostic includes verifier-specific context (e.g. type mismatch description, register state).

---

### T-1305b  Successful verification produces no diagnostics

**Validates:** GW-1305

**Procedure:**
1. Ingest a valid BPF ELF that passes Prevail verification.
2. Assert: the success response contains no diagnostic messages.
3. Assert: the program is stored and retrievable by hash.

---

### T-1306a  File sink writes to `<db-path>.log`

**Validates:** GW-1306

**Procedure:**
1. Start the gateway in service mode with database path `test.db`.
2. Trigger a log event (e.g., register a node).
3. Assert: `test.log` exists and contains the logged event.

### T-1306b  ETW provider registered

**Validates:** GW-1306

**Procedure:**
1. Start the gateway in service mode on Windows.
2. Query ETW providers for `sonde-gateway`.
3. Assert: the provider is registered and active.

### T-1306c  Runtime log-level reload

**Validates:** GW-1306

**Procedure:**
1. Start the gateway in service mode with default log level (`sonde_gateway=warn`).
2. Set `RUST_LOG=sonde_gateway=debug` and send the reload signal.
3. Within 5 seconds, trigger a debug-level event.
4. Assert: the debug event appears in the log file.

### T-1306d  File sink failure — graceful degradation

**Validates:** GW-1306

**Procedure:**
1. Configure the gateway with a database path in a non-writable directory.
2. Start the gateway.
3. Assert: the gateway starts successfully (does not crash).
4. Assert: an ERROR-level diagnostic is emitted to the ETW sink indicating the log file could not be opened.

### T-1307a  IngestProgram empty image includes operation and guidance

**Validates:** GW-1307

**Procedure:**
1. Call `IngestProgram` with an empty byte slice.
2. Assert: the gRPC error message contains the operation name (e.g., `"IngestProgram"` or `"ingest"`).
3. Assert: the error message contains actionable guidance.

### T-1307b  AssignProgram missing program includes hash and guidance

**Validates:** GW-1307

**Procedure:**
1. Call `AssignProgram` with a `program_hash` that does not exist in storage.
2. Assert: the error message includes the program hash.
3. Assert: the error message includes guidance (e.g., `"ingest"` or `"upload"`).

### T-1307c  Key provider missing file includes path and guidance

**Validates:** GW-1307

**Procedure:**
1. Create a `FileKeyProvider` pointing to a nonexistent path.
2. Attempt to load the key.
3. Assert: the error message includes the file path.
4. Assert: the error message includes guidance for creating the key file.

### T-1307d  Key provider wrong length includes expected vs actual

**Validates:** GW-1307

**Procedure:**
1. Call `parse_hex_key` with a hex string shorter than 64 characters.
2. Assert: the error includes expected and actual character counts.

### T-1307e  EnvKeyProvider not set includes variable name and guidance

**Validates:** GW-1307

**Procedure:**
1. Create an `EnvKeyProvider` referencing a nonexistent environment variable.
2. Attempt to load the key.
3. Assert: the error includes the variable name and guidance.

### T-1307f  SqliteStorage open failure includes path and guidance

**Validates:** GW-1307

**Procedure:**
1. Call `SqliteStorage::open` with an invalid directory path.
2. Assert: the error message includes the path and guidance about directory permissions.

### T-1307g  Import state decryption failure includes guidance

**Validates:** GW-1307

**Procedure:**
1. Call `import_state` with garbage data.
2. Assert: the error includes variant-specific guidance (e.g., about passphrase or corruption).

### T-1307h  Export state empty passphrase includes guidance

**Validates:** GW-1307

**Procedure:**
1. Call `export_state` with an empty passphrase.
2. Assert: the error includes operation context and guidance.

### T-1307i  QueueEphemeral with wrong profile includes hash and profile

**Validates:** GW-1307

**Procedure:**
1. Ingest a program with the resident profile.
2. Call `QueueEphemeral` with that program's hash.
3. Assert: the error includes the program hash and profile.

### T-1308  APP_DATA handler pipeline logging

**Validates:** GW-1308

**Procedure:**
1. Register a handler process (e.g., a Python echo script) with `program_hash = "*"`.
2. Simulate a node sending APP_DATA with a known payload.
3. Wait for the handler to reply and exit.
4. Capture tracing output.
5. Assert: an INFO log with `"APP_DATA received"` includes `node_id`, `program_hash`, and `len` fields (AC1).
6. Assert: an INFO log with `"handler matched"` includes `program_hash` and `command` fields (AC2).
7. Assert: an INFO log with `"handler invoked"` includes the `command` field (AC3).
8. Assert: an INFO log with `"handler replied"` includes the `len` field (AC4).
9. Assert: a log with `"handler exited"` includes the `code` field (AC5).
10. Simulate a node with `current_program_hash = None` sending APP_DATA.
11. Assert: a WARN log with `"APP_DATA dropped"` includes `node_id` and indicates missing `current_program_hash` (AC6).
12. Simulate a node whose `current_program_hash` does not match any handler.
13. Assert: a WARN log with `"APP_DATA dropped"` includes `node_id`, `program_hash`, and `handler_count` (AC6).
14. Register a handler whose stderr produces output (e.g., a script that writes to stderr on startup).
15. Assert: the handler's stderr lines appear in the gateway log at WARN level, tagged with the handler command (AC7).

---

### T-1400  Handler storage CRUD

**Validates:** GW-1401

**Procedure:**
1. Create an in-memory `SqliteStorage` instance.
2. Call `add_handler` with `program_hash` = `"*"`, `command` = `"python"`, `args` = `["handler.py"]`, `working_dir` = `None`.
3. Assert: `list_handlers` returns one record matching the inserted values.
4. Call `add_handler` with the same `program_hash` `"*"`.
5. Assert: returns `Ok(false)` (duplicate detected without creating a new row, consistent with the `insert_node_if_not_exists` pattern).
6. Assert: `list_handlers` still returns one record.
7. Call `add_handler` with a valid 64-char hex `program_hash`.
8. Assert: returns `Ok(true)` and `list_handlers` returns two records.
9. Call `remove_handler` with the hex `program_hash`.
10. Assert: returns `true` and `list_handlers` returns one record.
11. Call `remove_handler` with a non-existent `program_hash`.
12. Assert: returns `false`.

---

### T-1401  Handler CRUD via admin API

**Validates:** GW-1402

**Procedure:**
1. Start gateway with no handlers configured.
2. Call `ListHandlers` via gRPC.
3. Assert: response contains zero handlers.
4. Call `AddHandler` with `program_hash` = `"*"`, `command` = `"echo"`, `reply_timeout_ms` = `5000`.
5. Assert: success response.
6. Call `ListHandlers`.
7. Assert: response contains one handler with matching fields (including `reply_timeout_ms` = `5000`).
8. Call `AddHandler` with the same `program_hash` = `"*"`.
9. Assert: gRPC status `ALREADY_EXISTS`.
10. Call `RemoveHandler` with `program_hash` = `"*"`.
11. Assert: success response and `ListHandlers` returns zero handlers.
12. Call `RemoveHandler` with `program_hash` = `"*"` again.
13. Assert: gRPC status `NOT_FOUND`.

---

### T-1402  Handler persistence across restart

**Validates:** GW-1401

**Procedure:**
1. Start a gateway with a file-backed `SqliteStorage`.
2. Call `AddHandler` with `program_hash` = `"*"`, `command` = `"python"`, `args` = `["handler.py"]`.
3. Stop the gateway.
4. Restart the gateway with the same database file.
5. Call `ListHandlers`.
6. Assert: the handler added in step 2 is present with identical configuration.

---

### T-1403  Live reload — handler add

**Validates:** GW-1404

**Procedure:**
1. Start gateway with no handlers configured. Register a test node with a known `program_hash`.
2. Complete a WAKE handshake and send `APP_DATA`.
3. Assert: no `APP_DATA_REPLY` is sent (no handler matched).
4. Call `AddHandler` with the node's `program_hash` and a test echo handler command.
5. Complete another WAKE handshake and send `APP_DATA`.
6. Assert: `APP_DATA_REPLY` is received (the newly added handler processed the request).

---

### T-1404  Live reload — handler remove

**Validates:** GW-1404

**Procedure:**
1. Start gateway with a catch-all handler (`program_hash` = `"*"`). Register a test node.
2. Complete a WAKE handshake and send `APP_DATA`.
3. Assert: `APP_DATA_REPLY` is received (handler matched).
4. Call `RemoveHandler` with `program_hash` = `"*"`.
5. Complete another WAKE handshake and send `APP_DATA`.
6. Assert: no `APP_DATA_REPLY` is sent (handler removed).
7. Assert: the handler process from step 2 is no longer running.

---

### T-1405  Bootstrap from YAML file

**Validates:** GW-1405

**Procedure:**
1. Create a temporary `handlers.yaml` with two entries: a catch-all (`"*"`) and a specific hex hash.
2. Start gateway with `--handler-config handlers.yaml` and an empty database.
3. Call `ListHandlers`.
4. Assert: both handlers from the YAML file are present.
5. Call `RemoveHandler` for the hex-hash entry.
6. Restart gateway with `--handler-config handlers.yaml` and the same database.
7. Call `ListHandlers`.
8. Assert: both handlers are present (the hex-hash entry was re-imported from YAML) and the catch-all was not duplicated.

---

### T-1405a  Bootstrap with invalid YAML entry

**Validates:** GW-1405

**Procedure:**
1. Create a `handlers.yaml` with one valid entry and one entry containing a malformed `program_hash` (e.g., `"not-a-hex-string"`).
2. Start gateway with `--handler-config handlers.yaml`.
3. Assert: the gateway starts successfully.
4. Assert: a warning is logged for the invalid entry.
5. Call `ListHandlers`.
6. Assert: only the valid entry was imported.

---

### T-1406  State export/import with handlers

**Validates:** GW-1406

**Procedure:**
1. Start gateway A. Add two handlers via `AddHandler`, configuring each with a distinct, non-default `reply_timeout_ms` value (for example, 5000 and 30000).
2. Call `ExportState` with a test passphrase.
3. Start gateway B with an empty database and different handlers.
4. Call `ImportState` on gateway B with the bundle from step 2.
5. Call `ListHandlers` on gateway B.
6. Assert: gateway B has exactly the two handlers from gateway A (the pre-existing handlers were replaced), and each handler's `reply_timeout_ms` matches the value configured in step 1 (non-default timeouts round-trip through `ExportState`/`ImportState`).

---

### T-1406a  State import — backwards compatibility

**Validates:** GW-1406

**Procedure:**
1. Start a gateway with two handlers configured.
2. Import a state bundle that was exported from an older gateway version (no handler records in the bundle).
3. Call `ListHandlers`.
4. Assert: the two pre-existing handlers are preserved (not deleted).

---

### T-1407  Handler add — program_hash validation

**Validates:** GW-1402

**Procedure:**
1. Call `AddHandler` with `program_hash` = `"invalid"`.
2. Assert: gRPC status `INVALID_ARGUMENT`.
3. Call `AddHandler` with `program_hash` = `"AABB"` (too short).
4. Assert: gRPC status `INVALID_ARGUMENT`.
5. Call `AddHandler` with `program_hash` = 64-char hex string.
6. Assert: success.
7. Call `AddHandler` with `program_hash` = `"*"`.
8. Assert: success.

---

### T-1407a  HandlerRouter always initialized

**Validates:** GW-1407

**Procedure:**
1. Start gateway without `--handler-config` and with an empty database (no handlers).
2. Assert: the gateway starts successfully and the `HandlerRouter` is initialized (not `None`).
3. Call `AddHandler` with a catch-all (`"*"`) and a test echo handler command.
4. Send `APP_DATA` for any program hash.
5. Assert: `APP_DATA_REPLY` is received (the handler processed the request without restart).

---

### T-1407b  HandlerRouter shared between engine and admin

**Validates:** GW-1407

**Procedure:**
1. Start gateway with one handler pre-loaded in the database.
2. Send `APP_DATA` matching the handler's `program_hash`.
3. Assert: handler receives the DATA message (engine reads from shared router).
4. Call `RemoveHandler` via admin API.
5. Send `APP_DATA` again.
6. Assert: no `APP_DATA_REPLY` (admin wrote to the same shared router the engine reads).

---

### T-1405b  Bootstrap builds router from database

**Validates:** GW-1405, GW-1407

**Procedure:**
1. Create a temporary `handlers.yaml` with a catch-all handler (`"*"`) and a test echo handler command.
2. Start gateway with `--handler-config handlers.yaml` and an empty database.
3. Send `APP_DATA` for any program hash.
4. Assert: `APP_DATA_REPLY` is received (handler routed via DB-built router).
5. Call `RemoveHandler` for `"*"` via admin API.
6. Send `APP_DATA` again.
7. Assert: no `APP_DATA_REPLY` is sent (router was built from DB, and admin removal updated the shared router — proving the YAML was a seed, not the routing source).

---

### T-1406b  State import triggers HandlerRouter reload

**Validates:** GW-1404, GW-1406

**Procedure:**
1. Start gateway A with no handlers. Add a catch-all handler via `AddHandler` with a test echo handler command.
2. Call `ExportState` with a test passphrase.
3. Start gateway B with no handlers configured and an empty database.
4. Send `APP_DATA` on gateway B.
5. Assert: no `APP_DATA_REPLY` (no handlers).
6. Call `ImportState` on gateway B with the bundle from step 2.
7. Send `APP_DATA` on gateway B.
8. Assert: `APP_DATA_REPLY` is received (imported handler is immediately routable without restart).

---

## 14  Installer and service management tests

### T-1500  MSI adds PATH entry

**Validates:** GW-1500

**Procedure:**
1. Install the MSI on a clean Windows VM.
2. Open a new PowerShell window (not the same session used for installation).
3. Run `sonde-gateway --version`.
4. Assert: the command succeeds and prints a version string.
5. Run `$env:PATH -split ';' | Where-Object { $_ -match 'Sonde' }`.
6. Assert: exactly one entry matches and it points to the installed `bin` directory.
7. Uninstall the MSI.
8. Open a new PowerShell window.
9. Assert: the `Sonde\bin` entry is no longer present in `$env:PATH`.

---

### T-1501  `sonde-gateway install` registers Windows service

**Validates:** GW-1501

**Procedure:**
1. On a Windows machine with the gateway binary on PATH, open an elevated PowerShell prompt.
2. Run `sonde-gateway install --port COM5 --db C:\ProgramData\sonde\gateway.db --master-key-file C:\ProgramData\sonde\master-key.hex`.
3. Assert: the command exits with code 0 and prints a success message.
4. Run `sc.exe qc sonde-gateway`.
5. Assert: the service exists with `START_TYPE` = `AUTO_START`.
6. Assert: the `BINARY_PATH_NAME` includes `--port COM5`, `--db`, and `--master-key-file` flags.
7. Run `sonde-gateway install --port COM6 --db C:\ProgramData\sonde\gateway.db --master-key-file C:\ProgramData\sonde\master-key.hex`.
8. Assert: the command exits with code 0 (idempotent update).
9. Run `sc.exe qc sonde-gateway`.
10. Assert: `BINARY_PATH_NAME` now includes `--port COM6`.

---

### T-1502  `sonde-gateway uninstall` removes Windows service

**Validates:** GW-1502

**Procedure:**
1. Prerequisite: a service registered via `sonde-gateway install` (see T-1501).
2. Start the service: `sc.exe start sonde-gateway`.
3. Run `sonde-gateway uninstall` from an elevated prompt.
4. Assert: the command exits with code 0.
5. Run `sc.exe query sonde-gateway`.
6. Assert: the service is not found (exit code indicates failure).
7. Assert: the database file and master key file still exist on disk.
8. Run `sonde-gateway uninstall` again.
9. Assert: the command exits with code 0 and prints an informational "not registered" message.

---

### T-1503  Service starts and connects to modem on boot

**Validates:** GW-1501, GW-1502

**Procedure:**
1. Register the service via `sonde-gateway install --port <MODEM_PORT> --db <DB_PATH> --master-key-file <KEY_PATH>`.
2. Reboot the machine (or restart the service: `sc.exe start sonde-gateway` on Windows, `systemctl start sonde-gateway` on Linux).
3. Assert: the service reaches `RUNNING` state within 30 seconds.
4. Assert: the gateway log contains `"modem transport ready"`.
5. Stop the service.
6. Assert: the service stops cleanly within 10 seconds.

---

### T-1504  Linux `.deb` installs and enables systemd service

**Validates:** GW-1503

**Procedure:**
1. Install the `.deb` package on a clean Debian/Ubuntu VM: `sudo dpkg -i sonde_<VERSION>_amd64.deb`.
2. Assert: the `sonde` user and group exist (`getent passwd sonde` succeeds).
3. Assert: the `sonde` user is a member of the `dialout` group.
4. Assert: `/lib/systemd/system/sonde-gateway.service` exists.
5. Assert: `/etc/sonde/environment` exists and contains `SERIAL_PORT=/dev/ttyUSB0`.
6. Assert: `systemctl is-enabled sonde-gateway.service` returns `enabled` (the `postinst` script enables the unit).
7. Edit `/etc/sonde/environment` to set the correct serial port if it differs from `/dev/ttyUSB0`.
8. Run `systemctl start sonde-gateway`.
9. Assert: `systemctl is-active sonde-gateway.service` returns `active`.
10. Assert: the gateway log contains `"modem transport ready"`.
11. Remove the package: `sudo dpkg -r sonde`.
12. Assert: the service is stopped and disabled.
13. Assert: `/var/lib/sonde/gateway.db` is preserved (not deleted by removal).

---

## 15  App bundle deployment

### T-1600  Deploy valid bundle

**Traces to:** GW-1600

**Preconditions:** Gateway running with no programs, handlers, or nodes matching the bundle. At least one node in the bundle must be registered in the gateway.

**Steps:**
1. Create a valid `.sondeapp` bundle with one program (`temp-reader`, resident), one handler (python3, `handler/ingest.py`), and two nodes (`sensor-1`, `sensor-2`).
2. Register nodes `sensor-1` and `sensor-2` in the gateway.
3. Run `sonde-admin deploy <bundle-path>`.

**Expected:**
1. Exit code 0.
2. `sonde-admin program list` shows the ingested program.
3. `sonde-admin handler list` shows the configured handler.
4. `sonde-admin node get sensor-1` shows `assigned_program_hash` matching the program.
5. `sonde-admin node get sensor-2` shows `assigned_program_hash` matching the program.
6. Output includes deploy summary with counts.

---

### T-1601  Idempotent re-deploy

**Traces to:** GW-1601

**Preconditions:** T-1600 completed successfully (bundle already deployed).

**Steps:**
1. Run `sonde-admin deploy <bundle-path>` again with the same bundle.

**Expected:**
1. Exit code 0.
2. Output shows all steps as "skipped (already ingested/configured/assigned)".
3. Gateway state is unchanged from after T-1600.

---

### T-1601a  Deploy with handler config mismatch

**Traces to:** GW-1601 (AC-5)

**Preconditions:** Bundle deployed. Then handler for the same program hash is manually changed via `sonde-admin handler remove` + `handler add` with different args.

**Steps:**
1. Deploy the bundle initially.
2. Manually remove and re-add the handler with different args.
3. Run `sonde-admin deploy <bundle-path>` again.

**Expected:**
1. Exit code 0.
2. Warning printed about handler config mismatch.
3. The manually configured handler is NOT overwritten.

---

### T-1602  Deploy with unregistered node

**Traces to:** GW-1600

**Preconditions:** Gateway running. Bundle references node `unknown-node` which is NOT registered.

**Steps:**
1. Create a bundle targeting node `unknown-node`.
2. Run `sonde-admin deploy <bundle-path>`.

**Expected:**
1. Program ingestion and handler configuration succeed.
2. Node assignment for `unknown-node` warns "node not registered" and continues.
3. Exit code 0 (warning, not failure).

---

### T-1603  Undeploy removes handlers

**Traces to:** GW-1602

**Preconditions:** Bundle from T-1600 is deployed.

**Steps:**
1. Run `sonde-admin undeploy <bundle-path>`.

**Expected:**
1. Exit code 0.
2. `sonde-admin handler list` no longer shows the bundle's handler.
3. Nodes are still assigned (warning printed about each).
4. Programs are still in the library (not removed without `--remove-programs`).

---

### T-1603a  Undeploy preserves non-bundle resources

**Traces to:** GW-1602 (AC-6)

**Preconditions:** Bundle deployed. A separate handler (not in the bundle) is registered via `sonde-admin handler add`.

**Steps:**
1. Deploy the bundle.
2. Register a non-bundle handler: `sonde-admin handler add <other-hash> other-command`.
3. Run `sonde-admin undeploy <bundle-path>`.

**Expected:**
1. The bundle's handler is removed.
2. The non-bundle handler is still present in `sonde-admin handler list`.
3. Any non-bundle programs and nodes are unaffected.

---

### T-1604  Undeploy with --remove-programs

**Traces to:** GW-1602

**Preconditions:** Bundle deployed, nodes have been unassigned manually.

**Steps:**
1. Unassign nodes from the bundle's program.
2. Run `sonde-admin undeploy <bundle-path> --remove-programs`.

**Expected:**
1. Exit code 0.
2. Handlers removed.
3. Programs removed from library.
4. `sonde-admin program list` no longer shows the bundle's program.

---

### T-1605  Undeploy refuses to remove assigned programs

**Traces to:** GW-1602

**Preconditions:** Bundle deployed, nodes still assigned.

**Steps:**
1. Run `sonde-admin undeploy <bundle-path> --remove-programs`.

**Expected:**
1. Handlers removed.
2. Programs NOT removed (still assigned to nodes).
3. Warning printed: "program `<hash>` is still assigned to node(s): sensor-1, sensor-2".

---

### T-1605a  Undeploy with --force removes assigned programs

**Traces to:** GW-1602 (AC-5)

**Preconditions:** Bundle deployed, nodes still assigned to bundle programs.

**Steps:**
1. Run `sonde-admin undeploy <bundle-path> --remove-programs --force`.

**Expected:**
1. Handlers removed.
2. Nodes are unassigned from bundle programs first.
3. Programs are removed from the library.
4. `sonde-admin program list` no longer shows the bundle's program.
5. `sonde-admin node get sensor-1` shows no assigned program.

---

### T-1606  Validate command — offline

**Traces to:** GW-1603

**Steps:**
1. Stop the gateway.
2. Run `sonde-admin validate <bundle-path>` with a valid bundle.

**Expected:**
1. Exit code 0 (no gateway connection required).
2. Output indicates bundle is valid.

---

### T-1606a  Validate command — invalid bundle

**Traces to:** GW-1603 (AC-2)

**Steps:**
1. Create a `.sondeapp` bundle with a missing ELF file (program path doesn't exist).
2. Run `sonde-admin validate <bundle-path>`.

**Expected:**
1. Exit code non-zero.
2. Stderr includes "program file not found" validation error.

---

### T-1607  Deploy dry-run

**Traces to:** GW-1604

**Preconditions:** Gateway running, bundle not yet deployed.

**Steps:**
1. Run `sonde-admin deploy --dry-run <bundle-path>`.

**Expected:**
1. Exit code 0.
2. Output lists actions that WOULD be taken (ingest, add handler, assign).
3. `sonde-admin program list` shows NO new programs (nothing was actually ingested).
4. `sonde-admin handler list` shows NO new handlers.

---

### T-1608  Deploy with gateway unreachable

**Traces to:** GW-1600 (AC-4)

**Preconditions:** Gateway is NOT running.

**Steps:**
1. Create a valid `.sondeapp` bundle.
2. Run `sonde-admin deploy <bundle-path>`.

**Expected:**
1. Exit code non-zero.
2. Error message indicates connection failure (e.g., "failed to connect to gateway").
3. The error identifies the failing step (program ingestion, since that is the first gRPC call).

---

| GW-1306 | T-1306a, T-1306b, T-1306c, T-1306d |
| GW-1307 | T-1307a, T-1307b, T-1307c, T-1307d, T-1307e, T-1307f, T-1307g, T-1307h, T-1307i |
| GW-1308 | T-1308 |
| GW-1401 | T-1400, T-1402 |
| GW-1402 | T-1401, T-1407 |
| GW-1403 | *(validated via manual CLI UX validation procedure)* |
| GW-1404 | T-1403, T-1404 |
| GW-1405 | T-1405, T-1405a |
| GW-1406 | T-1406, T-1406a |
| GW-1407 | T-1407a, T-1407b, T-1405b |
| GW-1500 | T-1500 |
| GW-1501 | T-1501, T-1503 |
| GW-1502 | T-1502, T-1503 |
| GW-1503 | T-1504 |
| GW-1600 | T-1600, T-1602, T-1608 |
| GW-1601 | T-1601, T-1601a |
| GW-1602 | T-1603, T-1603a, T-1604, T-1605, T-1605a |
| GW-1603 | T-1606, T-1606a |
| GW-1604 | T-1607 |
