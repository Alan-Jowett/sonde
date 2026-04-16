<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Node Validation Specification

> **Document status:** Draft  
> **Scope:** Integration and system-level test plan for the Sonde node firmware.  
> **Audience:** Implementers (human or LLM agent) writing node firmware tests.  
> **Related:** [node-requirements.md](node-requirements.md), [node-design.md](node-design.md), [protocol.md](protocol.md)

---

## 1  Overview

This document defines integration test cases that validate the node firmware against the requirements in [node-requirements.md](node-requirements.md). Each test case is traceable to one or more requirements.

**Scope:** These are integration tests that exercise the firmware through its external interfaces (transport, flash partitions, BPF execution). Unit tests for internal modules are expected but are not specified here.

**Test harness:** Tests run on the target hardware (ESP32-C3/S3) or a host-based simulation with a **mock gateway** (responds to node frames with valid authenticated responses) and **mock HAL** (simulates bus peripherals for BPF helper testing).

---

## 2  Test environment

### 2.1  Mock gateway

A test fixture that simulates the gateway side of the protocol:

- Listens for inbound frames from the node.
- Decrypts and authenticates inbound frames using `sonde-protocol` AES-256-GCM.
- Responds with configurable COMMAND, CHUNK, and APP_DATA_REPLY messages.
- Tracks received frames for assertion (message types, sequence numbers, payloads).
- Supports configurable behaviors: delay responses, drop responses (simulate timeout), send invalid AEAD ciphertext, send wrong nonce/seq.

### 2.2  Mock HAL

A stub implementation of the HAL bus drivers:

- I2C: configurable read/write responses per device address.
- SPI: configurable transfer responses.
- GPIO: configurable pin state.
- ADC: configurable channel readings.
- All stubs record calls for assertion.

### 2.3  Flash simulation

For host-based testing, flash partitions (key, program_a, program_b, schedule) are backed by in-memory buffers or temp files.

### 2.4  Test program library

A set of pre-compiled BPF programs (as CBOR program images) for testing:

- `nop_program` — returns 0, no helpers called.
- `send_program` — calls `send()` with a fixed blob.
- `send_recv_program` — calls `send_recv()` and checks the reply.
- `map_program` — reads and writes a map.
- `early_wake_program` — calls `set_next_wake(10)`.
- `oversized_map_program` — declares maps exceeding the memory budget.
- `deep_call_program` — BPF-to-BPF calls at max depth (8 frames).
- `budget_exceeded_program` — runs more instructions than the budget allows.

---

## 3  Protocol and communication tests

### T-N100  No transmission when unpaired

**Validates:** ND-0100, ND-0400

**Procedure:**
1. Boot the node with an erased key partition (no PSK).
2. Wait 5 seconds.
3. Assert: no frames transmitted.
4. Assert: node enters deep sleep.

---

### T-N101  Valid CBOR encoding

**Validates:** ND-0101

**Procedure:**
1. Boot the node with a valid PSK and program.
2. Capture the WAKE frame.
3. Decode the CBOR payload.
4. Assert: payload is valid CBOR with integer keys matching the protocol key map.

---

### T-N102  Frame format compliance

**Validates:** ND-0102

**Procedure:**
1. Capture two WAKE frames from separate wake cycles.
2. Assert: bytes 0–1 = key_hint (big-endian).
3. Assert: byte 2 = `MSG_WAKE` (0x01).
4. Assert: bytes 3–10 = nonce (big-endian 64-bit value; verify it differs between the two frames).
5. Assert: last 16 bytes = valid AES-256-GCM authentication tag; frame decrypts successfully with the node's PSK.
6. Assert: total length ≤ 250 bytes.

---

### T-N103  Frame size enforcement

**Validates:** ND-0103

**Procedure:**
1. Compute the maximum allowed `blob` length: encode an APP_DATA CBOR map with an empty blob, measure the CBOR overhead, then `max_blob_len = 207 − cbor_overhead`.
2. Install `send_program` that calls `send()` with a blob of exactly `max_blob_len` bytes.
3. Capture the APP_DATA frame.
4. Assert: CBOR payload length ≤ 207 bytes.
5. Assert: total frame length ≤ 250 bytes.

---

### T-N104  Oversized blob rejected

**Validates:** ND-0103

**Procedure:**
1. Install a BPF program that calls `send()` with a blob that would exceed the frame budget.
2. Assert: `send()` returns a negative error code.
3. Assert: no APP_DATA frame is transmitted.

---

## 4  Wake cycle tests

### T-N200  Normal wake cycle

**Validates:** ND-0200

**Procedure:**
1. Boot node. Mock gateway responds with NOP.
2. Assert: node sends WAKE, receives COMMAND, executes BPF, sleeps.
3. Assert: exactly one WAKE sent, exactly one COMMAND processed.

---

### T-N201  Wake cycle — no gateway response

**Validates:** ND-0200, ND-0700

**Procedure:**
1. Boot node. Mock gateway does not respond.
2. Assert: node sends WAKE, retries up to 3 times (~600 ms apart: 200 ms response timeout + 400 ms backoff).
3. Assert: after 3 failures, node sleeps without executing BPF.

---

### T-N202  WAKE message fields

**Validates:** ND-0201

**Procedure:**
1. Install a known program (hash X). Set battery to 3300 mV via mock ADC.
2. Boot node.
3. Capture WAKE frame, parse the fixed 11-byte frame header, and decode the CBOR payload.
4. Assert: `firmware_abi_version` matches firmware ABI.
5. Assert: `program_hash` = hash X.
6. Assert: `battery_mv` = 3300.
7. Assert: `firmware_version` is a valid semantic version string matching the compiled firmware version.
8. Assert: the WAKE frame header `nonce` field (in the fixed 11-byte header, not the CBOR payload) is present and sourced from the hardware RNG.

---

### T-N203  WAKE with no program installed

**Validates:** ND-0201

**Procedure:**
1. Boot node with erased program partitions.
2. Capture WAKE.
3. Assert: `program_hash` is zero-length.

---

### T-N204  COMMAND NOP processing

**Validates:** ND-0202

**Procedure:**
1. Mock gateway responds with NOP, `starting_seq = S`, `timestamp_ms = T`.
2. Assert: node proceeds to BPF execution.
3. Assert: node uses `S` as sequence number for first post-WAKE message.

---

### T-N205  COMMAND UPDATE_SCHEDULE

**Validates:** ND-0202, ND-0203

**Procedure:**
1. Mock gateway responds with UPDATE_SCHEDULE, `interval_s = 120`.
2. Assert: node stores new interval.
3. Assert: node sleeps for 120 seconds (verified via sleep manager).

---

### T-N206  COMMAND REBOOT

**Validates:** ND-0202

**Procedure:**
1. Mock gateway responds with REBOOT.
2. Assert: node restarts firmware.

---

### T-N207  COMMAND unknown type

**Validates:** ND-0202

**Procedure:**
1. Mock gateway responds with `command_type = 0xFF` (unknown).
2. Assert: node treats it as NOP and proceeds to BPF execution.

---

### T-N208  Sleep interval with set_next_wake

**Validates:** ND-0203

**Procedure:**
1. Set base interval to 300s. Install `early_wake_program` (calls `set_next_wake(10)`).
2. Run wake cycle.
3. Assert: node sleeps for `min(10, 300) = 10` seconds.
4. On next wake, assert: node sleeps for 300s (base interval restored).

---

### T-N209  set_next_wake cannot extend interval

**Validates:** ND-0203

**Procedure:**
1. Set base interval to 60s. Install program that calls `set_next_wake(600)`.
2. Run wake cycle.
3. Assert: node sleeps for 60s (not 600s).

---

## 5  Authentication and replay protection tests

### T-N300  AEAD on outbound frames

**Traceability:** ND-0300

**Procedure:**
1. Capture any outbound frame.
2. Verify AES-256-GCM decryption succeeds using the node's PSK.
3. Assert: frame uses AEAD wire format (11B header + ciphertext + 16B GCM tag).

---

### T-N301  Invalid AEAD ciphertext rejected

**Validates:** ND-0301

**Procedure:**
1. Mock gateway sends COMMAND with corrupted ciphertext (AEAD authentication fails).
2. Assert: node discards the frame.
3. Assert: node retries WAKE (treats it as no response).

---

### T-N302  Response binding — correct nonce echoed

**Validates:** ND-0302

**Procedure:**
1. Node sends WAKE with nonce N.
2. Mock gateway responds with COMMAND echoing nonce N.
3. Assert: node accepts the COMMAND.

---

### T-N303  Response binding — wrong nonce rejected

**Validates:** ND-0302

**Procedure:**
1. Node sends WAKE with nonce N.
2. Mock gateway responds with COMMAND echoing nonce N+1.
3. Assert: node discards the response.

---

### T-N304  Response binding — wrong seq on CHUNK

**Validates:** ND-0302

**Procedure:**
1. During chunked transfer, node sends GET_CHUNK with seq S.
2. Mock gateway responds with CHUNK echoing seq S+1.
3. Assert: node discards the CHUNK.

---

### T-N305  Sequence number management

**Validates:** ND-0303

**Procedure:**
1. Mock gateway assigns `starting_seq = 1000`.
2. Node sends GET_CHUNK (seq=1000), APP_DATA (seq=1001), APP_DATA (seq=1002).
3. Assert: sequence numbers increment by 1 for each outbound message.
4. Node sleeps. On next wake, mock gateway assigns `starting_seq = 5000`.
5. Assert: the first outbound message in the second wake cycle uses seq=5000, not a continuation from the first cycle.
6. Assert: no sequence state is persisted across deep sleep (ND-0303 AC3).
7. Perform a third wake cycle with `starting_seq = 2000`. Assert: the node uses seq=2000, confirming cross-sleep isolation from both prior cycles.

---

### T-N306  Nonce uniqueness

**Validates:** ND-0304

**Procedure:**
1. Run 1000 wake cycles.
2. Collect the WAKE nonce from each cycle.
3. Assert: no duplicates.

---

## 6  Key storage and provisioning tests

### T-N400  PSK storage and retrieval

**Validates:** ND-0400

