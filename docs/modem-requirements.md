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
2. Inbound frames with `len` > 1025 are treated as framing errors and trigger the `RESET`-based resynchronization procedure.
3. Inbound frames with unknown `type` values are silently discarded.

---

### MD-0102  Maximum frame size

**Priority:** Must
**Source:** modem-protocol.md §2.1

**Description:**
The modem firmware MUST accept serial frames where the `len` field (which covers `TYPE` + `BODY`) is up to 1025. This corresponds to a maximum on-wire frame size of 1027 bytes including the 2-byte `LEN` field.

**Acceptance criteria:**

1. A serial frame with `len` = 1025 (1027 bytes on wire including the 2-byte `LEN` field) is accepted and processed without error.
2. A serial frame with `len` > 1025 triggers the `RESET`-based resynchronization procedure.

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

### MD-0208  SEND_FRAME body validation

**Priority:** Must
**Source:** modem-protocol.md §6.1

**Description:**
The modem firmware MUST silently discard `SEND_FRAME` messages whose body is shorter than 7 bytes (6-byte peer MAC + at least 1 byte of frame data). No `ERROR` is sent; the modem continues processing subsequent frames normally.

**Acceptance criteria:**

1. A `SEND_FRAME` with fewer than 7 body bytes is silently discarded.
2. `tx_count` is not incremented for the discarded frame.
3. The modem remains operational after the discard.

---

### MD-0209  SET_CHANNEL error reporting

**Priority:** Must
**Source:** modem-protocol.md §6.1

**Description:**
The modem firmware MUST respond with `ERROR(CHANNEL_SET_FAILED)` when `SET_CHANNEL` specifies an invalid channel number (e.g., 0 or > 14). The current channel MUST remain unchanged.

**Acceptance criteria:**

1. `SET_CHANNEL(0)` produces `ERROR(CHANNEL_SET_FAILED)`.
2. `SET_CHANNEL(15)` produces `ERROR(CHANNEL_SET_FAILED)`.
3. The operating channel is unchanged after a failed `SET_CHANNEL`.

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
4. The peer table is empty after `RESET`.

---

### MD-0301  USB disconnection handling

**Priority:** Must
**Source:** modem-protocol.md §6.3

**Description:**
If the USB-CDC connection is lost, the modem firmware MUST continue running, discard any incoming ESP-NOW frames, and re-send `MODEM_READY` on reconnection.

**Acceptance criteria:**

1. Unplugging and re-plugging USB produces a new `MODEM_READY` on the re-opened port.
2. The modem does not crash or require a power cycle after USB disconnection.
3. ESP-NOW frames arriving during USB disconnection are silently discarded (not queued and flushed on reconnect).

> **Known limitation:** USB disconnection is detected reactively via I/O failure on the next `write()` or `is_connected()` check, not by a hardware interrupt. There may be a brief window between physical disconnection and detection during which a small number of ESP-NOW frames are buffered in the ESP-NOW receive ring before `Bridge::poll()` drains it. These frames are discarded on the next poll cycle once the disconnection is detected.

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

- MUST NOT perform cryptographic verification or any cryptographic operation.
- MUST NOT parse CBOR payloads or interpret frame contents.
- MUST NOT maintain sessions, node state, or protocol-level logic.
- MUST NOT perform over-the-air updates (firmware is flashed via USB/esptool).

---

## 7  BLE pairing relay

### MD-0400  Gateway Pairing Service

**Priority:** Must
**Source:** ble-pairing-protocol.md §3.1, §3.2

**Description:**
The modem MUST host the BLE GATT Gateway Pairing Service (UUID `0000FE60-0000-1000-8000-00805F9B34FB`) with a Gateway Command characteristic (UUID `0000FE61-0000-1000-8000-00805F9B34FB`, Write + Indicate). Writes received on this characteristic MUST be forwarded to the gateway over USB-CDC. Indications from the gateway MUST be forwarded to the connected BLE client.

**Acceptance criteria:**

