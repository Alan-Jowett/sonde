<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Design Specification

> **Document status:** Draft
> **Scope:** Architecture and internal design of the ESP32-S3 radio modem firmware.
> **Audience:** Implementers (human or LLM agent) building the modem firmware.
> **Related:** [modem-requirements.md](modem-requirements.md), [modem-protocol.md](modem-protocol.md), [node-design.md](node-design.md)

---

## 1  Overview

The modem firmware is a tri-directional bridge between USB-CDC, ESP-NOW, and BLE GATT, with an additional gateway-controlled OLED display output path. It runs on an ESP32-S3 and has no awareness of the Sonde node–gateway protocol — it relays opaque byte frames, adding only peer MAC address and RSSI metadata, and renders opaque display framebuffers without interpreting their content. BLE is used exclusively for the Gateway Pairing Service, which relays pairing messages between a phone and the gateway.

```
┌──────────────────────────────────────────────────────────────┐
│  ESP32-S3 Modem Firmware                                     │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌────────────┐  │
│  │ USB-CDC  │──│  Serial  │──│  Bridge   │──│  ESP-NOW   │  │
│  │ Driver   │  │  Codec   │  │  Logic    │  │  Driver    │  │
│  └──────────┘  └──────────┘  └─────┬─────┘  └────────────┘  │
│                                    │                         │
│  ┌──────────┐  ┌──────────┐  ┌─────┴─────┐  ┌────────────┐  │
│  │ Counters │  │ Peer     │  │ BLE GATT  │──│  BLE       │  │
│  │ & Status │  │ Table    │  │ Service   │  │  Driver    │  │
│  └──────────┘  └──────────┘  └───────────┘  └────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

> **Note:** The OLED display path is omitted from the diagram for readability; see §9a for the display driver design.

The firmware is intentionally minimal — no application- or protocol-layer crypto (all BLE link security is handled inside the BLE stack), no CBOR parsing, no OTA updates. The modem does not interpret message contents on any transport. The display path is intentionally dumb: it writes gateway-supplied framebuffers to the OLED but does not generate text, menus, or pairing screens. BLE connection state is managed only for the GATT pairing relay; the modem holds no protocol-level sessions.

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
| **Bridge logic** | Routes messages between USB, ESP-NOW, and BLE; command dispatch; reset orchestration | MD-0201, MD-0202, MD-0205, MD-0208, MD-0209, MD-0300 |
| **ESP-NOW driver** | WiFi/ESP-NOW init, send, receive callback, channel config | MD-0200, MD-0206, MD-0207 |
| **Peer table** | Auto-registration and LRU eviction of ESP-NOW peers | MD-0203, MD-0204 |
| **Counters & status** | `tx_count`, `rx_count`, `tx_fail_count`, `uptime_s` tracking | MD-0303 |
| **BLE driver** | NimBLE stack init, advertising start/stop, GATT server, LESC pairing | MD-0402, MD-0404, MD-0407, MD-0412 |
| **BLE GATT service** | Gateway Pairing Service + Gateway Command characteristic; indication pacing; Write Long reassembly | MD-0400, MD-0401, MD-0403, MD-0408, MD-0409 |
| **BLE lifecycle** | `BLE_ENABLE`/`BLE_DISABLE` handling, connection/disconnection events, `BLE_CONNECTED`/`BLE_DISCONNECTED` notifications, idle timeout | MD-0405, MD-0410, MD-0411, MD-0413, MD-0414, MD-0415 |
| **Watchdog** *(cross-cutting)* | Task watchdog feed in main loop; hardware reset on stall | MD-0302 |
| **Button scanner** | GPIO2 polling, debounce, press classification, `EVENT_BUTTON` emission | MD-0600, MD-0601, MD-0602, MD-0603, MD-0604, MD-0605 |
| **Display driver** | Validate `DISPLAY_FRAME`, convert row-major pixels to SSD1306 page writes, incremental I²C flush, `EVENT_ERROR` emission | MD-0700, MD-0701, MD-0702, MD-0703, MD-0704 |

---

## 4  USB-CDC driver

The ESP32-S3 has a native USB peripheral (not USB-over-JTAG). The firmware uses ESP-IDF's `tinyusb` CDC-ACM class driver.

### 4.1  Initialization

1. Configure TinyUSB CDC-ACM descriptor.
2. Register the receive callback for inbound data from the gateway.
3. The CDC device enumerates automatically when USB is connected.

### 4.2  Read path

The CDC receive callback is invoked when the host writes data. Received bytes are appended to a ring buffer that the serial codec reads from in the main loop.

> **USB-CDC RX ring buffer (D9-1):** The USB-CDC receive callback stores inbound bytes in a pre-allocated, fixed-capacity ring buffer (e.g., `USB_RX_RING_CAP` slots). When the ring is full, incoming bytes are silently dropped and a `drop_count` counter is incremented. Bytes may also be dropped if the USB/TinyUSB task callback cannot acquire the ring mutex (contention with the main loop draining the buffer). This is intentional — the callback runs in the USB task context where heap allocation and blocking are unsafe; only the main loop drains the ring into the serial codec.

### 4.3  Write path

Outbound frames (e.g., `RECV_FRAME`, `MODEM_READY`) are enqueued into a TX ring buffer from any producer context (including ESP-NOW receive callbacks). The main loop drains this buffer and writes to the USB-CDC TX endpoint. If the host-side TX buffer is full, the main loop retries with back-pressure, but callbacks never perform blocking USB writes.

### 4.4  Disconnection detection

Connectivity is inferred from USB read/write success — there is no DTR line-state callback available in the current ESP-IDF Rust HAL (`esp-idf-hal` v0.45). When a read or write returns an error, the firmware sets `usb_connected` to `false`. Successful writes set `usb_connected` back to `true` but do not trigger any notification on their own. When a subsequent read succeeds (data arrives from a re-opened port), `usb_connected` flips to `true` and the bridge sends `MODEM_READY`.

The shared `usb_connected` flag (an `AtomicBool`) is also read by the ESP-NOW receive callback, which discards inbound radio frames while USB is disconnected (MD-0301).

> **Future improvement:** If ESP-IDF exposes a DTR line-state change callback, register it to set `usb_connected` proactively rather than reactively on I/O failure.

---

## 5  Serial codec

The serial codec implements the length-prefixed framing protocol defined in [modem-protocol.md §2](modem-protocol.md#2--serial-framing).

### 5.1  Shared code

The frame envelope encoder/decoder and message type constants live in `sonde-protocol::modem` (a `no_std` module shared with the gateway). This guarantees wire-format compatibility.

### 5.2  Inbound decoding (gateway → modem)

1. Read 2 bytes → `len` (big-endian u16).
2. If `len` = 0 → silently discard (no resync needed).
3. If `len` > 1025 → discard, trigger `RESET`-based resync.
4. Read `len` bytes → `type` (1 byte) + `body` (remaining).
5. Dispatch by `type`:

| Type | Handler |
|------|---------|
| 0x01 `RESET` | → `handle_reset()` |
| 0x02 `SEND_FRAME` | → `handle_send_frame(body)` |
| 0x03 `SET_CHANNEL` | → `handle_set_channel(body)` |
| 0x04 `GET_STATUS` | → `handle_get_status()` |
| 0x05 `SCAN_CHANNELS` | → `handle_scan_channels()` |
| 0x09 `DISPLAY_FRAME` | → `handle_display_frame(body)` |
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

The callback copies the frame into the ESP-NOW RX ring buffer; the main loop drains the ring and writes `RECV_FRAME` messages to USB. For each frame that is successfully forwarded to USB, the `rx_count` counter is incremented.

> **ESP-NOW RX ring buffer (D9-1):** The ESP-NOW receive callback stores inbound frames in a pre-allocated, fixed-capacity ring buffer (`RX_RING_CAP = 16` slots). When the ring is full (or a push into the ring fails), incoming frames are silently dropped and a `drop_count` counter is incremented. If the WiFi-task callback cannot acquire the ring mutex (contention with the main loop draining the buffer), the frame is also dropped, and this is recorded separately via an atomic `contention_drops` counter. This is intentional — the callback runs in the WiFi task context where heap allocation and blocking are unsafe.

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
    // bridge.poll() handles:
    //   - USB serial decode + dispatch
    //   - BLE event drain
    //   - Button GPIO poll → EVENT_BUTTON on classified release
    //   - Incremental OLED page flush
    bridge.poll();

    feed_watchdog();
}
```

