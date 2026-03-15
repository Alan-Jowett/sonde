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

### T-0102  Serial framing — valid frame and max length

**Validates:** MD-0101, MD-0102

**Procedure:**
1. Send `RESET` to the modem.
2. Wait for `MODEM_READY`.
3. Send `GET_STATUS`.
4. Assert: response is a well-formed `STATUS` message with correct `LEN || TYPE || BODY` envelope.
5. Construct a well-formed serial frame with `len` = 512 (maximum allowed). Use `type` = 0x7F (unknown) and 511 bytes of padding as body.
6. Send the `len` = 512 frame to the modem.
7. Assert: modem silently discards the unknown type (no crash, no `ERROR`).
8. Send `GET_STATUS`.
9. Assert: `STATUS` response is received (framing remains synchronized after max-length frame).

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
2. Send `SEND_FRAME` to 21 unique MAC addresses (exceeds ~20 peer limit). Since most of these MACs have no real radio peer, the modem may report send failures — this is expected. The test validates peer table management, not delivery.
3. Assert: modem does not crash or send `ERROR`.
4. Send `GET_STATUS`.
5. Assert: `tx_count` = 21 (all sends were attempted despite evictions).
6. Send `SEND_FRAME` to the 1st MAC again (which was evicted).
7. Assert: `tx_count` increments (peer was re-registered after eviction).

**Note:** For a stronger test, use a test firmware build with a reduced peer table capacity (e.g., 3 entries) so eviction can be validated with a small number of real radio peers.

---

### T-0204  Frame ordering preserved (radio → USB)

**Validates:** MD-0205

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SET_CHANNEL(CH)` to match the radio peer; wait for `SET_CHANNEL_ACK(CH)`.
3. Have the radio peer send 10 ESP-NOW frames with sequential payload values (0x01 through 0x0A) on channel CH.
4. Collect `RECV_FRAME` messages from USB for a bounded period (until no new frames arrive for 500ms, or after a maximum timeout).
5. From the collected `RECV_FRAME` messages, extract payload values and assert that:
   - The sequence of observed payloads is strictly increasing (no reordering or duplicates).
   - At least 2 sequential payloads were observed to make the ordering check meaningful.

**Note:** ESP-NOW may drop frames under adverse RF conditions. The test validates ordering of delivered frames, not guaranteed delivery.

---

### T-0204a  Frame ordering preserved (USB → radio)

**Validates:** MD-0205

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SET_CHANNEL(CH)` to match the radio peer; wait for `SET_CHANNEL_ACK(CH)`.
3. Send 10 `SEND_FRAME` messages with sequential payload values (0x01 through 0x0A) to the radio peer's MAC.
4. Have the radio peer collect received ESP-NOW frames for a bounded period.
5. From the collected frames, extract payload values and assert that:
   - The sequence of observed payloads is strictly increasing (no reordering or duplicates).
   - At least 2 sequential payloads were observed.

**Note:** Same loss-tolerant approach as T-0204.

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
5. Assert: `count` is 14.
6. Assert: the result contains exactly one entry for each WiFi channel 1–14 (no missing channels, no duplicates).
7. Assert: each entry has valid `channel` (1–14), `ap_count` ≥ 0, `strongest_rssi` ≤ 0 (or 0 if no APs).

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

### T-0301  USB-CDC serial link drop and reconnection

**Validates:** MD-0301

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Close the host-side serial port (drop DTR, simulating USB-CDC link drop without physical unplug).
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

**Validates:** modem-protocol.md §6.1

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send a `SEND_FRAME` with only 3 bytes of body (less than 6-byte MAC + 1 byte data).
3. Send `GET_STATUS`.
4. Assert: modem is still operational (did not crash).
5. Assert: `tx_count` is unchanged (frame was silently discarded).

---

### T-0401  SET_CHANNEL with invalid channel

**Validates:** modem-protocol.md §6.1

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
4. Assert: modem does not send any `ERROR` or diagnostic message over the serial protocol in response to the invalid payload.

---

## 8  BLE pairing relay tests

### T-0600  Gateway Pairing Service lifecycle

**Validates:** MD-0407, MD-0412, MD-0413