1. The Gateway Pairing Service is discoverable via GATT service discovery.
2. The Gateway Command characteristic supports Write and Indicate properties.
3. A write to the characteristic produces a corresponding serial message on USB-CDC.
4. An indication sent from the gateway via USB-CDC is delivered to the BLE client.

---

### MD-0401  BLE ↔ USB-CDC message relay

**Priority:** Must
**Source:** ble-pairing-protocol.md §4

**Description:**
The modem MUST relay BLE pairing messages between the BLE GATT characteristic and the USB-CDC serial link transparently. The modem MUST NOT interpret, validate, or modify the BLE message envelope contents — it is an opaque transport. The relay MUST preserve message boundaries (each GATT write maps to one serial message and vice versa).

**Acceptance criteria:**

1. Bytes written to the GATT characteristic arrive on USB-CDC unmodified.
2. Bytes sent from the gateway via USB-CDC arrive as a BLE indication unmodified.
3. Each GATT write produces exactly one `BLE_RECV` serial message; each `BLE_INDICATE` serial message produces one logical BLE indication (which the modem MAY fragment into multiple ATT indications per MD-0403).
4. Invalid or garbage payloads are relayed without error or modification.

---

### MD-0402  ATT MTU negotiation

**Priority:** Must
**Source:** ble-pairing-protocol.md §3.4

**Description:**
The modem MUST negotiate ATT MTU ≥ 247 bytes with the connecting BLE client. If the negotiated MTU is < 247, the modem MUST reject the connection.

**Acceptance criteria:**

1. A BLE client requesting MTU = 512 negotiates an MTU ≥ 247.
2. A BLE client that cannot negotiate MTU ≥ 247 is disconnected by the modem.

---

### MD-0403  Indication fragmentation

**Priority:** Must
**Source:** ble-pairing-protocol.md §3.4

**Description:**
When sending indications larger than (MTU − 3) bytes, the modem MUST fragment the message into chunks of at most (MTU − 3) bytes, send each chunk as a separate indication, and wait for ATT Handle Value Confirmation before sending the next chunk. Messages MUST NOT be interleaved.

**Acceptance criteria:**

1. A message larger than (MTU − 3) bytes is split into multiple indications.
2. Each indication payload is ≤ (MTU − 3) bytes.
3. The modem waits for ATT Handle Value Confirmation between chunks.
4. The reassembled message on the client matches the original.
5. The indication fragment queue is bounded at 64 chunks. `BLE_INDICATE` messages whose fragments would exceed this limit MUST be silently dropped with a warning log.

---

### MD-0404  BLE LESC pairing

**Priority:** Must
**Source:** ble-pairing-protocol.md §8.2, §9.2

**Description:**
The modem MUST support BLE LESC Numeric Comparison pairing as the default method to establish an encrypted link with the connecting phone. The modem MUST proactively initiate LESC pairing from the server side immediately after a BLE client connects, ensuring pairing is triggered regardless of whether the client initiates it. During Numeric Comparison, the modem relays the 6-digit passkey to the gateway via `BLE_PAIRING_CONFIRM` and waits for `BLE_PAIRING_CONFIRM_REPLY` before accepting or rejecting the pairing. Just Works remains available as a fallback for environments without operator presence.

**Acceptance criteria:**

1. LESC Numeric Comparison pairing completes successfully with a connecting phone.
2. The resulting BLE link is encrypted.
3. The 6-digit passkey is relayed to the gateway via `BLE_PAIRING_CONFIRM`.
4. Just Works pairing completes successfully when the connecting phone does not support Numeric Comparison; no `BLE_PAIRING_CONFIRM` is sent.
5. The modem MUST initiate LESC pairing from the server side (via `ble_gap_security_initiate`) in the `on_connect` callback, ensuring pairing is triggered even when the client does not initiate it (e.g. btleplug on WinRT, or any client that connects without calling `createBond`).

---

### MD-0405  BLE connection lifecycle

**Priority:** Must
**Source:** ble-pairing-protocol.md §3

**Description:**
The modem MUST support one BLE connection at a time for the Gateway Pairing Service. When a BLE client disconnects, the modem MUST clean up all GATT state and be ready to accept a new connection. BLE pairing operations MUST NOT interfere with concurrent ESP-NOW radio operations.