The main loop delegates all I/O to `Bridge::poll()`, which decodes USB serial frames, drains BLE events, polls the button GPIO, and advances at most one OLED page flush per iteration. ESP-NOW frames arrive via callback into the ring; the bridge constructs `RECV_FRAME` messages and writes them to USB. Button press/release is detected via non-blocking GPIO reads and classified by duration (see §16).

> **Per-poll processing caps (D9-2):** BLE events are drained up to `MAX_BLE_EVENTS_PER_POLL` (16) per main-loop iteration to prevent sustained BLE traffic from starving serial decode and ESP-NOW radio processing.

> **Queue size limits (D9-3):** The BLE driver uses a bounded event queue (`MAX_BLE_EVENT_QUEUE = 32`). Events arriving when the queue is full are dropped; the firmware emits warnings for some drop conditions (for example, GATT writes rejected when not authenticated or when the event queue is full, and indication queue overflow). Outbound BLE indication fragments are also bounded (`MAX_INDICATION_CHUNKS = 64`); `BLE_INDICATE` messages that would exceed this limit are rejected. These caps prevent unbounded memory growth on the constrained ESP32-S3.

## 9a  Display output

The display path is integrated into `Bridge::poll()` so rendering remains subordinate to USB, ESP-NOW, and BLE work. Display I²C operations are never performed from WiFi, USB, or BLE callbacks.

