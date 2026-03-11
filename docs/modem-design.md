<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Design Specification

> **Document status:** Draft
> **Scope:** Architecture and internal design of the ESP32-S3 radio modem firmware.
> **Audience:** Implementers (human or LLM agent) building the modem firmware.
> **Related:** [modem-requirements.md](modem-requirements.md), [modem-protocol.md](modem-protocol.md), [node-design.md](node-design.md)

---

## 1  Overview

The modem firmware is a simple bidirectional bridge between USB-CDC and ESP-NOW. It runs on an ESP32-S3 and has no awareness of the Sonde node–gateway protocol — it relays opaque byte frames, adding only peer MAC address and RSSI metadata.

```
┌──────────────────────────────────────────────────────────────┐
│  ESP32-S3 Modem Firmware                                     │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌────────────┐  │
│  │ USB-CDC  │──│  Serial  │──│  Bridge   │──│  ESP-NOW   │  │
│  │ Driver   │  │  Codec   │  │  Logic    │  │  Driver    │  │
│  └──────────┘  └──────────┘  └───────────┘  └────────────┘  │
│                                                              │
│  ┌──────────┐  ┌──────────┐                                  │
│  │ Counters │  │ Peer     │                                  │
│  │ & Status │  │ Table    │                                  │
│  └──────────┘  └──────────┘                                  │
└──────────────────────────────────────────────────────────────┘
```

The firmware is intentionally minimal — no crypto, no CBOR parsing, no sessions, no OTA updates.

---

## 2  Technology choices

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust | Shared toolchain with `sonde-node`; memory safety; `no_std` support |
| Platform | ESP-IDF via `esp-idf-sys` | Native USB-CDC and ESP-NOW support on ESP32-S3 |
| Async | None (single-threaded event loop) | Firmware is simple enough to not need an async runtime |
| Serial codec | `sonde-protocol::modem` | Shared `no_std` codec ensures wire-format compatibility with the gateway |

---

## 3  Module architecture

### 3.1  Module responsibilities

| Module | Responsibility | Requirements covered |
|--------|---------------|---------------------|
| **USB-CDC driver** | USB-CDC ACM device initialization, byte-level read/write | MD-0100 |
| **Serial codec** | Length-prefixed frame encode/decode, message type dispatch | MD-0101, MD-0102, MD-0103 |
| **Bridge logic** | Routes messages between USB and ESP-NOW, command dispatch | MD-0201, MD-0202, MD-0205 |
| **ESP-NOW driver** | WiFi/ESP-NOW init, send, receive callback, channel config | MD-0200, MD-0206, MD-0207 |
| **Peer table** | Auto-registration and LRU eviction of ESP-NOW peers | MD-0203, MD-0204 |
| **Counters & status** | `tx_count`, `rx_count`, `tx_fail_count`, `uptime_s` tracking | MD-0303 |

---

## 4  USB-CDC driver

The ESP32-S3 has a native USB peripheral (not USB-over-JTAG). The firmware uses ESP-IDF's `tinyusb` CDC-ACM class driver.

### 4.1  Initialization

1. Configure TinyUSB CDC-ACM descriptor.
2. Register the receive callback for inbound data from the gateway.
3. The CDC device enumerates automatically when USB is connected.

### 4.2  Read path

The CDC receive callback is invoked when the host writes data. Received bytes are appended to a ring buffer that the serial codec reads from in the main loop.

### 4.3  Write path

Outbound frames (e.g., `RECV_FRAME`, `MODEM_READY`) are enqueued into a TX ring buffer from any producer context (including ESP-NOW receive callbacks). The main loop drains this buffer and writes to the USB-CDC TX endpoint. If the host-side TX buffer is full, the main loop retries with back-pressure, but callbacks never perform blocking USB writes.

### 4.4  Disconnection detection

The firmware monitors the DTR line state. When DTR drops (host closed the port), the firmware sets a `usb_connected` flag to `false` and discards inbound ESP-NOW frames. When DTR reasserts, the firmware resets state and sends `MODEM_READY`.

---

## 5  Serial codec

