<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Validation Specification

> **Document status:** Draft
> **Scope:** Integration and system-level test plan for the ESP32-S3 radio modem firmware.
> **Audience:** Implementers (human or LLM agent) writing modem firmware tests.
> **Related:** [modem-requirements.md](modem-requirements.md), [modem-design.md](modem-design.md), [modem-protocol.md](modem-protocol.md)

---

## 1  Overview

This document defines test cases that validate the modem firmware against the requirements in [modem-requirements.md](modem-requirements.md). Each test case is traceable to one or more requirements.

**Scope:** These are integration tests exercised through the modem's two external interfaces: USB-CDC serial and ESP-NOW radio. Unit tests for internal modules are expected but not specified here.

**Test harness:** Tests use a **host-side test runner** that communicates with the modem over USB-CDC (or a PTY for simulated tests). The test runner speaks the modem serial protocol defined in [modem-protocol.md](modem-protocol.md). For radio-side tests, a second ESP32 running a simple ESP-NOW echo/inject program acts as the **radio peer**.

---

## 2  Test environment

### 2.1  Host-side test runner

A program running on the host that:

- Opens the modem's serial port (USB-CDC or PTY).
- Sends modem serial protocol commands (`RESET`, `SEND_FRAME`, `SET_CHANNEL`, `GET_STATUS`, `SCAN_CHANNELS`).
- Receives and validates modem responses (`MODEM_READY`, `RECV_FRAME`, `STATUS`, etc.).
- Provides helper methods for constructing and parsing serial frames.

### 2.2  Radio peer

A second ESP32 device (or simulator) that:

- Sends ESP-NOW frames to the modem's MAC address (for testing `RECV_FRAME` forwarding).
- Receives ESP-NOW frames from the modem (for testing `SEND_FRAME` transmission).
- Reports received frame contents back to the test runner for assertion.

### 2.3  PTY mock (hardware-free testing)

For tests that do not require real radio hardware, a PTY pair replaces the USB-CDC link. A mock modem process on the PTY slave can simulate modem behavior for gateway-side integration tests. Radio-side behavior is not testable via PTY.

---

## 3  USB-CDC interface tests

### T-0100  USB-CDC device enumeration

**Validates:** MD-0100

**Procedure:**
1. Connect the modem to a host via USB.
2. Assert: host OS enumerates a virtual serial port within 5 seconds.
3. Assert: no special driver installation is required.

---

### T-0101  MODEM_READY on boot

**Validates:** MD-0104

**Procedure:**
1. Open the modem's serial port.
2. Wait up to 2 seconds for a `MODEM_READY` message.
3. Assert: `MODEM_READY` is received within 2 seconds.
4. Assert: `firmware_version` is a valid 4-byte value.
5. Assert: `mac_address` is a valid 6-byte MAC (not all zeros).

---

### T-0102  Serial framing — valid frame

**Validates:** MD-0101

**Procedure:**
1. Send `RESET` to the modem.
2. Wait for `MODEM_READY`.
3. Send `GET_STATUS`.
4. Assert: response is a well-formed `STATUS` message with correct `LEN || TYPE || BODY` envelope.

---

### T-0103  Serial framing — oversized len

**Validates:** MD-0102

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send a frame with `len` = 1000 (exceeds 512 max).
3. Send `RESET` (to resynchronize).
4. Wait for `MODEM_READY`.
5. Assert: modem recovered and is operational.

---

### T-0104  Unknown message type

**Validates:** MD-0103

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send a well-formed frame with `type` = 0x7F (undefined).
3. Send `GET_STATUS`.
4. Assert: `STATUS` response is received (modem did not crash or hang).

---

## 4  ESP-NOW interface tests

### T-0200  Frame forwarding — radio to USB

**Validates:** MD-0201, MD-0205

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Set channel to match the radio peer.
3. Have the radio peer send an ESP-NOW frame with known payload to the modem.
4. Assert: a `RECV_FRAME` message is received on USB.
5. Assert: `peer_mac` matches the radio peer's MAC.
6. Assert: `frame_data` is byte-for-byte identical to the sent payload.
7. Assert: `rssi` is a plausible value (−100 to 0 dBm).

---

### T-0201  Frame transmission — USB to radio

**Validates:** MD-0202

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Set channel to match the radio peer.
3. Send `SEND_FRAME` with the radio peer's MAC and a known payload.
4. Assert: the radio peer receives the frame.
5. Assert: the received payload matches what was sent.

---

### T-0202  Automatic peer registration

**Validates:** MD-0203

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY` (peer table is empty).
2. Send `SEND_FRAME` to a MAC that has never been registered.
3. Assert: the frame is transmitted successfully (radio peer receives it).
4. Send `GET_STATUS`.
5. Assert: `tx_fail_count` is 0.

---

### T-0203  Peer table LRU eviction

**Validates:** MD-0204

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SEND_FRAME` to 21 unique MAC addresses (exceeds ~20 peer limit).
3. Assert: modem does not crash or return errors.
4. Send `SEND_FRAME` to the 1st MAC again.
5. Assert: frame is transmitted (peer was re-registered after eviction).

---

### T-0204  Frame ordering preserved

**Validates:** MD-0205

**Procedure:**
1. Have the radio peer send 10 ESP-NOW frames with sequential payload values (0x01 through 0x0A).
2. Collect all 10 `RECV_FRAME` messages from USB.
3. Assert: payloads arrive in order (0x01, 0x02, ..., 0x0A).