### 9a.1  Hardware target

The modem drives an SSD1306-compatible 128×64 OLED over I²C on the ESP32-S3 module's D4/D5 pins at 7-bit address `0x3C`.

### 9a.2  Command handling

On `DISPLAY_FRAME`:

1. Validate that the body length is exactly 1024 bytes.
2. Treat the body as a row-major, MSB-first framebuffer (`0x80` = leftmost pixel).
3. Copy the framebuffer into a pending display buffer.
4. Mark a new flush sequence starting at page 0.

If the body length is not 1024 bytes, the bridge enqueues `EVENT_ERROR(INVALID_FRAME)` and leaves the current display contents unchanged.

### 9a.3  Incremental OLED flush

The SSD1306 display RAM is page-oriented (8 vertical pixels per byte), while `DISPLAY_FRAME` uses row-major pixels. The display driver therefore converts the pending framebuffer into SSD1306 page writes during flush.

To preserve modem responsiveness (MD-0702), `display.poll()` performs at most one SSD1306 page write per main-loop iteration:

1. If the display is not initialized yet, send the SSD1306 init sequence once.
2. Select the next page/column window.
3. Convert one 128-byte page from the row-major framebuffer into SSD1306 page bytes.
4. Issue the I²C write for that page and return to the main loop.

If a newer `DISPLAY_FRAME` arrives while an older one is still flushing, the pending buffer is replaced and the next flush restarts from page 0 so the display converges to the latest gateway-supplied image.

### 9a.4  Failure handling

If any OLED I²C transaction fails during initialization or page flush:

1. Abort the current flush attempt.
2. Enqueue `EVENT_ERROR(DISPLAY_WRITE_FAILED)`.
3. Leave all non-display modem state unchanged.
4. Accept future `DISPLAY_FRAME` commands normally.

---

## 10  Reset behavior

On `RESET` command or USB reconnection:

1. `esp_now_deinit()`.
2. Clear peer table.
3. Reset all counters.
4. `esp_now_init()` on channel 1.
5. Re-register callbacks.
6. Reset the serial codec's inbound parser state.
7. If BLE is enabled, perform the same internal disable logic as handling `BLE_DISABLE`: stop advertising and disconnect any active BLE client.
8. Send `MODEM_READY`.

`MODEM_READY` MUST be sent within 2 seconds of USB enumeration (or re-enumeration after reconnection) per MD-0104. This deadline applies to both initial boot and `RESET`-triggered reinitialisation.

---

## 11  Watchdog

The firmware uses the ESP-IDF task watchdog (`esp_task_wdt`):

- Timeout: 35 seconds (set via `CONFIG_ESP_TASK_WDT_TIMEOUT_S=35` in `crates/sonde-modem/sdkconfig.defaults`).
- The main loop feeds the watchdog on each iteration.
- If the main loop stalls (e.g., deadlock, infinite loop), the watchdog triggers a hardware reset (`CONFIG_ESP_TASK_WDT_PANIC=y`).
- After reset, the firmware boots normally and sends `MODEM_READY`.