**Acceptance criteria:**

1. Only one BLE client can be connected at a time.
2. After a BLE client disconnects, a new client can connect and use the service without stale state.
3. Concurrent BLE and ESP-NOW operations do not interfere with each other.

---

### MD-0406  *(Superseded — see MD-0410 and MD-0411)*

> This requirement is superseded by MD-0410 (`BLE_CONNECTED`) and MD-0411 (`BLE_DISCONNECTED`), which provide mandatory connection lifecycle notifications with defined serial message types.

---

### MD-0407  BLE advertising

**Priority:** Must
**Source:** ble-pairing-protocol.md §3.1, modem-protocol.md §4.13, §4.14

**Description:**
BLE advertising is OFF by default after boot and after `RESET`. The modem MUST start advertising the Gateway Pairing Service UUID only after receiving a `BLE_ENABLE` command from the gateway. The modem MUST stop advertising and disconnect any active BLE client on `BLE_DISABLE`. When no BLE client is connected and BLE is enabled, the modem MUST advertise so that phones can discover the gateway for pairing.

**Acceptance criteria:**

1. After boot/RESET, no BLE advertisements are emitted.
2. After `BLE_ENABLE`, the Gateway Pairing Service UUID is present in BLE advertisements.
3. A phone scanning for BLE devices can discover the modem by the service UUID.
4. `BLE_DISABLE` stops advertising and disconnects any active BLE client.
5. Advertising resumes after a BLE client disconnects (if BLE is still enabled).

---

### MD-0408  BLE_INDICATE relay

**Priority:** Must
**Source:** modem-protocol.md §4.9

**Description:**
On receiving a `BLE_INDICATE` (0x20) serial message from the gateway, the modem MUST deliver the `ble_data` as a GATT indication on the Gateway Command characteristic. If no BLE client is connected, the modem MUST silently discard the message. If the serial frame body is empty (no `ble_data`), the modem MUST silently discard the message (per modem-protocol.md §4.9). The modem MUST handle indication fragmentation per ble-pairing-protocol.md §3.4.

**Acceptance criteria:**

1. `BLE_INDICATE` data is delivered as a GATT indication to the connected phone.
2. If no BLE client is connected, the message is silently discarded.
3. If the serial frame body is empty (no `ble_data`), the message is silently discarded.
4. Messages larger than (MTU − 3) bytes are fragmented into multiple indications.

---

### MD-0409  BLE_RECV forwarding

**Priority:** Must
**Source:** modem-protocol.md §4.10

**Description:**
When a phone writes to the Gateway Command characteristic, the modem MUST forward the complete reassembled write payload to the gateway as a `BLE_RECV` (0xA0) serial message. The modem MUST NOT inspect or modify the payload. Empty GATT writes (zero payload bytes) MUST be silently discarded — no `BLE_RECV` is sent (per modem-protocol.md §4.10).

**Acceptance criteria:**

1. GATT writes are forwarded as `BLE_RECV` serial messages.
2. Write Long payloads are reassembled before forwarding.
3. The payload is forwarded unmodified.
4. Empty GATT writes are silently discarded.
5. If a GATT write arrives before server-initiated LESC pairing completes (i.e. `authenticated` is still `false`), the modem MUST buffer the write and forward it as `BLE_RECV` once authentication succeeds, rather than discarding it.
6. The pre-authentication write buffer holds at most one write (single-slot). If a second GATT write arrives before authentication completes, it replaces the previously buffered write. The client should not send more than one write before receiving a response.
7. For normal BLE event enqueueing, the modem uses a 32-entry BLE event queue. A newly arriving GATT write that would be enqueued while that queue is already full MUST be silently dropped with a warning log. Forwarding the single buffered pre-authentication write after authentication succeeds is exempt from this drop rule.

---

### MD-0410  BLE_CONNECTED notification

**Priority:** Must
**Source:** modem-protocol.md §4.11

**Description:**
When a BLE client connects and completes LESC pairing, the modem MUST send a `BLE_CONNECTED` (0xA1) serial message containing the peer BLE address and negotiated ATT MTU.

