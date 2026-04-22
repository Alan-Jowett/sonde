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

> **Implementation note:** The current `device_tests.rs` implementation (`t0101_modem_ready_after_reset`) verifies field values but does not assert the 2-second timing constraint from MD-0104. A future update should record `Instant::now()` before `reset_and_wait()` and assert `elapsed <= Duration::from_secs(2)` on return.

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

**Validates:** MD-0200, MD-0201, MD-0205

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
2. Send `SEND_FRAME` to a known radio peer (to populate the peer table and verify baseline connectivity on the default channel).
3. Send `SET_CHANNEL(6)`.
4. Assert: `SET_CHANNEL_ACK(6)` is received.
5. Have the radio peer send a frame on channel 6.
6. Assert: `RECV_FRAME` is received.
7. Have the radio peer send a frame on channel 1.
8. Assert: no `RECV_FRAME` is received (modem is on channel 6).

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
9. Assert: `uptime_s` < 3 (near-zero after `RESET`).

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

### T-0304  Watchdog triggers on stalled main loop

**Validates:** MD-0302

**Procedure:**
1. Flash a test firmware build that stalls the main loop after a trigger command (e.g., stops calling `feed_watchdog()` on a specific `GET_STATUS` sequence).
2. Send the trigger command.
3. Wait up to 15 seconds.
4. Assert: the modem reboots (watchdog hardware reset) and sends `MODEM_READY` on the serial port.
5. Assert: modem is fully operational after the watchdog-triggered reboot.

> **Note:** This test requires a special test firmware build and real hardware. It cannot be validated via PTY mock. The 10-second watchdog timeout plus reboot time should complete within 15 seconds.

---

## 6  Error handling tests

### T-0400  SEND_FRAME with body too short

**Validates:** MD-0208

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send a `SEND_FRAME` with only 3 bytes of body (less than 6-byte MAC + 1 byte data).
3. Send `GET_STATUS`.
4. Assert: modem is still operational (did not crash).
5. Assert: `tx_count` is unchanged (frame was silently discarded).

---

### T-0401  SET_CHANNEL with invalid channel

**Validates:** MD-0209

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

**Validates:** MD-0205

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

### T-0607a  Server-initiated LESC pairing — passive client

**Validates:** MD-0404 (criterion 5)

**Procedure:**
1. Send `BLE_ENABLE`. Connect a BLE client that does **not** initiate pairing on its own (no `createBond` or equivalent — a plain GATT connect).
2. Assert: the modem initiates LESC pairing from the server side (the client receives an SMP Security Request).
3. Assert: `BLE_PAIRING_CONFIRM` received on gateway side with a 6-digit passkey.
4. Send `BLE_PAIRING_CONFIRM_REPLY(0x01)` (accept).
5. Assert: pairing succeeds, link is encrypted, and `BLE_CONNECTED` received.
6. Send a GATT write. Assert: write is relayed via `BLE_RECV` (the `authenticated` flag is set).

### T-0607b  Pre-auth GATT write buffered until authentication completes

**Validates:** MD-0404 (criterion 5), MD-0409 (criterion 5)

**Procedure:**
1. Send `BLE_ENABLE`. Connect a BLE client (plain GATT connect, no client-initiated pairing).
2. Immediately send a GATT write to the Gateway Command characteristic **before** pairing completes.
3. Assert: the write is **not** forwarded via `BLE_RECV` yet (it is buffered).
4. Assert: the modem logs an info message (`GATT write … bytes buffered (awaiting authentication)`).
5. Allow server-initiated pairing to complete, confirm via `BLE_PAIRING_CONFIRM_REPLY(0x01)`.
6. Assert: the buffered write is flushed and forwarded via `BLE_RECV` after `authenticated` becomes true.
7. Assert: `BLE_CONNECTED` is sent after the flushed write.

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

### T-0617  *(Subsumed by T-0600)*

---

### T-0618  *(Subsumed by T-0600)*

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

### T-0623  Indication confirmation pacing

**Validates:** MD-0403