> **sdkconfig note (D9-6):** The root `sdkconfig.defaults.esp32s3` sets `CONFIG_ESP_TASK_WDT_TIMEOUT_S=10`, and the modem crate's `crates/sonde-modem/sdkconfig.defaults` sets it to 35. During the modem build, both files are passed to ESP-IDF via `ESP_IDF_SDKCONFIG_DEFAULTS` in the order `sdkconfig.defaults.esp32s3;crates/sonde-modem/sdkconfig.defaults`. ESP-IDF applies defaults files in list order, with later files overriding earlier ones, so the crate-specific value of 35 seconds takes precedence. The effective watchdog timeout for the modem is therefore 35 seconds.

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
| USB-CDC disconnection | Set `usb_connected = false`, discard ESP-NOW frames; resend `MODEM_READY` when a USB read detects the transition back to connected |
| ESP-NOW init failure | Panic → automatic reboot |
| WiFi init failure | Panic → automatic reboot |
| Channel set failure | Send `ERROR(CHANNEL_SET_FAILED)` to gateway |
| `SEND_FRAME` with body < 7 bytes | Silently discard (codec returns `BodyTooShort`, bridge continues) |
| `DISPLAY_FRAME` with body length != 1024 | Send `EVENT_ERROR(INVALID_FRAME)`; keep the previous display image |
| OLED I²C write failure | Send `EVENT_ERROR(DISPLAY_WRITE_FAILED)`; abort the current display flush |
| Serial frame `len` > 1025 | Decoder reset; gateway must send `RESET` to resync (modem-protocol.md §2.3) |
| Unknown serial message type | Silently discard |

> **Note:** WiFi and ESP-NOW initialization failures are treated as unrecoverable. The firmware uses `.expect()`, so a failed init call will panic. Early in boot this panic is handled directly by the panic handler (before the task watchdog is configured for the current task); later, if the main loop stalls, the task watchdog (configured with `trigger_panic: true`) will trigger a panic and reset. In both cases the observable behavior is an automatic reboot. Sending `ERROR` messages to the gateway before init completes is not possible because USB-CDC may not be ready.

---

## 14  Diagnostics

The modem uses the Rust `log` crate with the ESP-IDF logging backend (`EspLogger`). Diagnostic output is routed to **UART0** (the USB-UART bridge chip on most ESP32-S3 dev boards), **not** the native USB-CDC port.

This separation is critical: the USB-CDC port (GPIO19/20) carries the binary modem protocol exclusively. Mixing log text into the protocol stream would corrupt framing. The UART port is independent and can be monitored concurrently.

### 14.1  Dual-port setup

On a typical ESP32-S3-DevKitC-1 with two USB connectors:

| Port | Connector label | GPIO | Carries | Baud rate |
|------|----------------|------|---------|-----------|
| UART | "UART" or "COM" | 43/44 (via USB-UART bridge chip) | Diagnostic logs (`log::info!`, panics) | 115200 |
| USB | "USB" or "OTG" | 19/20 (native USB peripheral) | Binary modem protocol (gateway link) | N/A (USB full-speed) |

Connect both ports to the host. Use `idf.py monitor` (or any serial terminal at 115200 baud) on the UART port to observe boot messages, state transitions, and warnings.

### 14.2  Log levels

| Level | Usage |
|-------|-------|
| `info!` | Startup, channel changes, RESET, MODEM_READY sent, ESP-NOW init, ESP-NOW frame received/sent, BLE connect/disconnect, BLE advertising start/stop, BLE GATT writes, BLE pairing events |
| `debug!` | USB-CDC serial messages sent/received |
| `warn!` | USB write errors, ESP-NOW send failures, peer add failures, encode errors, BLE pairing failures |

The default log level is INFO (`sdkconfig.defaults`: `CONFIG_LOG_DEFAULT_LEVEL_INFO`). In debug builds, the maximum compiled-in level is TRACE (all Rust `log` levels are included), and the modem sets the Rust `log` facade's runtime maximum via `log::set_max_level(...)`. ESP-IDF's `esp_log_level_set()` may further restrict output below that runtime maximum but cannot raise it. In release builds without the `verbose` feature, the compile-time maximum is WARN (see §14.2a).

