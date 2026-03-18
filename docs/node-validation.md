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
- Verifies HMAC and decodes CBOR using `sonde-protocol`.
- Responds with configurable COMMAND, CHUNK, and APP_DATA_REPLY messages.
- Tracks received frames for assertion (message types, sequence numbers, payloads).
- Supports configurable behaviors: delay responses, drop responses (simulate timeout), send invalid HMAC, send wrong nonce/seq.

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
5. Assert: last 32 bytes = valid HMAC-SHA256 over preceding bytes.
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
2. Assert: node sends WAKE, retries up to 3 times (100 ms apart).
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
7. Assert: the WAKE frame header `nonce` field (in the fixed 11-byte header, not the CBOR payload) is present and sourced from the hardware RNG.

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

### T-N300  HMAC on outbound frames

**Validates:** ND-0300

**Procedure:**
1. Capture any outbound frame.
2. Independently compute HMAC-SHA256 over header + payload using the node's PSK.
3. Assert: computed HMAC matches the frame's last 32 bytes.

---

### T-N301  Invalid HMAC rejected

**Validates:** ND-0301

**Procedure:**
1. Mock gateway sends COMMAND with a corrupted HMAC.
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
3. Assert: no frames transmitted. Node enters deep sleep with radio off.

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
2. Install program that calls `spi_transfer()`.
3. Assert: received data matches expected echo.

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

**Validates:** ND-0604

**Procedure:**
1. Install program that calls `bpf_trace_printk("hello")`.
2. Assert: "hello" appears in debug output.

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

## 9  Timing and retry tests

### T-N700  WAKE retry count and timing

**Validates:** ND-0700

**Procedure:**
1. Mock gateway does not respond.
2. Assert: node sends exactly 4 WAKE frames (1 initial + 3 retries).
3. Assert: ~100 ms between each attempt.
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
1. Mock gateway delays response by 100 ms (>50 ms timeout).
2. Assert: node treats it as timeout and retries.

---

## 10  Error handling tests

### T-N800  Malformed CBOR — no crash

**Validates:** ND-0800

**Procedure:**
1. Mock gateway sends COMMAND with valid HMAC but garbage CBOR payload.
2. Assert: node discards the frame.
3. Assert: no crash, node retries or sleeps.

---

### T-N801  Unexpected msg_type

**Validates:** ND-0801

**Procedure:**
1. Mock gateway sends a frame with `msg_type = 0x99` (unknown) and valid HMAC.
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
4. Assert: device name matches `sonde-XXXX` where XXXX = last 4 hex digits of BLE MAC.
5. Connect and discover services.
6. Assert: Node Command characteristic `0000FE51-0000-1000-8000-00805F9B34FB` is present with Write+Indicate properties.

---

### T-N903  MTU negotiation and LESC pairing

**Validates:** ND-0904

**Procedure:**
1. Connect to node in BLE pairing mode.
2. Request ATT MTU of 247.
3. Assert: negotiated MTU is ≥ 247.
4. Initiate LESC Just Works pairing.
5. Assert: pairing completes successfully.

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
6. Assert: HMAC-SHA256 over header+payload verifies with the PSK from NVS key `psk`.
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
2. Mock gateway responds with PEER_ACK: valid HMAC, nonce = N, correct `registration_proof` = HMAC-SHA256(`node_psk`, `"sonde-peer-ack-v1" ‖ encrypted_payload`).
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
3. Mock gateway does not respond (or responds with invalid HMAC).
4. Assert: `reg_complete` flag is cleared.
5. Assert: on next boot the node sends PEER_REQUEST instead of WAKE.

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

## Appendix A  Test-to-requirement traceability