**Procedure:**
1. Connect via BLE with MTU = 247. Complete LESC pairing.
2. Send a `BLE_INDICATE` from the gateway containing a payload > 488 bytes
   (at least 3 chunks at 244 bytes each).
3. On the phone side, capture the timing of each received indication chunk.
4. Assert: each chunk is received only after the phone's BLE stack has sent
   the ATT Handle Value Confirmation for the previous chunk.
5. Assert: no `"BLE: indication failed:"` errors appear in the modem log.
6. Assert: reassembled message matches the original.

---

### T-0624  Indication pacing under slow client

**Validates:** MD-0403

**Procedure:**
1. Connect via BLE with a long connection interval (e.g. 30 ms).
2. Send a `BLE_INDICATE` from the gateway containing a payload requiring
   ≥ 4 chunks.
3. Assert: all chunks are delivered successfully (no `"indication failed"`
   warnings in modem log).
4. Assert: the modem does not burst-send multiple indications within a
   single connection interval.

---

### T-0625  Send failure increments `tx_fail_count`

**Validates:** MD-0202, MD-0303

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `GET_STATUS` and record baseline `tx_count_0` and `tx_fail_count_0`.
3. Send `SEND_FRAME` to a peer MAC that is not present on the channel
   (e.g. `02:00:00:00:00:01`, a locally administered unicast address,
   on an empty channel).
4. Poll `GET_STATUS` (e.g. every 50 ms, up to 500 ms) until
   `tx_count` > `tx_count_0`.
5. Assert: `tx_count` ≥ `tx_count_0 + 1` (send was attempted).
6. Assert: `tx_fail_count` ≥ `tx_fail_count_0 + 1` (delivery failure was recorded).

---

### T-0626  Peer table cleared on channel change

**Validates:** MD-0206

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SEND_FRAME` messages to N distinct peer MACs, where N equals the
   documented peer table capacity.
3. Send `SET_CHANNEL(6)`, wait for `SET_CHANNEL_ACK(6)`.
4. Send `SEND_FRAME` messages to N new, distinct peer MACs.
5. Assert: all N sends in step 4 succeed without error (proving the table
   had room for N new peers — i.e., it was emptied by the channel change).

---

### T-0627  ESP-NOW resumes after channel scan

**Validates:** MD-0207

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`. Set channel to match the radio peer.
2. Confirm baseline: have the radio peer send a frame; assert `RECV_FRAME`
   received.
3. Send `SCAN_CHANNELS`, wait for `SCAN_RESULT`.
4. Have the radio peer send another frame on the original channel.
5. Assert: `RECV_FRAME` is received (ESP-NOW is operational after scan).
6. Send `SEND_FRAME` to the radio peer.
7. Assert: the radio peer receives the frame (TX path also works after scan).

---

### T-0628  Peer table cleared on RESET

