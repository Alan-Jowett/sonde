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

The system consists of three components connected in series: the Gateway (running on the host) communicates with the ESP32-S3 Radio Modem over a USB-CDC serial link, and the Radio Modem communicates wirelessly with Sensor Nodes (e.g., ESP32-C3) using ESP-NOW radio.

```
┌──────────┐   USB-CDC   ┌──────────────┐  ESP-NOW  ┌──────────────┐
│  Gateway │◄───────────►│  ESP32-S3    │◄ ─ ─ ─ ─ ►│  Sensor Node │
│  (host)  │   serial    │  Radio Modem │   radio   │  (ESP32-C3)  │
└──────────┘             └──────────────┘           └──────────────┘
```

The modem is **protocol-unaware**: it does not perform cryptographic verification, CBOR parsing, session management, or any cryptographic operation. It relays opaque byte frames between USB and radio, adding only the peer MAC address and RSSI metadata.

### 1.1  Design principles

- **Simplicity** — length-prefixed framing on a reliable byte stream. No byte stuffing, no escaping.
- **Transparency** — the modem does not interpret frame contents; the gateway controls all protocol logic.
- **Testability** — the protocol works identically over USB-CDC, a Linux PTY pair, or a TCP socket, enabling hardware-free end-to-end testing.
- **Mostly fire-and-forget sends** — ordinary modem commands remain request/response or fire-and-forget as documented below. Large display transfers use an explicit chunk ACK / retransmit subprotocol because the USB-CDC link must be treated as lossy for long writes.

---

## 2  Serial framing

USB-CDC provides reliable, ordered byte delivery. The protocol uses simple length-prefixed framing with no byte stuffing.

### 2.1  Frame envelope

Every serial frame starts with a 2-byte big-endian `LEN` field that gives the combined length of the TYPE byte and BODY in bytes (so the minimum value is 1 and the maximum is 1025). The `LEN` field itself is not included in the count. Following `LEN` is a single `TYPE` byte (the message type discriminator), then the variable-length `BODY` (0 to 1024 bytes).