| Requirement | Test(s) |
|---|---|
| ND-0100 | T-N100 |
| ND-0101 | T-N101 |
| ND-0102 | T-N102 |
| ND-0103 | T-N103, T-N104 |
| ND-0200 | T-N200, T-N201 |
| ND-0201 | T-N202, T-N203 |
| ND-0202 | T-N204, T-N205, T-N206, T-N207 |
| ND-0203 | T-N205, T-N208, T-N209 |
| ND-0300 | T-N300 |
| ND-0301 | T-N301 |
| ND-0302 | T-N302, T-N303, T-N304 |
| ND-0303 | T-N305 |
| ND-0304 | T-N306 |
| ND-0400 | T-N100, T-N400, T-N401 |
| ND-0402 | T-N404 |
| ND-0403 | *(verified by secure boot platform tests)* |
| ND-0403a | *(verified by flash encryption platform tests)* |
| ND-0500 | T-N500 |
| ND-0501 | T-N501, T-N502 |
| ND-0501a | T-N503 |
| ND-0502 | T-N504 |
| ND-0503 | T-N505 |
| ND-0504 | T-N506 |
| ND-0505 | T-N507, T-N508, T-N509 |
| ND-0506 | T-N508, T-N510 |
| ND-0600 | *(validated by automated helper ABI conformance test that asserts exported helper IDs and signatures match the published spec across firmware versions)* |
| ND-0601 | T-N600, T-N601, T-N602, T-N603 |
| ND-0602 | T-N604, T-N605, T-N606 |
| ND-0603 | T-N607, T-N608, T-N609 |
| ND-0604 | T-N610, T-N611, T-N612, T-N613 |
| ND-0605 | T-N614, T-N615 |
| ND-0606 | T-N616 |
| ND-0700 | T-N201, T-N700 |
| ND-0701 | T-N701 |
| ND-0702 | T-N702 |
| ND-0800 | T-N800 |
| ND-0801 | T-N801 |
| ND-0802 | T-N802 |
| ND-0900 | T-N900 |
| ND-0901 | T-N901 |
| ND-0902 | T-N902 |
| ND-0903 | T-N902 |
| ND-0904 | T-N903 |
| ND-0905 | T-N904, T-N905, T-N906 |
| ND-0906 | T-N904 |
| ND-0907 | T-N905, T-N908 |
| ND-0908 | T-N907 |
| ND-0909 | T-N909 |
| ND-0910 | T-N910 |
| ND-0911 | T-N911 |
| ND-0912 | T-N912, T-N913, T-N914 |
| ND-0913 | T-N915 |
| ND-0914 | T-N916 |
| ND-0915 | T-N917 |
| ND-0916 | T-N918 |
| ND-0917 | T-N906 |
| ND-0918 | *(verified by sdkconfig.defaults setting)* |

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
| T-N300 | `test_outbound_frame_format`, `t_e2e_002_hmac_round_trip` | wake_cycle.rs, e2e_tests.rs |
| T-N301 | `test_invalid_hmac_discarded`, `t_e2e_003_wrong_psk_rejected`, `t_e2e_040_unknown_node`, `t_e2e_053_bridged_wrong_psk` | wake_cycle.rs, e2e_tests.rs |
| T-N302 | `test_outbound_frame_format`, `t_e2e_002_hmac_round_trip` | wake_cycle.rs, e2e_tests.rs |
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
| T-N503 | *(not yet covered — current transfer tests use empty map lists; a test with 2 map definitions, LDDW pointer resolution, and RTC SRAM allocation is needed)* | — |
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
| T-N700 | `test_wake_retries_exhausted` | wake_cycle.rs |
| T-N701 | `test_chunked_transfer_chunk_retry_exhausted` | wake_cycle.rs |
| T-N800 | `test_malformed_cbor_discarded` | wake_cycle.rs |
| T-N801 | `test_unexpected_msg_type_discarded` | wake_cycle.rs |
| T-N802 | `test_chunked_transfer_wrong_chunk_index` | wake_cycle.rs |
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

> **Note:** Spec cases marked *(hardware — validated on target)* require the
> NimBLE BLE stack or physical peripherals and cannot run in the host-based
> test suite. T-N503 (map decoding with LDDW pointer resolution),
> T-N616 (map memory budget enforcement), and T-N702 (response timeout — mock gateway delays
> \> 50 ms) are host-testable but not yet implemented.