### 14.2a  Build-type–aware log levels (MD-0505)

The modem applies the same build-type–aware policy as the node (see ND-1012) to eliminate logging overhead in release firmware builds. Two mutually exclusive Cargo features control the release compile-time max level.

**Compile-time filtering:**

| Build profile | Cargo feature | Effect |
|---|---|---|
| `dev` (debug) | `max_level_trace` | All levels compiled in |
| `release` / `firmware` (quiet, default) | `quiet` → `log/release_max_level_warn` | `trace!`, `debug!`, and `info!` call-sites become no-ops |
| `release` / `firmware` (verbose) | `verbose` → `log/release_max_level_debug` | `trace!` call-sites become no-ops; `debug!` and `info!` remain compiled in |

A `compile_error!` fires if both `quiet` and `verbose` are enabled. To build verbose firmware: `--features esp,verbose --no-default-features`.

**Runtime default:**

After `EspLogger::initialize_default()`, the modem binary sets the runtime level based on build type and feature:

```rust
#[cfg(any(debug_assertions, feature = "verbose"))]
log::set_max_level(log::LevelFilter::Info);
#[cfg(not(any(debug_assertions, feature = "verbose")))]
log::set_max_level(log::LevelFilter::Warn);
```

Debug and verbose builds default to INFO; release quiet builds default to WARN. Note: the effective runtime gate for Rust logs is `log::set_max_level(...)` as shown above; `esp_log_level_set()` may further restrict ESP-IDF tag output, but it cannot raise the level above this max or re-enable call-sites that were stripped at compile time.

### 14.3  Operational logging (MD-0500 – MD-0504)

The modem emits structured `log` macro calls at key operational boundaries to provide runtime visibility into radio, BLE, and USB-CDC activity. All logging uses the ESP-IDF `log` crate (`log::info!`, `log::debug!`, `log::warn!`) — **not** the `tracing` crate.

**ESP-NOW frames (MD-0500):** Each forwarded received frame logs peer MAC, payload length, and RSSI at INFO level. Each outgoing send logs peer MAC, payload length, and send result (success/failure) at INFO level, with failures additionally logged at WARN.

**BLE lifecycle (MD-0501):** Connection, disconnection, advertising start, and advertising stop are logged at INFO level with relevant metadata (peer address, MTU, HCI reason code).

**BLE GATT writes (MD-0502):** Authenticated writes, pre-auth buffered writes, and post-auth flush events are logged at INFO level with payload length and authentication state.

**USB-CDC messages (MD-0503):** Sent and received serial messages are logged at DEBUG level with message type and length. This keeps the default INFO output clean while allowing detailed relay tracing when DEBUG is enabled.

**BLE pairing (MD-0504):** Server-initiated LESC security, authentication success, and authentication failure are logged at INFO/WARN level as appropriate.

### 14.3a  Error diagnostic observability (MD-0506)

When the modem encounters an error at an operator-visible boundary, the error log includes: (1) the failed operation name, (2) non-sensitive metadata, (3) the specific error from the underlying subsystem, and (4) actionable guidance where possible. Diagnostics never log raw BLE attribute or notification payload contents.

**Covered boundaries:**

| Boundary | Diagnostic fields |
|---|---|
| BLE indication failure | NimBLE error (debug string) |
| ESP-NOW send failure | target peer MAC address, payload length, success flag |
| USB-CDC I/O error | operation name, ESP-IDF error |

### 14.4  Configuration

The following `sdkconfig.defaults` entries control console routing:

```ini
CONFIG_ESP_CONSOLE_UART_DEFAULT=y
CONFIG_ESP_CONSOLE_UART_NUM=0
CONFIG_ESP_CONSOLE_UART_BAUDRATE=115200
```

### 14.5  Flash configuration

The ESP32-S3 modem firmware requires specific flash parameters in `sdkconfig.defaults.esp32s3` so that `elf2image` in CI uses matching values and the merged flash image boots correctly:

```ini
CONFIG_ESPTOOLPY_FLASHMODE_DIO=y
CONFIG_ESPTOOLPY_FLASHFREQ_80M=y
CONFIG_ESPTOOLPY_FLASHSIZE_16MB=y
```