**Acceptance criteria:**

1. `BLE_CONNECTED` is sent after successful LESC pairing.
2. The message includes the correct peer address and MTU.
3. MTU reported is always ≥ 247.

---

### MD-0411  BLE_DISCONNECTED notification

**Priority:** Must
**Source:** modem-protocol.md §4.12

**Description:**
When the BLE client disconnects, the modem MUST send a `BLE_DISCONNECTED` (0xA2) serial message containing the peer BLE address and HCI disconnect reason code.

**Acceptance criteria:**

1. `BLE_DISCONNECTED` is sent on every BLE disconnect.
2. The message includes the peer address and reason code.

---

### MD-0412  BLE advertising default off

**Priority:** Must
**Source:** modem-protocol.md §4.13

**Description:**
BLE advertising MUST be disabled by default after boot and after `RESET`. The modem MUST NOT advertise BLE services until it receives a `BLE_ENABLE` command from the gateway. This prevents BLE from interfering with ESP-NOW radio operations during normal sensor data collection.

**Acceptance criteria:**

1. After boot/RESET, no BLE advertisements are emitted.
2. BLE advertising begins only after receiving `BLE_ENABLE`.

---

### MD-0413  BLE_ENABLE / BLE_DISABLE commands

**Priority:** Must
**Source:** modem-protocol.md §4.13, §4.14

**Description:**
The modem MUST start BLE advertising on `BLE_ENABLE` and stop advertising + disconnect any active BLE client on `BLE_DISABLE`. Both commands are idempotent.

**Acceptance criteria:**

1. `BLE_ENABLE` starts advertising; `BLE_DISABLE` stops it.
2. `BLE_DISABLE` disconnects any active BLE client (triggering `BLE_DISCONNECTED`).
3. Sending `BLE_ENABLE` when already enabled is a no-op.
4. Sending `BLE_DISABLE` when already disabled is a no-op.

---

### MD-0414  Numeric Comparison pin relay

**Priority:** Must
**Source:** modem-protocol.md §4.15, §4.16

**Description:**
During BLE LESC Numeric Comparison pairing, the modem MUST send `BLE_PAIRING_CONFIRM` with the 6-digit passkey to the gateway and wait for `BLE_PAIRING_CONFIRM_REPLY` before accepting or rejecting the pairing at the application level (see MD-0416 for the SMP-layer tentative accept model). If no reply is received within 30 seconds, the modem MUST reject the pairing.

**Acceptance criteria:**

1. `BLE_PAIRING_CONFIRM` contains the correct 6-digit passkey.
2. `BLE_PAIRING_CONFIRM_REPLY(0x01)` accepts the pairing.
3. `BLE_PAIRING_CONFIRM_REPLY(0x00)` rejects the pairing.
4. No reply within 30 s → pairing rejected.

---

### MD-0415  BLE idle timeout

**Priority:** Must
**Source:** security hardening

**Description:**
The modem MUST enforce a 60-second idle timeout on BLE connections before SMP pairing progresses. If a client connects but does not initiate pairing (no SMP traffic and no progress to Numeric Comparison) within 60 seconds, the modem MUST disconnect the client. This prevents resource exhaustion from abandoned or malicious connections that occupy the single-client BLE slot indefinitely. Once Numeric Comparison has started, the separate 30-second timeout defined in MD-0414 applies.

**Acceptance criteria:**

1. A BLE client that connects and does not initiate pairing (no SMP progress to Numeric Comparison) within 60 s is disconnected by the modem.
2. `BLE_DISCONNECTED` is sent to the gateway after the idle disconnect.
3. If BLE is still enabled, advertising resumes after the idle disconnect.

---

### MD-0416  LESC tentative accept model

**Priority:** Must
**Source:** modem-design.md §15.2 (D9-5)