```
┌───────────┬──────────┬──────────────────────────────┐
│  LEN (2B) │ TYPE (1B)│  BODY (0 .. N bytes)         │
│  BE u16   │          │                              │
└───────────┴──────────┴──────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `len` | Unsigned integer | 2 bytes, big-endian | Length of TYPE + BODY in bytes (does not include the LEN field itself). Minimum value: 1. Maximum value: 1025. |
| `type` | Unsigned integer | 1 byte | Message type discriminator (see §3). |
| `body` | Bytes | 0 .. 1024 bytes | Type-specific payload. |

The maximum body size of 1024 bytes is retained so protocol extensions can carry large opaque payloads, but normal display updates now use the reliable chunked transfer defined in §4.7a–§4.7c instead of a single 1024-byte serial command.

### 2.2  Receiver behavior

- Frames with `len` = 0 MUST be silently discarded.
- Frames with `len` > 1025 MUST be treated as a framing error. The receiver MUST NOT skip `len` bytes (the value is untrusted and could be up to 65,535). Instead, the receiver MUST initiate the `RESET`-based resynchronization procedure (§2.3).
- Frames with an unknown `type` value MUST be silently discarded (forward compatibility).

### 2.3  Synchronization recovery

If the gateway opens a serial port mid-stream (e.g., after a modem reset or hot-plug), it may land in the middle of a frame. Recovery procedure:

1. Gateway sends `RESET` (TYPE = 0x01, empty body).
2. Modem firmware resets its receive-side framing parser on any `RESET` command and then sends a `MODEM_READY` frame (`LEN` = 0x00 0x0B, `TYPE` = 0x81, 10-byte body).
3. After sending `RESET`, the gateway MUST discard received bytes until it observes a valid `MODEM_READY` frame: a two-byte big-endian length of 11 (`0x00 0x0B`), followed by `TYPE` = `0x81`, followed by exactly 10 bytes of body (`firmware_version` + `mac_address`). This deterministic framing pattern allows the gateway to resynchronize even if the `RESET` was sent mid-frame.

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
| 0x09 | `DISPLAY_FRAME_BEGIN` | §4.7a | Begin a reliable framebuffer transfer to the attached OLED. |
| 0x0A | `DISPLAY_FRAME_CHUNK` | §4.7b | Send one acknowledged chunk of a reliable framebuffer transfer. |
| 0x20 | `BLE_INDICATE` | §4.9 | Send a BLE indication to the connected phone (gateway response wrapped in BLE envelope). |
| 0x21 | `BLE_ENABLE` | §4.13 | Enable BLE advertising and accept connections for the Gateway Pairing Service. |
| 0x22 | `BLE_DISABLE` | §4.14 | Disable BLE advertising and disconnect any active BLE client. |
| 0x23 | `BLE_PAIRING_CONFIRM_REPLY` | §4.16 | Accept or reject the BLE Numeric Comparison pairing. |

### 3.2  Modem → Gateway

| Type | Name | Body | Description |
|------|------|------|-------------|
| 0x81 | `MODEM_READY` | §4.3 | Modem is initialized and ready. Sent on boot and after `RESET`. |
| 0x82 | `RECV_FRAME` | §4.4 | An inbound ESP-NOW frame was received from a node. |
| 0x84 | `SET_CHANNEL_ACK` | §4.5 | Confirms a channel change. |
| 0x85 | `STATUS` | §4.6 | Modem status and counters (response to `GET_STATUS`). |
| 0x86 | `SCAN_RESULT` | §4.7 | Per-channel AP survey results (response to `SCAN_CHANNELS`). |
| 0x87 | `DISPLAY_FRAME_ACK` | §4.7c | Acknowledges progress on a reliable display transfer. |
| 0x89 | `EVENT_ERROR` | §4.8a | Recoverable display-path error notification. |
| 0x8F | `ERROR` | §4.8 | Unrecoverable modem error. |
| 0xA0 | `BLE_RECV` | §4.10 | A BLE GATT write was received from the connected phone. |
| 0xA1 | `BLE_CONNECTED` | §4.11 | A BLE client connected to the Gateway Pairing Service. |
| 0xA2 | `BLE_DISCONNECTED` | §4.12 | The BLE client disconnected. |
| 0xA3 | `BLE_PAIRING_CONFIRM` | §4.15 | Numeric Comparison pin display request — gateway must show the pin to the operator. |
| 0xB0 | `EVENT_BUTTON` | §4.17 | A debounced button press was detected on the 1-Wire data line. |

---

## 4  Message definitions

All multi-byte integers are big-endian unless otherwise stated.

### 4.1  SEND_FRAME (Gateway → Modem)

Transmit `frame_data` to the specified peer via ESP-NOW. The modem auto-registers unknown peer MACs transparently. This is a fire-and-forget operation — no per-frame response is sent. Delivery failures are counted in `tx_fail_count` (see §4.6).

The `SEND_FRAME` body consists of two fields concatenated: a 6-byte `peer_mac` destination address followed by the variable-length opaque `frame_data` (1 to 250 bytes).

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

The `MODEM_READY` body contains two fields: a 4-byte big-endian `firmware_version` (encoded as major.minor.patch.build, one byte each, derived from the crate's `CARGO_PKG_VERSION` at compile time) followed by the 6-byte `mac_address` (the modem's own WiFi MAC).

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

The `RECV_FRAME` body contains three fields: a 6-byte `peer_mac` (source MAC address), a 1-byte signed `rssi` (received signal strength in dBm), and the variable-length `frame_data` (1 to 250 bytes) — the exact bytes received over the air, unmodified.

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

The `STATUS` body contains five consecutive big-endian fields: `channel` (1 byte, current WiFi channel), `uptime_s` (4 bytes, seconds since boot or RESET), `tx_count` (4 bytes, total frames transmitted), `rx_count` (4 bytes, total frames received and forwarded), and `tx_fail_count` (4 bytes, MAC-layer send failures). Total body size: 17 bytes.

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

The `SCAN_RESULT` body starts with a 1-byte `count` field (number of channel entries, typically 14) followed by `count` entries of 3 bytes each. Each entry holds three 1-byte fields: `channel` (WiFi channel number), `ap_count` (number of APs detected on that channel, capped at 255), and `strongest_rssi` (signed dBm RSSI of the strongest AP; 0 if none detected).

```
┌────────────┬─────────────────────────────────────────────────────┐
│ count (1B) │ entries: (channel[1] | ap_count[1] | strongest_rssi[1]) × N  │
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

