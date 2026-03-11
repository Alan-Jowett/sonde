<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Requirements Specification

> **Document status:** Draft
> **Source:** Derived from [modem-protocol.md](modem-protocol.md).
> **Scope:** This document covers the ESP32-S3 **radio modem firmware** only. Gateway-side modem transport requirements are in [gateway-requirements.md](gateway-requirements.md) §11.
> **Related:** [modem-protocol.md](modem-protocol.md), [gateway-requirements.md](gateway-requirements.md), [node-requirements.md](node-requirements.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Modem** | An ESP32-S3 device connected to the gateway host via USB, acting as a transparent ESP-NOW radio bridge. |
| **Gateway host** | The computer running the Sonde gateway service. It has no ESP-NOW radio hardware of its own. |
| **Modem serial protocol** | The length-prefixed framing protocol defined in [modem-protocol.md](modem-protocol.md). |
| **Peer table** | The ESP-NOW peer list maintained by the modem firmware (max ~20 entries on ESP-IDF). |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`MD-XXYY`).
- **Title** — Short name.
- **Description** — What the modem firmware must do.
- **Acceptance criteria** — Observable, testable conditions that confirm the requirement is met.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Section of the modem protocol specification that motivates the requirement.

---

## 3  USB-CDC interface

### MD-0100  USB-CDC device presentation

**Priority:** Must
**Source:** modem-protocol.md §1

**Description:**
The modem firmware MUST present a USB-CDC ACM device when connected to a host via USB.

**Acceptance criteria:**

1. Host OS enumerates a virtual serial port (e.g., `/dev/ttyACM0` on Linux, `COMx` on Windows).
2. No special host-side driver is required beyond the standard CDC ACM class driver.

---

### MD-0101  Serial framing compliance

**Priority:** Must
**Source:** modem-protocol.md §2