**Description:**
Because NimBLE's `on_confirm_pin` callback is synchronous and cannot block for the gateway's asynchronous `BLE_PAIRING_CONFIRM_REPLY`, the modem MUST accept the LESC pairing immediately at the BLE SMP layer to allow the BLE stack to proceed with key exchange, then defer the final authentication decision to the gateway via the passkey relay.  This refines MD-0414: the "wait for `BLE_PAIRING_CONFIRM_REPLY`" requirement in MD-0414 applies to the modem's application-level authorization decision, not to the synchronous SMP callback response.  The encrypted link is established before operator approval; the following mitigations MUST be in place:

**Acceptance criteria:**

1. `BleEvent::Connected` is deferred until the operator accepts the passkey via `BLE_PAIRING_CONFIRM_REPLY(0x01)`.
2. GATT writes received before authentication are buffered (not processed) until the `authenticated` flag is set (see MD-0409 criterion 5).
3. NVS bond persistence is disabled (`CONFIG_BT_NIMBLE_NVS_PERSIST=n`) so that tentatively accepted bonds are not persisted across reboots.
4. On operator rejection (`BLE_PAIRING_CONFIRM_REPLY(0x00)`), the client is disconnected immediately.

---

## 8  Operational logging

### MD-0500  ESP-NOW frame logging