### 4.7a  DISPLAY_FRAME_BEGIN (Gateway → Modem)

Begin a **reliable** display transfer for a complete 128×64 monochrome framebuffer. The modem renders only complete frames; partial updates are not supported. The gateway MUST send `DISPLAY_FRAME_BEGIN`, wait for `DISPLAY_FRAME_ACK(next_chunk_index = 0)`, then send exactly eight `DISPLAY_FRAME_CHUNK` messages (§4.7b), waiting for an ACK after each chunk.

The `DISPLAY_FRAME_BEGIN` body contains a single 1-byte `transfer_id` chosen by the gateway. The `transfer_id` scopes ACKs and retries for one display update attempt.

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `transfer_id` | Unsigned integer | 1 byte | Gateway-chosen identifier for this display transfer attempt. |

If a new `DISPLAY_FRAME_BEGIN` arrives while an earlier display transfer is incomplete, the modem aborts the old transfer state and starts the new one.

### 4.7b  DISPLAY_FRAME_CHUNK (Gateway → Modem)

Send one chunk of the reliable display transfer started by `DISPLAY_FRAME_BEGIN`.

Each framebuffer is exactly 1024 bytes representing a 128×64 monochrome framebuffer in **row-major** order. Rows are transmitted top-to-bottom. Within each row, bytes progress left-to-right, and within each byte the most-significant bit corresponds to the leftmost pixel of the 8-pixel group.

The transfer is split into exactly **8 chunks of 128 bytes each**. The gateway MUST send chunk indices in ascending order from 0 through 7. After each accepted chunk, the modem sends `DISPLAY_FRAME_ACK` with the next expected chunk index.

The `DISPLAY_FRAME_CHUNK` body contains three fields:

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `transfer_id` | Unsigned integer | 1 byte | Must match the active transfer begun by `DISPLAY_FRAME_BEGIN`. |
| `chunk_index` | Unsigned integer | 1 byte | Zero-based chunk index, valid range `0..=7`. |
| `chunk_data` | Bytes | 128 bytes | Raw framebuffer bytes for this chunk. |

```text
framebuffer_len = 1024 bytes
chunk 0: bytes 0..127
chunk 1: bytes 128..255
...
chunk 7: bytes 896..1023
```

Pixel mapping for byte `framebuffer[(y * 16) + (x / 8)]` after reassembly:

| Bit | Pixel |
|-----|-------|
| 7 (`0x80`) | `x + 0` |
| 6 (`0x40`) | `x + 1` |
| 5 (`0x20`) | `x + 2` |
| 4 (`0x10`) | `x + 3` |
| 3 (`0x08`) | `x + 4` |
| 2 (`0x04`) | `x + 5` |
| 1 (`0x02`) | `x + 6` |
| 0 (`0x01`) | `x + 7` |

The modem MUST NOT interpret framebuffer content semantically. It treats the payload as opaque pixel data supplied by the gateway.

If the modem receives a duplicate or out-of-order `DISPLAY_FRAME_CHUNK` for the active `transfer_id`, it MUST NOT advance transfer state or emit `EVENT_ERROR`. Instead, it re-sends `DISPLAY_FRAME_ACK` carrying the current `next_chunk_index`, allowing the gateway to retry safely after a lost ACK.

### 4.7c  DISPLAY_FRAME_ACK (Modem → Gateway)

Acknowledges progress on a reliable display transfer.

The `DISPLAY_FRAME_ACK` body contains two fields:

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `transfer_id` | Unsigned integer | 1 byte | The active transfer identifier being acknowledged. |
| `next_chunk_index` | Unsigned integer | 1 byte | The next chunk index the modem expects. `0` acknowledges `DISPLAY_FRAME_BEGIN`; `8` acknowledges a complete transfer. |

When the modem sends `DISPLAY_FRAME_ACK(transfer_id, 8)`, it has accepted the full 1024-byte framebuffer and queued it for OLED rendering.

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