**Procedure:**
1. Write a known PSK to the key partition.
2. Boot the node.
3. Assert: outbound WAKE frame is authenticated with the stored PSK.

---

### T-N401  Unpaired node does not communicate

**Validates:** ND-0400

**Procedure:**
1. Erase the key partition.
2. Boot the node.
3. Assert: no ESP-NOW frames transmitted. Node enters BLE pairing mode (ND-0900 boot path 1).

---

### T-N402  Full onboarding to wake cycle

**Validates:** ND-0900, ND-0906, ND-0909, ND-0913, ND-0914

**Procedure:**
1. Boot an unpaired node; assert it enters BLE pairing mode (boot path 1).
2. Provision the node via NODE_PROVISION with valid credentials; assert NODE_ACK(0x00).
3. Disconnect BLE; node reboots.
4. Assert: node enters PEER_REQUEST path (boot path 2 — PSK stored, `reg_complete` not set).
5. Mock gateway responds with a valid PEER_ACK; assert `reg_complete` is set in NVS.
6. Node reboots; assert it enters normal WAKE cycle (boot path 3).
7. Mock gateway responds with a valid COMMAND.
8. Assert: `peer_payload` is erased from NVS after the first successful WAKE/COMMAND exchange (ND-0914).

---

### T-N403  Same-session re-provision overwrites credentials

**Validates:** ND-0905, ND-0907

**Procedure:**
1. Boot an unpaired node into BLE pairing mode.
2. Send NODE_PROVISION with credentials A; assert NODE_ACK(0x00).
3. Without disconnecting, send NODE_PROVISION with credentials B on the same BLE connection.
4. Assert: NODE_ACK(0x00) returned for the second provision.
5. Assert: NVS contains credentials B (credentials A are overwritten).
6. Assert: `reg_complete` flag is cleared (ready for PEER_REQUEST on next boot).

---

### T-N404  Factory reset

**Validates:** ND-0402

**Procedure:**
1. Node has PSK, program, and map data.
2. Trigger factory reset.
3. Assert: key partition is erased (no magic bytes).
4. Assert: program partitions are erased.
5. Assert: map data in RTC SRAM is zeroed.
6. Boot the node.
7. Assert: node does not communicate (unpaired).

---

## 7  Program transfer and execution tests

### T-N500  Complete chunked transfer

**Validates:** ND-0500

**Procedure:**
1. Mock gateway responds with UPDATE_PROGRAM (4 chunks).
2. Assert: node sends GET_CHUNK for indices 0, 1, 2, 3 in order.
3. Assert: each GET_CHUNK uses an incrementing sequence number.
4. Mock gateway responds with correct CHUNK data for each.
5. Assert: node sends PROGRAM_ACK with the correct hash.

---

### T-N501  Program hash verification — pass

**Validates:** ND-0501

**Procedure:**
1. Complete chunked transfer with matching hash.
2. Assert: PROGRAM_ACK sent, program installed.

---

### T-N502  Program hash verification — fail

**Validates:** ND-0501

**Procedure:**
1. Mock gateway sends chunks that produce the wrong hash.
2. Assert: no PROGRAM_ACK sent.
3. Assert: program is discarded, old program remains active.
4. Assert: node sleeps.

---

### T-N503  Program image decoding

**Validates:** ND-0501a

**Procedure:**
1. Transfer a program image with 2 map definitions.
2. Assert: bytecode is extracted correctly.
3. Assert: 2 maps are allocated in RTC SRAM with correct sizes.
4. Assert: LDDW `src=1` instructions are resolved to valid map pointers.

---

### T-N504  A/B partition atomic update

**Validates:** ND-0502

**Procedure:**
1. Install program A (active on partition A).
2. Begin transfer of program B (written to partition B).
3. Simulate power loss mid-transfer (abort before hash verification).
4. Boot node.
5. Assert: program A is still active (partition A untouched).

---

### T-N505  Ephemeral program — RAM storage

**Validates:** ND-0503

**Procedure:**
1. Mock gateway responds with RUN_EPHEMERAL.
2. Complete transfer.
3. Assert: program is stored in RAM, not flash.
4. Assert: program executes.
5. Assert: after execution, ephemeral program memory is freed.
6. Assert: resident program is unaffected.

---

### T-N506  BPF execution — basic

**Validates:** ND-0504

**Procedure:**
1. Install `nop_program`.
2. Run wake cycle.
3. Assert: BPF program executes and returns 0.

---

### T-N507  Execution context

**Validates:** ND-0505

**Procedure:**
1. Mock gateway sends `timestamp_ms = 1710000000000`.
2. Install a program that reads `ctx->timestamp`, `ctx->battery_mv`, `ctx->firmware_abi_version`, `ctx->wake_reason` and sends them via `send()`.
3. Assert: `timestamp` ≈ 1710000000000 + elapsed time since COMMAND was processed (within a few ms tolerance).
4. Assert: `battery_mv` matches ADC reading.
5. Assert: `firmware_abi_version` matches firmware.
6. Assert: `wake_reason` = `WAKE_SCHEDULED` (0x00).

---

### T-N508  Wake reason — program update

**Validates:** ND-0505, ND-0506

**Procedure:**
1. Complete a program update.
2. New program reads `ctx->wake_reason` and sends it.
3. Assert: `wake_reason` = `WAKE_PROGRAM_UPDATE` (0x02).

---

### T-N509  Wake reason — early wake

**Validates:** ND-0505

**Procedure:**
1. Install `early_wake_program` (calls `set_next_wake(10)`).
2. Node wakes early.
3. Program reads `ctx->wake_reason` and sends it.
4. Assert: `wake_reason` = `WAKE_EARLY` (0x01).

---

### T-N510  Post-update immediate execution

**Validates:** ND-0506

**Procedure:**
1. Install a new program via chunked transfer.
2. Assert: new program executes in the same wake cycle (after PROGRAM_ACK).
3. Assert: node does not sleep between PROGRAM_ACK and execution.

---

## 8  BPF environment tests

### T-N600  Bus helpers — I2C read/write

**Validates:** ND-0601

**Procedure:**
1. Configure mock I2C device at bus 0, addr 0x48 to return `[0x1A, 0x2B]`.
2. Install program that calls `i2c_read(handle, buf, 2)`.
3. Assert: program receives `[0x1A, 0x2B]`.

---

### T-N601  Bus helpers — I2C error

**Validates:** ND-0601

**Procedure:**
1. Configure mock I2C to return NACK.
2. Install program that calls `i2c_read()`.
3. Assert: helper returns negative value.

---

### T-N602  Bus helpers — SPI transfer

**Validates:** ND-0601

**Procedure:**
1. Configure mock SPI to echo transmitted bytes.
2. Fill a buffer with test data, call `spi_transfer()` with the buffer.
3. Assert: buffer contents after the call match the original test data (echo).

---

### T-N603  Bus helpers — GPIO and ADC

**Validates:** ND-0601

**Procedure:**
1. Configure mock GPIO pin 5 = high. Configure mock ADC channel 0 = 2048.
2. Install program that reads GPIO and ADC.
3. Assert: `gpio_read(5)` returns 1, `adc_read(0)` returns 2048.

---

### T-N604  Communication — send()

**Validates:** ND-0602

**Procedure:**
1. Install `send_program` that calls `send([0xAA, 0xBB])`.
2. Run wake cycle.
3. Assert: APP_DATA frame captured with blob `[0xAA, 0xBB]`.
4. Assert: no APP_DATA_REPLY expected (fire-and-forget).

---

### T-N605  Communication — send_recv()

**Validates:** ND-0602

**Procedure:**
1. Install `send_recv_program`.
2. Mock gateway replies with `[0xCC, 0xDD]`.
3. Assert: APP_DATA sent, APP_DATA_REPLY received.
4. Assert: program receives `[0xCC, 0xDD]` as the reply.

---

### T-N606  Communication — send_recv() timeout

**Validates:** ND-0602

**Procedure:**
1. Install `send_recv_program`. Mock gateway does not reply.
2. Assert: `send_recv()` returns negative (timeout).
3. Assert: program continues execution.

---

### T-N606a  BPF send() produces AEAD-authenticated frame

**Validates:** ND-0602, ND-0300

**Procedure:**
1. Install a program that calls `send([0xAA, 0xBB])`.
2. Run wake cycle with AEAD providers installed (via `install`).
3. Capture the outbound APP_DATA frame from the transport.
4. Assert: the frame has the AEAD wire format (11B header + ciphertext + 16B GCM tag), NOT the legacy format (11B header + plaintext + 32B tag).
5. Assert: `decode_frame()` + `open_frame()` successfully decrypts the frame using the node's PSK.
6. Assert: the decrypted CBOR contains AppData with blob `[0xAA, 0xBB]`.

---

### T-N606b  BPF send_recv() AEAD round-trip

**Validates:** ND-0602, ND-0300

**Procedure:**
1. Install a program that calls `send_recv([0xAA, 0xBB])`.
2. Run wake cycle with AEAD providers installed. Pre-queue an AEAD-encrypted APP_DATA_REPLY on the transport.
3. Assert: the outbound APP_DATA frame is AEAD-authenticated (same checks as T-N606a).
4. Assert: the node successfully decrypts the AEAD APP_DATA_REPLY and `send_recv()` returns the reply data.

---

### T-N607  Map operations — read/write

**Validates:** ND-0603

**Procedure:**
1. Install `map_program` that writes value 42 to key 0, then reads it back.
2. Run wake cycle.
3. Assert: program reads back 42.

---

### T-N608  Map persistence across sleep

**Validates:** ND-0603

**Procedure:**
1. Install `map_program` that writes value 42.
2. Run wake cycle. Node sleeps.
3. Node wakes. Program reads the map.
4. Assert: value is still 42 (survived deep sleep).

---

### T-N609  Ephemeral map_update_elem rejected

**Validates:** ND-0603

**Procedure:**
1. Run an ephemeral program that calls `map_update_elem()`.
2. Assert: helper returns error.
3. Assert: map data is unchanged.

---

### T-N610  System helpers — get_time and get_battery_mv

**Validates:** ND-0604

