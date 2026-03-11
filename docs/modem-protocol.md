<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Serial Protocol Specification

> **Document status:** Draft
> **Scope:** Wire-level protocol between the Sonde gateway and a USB-attached ESP-NOW radio modem.
> **Audience:** Implementers building the gateway transport adapter or the modem firmware.
> **Related:** [modem-requirements.md](modem-requirements.md), [protocol.md](protocol.md), [gateway-design.md](gateway-design.md)

---

## 1  Overview

The gateway runs on a host computer that has no ESP-NOW radio hardware. An ESP32-S3 connected via USB acts as a **radio modem** — a transparent bridge between the host and the ESP-NOW wireless network:

```
┌──────────┐   USB-CDC   ┌──────────────┐  ESP-NOW  ┌──────────────┐
│  Gateway │◄───────────►│  ESP32-S3    │◄ ─ ─ ─ ─ ►│  Sensor Node │
│  (host)  │   serial    │  Radio Modem │   radio   │  (ESP32-C3)  │
└──────────┘             └──────────────┘           └──────────────┘
```

The modem is **protocol-unaware**: it does not perform HMAC verification, CBOR parsing, session management, or any cryptographic operation. It relays opaque byte frames between USB and radio, adding only the peer MAC address and RSSI metadata.

### 1.1  Design principles

- **Simplicity** — length-prefixed framing on a reliable byte stream. No byte stuffing, no escaping.
- **Transparency** — the modem does not interpret frame contents; the gateway controls all protocol logic.
- **Testability** — the protocol works identically over USB-CDC, a Linux PTY pair, or a TCP socket, enabling hardware-free end-to-end testing.
- **Fire-and-forget sends** — the gateway does not wait for per-frame delivery acknowledgement. The modem tracks failure counters that the gateway polls periodically.

---

## 2  Serial framing

USB-CDC provides reliable, ordered byte delivery. The protocol uses simple length-prefixed framing with no byte stuffing.

### 2.1  Frame envelope

```
┌───────────┬──────────┬──────────────────────────────┐
│  LEN (2B) │ TYPE (1B)│  BODY (0 .. N bytes)         │
│  BE u16   │          │                              │
└───────────┴──────────┴──────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `len` | Unsigned integer | 2 bytes, big-endian | Length of TYPE + BODY in bytes (does not include the LEN field itself). Minimum value: 1. Maximum value: 512. |
| `type` | Unsigned integer | 1 byte | Message type discriminator (see §3). |
| `body` | Bytes | 0 .. 511 bytes | Type-specific payload. |

The maximum body size of 512 bytes provides generous headroom above the 250-byte ESP-NOW frame limit plus the 6-byte MAC address overhead.

### 2.2  Receiver behavior

- Frames with `len` = 0 MUST be silently discarded.
- Frames with `len` > 512 MUST be silently discarded. The receiver MUST skip `len` bytes to resynchronize.
- Frames with an unknown `type` value MUST be silently discarded (forward compatibility).

### 2.3  Synchronization recovery

If the gateway opens a serial port mid-stream (e.g., after a modem reset or hot-plug), it may land in the middle of a frame. Recovery procedure:

1. Gateway sends `RESET` (TYPE = 0x01, empty body).
2. Gateway discards all received bytes until it sees a valid `MODEM_READY` frame.
3. Modem firmware resets its receive-side framing parser on any `RESET` command, so even a partial frame followed by a valid `RESET` will resynchronize both sides.

---

## 3  Message types

Message types are partitioned by direction:

| Range | Direction |
|-------|-----------|
| 0x01 – 0x7F | Gateway → Modem (commands) |
| 0x81 – 0xFF | Modem → Gateway (events / responses) |

### 3.1  Gateway → Modem

| Type | Name | Body | Description |
|------|------|------|-------------|
| 0x01 | `RESET` | (empty) | Reset modem state, re-initialize ESP-NOW, clear peer table. Modem responds with `MODEM_READY`. |
| 0x02 | `SEND_FRAME` | §4.1 | Transmit an ESP-NOW frame to a specified peer. Fire-and-forget. |
| 0x03 | `SET_CHANNEL` | §4.2 | Set the WiFi/ESP-NOW channel. Modem responds with `SET_CHANNEL_ACK`. |
| 0x04 | `GET_STATUS` | (empty) | Query modem status and counters. Modem responds with `STATUS`. |
| 0x05 | `SCAN_CHANNELS` | (empty) | Perform a WiFi AP scan across all channels. Modem responds with `SCAN_RESULT`. |

### 3.2  Modem → Gateway

| Type | Name | Body | Description |
|------|------|------|-------------|
| 0x81 | `MODEM_READY` | §4.3 | Modem is initialized and ready. Sent on boot and after `RESET`. |
| 0x82 | `RECV_FRAME` | §4.4 | An inbound ESP-NOW frame was received from a node. |
| 0x84 | `SET_CHANNEL_ACK` | §4.5 | Confirms a channel change. |
| 0x85 | `STATUS` | §4.6 | Modem status and counters (response to `GET_STATUS`). |
| 0x86 | `SCAN_RESULT` | §4.7 | Per-channel AP survey results (response to `SCAN_CHANNELS`). |
| 0x8F | `ERROR` | §4.8 | Unrecoverable modem error. |

---

## 4  Message definitions

All multi-byte integers are big-endian unless otherwise stated.

### 4.1  SEND_FRAME (Gateway → Modem)

Transmit `frame_data` to the specified peer via ESP-NOW. The modem auto-registers unknown peer MACs transparently (see §5.3). This is a fire-and-forget operation — no per-frame response is sent. Delivery failures are counted in `tx_fail_count` (see §4.6).

```
┌──────────────────┬──────────────────────────────────┐
│  peer_mac (6B)   │  frame_data (N bytes)            │
└──────────────────┴──────────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `peer_mac` | Bytes | 6 bytes | Destination MAC address. `FF:FF:FF:FF:FF:FF` for broadcast. |
| `frame_data` | Bytes | 1 .. 250 bytes | Opaque frame to transmit. The modem does not inspect or modify this data. |