`CONFIG_ESPTOOLPY_FLASHMODE_DIO=y` selects Dual I/O (DIO) SPI mode. DIO uses 2 data lines for both address and data phases and is widely compatible across flash chips found on ESP32-S3 modules. It is more conservative than QIO (Quad I/O) and avoids pin-multiplexing issues on boards that do not route all four QSPI data lines.

`CONFIG_ESPTOOLPY_FLASHFREQ_80M=y` sets the SPI flash clock to 80 MHz, which is the maximum supported by the ESP32-S3 in DIO mode and improves firmware load performance.

`CONFIG_ESPTOOLPY_FLASHSIZE_16MB=y` declares the installed flash capacity. This must match the actual hardware. The partition table is sized accordingly; using a mismatched value causes the bootloader to reject the partition table at boot.

---

## 15  BLE pairing relay

The modem hosts the BLE Gateway Pairing Service, enabling phones to discover and pair with the gateway over Bluetooth Low Energy. The modem acts as an opaque relay — it forwards BLE messages to/from the gateway via dedicated serial message types (`BLE_RECV`, `BLE_INDICATE`) without interpreting payload contents.

### 15.1  GATT service setup

The modem registers a single GATT service using the NimBLE stack (MD-0400):

- **Service UUID:** `0000FE60-0000-1000-8000-00805F9B34FB` (Gateway Pairing Service)
- **Characteristic UUID:** `0000FE61-0000-1000-8000-00805F9B34FB` (Gateway Command)
- **Properties:** Write + Indicate

The characteristic supports Write for phone→gateway messages and Indicate (with ATT Handle Value Confirmation) for gateway→phone messages (MD-0401).

### 15.2  LESC pairing

The modem uses BLE LESC Numeric Comparison as the default pairing method (MD-0402, MD-0404). The modem proactively initiates LESC pairing from the server side by calling `ble_gap_security_initiate(conn_handle)` in the `on_connect` callback (MD-0404 criterion 5). This sends an SMP Security Request to the client, ensuring pairing is triggered regardless of client behavior. During pairing:

1. The `on_connect` callback calls `ble_gap_security_initiate(conn_handle)` to start the SMP exchange.
2. The NimBLE stack generates a 6-digit passkey.
3. The modem sends `BLE_PAIRING_CONFIRM` to the gateway with the passkey.
4. The BLE stack proceeds with LESC key exchange immediately (see D9-5 below — `on_confirm_pin` cannot block). The modem then waits for `BLE_PAIRING_CONFIRM_REPLY` — accept (`0x01`) or reject (`0x00`) — before setting the `authenticated` flag and emitting `BLE_CONNECTED`.
5. If no reply arrives within 30 seconds, the modem rejects the pairing (MD-0414).
6. On successful pairing and operator acceptance, the link is encrypted and `BLE_CONNECTED` is sent (MD-0410).

Just Works remains available as a fallback when the phone does not support Numeric Comparison (MD-0404).

> **Tentative accept model (D9-5):** NimBLE's `on_confirm_pin` callback is synchronous — it requires an immediate yes/no return and cannot block waiting for the gateway's asynchronous `BLE_PAIRING_CONFIRM_REPLY`. The modem returns `true` to let the BLE stack proceed with LESC key exchange immediately, then relays the passkey to the gateway for operator verification. This means the encrypted link is established *before* operator approval. Multiple mitigations bound the security impact: (1) `BleEvent::Connected` is deferred until the operator accepts, (2) GATT writes are gated on the `authenticated` flag (see § 15.2.1 below), (3) NVS bond persistence is disabled (`CONFIG_BT_NIMBLE_NVS_PERSIST=n`), and (4) the client is disconnected immediately on rejection.

#### 15.2.1  Write gating on `authenticated` flag (D9-4)

GATT writes to the Gateway Command characteristic are gated on the `authenticated` flag in `BleState` (MD-0402, MD-0414). The flag is `false` at connection time and only set to `true` after LESC pairing completes *and* the operator accepts the Numeric Comparison passkey.

