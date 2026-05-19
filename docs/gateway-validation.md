<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Validation Specification

> **Document status:** Draft  
> **Scope:** Integration and system-level test plan for the Sonde gateway.  
> **Audience:** Implementers (human or LLM agent) writing gateway tests.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [gateway-design.md](gateway-design.md), [gateway-companion-api.md](gateway-companion-api.md), [protocol.md](protocol.md)

---

## 1  Overview

This document defines integration test cases that validate the gateway against the requirements in [gateway-requirements.md](gateway-requirements.md). Each test case is traceable to one or more requirements.

**Scope:** These are integration tests that exercise the gateway through its external interfaces (transport, handler I/O, and local gRPC APIs). Unit tests for internal modules are expected but are not specified here.

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
1. Send a WAKE with `firmware_abi_version=1`, `program_hash=<known_hash>`, `battery_mv=3300`, `firmware_version="0.7.0"`.
2. Assert: gateway responds with a COMMAND.
3. Assert: the node's durable registry entry is updated with the received `firmware_abi_version` and `firmware_version`.
4. Assert: the gateway's runtime node-observation state records `battery_mv = 3300`.

---

### T-0104  WAKE with missing fields

**Validates:** GW-0102

**Procedure:**
1. Send a WAKE missing `battery_mv` (valid AEAD, valid header).
2. Assert: gateway discards the frame (no COMMAND response).
3. Send a WAKE missing `firmware_version` (valid AEAD, valid header).
4. Assert: gateway discards the frame (no COMMAND response).

---

### T-0105  COMMAND response structure

**Validates:** GW-0103

**Procedure:**
1. Send a valid WAKE.
2. Capture all COMMAND responses until the wake cycle ends (i.e., until the next WAKE is sent or the session is torn down).
3. Assert: exactly one COMMAND is received for this WAKE (no duplicates).
4. Assert: response header `nonce` matches the WAKE nonce.
5. Assert: CBOR payload contains `command_type`, `starting_seq`, and `timestamp_ms`.
6. Assert: `timestamp_ms` is a reasonable UTC value (within 5 seconds of test clock).
7. Send at least 4 additional WAKEs, each with a distinct nonce, collecting all `starting_seq` values.
8. Assert: no two `starting_seq` values are identical (fresh value each time).
9. Assert: the consecutive deltas between `starting_seq` values are not all equal (rules out simple incrementing counters).

---

### T-0106  Frame size constraint

**Validates:** GW-0104

**Procedure:**
1. Register a program with chunks that approach the frame size limit.
2. Trigger a chunked transfer.
3. Capture all outbound CHUNK frames.
4. Assert: every outbound frame ≤ 250 bytes.
5. Assert: the ciphertext region of every outbound frame (frame length minus 11-byte header minus 16-byte GCM tag) ≤ 223 bytes.

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
5. Query the node's status via the admin API.
6. Assert: the recorded schedule interval for the node is 120 seconds.

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
5. Assert: the gateway binary does not link against LLVM, clang, or any compiler toolchain (AC5 — runtime).
6. Assert: the build dependency graph (e.g., `cargo tree -p sonde-gateway`) contains no LLVM or clang crates (AC5 — build time).
7. Assert: chunk serving (GW-0300) reads from the pre-built CBOR image without re-encoding or re-verifying (AC6).

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
1. Register a node with a known admin-assigned `node_id` (e.g., `"sensor-alpha"`) that is distinct from the node's PSK, key hint, or any internal identifier.
2. Complete a WAKE handshake. Send APP_DATA with blob `[0x01, 0x02, 0x03]`.
3. Assert: handler receives a DATA message with correct `msg_type`, `request_id`, `node_id`, `program_hash`, `data`, and `timestamp`.
4. Assert: `data` matches the original blob.
5. Assert: `node_id` in the DATA message equals the admin-assigned identifier (`"sensor-alpha"`), not the PSK, key hint, or any cryptographic material.

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

### T-0517a  Node timeout suppressed after restart until next WAKE

**Validates:** GW-0507

**Procedure:**
1. Register a node with a known schedule (`interval_s = 10`).
2. Start the gateway, send one WAKE, then stop the gateway before the timeout interval elapses.
3. Start a fresh gateway instance using the same durable state, but do not send a new WAKE from the node.
4. Wait longer than `node_timeout_multiplier × interval_s`.
5. Assert: no `node_timeout` EVENT is emitted for the node.
6. Send a new WAKE from the node.
7. Wait longer than `node_timeout_multiplier × interval_s` without another WAKE.
8. Assert: the handler now receives `node_timeout` with `last_seen` from the post-restart WAKE.

---

### T-0518  WAKE with piggybacked blob routed to handler

**Validates:** GW-0510

**Procedure:**
1. Send WAKE containing `blob` `[0xAA]`.
2. Assert: handler receives `DATA` message with `data=[0xAA]`, correct `node_id`, `program_hash`, `timestamp`.

---

### T-0519  WAKE blob handler reply is always deferred

**Validates:** GW-0510, GW-0509

**Procedure:**
1. Send WAKE with `blob`.
2. Handler replies with `data=[0xBB]`, `delivery=0`.
3. Assert: no immediate `APP_DATA_REPLY` sent to node.
4. Assert: deferred reply `[0xBB]` stored for the node.

---

### T-0520  Deferred reply piggybacked on NOP COMMAND

**Validates:** GW-0511

**Procedure:**
1. Store deferred reply `[0xBB]` for a node.
2. Node sends next WAKE.
3. Assert: NOP COMMAND response contains `blob` `[0xBB]` (key 10).

---

### T-0521  Deferred reply cleared after delivery

**Validates:** GW-0511

**Procedure:**
1. After deferred reply is delivered (per T-0520), node sends another WAKE.
2. Assert: NOP COMMAND does NOT contain `blob`.

---

### T-0522  Deferred reply latest-wins

**Validates:** GW-0509

**Procedure:**
1. Store deferred reply `[0x01]` for a node.
2. Store another deferred reply `[0x02]` before delivery.
3. Node sends WAKE.
4. Assert: NOP COMMAND carries `blob` `[0x02]`, not `[0x01]`.

---

### T-0523  Deferred data not delivered on non-NOP command

**Validates:** GW-0512

**Procedure:**
1. Store deferred reply for a node.
2. Set pending program update for that node.
3. Node sends WAKE.
4. Assert: COMMAND is UPDATE_PROGRAM and does NOT contain `blob`.
5. Assert: deferred data still stored.
6. Clear pending update.
7. Node sends next WAKE.
8. Assert: NOP COMMAND now contains the deferred `blob`.

---

### T-0524  DATA_REPLY with delivery=1 stores data

**Validates:** GW-0506 (AC4)

**Procedure:**
1. Complete WAKE handshake.
2. Send APP_DATA.
3. Handler replies with `delivery=1`, `data=[0xCC]`.
4. Assert: no `APP_DATA_REPLY` sent to node.
5. Assert: deferred data `[0xCC]` stored for the node.

---

### T-0525  DATA_REPLY with delivery=0 sends immediately

**Validates:** GW-0506 (AC5)

**Procedure:**
1. Complete WAKE handshake.
2. Send APP_DATA.
3. Handler replies with `delivery=0`, `data=[0xCC]`.
4. Assert: `APP_DATA_REPLY` sent to node with blob `[0xCC]` (existing behavior).

---

### T-0526  WAKE without blob — existing behavior preserved

**Validates:** GW-0102

**Procedure:**
1. Send WAKE without `blob` field (existing format).
2. Assert: gateway processes it normally.
3. Assert: no `DATA` message sent to handler for the blob.
4. Assert: COMMAND response is correct.

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
2. Assert: the gateway's runtime node-observation state reports `last_battery_mv = 3300`.
3. Assert: local node-status surfaces in the same gateway process can display `3300 mV`.
4. Restart the gateway against the same database without sending another WAKE.
5. Assert: battery is absent from local node-status surfaces until the next WAKE.

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

### T-0705  Battery telemetry is not durably persisted

**Validates:** GW-0702

**Procedure:**
1. Send WAKE with `battery_mv = 3300`.
2. Send WAKE with `battery_mv = 3100`.
3. Send WAKE with `battery_mv = 2900`.
4. Restart the gateway against the same database.
5. Assert: the reloaded durable node record does not restore a battery value or local battery history.
6. Assert: battery becomes available again only after a new WAKE is processed.

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

### T-0803a  ListPrograms `has_decoder` indicator

**Validates:** GW-0802 (AC-2), GW-1902

**Procedure:**
1. Ingest a program without a decoder section.
2. Store a program record with a decoder image via storage.
3. Call `ListPrograms`.

**Expected:**
1. The program without a decoder has `has_decoder = false`.
2. The program with a decoder has `has_decoder = true`.

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

### T-0809a  Node status omits `last_seen` before first post-startup WAKE

**Validates:** GW-0804

**Procedure:**
1. Register a node.
2. Start the gateway and immediately call `GetNodeStatus` before any WAKE from that node.
3. Assert: status is returned successfully.
4. Assert: `last_seen` is absent.
5. Send WAKE from the node.
6. Call `GetNodeStatus` again.
7. Assert: `last_seen` is now present and recent.

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
7. Assert: `last_seen` is absent for imported nodes until each node completes a new `WAKE`.

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

### T-0815aa  CLI modem commands invoke RPCs

**Validates:** GW-0807

**Procedure:**
1. Start the gateway with a mock modem transport and admin API enabled.
2. Run `sonde-admin modem status`.
3. Assert: output includes radio channel, TX/RX/fail counters, and uptime.
4. Run `sonde-admin modem set-channel 6`.
5. Assert: command succeeds and modem channel is updated.
6. Run `sonde-admin modem scan`.
7. Assert: output includes per-channel AP counts and RSSI values.

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
4. Assert: the response contains `rf_channel = 7`, not `1`.

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