The serial codec implements the length-prefixed framing protocol defined in [modem-protocol.md §2](modem-protocol.md#2--serial-framing).

### 5.1  Shared code

The frame envelope encoder/decoder and message type constants live in `sonde-protocol::modem` (a `no_std` module shared with the gateway). This guarantees wire-format compatibility.

### 5.2  Inbound decoding (gateway → modem)

1. Read 2 bytes → `len` (big-endian u16).
2. If `len` = 0 → silently discard (no resync needed).
3. If `len` > 512 → discard, trigger `RESET`-based resync.
4. Read `len` bytes → `type` (1 byte) + `body` (remaining).
5. Dispatch by `type`:

| Type | Handler |
|------|---------|
| 0x01 `RESET` | → `handle_reset()` |
| 0x02 `SEND_FRAME` | → `handle_send_frame(body)` |
| 0x03 `SET_CHANNEL` | → `handle_set_channel(body)` |
| 0x04 `GET_STATUS` | → `handle_get_status()` |
| 0x05 `SCAN_CHANNELS` | → `handle_scan_channels()` |
| Unknown | → silently discard |

### 5.3  Outbound encoding (modem → gateway)

1. Compute `len` = 1 (type) + body length.
2. Write `len` as 2 bytes big-endian.
3. Write `type` (1 byte).
4. Write body.

---

## 6  ESP-NOW driver

### 6.1  Initialization

1. Initialize WiFi in station mode (no AP connection — station mode is required for ESP-NOW).
2. Call `esp_now_init()`.
3. Register the receive callback (`esp_now_register_recv_cb`).
4. Register the send callback (`esp_now_register_send_cb`).
5. Default channel: 1.

### 6.2  Receive callback

The ESP-NOW receive callback is invoked from the WiFi task. It receives:
- `mac_addr`: sender's 6-byte MAC.
- `data`: frame payload (up to 250 bytes).
- `rssi`: signal strength (from the `esp_now_recv_info_t` struct).

The callback constructs a `RECV_FRAME` message and writes it to the USB-CDC TX path. The `rx_count` counter is incremented.

### 6.3  Send path

On `SEND_FRAME`:

1. Extract `peer_mac` (first 6 bytes) and `frame_data` (remaining bytes).
2. If `peer_mac` is not in the ESP-NOW peer table → call `add_peer()`.
3. Call `esp_now_send(peer_mac, frame_data, len)` and increment `tx_count`.
4. In the send callback, if delivery failed, increment `tx_fail_count`.

### 6.4  Channel configuration

On `SET_CHANNEL`:

1. Call `esp_wifi_set_channel(channel, WIFI_SECOND_CHAN_NONE)`.
2. Clear the peer table (peers are channel-specific).
3. Send `SET_CHANNEL_ACK`.

### 6.5  Channel scanning

On `SCAN_CHANNELS`:

1. Call `esp_wifi_scan_start()` with `channel = 0` (all channels), blocking mode.
2. Call `esp_wifi_scan_get_ap_records()`.
3. Aggregate per channel: count APs, track strongest RSSI.
4. Send `SCAN_RESULT`.
5. Re-initialize ESP-NOW on the current channel (scanning may disrupt it).

---

## 7  Peer table management

ESP-NOW supports a maximum of ~20 peers. The modem manages this transparently.

### 7.1  Data structure

```rust
struct PeerEntry {
    mac: [u8; 6],
    last_used: u32,  // uptime tick at last send
}
```

A fixed-size array of `PeerEntry` (capacity 20). Entries are tracked by insertion order and last-use time.

### 7.2  Auto-registration

When `SEND_FRAME` targets a MAC not in the table:

1. If the table is not full → `esp_now_add_peer()` and insert entry.
2. If the table is full → evict the least-recently-used entry (`esp_now_del_peer()` + `esp_now_add_peer()`).

### 7.3  Clear on channel change

On `SET_CHANNEL` or `RESET`, all peers are removed via `esp_now_del_peer()` and the table is cleared.

---

## 8  Counters and status

The firmware maintains four counters:

| Counter | Incremented when |
|---------|-----------------|
| `tx_count` | `esp_now_send()` is called (regardless of outcome) |
| `rx_count` | `RECV_FRAME` is written to USB |
| `tx_fail_count` | ESP-NOW send callback reports failure |
| `uptime_s` | Derived from `esp_timer_get_time() / 1_000_000` |

All counters reset to zero on boot and on `RESET`.

On `GET_STATUS`, the firmware reads the current values and sends a `STATUS` message.

---

## 9  Main loop

The firmware runs a single-threaded event loop (no RTOS tasks beyond the WiFi/USB system tasks):

```
loop {
    if usb_has_data() {
        frame = serial_codec.decode();
        dispatch(frame);
    }

    // ESP-NOW receive callback writes RECV_FRAME to USB
    // asynchronously from the WiFi task — no polling needed.

    feed_watchdog();
}
```

The main loop polls the USB receive buffer. ESP-NOW frames arrive via callback and are written to USB from the callback context (or queued if contention exists).

---

## 10  Reset behavior

On `RESET` command or USB reconnection:

1. `esp_now_deinit()`.
2. Clear peer table.
3. Reset all counters.
4. `esp_now_init()` on channel 1.
5. Re-register callbacks.
6. Reset the serial codec's inbound parser state.
7. Send `MODEM_READY`.

---

## 11  Watchdog

The firmware uses the ESP-IDF task watchdog (`esp_task_wdt`):

- Timeout: 10 seconds.
- The main loop feeds the watchdog on each iteration.
- If the main loop stalls (e.g., deadlock, infinite loop), the watchdog triggers a hardware reset.
- After reset, the firmware boots normally and sends `MODEM_READY`.

---

## 12  Shared code with sonde-node

Both the modem and node firmware target ESP32 via ESP-IDF. Shared code:

| Module | Shared between | Contents |
|--------|---------------|----------|
| `sonde-protocol::modem` | Gateway + modem | Serial frame codec, message types |
| ESP-NOW driver (planned) | Modem + node | WiFi init, ESP-NOW init/send/recv, peer management |

The modem extends the shared ESP-NOW driver with channel scanning and USB bridging. The node extends it with wake-cycle integration and key storage.

---

## 13  Error handling

| Condition | Behavior |
|-----------|----------|
| USB-CDC disconnection | Set `usb_connected = false`, discard ESP-NOW frames, send `MODEM_READY` on reconnect |
| ESP-NOW init failure | Send `ERROR(ESPNOW_INIT_FAILED)` to gateway |
| WiFi init failure | Send `ERROR(WIFI_INIT_FAILED)` to gateway |
| Channel set failure | Send `ERROR(CHANNEL_SET_FAILED)` to gateway |
| `SEND_FRAME` with body < 7 bytes | Silently discard (no MAC + data) |
| Serial frame `len` > 512 | Trigger `RESET`-based resync (§2.3 of modem-protocol.md) |
| Unknown serial message type | Silently discard |