**Procedure:**
1. Power on modem with gateway connected via USB-CDC. Do NOT send `BLE_ENABLE`.
2. Scan for BLE advertisements from the modem.
3. Assert: no Gateway Pairing Service UUID advertised (BLE advertising is off by default).
4. Send `BLE_ENABLE` to modem.
5. Scan for BLE advertisements from the modem.
6. Assert: Gateway Pairing Service UUID `0000FE60-0000-1000-8000-00805F9B34FB` is advertised.
7. Connect a BLE client to the modem, then disconnect.
8. Scan for BLE advertisements again.
9. Assert: Gateway Pairing Service UUID is advertised again after disconnect (BLE still enabled).

> **Note:** This test consolidates the default-off (T-0617), enable (T-0618), and post-disconnect re-advertisement assertions into a single lifecycle test. T-0617 and T-0618 are subsumed by this test.

---

### T-0601  BLE GATT characteristic setup

**Validates:** MD-0400

**Procedure:**
1. Connect to modem via BLE.
2. Discover services and characteristics.
3. Assert: Gateway Command characteristic (UUID `0000FE61-0000-1000-8000-00805F9B34FB`) exists with Write and Indicate properties.

---

### T-0602  MTU negotiation ≥ 247

**Validates:** MD-0402

**Procedure:**
1. Connect to modem via BLE and request MTU = 512.
2. Assert: negotiated MTU ≥ 247.

---

### T-0602a  MTU negotiation below minimum rejected

**Validates:** MD-0402

**Procedure:**
1. Connect to modem via BLE and force a low MTU (e.g., 185).
2. Assert: modem rejects or disconnects the BLE connection.
3. Assert: no `BLE_CONNECTED` event is sent to the gateway.

---

### T-0603  BLE write → USB-CDC relay

**Validates:** MD-0401

**Procedure:**
1. Connect via BLE. Write a test envelope (TYPE=0x01, LEN, BODY) to the Gateway Command characteristic.
2. Read from USB-CDC serial on the gateway side.
3. Assert: gateway receives a `BLE_RECV` (0xA0) serial message whose `ble_data` payload matches the bytes written over BLE.

---

### T-0604  USB-CDC → BLE indication relay

**Validates:** MD-0401

**Procedure:**
1. Connect via BLE, subscribe to indications on Gateway Command characteristic.
2. Send a `BLE_INDICATE` (0x20) serial message from the gateway side containing a test BLE envelope.
3. Assert: the exact bytes arrive as a BLE indication on the phone side.

---

### T-0605  Indication fragmentation

**Validates:** MD-0403

**Procedure:**
1. Connect via BLE with MTU = 247. Send a message from the gateway that is > (247 − 3) = 244 bytes.
2. Assert: the modem fragments the message into multiple indications, each ≤ 244 bytes.
3. Assert: reassembled message on the phone side matches the original.

---

### T-0606  Opaque relay (no content inspection)

**Validates:** MD-0401

**Procedure:**
1. Write a GATT payload containing invalid/garbage bytes (not a valid BLE envelope).
2. Assert: modem forwards the bytes to USB-CDC without modification or error.
3. Assert: modem does not send any ERROR or diagnostic message.

---

### T-0607  BLE LESC Numeric Comparison — link establishment

**Validates:** MD-0404

**Procedure:**
1. Send `BLE_ENABLE`. Connect to modem via BLE and initiate LESC Numeric Comparison pairing.
2. Assert: `BLE_PAIRING_CONFIRM` received on gateway side with a 6-digit passkey.
3. Send `BLE_PAIRING_CONFIRM_REPLY(0x01)` (accept).
4. Assert: pairing succeeds, link is encrypted, and `BLE_CONNECTED` received.

> **Note:** This test validates link establishment only. Pin relay accept/reject/timeout semantics are covered by T-0620, T-0621, and T-0622 (MD-0414).

---

### T-0608  BLE disconnect cleanup

**Validates:** MD-0405

**Procedure:**
1. Connect via BLE, perform a GATT write, then disconnect.
2. Reconnect via BLE.
3. Assert: GATT state is clean (no stale data from previous connection).
4. Assert: new writes and indications work correctly.

---

### T-0609  BLE and ESP-NOW concurrent operation

**Validates:** MD-0405

**Procedure:**
1. Establish a BLE connection and start a GATT write/indicate exchange.
2. Simultaneously send ESP-NOW frames through the modem.
3. Assert: both BLE and ESP-NOW operations complete successfully without interference.

---

### T-0609a  Second BLE connection rejected while one is active

**Validates:** MD-0405