### T-0815f  Transient modem display via admin API

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a mock or real modem transport that captures display frames.
2. Call `ShowModemDisplayMessage` with a single-line message.
3. Assert: the RPC returns successfully before the 60-second timeout expires.
4. Assert: the modem transport receives a display update corresponding to the requested text.
5. Call `ShowModemDisplayMessage` with four lines.
6. Assert: the RPC returns successfully before the 60-second timeout expires.
7. Assert: the modem transport receives a display update corresponding to all four requested lines.
8. Wait for the 60-second restore timeout associated with the four-line request (or advance paused test time).
9. Assert: the modem transport receives the normal `Sonde Gateway v<semver>` banner after the timeout.

---

### T-0815g  New transient display request replaces older one

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a mock or real modem transport that captures display frames.
2. Call `ShowModemDisplayMessage` with an initial message.
3. Before 60 seconds elapse, call `ShowModemDisplayMessage` again with a different message.
4. Assert: the second message is rendered to the modem display.
5. Wait until 60 seconds have elapsed from the first request but not from the second.
6. Assert: the gateway does not restore the default banner yet.
7. Wait until 60 seconds have elapsed from the second request.
8. Assert: the gateway restores the default banner exactly once after the second timeout window.

---

### T-0815h  Transient display rejected during BLE pairing

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a modem transport and an active BLE pairing session.
2. Call `ShowModemDisplayMessage`.
3. Assert: the RPC returns `FAILED_PRECONDITION`.
4. Assert: no transient admin display update is sent to the modem.

---

### T-0815i  Transient display rejects invalid line count

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a modem transport.
2. Call `ShowModemDisplayMessage` with zero lines.
3. Assert: the RPC returns `INVALID_ARGUMENT`.
4. Call `ShowModemDisplayMessage` with five lines.
5. Assert: the RPC returns `INVALID_ARGUMENT`.

---

### T-0815j  Transient display without modem transport

**Validates:** GW-0809

**Procedure:**
1. Start the gateway without a modem transport.
2. Call `ShowModemDisplayMessage`.
3. Assert: the RPC returns `UNAVAILABLE`.

---

### T-0815k  Persistent transient display message

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a mock or real modem transport that captures display frames.
2. Call `ShowModemDisplayMessage` with a single-line message and `persistent = true`.
3. Assert: the RPC returns successfully.
4. Assert: the modem transport receives a display update corresponding to the requested text.
5. Wait longer than 60 seconds (or advance paused test time).
6. Assert: the gateway does **not** restore the default `Sonde Gateway v<semver>` banner — the persistent message remains.
7. Call `ShowModemDisplayMessage` with a different message and `persistent = false`.
8. Assert: the second message replaces the persistent one on the modem display.
9. Wait for the 60-second restore timeout associated with the second (non-persistent) request.
10. Assert: the gateway restores the default banner after the second message's timeout.

---

### T-0815l  Non-persistent message replaces persistent message

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a mock or real modem transport that captures display frames.
2. Call `ShowModemDisplayMessage` with `persistent = true` and a message.
3. Assert: the message is displayed.
4. Call `ShowModemDisplayMessage` with `persistent = false` (default) and a different message.
5. Assert: the second message replaces the first.
6. Assert: the 60-second restore timer is started for the second message.
7. Wait for the 60-second restore timeout.
8. Assert: the gateway restores the default banner.

---

### T-0815m  Persistent message state cleared on restart

**Validates:** GW-0809

**Procedure:**
1. Start the gateway with a mock modem transport.
2. Call `ShowModemDisplayMessage` with `persistent = true` and a custom message.
3. Assert: the persistent message is displayed.
4. Stop and restart the gateway with the same database.
5. Assert: the modem displays the normal `Sonde Gateway v<semver>` startup banner, not the previously persistent message.

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

### T-0816a  Admin CLI human-readable node-status output prefers `source_filename` and falls back to hash

**Validates:** ADMIN-0200, ADMIN-0201, ADMIN-0403, GW-0400, GW-0402

**Procedure:**
1. Start a gateway instance with the admin socket enabled.
2. Arrange node state for two nodes so human-readable node-status commands exercise both resolution paths:
   - node A has assigned/current program hashes that reference a stored program record with `source_filename = "temp-reader.o"`.
   - node B has assigned/current program hashes that reference a stored program record with no `source_filename`.
3. Run `sonde-admin node list`.
4. Assert: node A's assigned/current program fields show `temp-reader.o`, never a full path.
5. Assert: node B's assigned/current program fields show the hash because filename metadata is unavailable.
6. Run `sonde-admin node get <node-a-id>`.
7. Assert: assigned/current program fields show `temp-reader.o`, never a full path.
8. Run `sonde-admin status <node-a-id>`.
9. Assert: the current program field shows `temp-reader.o`, never a full path.
10. Run `sonde-admin --format json node get <node-a-id>` and `sonde-admin --format json status <node-a-id>`.
11. Assert: JSON output remains hash-based.

---

### T-0816b  Admin CLI verbose node-status output includes hashes alongside filenames

**Validates:** ADMIN-0105, ADMIN-0200, ADMIN-0201, ADMIN-0403

**Procedure:**
1. Start a gateway instance with node state matching T-0816a, including a node whose program record has `source_filename = "temp-reader.o"`.
2. Run `sonde-admin --verbose node list`.
3. Assert: for the filename-backed node, assigned/current program fields include both `temp-reader.o` and the underlying hash.
4. Run `sonde-admin --verbose node get <node-a-id>`.
5. Assert: assigned/current program fields include both `temp-reader.o` and the underlying hash.
6. Run `sonde-admin --verbose status <node-a-id>`.
7. Assert: the current program field includes both `temp-reader.o` and the underlying hash.

---

### T-0817  Admin CLI error handling

**Validates:** GW-0806

**Procedure:**
1. Run `sonde-admin node get nonexistent-node`.
2. Assert: non-zero exit code and meaningful error message.
3. Run `sonde-admin program assign <node-id> 0000000000000000000000000000000000000000000000000000000000000000`.
4. Assert: non-zero exit code indicating program not found.

---

## 9B  Control-plane connector tests

### T-0818  Connector API availability, framing, and local-only binding

**Validates:** GW-0810

**Procedure:**
1. Start the gateway.
2. Connect to the configured connector socket using the real local IPC transport (Unix domain socket or Windows named pipe), not an in-memory test stream.
3. Send one malformed framed connector record (for example, a length prefix that exceeds `connector_max_message_size` or a length prefix that does not match the delivered payload bytes) and assert the gateway closes the connector connection cleanly within a bounded timeout (for example, 1 second).
4. Open a fresh connector connection and keep it active without requiring any protocol-specific ACK or response; this sub-case validates only the local socket/session behavior, not connector payload semantics.
5. Attempt to use a `GatewayAdmin` gRPC client against the connector socket and assert the call fails within a bounded timeout, proving the connector endpoint is not a second admin gRPC service.
6. Assert: the connector API is bound to a local-only transport (Unix domain socket or Windows named pipe) distinct from the admin API endpoint.
7. Assert: no TCP listener is opened for the connector API.
8. Open a second connector client while the fresh connector session from step 4 remains active and assert the second connection is rejected or closed without disrupting the active connector session.
9. While the connector session from step 4 remains active, perform an operator flow via the `GatewayAdmin` gRPC API (e.g., register a node, list nodes, or open a pairing window).
10. Assert: the operator flow succeeds normally, confirming `GatewayAdmin` remains fully functional while a connector session is active.

---

### T-0819  Per-entity desired-state ingress updates gateway reconciliation state

**Validates:** GW-0811

**Procedure:**
1. Start the gateway and register a node.
2. Send one `DESIRED_STATE` message targeting that node through the connector API with a concrete node desired-state map, for example `assigned_program_hash` and `schedule_interval_s`.
3. Send one invalid `DESIRED_STATE` message with an unknown `entity_kind`, then assert the gateway rejects the message or closes the connector connection and does not update desired state for any entity.
4. Assert: after the valid message, the gateway replaces any prior desired state for that node with the complete desired state from the message.
5. Send the node's next `WAKE`.
6. Assert: the resulting `COMMAND` reflects gateway reconciliation of the new desired state through the normal pending-command path rather than a direct imperative connector command.
7. Repeat the procedure with a gateway-targeted `DESIRED_STATE` message whose `entity_id` is the empty string and whose `desired_state` map is empty, then assert it updates gateway-scoped desired state without masquerading as a node-targeted command.

---

### T-0819a  Inline ELF ingestion from DESIRED_STATE

**Validates:** GW-0811

**Procedure:**
1. Start the gateway and register a node. Do not pre-ingest any programs.
2. Build a valid minimal BPF ELF binary via the test-program helpers.
3. Send a `DESIRED_STATE` message targeting the node with `assigned_program_hash` (key 1) set to the expected program hash, `assigned_program_elf` (key 5) set to the raw ELF bytes, and `assigned_program_verification_profile` (key 6) set to `"resident"`.
4. Assert: the gateway accepts the message without error.
5. Assert: the program is now present in `Storage::get_program()` with the expected hash, `Resident` profile, and correct image bytes.
6. Assert: the node's `assigned_program_hash` is updated to the new hash.

### T-0819b  Inline ELF with invalid bytes is rejected

**Validates:** GW-0811

**Procedure:**
1. Start the gateway and register a node.
2. Send a `DESIRED_STATE` message with `assigned_program_hash` (key 1) set to an arbitrary 32-byte hash, `assigned_program_elf` (key 5) set to `[0xDE, 0xAD, 0xBE, 0xEF]` (invalid ELF).
3. Assert: the gateway rejects the message with a verification error.
4. Assert: the node's desired state is NOT updated.

### T-0819c  Inline ELF hash mismatch is rejected

**Validates:** GW-0811

**Procedure:**
1. Start the gateway and register a node.
2. Build a valid BPF ELF. Compute its correct program hash.
3. Send a `DESIRED_STATE` message with `assigned_program_hash` (key 1) set to a **different** 32-byte hash, and `assigned_program_elf` (key 5) set to the valid ELF.
4. Assert: the gateway rejects the message because the ingested program's hash does not match the declared `assigned_program_hash`.