Because the modem initiates LESC pairing server-side in `on_connect` (MD-0404 criterion 5), clients may send their first GATT write (e.g. `REQUEST_GW_INFO`) before the SMP handshake and operator confirmation complete. Rather than silently discarding such writes, the modem buffers **one** pre-authentication write in `BleState::pending_write`. When `authenticated` becomes `true`, the buffered write is flushed to the event queue as a `BleEvent::Recv` immediately before the deferred `BleEvent::Connected`. This ensures the gateway receives the write without requiring the client to retry. The buffer is cleared on disconnect.

### 15.3  ATT MTU and indication pacing

The modem negotiates ATT MTU ≥ 247 with the connecting client. If the negotiated MTU is below 247, the modem disconnects the client (MD-0402).

When sending indications larger than (MTU − 3) bytes, the modem fragments the message into chunks of at most (MTU − 3) bytes and sends each chunk as a separate ATT indication (MD-0403). The modem MUST wait for an ATT Handle Value Confirmation before sending the next chunk. Messages from different `BLE_INDICATE` commands are never interleaved.

### 15.4  BLE connection lifecycle

Only one BLE client may be connected at a time (MD-0405). On connection:

- The modem sends `BLE_CONNECTED` (0xA1) with the peer BLE address and negotiated MTU (MD-0410).

On disconnection:

- The modem sends `BLE_DISCONNECTED` (0xA2) with the peer address and HCI reason code (MD-0411).
- All GATT state is cleaned up; subsequent connections start fresh (MD-0405).

> **Reason code approximation (D10-4):** NimBLE's `on_disconnect` callback provides a `BLEError` result, but the wrapper does not expose a public accessor for the raw HCI reason code. The modem maps `Ok(())` to `0x16` (`BLE_ERR_CONN_TERM_LOCAL`) and any `Err(_)` to `0x13` (`BLE_ERR_REM_USER_CONN_TERM`) as a best-effort default. This means the exact HCI reason code reported in `BLE_DISCONNECTED` may not match the actual reason. This is a NimBLE Rust binding limitation.

The modem enforces a 60-second idle timeout on BLE connections (MD-0415). A timer starts when a client connects. If no BLE pairing procedure is initiated within 60 seconds, the modem disconnects the client, sends `BLE_DISCONNECTED`, and resumes advertising (if enabled). Once Numeric Comparison or passkey confirmation has started, the separate 30-second pairing timeout defined in MD-0414 applies instead of the 60-second idle timeout. This prevents abandoned or malicious connections from blocking the single-client BLE slot indefinitely.

BLE pairing operations do not interfere with concurrent ESP-NOW radio operations (MD-0405).

### 15.5  Write Long reassembly

GATT Write Long (Prepare Write + Execute Write) payloads are reassembled by the NimBLE stack before the modem forwards the complete payload as a single `BLE_RECV` (0xA0) serial message (MD-0409). Empty GATT writes are silently discarded.

### 15.6  Serial message flow

Two serial message types carry BLE traffic:

| Direction | Type | Code | Description |
|-----------|------|------|-------------|
| Gateway → modem | `BLE_INDICATE` | 0x20 | Delivers `ble_data` as a GATT indication to the connected phone (MD-0408) |
| Modem → gateway | `BLE_RECV` | 0xA0 | Forwards a GATT write from the phone to the gateway (MD-0409) |

If no BLE client is connected when `BLE_INDICATE` arrives, the message is silently discarded. Empty `BLE_INDICATE` frames (no `ble_data`) are also silently discarded (MD-0408).

### 15.7  Advertising control

BLE advertising is **off by default** after boot and after `RESET` (MD-0407, MD-0412). The gateway controls advertising via:

- `BLE_ENABLE` — starts advertising the Gateway Pairing Service UUID so phones can discover the modem (MD-0413).
- `BLE_DISABLE` — stops advertising and disconnects any active BLE client, triggering `BLE_DISCONNECTED` (MD-0413).

Both commands are idempotent. When no client is connected and BLE is enabled, the modem advertises continuously.

### 15.8  Disconnection cleanup

On BLE client disconnection (MD-0405, MD-0413, MD-0414):

1. Pending indication fragments are discarded.
2. GATT characteristic state is reset.
3. `BLE_DISCONNECTED` is sent to the gateway (MD-0411).
4. If BLE is still enabled, advertising resumes (MD-0407).