**Priority:** Must
**Source:** operational visibility (issue #459)

**Description:**
The modem MUST log ESP-NOW frame activity to the diagnostic UART so that operators can observe radio traffic without additional tooling.

**Acceptance criteria:**

1. When an ESP-NOW frame is received and forwarded to USB-CDC, the modem logs at INFO level: peer MAC, payload length, and RSSI.
2. When an ESP-NOW frame is sent, the modem logs at INFO level: peer MAC, payload length, and send result (success or failure).

---

### MD-0501  BLE lifecycle logging

**Priority:** Must
**Source:** operational visibility (issue #459)

**Description:**
The modem MUST log BLE connection lifecycle events at INFO level to the diagnostic UART.

**Acceptance criteria:**

1. When a BLE client connects, the modem logs at INFO level: peer address and initial MTU.
2. When a BLE client disconnects, the modem logs at INFO level: peer address and HCI reason code.
3. When BLE advertising starts, the modem logs at INFO level.
4. When BLE advertising stops, the modem logs at INFO level.

---

### MD-0502  BLE GATT write logging

**Priority:** Must
**Source:** operational visibility (issue #459)

**Description:**
The modem MUST log BLE GATT write events at INFO level to the diagnostic UART.

**Acceptance criteria:**

1. When an authenticated GATT write is received and forwarded, the modem logs at INFO level: payload length.
2. When a GATT write is buffered (awaiting authentication), the modem logs at INFO level: payload length and authentication state.
3. When a buffered GATT write is flushed after authentication, the modem logs at INFO level: payload length.

---

### MD-0503  USB-CDC message logging

**Priority:** Must
**Source:** operational visibility (issue #459)

**Description:**
The modem MUST log USB-CDC serial message relay events at DEBUG level to the diagnostic UART.

**Acceptance criteria:**

1. When a USB-CDC serial message is sent to the gateway, the modem logs at DEBUG level: message type and encoded frame length.
2. When a USB-CDC serial message is received from the gateway, the modem logs at DEBUG level: message type.

---

### MD-0504  BLE pairing event logging

**Priority:** Must
**Source:** operational visibility (issue #459)

**Description:**
The modem MUST log BLE pairing events at INFO level to the diagnostic UART.

**Acceptance criteria:**

1. When server-initiated LESC pairing is triggered, the modem logs at INFO level: connection handle.
2. When authentication completes successfully, the modem logs at INFO level.
3. When authentication fails, the modem logs at WARN level: failure reason.

---

### MD-0505  Build-type–aware log levels

**Priority:** Must
**Source:** issue #496

**Description:**
The modem MUST apply build-type–aware log-level policies identical to the node (ND-1012) to eliminate logging overhead in release firmware builds. Logging requirements elsewhere in this document that mandate INFO-level logging (MD-0500–MD-0504) apply to debug and verbose firmware builds; in release builds (without the `verbose` feature) those log call-sites are compiled out.

**Acceptance criteria:**

1. In debug builds, the compile-time maximum log level is TRACE.
2. In release `quiet` builds (the default, without the `verbose` feature), the compile-time maximum log level is WARN (`trace!`, `debug!`, and `info!` call-sites are no-ops).
3. In release `verbose` builds (with `feature = "verbose"` enabled), the compile-time maximum log level is DEBUG (`trace!` call-sites are no-ops; `debug!` and `info!` call-sites remain compiled in).
4. The runtime default log level is INFO in debug and release `verbose` builds, and WARN in release `quiet` builds.
5. MD-0500–MD-0504 INFO-level requirements are satisfied by the verbose build variant emitting those events; release `quiet` builds omit them without functional behavior changes.
6. The `quiet` and `verbose` features are mutually exclusive; a `compile_error!` fires if both are enabled.

---

### MD-0506  Error diagnostic observability

**Priority:** Must
**Source:** Issue #532

**Description:**
When the modem encountersan error at an operator-visible boundary (BLE GATT operations, ESP-NOW transmission, USB-CDC I/O), the error log MUST include sufficient context for an operator to diagnose the root cause without access to source code. At minimum, each error MUST include: (1) the operation that failed (e.g., "BLE GATT write", "NODE_ACK indication", "ESP-NOW send"), (2) the non-sensitive metadata for the input or parameters that triggered it (e.g., peer MAC address, characteristic UUID or handle, frame type and length), (3) the specific error from the underlying subsystem (e.g., NimBLE return code, ESP-NOW status), and (4) actionable guidance where possible (e.g., "check BLE connection state", "verify peer is in range"). Diagnostics MUST NOT log raw BLE attribute or notification payload contents, nor full ESP-NOW application payload bytes, at any log level; only metadata necessary for troubleshooting may be recorded.

**Acceptance criteria:**

1. Every error log entry at an operator-visible boundary includes the failed operation name, the triggering input/parameters expressed as metadata (peer MAC, UUID/handle, frame type/length, etc.), and the underlying subsystem error.
2. Where a corrective action is known, the error includes actionable guidance text.
3. BLE indication failures (e.g., NODE_ACK) include the NimBLE return code, the current connection state, and the characteristic handle rather than a generic "invalid arguments" message.
4. ESP-NOW send failures include the target peer MAC address and the ESP-NOW status code, but do not include raw application payload bytes.

---

## 9  Button input

### MD-0600  Button GPIO configuration

**Priority:** Must
**Source:** modem-protocol.md §4.17, Issue #756

**Description:**
The modem firmware MUST configure GPIO2 (XIAO ESP32-S3 silk label D1, 1-Wire data line) as a GPIO input, active-low. The firmware MUST enable the ESP32-S3 internal pull-up unconditionally; this is safe to use alongside the carrier board's external pull-up (parallel pull-ups simply lower the effective resistance).

**Acceptance criteria:**

1. GPIO2 is configured as input at boot.
2. The pin reads HIGH when the button is not pressed.
3. The pin reads LOW when the button is pressed.

---

### MD-0601  Button debounce

**Priority:** Must
**Source:** modem-protocol.md §4.17, Issue #756

**Description:**
The modem firmware MUST debounce the button input with a 30 ms window. Transitions shorter than 30 ms MUST be ignored.

**Acceptance criteria:**

1. A LOW pulse shorter than 30 ms does not generate an event.
2. A LOW pulse of 30 ms or longer is recognized as a valid press.

---

### MD-0602  Button press classification

**Priority:** Must
**Source:** modem-protocol.md §4.17, Issue #756

**Description:**
The modem firmware MUST classify button presses by their duration, measured from the debounced press to the debounced release:

- **BUTTON_SHORT**: press duration < 1 second.
- **BUTTON_LONG**: press duration ≥ 1 second.

The classification MUST be determined on release — no event is emitted while the button is held.

**Acceptance criteria:**

1. A 500 ms press emits BUTTON_SHORT.
2. A 999 ms press emits BUTTON_SHORT.
3. A 1000 ms press emits BUTTON_LONG.
4. A 1500 ms press emits BUTTON_LONG.
5. No event is emitted while the button remains held.

---

### MD-0603  EVENT_BUTTON emission

**Priority:** Must
**Source:** modem-protocol.md §4.17, Issue #756

**Description:**
On button release, the modem firmware MUST emit an `EVENT_BUTTON` message (serial type `0xB0`) over USB-CDC. The message body contains a single `button_type` byte: `0x00` for BUTTON_SHORT, `0x01` for BUTTON_LONG. No state is retained between events. No acknowledgement from the gateway is expected.

**Acceptance criteria:**

1. `EVENT_BUTTON` is sent within one main-loop poll cycle of the debounced release.
2. The `button_type` byte is `0x00` for short presses and `0x01` for long presses.
3. No state persists between successive button events.

---

### MD-0604  Button non-interference

**Priority:** Must
**Source:** Issue #756

**Description:**
Button scanning MUST NOT block or delay ESP-NOW RX/TX, BLE callbacks, or USB-CDC framing. The firmware MUST use non-blocking GPIO polling in the main loop — no dedicated FreeRTOS task, no GPIO interrupts that could preempt radio callbacks.

**Acceptance criteria:**

1. ESP-NOW frame forwarding latency is not measurably increased when button events are occurring.
2. BLE pairing operations complete successfully during concurrent button presses.
3. No GPIO interrupt service routines are registered for the button pin.

---

### MD-0605  No button-semantic logic in modem

**Priority:** Must
**Source:** Issue #756

**Description:**
The modem MUST NOT interpret button meaning. Specifically, the modem MUST NOT: enter pairing mode based on button presses, generate display content based on button state, change BLE advertising state, or maintain any button-related state beyond the debounce and classification logic required by MD-0601 and MD-0602. All button semantics are handled by the gateway.

**Acceptance criteria:**

1. No code path in the modem firmware maps button events to pairing, locally generated display behavior, BLE, or any other subsystem action.
2. `EVENT_BUTTON` is a pure, stateless notification.

---

## 10  Display output

### MD-0700  DISPLAY_FRAME command acceptance

**Priority:** Must
**Source:** modem-protocol.md §4.7a, Issue #757

**Description:**
The modem firmware MUST accept a `DISPLAY_FRAME` serial command carrying exactly 1024 bytes of framebuffer data for a 128×64 monochrome display. The framebuffer format is row-major, top-to-bottom, left-to-right, with the most-significant bit representing the leftmost pixel of each 8-pixel group. The modem MUST support full-frame updates only; partial updates are not supported.

**Acceptance criteria:**

1. A `DISPLAY_FRAME` body of exactly 1024 bytes is accepted.
2. A `DISPLAY_FRAME` body of 1023 bytes produces `EVENT_ERROR(INVALID_FRAME)`.
3. A `DISPLAY_FRAME` body larger than 1024 bytes exceeds the serial framing limit and is handled as a framing error, not as `EVENT_ERROR(INVALID_FRAME)`.
4. The modem does not expose any partial-row, partial-page, or sub-rectangle display update command.

---

### MD-0701  SSD1306 full-frame rendering

**Priority:** Must
**Source:** Issue #757

**Description:**
The modem firmware MUST render each accepted `DISPLAY_FRAME` to an SSD1306-compatible 128×64 I²C OLED connected to the ESP32-S3 module's D4/D5 pins at 7-bit I²C address `0x3C`. The rendered image MUST match the supplied framebuffer orientation and pixel values.

**Acceptance criteria:**

1. The display write targets I²C address `0x3C`.
2. Pixels rendered on the OLED match the supplied 128×64 framebuffer, including row order and MSB-first bit packing.
3. Every accepted update refreshes the full display image rather than only a subset of pages or columns.

---

### MD-0702  Display-path non-interference

**Priority:** Must
**Source:** Issue #757

**Description:**
Display rendering MUST NOT block or materially delay ESP-NOW RX/TX, BLE callbacks, or USB-CDC framing. Display work MUST be scheduled so that modem radio, BLE, and serial duties continue to make progress while frames are being rendered.

**Acceptance criteria:**

1. ESP-NOW frame forwarding latency is not measurably increased during repeated `DISPLAY_FRAME` updates.
2. BLE pairing operations complete successfully while `DISPLAY_FRAME` updates are being rendered.
3. USB-CDC command processing continues while display updates are in flight.

---

### MD-0703  No firmware-generated display UI

**Priority:** Must
**Source:** Issue #757

**Description:**
The modem firmware MUST NOT generate text, menus, pairing screens, status overlays, or any other display content on its own. The modem MUST NOT interpret framebuffer meaning. Its only display responsibility is to render the exact gateway-supplied framebuffer onto the OLED.

**Acceptance criteria:**

1. No code path synthesizes display pixels from button state, BLE state, pairing state, radio state, or any other modem-local state.
2. The same `DISPLAY_FRAME` payload produces the same rendered output regardless of modem runtime state.
3. The modem does not implement text rendering, menu generation, or display-side pairing UX.

---

### MD-0704  Recoverable display error reporting

**Priority:** Must
**Source:** modem-protocol.md §4.8a, Issue #757

**Description:**
Display-related faults MUST be reported via `EVENT_ERROR`, not the unrecoverable `ERROR` message. Invalid `DISPLAY_FRAME` payload size MUST report `EVENT_ERROR(INVALID_FRAME)`. OLED I²C write failure MUST report `EVENT_ERROR(DISPLAY_WRITE_FAILED)`. These errors are recoverable and MUST NOT reset unrelated modem state.

**Acceptance criteria:**

1. Invalid `DISPLAY_FRAME` length produces `EVENT_ERROR(INVALID_FRAME)`.
2. An I²C write failure during display update produces `EVENT_ERROR(DISPLAY_WRITE_FAILED)`.
3. After either error, the modem remains operational for ESP-NOW, BLE, USB-CDC, and future `DISPLAY_FRAME` commands.

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
| MD-0208 | SEND_FRAME body validation | Must |
| MD-0209 | SET_CHANNEL error reporting | Must |
| MD-0300 | Reset command | Must |
| MD-0301 | USB disconnection handling | Must |
| MD-0302 | Watchdog timer | Should |
| MD-0303 | Status reporting | Must |
| MD-0400 | Gateway Pairing Service | Must |
| MD-0401 | BLE ↔ USB-CDC message relay | Must |
| MD-0402 | ATT MTU negotiation | Must |
| MD-0403 | Indication fragmentation | Must |
| MD-0404 | BLE LESC pairing | Must |
| MD-0405 | BLE connection lifecycle | Must |
| MD-0406 | *(Superseded by MD-0410/MD-0411)* | — |
| MD-0407 | BLE advertising | Must |
| MD-0408 | BLE_INDICATE relay | Must |
| MD-0409 | BLE_RECV forwarding | Must |
| MD-0410 | BLE_CONNECTED notification | Must |
| MD-0411 | BLE_DISCONNECTED notification | Must |
| MD-0412 | BLE advertising default off | Must |
| MD-0413 | BLE_ENABLE / BLE_DISABLE commands | Must |
| MD-0414 | Numeric Comparison pin relay | Must |
| MD-0415 | BLE idle timeout | Must |
| MD-0416 | LESC tentative accept model | Must |
| MD-0500 | ESP-NOW frame logging | Must |
| MD-0501 | BLE lifecycle logging | Must |
| MD-0502 | BLE GATT write logging | Must |
| MD-0503 | USB-CDC message logging | Must |
| MD-0504 | BLE pairing event logging | Must |
| MD-0505 | Build-type–aware log levels | Must |
| MD-0506 | Error diagnostic observability | Must |
| MD-0600 | Button GPIO configuration | Must |
| MD-0601 | Button debounce | Must |
| MD-0602 | Button press classification | Must |
| MD-0603 | EVENT_BUTTON emission | Must |
| MD-0604 | Button non-interference | Must |
| MD-0605 | No button-semantic logic in modem | Must |
| MD-0700 | DISPLAY_FRAME command acceptance | Must |
| MD-0701 | SSD1306 full-frame rendering | Must |
| MD-0702 | Display-path non-interference | Must |
| MD-0703 | No firmware-generated display UI | Must |
| MD-0704 | Recoverable display error reporting | Must |