### 4.8a  EVENT_ERROR (Modem → Gateway)

Reports a **recoverable** display-path error. Unlike `ERROR` (§4.8), `EVENT_ERROR` does not imply that the modem must be reset or that unrelated radio/BLE/USB functionality has failed.

The `EVENT_ERROR` body is a single 1-byte `error_code` field.

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `error_code` | Unsigned integer | 1 byte | Recoverable display error code (see table below). |

| Error code | Name | Description |
|------------|------|-------------|
| 0x01 | `INVALID_FRAME` | Reliable display-transfer metadata was malformed (for example, wrong begin/chunk body length or an invalid chunk index). |
| 0x02 | `DISPLAY_WRITE_FAILED` | The modem failed to write the accepted framebuffer to the OLED over I²C. |

### 4.9  BLE_INDICATE (Gateway → Modem)

Gateway sends a BLE indication payload to the connected phone via the Gateway Command characteristic. The modem handles indication fragmentation per ATT MTU (see ble-pairing-protocol.md §3.4). This is a fire-and-forget operation — no per-message response is sent. If no BLE client is connected, the modem silently discards the message.

The `BLE_INDICATE` body is a single variable-length field: the opaque `ble_data` (1 to 511 bytes) to relay to the BLE client.

```
┌──────────────────────────────────┐
│  ble_data (N bytes)              │
└──────────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `ble_data` | Bytes | 1 .. 511 bytes | Opaque payload relayed to the BLE client. Typically a BLE message envelope (TYPE + LEN + BODY per ble-pairing-protocol.md §4), but the modem does not inspect or validate the contents. A `BLE_INDICATE` serial frame whose BODY length is 0 (i.e., the serial envelope contains only the type byte and no `ble_data`) is invalid and MUST be silently discarded by the modem. |

### 4.10  BLE_RECV (Modem → Gateway)

A BLE GATT write was received on the Gateway Command characteristic from the connected phone. The modem forwards the complete reassembled write payload (after Write Long reassembly if applicable). Empty GATT writes (zero payload bytes) MUST be silently discarded — no `BLE_RECV` is sent.

The `BLE_RECV` body is a single variable-length field: the opaque `ble_data` (1 to 511 bytes) received from the BLE client.

```
┌──────────────────────────────────┐
│  ble_data (N bytes)              │
└──────────────────────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `ble_data` | Bytes | 1 .. 511 bytes | Opaque payload received from the BLE client. Typically a BLE message envelope (TYPE + LEN + BODY per ble-pairing-protocol.md §4), but the modem does not inspect or validate the contents. |

### 4.11  BLE_CONNECTED (Modem → Gateway)

A BLE client connected to the Gateway Pairing Service and completed LESC pairing. Sent after MTU negotiation and LESC pairing succeed.

The `BLE_CONNECTED` body contains two fields: the 6-byte `peer_addr` (BLE address of the connected phone) followed by a 2-byte big-endian `mtu` (negotiated ATT MTU, always ≥ 247).

```
┌──────────────────┬────────────┐
│  peer_addr (6B)  │  mtu (2B)  │
└──────────────────┴────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `peer_addr` | Bytes | 6 bytes | BLE address of the connected phone. |
| `mtu` | Unsigned integer | 2 bytes, big-endian | Negotiated ATT MTU. Always ≥ 247. |

### 4.12  BLE_DISCONNECTED (Modem → Gateway)

The BLE client disconnected from the Gateway Pairing Service.

The `BLE_DISCONNECTED` body contains two fields: the 6-byte `peer_addr` (BLE address of the disconnected phone) followed by a 1-byte `reason` (BLE HCI disconnect reason code).

```
┌──────────────────┬──────────────┐
│  peer_addr (6B)  │  reason (1B) │
└──────────────────┴──────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `peer_addr` | Bytes | 6 bytes | BLE address of the disconnected phone. |
| `reason` | Unsigned integer | 1 byte | BLE HCI disconnect reason code. |

### 4.13  BLE_ENABLE (Gateway → Modem)

Enable BLE advertising for the Gateway Pairing Service. The modem starts advertising and accepts incoming BLE connections. BLE advertising is OFF by default after boot/RESET — the gateway must send `BLE_ENABLE` before a phone can discover the modem. If already enabled, this is a no-op.