### 4.2  SET_CHANNEL (Gateway → Modem)

Set the WiFi channel used for ESP-NOW communication.

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `channel` | Unsigned integer | 1 byte | WiFi channel number (1 – 14). |

The modem MUST respond with `SET_CHANNEL_ACK` (§4.5) after the channel change takes effect. The modem clears its ESP-NOW peer table on channel change.

### 4.3  MODEM_READY (Modem → Gateway)

Sent when the modem has completed initialization and is ready to send and receive ESP-NOW frames. The gateway MUST wait for this message before sending any other commands.

```
┌────────────────────────┬──────────────────┐
│  firmware_version (4B) │  mac_address (6B)│
└────────────────────────┴──────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `firmware_version` | Unsigned integer | 4 bytes, big-endian | Modem firmware version (semantic: major.minor.patch.build, one byte each). |
| `mac_address` | Bytes | 6 bytes | The modem's own WiFi MAC address (the source address for all transmitted ESP-NOW frames). |

This message is sent:
- On power-up / USB enumeration (within 2 seconds).
- After a `RESET` command.
- After USB-CDC reconnection (if the host disconnected and reconnected).

### 4.4  RECV_FRAME (Modem → Gateway)

An ESP-NOW frame was received from a remote peer.

```
┌──────────────────┬────────────┬──────────────────────────────────┐
│  peer_mac (6B)   │  rssi (1B) │  frame_data (N bytes)            │
└──────────────────┴────────────┴──────────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `peer_mac` | Bytes | 6 bytes | Source MAC address of the remote peer. |
| `rssi` | Signed integer | 1 byte (i8) | Received signal strength in dBm (typically −30 to −90). |
| `frame_data` | Bytes | 1 .. 250 bytes | The received frame, unmodified. |

The modem forwards **all** received ESP-NOW frames without filtering. The gateway is responsible for authentication and discarding unrecognized frames.

### 4.5  SET_CHANNEL_ACK (Modem → Gateway)

Confirms that the ESP-NOW channel has been changed.

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `channel` | Unsigned integer | 1 byte | The new active channel. |

### 4.6  STATUS (Modem → Gateway)

Response to `GET_STATUS`. Reports modem health and counters.

```
┌────────────┬───────────────┬──────────────┬──────────────┬───────────────────┐
│ channel(1B)│ uptime_s (4B) │ tx_count(4B) │ rx_count(4B) │ tx_fail_count(4B) │
└────────────┴───────────────┴──────────────┴──────────────┴───────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `channel` | Unsigned integer | 1 byte | Current WiFi channel. |
| `uptime_s` | Unsigned integer | 4 bytes, big-endian | Seconds since last boot or `RESET`. |
| `tx_count` | Unsigned integer | 4 bytes, big-endian | Total ESP-NOW frames transmitted (including failures). |
| `rx_count` | Unsigned integer | 4 bytes, big-endian | Total ESP-NOW frames received and forwarded to USB. |
| `tx_fail_count` | Unsigned integer | 4 bytes, big-endian | ESP-NOW send failures (MAC-layer delivery failed). |

Counters reset to zero on boot and on `RESET`. The gateway polls `GET_STATUS` periodically (e.g., every 30 seconds) to monitor modem health and detect send failures.

### 4.7  SCAN_RESULT (Modem → Gateway)

Response to `SCAN_CHANNELS`. Reports per-channel WiFi AP activity to help the administrator select the least congested channel.

```
┌────────────┬─────────────────────────────────────────────────────┐
│ count (1B) │ entries: (channel[1] | ap_count[1] | rssi[1]) × N  │
└────────────┴─────────────────────────────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `count` | Unsigned integer | 1 byte | Number of channel entries (typically 14 for channels 1–14). |
| `channel` | Unsigned integer | 1 byte | WiFi channel number. |
| `ap_count` | Unsigned integer | 1 byte | Number of APs detected on this channel (capped at 255). |
| `strongest_rssi` | Signed integer | 1 byte (i8) | RSSI of the strongest AP on this channel (dBm). 0 if no APs detected. |

Each entry is 3 bytes. For all 14 channels the total body is 1 + (14 × 3) = 43 bytes.

**Important:** Channel scanning temporarily takes over the WiFi radio (~2–3 seconds). Inbound ESP-NOW frames may be dropped during a scan. This command should only be used during initial setup or administrator-triggered maintenance, not during normal gateway operation.

### 4.8  ERROR (Modem → Gateway)

Reports an unrecoverable modem error. The gateway should log this and may attempt a `RESET`.

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `error_code` | Unsigned integer | 1 byte | Error category (see table below). |
| `message` | UTF-8 string | 0 .. N bytes | Human-readable error description. |

| Error code | Name | Description |
|------------|------|-------------|
| 0x01 | `ESPNOW_INIT_FAILED` | ESP-NOW initialization failed. |
| 0x02 | `WIFI_INIT_FAILED` | WiFi stack initialization failed. |
| 0x03 | `CHANNEL_SET_FAILED` | Failed to set the requested channel. |
| 0xFF | `UNKNOWN` | Unclassified error. |