**Procedure:**
1. Connect a first phone via BLE. Complete MTU negotiation and LESC pairing. Confirm `BLE_CONNECTED` received on gateway side.
2. Attempt to connect a second BLE client to the Gateway Pairing Service.
3. Assert: the second connection is rejected or queued — only one concurrent BLE client is supported.
4. Assert: the first connection continues to operate normally.

---

### T-0610  *(Removed — superseded by T-0614 and T-0615)*

---

### T-0611  BLE_INDICATE relay to phone

**Validates:** MD-0408

**Procedure:**
1. Connect a phone via BLE. Complete MTU negotiation and LESC pairing. Confirm `BLE_CONNECTED` received on gateway side.
2. Send `BLE_INDICATE` (0x20) from gateway with a test BLE envelope.
3. Assert: phone receives a GATT indication with the exact bytes.

---

### T-0612  BLE_INDICATE with no BLE client

**Validates:** MD-0408

**Procedure:**
1. With no BLE client connected, send `BLE_INDICATE` from gateway.
2. Assert: no error; message is silently discarded.

---

### T-0613  BLE_RECV forwarding

**Validates:** MD-0409

**Procedure:**
1. Connect a phone via BLE.
2. Phone writes a BLE envelope to the Gateway Command characteristic.
3. Assert: gateway receives `BLE_RECV` (0xA0) with the exact bytes.

---

### T-0613a  Empty BLE_INDICATE silently discarded

**Validates:** MD-0408

**Procedure:**
1. Connect a phone via BLE.
2. Send a `BLE_INDICATE` serial frame with an empty body (no `ble_data`).
3. Assert: modem silently discards the message — no indication is sent to the phone.

---

### T-0613b  Empty GATT write silently discarded

**Validates:** MD-0409

**Procedure:**
1. Connect a phone via BLE.
2. Phone writes zero bytes to the Gateway Command characteristic.
3. Assert: no `BLE_RECV` message is sent to the gateway.

---

### T-0614  BLE_CONNECTED notification

**Validates:** MD-0410

**Procedure:**
1. Connect a phone via BLE. Complete LESC pairing and MTU negotiation.
2. Assert: gateway receives `BLE_CONNECTED` (0xA1) with peer address and MTU ≥ 247.

---

### T-0615  BLE_DISCONNECTED notification

**Validates:** MD-0411

**Procedure:**
1. Connect a phone via BLE.
2. Disconnect the phone.
3. Assert: gateway receives `BLE_DISCONNECTED` (0xA2) with peer address and reason code.

---

### T-0616  BLE relay round-trip

**Validates:** MD-0408, MD-0409

**Procedure:**
1. Connect a phone via BLE.
2. Phone writes `REQUEST_GW_INFO` envelope to Gateway Command characteristic.
3. Assert: gateway receives `BLE_RECV` with the envelope.
4. Gateway sends `BLE_INDICATE` with `GW_INFO_RESPONSE` envelope.
5. Assert: phone receives the indication with the exact response bytes.

---

### T-0617  BLE advertising off by default

**Validates:** MD-0412

**Procedure:**
1. Power on modem. Do NOT send `BLE_ENABLE`.
2. Scan for BLE advertisements.
3. Assert: no Gateway Pairing Service UUID advertised.

---

### T-0618  BLE_ENABLE starts advertising

**Validates:** MD-0413

**Procedure:**
1. Send `BLE_ENABLE` to modem.
2. Scan for BLE advertisements.
3. Assert: Gateway Pairing Service UUID is advertised.

---

### T-0619  BLE_DISABLE stops advertising and disconnects

**Validates:** MD-0413

**Procedure:**
1. Send `BLE_ENABLE`. Connect a phone via BLE.
2. Send `BLE_DISABLE`.
3. Assert: phone is disconnected. `BLE_DISCONNECTED` received.
4. Scan for BLE advertisements. Assert: no advertising.

---

### T-0620  Numeric Comparison pin relay

**Validates:** MD-0414

**Procedure:**
1. Send `BLE_ENABLE`. Connect phone with Numeric Comparison pairing.
2. Assert: `BLE_PAIRING_CONFIRM` received with a 6-digit passkey.
3. Send `BLE_PAIRING_CONFIRM_REPLY(0x01)`.
4. Assert: pairing completes. `BLE_CONNECTED` received.

---

### T-0621  Numeric Comparison rejected