Body: (empty — no fields)

### 4.14  BLE_DISABLE (Gateway → Modem)

Disable BLE advertising and disconnect any active BLE client. If a BLE client is connected, the modem disconnects it (triggering a `BLE_DISCONNECTED` event) before stopping advertising. If already disabled, this is a no-op.

Body: (empty — no fields)

### 4.15  BLE_PAIRING_CONFIRM (Modem → Gateway)

During BLE LESC Numeric Comparison pairing, the modem sends this message to the gateway with the 6-digit pin that should be displayed to the operator. The gateway (or admin CLI) shows the pin; the operator verifies it matches the phone's display. The gateway responds with `BLE_PAIRING_CONFIRM_REPLY` (§4.16) to accept or reject the pairing.

The `BLE_PAIRING_CONFIRM` body is a single 4-byte big-endian `passkey` field containing a value from 0 to 999999. The gateway MUST display it zero-padded to 6 digits.

```
┌──────────────────┐
│  passkey (4B)    │
└──────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `passkey` | Unsigned integer | 4 bytes, big-endian u32 | 6-digit Numeric Comparison passkey (0–999999). Display as zero-padded 6 digits. |

### 4.16  BLE_PAIRING_CONFIRM_REPLY (Gateway → Modem)

Gateway's response to a `BLE_PAIRING_CONFIRM` — accepts or rejects the Numeric Comparison pairing.

The `BLE_PAIRING_CONFIRM_REPLY` body is a single 1-byte `accept` field: `0x01` means the operator confirmed the passkeys match (accept pairing); `0x00` means the operator rejected or the confirmation timed out (reject pairing).

```
┌──────────────┐
│  accept (1B) │
└──────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `accept` | Unsigned integer | 1 byte | `0x01` = accept (operator confirmed pin matches), `0x00` = reject (operator rejected or timeout). |

### 4.17  EVENT_BUTTON (Modem → Gateway)

A debounced button press was detected on the 1-Wire data line (GPIO2 / XIAO D1). The modem classifies presses by duration and emits this event on button release. The modem does not interpret button meaning — all semantics (pairing, menus, UX) are handled by the gateway.

The `EVENT_BUTTON` body is a single 1-byte `button_type` field.

```
┌──────────────────┐
│  button_type (1B)│
└──────────────────┘
```

| Field | Type | Size | Description |
|-------|------|------|-------------|
| `button_type` | Unsigned integer | 1 byte | `0x00` = BUTTON_SHORT (press < 1 s), `0x01` = BUTTON_LONG (press ≥ 1 s). |

---

## 5  Message flows

### 5.1  Startup

The gateway MUST always send `RESET` when opening the serial port, regardless of whether the modem was just powered on or was already running. This ensures deterministic state.

The startup sequence is: (1) gateway opens the serial port and immediately sends `RESET`; (2) the modem performs initialization and sends `MODEM_READY` (containing its firmware version and MAC address); (3) the gateway sends `SET_CHANNEL` with the desired WiFi channel; (4) the modem applies the channel and responds with `SET_CHANNEL_ACK`; then normal operation begins.

```
Gateway                          Modem
   │                               │
   │──── [open serial port] ──────►│
   │──── RESET ───────────────────►│
   │                               │
   │◄──── MODEM_READY ─────────────│
   │                               │
   │──── SET_CHANNEL(ch) ─────────►│
   │◄──── SET_CHANNEL_ACK(ch) ─────│
   │                               │
   │  ════ normal operation ════   │
```

Any `MODEM_READY` received before `RESET` completes (e.g., from USB enumeration) is discarded. The gateway only accepts `MODEM_READY` after it has sent `RESET`.

**BLE advertising** is OFF after boot and after `RESET`. The gateway must send `BLE_ENABLE` (§4.13) to start BLE advertising before a phone can discover the modem.

### 5.2  Normal operation (frame relay)

During normal operation, two independent flows run concurrently:

**Inbound (radio → gateway):** The modem sends `RECV_FRAME` whenever an ESP-NOW frame arrives. These are asynchronous — they can arrive at any time, interleaved with responses to gateway commands.