**Procedure:**
1. Mock gateway sends `timestamp_ms = 1710000000000`.
2. Install program that calls `get_time()` and `get_battery_mv()`.
3. Assert: `get_time()` ≈ 1710000000000 + elapsed time since COMMAND was processed (within a few ms tolerance).
4. Assert: `get_battery_mv()` matches mock ADC.

---

### T-N611  System helpers — delay_us

**Validates:** ND-0604

**Procedure:**
1. Install program that calls `delay_us(1000)`.
2. Assert: execution pauses for approximately 1000 µs.

---

### T-N612  System helpers — set_next_wake ephemeral rejected

**Validates:** ND-0604

**Procedure:**
1. Run ephemeral program that calls `set_next_wake()`.
2. Assert: helper returns error.
3. Assert: sleep interval is unchanged.

---

### T-N613  System helpers — bpf_trace_printk

**Validates:** ND-0604, ND-1006

**Procedure:**
1. Install program that calls `bpf_trace_printk("hello")`.
2. Assert: log output includes a record for "hello" whose log level is INFO (for example, the line is tagged or prefixed as INFO, not DEBUG).

---

### T-N614  Execution constraints — instruction budget

**Validates:** ND-0605

**Procedure:**
1. Install `budget_exceeded_program`.
2. Run wake cycle.
3. Assert: BPF execution is terminated.
4. Assert: node sleeps normally (no crash).

---

### T-N615  Execution constraints — call depth

**Validates:** ND-0605

**Procedure:**
1. Install a program with BPF-to-BPF calls exceeding 8 frames.
2. Assert: program is terminated at runtime (or rejected by verifier).

---

### T-N616  Map memory budget enforcement

**Validates:** ND-0606

**Procedure:**
1. Transfer `oversized_map_program` (maps exceed RTC SRAM budget).
2. Assert: installation fails.
3. Assert: existing program remains active.

---

### T-N619  Global variable map (map_type 0) accepted

**Validates:** ND-0606

**Procedure:**
1. Build map definitions containing a `map_type` 0 map (global variable map from `.rodata`/`.data`).
2. Call `validate_map_defs()`.
3. Assert: validation succeeds.
4. Allocate the maps.
5. Assert: allocation succeeds and the map is usable (lookup/update work).

---

### T-N620  Unsupported map_type rejected

**Validates:** ND-0606

**Procedure:**
1. Build map definitions containing a `map_type` 2 map (unsupported type).
2. Call `validate_map_defs()`.
3. Assert: validation fails with `ProgramDecodeFailed`.

---

### T-N621  send_async queues data

**Validates:** ND-0602 (AC4), ND-0609

**Procedure:**
1. Install a BPF program that calls `send_async([0xCC, 0xDD])`.
2. Run wake cycle.
3. Assert: `send_async()` returns 0.
4. Assert: no APP_DATA frame is transmitted during this wake cycle.
5. Assert: data remains in queue.

---

### T-N622  send_async piggybacked on WAKE

**Validates:** ND-0610

**Procedure:**
1. Install program that calls `send_async([0xCC, 0xDD])`.
2. Complete wake cycle (data queued).
3. On next wake cycle, capture WAKE frame.
4. Assert: WAKE CBOR payload contains `blob` (key 10) with value `[0xCC, 0xDD]`.

---

### T-N623  send_async overflow to APP_DATA

**Validates:** ND-0610, ND-0611

**Procedure:**
1. Install program that calls `send_async()` twice (2 messages queued).
2. On next wake cycle, capture all frames.
3. Assert: WAKE does NOT contain `blob`.
4. Assert: 2 APP_DATA frames sent after COMMAND, each with correct sequence numbers.

---

### T-N624  send_async oversized falls back to APP_DATA

**Validates:** ND-0610

**Procedure:**
1. Install program that calls `send_async()` with a blob that exceeds the WAKE payload budget.
2. On next wake cycle, capture frames.
3. Assert: WAKE does NOT contain `blob`.
4. Assert: blob sent via APP_DATA after COMMAND.

---

### T-N625  send_async queue full returns error

**Validates:** ND-0613

**Procedure:**
1. Install program that calls `send_async()` 11 times with small blobs.
2. Assert: first 10 calls return 0.
3. Assert: 11th call returns -1.

---

### T-N626  send_async queue cleared after send

**Validates:** ND-0609

**Procedure:**
1. Call `send_async()` once.
2. Complete two wake cycles.
3. Assert: WAKE in second cycle has `blob`.
4. Assert: WAKE in third cycle does NOT have `blob` (queue was cleared after send).

---

### T-N627  send_async queue cleared on program load

**Validates:** ND-0609

**Procedure:**
1. Call `send_async()`.
2. Before next WAKE, trigger UPDATE_PROGRAM.
3. Assert: the new program's first WAKE does NOT contain the old program's queued `blob`.

---

### T-N627a  send_async rejects oversized blob with -2

**Validates:** ND-0602 (AC4)

**Procedure:**
1. Compute the maximum APP_DATA blob length (same as `send()` — 223 bytes minus CBOR map overhead).
2. Install a BPF program that calls `send_async()` with a blob of `max + 1` bytes.
3. Assert: `send_async()` returns -2.
4. Assert: the queue remains empty (no data piggybacked on next WAKE).

---

### T-N628  sonde_context downlink data populated

**Validates:** ND-0612

**Procedure:**
1. Install BPF program that reads `ctx->data_start` to `ctx->data_end`.
2. On the next WAKE, send NOP COMMAND with `blob` `[0xEE, 0xFF]`.
3. Assert: in that wake cycle, the BPF program finds `[0xEE, 0xFF]`.

---

### T-N629  sonde_context no downlink data

**Validates:** ND-0612

**Procedure:**
1. Install BPF program that reads `ctx->data_start` and `ctx->data_end`.
2. On the next WAKE, send NOP COMMAND without `blob` field.
3. Assert: BPF program's `ctx->data_start == 0` and `ctx->data_end == 0`.

---

### T-N630  Backward compatibility — send() unchanged

**Validates:** ND-0614

**Procedure:**
1. Install `send_program` that calls `send([0xAA, 0xBB])`.
2. Run wake cycle.
3. Assert: APP_DATA sent immediately during wake cycle.
4. Assert: existing behavior preserved exactly.

---

### T-N631  Backward compatibility — WAKE without blob accepted

**Validates:** ND-0614

**Procedure:**
1. Send WAKE without `blob` field (existing format).
2. Assert: gateway processes it normally and responds with COMMAND.

---

### T-N632  send_async from ephemeral program

**Validates:** ND-0609

**Procedure:**
1. Run ephemeral program that calls `send_async([0x01])`.
2. Assert: returns 0.
3. Complete wake cycle.
4. Assert: data piggybacked on next WAKE.

---

## 9  Timing and retry tests

### T-N700  WAKE retry count and timing

**Validates:** ND-0700

**Procedure:**
1. Mock gateway does not respond.
2. Assert: node sends exactly 4 WAKE frames (1 initial + 3 retries).
3. For each unanswered WAKE except the final one, assert the node waits up to 200 ms (`RESPONSE_TIMEOUT_MS`) for a reply, then delays 400 ms (`RETRY_DELAY_MS`) before the next WAKE, giving ~600 ms between successive transmissions on a timeout-only path.
4. Assert: node sleeps after final retry.

---

### T-N701  Chunk transfer retry

**Validates:** ND-0701

**Procedure:**
1. Mock gateway drops CHUNK response for chunk index 2.
2. Assert: node retries GET_CHUNK {2} up to 3 times.
3. If all retries fail: assert node aborts and sleeps.
4. On next wake: assert transfer restarts from chunk 0.

---

### T-N702  Response timeout

**Validates:** ND-0702

**Procedure:**
1. Mock gateway delays response by 250 ms (>200 ms timeout).
2. Assert: node treats it as timeout, waits 400 ms backoff (`RETRY_DELAY_MS`), then retries.

---

## 10  Error handling tests

### T-N800  Malformed CBOR — no crash

**Validates:** ND-0800

**Procedure:**
1. Mock gateway sends COMMAND with valid AEAD encryption but garbage CBOR payload (encrypted valid GCM tag over malformed content).
2. Assert: node discards the frame.
3. Assert: no crash, node retries or sleeps.

---

### T-N801  Unexpected msg_type

**Validates:** ND-0801

**Procedure:**
1. Mock gateway sends a frame with `msg_type = 0x99` (unknown) and valid AEAD encryption.
2. Assert: node discards the frame.

---

### T-N802  Chunk index mismatch

**Validates:** ND-0802

**Procedure:**
1. Node sends GET_CHUNK {3}.
2. Mock gateway responds with CHUNK {5} (wrong index).
3. Assert: node discards the response.
4. Assert: node retries GET_CHUNK {3}.

---

### T-N803  Stale COMMAND during chunk transfer

**Validates:** ND-0701 AC4, ND-0801

**Procedure:**
1. Queue a valid COMMAND (UpdateProgram) as the WAKE response.
2. Queue a stale COMMAND (Nop) — simulates a duplicate from a WAKE retry still in the receive buffer.
3. Queue the real CHUNK response immediately after the stale COMMAND.
4. Run the wake cycle.
5. Assert: the node successfully installs and executes the BPF program.
6. Assert: the stale COMMAND did not consume a retry attempt (transfer succeeds on first GET_CHUNK attempt).

---

## 11  BLE pairing and registration tests

### T-N900  Boot priority order

**Validates:** ND-0900

**Procedure:**
1. Configure node with no PSK, pairing button not held.
2. Assert: node enters BLE pairing mode.
3. Configure node with PSK stored, `reg_complete` not set.
4. Assert: node sends PEER_REQUEST.
5. Configure node with PSK stored, `reg_complete` set.
6. Assert: node enters normal WAKE cycle.

---

### T-N901  Pairing button detection

**Validates:** ND-0901

**Procedure:**
1. Hold pairing GPIO LOW for 600 ms after reset.
2. Assert: node detects button hold and enters BLE pairing mode.
3. Hold pairing GPIO LOW for 300 ms after reset.
4. Assert: node does NOT detect button hold.

---

