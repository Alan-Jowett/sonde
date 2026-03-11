<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Requirements Specification

> **Document status:** Draft
> **Scope:** Requirements for the ESP32-S3 radio modem firmware and the gateway transport adapter.
> **Audience:** Implementers building the modem firmware or the gateway USB transport.
> **Related:** [modem-protocol.md](modem-protocol.md), [gateway-requirements.md](gateway-requirements.md), [node-requirements.md](node-requirements.md)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Modem** | An ESP32-S3 device connected to the gateway host via USB, acting as a transparent ESP-NOW radio bridge. |
| **Gateway host** | The computer running the Sonde gateway service. It has no ESP-NOW radio hardware of its own. |
| **Modem serial protocol** | The length-prefixed framing protocol defined in [modem-protocol.md](modem-protocol.md). |
| **Peer table** | The ESP-NOW peer list maintained by the modem firmware (max ~20 entries on ESP-IDF). |
| **PTY** | A pseudo-terminal pair used to emulate a serial link for testing without hardware. |

---

## 2  Requirement format

Each requirement has:

- **ID** — `MD-XXYY` (Modem, section × 100 + sequence).
- **Priority** — Must / Should / May (RFC 2119).
- **Description** — what the implementation must do.
- **Acceptance criteria** — how to verify compliance.

---

## 3  Modem firmware — USB-CDC interface

### MD-0100  USB-CDC device presentation

**Priority:** Must

The modem firmware MUST present a USB-CDC ACM device when connected to a host via USB.

**Acceptance criteria:**
1. Host OS enumerates a virtual serial port (e.g., `/dev/ttyACM0` on Linux, `COMx` on Windows).
2. No special host-side driver is required beyond the standard CDC ACM class driver.

### MD-0101  Serial framing compliance

**Priority:** Must