The inbound flow shows the modem pushing unsolicited `RECV_FRAME` messages to the gateway as ESP-NOW frames arrive from nodes, with the gateway sending `SEND_FRAME` responses back through the modem.

```
Gateway                          Modem
   │                               │
   │◄──── RECV_FRAME ──────────────│  (node sent a WAKE)
   │                               │
   │──── SEND_FRAME ──────────────►│  (gateway sends COMMAND)
   │                               │
   │◄──── RECV_FRAME ──────────────│  (node sent GET_CHUNK)
   │──── SEND_FRAME ──────────────►│  (gateway sends CHUNK)
   │          ⋮                    │
```

**Outbound (gateway → radio):** The gateway sends `SEND_FRAME` which the modem transmits immediately. No per-frame response is expected.

### 5.3  Health check

In the health-check flow, the gateway sends `GET_STATUS` and the modem responds synchronously with a `STATUS` message containing current counters (channel, uptime, transmit count, receive count, and failure count).

```
Gateway                          Modem
   │                               │
   │──── GET_STATUS ──────────────►│
   │◄──── STATUS ──────────────────│
   │                               │
```

The gateway polls periodically (recommended: every 30 seconds). A rising `tx_fail_count` indicates radio delivery problems.

### 5.4  Channel survey

In the channel-survey flow, the gateway sends `SCAN_CHANNELS` and the modem performs a WiFi AP scan (interrupting ESP-NOW reception for approximately 2–3 seconds), then sends a `SCAN_RESULT` with per-channel AP counts and RSSI values.

```
Gateway                          Modem
   │                               │
   │──── SCAN_CHANNELS ───────────►│
   │        (radio scanning        │
   │         ~2–3 seconds)         │
   │◄──── SCAN_RESULT ─────────────│
   │                               │
```

ESP-NOW reception is interrupted during the scan. This flow is only used during setup or maintenance.

### 5.5  Error recovery

When the modem encounters an unrecoverable error it sends an `ERROR` message (with error code and human-readable description) to the gateway. The gateway logs the error and responds by sending `RESET`, which causes the modem to reinitialize and send a fresh `MODEM_READY`. The gateway then re-establishes the channel with `SET_CHANNEL` / `SET_CHANNEL_ACK`.

```
Gateway                          Modem
   │                               │
   │◄──── ERROR(code, msg) ────────│
   │                               │
   │──── RESET ───────────────────►│
   │◄──── MODEM_READY ─────────────│
   │──── SET_CHANNEL(ch) ─────────►│
   │◄──── SET_CHANNEL_ACK(ch) ─────│
   │                               │
```

On `ERROR`, the gateway logs the error and sends `RESET` to attempt recovery.

### 5.6  BLE pairing relay

When a phone connects via BLE for pairing, the modem relays GATT messages between the phone and the gateway. The gateway must first enable BLE advertising via `BLE_ENABLE`:

The BLE pairing relay involves three parties: the Gateway, the Modem, and the Phone. The sequence is: (1) gateway sends `BLE_ENABLE` to start advertising; (2) phone discovers and connects via BLE; (3) LESC Numeric Comparison pairing occurs — the modem sends the 6-digit passkey to the gateway via `BLE_PAIRING_CONFIRM`, the operator confirms it matches the phone display, and the gateway replies with `BLE_PAIRING_CONFIRM_REPLY` (accept); (4) modem sends `BLE_CONNECTED`; (5) the phone sends GATT writes (relayed as `BLE_RECV`) and the gateway sends GATT indications back via `BLE_INDICATE`; (6) after the phone disconnects, the modem sends `BLE_DISCONNECTED` and the gateway disables BLE advertising with `BLE_DISABLE`.