### T-N902  BLE GATT service registration and advertising

**Validates:** ND-0902, ND-0903

**Procedure:**
1. Boot node into BLE pairing mode.
2. Scan for BLE advertisements from a test central.
3. Assert: advertisement contains Node Provisioning Service UUID `0000FE50-0000-1000-8000-00805F9B34FB`.
4. Assert: device name in the advertising payload matches `sonde-XXXX` where XXXX = last 4 hex digits of BLE MAC.
5. Connect and discover services.
6. Assert: Node Command characteristic `0000FE51-0000-1000-8000-00805F9B34FB` is present with Write+Indicate properties.
7. Assert: the GAP device name (read after connecting) matches `sonde-XXXX`, not the NimBLE default (`nimble`) (ND-0903 criterion 3).

---

### T-N903  MTU negotiation and LESC pairing

**Validates:** ND-0904

**Procedure:**
1. Connect to node in BLE pairing mode.
2. Request ATT MTU of 247.
3. Assert: negotiated MTU is ≥ 247.
4. Initiate LESC Just Works pairing.
5. Assert: pairing completes successfully.

### T-N903a  Server-initiated LESC pairing — passive client

**Validates:** ND-0904 (criterion 3)

**Procedure:**
1. Connect a BLE client to the node that does **not** initiate pairing on its own (plain GATT connect, no `createBond`).
2. Assert: the node initiates LESC pairing from the server side (the client receives an SMP Security Request).
3. Assert: LESC Just Works pairing completes successfully.

### T-N903b  Pre-auth GATT write buffered until authentication completes

**Validates:** ND-0904 (criterion 4)

**Procedure:**
1. Connect a BLE client to the node (plain GATT connect, no client-initiated pairing).
2. Immediately send a GATT write to the Node Command characteristic **before** LESC pairing completes.
3. Assert: the write is buffered, not discarded.
4. Allow server-initiated LESC pairing to complete.
5. Assert: the buffered write is processed after `authenticated` becomes true.
6. Assert: a `NODE_ACK` indication is sent in response.

---

### T-N904  NODE_PROVISION happy path

**Validates:** ND-0905, ND-0906

**Procedure:**
1. Boot unpaired node into BLE pairing mode.
2. Write NODE_PROVISION with valid `node_key_hint`, `node_psk`, `rf_channel`, `payload_len`, `encrypted_payload`.
3. Assert: node responds NODE_ACK(0x00).
4. Assert: NVS contains `psk`, `key_hint`, `channel`, `peer_payload` (see ND-0916 for key mapping).
5. Assert: `reg_complete` flag is cleared.

---

### T-N905  Same-session re-provision

**Validates:** ND-0905, ND-0907

**Procedure:**
1. Boot an unpaired node into BLE pairing mode. Send a first NODE_PROVISION with credentials A — assert NODE_ACK(0x00).
2. Without disconnecting, send a second NODE_PROVISION with credentials B (same BLE session, per ND-0907).
3. Assert: node responds NODE_ACK(0x00) (same-session re-provision is allowed).
4. Assert: NVS contains credentials B (overwritten).

---

### T-N906  NODE_PROVISION with pairing button — factory reset

**Validates:** ND-0905, ND-0917

**Procedure:**
1. Provision a node with credentials and a resident BPF program.
2. Reboot with pairing button held ≥ 500 ms.
3. Write NODE_PROVISION with new credentials.
4. Assert: existing PSK, persistent map data, and resident BPF program are erased before new credentials are written.
5. Assert: node responds NODE_ACK(0x00).
6. Assert: NVS contains only the new credentials.

---

### T-N907  NODE_PROVISION NVS write failure

**Validates:** ND-0908

**Procedure:**
1. Boot node into BLE pairing mode.
2. Inject an NVS write failure (e.g., mock NVS full).
3. Write NODE_PROVISION with valid payload.
4. Assert: node responds NODE_ACK(0x02).
5. Assert: no partial credentials remain in NVS.

---

### T-N908  BLE mode persistence and reboot on disconnect

**Validates:** ND-0907

**Procedure:**
1. Boot node into BLE pairing mode and connect.
2. Write NODE_PROVISION; receive NODE_ACK(0x00).
3. Write a second NODE_PROVISION on the same connection.
4. Assert: node accepts the second provision (remains in BLE mode).
5. Disconnect BLE.
6. Assert: node reboots.

---

### T-N909  PEER_REQUEST frame construction

**Validates:** ND-0909

**Procedure:**
1. Provision node via BLE, reboot (PSK stored, `reg_complete` not set).
2. Capture the transmitted PEER_REQUEST frame.
3. Assert: `msg_type` = 0x05.
4. Assert: nonce is exactly 8 bytes and is sourced from the RNG abstraction (verified via mock RNG in test). Assert it is not a fixed constant (e.g., not always zero).
5. Assert: CBOR payload decodes to `{1: <value>}` where the value matches NVS key `peer_payload`.
6. Assert: AES-256-GCM authenticated encryption verifies with the PSK from NVS key `psk`.
7. Assert: ESP-NOW channel matches NVS key `channel`.

---

### T-N910  PEER_REQUEST retransmission across wake cycles

**Validates:** ND-0910

**Procedure:**
1. Provision node; do not run a gateway (no PEER_ACK).
2. Allow two wake cycles to elapse.
3. Assert: PEER_REQUEST is transmitted on each wake cycle.
4. Assert: interval between transmissions matches configured wake interval.

---

### T-N911  PEER_ACK timeout — deep sleep

**Validates:** ND-0911

**Procedure:**
1. Provision node; let it transmit PEER_REQUEST.
2. Do not send PEER_ACK.
3. Assert: node listens for at least 10 seconds (±500 ms) after end of PEER_REQUEST transmission.
4. Assert: node enters deep sleep after the listen window expires.

---

### T-N912  PEER_ACK verification happy path

**Validates:** ND-0912

**Procedure:**
1. Node sends PEER_REQUEST with nonce N.
2. Mock gateway responds with PEER_ACK: valid AES-256-GCM encryption with `node_psk`, nonce = N.
3. Assert: node accepts the PEER_ACK.

---

### T-N913  PEER_ACK with wrong nonce — discard

**Validates:** ND-0912

**Procedure:**
1. Node sends PEER_REQUEST with nonce N.
2. Mock gateway responds with PEER_ACK containing nonce N+1 (mismatch).
3. Assert: node discards the PEER_ACK.
4. Assert: `reg_complete` flag is NOT set.

---

### T-N914  PEER_ACK with wrong registration proof — discard

**Validates:** ND-0912

**Procedure:**
1. Node sends PEER_REQUEST with nonce N.
2. Mock gateway responds with PEER_ACK containing correct nonce but incorrect `registration_proof`.
3. Assert: node discards the PEER_ACK.
4. Assert: `reg_complete` flag is NOT set.

---

### T-N915  Valid PEER_ACK sets registration-complete flag

**Validates:** ND-0913

**Procedure:**
1. Node sends PEER_REQUEST.
2. Mock gateway responds with a valid PEER_ACK.
3. Assert: `reg_complete` flag is set in NVS.
4. Assert: `peer_payload` is still present in NVS.

---

### T-N916  First successful WAKE/COMMAND erases encrypted payload (`peer_payload`)

**Validates:** ND-0914

**Procedure:**
1. Complete BLE pairing and registration (PEER_ACK accepted, `reg_complete` set).
2. Reboot node; node enters normal WAKE cycle.
3. Mock gateway responds with a valid COMMAND to the WAKE.
4. Assert: `peer_payload` is erased from NVS after the COMMAND is processed.

---

### T-N917  WAKE failure after registration — revert to PEER_REQUEST

**Validates:** ND-0915

**Procedure:**
1. Complete BLE pairing and registration (`reg_complete` set).
2. Reboot node; node sends WAKE.
3. Mock gateway does not respond (or responds with invalid AEAD ciphertext).
4. Assert: `reg_complete` flag is cleared.
5. Assert: on next boot the node sends PEER_REQUEST instead of WAKE.

---

## 11  Operational logging

### T-N1000  Boot reason log — power-on

**Validates:** ND-1000

**Procedure:**
1. Cold-boot the node (power-on reset, not deep-sleep wake).
2. Capture serial output.
3. Assert: an INFO log line contains "boot_reason=power_on".

---

### T-N1001  Boot reason log — deep-sleep wake

**Validates:** ND-1000

**Procedure:**
1. Boot the node from deep sleep (previous cycle entered deep sleep).
2. Capture serial output.
3. Assert: an INFO log line contains "boot_reason=deep_sleep_wake".

---

### T-N1002  Wake cycle started log

**Validates:** ND-1001

**Procedure:**
1. Pair the node and let it enter a normal wake cycle.
2. Assert: an INFO log is emitted containing "wake cycle started" with `key_hint` and `wake_reason`.

---

### T-N1003  WAKE frame sent log

**Validates:** ND-1002

**Procedure:**
1. Run a wake cycle against a mock gateway.
2. Assert: an INFO log is emitted containing "WAKE sent" with `key_hint` and `nonce` (hex).

---

### T-N1004  COMMAND received log

**Validates:** ND-1003

**Procedure:**
1. Mock gateway responds with a valid COMMAND (Nop).
2. Assert: an INFO log is emitted containing "COMMAND received" and `command_type=Nop`.

---

### T-N1005  PEER_REQUEST sent log

**Validates:** ND-1004

**Procedure:**
1. Node has PSK but `reg_complete` is not set (PEER_REQUEST path).
2. Assert: an INFO log is emitted containing "PEER_REQUEST sent" with `key_hint`.

---

### T-N1006  PEER_ACK received log

**Validates:** ND-1005

**Procedure:**
1. Mock gateway responds with a valid PEER_ACK.
2. Assert: an INFO log is emitted containing "PEER_ACK received" and "registration complete".

---

### T-N1007  BPF execution log — success

**Validates:** ND-1006

**Procedure:**
1. Run a wake cycle with a valid resident program.
2. Assert: an INFO log is emitted containing "BPF execute" and `program_hash` (hex prefix).
3. Assert: an INFO log is emitted containing the execution result.

---

