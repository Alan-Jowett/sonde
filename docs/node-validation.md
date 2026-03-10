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
3. Capture WAKE frame, decode payload.
4. Assert: `firmware_abi_version` matches firmware ABI.
5. Assert: `program_hash` = hash X.
6. Assert: `battery_mv` = 3300.
7. Assert: WAKE includes a `nonce` field sourced from hardware RNG.

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

### T-N402  USB pairing

**Validates:** ND-0401

**Procedure:**
1. Erase the key partition.
2. Write PSK + key_hint + magic via USB serial interface.
3. Boot the node.
4. Assert: node sends WAKE authenticated with the new PSK.

---

### T-N403  Pairing rejected when already paired

**Validates:** ND-0401

**Procedure:**
1. Node has an existing PSK.
2. Attempt USB pairing with a new PSK.
3. Assert: pairing is rejected (factory reset required first).

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
3. Assert: `timestamp` ≈ 1710000000000 (within a few ms of local elapsed).
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
3. Assert: `get_time()` ≈ 1710000000000.
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
| ND-0401 | T-N402, T-N403 |
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