---

### T-0205  Channel change

**Validates:** MD-0206

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SET_CHANNEL(6)`.
3. Assert: `SET_CHANNEL_ACK(6)` is received.
4. Have the radio peer send a frame on channel 6.
5. Assert: `RECV_FRAME` is received.
6. Have the radio peer send a frame on channel 1.
7. Assert: no `RECV_FRAME` is received (modem is on channel 6).

---

### T-0206  Channel scanning

**Validates:** MD-0207

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SCAN_CHANNELS`.
3. Wait up to 10 seconds for `SCAN_RESULT`.
4. Assert: `SCAN_RESULT` is received.
5. Assert: `count` is between 1 and 14.
6. Assert: each entry has valid `channel` (1–14), `ap_count` ≥ 0, `strongest_rssi` ≤ 0 (or 0 if no APs).

---

## 5  Reliability and reset tests

### T-0300  RESET clears state

**Validates:** MD-0300

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send several `SEND_FRAME` messages (to increment `tx_count`).
3. Send `SET_CHANNEL(11)`.
4. Send `RESET`.
5. Wait for `MODEM_READY`.
6. Send `GET_STATUS`.
7. Assert: `tx_count` = 0, `rx_count` = 0, `tx_fail_count` = 0.
8. Assert: `channel` = 1 (reverted to default).

---

### T-0301  USB disconnection and reconnection

**Validates:** MD-0301

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Close the serial port (simulate USB disconnect).
3. Wait 2 seconds.
4. Re-open the serial port.
5. Assert: `MODEM_READY` is received on the new connection.
6. Send `GET_STATUS`.
7. Assert: modem is operational.

---

### T-0302  Status counter accuracy

**Validates:** MD-0303

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `GET_STATUS`.
3. Assert: `tx_count` = 0, `rx_count` = 0, `tx_fail_count` = 0.
4. Send 5 `SEND_FRAME` messages.
5. Have the radio peer send 3 ESP-NOW frames to the modem.
6. Wait for all 3 `RECV_FRAME` messages.
7. Send `GET_STATUS`.
8. Assert: `tx_count` = 5, `rx_count` = 3.
9. Assert: `uptime_s` > 0.

---

### T-0303  MODEM_READY after RESET

**Validates:** MD-0300, MD-0104

**Procedure:**
1. Send `RESET`.
2. Assert: `MODEM_READY` is received within 2 seconds.
3. Send `RESET` again.
4. Assert: `MODEM_READY` is received within 2 seconds.
5. Repeat 5 times to confirm stability.

---

## 6  Error handling tests

### T-0400  SEND_FRAME with body too short

**Validates:** MD-0202

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send a `SEND_FRAME` with only 3 bytes of body (less than 6-byte MAC + 1 byte data).
3. Send `GET_STATUS`.
4. Assert: modem is still operational (did not crash).
5. Assert: `tx_count` is unchanged (frame was silently discarded).

---

### T-0401  SET_CHANNEL with invalid channel

**Validates:** MD-0206

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SET_CHANNEL(0)` (invalid).
3. Assert: `ERROR(CHANNEL_SET_FAILED)` is received.
4. Send `SET_CHANNEL(15)` (invalid).
5. Assert: `ERROR(CHANNEL_SET_FAILED)` is received.
6. Send `GET_STATUS`.
7. Assert: `channel` is still 1 (unchanged).

---

### T-0402  Framing error recovery

**Validates:** MD-0102

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send 100 random bytes (corrupt framing).
3. Send `RESET`.
4. Assert: `MODEM_READY` is received (modem recovered via resync).

---

## 7  Non-requirement validation

### T-0500  Modem does not interpret frame contents

**Validates:** §6 Non-requirements

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SEND_FRAME` with `frame_data` containing invalid CBOR (e.g., `0xFF 0xFF 0xFF`).
3. Assert: the radio peer receives the frame with the exact same invalid bytes.
4. Assert: modem did not reject, modify, or log an error about the frame contents.

---

## Appendix A  Test index

| ID | Title | Validates |
|----|-------|-----------|
| T-0100 | USB-CDC device enumeration | MD-0100 |
| T-0101 | MODEM_READY on boot | MD-0104 |
| T-0102 | Serial framing — valid frame | MD-0101 |
| T-0103 | Serial framing — oversized len | MD-0102 |
| T-0104 | Unknown message type | MD-0103 |
| T-0200 | Frame forwarding — radio to USB | MD-0201, MD-0205 |
| T-0201 | Frame transmission — USB to radio | MD-0202 |
| T-0202 | Automatic peer registration | MD-0203 |
| T-0203 | Peer table LRU eviction | MD-0204 |
| T-0204 | Frame ordering preserved | MD-0205 |
| T-0205 | Channel change | MD-0206 |
| T-0206 | Channel scanning | MD-0207 |
| T-0300 | RESET clears state | MD-0300 |
| T-0301 | USB disconnection and reconnection | MD-0301 |
| T-0302 | Status counter accuracy | MD-0303 |
| T-0303 | MODEM_READY after RESET | MD-0300, MD-0104 |
| T-0400 | SEND_FRAME with body too short | MD-0202 |
| T-0401 | SET_CHANNEL with invalid channel | MD-0206 |
| T-0402 | Framing error recovery | MD-0102 |
| T-0500 | Modem does not interpret frame contents | §6 Non-requirements |