### T-N1014  bpf_trace_printk emitted at INFO level

> **Naming note:** This test validates ND-1006. The ID T-N1014 follows the
> sequential allocation order in which it was added; it is not named
> T-N1006 because that ID is already assigned to a different test case.

**Validates:** ND-1006

**Procedure:**
1. Install program that calls `bpf_trace_printk("hello")`.
2. Run wake cycle.
3. Assert: an INFO log is emitted containing `bpf_trace_printk: hello`.
4. Assert: the log entry containing `bpf_trace_printk: hello` is tagged at INFO level (not DEBUG).

---

### T-N1015  BPF helper I/O logging at DEBUG level

> **Naming note:** This test validates ND-1010. The ID T-N1015 follows the
> sequential allocation order in which it was added; it is not named
> T-N1010 because that ID is already assigned to a different test case.

**Validates:** ND-1010

**Procedure:**
1. Configure the node so that ESP-IDF logging for BPF helper logs is set to DEBUG (for example, by setting the default log level to DEBUG or calling `esp_log_level_set` for the helper log tag in the test firmware).
2. Install program that calls `i2c_read`, `send`, `gpio_read`, `i2c_write`, or `adc_read`.
3. Run wake cycle.
4. Assert: a DEBUG log is emitted for each I/O helper call containing the helper name and `result=`.

---

### T-N1016  GPIO state after sleep preparation

**Validates:** ND-1013

**Procedure:**
1. Provision a node with non-default I2C pins (e.g., SDA=5, SCL=6).
2. Install a BPF program that calls `gpio_write` on an additional output GPIO (e.g., GPIO 7).
3. Run a complete wake cycle so that I2C and GPIO peripherals are active.
4. Assert: `prepare_for_sleep()` is called before deep sleep entry.
5. Assert: I2C SDA and SCL GPIOs are reset to disabled/high-impedance with no pull resistors.
6. Assert: the BPF-configured output GPIO is reset to a disabled state.
7. Assert: the RTC wake-up GPIO (pairing button) retains its configured state.

---

### T-N1008  Deep sleep entered log

**Validates:** ND-1007

**Procedure:**
1. Run a wake cycle to completion.
2. Assert: an INFO log is emitted containing "entering deep sleep" with `duration_seconds` and `reason`.

---

### T-N1009  RNG failure WARN log

**Validates:** ND-1009

**Procedure:**
1. Configure mock RNG to fail health check.
2. Run `run_wake_cycle`.
3. Assert: a WARN log is emitted containing "RNG health check failed".

---

### T-N1010  WAKE retries exhausted WARN log

**Validates:** ND-1009

**Procedure:**
1. Configure mock gateway to never respond (all timeouts).
2. Run `run_wake_cycle`.
3. Assert: a WARN log is emitted containing "WAKE/COMMAND failed".

---

### T-N1011  AEAD verification failure WARN log

**Validates:** ND-1009

**Procedure:**
1. Configure mock gateway to respond with a frame bearing corrupted AEAD ciphertext.
2. Run `run_wake_cycle`.
3. Assert: a WARN log is emitted containing "COMMAND verification failed".

---

### T-N1012  BLE pairing mode entry log

**Validates:** ND-1008

**Procedure:**
1. Boot the node with no PSK provisioned (or with the pairing button held).
2. Capture serial output.
3. Assert: an INFO log is emitted containing "entering BLE pairing mode".

---

### T-N1013  BLE pairing mode exit log

**Validates:** ND-1008

**Procedure:**
1. Complete or abort a BLE pairing session.
2. Capture serial output.
3. Assert: a log at INFO or WARN level is emitted containing "BLE pairing mode exited" or "BLE pairing mode failed".

---

### T-N1017  GET_CHUNK request logged at DEBUG

**Validates:** ND-1011

**Procedure:**
1. Build a verbose firmware variant.
2. Trigger a program transfer so the node sends `GET_CHUNK` requests.
3. Capture serial output.
4. Assert: a DEBUG log is emitted for each `GET_CHUNK` request, including `chunk_index` and `attempt`.

### T-N1018  CHUNK response logged at DEBUG

**Validates:** ND-1011

**Procedure:**
1. Build a verbose firmware variant.
2. Complete a chunked program transfer.
3. Capture serial output.
4. Assert: a DEBUG log is emitted for each `CHUNK` response received, including `chunk_index` and data length.

### T-N1019  Build-type quiet strips INFO call-sites

**Validates:** ND-1012

**Procedure:**
1. Build the node firmware with the default `quiet` feature (no `verbose`).
2. Boot the node and complete a wake cycle.
3. Capture serial output.
4. Assert: no INFO-level log lines appear (only WARN and ERROR).
5. Assert: the wake cycle completes successfully despite the absence of INFO logging.

### T-N1020  Build-type verbose retains INFO and DEBUG

**Validates:** ND-1012

**Procedure:**
1. Build the node firmware with `--features esp,verbose --no-default-features`.
2. Boot the node and complete a wake cycle.
3. Capture serial output.
4. Assert: INFO-level operational logs (boot reason, wake cycle, WAKE sent) are present.
5. Assert: DEBUG-level logs are visible when triggered (e.g., BPF helper I/O).

### T-N1021  Error diagnostic includes operation and subsystem error

**Validates:** ND-1014

**Procedure:**
1. Trigger an operator-visible error (e.g., NVS read failure by corrupting NVS partition, or AEAD decryption failure with wrong PSK).
2. Capture serial output.
3. Assert: the error log includes the failed operation name (e.g., `"AEAD decryption"`, `"NVS read"`).
4. Assert: the error log includes the underlying subsystem error (e.g., ESP-IDF error code).
5. Assert: no secret key material (PSK bytes, passwords) appears in the log.

### T-N1022  Boot commit hash and ABI version at WARN level

**Validates:** ND-1015

**Procedure:**
1. Build a quiet firmware variant (default release profile).
2. Boot the node.
3. Capture serial output.
4. Assert: a WARN-level log line contains the firmware commit hash.
5. Assert: a WARN-level log line contains the ABI version.

### T-N1023  ESP-NOW channel logged at WARN level during boot

**Validates:** ND-1016

**Procedure:**
1. Build a quiet firmware variant (default release profile).
2. Boot the node with a provisioned channel (e.g., channel 6).
3. Capture serial output.
4. Assert: a WARN-level log line contains the ESP-NOW channel number before transport initialization.

---

### T-N918  NVS layout includes BLE pairing fields

**Validates:** ND-0916

**Procedure:**
1. Provision node via BLE with a known `encrypted_payload`.
2. Read NVS contents.
3. Assert: `peer_payload` key exists and contains the expected blob.
4. Assert: `reg_complete` key exists as a `u32` value.
5. Assert: existing NVS keys (`magic`, `key_hint`, `psk`, `channel`, `interval`, `active_p`, `prog_a`, `prog_b`) are unaffected.

---

### T-N919  Unknown CBOR keys ignored

**Validates:** ND-0101

**Procedure:**
1. Send an inbound message containing unknown CBOR integer keys alongside valid fields.
2. Assert: node processes the message normally.
3. Assert: unknown keys do not cause an error or affect behavior.

---

### T-N920  `send_recv()` rejects oversized blob

**Validates:** ND-0103

**Procedure:**
1. Load a BPF program that calls `send_recv()` with a blob whose length exceeds
   the maximum allowed blob length derived from the 250-byte APP_DATA frame
   budget (see T-N103 `max_blob_len`).
2. Execute the program.
3. Assert: `send_recv()` returns an error code.
4. Assert: no APP_DATA frame is transmitted.

---

### T-N921  Duplicate COMMAND during BPF execution discarded

**Validates:** ND-0200

**Procedure:**
1. Complete WAKE handshake; node receives COMMAND and begins BPF execution.
2. Inject a second valid COMMAND frame during BPF execution.
3. Assert: the second COMMAND is discarded.
4. Assert: BPF execution completes using the original COMMAND context.

---

### T-N922  COMMAND `timestamp_ms` populates `sonde_context`

**Validates:** ND-0202

**Procedure:**
1. Mock gateway sends COMMAND with a known `timestamp_ms` value T.
2. Node executes BPF program.
3. Assert: `sonde_context.timestamp` (milliseconds since epoch) is derived from T plus local elapsed time.

---

### T-N923  `set_next_wake()` one-shot then restore base interval

**Validates:** ND-0203

**Procedure:**
1. Configure node with a base wake interval of 60 s.
2. BPF program calls `set_next_wake(5000)` (5 s).
3. Assert: node sleeps for approximately 5 s.
4. Node wakes and completes the next cycle without calling `set_next_wake()`.
5. Assert: node sleeps for the base interval (60 s).

---

### T-N924  Invalid AEAD COMMAND — silent discard, no diagnostic frame

**Validates:** ND-0301

**Procedure:**
1. Node sends WAKE; mock gateway responds with a COMMAND bearing invalid AEAD ciphertext.
2. Assert: node discards the COMMAND.
3. Monitor all transmitted frames until the next WAKE retry.
4. Assert: no error response or diagnostic frame is transmitted — only the next WAKE retry appears.

---

### T-N925  APP_DATA_REPLY with mismatched nonce discarded

**Validates:** ND-0302

**Procedure:**
1. BPF program calls `send_recv()`.
2. Mock gateway replies with an APP_DATA_REPLY bearing a nonce that does not match the request.
3. Assert: node discards the reply.
4. Assert: `send_recv()` times out or returns an error.

---

### T-N926  Sequence numbers reset across wake cycles

**Validates:** ND-0303

**Procedure:**
1. Complete wake cycle 1; gateway provides `starting_seq = S1`.
2. Verify outbound messages in cycle 1 use sequence numbers starting at S1.
3. Complete wake cycle 2; gateway provides `starting_seq = S2`.
4. Assert: outbound messages in cycle 2 start at S2, with no carry-over from cycle 1.

---

### T-N927  RNG health-check failure aborts wake cycle

**Validates:** ND-0304

**Procedure:**
1. Configure the node's RNG backend (via the `crate::traits::Rng` trait) to use a
   mock whose `health_check()` method deterministically fails.