On `BLE_DISABLE`, the modem disconnects the active client (if any) and stops advertising. On `RESET`, BLE is disabled as part of the full reinitialisation sequence.

> **`advertise_on_disconnect` interaction (D10-5):** During GATT server initialization the modem calls `ble_server.advertise_on_disconnect(true)` so that advertising resumes automatically after a client disconnect (MD-0407). This is a NimBLE stack-level setting that persists for the lifetime of the BLE server. When `BLE_DISABLE` is received, the modem explicitly stops advertising and clears its internal `advertising` flag, but does *not* clear the stack-level `advertise_on_disconnect` policy. If `disable()` disconnects an active client while `advertise_on_disconnect(true)` is still in effect, NimBLE may restart advertising in response to that disconnect and keep it enabled even though the gateway has issued `BLE_DISABLE`. Implementations **must** provide a mitigation in code (for example, clearing `advertise_on_disconnect` before initiating the disconnect, or ensuring that the post-disconnect path performs a final `ble_advertising.stop()` when BLE is disabled) so that advertising is guaranteed to remain off after `BLE_DISABLE`, as required by MD-0407/MD-0413.

---

## 16  Button scanner

The modem detects button presses on the 1-Wire data line (GPIO2 / XIAO ESP32-S3 silk label D1) and emits `EVENT_BUTTON` messages to the gateway. The modem does not interpret button semantics.

### 16.1  GPIO configuration

At boot, GPIO2 is configured as an input pin with active-low logic. The firmware unconditionally enables the ESP32-S3 internal pull-up; this is safe alongside the carrier board's external pull-up (parallel pull-ups simply lower the effective resistance).

### 16.2  Debounce and classification state machine

The button scanner is a polling-based state machine called once per main-loop iteration. It uses `Instant::now()` for timing (ESP-IDF monotonic clock).

```
         ┌──────────┐
         │  Idle    │◄──────── GPIO HIGH (not pressed)
         │          │
         └────┬─────┘
              │ GPIO LOW detected
              ▼
         ┌──────────┐
         │ Debounce │  wait 30 ms with GPIO continuously LOW
         │ Press    │
         └────┬─────┘
              │ 30 ms elapsed, still LOW
              ▼
         ┌──────────┐
         │ Pressed  │  record press_start = now()
         │          │
         └────┬─────┘
              │ GPIO HIGH detected (release)
              ▼
         ┌──────────┐
         │ Debounce │  wait 30 ms with GPIO continuously HIGH
         │ Release  │
         └────┬─────┘
              │ 30 ms elapsed, still HIGH
              ▼
         ┌──────────┐
         │ Classify │  duration = now() - press_start
         │ & Emit   │  < 1 s → BUTTON_SHORT (0x00)
         │          │  ≥ 1 s → BUTTON_LONG  (0x01)
         └────┬─────┘
              │ EVENT_BUTTON emitted
              ▼
         ┌──────────┐
         │  Idle    │
         └──────────┘
```

If during the debounce-press phase the GPIO returns HIGH before 30 ms elapse, the state machine resets to Idle (glitch rejected). Similarly, if during debounce-release the GPIO returns LOW before 30 ms elapse, the state machine returns to Pressed (bounce rejected).

### 16.3  Module structure

The button scanner is split into two layers:

- **`ButtonScanner`** (platform-independent, in `bridge.rs` or a new `button.rs` module): Implements the state machine and classification logic. Generic over a GPIO read function. Testable on the host.
- **ESP GPIO wrapper** (ESP-IDF, in `bin/modem.rs`): Configures GPIO2 via `esp-idf-hal` and provides the read function to `ButtonScanner`.

### 16.4  Integration with Bridge

The `ButtonScanner::poll()` method returns `Option<u8>` — `Some(0x00)` for BUTTON_SHORT, `Some(0x01)` for BUTTON_LONG, or `None` if no event occurred. The bridge calls `poll()` each main-loop iteration and, when `Some(button_type)` is returned, encodes and sends an `EVENT_BUTTON` frame over USB-CDC.

### 16.5  Non-interference

Button polling is a single non-blocking GPIO read per main-loop iteration — no interrupts, no dedicated FreeRTOS task, no blocking waits. The polling cost is negligible compared to the existing USB and ESP-NOW processing in the loop.