```
Gateway                          Modem                            Phone
   │                               │                               │
   │──── BLE_ENABLE ──────────────►│                               │
   │                               │── start advertising ─────────►│
   │                               │                               │
   │                               │◄──── BLE connect ─────────────│
   │                               │                               │
   │                               │◄──── LESC Numeric Comparison ─│
   │◄──── BLE_PAIRING_CONFIRM ────│  (passkey = 123456)           │
   │                               │                               │
   │  (operator verifies pin)      │                               │
   │──── BLE_PAIRING_CONFIRM_REPLY►│  (accept = 0x01)             │
   │                               │──── pairing complete ────────►│
   │                               │                               │
   │◄──── BLE_CONNECTED ──────────│                               │
   │                               │                               │
   │                               │◄──── GATT write ──────────────│
   │◄──── BLE_RECV ───────────────│  (REQUEST_GW_INFO)            │
   │                               │                               │
   │──── BLE_INDICATE ────────────►│                               │
   │                               │──── GATT indication ─────────►│  (GW_INFO_RESPONSE)
   │                               │                               │
   │                               │◄──── GATT write ──────────────│
   │◄──── BLE_RECV ───────────────│  (REGISTER_PHONE)            │
   │                               │                               │
   │──── BLE_INDICATE ────────────►│                               │
   │                               │──── GATT indication ─────────►│  (PHONE_REGISTERED)
   │                               │                               │
   │                               │◄──── BLE disconnect ──────────│
   │◄──── BLE_DISCONNECTED ────────│                               │
   │                               │                               │
   │──── BLE_DISABLE ─────────────►│                               │
   │                               │── stop advertising           │
```

BLE pairing relay operates concurrently with ESP-NOW frame relay (§5.2). The modem MUST NOT block ESP-NOW operations while a BLE client is connected.

### 5.7  Display update

Display updates are gateway-driven and independent of radio/BLE traffic.

```text
Gateway                          Modem
   │                               │
   │──── DISPLAY_FRAME_BEGIN ─────►│
   │◄──── DISPLAY_FRAME_ACK(0) ────│
   │──── DISPLAY_FRAME_CHUNK(0) ──►│
   │◄──── DISPLAY_FRAME_ACK(1) ────│
   │            ...                │
   │──── DISPLAY_FRAME_CHUNK(7) ──►│
   │◄──── DISPLAY_FRAME_ACK(8) ────│
   │                               │── render full framebuffer to OLED
   │                               │
   │◄──── EVENT_ERROR? ────────────│  (only on INVALID_FRAME or DISPLAY_WRITE_FAILED)
```

---

## 6  Error handling

### 6.1  Invalid frames

| Condition | Receiver behavior |
|-----------|-------------------|
| `len` = 0 | Silently discard. |
| `len` > 1025 | Framing error. MUST NOT skip `len` bytes (untrusted value). Trigger `RESET`-based resynchronization (§2.3). |
| Unknown `type` | Silently discard (forward compatibility). |
| `SEND_FRAME` body < 7 bytes (no MAC + data) | Modem silently discards. |
| `SET_CHANNEL` with `channel` = 0 or > 14 | Modem sends `ERROR` with code `CHANNEL_SET_FAILED`. |
| `DISPLAY_FRAME_BEGIN` body length != 1 | Modem sends `EVENT_ERROR(INVALID_FRAME)` and leaves the current display contents unchanged. |
| `DISPLAY_FRAME_CHUNK` body length != 130 | Modem sends `EVENT_ERROR(INVALID_FRAME)` and aborts the active display transfer. |
| `DISPLAY_FRAME_CHUNK` transfer metadata invalid (unexpected transfer_id before any begin, chunk_index > 7) | Modem sends `EVENT_ERROR(INVALID_FRAME)` and aborts the active display transfer. |

### 6.2  Missing responses

The gateway expects responses for request-response commands. If a response is not received within the timeout (§7), the gateway should:

1. Log the timeout.
2. Send `RESET` and re-run the startup sequence (§5.1).

`SEND_FRAME` is fire-and-forget and has no expected response — it cannot time out. `DISPLAY_FRAME_BEGIN` and each `DISPLAY_FRAME_CHUNK` MUST be acknowledged by `DISPLAY_FRAME_ACK`.

### 6.3  USB disconnection

If the USB-CDC link drops:

- **Modem side:** Continues running, discards inbound ESP-NOW frames, re-sends `MODEM_READY` on reconnection.
- **Gateway side:** Detects the serial port closure, logs the event, and re-opens the port when available. On reconnection, sends `RESET` and re-runs startup (§5.1).