2. Run the wake cycle under the test harness.
3. Assert: wake cycle aborts early (returns `WakeCycleOutcome::Sleep` before
   sending WAKE).
4. Assert: no WAKE frame is transmitted.

> **Note:** This test requires a build where the RNG is injectable via the HAL
> trait. On hardware-only builds without a mock RNG hook, record T-N927 as
> "Not Applicable" in the validation report.

---

### T-N928  Program image with map definitions and LDDW relocation

**Validates:** ND-0501a

**Procedure:**
1. Ingest a BPF program image containing 2 map definitions.
2. Transfer the image to the node via chunked transfer.
3. Node loads the program.
4. Assert: LDDW relocations resolve to valid map addresses.
5. Assert: maps are allocated in RTC SRAM.

---

### T-N929  Write to read-only `sonde_context` silently ignored

**Validates:** ND-0505

**Procedure:**
1. Load a BPF program that attempts to write to the `sonde_context` memory region.
2. Execute the program.
3. Assert: the `sonde_context` fields are unchanged after the write attempt.
4. Assert: execution continues normally (program is not terminated for the attempt).

---

### T-N930  Helper ABI conformance

**Validates:** ND-0600

**Procedure:**
1. Enumerate all exported BPF helper IDs and their function signatures from the firmware.
2. Load the published helper spec from `bpf-environment.md`.
3. Assert: every helper ID and signature in the firmware matches the published spec exactly.

---

### T-N931  Ephemeral program uses bus helpers

**Validates:** ND-0601

**Procedure:**
1. Load an ephemeral BPF program that calls I2C, SPI, GPIO, and ADC bus helpers.
2. Execute the ephemeral program.
3. Assert: all bus helper calls succeed and return expected values.
4. Assert: behavior is identical to the same calls from a resident program.

---

### T-N932  `map_lookup_elem` returns NULL for unwritten key

**Validates:** ND-0603

**Procedure:**
1. Load a BPF program with a map.
2. Call `map_lookup_elem` with a key that was never written.
3. Assert: the helper returns NULL (0).

---

### T-N933  `delay_us()` rejects excessive duration

**Validates:** ND-0604

**Procedure:**
1. Load a BPF program that calls `delay_us()` with a value exceeding the documented maximum.
2. Execute the program.
3. Assert: the firmware rejects or clamps the delay (does not delay for the full excessive duration).

---

### T-N934  Stack overflow terminates BPF program

**Validates:** ND-0605

**Procedure:**
1. Load a BPF program that writes beyond the 512-byte per-frame stack boundary.
2. Execute the program.
3. Assert: the interpreter terminates the program with a stack violation error.

---

### T-N935  Map memory budget exceeded rejects program load

**Validates:** ND-0606

**Procedure:**
1. Build a program image whose total map memory exceeds the RTC SRAM budget.
2. Attempt to load the program on the node.
3. Assert: the load is rejected.
4. Assert: the program does not execute.

---

### T-N936  Chunked transfer retry backoff and cadence

**Validates:** ND-0701

**Procedure:**
1. Begin a chunked transfer.
2. Simulate a missing-chunk scenario that triggers retries.
3. Measure the backoff delay (from response timeout expiry to next `GET_CHUNK` transmission).
4. Assert: the backoff delay is approximately 400 ms (`RETRY_DELAY_MS`, ±20 ms).
5. Assert: the total interval between successive `GET_CHUNK` transmissions on a timeout-only path is approximately 600 ms (200 ms response timeout + 400 ms backoff, ±50 ms).

---

### T-N937  Response timeout boundary at 200 ms

**Validates:** ND-0702

**Procedure:**
1. Node sends a request.
2. Mock gateway responds at 150 ms after the request.
3. Assert: node accepts the response (under 200 ms timeout).
4. Repeat: node sends a request; mock gateway responds at 250 ms.
5. Assert: node treats the late response as a timeout.

---

### T-N938  Wrong-context known msg_type silently discarded

**Validates:** ND-0801

**Procedure:**
1. Node is in the COMMAND-wait state (expecting a COMMAND response).
2. Send a valid CHUNK frame (known msg_type, wrong context).
3. Assert: the frame is silently discarded.
4. Assert: no error response is transmitted.

---

### T-N939  BLE connection with MTU < 247 rejected

**Validates:** ND-0904

**Procedure:**
1. Initiate a BLE connection to the node with an MTU lower than 247.
2. Assert: the node drops (or refuses) the connection.

---

### T-N940  NODE_PROVISION with invalid `payload_len` rejected

**Validates:** ND-0905

**Procedure:**
1. Send a NODE_PROVISION message where `payload_len` exceeds the remaining data in the buffer.
2. Assert: the node rejects the message.
3. Assert: the node does not read beyond the buffer boundary.

---

### T-N941  PEER_ACK with corrupted AEAD ciphertext silently discarded

**Validates:** ND-0912

**Procedure:**
1. Send a PEER_ACK with a valid nonce but corrupted AEAD ciphertext (authentication tag tampered).
2. Assert: the node silently discards the frame.
3. Assert: no error response is transmitted.

---

### T-N942  Task watchdog timer enabled

**Validates:** ND-0919

**Procedure:**
1. Inspect `sdkconfig.defaults` for the node firmware.
2. Assert: `CONFIG_ESP_TASK_WDT_EN=y` is present.
3. Assert: `CONFIG_ESP_TASK_WDT_TIMEOUT_S=20` is present.
4. Assert: `CONFIG_ESP_TASK_WDT_PANIC=y` is present.
5. Inspect `node.rs` main function.
6. Assert: `esp_task_wdt_add()` is called at startup to register the main task.
7. Assert: `esp_task_wdt_delete()` is called after the wake cycle completes.

> **Note:** Verifying that the watchdog triggers on a stalled main loop requires a special test firmware build and real hardware (similar to modem T-0304). This test validates configuration and registration code only.

---

### T-N0607a  I2C pins read from NVS at HAL init

**Validates:** ND-0608 (AC 1, 3)

**Procedure:**
1. Write SDA=4, SCL=5 to NVS keys `i2c0_sda` and `i2c0_scl`.
2. Trigger HAL initialization (power-on reset or deep-sleep wake).
3. Assert: the I2C0 peripheral is configured with SDA=4, SCL=5.
4. Assert: pin assignments survive a deep-sleep cycle — repeat step 2 and verify the same pins are used.

---

### T-N0607b  Missing NVS keys fall back to defaults

**Validates:** ND-0608 (AC 2)

**Procedure:**
1. Erase NVS keys `i2c0_sda` and `i2c0_scl` (or start with a fresh NVS partition).
2. Trigger HAL initialization.
3. Assert: the I2C0 peripheral is configured with the compiled-in defaults SDA=0, SCL=1.

---

### T-N0607c  Pin config persisted from NODE_PROVISION with CBOR pin data

**Validates:** ND-0608 (AC 5)

**Procedure:**
1. Construct a NODE_PROVISION BLE message body with a valid encrypted payload followed by a deterministic CBOR map `{1: 6, 2: 7}` (SDA=6, SCL=7).
2. Deliver the message to the node's provisioning handler.
3. Assert: NVS keys `i2c0_sda` and `i2c0_scl` are written with values 6 and 7 respectively.
4. Trigger a HAL re-initialization (or reboot).
5. Assert: the I2C0 peripheral is configured with SDA=6, SCL=7.

---

### T-N0607d  NODE_PROVISION without pin config — backward compatible

**Validates:** ND-0608 (AC 6)

**Procedure:**
1. Construct a NODE_PROVISION BLE message body with a valid encrypted payload and no trailing pin config bytes.
2. Deliver the message to the node's provisioning handler.
3. Assert: provisioning succeeds without error.
4. Assert: NVS keys `i2c0_sda` and `i2c0_scl` are not written (or remain at their prior values).

---

### T-N0607e  Factory reset does NOT erase pin config

**Validates:** ND-0608 (AC 4), ND-0917

**Procedure:**
1. Write SDA=4, SCL=5 to NVS keys `i2c0_sda` and `i2c0_scl`.
2. Trigger a factory reset (ND-0917).
3. Assert: NVS keys `i2c0_sda` and `i2c0_scl` still contain 4 and 5.
4. Trigger HAL initialization.
5. Assert: the I2C0 peripheral is configured with SDA=4, SCL=5.

---

### T-N0607f  Invalid CBOR trailing data treated as no pin config

**Validates:** ND-0608 (AC 6)

**Procedure:**
1. Construct a NODE_PROVISION BLE message body with a valid encrypted payload followed by invalid trailing bytes (e.g., truncated CBOR or random data).
2. Deliver the message to the node's provisioning handler.
3. Assert: provisioning succeeds (the encrypted payload is processed normally).
4. Assert: NVS keys `i2c0_sda` and `i2c0_scl` are not written.

---

## 12  Diagnostic relay tests

### T-N1100  DIAG_RELAY_REQUEST accepted in pairing mode

**Validates:** ND-1100

**Procedure:**
1. Boot node into BLE pairing mode (no PSK stored).
2. Connect via BLE and send a `DIAG_RELAY_REQUEST` (envelope type 0x02) with `rf_channel=6` and a valid 50-byte payload.
3. Assert: the node processes the request (does not reject or ignore it).
4. Assert: a `DIAG_RELAY_RESPONSE` (envelope type 0x82) is received via BLE indication.

---

### T-N1101  DIAG_RELAY_REQUEST invalid channel rejected

**Validates:** ND-1100

**Procedure:**
1. Boot node into BLE pairing mode.
2. Send `DIAG_RELAY_REQUEST` with `rf_channel=14` (out of range).
3. Assert: node responds with `DIAG_RELAY_RESPONSE(status=0x02)`.
4. Assert: no ESP-NOW frame is broadcast.

---

### T-N1102  DIAG_RELAY_REQUEST empty payload rejected

**Validates:** ND-1100

**Procedure:**
1. Boot node into BLE pairing mode.
2. Send `DIAG_RELAY_REQUEST` with `rf_channel=6` and `payload_len=0`.
3. Assert: node responds with `DIAG_RELAY_RESPONSE(status=0x02)`.