---

### T-0820  Upstream actual-state/status update after `WAKE`

**Validates:** GW-0812

**Procedure:**
1. Start the gateway and register a node with an assigned resident program.
2. Connect one connector client.
3. Configure a known node schedule interval, then send a valid `WAKE` from the registered node with known `program_hash`, `battery_mv`, `firmware_abi_version`, and `firmware_version`.
4. Assert: the connector client receives exactly one upstream actual-state/status message for that `WAKE`.
5. Assert: the message contains the expected `node_id`, current and assigned program hashes, `schedule_interval_s`, `battery_mv`, `firmware_abi_version`, `firmware_version`, and a recent timestamp.
6. Assert: the message is emitted only after the gateway has updated the node's latest-known status.
7. While the connector session remains continuously connected, trigger a gateway-scoped status change that materially affects reconciliation (e.g., remove or change the node's assigned program or schedule interval via the admin API).
8. Assert: the connector client receives an upstream status update reflecting the gateway-scoped change.

---

### T-0821  Upstream application-data delivery for `APP_DATA` and WAKE piggybacked blobs

**Validates:** GW-0813

**Procedure:**
1. Start the gateway, register a node, and connect one connector client.
2. Send `APP_DATA { blob = [0xAA, 0xBB] }` from the node.
3. Assert: the connector client receives one upstream application-data message with origin `app_data`.
4. Assert: the message contains the expected `node_id`, `program_hash`, payload bytes `[0xAA, 0xBB]`, and a recent timestamp.
5. Send a `WAKE` carrying `blob = [0xCC]`.
6. Assert: the connector client first receives the corresponding actual-state/status message and then the application-data message with origin `wake_blob`.
7. Assert: the payload bytes are delivered without modification.
8. Assert: no connector reply path is required or used for node responses; the handler path remains responsible for `send_recv()` replies.

---

### T-0822  Connector transport remains asynchronous and non-RPC-coupled

**Validates:** GW-0814

**Procedure:**
1. Start the gateway and connect one connector client.
2. Send one desired-state message without concurrently delivering a node `WAKE`.
3. Assert: the gateway accepts and stores the desired state without requiring an immediate node interaction.
4. Later, send the node's next `WAKE`.
5. Assert: the gateway applies the previously stored desired state during its normal reconciliation path.
6. Verify that upstream actual-state and application-data messages can be emitted independently of any synchronous request/response exchange with the connector.

---

### T-0823  Detectable connector-delivery failure is surfaced to operators

**Validates:** GW-0815

**Procedure:**
1. Start the gateway and connect one connector client.
2. Induce a connector-delivery failure or desynchronization condition that the gateway can detect, for example by forcing the connector subscriber to lag far enough behind the live event stream that the gateway observes a dropped-event condition.
3. Assert: the gateway surfaces the condition through operator-visible status, logging, or both.
4. Assert: the surfaced condition makes clear that control-plane desired state, upstream actual-state/app-data visibility, and reconciliation progress may be stale.
5. Assert: the emitted `CONNECTOR_HEALTH.details` identifies the detected failure mode and the stale-state scope that operators must revalidate.
6. Assert: the gateway does not silently continue reporting healthy steady-state reconciliation after the detected loss condition.

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

### T-1103a  Gateway startup sends modem display banner via reliable transfer

**Validates:** GW-1101a

**Procedure:**
1. Start a full gateway instance with a PTY-based `MockModem`.
2. Complete the modem startup handshake (`RESET` → `MODEM_READY` → `SET_CHANNEL` → `SET_CHANNEL_ACK`).
3. Assert: the mock modem receives exactly one `DISPLAY_FRAME_BEGIN` followed by eight `DISPLAY_FRAME_CHUNK` messages after the handshake completes.
4. After each begin/chunk, send the expected `DISPLAY_FRAME_ACK` from the mock modem.
5. Reassemble the chunk payloads and assert: the framebuffer contains a rendering of `Sonde Gateway v<semver>` using the gateway crate semantic version.

---

### T-1103b  Short press advances display status page when pairing is inactive

**Validates:** GW-1101b, GW-1208

**Procedure:**
1. Start a full gateway instance with a mock modem and complete the startup handshake.
2. Record the framebuffer sent for the default `Sonde Gateway v<semver>` banner.
3. Inject `EVENT_BUTTON(BUTTON_SHORT)` while no BLE pairing session is active.
4. Assert: the gateway sends a new reliable display transfer.
5. Reassemble the new framebuffer and assert: it renders the `Channel` status page.
6. Assert: no `BLE_ENABLE` or `BLE_DISABLE` message is sent as a result of the short press.

---

### T-1103c  Repeated short presses reset the status-page timeout and eventually restore the banner

**Validates:** GW-1101b, GW-1101c

**Procedure:**
1. Start a gateway instance with a mock modem and complete the startup handshake.
2. Inject `EVENT_BUTTON(BUTTON_SHORT)` while no BLE pairing session is active and capture the resulting status-page framebuffer.
3. Before 60 seconds elapse, inject a second `EVENT_BUTTON(BUTTON_SHORT)` to enter the `Nodes` page.
4. Assert: the gateway sends another reliable display transfer for the `Nodes` page and resets the page timeout.
5. If the rendered `Nodes` page is taller than 64 pixels, observe one or more autonomous scroll updates and assert: they occur without any additional button events.
6. Advance time by just under 60 seconds from the second short press and assert: the default banner has not yet been restored.
7. Advance time past the 60-second timeout.
8. Assert: the gateway sends the default `Sonde Gateway v<semver>` banner again even if the `Nodes` page was autonomously scrolling.
9. Assert: after the banner restore, no further autonomous `Nodes` page scroll updates are emitted without another button press.

---

### T-1103d  `Nodes` page text shows operational node details with property/value display formatting

**Validates:** GW-1101b

**Procedure:**
1. Start a gateway instance with a mock modem and complete the startup handshake.
2. Populate storage with at least two nodes whose metadata exercises optional fields: assigned/current program identifiers, battery, last seen, and schedule on one node, and at least one omitted optional field on another. At least one displayed program must have `source_filename` metadata and at least one displayed program must exercise hash fallback.
3. Inject `EVENT_BUTTON(BUTTON_SHORT)` twice while no BLE pairing session is active so the second page shown is `Nodes`.
4. Reassemble the first visible `Nodes` page framebuffer.
5. Assert: the rendered text uses `node_id`, assigned/current program identifiers, battery, last seen, and schedule in `node_id` order; omits absent optional fields; excludes `key_hint`; formats `last seen` in local time with locale-style date/time output; and renders each displayed field as a left-aligned property line followed by a left-aligned `- value` line.
6. Assert: when a displayed program has `source_filename`, the rendered identifier is that basename and never a full path; when `source_filename` is absent, the rendered identifier is the hash.

---

### T-1103e  Oversized `Nodes` page scrolls 3 pixels every 50 ms with leading and trailing blank scroll

**Validates:** GW-1101d

**Procedure:**
1. Start a gateway instance with a mock modem and complete the startup handshake.
2. Populate storage with enough nodes that the rendered `Nodes` page exceeds 64 pixels in height.
3. Inject `EVENT_BUTTON(BUTTON_SHORT)` twice while no BLE pairing session is active so the second page shown is `Nodes`.
4. Independently construct the expected full rendered `Nodes` page image using the display-specific field set and omission rules, and confirm that its height exceeds 64 pixels.
5. Capture the initial `Nodes` page framebuffer and treat it as offset 0.
6. Assert: offset 0 is the blank lead-in window, so the rendered content begins entering from the bottom of the display on subsequent updates.
7. Advance mocked time by 50 ms.
8. Assert: the gateway sends a new reliable display transfer whose visible window is shifted by 3 pixels relative to offset 0.
9. Continue advancing mocked time in 50 ms increments and assert: each emitted framebuffer matches the next 3-pixel window over the independently constructed full rendered image, including the leading blank region before the text enters from the bottom and trailing windows where the rendered rows move upward and blank space enters from the bottom until the bottom-most rendered text has scrolled off the top of the display.
10. Assert: the next 50 ms update restarts the visible window at the top of the blank lead-in region.

---

### T-1103f  Re-entering `Nodes` page restarts scroll at the top

**Validates:** GW-1101d

**Procedure:**
1. Start a gateway instance with a mock modem and complete the startup handshake.
2. Populate storage with enough nodes that the rendered `Nodes` page exceeds 64 pixels in height.
3. Inject `EVENT_BUTTON(BUTTON_SHORT)` twice to enter the `Nodes` page.
4. Advance mocked time long enough for at least one autonomous scroll update.
5. Inject another `EVENT_BUTTON(BUTTON_SHORT)` to leave the `Nodes` page, then another `EVENT_BUTTON(BUTTON_SHORT)` to return to it on the next cycle.
6. Assert: the first framebuffer shown after re-entering `Nodes` matches the blank lead-in starting window rather than a previously scrolled offset.

---

### T-1103g  Short `Nodes` page is static and shows the empty-registry message

**Validates:** GW-1101b, GW-1101d

**Procedure:**
1. Start a gateway instance with a mock modem and complete the startup handshake.
2. Leave the node registry empty.
3. Inject `EVENT_BUTTON(BUTTON_SHORT)` twice while no BLE pairing session is active so the second page shown is `Nodes`.
4. Reassemble the `Nodes` page framebuffer.
5. Assert: the rendered text is `No nodes registered.`
6. Advance mocked time by at least 120 ms.
7. Assert: no autonomous scroll update is sent before the normal 60-second idle-return timeout fires.

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

**Validates:** GW-1103 (criterion 5), GW-1101a

**Procedure:**
1. Start a full gateway instance with a PTY-based `MockModem`.
2. Simulate a modem disconnect by closing the PTY slave fd.
3. Assert: the frame processing loop and BLE event loop do **not** exit.
4. Reconnect the mock modem (reopen PTY, send `MODEM_READY`).
5. Assert: after the modem handshake completes, the gateway re-sends the version banner via the reliable display-transfer subprotocol.
6. Assert: the gateway resumes processing frames normally.

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

### T-1104d  Modem warm reboot — unexpected MODEM_READY fires warm_reboot_notify and cancels pending waiters

**Validates:** GW-1103 (criteria 7, 9)

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a mock duplex stream. Complete startup handshake with `SET_CHANNEL(1)`.
2. Register a pending `change_channel` waiter (put a sender in `channel_ack_slot`).
3. Simulate a modem warm reboot: send an unsolicited `MODEM_READY` from the mock stream (without closing the port).
4. Assert: `warm_reboot_notify` fires (the reader task detected the unexpected `MODEM_READY`).
5. Assert: the pending `channel_ack_slot` sender was cancelled (dropped) before `warm_reboot_notify` fired — the waiter receives an error, not a hang.

---

### T-1104e  Modem warm reboot — gateway re-runs startup with persisted channel, no backoff

**Validates:** GW-1103 (criteria 7–8), GW-0808 (AC 6), GW-1101a

**Procedure:**
1. Start a gateway instance (simulated with a mock modem duplex stream). Persist channel 7 via `SetModemChannel(7)`.
2. Simulate a modem warm reboot: send an unsolicited `MODEM_READY` from the mock stream.
3. Assert: the gateway does **not** sleep before starting recovery (no measurable delay between warm reboot detection and the next `RESET` being sent). *(Manual/expected — not yet automated; requires a gateway-level test harness.)*
4. Assert: the gateway sends `RESET` and then `SET_CHANNEL(7)` — not `SET_CHANNEL(1)` — as part of the re-initialization.
5. Assert: after the handshake completes, the gateway re-sends the version banner via the reliable display-transfer subprotocol.
6. Assert: after successful recovery, a subsequent simulated serial disconnect triggers a reconnect with a 1 s backoff (not a previously accumulated backoff value). *(Manual/expected — not yet automated; requires a gateway-level test harness.)*

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

### T-1107a  Modem EVENT_ERROR handling

**Validates:** GW-1103

**Procedure:**
1. Create a `UsbEspNowTransport` connected to a PTY-based `MockModem`.
2. Complete startup.
3. Inject an `EVENT_ERROR(DISPLAY_WRITE_FAILED)` message from the mock modem.
4. Assert: the recoverable display fault is logged as a warning.
5. Inject a `RECV_FRAME` from the mock modem.
6. Assert: `transport.recv()` still returns the frame; no reset or reconnect was triggered by the display fault.

---

### T-1107b  Reliable display transfer ACK timeout triggers reconnect

**Validates:** GW-1101a, GW-1103

**Procedure:**
1. Start a full gateway instance with a PTY-based `MockModem`.
2. Complete the modem startup handshake.
3. Accept `DISPLAY_FRAME_BEGIN` with `DISPLAY_FRAME_ACK(next_chunk_index = 0)`.
4. Accept some, but not all, `DISPLAY_FRAME_CHUNK` messages, then stop replying with `DISPLAY_FRAME_ACK`.
5. Assert: the gateway retransmits the last unacknowledged display chunk up to its retry budget.
6. Assert: after the retry budget is exhausted, the gateway enters the modem recovery path (`RESET` → `MODEM_READY` → `SET_CHANNEL`) instead of logging a recoverable display warning and continuing.

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

**Validates:** GW-1200 (retired)

> **RETIRED (issue #495).** The gateway no longer generates an Ed25519 identity keypair. Phone registration uses a direct PSK exchange; no asymmetric cryptography is required.

---

### T-1201  Gateway ID generation and persistence

**Validates:** GW-1201 (retired)

> **RETIRED (issue #495).** The gateway no longer generates or persists a `gateway_id`. Identity is established through phone PSKs issued during BLE pairing.

---

### T-1202  Ed25519 to X25519 conversion and low-order rejection

**Validates:** GW-1202 (retired)

> **RETIRED (issue #495).** X25519 / ECDH key agreement is no longer used. The phone generates the PSK directly and transmits it over the authenticated BLE channel.

---

### T-1203  REQUEST_GW_INFO happy path

**Validates:** GW-1206 (retired)

> **RETIRED (issue #495).** The `REQUEST_GW_INFO` / `GW_INFO_RESPONSE` exchange has been removed. The simplified BLE pairing flow uses `REGISTER_PHONE` / `PHONE_REGISTERED` only.

---

### T-1204  GW_INFO_RESPONSE signature fails with wrong challenge

**Validates:** GW-1206 (retired)

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

### T-1217  Key hint consistency check

**Validates:** GW-1217

**Procedure:**
1. Construct a `PEER_REQUEST` whose CBOR `node_key_hint` does **not** match `SHA-256(node_psk)[30..32]`.
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
3. Assert: session origin is `admin`.
4. Assert: `BLE_ENABLE` sent to modem.
5. Inject `EVENT_BUTTON(BUTTON_SHORT)` from the modem.
6. Assert: the registration window remains open.
7. Wait for window timeout.
8. Assert: `BLE_DISABLE` sent to modem.
9. Assert: registration window is closed.

---

### T-1221a  Button long press opens pairing session

**Validates:** GW-1208, GW-1222a

**Procedure:**
1. Ensure no BLE pairing session is active.
2. Inject `EVENT_BUTTON(BUTTON_LONG)` from the mock modem.
3. Assert: the registration window opens.
4. Assert: session origin is `button`.
5. Assert: the modem receives `BLE_ENABLE`.
6. Assert: the gateway sends a display update rendering `Pairing`.

---

### T-1221b  Long press ignored while pairing active

**Validates:** GW-1208

**Procedure:**
1. Open a button-initiated BLE pairing session via `EVENT_BUTTON(BUTTON_LONG)`.
2. Record the session deadline and count of outbound `BLE_ENABLE` messages.
3. Inject a second `EVENT_BUTTON(BUTTON_LONG)`.
4. Assert: the existing session remains open with the same origin and deadline.
5. Assert: no additional `BLE_ENABLE` is sent.

---

### T-1221c  Button short press cancels button pairing

**Validates:** GW-1208, GW-1222a

**Procedure:**
1. Open a button-initiated BLE pairing session via `EVENT_BUTTON(BUTTON_LONG)`.
2. Inject `EVENT_BUTTON(BUTTON_SHORT)`.
3. Assert: the registration window closes.
4. Assert: the modem receives `BLE_DISABLE`.
5. Assert: the gateway sends a display update rendering `Cancelled`.
6. Assert: about 2 seconds later, the gateway restores the normal Sonde Gateway version banner.
7. Assert: the short press does not navigate to a status page while the button pairing session is active.

---

### T-1221d  Button pairing timeout closes window

**Validates:** GW-1208, GW-1222a

**Procedure:**
1. Open a button-initiated BLE pairing session.
2. Wait for the implementation-defined button-pairing timeout to expire. In the current gateway implementation, this timeout is fixed at 120 seconds rather than shortened specifically for validation.
3. Assert: the registration window closes.
4. Assert: the modem receives `BLE_DISABLE`.
5. Assert: the gateway sends a display update rendering `Timed out`.
6. Assert: about 2 seconds later, the gateway restores the normal Sonde Gateway version banner.

---

### T-1222  Admin Numeric Comparison requires explicit confirmation

**Validates:** GW-1222

**Procedure:**
1. Start a BLE pairing session via admin API (`OpenBlePairing`).
2. Connect phone via BLE. Modem sends `BLE_PAIRING_CONFIRM(passkey=123456)`.
3. Assert: gateway forwards the passkey to the admin API client (e.g., as a streaming gRPC event or CLI prompt).
4. Assert: no `BLE_PAIRING_CONFIRM_REPLY` is sent until the admin client responds.
5. Admin client accepts.
6. Assert: gateway sends `BLE_PAIRING_CONFIRM_REPLY(accept=true)` to the modem.

> **Note:** In automated integration tests, run `sonde-admin pairing start` against a mock modem that injects `BLE_PAIRING_CONFIRM`, capture stdout, and assert the passkey appears. Operator confirmation is simulated by piping `y` to stdin.

---

### T-1222a  Button pairing Numeric Comparison auto-confirms and shows passkey

**Validates:** GW-1222a

**Procedure:**
1. Open a button-initiated BLE pairing session via `EVENT_BUTTON(BUTTON_LONG)`.
2. Inject `BLE_CONNECTED`.
3. Assert: the gateway sends a display update rendering `Phone connected`.
4. Inject `BLE_PAIRING_CONFIRM(passkey=123456)`.
5. Assert: the gateway sends a display update rendering `Pin` and `123456`.
6. Assert: the gateway sends `BLE_PAIRING_CONFIRM_REPLY(accept=true)` without any admin confirmation RPC.

---

### T-1222c  Admin Numeric Comparison reject path

**Validates:** GW-1222

**Procedure:**
1. Start a BLE pairing session via admin API (`OpenBlePairing`).
2. Connect phone via BLE. Modem sends `BLE_PAIRING_CONFIRM(passkey=123456)`.
3. Assert: gateway forwards the passkey to the admin API client.
4. Admin client rejects the passkey.
5. Assert: gateway sends `BLE_PAIRING_CONFIRM_REPLY(accept=false)` to the modem.
6. Assert: the BLE pairing session remains open (rejection does not close the window).

---

### T-1222b  Button pairing success display progression

**Validates:** GW-1222a

**Procedure:**
1. Open a button-initiated BLE pairing session via `EVENT_BUTTON(BUTTON_LONG)`.
2. Inject `BLE_CONNECTED`.
3. Inject `BLE_PAIRING_CONFIRM(passkey=123456)`.
4. Inject a successful `REGISTER_PHONE` flow and `PHONE_REGISTERED` event.
5. Assert: the gateway sends a display update rendering `Provisioned`.
6. Close the session normally after the successful registration path completes.
7. Assert: the gateway sends a display update rendering `Done`.
8. Assert: about 2 seconds later, the gateway restores the normal Sonde Gateway version banner.

---

### T-1223  Ed25519 seed replication

**Validates:** GW-1203 (retired)

> **RETIRED (issue #495).** Ed25519 identity and `gateway_id` have been removed. State replication is covered by T-1002 (export/import round-trip) and T-1005b.

---

### T-1223a  Phone HMAC verification

**Validates:** GW-1213 (retired)

> **RETIRED (issue #495).** AEAD decryption (AES-256-GCM) provides authentication. A separate phone HMAC verification step is no longer needed. Phone authentication is covered by T-1210 (PEER_REQUEST decryption happy path) and T-1213 (Phone AEAD with revoked PSK).

---

### T-1223b  PEER_REQUEST frame HMAC verification

**Validates:** GW-1214 (retired)

> **RETIRED (issue #495).** Frame-level HMAC-SHA256 is replaced by AES-256-GCM authenticated encryption. Frame AEAD verification is covered by T-1214 (PEER_REQUEST frame AEAD verification).

---

### T-1224  BLE GATT server via modem relay

**Validates:** GW-1204

**Procedure:**
1. Complete modem startup.
2. Using a BLE test client, scan for the modem and connect to its GATT server.
3. Discover services and assert: the Gateway Pairing Service UUID matches the value specified for GW-1204 in `ble-pairing-protocol.md`.
4. Within the Gateway Pairing Service, discover characteristics and assert: the Gateway Command characteristic UUID matches the value specified for GW-1204 and supports both Write and Indicate operations.
5. Open a BLE pairing session via the admin API.
6. Mock modem: inject a `BLE_RECV` message containing a `REGISTER_PHONE` command on the Gateway Command characteristic.
7. Assert: gateway processes the command and sends a `BLE_INDICATE` message to the modem on the same Gateway Command characteristic containing a valid `PHONE_REGISTERED` response.
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

### T-1301a  Modem transport state logging

**Validates:** GW-1301

**Procedure:**
1. Configure a `UsbEspNowTransport` with `#[traced_test]`.
2. Open the serial port to a mock modem.
3. Assert: an `INFO`-level log entry is emitted containing the state `connected`.
4. Complete the modem startup handshake.
5. Assert: an `INFO`-level log entry is emitted containing the state `ready`.
6. Drop or disconnect the mock modem.
7. Assert: an `INFO`-level log entry is emitted containing the state `disconnecting`.
8. Allow the transport to enter its reconnect loop.
9. Assert: an `INFO`-level log entry is emitted containing the state `reconnecting` and the backoff delay.

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

### T-1304  Build metadata format validation

**Validates:** GW-1303

**Procedure:**
1. At compile time, verify the build metadata environment variables are set.
2. Assert: `CARGO_PKG_VERSION` is a valid semver string (`major.minor.patch`, all numeric).
3. Assert: `SONDE_GIT_COMMIT` is a 7-character hex string or `unknown`.
4. Assert: the version string matches the pattern `<semver> (<7-char-hash-or-unknown>)`.
5. Start the gateway with `#[traced_test]` or tracing capture.
6. Assert: the startup log includes the version string with the embedded commit hash (AC3).

> **Note:** This test validates the build metadata format at compile time
> rather than invoking the binary's `--version` CLI.  Integration testing
> of the CLI output is performed manually during release validation.

---

### T-1304a  Build-type–aware log-level policy

**Validates:** GW-1304

**Procedure:**
1. Build the gateway in debug mode.
2. Assert: the compile-time maximum tracing level is TRACE (i.e., `tracing` is configured with `max_level_trace`, no `release_max_level_*` feature).
3. Start the gateway in debug mode without `RUST_LOG` set.
4. Assert: the default `EnvFilter` is `sonde_gateway=info`.
5. Build the gateway in release mode.
6. Assert: the compile-time maximum tracing level is still TRACE.
7. Start the gateway in release mode without `RUST_LOG` set.
8. Assert: the default `EnvFilter` is `sonde_gateway=warn`.
9. Set `RUST_LOG=sonde_gateway=debug` and restart.
10. Assert: the `EnvFilter` reflects the override in both build types.

---

### T-1305a  Verification failure includes instruction-level diagnostics

**Validates:** GW-1305

**Procedure:**
1. Ingest a BPF ELF that triggers a Prevail forward-analysis failure (e.g. an invalid helper call or type violation).
2. Assert: the gRPC error message contains at least one instruction-level diagnostic line from the verifier.
3. Assert: the diagnostic includes verifier-specific context (e.g. type mismatch description, register state).
4. Ingest a BPF ELF whose verifier diagnostics deterministically exceed the implementation-defined maximum length (e.g., a program with many distinct type violations across multiple instructions).
5. Assert: the first error from `find_first_error()` is preserved in the gRPC error message.
6. Assert: a truncation marker (e.g., `"[... diagnostics truncated]"`) is present (AC1).

---

### T-1305b  Successful verification produces no diagnostics

**Validates:** GW-1305

**Procedure:**
1. Ingest a valid BPF ELF that passes Prevail verification.
2. Assert: the success response contains no diagnostic messages.
3. Assert: the program is stored and retrievable by hash.

---

### T-1305c  CLI verbose and default diagnostic display

**Validates:** GW-1305

**Procedure:**
1. Ingest an invalid BPF ELF via `sonde-admin program ingest` (without `--verbose`).
2. Assert: the CLI displays the first verification error (instruction label and error description).
3. Assert: the CLI displays a hint suggesting `--verbose` for full invariants (AC3).
4. Ingest the same invalid BPF ELF via `sonde-admin program ingest --verbose`.
5. Assert: the CLI displays the verifier invariant output (register/type state at reachable instructions), equivalent in content to Prevail's `-v` flag (AC2).
6. If the invariant listing is truncated, assert: the truncation is explicitly indicated.

---

### T-1306a  File sink writes to `<basename>.log` (replace extension)

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
12. Simulate a node whose `current_program_hash` does not match any handler, with no connector subscribers.
13. Assert: a WARN log with `"APP_DATA dropped"` includes `node_id`, `program_hash`, and `handler_count` (AC6).
14. Simulate a node whose `current_program_hash` does not match any handler, with an active connector subscriber.
15. Assert: a DEBUG log with `"forwarded to connector"` includes `node_id`, `program_hash`, `handler_count`, and `connector_subscribers` (AC6). No WARN is emitted.
16. Register a handler whose stderr produces output (e.g., a script that writes to stderr on startup).
17. Assert: the handler's stderr lines appear in the gateway log at WARN level, tagged with the handler command (AC7).

---

### T-1400a  Bounded shutdown within 5 seconds

**Validates:** GW-1400

**Procedure:**
1. Start the gateway connected to a mock modem.
2. Place the mock serial port in a faulted state (e.g., simulate OS error 22 on reads/writes).
3. Send a shutdown signal (SIGTERM / Ctrl-C or `SERVICE_CONTROL_STOP`).
4. Wait for the "gateway stopped" log entry.
5. Assert: the process terminates within 5 seconds after the "gateway stopped" log.
6. Assert: a warning-level log entry is emitted before the force-exit (e.g., "force-exiting after shutdown timeout").
7. Repeat steps 1–5 without the faulted serial port.
8. Assert: the gateway shuts down gracefully (no force-exit warning).

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

### T-1403a  CLI handler management commands

**Validates:** GW-1403

**Procedure:**
1. Start a gateway with the admin API enabled.
2. Run `sonde-admin handler list` and assert: output contains zero handlers (empty table or empty JSON array with `--format json`).
3. Run `sonde-admin handler add "*" echo --reply-timeout-ms 5000 --working-dir /tmp` and assert: command succeeds.
4. Run `sonde-admin handler list` and assert: output contains one handler with `program_hash = "*"`, `command = "echo"`, `reply_timeout_ms = 5000`, and `working_dir = "/tmp"`.
5. Run `sonde-admin handler list --format json` and assert: output is valid JSON containing the same handler fields.
6. Run `sonde-admin handler add abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd echo2` (valid 64-char hex hash) and assert: command succeeds.
7. Run `sonde-admin handler list` and assert: output contains two handlers.
8. Run `sonde-admin handler remove "*"` and assert: command succeeds.
9. Run `sonde-admin handler list` and assert: output contains one handler (the hex-hash handler).

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

### T-1501a  MSI install dialog, auto-detect, ACL, uninstall, and upgrade

**Validates:** GW-1501

**Procedure:**
1. Run the MSI installer on a Windows machine.
2. Assert: the install wizard includes a "Modem Configuration" dialog page with a COM port selector (AC1).
3. Connect an ESP32-S3 modem (VID `303A`, PID `1001`) before reaching the dialog.
4. Assert: the COM port field is pre-populated with the detected port (AC2).
5. Complete the install.
6. Assert: the `%ProgramData%\sonde\` directory exists with appropriate ACLs restricting write access to administrators and the service account (AC5).
7. Run the MSI uninstaller.
8. Assert: the service is stopped and removed, but the database and key files remain on disk (AC6).
9. Re-install the service via MSI, then run an MSI upgrade (newer version).
10. Assert: the service is stopped before upgrade and restarted after, with the existing configuration preserved (AC7).

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
10. Assert: `/var/lib/sonde/master-key.hex` exists and contains 64 hex characters (auto-generated by `--generate-master-key`).
11. Assert: the gateway log contains `"master key loaded"` and `"modem transport ready"`.
12. Remove the package: `sudo dpkg -r sonde`.
13. Assert: the service is stopped and disabled.
14. Assert: `/var/lib/sonde/gateway.db` is preserved (not deleted by removal).

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
4. `sonde-admin --verbose node get sensor-1` shows the assigned program hash matching the deployed program.
5. `sonde-admin --verbose node get sensor-2` shows the assigned program hash matching the deployed program.
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

## 16  Pairing-time diagnostic tests

### T-1700  DIAG_REQUEST valid frame accepted

**Traces to:** GW-1700 (AC-1, AC-2, AC-3)

**Preconditions:** Gateway running with a registered phone PSK (key_hint=0x1234).

**Steps:**
1. Construct a `DIAG_REQUEST` frame with `key_hint=0x1234`, `msg_type=0x06`, random nonce, CBOR `{1: 0x01}`, encrypted with phone_psk.
2. Deliver the frame to the gateway via the transport.

**Expected:**
1. Gateway decrypts the frame successfully.
2. Gateway processes the `DIAG_REQUEST` and sends a `DIAG_REPLY`.

---

### T-1701  DIAG_REQUEST wrong PSK silently discarded

**Traces to:** GW-1700 (AC-4)

**Preconditions:** Gateway running with a registered phone PSK.

**Steps:**
1. Construct a `DIAG_REQUEST` frame encrypted with a different PSK (not registered).
2. Deliver the frame to the gateway.

**Expected:**
1. Gateway silently discards the frame.
2. No `DIAG_REPLY` is sent.
3. A debug-level log entry is recorded.

---

### T-1702  DIAG_REQUEST revoked PSK rejected

**Traces to:** GW-1700 (AC-2)

**Preconditions:** Gateway running. Register a phone PSK, then revoke it.

**Steps:**
1. Construct a `DIAG_REQUEST` encrypted with the revoked PSK.
2. Deliver the frame to the gateway.

**Expected:**
1. Gateway silently discards the frame (revoked PSKs are not candidates).
2. No `DIAG_REPLY` is sent.

---

### T-1703  DIAG_REQUEST no session required

**Traces to:** GW-1701 (AC-1, AC-2, AC-3)

**Preconditions:** Gateway running with a registered phone PSK. No active node sessions.

**Steps:**
1. Construct a valid `DIAG_REQUEST` from an unknown sender MAC.
2. Deliver the frame to the gateway.

**Expected:**
1. Gateway processes the request despite no active session for the sender.
2. A `DIAG_REPLY` is sent.
3. No session state is created.

---

### T-1704  DIAG_REPLY contains correct RSSI

**Traces to:** GW-1702 (AC-1, AC-2)

**Preconditions:** Gateway running. Transport metadata reports RSSI = −65 dBm.

**Steps:**
1. Send a valid `DIAG_REQUEST`.
2. Capture the `DIAG_REPLY` frame.
3. Decrypt and decode the CBOR payload.

**Expected:**
1. `rssi_dbm` field = −65.
2. `diagnostic_type` = 0x01.

---

### T-1705  Signal quality assessment — good

**Traces to:** GW-1703 (AC-1, AC-3)

**Preconditions:** Gateway running with default thresholds (good ≥ −60, bad < −75).

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −50 dBm.
2. Decode the `DIAG_REPLY`.

**Expected:**
1. `signal_quality` = 0 (good).

---

### T-1706  Signal quality assessment — marginal

**Traces to:** GW-1703 (AC-1, AC-3)

**Preconditions:** Gateway running with default thresholds.

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −70 dBm.
2. Decode the `DIAG_REPLY`.

**Expected:**
1. `signal_quality` = 1 (marginal).

---

### T-1707  Signal quality assessment — bad

**Traces to:** GW-1703 (AC-1, AC-3)

**Preconditions:** Gateway running with default thresholds.

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −80 dBm.
2. Decode the `DIAG_REPLY`.

**Expected:**
1. `signal_quality` = 2 (bad).

---

### T-1708  Signal quality with custom thresholds

**Traces to:** GW-1705 (AC-1, AC-2)

**Preconditions:** Gateway configured with good_threshold = −50, bad_threshold = −65.

**Steps:**
1. Deliver a `DIAG_REQUEST` with RSSI = −55 dBm.
2. Decode the `DIAG_REPLY`.

**Expected:**
1. `signal_quality` = 1 (marginal — below −50 but above −65).

---

### T-1704a  DIAG_REPLY encryption, CBOR fields, and reply MAC

**Traces to:** GW-1704 (AC-1, AC-4, AC-5)

**Preconditions:** Gateway running with a registered phone PSK (`phone_psk`). Mock modem transport capturing outbound frames.

**Steps:**
1. Construct and send a valid `DIAG_REQUEST` encrypted with `phone_psk` from a known sender MAC.
2. Capture the outbound `DIAG_REPLY` frame from the mock modem transport.

**Expected:**
1. The `DIAG_REPLY` frame can be decrypted with the same `phone_psk` used for the request (AC1).
2. The decrypted CBOR payload contains all three required fields: `diagnostic_type` (integer), `rssi_dbm` (integer), `signal_quality` (integer) (AC4).
3. The reply is addressed to the sender MAC from the original `RECV_FRAME` (AC5).

---

### T-1709  DIAG_REPLY nonce echoes request

**Traces to:** GW-1704 (AC-2)

**Preconditions:** Gateway running with a registered phone PSK.

**Steps:**
1. Construct a `DIAG_REQUEST` with nonce = `[0x01, 0x02, ..., 0x08]`.
2. Capture the `DIAG_REPLY` frame header.

**Expected:**
1. The `nonce` field in the reply header equals `[0x01, 0x02, ..., 0x08]`.

---

### T-1710  DIAG_REPLY uses phone key_hint

**Traces to:** GW-1704 (AC-3)

**Preconditions:** Gateway running with phone PSK (key_hint=0xABCD).

**Steps:**
1. Send a `DIAG_REQUEST` with `key_hint=0xABCD`.
2. Capture the `DIAG_REPLY` frame header.

**Expected:**
1. The `key_hint` in the reply header = `0xABCD`.
2. The reply can be decrypted with the phone PSK.

---

### T-1711  Diagnostic logging

**Traces to:** GW-1706 (AC-1, AC-2, AC-3)

**Preconditions:** Gateway running with tracing subscriber capturing INFO-level events.

**Steps:**
1. Send a valid `DIAG_REQUEST`.
2. Capture log output.

**Expected:**
1. An INFO-level log entry for DIAG_REQUEST reception includes sender MAC and key_hint.
2. An INFO-level log entry for DIAG_REPLY transmission includes RSSI and signal quality.
3. No PSK material appears in any log entry.

---

### T-1712  Signal quality boundary — exact good threshold

**Traces to:** GW-1703 (AC-1)

**Preconditions:** Gateway running with default thresholds (good ≥ −60).

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −60 dBm (exact boundary).

**Expected:**
1. `signal_quality` = 0 (good — boundary is inclusive).

---

### T-1713  Signal quality boundary — just below good threshold

**Traces to:** GW-1703 (AC-1)

**Preconditions:** Gateway running with default thresholds.

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −61 dBm.

**Expected:**
1. `signal_quality` = 1 (marginal).

---

### T-1714  Signal quality boundary — exact bad threshold

**Traces to:** GW-1703 (AC-1)

**Preconditions:** Gateway running with default thresholds (bad < −75).

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −75 dBm (exact boundary).

**Expected:**
1. `signal_quality` = 1 (marginal — bad threshold is exclusive).

---

### T-1715  Signal quality boundary — just below bad threshold

**Traces to:** GW-1703 (AC-1)

**Preconditions:** Gateway running with default thresholds.

**Steps:**
1. Deliver a `DIAG_REQUEST` with transport RSSI = −76 dBm.

**Expected:**
1. `signal_quality` = 2 (bad).

---

### T-1716  RSSI sentinel when transport provides no RSSI

**Traces to:** GW-1702 (AC-3)

**Preconditions:** Gateway running with a loopback transport (no RSSI metadata).

**Steps:**
1. Send a valid `DIAG_REQUEST` via the loopback transport.
2. Decode the `DIAG_REPLY`.

**Expected:**
1. `rssi_dbm` = 0 (sentinel value).
2. A WARN-level log entry indicates RSSI was unavailable.

---

### T-1717  Invalid threshold configuration rejected at startup

**Traces to:** GW-1705 (AC-3)

**Preconditions:** Gateway configured with good_threshold = −80, bad_threshold = −60 (invalid: good must be > bad).

**Steps:**
1. Start the gateway.

**Expected:**
1. An ERROR-level log entry indicates the RSSI thresholds are invalid.
2. The gateway falls back to default thresholds (good = −60, bad = −75).

---

## 17  Container image tests

### T-1800  Container image contains expected binaries, flashing tool, and bundled BPF assets

**Traces to:** GW-1800 (AC-2, AC-4), GW-1804 (AC-1), GW-1805 (AC-1)

**Preconditions:** Container image built from `Dockerfile.gateway` for the native architecture.

**Steps:**
1. Run `docker run --rm <image> --version`.
2. Run `docker run --rm --entrypoint sonde-admin <image> --version`.
3. Run `docker run --rm --entrypoint sonde-sht40-handler <image> --version`.
4. Run `docker run --rm --entrypoint sonde-tmp102-handler <image> --version`.
5. Run `docker run --rm --entrypoint espflash <image> --help`.
6. Run `docker run --rm --entrypoint sh <image> -c 'find /usr/local/share/sonde/test-programs -maxdepth 1 -name "*.o" | grep -q .'`.

**Expected:**
1. All six commands exit 0; the four Sonde binaries print version strings, `espflash` prints help text, and at least one bundled BPF test-program object is present.
2. No Rust toolchain or source code is present in the image; the only non-binary build outputs present are the intentionally bundled modem flash images and compiled BPF test-program objects.

---

### T-1801  Container image tagging — nightly

**Traces to:** GW-1801 (AC-1, AC-3)

**Preconditions:** Workflow triggered by schedule or `workflow_dispatch` (not a `v*` tag).

**Steps:**
1. Inspect the tags created by the manifest job.

**Expected:**
1. Tags include `nightly`, `nightly-YYYYMMDD`, and `sha-<short>`.
2. The `latest` tag is NOT created.

---

### T-1801a  Container image tagging — release

**Traces to:** GW-1801 (AC-2, AC-3)

**Preconditions:** Workflow triggered by a `v*` tag push.

**Steps:**
1. Inspect the tags created by the manifest job.

**Expected:**
1. Tags include `latest`, the semver version, and `sha-<short>`.
2. The `nightly` and `nightly-YYYYMMDD` tags are NOT created.

---

### T-1802  Container runtime defaults, non-root user, and writable volume

**Traces to:** GW-1802 (AC-2, AC-3, AC-4), GW-1804 (AC-5)

**Preconditions:** Container image built.

**Steps:**
1. Run `docker inspect <image>` and inspect `Config.Entrypoint` and `Config.Cmd`.
2. Run `docker run --rm --entrypoint sh <image> -c 'whoami'`.
3. Run `docker run --rm --entrypoint sh <image> -c 'touch /var/lib/sonde/test && rm /var/lib/sonde/test'`.
4. Run `docker run --rm <image> --help` and verify `--key-provider env` appears in the output.

**Expected:**
1. `ENTRYPOINT` is `["sonde-gateway"]` and `CMD` is `["--db", "/var/lib/sonde/sonde.db", "--port", "/dev/ttyACM0", "--key-provider", "env"]`, so the default startup path remains the gateway binary rather than `espflash`.
2. `whoami` outputs `sonde`.
3. File creation in `/var/lib/sonde` succeeds (writable by `sonde` user).
4. `--key-provider env` is accepted by the CLI help path.

---

### T-1802a  Container exposes bundled modem image paths

**Traces to:** GW-1802 (AC-7, AC-8), GW-1804 (AC-2, AC-3)

**Preconditions:** Container image built.

**Steps:**
1. Run `docker run --rm --entrypoint sh <image> -c 'test -f /usr/local/share/sonde/firmware/modem/default/flash_image.bin'`.
2. Run `docker run --rm --entrypoint sh <image> -c 'test -f /usr/local/share/sonde/firmware/modem/verbose/flash_image.bin'`.
3. Run `docker run --rm --entrypoint sh <image> -c 'test -r /usr/local/share/sonde/firmware/modem/default/flash_image.bin && test -r /usr/local/share/sonde/firmware/modem/verbose/flash_image.bin'`.

**Expected:**
1. Both files exist at the documented stable paths.
2. Both files are readable by the container's default non-root user.

---

### T-1802b  Container exposes bundled BPF test-program paths

**Traces to:** GW-1802 (AC-9), GW-1805 (AC-1, AC-2, AC-3)

**Preconditions:** Container image built.

**Steps:**
1. Run `docker run --rm --entrypoint sh <image> -c 'test -d /usr/local/share/sonde/test-programs'`.
2. Run `docker run --rm --entrypoint sh <image> -c 'find /usr/local/share/sonde/test-programs -maxdepth 1 -name "*.o" | grep -q .'`.
3. Run `docker run --rm --entrypoint sh <image> -c 'find /usr/local/share/sonde/test-programs -maxdepth 1 -name "*.o" -readable | grep -q .'`.

**Expected:**
1. The stable bundled BPF directory exists at `/usr/local/share/sonde/test-programs`.
2. The directory contains compiled `.o` test-program artifacts.
3. The bundled `.o` files are readable by the container's default non-root user.

---

### T-1803  Build without keyring feature

**Traces to:** GW-1803 (AC-1, AC-2, AC-4, AC-5)

**Preconditions:** `cargo build -p sonde-gateway --no-default-features` succeeds.

**Steps:**
1. Build `sonde-gateway` with `--no-default-features`.
2. Run the resulting binary with `--key-provider file --master-key-file <path> --help`.
3. Run the resulting binary with `--key-provider secret-service`.

**Expected:**
1. Build succeeds without the `secret-service` / `zbus` dependency.
2. `--key-provider file` is accepted.
3. `--key-provider secret-service` returns a `NotAvailable` error mentioning the `keyring` feature.

---

### T-1804  Multi-arch manifest contains both platforms

**Traces to:** GW-1800 (AC-3, AC-5), GW-1801 (AC-4)

**Preconditions:** Both amd64 and arm64 builds have completed and passed smoke tests.

**Steps:**
1. Run `docker manifest inspect ghcr.io/alan-jowett/sonde-gateway:<tag>`.

**Expected:**
1. The manifest lists two platforms: `linux/amd64` and `linux/arm64`.
2. Each platform entry references a distinct image digest.

---

### T-1805  Static musl linkage verified

**Traces to:** GW-1800 (AC-1)

**Preconditions:** Container image built.

**Steps:**
1. Run `docker run --rm --entrypoint sh <image> -c 'ldd /usr/local/bin/sonde-gateway 2>&1'`.

**Expected:**
1. Output contains one of `statically linked`, `not a dynamic executable`, or `not a valid dynamic program`; or, on musl default-PIE toolchains, a single optional-`/lib/` `ld-musl-*.so.1 (0x...)` line with no `=>` dependency entries.

---

### T-1806  Bundled modem images match same-run CI artifacts

**Traces to:** GW-1804 (AC-4)

**Preconditions:** A workflow run has produced `modem-firmware`, `modem-firmware-verbose`, and the container image for the same commit/tag. This applies both to nightly/release runs and to standalone `workflow_dispatch` runs, which must include a same-run modem-artifact production step before the image build.

**Steps:**
1. Download the `modem-firmware` and `modem-firmware-verbose` artifacts from the same workflow run that produced the container image.
2. Extract `/usr/local/share/sonde/firmware/modem/default/flash_image.bin` and `/usr/local/share/sonde/firmware/modem/verbose/flash_image.bin` from the container image.
3. Compute SHA-256 hashes for the two downloaded artifacts and the two extracted in-image files.

**Expected:**
1. The hash of the in-image default file matches the hash of the downloaded `modem-firmware` artifact.
2. The hash of the in-image verbose file matches the hash of the downloaded `modem-firmware-verbose` artifact.
3. The compared artifacts and image were all produced by the same workflow run.

---

### T-1806a  Bundled BPF test programs match same-run CI artifacts

**Traces to:** GW-1805 (AC-4), GW-1806 (AC-5)

**Preconditions:** A workflow run has produced the compiled BPF test-program artifact set and the container image for the same commit/tag.

**Steps:**
1. Download the compiled BPF test-program artifact set from the same workflow run that produced the container image.
2. Extract `/usr/local/share/sonde/test-programs/` from the container image.
3. Compare the file list and SHA-256 hashes of the downloaded `.o` files against the in-image copies.

**Expected:**
1. The same set of `.o` filenames exists in both locations.
2. Each in-image `.o` file is byte-identical to the same-run workflow artifact.
3. The compared artifacts and image were all produced by the same workflow run.

---

### T-1807  Nightly release publishes sensor handlers and BPF test programs

**Traces to:** GW-1806 (AC-1, AC-2, AC-3, AC-4)

**Preconditions:** `nightly-release.yml` run completed successfully.

**Steps:**
1. Inspect the workflow artifacts and/or GitHub release assets produced by the run.
2. Verify separate Linux handler assets exist for amd64 and arm64 for both `sonde-sht40-handler` and `sonde-tmp102-handler`.
3. Verify the compiled arch-independent BPF test-program artifact set is present.
4. Inspect the generated release notes / asset manifest.

**Expected:**
1. Distinct amd64 and arm64 assets exist for both Rust handler binaries.
2. A compiled BPF `.o` artifact set from `test-programs/` is present exactly once as an arch-independent asset group.
3. Asset names distinguish architectures without collisions.
4. The release notes / asset manifest enumerate the handler binaries and BPF test-program assets.

---

### T-1900  Decoder section extraction from ELF

**Traces to:** GW-1900 (AC-1)

**Preconditions:** A test ELF with both `SEC("sonde")` and `SEC("decoder")` sections.

**Steps:**
1. Ingest the dual-section ELF via `IngestProgram`.
2. Query the stored program record.

**Expected:**
1. Program stored with both node image and decoder image.
2. Node program hash matches hash of node image only (decoder excluded).

---

### T-1900a  ELF without decoder section (backward compat)

**Traces to:** GW-1900 (AC-2), GW-1905 (AC-1)

**Preconditions:** An existing single-section ELF (e.g., `tmp102_sensor.o`).

**Steps:**
1. Ingest a standard single-section ELF.

**Expected:**
1. Program stored with no decoder image.
2. Behavior identical to pre-feature gateway.

---

### T-1900b  ELF with decoder only (no sonde section) rejected

**Traces to:** GW-1900 (AC-3)

**Steps:**
1. Build an ELF with only `SEC("decoder")`, no `SEC("sonde")`.
2. Ingest via `IngestProgram`.

**Expected:**
1. Rejected with error indicating missing sonde section.

---

### T-1900c  ELF with empty decoder section treated as no decoder

**Traces to:** GW-1900 (AC-8)

**Steps:**
1. Build an ELF with `SEC("sonde")` and an empty `SEC("decoder")` (zero bytecode).
2. Ingest via `IngestProgram`.

**Expected:**
1. Program stored with no decoder image (empty section ignored).

---

### T-1900d  ELF with multiple decoder sections rejected

**Traces to:** GW-1900 (AC-7)

**Steps:**
1. Build an ELF with `SEC("sonde")` and two `SEC("decoder")` sections.
2. Ingest via `IngestProgram`.

**Expected:**
1. Rejected with error indicating multiple decoder sections.

---

### T-1900e  Invalid decoder section rejects entire ELF

**Traces to:** GW-1900 (AC-4)

**Steps:**
1. Build an ELF with a valid `SEC("sonde")` section and a `SEC("decoder")` section that fails Prevail verification (e.g., invalid helper call or type violation).
2. Ingest via `IngestProgram`.

**Expected:**
1. The entire ELF is rejected, even though the `sonde` section is valid.
2. The error message indicates the decoder section failed verification.

---

### T-1900f  Global data shared between sonde and decoder sections

**Traces to:** GW-1900 (AC-5)

**Steps:**
1. Build an ELF with `SEC("sonde")` and `SEC("decoder")` sections that share global data (`.rodata` or `.data` sections with map definitions used by both).
2. Ingest via `IngestProgram`.

**Expected:**
1. Both images are produced successfully.
2. Each image receives the map definitions and initial data relevant to its section.
3. Shared global data is correctly represented in both the node image and decoder image.

---

### T-1900g  Section name matching is exact

**Traces to:** GW-1900 (AC-6)

**Steps:**
1. Build an ELF with `SEC("sonde")`, a valid `SEC("decoder")`, and an additional section named `decoder.text` (or `decoderx`, `my_decoder`, etc.).
2. Ingest via `IngestProgram`.

**Expected:**
1. Only `SEC("decoder")` is recognized as the decoder section.
2. Sections with similar but non-matching names (e.g., `decoder.text`) are ignored.
3. The program is ingested successfully with one decoder image (from the exact `decoder` section).

---

### T-1901  Decoder verification with DecoderPlatform

**Traces to:** GW-1901 (AC-1)

**Preconditions:** A decoder program that calls only permitted helpers (`emit_reading`, `map_lookup_elem`, `bpf_trace_printk`).

**Steps:**
1. Ingest the ELF with valid sonde and decoder sections.

**Expected:**
1. Verification passes, decoder image stored.

---

### T-1901a  Decoder using hardware helpers rejected

**Traces to:** GW-1901 (AC-2)

**Steps:**
1. Build a decoder program that calls `i2c_read`.
2. Ingest the ELF.

**Expected:**
1. Verification fails with error about invalid/unknown helper.

---

### T-1901b  Decoder using send helper rejected

**Traces to:** GW-1901 (AC-2)

**Steps:**
1. Build a decoder program that calls `send`.
2. Ingest the ELF.

**Expected:**
1. Verification fails with error about invalid/unknown helper.

---

### T-1902  Decoder image storage and retrieval

**Traces to:** GW-1902 (AC-1, AC-2, AC-4)

**Steps:**
1. Ingest an ELF with decoder section.
2. Retrieve the program record by hash.
3. Assert decoder image is present and decodable as valid `ProgramImage`.
4. Remove the program via `RemoveProgram`.

**Expected:**
1. Decoder image present after ingest.
2. Decoder image removed after `RemoveProgram`.

---

### T-1902a  Decoder image replacement on re-ingest

**Traces to:** GW-1902 (AC-7)

**Preconditions:** Ingested ELF with decoder producing `emit_reading("v1", 1)`.

**Steps:**
1. Build a second ELF with identical `sonde` section but different `decoder`
   section (producing `emit_reading("v2", 2)`).
2. Re-ingest the second ELF.
3. Simulate APP_DATA and inspect enriched readings.

**Expected:**
1. Program hash is unchanged (same `sonde` bytecode).
2. Decoder image is replaced — readings contain `v2`, not `v1`.
3. Gateway logs decoder change at INFO level.

---

### T-1902b  Decoder removal on re-ingest without decoder

**Traces to:** GW-1902 (AC-8)

**Preconditions:** Ingested ELF with decoder section.

**Steps:**
1. Build a new ELF with identical `sonde` section but no `decoder` section.
2. Re-ingest.
3. Simulate APP_DATA and inspect forwarded message.

**Expected:**
1. Program hash is unchanged.
2. Decoder image is removed.
3. APP_DATA forwarded without `readings` field.

---

### T-1903  APP_DATA enrichment with decoder

**Traces to:** GW-1903 (AC-1, AC-3, AC-6)

**Preconditions:** Ingested program with a decoder that calls `emit_reading("temp_mc", 25125)`.

**Steps:**
1. Assign program to a test node. Simulate an APP_DATA from that node.
2. Inspect the DATA message forwarded to the handler.
3. Inspect the GW-0813 message forwarded to the connector.

**Expected:**
1. Both messages contain a `readings` field with `{ "temp_mc": 25125 }`.
2. Raw `data`/`blob` field is preserved byte-for-byte.

---

### T-1903a  APP_DATA without decoder forwarded unchanged

**Traces to:** GW-1903 (AC-2), GW-1905 (AC-3, AC-4)

**Preconditions:** Ingested program without a decoder section.

**Steps:**
1. Simulate an APP_DATA from a node running the program.

**Expected:**
1. DATA message has no `readings` field.

---

### T-1903b  Decoder failure does not block data delivery

**Traces to:** GW-1903 (AC-5)

**Preconditions:** Ingested program with a decoder that exceeds the instruction budget.

**Steps:**
1. Simulate an APP_DATA from a node.

**Expected:**
1. Warning logged.
2. Original unenriched message forwarded to handler and connector.

---

### T-1903c  Enriched message preserves raw blob unchanged

**Traces to:** GW-1903 (AC-8)

**Steps:**
1. Ingest a program with a decoder.
2. Simulate APP_DATA with known blob bytes.
3. Inspect forwarded DATA message.

**Expected:**
1. `data` field matches original blob bytes exactly.
2. `readings` field present as a separate sibling field.

---

### T-1903d  Both handler and connector receive identical enriched message

**Traces to:** GW-1903 (AC-6)

**Steps:**
1. Capture the DATA message sent to the handler and the GW-0813 message
   sent to the connector for the same APP_DATA.

**Expected:**
1. Both contain the same `readings` content.

---

### T-1904  emit_reading helper captures readings

**Traces to:** GW-1904 (AC-2, AC-3, AC-4)

**Steps:**
1. Execute a decoder BPF program that calls `emit_reading("a", 1)`,
   `emit_reading("b", 2)`, `emit_reading("a", 3)`.
2. Inspect the resulting readings.

**Expected:**
1. Readings map is `{ "a": 3, "b": 2 }` (last-write-wins for "a").

---

### T-1904a  emit_reading with name_len=64 succeeds

**Traces to:** GW-1904 (AC-8)

**Steps:**
1. Call `emit_reading` with a 64-byte name.

**Expected:**
1. Returns `0` (success).
2. Reading is included.

---

### T-1904b  emit_reading with name_len=65 returns -1

**Traces to:** GW-1904 (AC-8)

**Steps:**
1. Call `emit_reading` with a 65-byte name.

**Expected:**
1. Returns `-1`.
2. Reading is NOT included.

---

### T-1904c  emit_reading overflow (33rd reading) returns -2

**Traces to:** GW-1904 (AC-9)

**Steps:**
1. Call `emit_reading` 33 times with distinct names.

**Expected:**
1. First 32 calls return `0`.
2. 33rd call returns `-2`.
3. First 32 readings are included in the readings map.

---

### T-1904d  Decoder with rodata map reads initial data correctly

**Traces to:** GW-1904 (AC-5)

**Steps:**
1. Build a decoder with a `.rodata` global variable containing a lookup table.
2. Execute the decoder. It uses `map_lookup_elem` to read from the table.

**Expected:**
1. `map_lookup_elem` returns the expected initial data values.

---

### T-1904e  map_update_elem on rodata map returns error

**Traces to:** GW-1904 (AC-10)

**Steps:**
1. Build a decoder that calls `map_update_elem` on a `.rodata`-backed map.

**Expected:**
1. `map_update_elem` returns error (non-zero).

---

### T-1904f  Decoder context ABI — input_data and input_end pointers

**Traces to:** GW-1904 (AC-1)

**Steps:**
1. Build a decoder that loads `input_data` (ctx+0) and `input_end` (ctx+8).
2. Compute `input_end - input_data` to obtain the blob length.
3. Emit the length as a reading.
4. Execute with a 13-byte APP_DATA blob.

**Expected:**
1. The emitted reading equals 13 (the blob length).

---

### T-1904g  bpf_trace_printk executes without error

**Traces to:** GW-1904 (AC-6)

**Steps:**
1. Build a decoder that calls `bpf_trace_printk("hello", 5)`.
2. Execute the decoder.

**Expected:**
1. Execution succeeds without error.

> **Note:** Full assertion that the message appears at tracing target
> `decoder_bpf` requires `tracing-test` infrastructure.  This test
> confirms the helper executes without panic or error.

---

### T-1906  Program hash unchanged by decoder presence

**Traces to:** GW-1906 (AC-1)

**Steps:**
1. Build two ELFs from identical `sonde` source: one without decoder, one with.
2. Ingest both.

**Expected:**
1. Both produce the same node program hash.

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
| GW-1700 | T-1700, T-1701, T-1702 |
| GW-1701 | T-1703 |
| GW-1702 | T-1704, T-1716 |
| GW-1703 | T-1705, T-1706, T-1707, T-1712, T-1713, T-1714, T-1715 |
| GW-1704 | T-1709, T-1710 |
| GW-1705 | T-1708, T-1717 |
| GW-1706 | T-1711 |
| GW-1800 | T-1800, T-1804, T-1805 |
| GW-1801 | T-1801, T-1801a, T-1804 |
| GW-1802 | T-1802, T-1802a, T-1802b |
| GW-1803 | T-1803 |
| GW-1804 | T-1800, T-1802a, T-1806 |
| GW-1805 | T-1800, T-1802b, T-1806a |
| GW-1806 | T-1806a, T-1807 |
| GW-1900 | T-1900, T-1900a, T-1900b, T-1900c, T-1900d |
| GW-1901 | T-1901, T-1901a, T-1901b |
| GW-1902 | T-1902, T-1902a, T-1902b |
| GW-1903 | T-1903, T-1903a, T-1903b, T-1903c, T-1903d |
| GW-1904 | T-1904, T-1904a, T-1904b, T-1904c, T-1904d, T-1904e, T-1904f, T-1904g |
| GW-1905 | T-1900a, T-1903a |
| GW-1906 | T-1906 |