### 6.4  Unsolicited messages

The modem may send `RECV_FRAME`, `EVENT_ERROR`, `ERROR`, or `EVENT_BUTTON` at any time, interleaved with responses to gateway commands. The gateway's serial reader must demultiplex:

- `RECV_FRAME` → deliver to the `Transport::recv()` caller.
- `EVENT_ERROR` → log the recoverable display fault and continue operating.
- `ERROR` → log and optionally trigger recovery.
- `EVENT_BUTTON` → deliver to the button-event handler (e.g., registration window activation). **Note:** gateway-side consumption of `EVENT_BUTTON` is not yet implemented; the message is currently logged and otherwise ignored.
- Expected response (e.g., `STATUS`, `SET_CHANNEL_ACK`, `DISPLAY_FRAME_ACK`) → deliver to the waiting command.

---

## 7  Timing

| Event | Timeout | Action on timeout |
|-------|---------|-------------------|
| `MODEM_READY` after `RESET` or port open | 5 seconds | Log error, retry `RESET` (up to 3 attempts), then fail. |
| `SET_CHANNEL_ACK` after `SET_CHANNEL` | 2 seconds | Log error, send `RESET`, re-run startup. |
| `STATUS` after `GET_STATUS` | 2 seconds | Log warning, skip this poll cycle. |
| `SCAN_RESULT` after `SCAN_CHANNELS` | 10 seconds | Log error (scan may have failed). |
| `DISPLAY_FRAME_ACK` after `DISPLAY_FRAME_BEGIN` or `DISPLAY_FRAME_CHUNK` | 500 ms | Retransmit the same begin/chunk up to the configured retry budget; on retry exhaustion, treat the modem transport as failed and re-run startup. |

`SEND_FRAME` and `RECV_FRAME` remain asynchronous fire-and-forget / event traffic. Reliable display transfer is the only command family with per-chunk retransmission semantics.

The gateway does not retry individual commands (other than `RESET`). If a command fails, the recovery path is always: `RESET` → `MODEM_READY` → `SET_CHANNEL` → resume.

---

## 8  Protocol evolution

### 8.1  Forward compatibility

Both sides MUST silently discard frames with unrecognized `type` values. This prevents parser breakage when one side is newer than the other.

However, the reliable display-transfer subprotocol (`DISPLAY_FRAME_BEGIN`, `DISPLAY_FRAME_CHUNK`, `DISPLAY_FRAME_ACK`) requires **matched gateway and modem support**. An older modem will silently discard the new display-transfer commands, and a newer gateway will treat the missing ACKs as a transport failure and reconnect. Deploy this feature only when both sides have been updated together.

### 8.2  Version detection

The `firmware_version` field in `MODEM_READY` allows the gateway to detect the modem firmware version and adjust behavior if needed (e.g., skip `SCAN_CHANNELS` if the modem predates that feature).

### 8.3  Reserved type ranges

| Range | Purpose |
|-------|---------|
| 0x01 – 0x0F | Core modem commands (RESET, SEND_FRAME, SET_CHANNEL, GET_STATUS, SCAN_CHANNELS, DISPLAY_FRAME_BEGIN, DISPLAY_FRAME_CHUNK) |
| 0x10 – 0x1F | Reserved |
| 0x20 – 0x2F | BLE relay commands (BLE_INDICATE, BLE_ENABLE, BLE_DISABLE, BLE_PAIRING_CONFIRM_REPLY) |
| 0x30 – 0x7F | Reserved for future gateway → modem commands |
| 0x81 – 0x8F | Core modem events/responses (`MODEM_READY`, `RECV_FRAME`, `SET_CHANNEL_ACK`, `STATUS`, `SCAN_RESULT`, `EVENT_ERROR`, `ERROR`) |
| 0x90 – 0x9F | Reserved |
| 0xA0 – 0xAF | BLE relay events (BLE_RECV, BLE_CONNECTED, BLE_DISCONNECTED, BLE_PAIRING_CONFIRM) |
| 0xB0 – 0xBF | GPIO / hardware events (EVENT_BUTTON) |
| 0xC0 – 0xFF | Reserved for future modem → gateway messages |