**Validates:** MD-0300

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`.
2. Send `SEND_FRAME` messages to N distinct peer MACs, where N equals the
   documented peer table capacity.
3. Send `RESET`, wait for `MODEM_READY`.
4. Send `SEND_FRAME` messages to N new, distinct peer MACs.
5. Assert: all N sends in step 4 succeed without error (proving the table
   had room for N new peers — i.e., it was emptied by the RESET).

---

### T-0629  ESP-NOW frames discarded during USB disconnect

**Validates:** MD-0301

**Procedure:**
1. Send `RESET`, wait for `MODEM_READY`. Set channel to match the radio peer.
2. Close the host-side serial port (simulate USB-CDC link drop).
3. Have the radio peer send 5 ESP-NOW frames to the modem.
4. Wait 2 seconds, then re-open the serial port.
5. Wait for `MODEM_READY`.
6. Assert: no `RECV_FRAME` messages arrive for the 5 frames sent during
   disconnection (they were discarded, not queued).
7. Have the radio peer send 1 more frame.
8. Assert: `RECV_FRAME` is received (normal operation resumed).

---

### T-0630  BLE relay boundary preservation under rapid writes

**Validates:** MD-0401

**Procedure:**
1. Connect via BLE. Complete LESC pairing.
2. Write 5 distinct test envelopes to the Gateway Command characteristic
   in rapid succession (as fast as the phone BLE stack allows).
3. Assert: exactly 5 `BLE_RECV` serial messages are received on the gateway
   side — no merging, no splitting.
4. Assert: each `BLE_RECV` payload matches the corresponding GATT write
   byte-for-byte, in order.

---

### T-0631  Concurrent fragmented indications are not interleaved

**Validates:** MD-0403

**Procedure:**
1. Connect via BLE with MTU = 247. Complete LESC pairing.
2. Send two `BLE_INDICATE` messages from the gateway in rapid succession,
   each requiring ≥ 2 chunks (each > 244 bytes).
3. On the phone side, capture the order of received indication chunks.
4. Assert: all chunks from the first message arrive before any chunk from
   the second message (no interleaving).
5. Assert: both reassembled messages match their originals.

---

### T-0632  Just Works BLE fallback

**Validates:** MD-0404

**Procedure:**
1. Send `BLE_ENABLE`.
2. Connect a BLE client that only supports Just Works pairing (no
   display/keyboard IO capabilities).
3. Assert: BLE pairing completes successfully without Numeric Comparison.
4. Assert: the resulting link is encrypted.
5. Assert: no `BLE_PAIRING_CONFIRM` is sent to the gateway (Just Works
   does not involve operator confirmation).

---

### T-0633  BLE advertising off after RESET

**Validates:** MD-0407, MD-0412

**Procedure:**
1. Send `BLE_ENABLE`. Scan and confirm Gateway Pairing Service UUID is
   advertised.
2. Send `RESET`, wait for `MODEM_READY`.
3. Scan for BLE advertisements.
4. Assert: no Gateway Pairing Service UUID advertised (RESET disabled BLE).
5. Send `BLE_ENABLE`.
6. Scan for BLE advertisements.
7. Assert: Gateway Pairing Service UUID is advertised again.

---

### T-0634  Write Long reassembly

**Validates:** MD-0409

**Procedure:**
1. Connect via BLE with MTU = 247. Complete LESC pairing.
2. From the phone, perform an ATT Write Long (Prepare Write + Execute Write)
   to the Gateway Command characteristic with a payload > (MTU − 3) bytes.
3. Assert: the modem forwards a single `BLE_RECV` serial message containing
   the complete reassembled payload.
4. Assert: the payload is byte-for-byte identical to what the phone sent.

---

### T-0635  BLE_ENABLE and BLE_DISABLE idempotency

**Validates:** MD-0413

**Procedure:**
1. Send `BLE_ENABLE` twice in succession.
2. Assert: no error or crash; modem is advertising normally.
3. Connect a phone, complete LESC pairing, then disconnect.
4. Assert: modem re-advertises (BLE still enabled).
5. Send `BLE_DISABLE` twice in succession.
6. Assert: no error or crash; modem is not advertising.

---

### T-0636  BLE idle timeout disconnects unfinished pairing

**Validates:** MD-0415

**Procedure (60 s idle-timeout path, pairing not initiated):**
1. Send `BLE_ENABLE`. Connect a BLE client, but do **not** initiate or accept LESC pairing (do not access characteristics that require encryption/authentication, and reject/ignore any pairing prompts on the client).
2. Wait at least 60 seconds without sending any GATT writes that trigger security or completing pairing (the connection must remain in an unpaired, idle state).
3. Assert: the modem disconnects the idle client due to the 60 s BLE idle timeout.
4. Assert: `BLE_DISCONNECTED` is received on the gateway side as a result of the idle-timeout disconnect.

---

## 9  Operational logging tests

### T-0700  ESP-NOW received frame logged

**Validates:** MD-0500

**Procedure:**
1. Transmit an ESP-NOW frame from a radio peer.
2. Assert: the diagnostic UART contains an INFO-level log line with the peer MAC, payload length, and RSSI.

---

### T-0701  ESP-NOW sent frame logged

**Validates:** MD-0500

**Procedure:**
1. Send a `SEND_FRAME` command to the modem via USB-CDC that is expected to succeed.
2. Assert: the diagnostic UART contains an INFO-level log line with the destination peer MAC, payload length, and send result (success).
3. Induce an ESP-NOW send failure (for example, by targeting a non-responsive or invalid peer) using a `SEND_FRAME` command.
4. Assert: the diagnostic UART contains a WARN-level log line with the destination peer MAC, payload length, and send result (failure).

---

### T-0702  USB-CDC messages logged at DEBUG

**Validates:** MD-0503

**Procedure:**
1. Enable DEBUG-level logging on the modem.
2. Send a `SEND_FRAME` command from the gateway.
3. Assert: the diagnostic UART contains a DEBUG-level log line indicating the received message type.
4. Trigger a `RECV_FRAME` event from the radio.
5. Assert: the diagnostic UART contains a DEBUG-level log line indicating the sent message type and encoded length.

---

### T-0703  BLE lifecycle events logged

**Validates:** MD-0501

**Procedure:**
1. Send `BLE_ENABLE`. Assert: INFO log "BLE advertising started".
2. Connect a BLE client. Assert: INFO log with peer address and MTU.
3. Complete pairing and disconnect. Assert: INFO log with peer address and reason code.
4. Send `BLE_DISABLE`. Assert: INFO log "BLE advertising stopped".

---

### T-0704  BLE GATT write logging

**Validates:** MD-0502

**Procedure:**
1. Connect and authenticate a BLE client.
2. Write 20 bytes via GATT.
3. Assert: INFO log indicating GATT write with payload length 20.

### T-0705  BLE pairing event — LESC security initiated

**Validates:** MD-0504

**Procedure:**
1. Connect a BLE client and trigger server-initiated LESC pairing.
2. Capture UART diagnostic output.
3. Assert: an INFO log is emitted indicating LESC pairing initiated, including the connection handle.

### T-0706  BLE pairing event — authentication success

**Validates:** MD-0504

**Procedure:**
1. Complete a successful BLE LESC pairing.
2. Capture UART diagnostic output.
3. Assert: an INFO log is emitted indicating authentication success.

### T-0707  BLE pairing event — authentication failure

**Validates:** MD-0504

**Procedure:**
1. Trigger a BLE pairing authentication failure (e.g., reject Numeric Comparison).
2. Capture UART diagnostic output.
3. Assert: a WARN log is emitted indicating authentication failure with the failure reason.

### T-0708  Build-type quiet strips INFO call-sites

**Validates:** MD-0505

**Procedure:**
1. Build the modem firmware with the default `quiet` feature.
2. Boot the modem and forward a few ESP-NOW frames.
3. Capture UART diagnostic output.
4. Assert: no INFO-level log lines appear (MD-0500–MD-0504 logs are compiled out).
5. Assert: WARN-level logs (e.g., send failures) still appear.

### T-0709  Build-type verbose retains INFO and DEBUG

**Validates:** MD-0505

**Procedure:**
1. Build the modem firmware with `--features esp,verbose --no-default-features`.
2. Boot the modem and forward frames.
3. Capture UART diagnostic output.
4. Assert: INFO-level operational logs (ESP-NOW frame received/sent) are present.
5. Assert: DEBUG-level logs (USB-CDC messages) are visible.

### T-0710  Error diagnostic — BLE indication failure

**Validates:** MD-0506

**Procedure:**
1. Connect a BLE client and trigger an indication failure (e.g., disconnect mid-indication).
2. Capture UART diagnostic output.
3. Assert: the error log includes the operation name and NimBLE error (debug string).

### T-0711  Error diagnostic — ESP-NOW send failure

**Validates:** MD-0506

**Procedure:**
1. Trigger an ESP-NOW send failure (e.g., send to an unreachable peer MAC).
2. Capture UART diagnostic output.
3. Assert: the error log includes the target peer MAC address, payload length, and success flag.
4. Assert: raw payload bytes are not logged.

---

## 10  Button input tests

### T-0800  Button GPIO reads HIGH when idle

**Validates:** MD-0600

**Procedure:**
1. Boot the modem with no button pressed.
2. Read GPIO2 value.
3. Assert: GPIO2 reads HIGH.

---

### T-0801  Short press emits BUTTON_SHORT

**Validates:** MD-0601, MD-0602, MD-0603

**Procedure:**
1. Simulate a 500 ms button press (GPIO LOW for 500 ms, then HIGH).
2. Wait for `EVENT_BUTTON` on the serial link.
3. Assert: `EVENT_BUTTON` (type `0xB0`) is received.
4. Assert: `button_type` = `0x00` (BUTTON_SHORT).

---

### T-0802  Long press emits BUTTON_LONG

**Validates:** MD-0601, MD-0602, MD-0603

**Procedure:**
1. Simulate a 1500 ms button press (GPIO LOW for 1500 ms, then HIGH).
2. Wait for `EVENT_BUTTON` on the serial link.
3. Assert: `EVENT_BUTTON` (type `0xB0`) is received.
4. Assert: `button_type` = `0x01` (BUTTON_LONG).

---

### T-0803  Boundary — 999 ms press is SHORT, 1000 ms press is LONG

**Validates:** MD-0602

**Procedure:**
1. Simulate a 999 ms button press, then release.
2. Assert: `button_type` = `0x00` (BUTTON_SHORT).
3. Simulate a 1000 ms button press, then release.
4. Assert: `button_type` = `0x01` (BUTTON_LONG).

---

### T-0804  Debounce rejects glitches shorter than 30 ms

**Validates:** MD-0601

**Procedure:**
1. Simulate a 20 ms LOW pulse (press), then return HIGH.
2. Wait 100 ms.
3. Assert: no `EVENT_BUTTON` is emitted.

---

### T-0805  No event emitted while button is held

**Validates:** MD-0602

**Procedure:**
1. Simulate button press (GPIO LOW) and hold for 3 seconds without releasing.
2. Assert: no `EVENT_BUTTON` is emitted during the hold.
3. Release the button (GPIO HIGH).
4. Assert: `EVENT_BUTTON` with `button_type` = `0x01` (BUTTON_LONG) is emitted on release.

---

### T-0806  Button events do not interfere with ESP-NOW

**Validates:** MD-0604

**Procedure:**
1. Establish a baseline ESP-NOW forwarding rate (frames per second) with no button activity.
2. Repeat the same traffic load while simultaneously simulating rapid button presses (short and long).
3. Assert: ESP-NOW forwarding rate and latency show no measurable regression attributable to button polling.
4. Assert: `EVENT_BUTTON` messages are received correctly during the load.

---

### T-0806a  Button events do not interfere with BLE pairing

**Validates:** MD-0604

**Procedure:**
1. Enable BLE advertising via `BLE_ENABLE`.
2. Initiate a BLE LESC pairing from a phone while simultaneously simulating rapid button presses.
3. Assert: BLE pairing completes successfully.
4. Assert: `BLE_CONNECTED` and `EVENT_BUTTON` messages are both received on USB-CDC.

---

### T-0807  No button-semantic logic in modem

**Validates:** MD-0605

**Procedure:**
1. Simulate a BUTTON_LONG press (≥ 1 s).
2. Assert: `EVENT_BUTTON` is emitted.
3. Assert: BLE advertising state is unchanged.
4. Assert: no pairing mode is entered.
5. Assert: no display output is triggered.

---

### T-0808  EVENT_BUTTON round-trip codec

**Validates:** MD-0603

**Procedure:**
1. Construct an `EVENT_BUTTON` message with `button_type` = `0x00`.
2. Encode to serial frame.
3. Decode the frame.
4. Assert: decoded message matches the original.
5. Repeat with `button_type` = `0x01`.

---

### T-0809  Release-bounce rejected

**Validates:** MD-0601

**Procedure:**
1. Simulate a valid button press (GPIO LOW for 500 ms).
2. On release, bounce: GPIO HIGH for 15 ms, LOW for 10 ms, then HIGH sustained.
3. Assert: exactly one `EVENT_BUTTON` is emitted (the bounce does not create a second event).

---

### T-0810  Back-to-back presses emit independent events

**Validates:** MD-0603

**Procedure:**
1. Simulate a short press (300 ms), release, wait 100 ms, then simulate a long press (1200 ms), release.
2. Assert: two `EVENT_BUTTON` messages are received.
3. Assert: first `button_type` = `0x00` (SHORT), second `button_type` = `0x01` (LONG).
4. Assert: no state from the first press leaks into the second.

---

## Appendix A  Test index

| ID | Title | Validates |
|----|-------|-----------|
| T-0100 | USB-CDC device enumeration | MD-0100 |
| T-0101 | MODEM_READY on boot | MD-0104 |
| T-0102 | Serial framing — valid frame and max length | MD-0101, MD-0102 |
| T-0103 | Serial framing — oversized len | MD-0102 |
| T-0104 | Unknown message type | MD-0103 |
| T-0200 | Frame forwarding — radio to USB | MD-0200, MD-0201, MD-0205 |
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
| T-0304 | Watchdog triggers on stalled main loop | MD-0302 |
| T-0400 | SEND_FRAME with body too short | MD-0208 |
| T-0401 | SET_CHANNEL with invalid channel | MD-0209 |
| T-0402 | Framing error recovery | MD-0102 |
| T-0500 | Modem does not interpret frame contents | MD-0205 |
| T-0600 | Gateway Pairing Service lifecycle | MD-0407, MD-0412, MD-0413 |
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
| T-0623 | Indication confirmation pacing | MD-0403 |
| T-0624 | Indication pacing under slow client | MD-0403 |
| T-0625 | Send failure increments `tx_fail_count` | MD-0202, MD-0303 |
| T-0626 | Peer table cleared on channel change | MD-0206 |
| T-0627 | ESP-NOW resumes after channel scan | MD-0207 |
| T-0628 | Peer table cleared on RESET | MD-0300 |
| T-0629 | ESP-NOW frames discarded during USB disconnect | MD-0301 |
| T-0630 | BLE relay boundary preservation under rapid writes | MD-0401 |
| T-0631 | Concurrent fragmented indications are not interleaved | MD-0403 |
| T-0632 | Just Works BLE fallback | MD-0404 |
| T-0633 | BLE advertising off after RESET | MD-0407, MD-0412 |
| T-0634 | Write Long reassembly | MD-0409 |
| T-0635 | BLE_ENABLE and BLE_DISABLE idempotency | MD-0413 |
| T-0636 | BLE idle timeout disconnects unfinished pairing | MD-0415 |
| T-0700 | ESP-NOW received frame logged | MD-0500 |
| T-0701 | ESP-NOW sent frame logged | MD-0500 |
| T-0702 | USB-CDC messages logged at DEBUG | MD-0503 |
| T-0703 | BLE lifecycle events logged | MD-0501 |
| T-0704 | BLE GATT write logging | MD-0502 |
| T-0705 | BLE pairing event — LESC security initiated | MD-0504 |
| T-0706 | BLE pairing event — authentication success | MD-0504 |
| T-0707 | BLE pairing event — authentication failure | MD-0504 |
| T-0708 | Build-type quiet strips INFO call-sites | MD-0505 |
| T-0709 | Build-type verbose retains INFO and DEBUG | MD-0505 |
| T-0710 | Error diagnostic — BLE indication failure | MD-0506 |
| T-0711 | Error diagnostic — ESP-NOW send failure | MD-0506 |
| T-0800 | Button GPIO reads HIGH when idle | MD-0600 |
| T-0801 | Short press emits BUTTON_SHORT | MD-0601, MD-0602, MD-0603 |
| T-0802 | Long press emits BUTTON_LONG | MD-0601, MD-0602, MD-0603 |
| T-0803 | Boundary — 999 ms SHORT, 1000 ms LONG | MD-0602 |
| T-0804 | Debounce rejects glitches shorter than 30 ms | MD-0601 |
| T-0805 | No event emitted while button is held | MD-0602 |
| T-0806 | Button events do not interfere with ESP-NOW | MD-0604 |
| T-0806a | Button events do not interfere with BLE pairing | MD-0604 |
| T-0807 | No button-semantic logic in modem | MD-0605 |
| T-0808 | EVENT_BUTTON round-trip codec | MD-0603 |
| T-0809 | Release-bounce rejected | MD-0601 |
| T-0810 | Back-to-back presses emit independent events | MD-0603 |
