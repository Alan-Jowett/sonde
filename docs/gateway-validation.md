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

The helper handles header construction, CBOR encoding, sequence numbering, and HMAC computation.

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
1. Construct a frame with valid header and HMAC but garbage bytes as the CBOR payload.
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
1. Send a WAKE missing `battery_mv` (valid HMAC, valid header).
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

## 8  Authentication and security tests

### T-0600  Valid HMAC accepted

**Validates:** GW-0600

**Procedure:**
1. Send a correctly authenticated WAKE.
2. Assert: gateway processes it and responds with COMMAND.

---

### T-0601  Invalid HMAC rejected

**Validates:** GW-0600

**Procedure:**
1. Construct a WAKE with a valid header but corrupt the HMAC (flip one bit).
2. Send to gateway.
3. Assert: silently discarded, no response sent.

---

### T-0602  Wrong key rejected

**Validates:** GW-0600, GW-0601

**Procedure:**
1. Construct a WAKE using PSK_A but with a `key_hint` that maps to PSK_B.
2. Send to gateway.
3. Assert: HMAC verification fails, silently discarded.

---

### T-0603  key_hint collision handling

**Validates:** GW-0601

**Procedure:**
1. Register two nodes with the same `key_hint` but different PSKs.
2. Send WAKE from node A.
3. Assert: gateway tries both PSKs, accepts the one that matches.
4. Assert: response is sent to the correct peer address.

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

---

### T-0608  Frame overhead budget

**Validates:** GW-0603

**Procedure:**
1. Capture any outbound frame.
2. Assert: first 11 bytes are header (key_hint 2B + msg_type 1B + nonce 8B).
3. Assert: last 32 bytes are HMAC.
4. Assert: total frame = 11 + payload_len + 32.

---

### T-0609  Unknown node — silent discard

**Validates:** GW-1002

**Procedure:**
1. Send WAKE from an unregistered node (key_hint with no matching PSK).
2. Assert: no response sent.
3. Assert: no internal state changed.
4. Assert: event logged.

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
5. Assert: `send()` returns immediately (fire-and-forget, no response awaited).

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

### T-1105  Health monitoring — tx_fail_count rising

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

## Appendix A  Test-to-requirement traceability

| Requirement | Test(s) |
|---|---|
| GW-0100 | T-0100 |
| GW-0101 | T-0101, T-0102 |
| GW-0102 | T-0103, T-0104 |
| GW-0103 | T-0105 |
| GW-0104 | T-0106 |
| GW-0200 | T-0200, T-0205 |
| GW-0201 | T-0201, T-0205 |
| GW-0202 | T-0202, T-0205 |
| GW-0203 | T-0203, T-0205 |
| GW-0204 | T-0204, T-0205 |
| GW-0300 | T-0300 |
| GW-0301 | T-0301 |
| GW-0302 | T-0302 |
| GW-0400 | T-0400, T-0401 |
| GW-0401 | T-0402, T-0403, T-0404 |
| GW-0402 | T-0405, T-0406 |
| GW-0403 | T-0407 |
| GW-0500 | T-0500 |
| GW-0501 | T-0501, T-0502, T-0503 |
| GW-0502 | T-0504 |
| GW-0503 | T-0505, T-0506 |
| GW-0504 | T-0507, T-0508, T-0509 |
| GW-0505 | T-0500 |
| GW-0506 | T-0510, T-0511 |
| GW-0507 | T-0512 |
| GW-0508 | T-0513 |
| GW-0600 | T-0600, T-0601, T-0602 |
| GW-0601 | T-0602, T-0603 |
| GW-0601a | *(verified by storage implementation tests)* |
| GW-0602 | T-0604, T-0605, T-0606, T-0607, T-1004 |
| GW-0603 | T-0608 |
| GW-0700 | T-0700 |
| GW-0701 | T-0201, T-0701 |
| GW-0702 | T-0702 |
| GW-0703 | T-0703, T-0704 |
| GW-0704 | T-0801 *(registration side; USB provisioning is CLI-level)* |
| GW-0705 | T-0802 *(removal side; USB reset is CLI-level)* |
| GW-0800 | T-0800 |
| GW-0801 | T-0801, T-0802 |
| GW-0802 | T-0803, T-0804, T-0805 |
| GW-0803 | T-0805, T-0806, T-0807, T-0808 |
| GW-0804 | T-0809 |
| GW-0805 | T-0810 |
| GW-0806 | *(validated by CLI integration tests against a running gateway)* |
| GW-1000 | T-1000 |
| GW-1001 | T-1002 |
| GW-1002 | T-0609 |
| GW-1003 | T-1003 |
| GW-1004 | T-1001 |
| GW-1100 | T-1100, T-1101, T-1102, T-1108 |
| GW-1101 | T-1103, T-1104, T-1108 |
| GW-1102 | T-1105, T-1106 |
| GW-1103 | T-1107 |