---

### T-N1103  Diagnostic ESP-NOW broadcast

**Validates:** ND-1101

**Procedure:**
1. Boot node into BLE pairing mode.
2. Set up an ESP-NOW receiver on channel 6.
3. Send `DIAG_RELAY_REQUEST` with `rf_channel=6` and a known payload.
4. Assert: the ESP-NOW receiver captures a broadcast frame matching the payload exactly.
5. Assert: the destination MAC is `FF:FF:FF:FF:FF:FF`.

---

### T-N1104  Diagnostic reply reception and forwarding

**Validates:** ND-1102, ND-1105

**Procedure:**
1. Boot node into BLE pairing mode.
2. Send `DIAG_RELAY_REQUEST` with a valid payload.
3. From a test ESP-NOW transmitter, send a frame with `msg_type=0x85` at header offset 2 to the node.
4. Assert: node forwards the frame in a `DIAG_RELAY_RESPONSE(status=0x00, payload=<frame>)` BLE indication.
5. Assert: the forwarded payload is byte-identical to the ESP-NOW frame sent.

---

### T-N1105  Non-diagnostic ESP-NOW frames ignored during listen

**Validates:** ND-1102

**Procedure:**
1. Boot node into BLE pairing mode.
2. Send `DIAG_RELAY_REQUEST`.
3. During the listen window, send ESP-NOW frames with `msg_type=0x81` (COMMAND) and `msg_type=0x04` (APP_DATA).
4. Assert: node does not forward these frames to the pairing tool.
5. After the listen window expires (no `msg_type=0x85` received), assert `DIAG_RELAY_RESPONSE(status=0x01)`.

---

### T-N1106  Diagnostic retry behavior (3 attempts, 200ms backoff)

**Validates:** ND-1103

**Procedure:**
1. Boot node into BLE pairing mode.
2. Set up an ESP-NOW receiver that counts broadcasts but does NOT send a reply.
3. Send `DIAG_RELAY_REQUEST`.
4. Assert: the receiver counts exactly 4 broadcasts (1 initial + 3 retries).
5. Assert: the time between consecutive broadcasts is approximately 2.2 seconds (2s listen + 200ms backoff).
6. Assert: node sends `DIAG_RELAY_RESPONSE(status=0x01)` after all retries.

---

### T-N1107  Diagnostic timeout after all retries

**Validates:** ND-1104

**Procedure:**
1. Boot node into BLE pairing mode.
2. Send `DIAG_RELAY_REQUEST` with no gateway or modem available (no ESP-NOW reply will arrive).
3. Assert: node sends `DIAG_RELAY_RESPONSE(status=0x01, payload_len=0)` within approximately 9 seconds.

---

### T-N1108  Radio state restored after diagnostic

**Validates:** ND-1106

**Procedure:**
1. Boot node into BLE pairing mode. Record the initial ESP-NOW channel (or lack thereof).
2. Send `DIAG_RELAY_REQUEST` with `rf_channel=11`.
3. Wait for `DIAG_RELAY_RESPONSE`.
4. Assert: the ESP-NOW radio is restored to its pre-diagnostic state.
5. Assert: BLE remains connected and functional (send a second `DIAG_RELAY_REQUEST` successfully).

---

### T-N1109  Diagnostic followed by provisioning

**Validates:** ND-1106, ND-1100

**Procedure:**
1. Boot node into BLE pairing mode.
2. Run a complete diagnostic relay (send `DIAG_RELAY_REQUEST`, receive response).
3. Send `NODE_PROVISION` with valid provisioning data.
4. Assert: provisioning succeeds (`NODE_ACK` status=0x00).
5. Assert: NVS contains the expected PSK and channel.

---

### T-N1110  Multiple diagnostics in sequence

**Validates:** ND-1100, ND-1106

**Procedure:**
1. Boot node into BLE pairing mode.
2. Send `DIAG_RELAY_REQUEST` with `rf_channel=1`. Wait for response.
3. Send `DIAG_RELAY_REQUEST` with `rf_channel=6`. Wait for response.
4. Send `DIAG_RELAY_REQUEST` with `rf_channel=11`. Wait for response.
5. Assert: all three diagnostics complete successfully.
6. Assert: BLE remains connected and functional after all three.

---

## Appendix A  Test-to-requirement traceability

| Requirement | Test(s) |
|---|---|
| ND-0100 | T-N100 |
| ND-0101 | T-N101, T-N919 |
| ND-0102 | T-N102 |
| ND-0103 | T-N103, T-N104, T-N920 |
| ND-0200 | T-N200, T-N201, T-N921 |
| ND-0201 | T-N202, T-N203 |
| ND-0202 | T-N204, T-N205, T-N206, T-N207, T-N922 |
| ND-0203 | T-N205, T-N208, T-N209, T-N923 |
| ND-0300 | T-N606a, T-N606b (AEAD path) |
| ND-0301 | T-N301, T-N924 |
| ND-0302 | T-N302, T-N303, T-N304, T-N925 |
| ND-0303 | T-N305, T-N926 |
| ND-0304 | T-N306, T-N927 |
| ND-0400 | T-N100, T-N400, T-N401 |
| ND-0402 | T-N404 |
| ND-0403 | *(verified by secure boot platform tests)* |
| ND-0403a | *(verified by flash encryption platform tests)* |
| ND-0500 | T-N500 |
| ND-0501 | T-N501, T-N502 |
| ND-0501a | T-N503, T-N928 |
| ND-0502 | T-N504 |
| ND-0503 | T-N505 |
| ND-0504 | T-N506 |
| ND-0505 | T-N507, T-N508, T-N509, T-N929 |
| ND-0506 | T-N508, T-N510 |
| ND-0600 | T-N930 |
| ND-0601 | T-N600, T-N601, T-N602, T-N603, T-N931 |
| ND-0602 | T-N604, T-N605, T-N606, T-N621, T-N627a |
| ND-0603 | T-N607, T-N608, T-N609, T-N932 |
| ND-0604 | T-N610, T-N611, T-N612, T-N613, T-N933 |
| ND-0605 | T-N614, T-N615, T-N934 |
| ND-0606 | T-N616, T-N619, T-N620, T-N935 |
| ND-0700 | T-N201, T-N700 |
| ND-0701 | T-N701, T-N803, T-N936 |
| ND-0702 | T-N702, T-N937 |
| ND-0800 | T-N800 |
| ND-0801 | T-N801, T-N803, T-N938 |
| ND-0802 | T-N802 |
| ND-0900 | T-N900 |
| ND-0901 | T-N901 |
| ND-0902 | T-N902 |
| ND-0903 | T-N902 |
| ND-0904 | T-N903, T-N939 |
| ND-0905 | T-N904, T-N905, T-N906, T-N940 |
| ND-0906 | T-N904 |
| ND-0907 | T-N905, T-N908 |
| ND-0908 | T-N907 |
| ND-0909 | T-N909 |
| ND-0910 | T-N910 |
| ND-0911 | T-N911 |
| ND-0912 | T-N912, T-N913, T-N914, T-N941 |
| ND-0913 | T-N915 |
| ND-0914 | T-N916 |
| ND-0915 | T-N917 |
| ND-0916 | T-N918 |
| ND-0917 | T-N906 |
| ND-0918 | *(verified by sdkconfig.defaults setting)* |
| ND-0608 | T-N0607a, T-N0607b, T-N0607c, T-N0607d, T-N0607e, T-N0607f |
| ND-0609 | T-N621, T-N626, T-N627, T-N632 |
| ND-0610 | T-N622, T-N623, T-N624 |
| ND-0611 | T-N623 |
| ND-0612 | T-N628, T-N629 |
| ND-0613 | T-N625 |
| ND-0614 | T-N630, T-N631 |
| ND-1000 | T-N1000, T-N1001 |
| ND-1001 | T-N1002 |
| ND-1002 | T-N1003 |
| ND-1003 | T-N1004 |
| ND-1004 | T-N1005 |
| ND-1005 | T-N1006 |
| ND-1006 | T-N1007, T-N1014 |
| ND-1007 | T-N1008 |
| ND-1008 | T-N1012, T-N1013 |
| ND-1009 | T-N1009, T-N1010, T-N1011 |
| ND-1010 | T-N1015 |
| ND-1011 | T-N1017, T-N1018 |
| ND-1012 | T-N1019, T-N1020 |
| ND-1013 | T-N1016 |
| ND-1014 | T-N1021 |
| ND-1015 | T-N1022 |
| ND-1016 | T-N1023 |
| ND-1100 | T-N1100, T-N1101, T-N1102, T-N1109, T-N1110 |
| ND-1101 | T-N1103 |
| ND-1102 | T-N1104, T-N1105 |
| ND-1103 | T-N1106 |
| ND-1104 | T-N1107 |
| ND-1105 | T-N1104 |
| ND-1106 | T-N1108, T-N1109, T-N1110 |

---

## Appendix B  Test ID to test function traceability

This table lists spec test IDs (T-Nxxx) that have host-based automated tests and maps each to the
test function(s) that satisfy it. Spec IDs not listed here are either not yet implemented in the
host suite or require target hardware/BLE-stack validation; see the note below the table.
Test functions in `crates/sonde-node/src/` are unit tests; those in `crates/sonde-e2e/tests/e2e_tests.rs` are integration tests.