The modem firmware MUST implement the length-prefixed framing protocol defined in [modem-protocol.md §2](modem-protocol.md#2--serial-framing).

**Acceptance criteria:**
1. All outbound messages use the `LEN || TYPE || BODY` envelope.
2. Inbound frames with `len` > 512 are discarded.
3. Inbound frames with unknown `type` values are discarded.

### MD-0102  Maximum frame size

**Priority:** Must

The modem firmware MUST accept serial frames up to 512 bytes total (`LEN` + `TYPE` + `BODY`).

### MD-0103  Unknown message types

**Priority:** Must

The modem firmware MUST silently discard serial frames with unrecognized `type` values. This ensures forward compatibility when the gateway uses a newer protocol version.

### MD-0104  Ready notification timing

**Priority:** Must

The modem firmware MUST send `MODEM_READY` within 2 seconds of USB enumeration.

**Acceptance criteria:**
1. A test harness that opens the serial port receives `MODEM_READY` within 2 seconds.

---

## 4  Modem firmware — ESP-NOW interface

### MD-0200  ESP-NOW initialization

**Priority:** Must

The modem firmware MUST initialize ESP-NOW in WiFi station mode on the configured channel (default: channel 1).

### MD-0201  Frame forwarding (radio → USB)

**Priority:** Must

The modem firmware MUST forward all received ESP-NOW frames to the gateway as `RECV_FRAME` messages, including the sender MAC address and RSSI.

**Acceptance criteria:**
1. Every ESP-NOW frame received by the modem produces exactly one `RECV_FRAME` message on USB.
2. `peer_mac` matches the sender's MAC address.
3. `rssi` reflects the received signal strength.
4. `frame_data` is identical to the received ESP-NOW payload.

### MD-0202  Frame transmission (USB → radio)

**Priority:** Must

On `SEND_FRAME`, the modem firmware MUST transmit the frame via ESP-NOW to the specified peer MAC address. Sends are fire-and-forget; delivery failures increment `tx_fail_count`.

**Acceptance criteria:**
1. `esp_now_send()` is called with the correct peer MAC and frame data.
2. `tx_count` is incremented on every send attempt.
3. `tx_fail_count` is incremented when the ESP-NOW send callback reports failure.

### MD-0203  Automatic peer registration

**Priority:** Must

The modem firmware MUST auto-register unknown peer MAC addresses in the ESP-NOW peer table before sending. This is transparent to the gateway.

### MD-0204  Peer table eviction

**Priority:** Should

When the ESP-NOW peer table is full (~20 entries), the modem firmware SHOULD evict the least-recently-used peer to make room for the new one.

### MD-0205  Frame ordering

**Priority:** Must

The modem firmware MUST NOT buffer, reorder, filter, or modify frame data. Frames MUST be forwarded in the order received, in both directions.

### MD-0206  Channel change

**Priority:** Must

The modem firmware MUST support channel changes via `SET_CHANNEL` without requiring a reboot. The modem MUST clear its ESP-NOW peer table on channel change and respond with `SET_CHANNEL_ACK`.

### MD-0207  Channel scanning

**Priority:** Must

On `SCAN_CHANNELS`, the modem firmware MUST perform a WiFi AP scan across all channels using `esp_wifi_scan_start()` and report per-channel AP count and strongest RSSI via `SCAN_RESULT`.

**Acceptance criteria:**
1. Scan covers channels 1–14.
2. Each entry in `SCAN_RESULT` contains `channel`, `ap_count`, and `strongest_rssi`.
3. Modem resumes normal ESP-NOW operation after scan completes.

---

## 5  Modem firmware — reliability and reset

### MD-0300  Reset command

**Priority:** Must

On `RESET`, the modem firmware MUST:
1. De-initialize ESP-NOW.
2. Clear the peer table.
3. Reset all counters (`tx_count`, `rx_count`, `tx_fail_count`, `uptime_s`).
4. Re-initialize ESP-NOW on channel 1.
5. Reset the receive-side framing parser.
6. Send `MODEM_READY`.

### MD-0301  USB disconnection handling

**Priority:** Must

If the USB-CDC connection is lost, the modem firmware MUST:
1. Continue running (do not reset or power down).
2. Discard any incoming ESP-NOW frames.
3. Re-send `MODEM_READY` on reconnection.

### MD-0302  Watchdog timer

**Priority:** Should

The modem firmware SHOULD implement a watchdog timer (10 second timeout) and trigger a hardware reset if the main loop stalls.

### MD-0303  Status reporting

**Priority:** Must

The modem firmware MUST maintain `tx_count`, `rx_count`, `tx_fail_count`, and `uptime_s` counters, reported via `STATUS` in response to `GET_STATUS`.

**Acceptance criteria:**
1. Counters reset to zero on boot and on `RESET`.
2. `tx_count` increments on every `esp_now_send()` call.
3. `tx_fail_count` increments on every failed send callback.
4. `rx_count` increments on every `RECV_FRAME` forwarded to USB.
5. `uptime_s` reflects seconds since last boot or `RESET`.

---

## 6  Modem firmware — non-requirements

The modem firmware:
- MUST NOT perform HMAC verification or any cryptographic operation.
- MUST NOT parse CBOR payloads or interpret frame contents.
- MUST NOT maintain sessions, node state, or protocol-level logic.
- MUST NOT perform over-the-air updates (firmware is flashed via USB/esptool).

---

## 7  Gateway transport adapter

### MD-0700  Transport trait implementation

**Priority:** Must

The gateway MUST implement a `UsbEspNowTransport` that satisfies the existing `Transport` trait using the modem serial protocol.

| Transport method | Behavior |
|------------------|----------|
| `recv()` | Read serial frames, filter for `RECV_FRAME`, return `(frame_data, peer_mac)`. Handle other message types (e.g., `STATUS`, `ERROR`) internally. |
| `send(frame, peer)` | Construct and write a `SEND_FRAME` message. Return immediately (fire-and-forget). |

### MD-0701  Startup sequence

**Priority:** Must

On startup, the gateway transport adapter MUST:
1. Open the serial port (device path from gateway configuration).
2. Send `RESET`.
3. Wait for `MODEM_READY` (with a configurable timeout, default 5 seconds).
4. Send `SET_CHANNEL` with the channel from gateway configuration.
5. Wait for `SET_CHANNEL_ACK`.
6. Begin normal operation.

**Acceptance criteria:**
1. If `MODEM_READY` is not received within the timeout, the adapter returns an error.
2. The modem's MAC address from `MODEM_READY` is logged.

### MD-0702  Periodic health monitoring

**Priority:** Should

The gateway transport adapter SHOULD poll `GET_STATUS` periodically (recommended: every 30 seconds) and log:
- `tx_fail_count` delta since last poll.
- `uptime_s` (to detect unexpected modem reboots).

### MD-0703  Modem error handling

**Priority:** Must

On receiving an `ERROR` message from the modem, the gateway MUST log the error code and message. The gateway MAY attempt a `RESET` to recover.

---

## 8  Testing

### MD-0800  PTY-based integration testing

**Priority:** Must

The gateway transport adapter MUST be testable over a PTY (pseudo-terminal) pair without physical modem hardware.

```
┌──────────┐   PTY master   ┌─────────────────┐
│  Gateway  │◄──────────────►│  MockModem       │
│  (under   │   PTY slave    │  (test harness)  │
│   test)   │                │                  │
└──────────┘                └─────────────────┘
```

**Acceptance criteria:**
1. A `MockModem` test harness can send `MODEM_READY`, inject `RECV_FRAME` messages, and consume `SEND_FRAME` messages.
2. Full gateway wake-cycle tests (WAKE → COMMAND → chunked transfer → PROGRAM_ACK) pass over the PTY transport.

---

## 9  Code sharing

### MD-0900  Serial protocol codec

**Priority:** Must

The modem serial protocol frame encoder/decoder and message type constants MUST be implemented in the `sonde-protocol` crate (new `modem` module). This code is `no_std` compatible and shared between the gateway and modem firmware.

### MD-0901  Shared ESP-NOW driver

**Priority:** Should

ESP-NOW initialization, send (with auto peer registration and LRU eviction), and receive callback registration SHOULD be shared between the `sonde-modem` and `sonde-node` crates to avoid duplicating ESP-IDF platform bindings.

The shared layer provides:
- WiFi + ESP-NOW initialization.
- Send with automatic peer registration and LRU eviction.
- Receive callback registration.

The modem extends this with channel scanning and USB bridging. The node extends it with wake-cycle integration and key storage.