**Validates:** MD-0414

**Procedure:**
1. Send `BLE_ENABLE`. Connect phone with Numeric Comparison pairing.
2. Assert: `BLE_PAIRING_CONFIRM` received.
3. Send `BLE_PAIRING_CONFIRM_REPLY(0x00)`.
4. Assert: pairing rejected. No `BLE_CONNECTED` received.

---

### T-0622  Numeric Comparison timeout

**Validates:** MD-0414

**Procedure:**
1. Send `BLE_ENABLE`. Connect phone with Numeric Comparison pairing.
2. Assert: `BLE_PAIRING_CONFIRM` received.
3. Do not send a reply. Wait 30 s.
4. Assert: pairing rejected.

---

## Appendix A  Test index

| ID | Title | Validates |
|----|-------|-----------|
| T-0100 | USB-CDC device enumeration | MD-0100 |
| T-0101 | MODEM_READY on boot | MD-0104 |
| T-0102 | Serial framing — valid frame and max length | MD-0101, MD-0102 |
| T-0103 | Serial framing — oversized len | MD-0102 |
| T-0104 | Unknown message type | MD-0103 |
| T-0200 | Frame forwarding — radio to USB | MD-0201, MD-0205 |
| T-0201 | Frame transmission — USB to radio | MD-0202 |
| T-0202 | Automatic peer registration | MD-0203 |
| T-0203 | Peer table LRU eviction | MD-0204 |
| T-0204 | Frame ordering preserved (radio → USB) | MD-0205 |
| T-0204a | Frame ordering preserved (USB → radio) | MD-0205 |
| T-0205 | Channel change | MD-0206 |
| T-0206 | Channel scanning | MD-0207 |
| T-0300 | RESET clears state | MD-0300 |
| T-0301 | USB-CDC serial link drop and reconnection | MD-0301 |
| T-0302 | Status counter accuracy | MD-0303 |
| T-0303 | MODEM_READY after RESET | MD-0300, MD-0104 |
| T-0400 | SEND_FRAME with body too short | modem-protocol.md §6.1 |
| T-0401 | SET_CHANNEL with invalid channel | modem-protocol.md §6.1 |
| T-0402 | Framing error recovery | MD-0102 |
| T-0500 | Modem does not interpret frame contents | §6 Non-requirements |
| T-0600 | Gateway Pairing Service advertisement | MD-0407, MD-0412, MD-0413 |
| T-0601 | BLE GATT characteristic setup | MD-0400 |
| T-0602 | MTU negotiation ≥ 247 | MD-0402 |
| T-0602a | MTU negotiation below minimum rejected | MD-0402 |
| T-0603 | BLE write → USB-CDC relay | MD-0401 |
| T-0604 | USB-CDC → BLE indication relay | MD-0401 |
| T-0605 | Indication fragmentation | MD-0403 |
| T-0606 | Opaque relay (no content inspection) | MD-0401 |
| T-0607 | BLE LESC Numeric Comparison — link establishment | MD-0404 |
| T-0608 | BLE disconnect cleanup | MD-0405 |
| T-0609 | BLE and ESP-NOW concurrent operation | MD-0405 |
| T-0609a | Second BLE connection rejected while one is active | MD-0405 |
| T-0610 | *(Removed — superseded by T-0614/T-0615)* | — |
| T-0611 | BLE_INDICATE relay to phone | MD-0408 |
| T-0612 | BLE_INDICATE with no BLE client | MD-0408 |
| T-0613 | BLE_RECV forwarding | MD-0409 |
| T-0613a | Empty BLE_INDICATE silently discarded | MD-0408 |
| T-0613b | Empty GATT write silently discarded | MD-0409 |
| T-0614 | BLE_CONNECTED notification | MD-0410 |
| T-0615 | BLE_DISCONNECTED notification | MD-0411 |
| T-0616 | BLE relay round-trip | MD-0408, MD-0409 |
| T-0617 | *(Subsumed by T-0600)* | MD-0412 |
| T-0618 | *(Subsumed by T-0600)* | MD-0413 |
| T-0619 | BLE_DISABLE stops advertising and disconnects | MD-0413 |
| T-0620 | Numeric Comparison pin relay | MD-0414 |
| T-0621 | Numeric Comparison rejected | MD-0414 |
| T-0622 | Numeric Comparison timeout | MD-0414 |