**Description:**
The modem firmware MUST implement the length-prefixed framing protocol defined in [modem-protocol.md §2](modem-protocol.md#2--serial-framing).

**Acceptance criteria:**

1. All outbound messages use the `LEN || TYPE || BODY` envelope.
2. Inbound frames with `len` > 512 are discarded.
3. Inbound frames with unknown `type` values are silently discarded.

---

### MD-0102  Maximum frame size

**Priority:** Must
**Source:** modem-protocol.md §2.1

**Description:**
The modem firmware MUST accept serial frames with `len` values up to 512 (i.e., up to 514 bytes total including the 2-byte `LEN` field).

**Acceptance criteria:**

1. A serial frame with `len` = 512 is accepted and processed without error.
2. A frame with `len` > 512 triggers the `RESET`-based resynchronization procedure.

---

### MD-0103  Unknown message types

**Priority:** Must
**Source:** modem-protocol.md §2.2, §8.1

**Description:**
The modem firmware MUST silently discard serial frames with unrecognized `type` values. This ensures forward compatibility when the gateway uses a newer protocol version.

**Acceptance criteria:**

1. Sending a frame with an undefined type value does not crash, hang, or produce an error.
2. The modem continues processing subsequent valid frames normally.

---

### MD-0104  Ready notification timing

**Priority:** Must
**Source:** modem-protocol.md §4.3

**Description:**
The modem firmware MUST send `MODEM_READY` within 2 seconds of USB enumeration.

**Acceptance criteria:**

1. A test harness that opens the serial port receives `MODEM_READY` within 2 seconds.

---

## 4  ESP-NOW interface

### MD-0200  ESP-NOW initialization

**Priority:** Must
**Source:** modem-protocol.md §5.1

**Description:**
The modem firmware MUST initialize ESP-NOW in WiFi station mode on the configured channel (default: channel 1).

**Acceptance criteria:**

1. After `MODEM_READY`, the modem can receive ESP-NOW frames.
2. After `MODEM_READY`, the modem can transmit ESP-NOW frames via `SEND_FRAME`.

---

### MD-0201  Frame forwarding (radio → USB)

**Priority:** Must
**Source:** modem-protocol.md §4.4

**Description:**
The modem firmware MUST forward all received ESP-NOW frames to the gateway as `RECV_FRAME` messages, including the sender MAC address and RSSI.

**Acceptance criteria:**

1. Every ESP-NOW frame received by the modem produces exactly one `RECV_FRAME` message on USB.
2. `peer_mac` matches the sender's MAC address.
3. `rssi` reflects the received signal strength.
4. `frame_data` is byte-for-byte identical to the received ESP-NOW payload.

---

### MD-0202  Frame transmission (USB → radio)

**Priority:** Must
**Source:** modem-protocol.md §4.1

**Description:**
On `SEND_FRAME`, the modem firmware MUST transmit the frame via ESP-NOW to the specified peer MAC address. Sends are fire-and-forget; delivery failures increment `tx_fail_count`.

**Acceptance criteria:**

1. `esp_now_send()` is called with the correct peer MAC and frame data.
2. `tx_count` is incremented on every send attempt.
3. `tx_fail_count` is incremented when the ESP-NOW send callback reports failure.

---

### MD-0203  Automatic peer registration

**Priority:** Must
**Source:** modem-protocol.md §4.1

**Description:**
The modem firmware MUST auto-register unknown peer MAC addresses in the ESP-NOW peer table before sending. This is transparent to the gateway.

**Acceptance criteria:**

1. A `SEND_FRAME` to a never-before-seen MAC address succeeds without prior registration by the gateway.

---

### MD-0204  Peer table eviction

**Priority:** Should
**Source:** modem-protocol.md §4.1

**Description:**
When the ESP-NOW peer table is full (~20 entries), the modem firmware SHOULD evict the least-recently-used peer to make room for the new one.

**Acceptance criteria:**

1. After 20+ unique peers have been sent to, new peers can still be added.
2. Evicted peers can be re-registered transparently on next `SEND_FRAME`.

---

### MD-0205  Frame ordering

**Priority:** Must
**Source:** modem-protocol.md §4.4

**Description:**
The modem firmware MUST NOT buffer, reorder, filter, or modify frame data. Frames MUST be forwarded in the order received, in both directions.

**Acceptance criteria:**

1. Frames arrive at the gateway in the same order they were received over ESP-NOW.
2. Frames arrive at ESP-NOW in the same order they were received over USB.

---

### MD-0206  Channel change

**Priority:** Must
**Source:** modem-protocol.md §4.2

**Description:**
The modem firmware MUST support channel changes via `SET_CHANNEL` without requiring a reboot. The modem MUST clear its ESP-NOW peer table on channel change and respond with `SET_CHANNEL_ACK`.

**Acceptance criteria:**

1. After `SET_CHANNEL(N)`, the modem operates on channel N.
2. `SET_CHANNEL_ACK(N)` is sent after the change takes effect.
3. The peer table is empty after a channel change.

---

### MD-0207  Channel scanning

**Priority:** Must
**Source:** modem-protocol.md §4.7

**Description:**
On `SCAN_CHANNELS`, the modem firmware MUST perform a WiFi AP scan across all channels using `esp_wifi_scan_start()` and report per-channel AP count and strongest RSSI via `SCAN_RESULT`.

**Acceptance criteria:**

1. Scan covers channels 1–14.
2. Each entry in `SCAN_RESULT` contains `channel`, `ap_count`, and `strongest_rssi`.
3. Modem resumes normal ESP-NOW operation after scan completes.

---

## 5  Reliability and reset

### MD-0300  Reset command

**Priority:** Must
**Source:** modem-protocol.md §5.5

**Description:**
On `RESET`, the modem firmware MUST de-initialize ESP-NOW, clear the peer table, reset all counters, re-initialize ESP-NOW on channel 1, reset the receive-side framing parser, and send `MODEM_READY`.

**Acceptance criteria:**

1. After `RESET`, the modem sends `MODEM_READY`.
2. All counters (`tx_count`, `rx_count`, `tx_fail_count`) read zero in the next `STATUS`.
3. The channel reverts to 1.

---

### MD-0301  USB disconnection handling

**Priority:** Must
**Source:** modem-protocol.md §6.3

**Description:**
If the USB-CDC connection is lost, the modem firmware MUST continue running, discard any incoming ESP-NOW frames, and re-send `MODEM_READY` on reconnection.

**Acceptance criteria:**

1. Unplugging and re-plugging USB produces a new `MODEM_READY` on the re-opened port.
2. The modem does not crash or require a power cycle after USB disconnection.

---

### MD-0302  Watchdog timer

**Priority:** Should

**Description:**
The modem firmware SHOULD implement a watchdog timer (10 second timeout) and trigger a hardware reset if the main loop stalls.

**Acceptance criteria:**

1. A deliberate infinite loop in test firmware triggers a watchdog reset within 10 seconds.

---

### MD-0303  Status reporting

**Priority:** Must
**Source:** modem-protocol.md §4.6

**Description:**
The modem firmware MUST maintain `tx_count`, `rx_count`, `tx_fail_count`, and `uptime_s` counters, reported via `STATUS` in response to `GET_STATUS`.

**Acceptance criteria:**

1. Counters reset to zero on boot and on `RESET`.
2. `tx_count` increments on every `esp_now_send()` call.
3. `tx_fail_count` increments on every failed send callback.
4. `rx_count` increments on every `RECV_FRAME` forwarded to USB.
5. `uptime_s` reflects seconds since last boot or `RESET`.

---

## 6  Non-requirements

The modem firmware:

- MUST NOT perform HMAC verification or any cryptographic operation.
- MUST NOT parse CBOR payloads or interpret frame contents.
- MUST NOT maintain sessions, node state, or protocol-level logic.
- MUST NOT perform over-the-air updates (firmware is flashed via USB/esptool).

---

## Appendix A  Requirement index

| ID | Title | Priority |
|----|-------|----------|
| MD-0100 | USB-CDC device presentation | Must |
| MD-0101 | Serial framing compliance | Must |
| MD-0102 | Maximum frame size | Must |
| MD-0103 | Unknown message types | Must |
| MD-0104 | Ready notification timing | Must |
| MD-0200 | ESP-NOW initialization | Must |
| MD-0201 | Frame forwarding (radio → USB) | Must |
| MD-0202 | Frame transmission (USB → radio) | Must |
| MD-0203 | Automatic peer registration | Must |
| MD-0204 | Peer table eviction | Should |
| MD-0205 | Frame ordering | Must |
| MD-0206 | Channel change | Must |
| MD-0207 | Channel scanning | Must |
| MD-0300 | Reset command | Must |
| MD-0301 | USB disconnection handling | Must |
| MD-0302 | Watchdog timer | Should |
| MD-0303 | Status reporting | Must |