| Spec ID | Test function(s) | Location |
|---------|-----------------|----------|
| T-N100 | `test_unpaired_node_returns_unpaired` | wake_cycle.rs |
| T-N101 | `test_wake_cbor_integer_keys` | wake_cycle.rs |
| T-N102 | `test_outbound_frame_format` | wake_cycle.rs |
| T-N103 | `test_send_app_data_max_blob` | wake_cycle.rs |
| T-N104 | `test_send_app_data_oversized_blob` | wake_cycle.rs |
| T-N200 | `test_normal_nop_wake_cycle`, `t_e2e_001_nop_wake_cycle`, `t_e2e_002b_consecutive_wake_cycles`, `t_e2e_011_program_already_current`, `t_e2e_051_modem_frame_round_trip`, `t_e2e_052_bridged_consecutive_cycles`, `t_e2e_069_multi_node`, `t_e2e_070_full_use_case` | wake_cycle.rs, e2e_tests.rs |
| T-N201 | `test_wake_retries_exhausted` | wake_cycle.rs |
| T-N202 | `test_wake_message_fields`, `t_e2e_001_nop_wake_cycle` | wake_cycle.rs, e2e_tests.rs |
| T-N203 | `test_no_program_empty_hash` | wake_cycle.rs |
| T-N204 | `test_normal_nop_wake_cycle`, `t_e2e_011_program_already_current` | wake_cycle.rs, e2e_tests.rs |
| T-N205 | `wake_cycle::test_update_schedule`, `sleep::test_update_schedule`, `t_e2e_020_update_schedule` | wake_cycle.rs, sleep.rs, e2e_tests.rs |
| T-N206 | `test_reboot_command`, `t_e2e_021_reboot` | wake_cycle.rs, e2e_tests.rs |
| T-N207 | `test_unknown_command_treated_as_nop` | wake_cycle.rs |
| T-N208 | `test_set_next_wake_shorter`, `test_set_next_wake_equal` *(partial — unit tests cover SleepManager clamping logic; the full e2e set_next_wake → base-interval-restore cycle is not yet tested)* | sleep.rs |
| T-N209 | `test_set_next_wake_longer_clamped` | sleep.rs |
| T-N300 | `wake_command_exchange_round_trip`, `t_e2e_001_nop_wake_cycle` | wake_cycle.rs, aead_e2e_tests.rs |
| T-N301 | `t_e2e_003_wrong_psk_rejected`, `t_e2e_004_tampered_frame_discarded` | aead_e2e_tests.rs |
| T-N302 | `wake_command_exchange_round_trip`, `t_e2e_001_nop_wake_cycle` | wake_cycle.rs, aead_e2e_tests.rs |
| T-N303 | `test_wrong_nonce_discarded`, `test_send_recv_app_data_wrong_nonce` | wake_cycle.rs |
| T-N304 | `test_wrong_seq_on_chunk_discarded` | wake_cycle.rs |
| T-N305 | `test_sequence_increment_correctness`, `t_e2e_041_sequence_numbers` | wake_cycle.rs, e2e_tests.rs |
| T-N306 | `test_nonce_uniqueness_across_cycles`, `t_e2e_002b_consecutive_wake_cycles`, `t_e2e_052_bridged_consecutive_cycles` | wake_cycle.rs, e2e_tests.rs |
| T-N400 | `test_load_identity_unpaired` | key_store.rs |
| T-N401 | `test_load_identity_unpaired`, `test_unpaired_node_returns_unpaired` | key_store.rs, wake_cycle.rs |
| T-N402 | `t_e2e_064_onboarding_to_wake` | e2e_tests.rs |
| T-N403 | `t_n905_same_session_reprovision` | ble_pairing.rs |
| T-N404 | `test_factory_reset`, `t_e2e_068_factory_reset_reprovision` | key_store.rs, e2e_tests.rs |
| T-N500 | `test_chunked_transfer_success`, `t_e2e_010_full_program_update`, `t_e2e_054_bridged_program_update`, `t_e2e_070_full_use_case` | wake_cycle.rs, e2e_tests.rs |
| T-N501 | `test_chunked_transfer_success`, `t_e2e_010_full_program_update`, `t_e2e_054_bridged_program_update` | wake_cycle.rs, e2e_tests.rs |
| T-N502 | `test_program_transfer_hash_mismatch` | wake_cycle.rs |
| T-N503 | `test_program_image_decoding_with_maps` (partial — does not validate LDDW `src=1` map reference resolution) | wake_cycle.rs |
| T-N504 | `test_chunked_transfer_success`, `t_e2e_010_full_program_update` | wake_cycle.rs, e2e_tests.rs |
| T-N505 | `test_ephemeral_program_integration`, `t_e2e_022_run_ephemeral` | wake_cycle.rs, e2e_tests.rs |
| T-N506 | `test_chunked_transfer_success` | wake_cycle.rs |
| T-N507 | `test_execution_context_fields` | wake_cycle.rs |
| T-N508 | `test_chunked_transfer_success`, `test_wake_reason_program_update`, `test_wake_reason` (sleep.rs) | wake_cycle.rs, sleep.rs |
| T-N509 | `test_wake_reason_early`, `test_wake_reason` (sleep.rs) | wake_cycle.rs, sleep.rs |
| T-N510 | `test_post_update_immediate_execution` | wake_cycle.rs |
| T-N600 | `test_helper_i2c_read` | bpf_dispatch.rs |
| T-N601 | `test_helper_i2c_error` | bpf_dispatch.rs |
| T-N602 | `test_helper_spi_transfer` | bpf_dispatch.rs |
| T-N603 | `test_helper_gpio_and_adc` | bpf_dispatch.rs |
| T-N604 | `test_helper_send`, `test_send_recv_app_data_success`, `t_e2e_031_app_data_fire_and_forget`, `t_e2e_070_full_use_case` | bpf_dispatch.rs, wake_cycle.rs, e2e_tests.rs |
| T-N605 | `test_helper_send_recv`, `test_send_recv_app_data_success`, `t_e2e_030_app_data_round_trip` | bpf_dispatch.rs, wake_cycle.rs, e2e_tests.rs |
| T-N606 | `test_helper_send_recv_timeout`, `test_send_recv_app_data_timeout` | bpf_dispatch.rs, wake_cycle.rs |
| T-N607 | `test_helper_map_lookup_update` | bpf_dispatch.rs |
| T-N608 | `test_map_persistence_across_cycles` | wake_cycle.rs |
| T-N609 | `test_helper_map_update_ephemeral_rejected` | bpf_dispatch.rs |
| T-N610 | `test_helper_get_time_and_battery` | bpf_dispatch.rs |
| T-N611 | `test_helper_delay_us` | bpf_dispatch.rs |
| T-N612 | `test_helper_set_next_wake_ephemeral_rejected` | bpf_dispatch.rs |
| T-N613 | `test_helper_bpf_trace_printk` | bpf_dispatch.rs |
| T-N614 | `test_instruction_budget_exceeded_graceful` | wake_cycle.rs |
| T-N615 | `test_call_depth_exceeded_graceful` | wake_cycle.rs |
| T-N616 | `test_map_budget_exceeded_rejects_program` | wake_cycle.rs |
| T-N619 | `test_validate_accepts_global_variable_map_type_0` | map_storage.rs |
| T-N620 | `test_validate_rejects_unsupported_map_type` | map_storage.rs |
| T-N700 | `test_wake_retries_exhausted` | wake_cycle.rs |
| T-N701 | `test_chunked_transfer_chunk_retry_exhausted` | wake_cycle.rs |
| T-N800 | `test_malformed_cbor_discarded` | wake_cycle.rs |
| T-N801 | `test_unexpected_msg_type_discarded` | wake_cycle.rs |
| T-N802 | `test_chunked_transfer_wrong_chunk_index` | wake_cycle.rs |
| T-N803 | `test_stale_command_before_chunk_recovery` | wake_cycle.rs |
| T-N900 | *(hardware — validated on target: boot priority and BLE stack init)* | — |
| T-N901 | *(hardware — validated on target: pairing button detection)* | — |
| T-N902 | *(hardware — validated on target: BLE GATT service registration)* | — |
| T-N903 | *(hardware — validated on target: MTU negotiation and LESC pairing)* | — |
| T-N904 | `t_n904_happy_path`, `t_e2e_062_node_ble_provisioning`, `t_e2e_068_factory_reset_reprovision`, `t_e2e_070_full_use_case` | ble_pairing.rs, e2e_tests.rs |
| T-N905 | `t_n905_same_session_reprovision` | ble_pairing.rs |
| T-N906 | `t_n906_factory_reset_on_button_hold` | ble_pairing.rs |
| T-N907 | `t_n907_nvs_write_key_failure`, `t_n907_nvs_write_channel_failure`, `t_n907_nvs_write_peer_payload_failure`, `t_n907_nvs_write_reg_complete_failure` | ble_pairing.rs |
| T-N908 | *(hardware — validated on target: BLE mode persistence after provisioning)* | — |
| T-N909 | `t_e2e_063_peer_request_ack` | e2e_tests.rs |
| T-N910 | *(hardware — validated on target: PEER_REQUEST retransmission)* | — |
| T-N911 | `t_e2e_067_agent_revocation` | e2e_tests.rs |
| T-N912 | `t_e2e_063_peer_request_ack` | e2e_tests.rs |
| T-N913 | `t_e2e_065_deferred_erasure` | e2e_tests.rs |
| T-N914 | *(hardware — validated on target: PEER_ACK wrong registration proof)* | — |
| T-N915 | `t_e2e_063_peer_request_ack`, `t_e2e_064_onboarding_to_wake` | e2e_tests.rs |
| T-N916 | `t_e2e_064_onboarding_to_wake`, `t_e2e_065_deferred_erasure` | e2e_tests.rs |
| T-N917 | `t_e2e_066_self_healing` | e2e_tests.rs |
| T-N918 | *(hardware — validated on target: NVS layout for BLE pairing artifacts)* | — |
| T-N927 | `t_n927_rng_health_check_failure_aborts` | wake_cycle.rs |
| T-N929 | `t_n929_write_to_read_only_context_silently_ignored` | sonde_bpf_adapter.rs |
| T-N940 | `t_n940_payload_len_exceeds_remaining_data`, `t_n940_payload_len_max_u16_rejected` | ble_pairing.rs |
| T-N941 | `verify_peer_ack_valid`, `verify_peer_ack_wrong_nonce`, `verify_peer_ack_wrong_key` | peer_request.rs |
| T-N1016 | *(hardware — validated on target: GPIO state after sleep preparation)* | — |

> **Note:** Spec cases marked *(hardware — validated on target)* require the
> NimBLE BLE stack or physical peripherals and cannot run in the host-based
> test suite. T-N702 (response timeout — mock gateway delays
> \> 200 ms) is host-testable but not yet implemented.
> T-N919–T-N926, T-N928, T-N930–T-N939: spec procedures added — implementation pending.
