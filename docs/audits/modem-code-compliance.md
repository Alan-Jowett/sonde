<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Firmware — Code Compliance Audit

> **Audit type:** D8/D9/D10 forward + backward traceability
> **Spec baseline:** `modem-requirements.md` (27 active requirements, MD-0100 – MD-0414)
> **Code snapshot:** `crates/sonde-modem/src/` (bridge.rs, ble.rs, espnow.rs, usb\_cdc.rs, peer\_table.rs, status.rs, bin/modem.rs)
> **Protocol codec:** `crates/sonde-protocol/src/modem.rs`
> **Date:** 2026-03-20

---

## 1  Definitions

| Code | Meaning |
|------|---------|
| **D8** | Requirement exists in spec but **no implementing code found** |
| **D9** | Code behaviour exists but **no backing requirement** |
| **D10** | Requirement and code both exist but **constraint violated** or **acceptance criterion not met** |

---

## 2  Forward Traceability (Spec → Code)

### 2.1  USB-CDC Interface (MD-0100 – MD-0104)

| Req | Title | Pri | Verdict | Evidence |
|-----|-------|-----|---------|----------|
| MD-0100 | USB-CDC device presentation | Must | ✅ Pass | `usb_cdc.rs:UsbCdcDriver::new()` — initializes USB-CDC ACM via `UsbSerialDriver` on GPIO 19/20. `sdkconfig.defaults.esp32s3` routes console to UART0, keeping CDC for protocol only. |
| MD-0101 | Serial framing compliance | Must | ✅ Pass | Shared codec in `sonde-protocol::modem`. `FrameDecoder` enforces LEN‖TYPE‖BODY envelope. `bridge.rs:poll()` dispatches decoded messages; unknown types hit `ModemMessage::Unknown { .. } => {}` (line 291). Tests: `serial_framing_valid_frame_and_max_length`, `unknown_type_silently_discarded`. |
| MD-0102 | Maximum frame size | Must | ✅ Pass | `SERIAL_MAX_LEN = 512`. Decoder returns `FrameTooLarge` for len > 512. `bridge.rs` resets decoder on `FrameTooLarge` (lines 215-222). Tests: `serial_framing_oversized_len`, `framing_error_recovery_via_reset`. |
| MD-0103 | Unknown message types | Must | ✅ Pass | Codec returns `ModemMessage::Unknown`. Bridge dispatch silently discards. Test: `unknown_type_silently_discarded` verifies continued operation. |
| MD-0104 | Ready notification timing | Must | ✅ Pass | `bin/modem.rs` lines 96-105: retries `send_modem_ready()` in a loop for up to 2 seconds, breaking on USB connect or timeout. |

### 2.2  ESP-NOW Interface (MD-0200 – MD-0209)

| Req | Title | Pri | Verdict | Evidence |
|-----|-------|-----|---------|----------|
| MD-0200 | ESP-NOW initialization | Must | ✅ Pass | `espnow.rs:EspNowDriver::new()` — WiFi station mode via `BlockingWifi`, `EspNow::take()`, recv/send callbacks registered, default channel 1. |
| MD-0201 | Frame forwarding (radio → USB) | Must | ✅ Pass | `raw_recv_cb()` → `RxRing::push()` with peer\_mac, rssi, payload. `drain_one()` produces `RecvFrame`. `bridge.rs:poll()` sends `ModemMessage::RecvFrame`. Tests: `recv_frame_forwarded_to_serial`, `rx_count_incremented_on_forwarded_frame`. |
| MD-0202 | Frame transmission (USB → radio) | Must | ✅ Pass | `espnow.rs:send()` calls `counters.inc_tx()` then `espnow.send()`. Send callback increments `tx_fail_count` on `SendStatus::FAIL`. Tests: `send_frame_dispatched`. |
| MD-0203 | Automatic peer registration | Must | ✅ Pass | `espnow.rs:send()` calls `peer_table.ensure_peer()`. If peer is unknown, calls `espnow.add_peer()`. |
| MD-0204 | Peer table eviction | Should | ✅ Pass | `peer_table.rs:ensure_peer()` evicts LRU when `len >= MAX_PEERS` (20). `espnow.rs:send()` calls `espnow.del_peer(evicted)`. Tests: `lru_eviction`, `lru_respects_access_order`, `evicted_peer_can_be_readded`. |
| MD-0205 | Frame ordering | Must | ✅ Pass | `RxRing` is FIFO (head/tail). Bridge drains sequentially. No reordering in send path. Test: `multiple_recv_frames_forwarded_in_order`. |
| MD-0206 | Channel change | Must | ✅ Pass | `espnow.rs:set_channel()` validates 1-14, calls `esp_wifi_set_channel()`, clears all peers. `bridge.rs:handle_set_channel()` sends `SetChannelAck`. Test: `set_channel_ack`. |
| MD-0207 | Channel scanning | Must | ✅ Pass | `espnow.rs:scan_channels()` does `wifi.scan()`, aggregates per-channel AP count and strongest RSSI, returns 14 entries. Restores channel after scan. Test: `scan_channels_response`. |
| MD-0208 | SEND\_FRAME body validation | Must | ✅ Pass | Protocol codec enforces `SEND_FRAME_MIN_BODY_SIZE = 7` at decode time; returns `BodyTooShort`. Bridge silently discards (lines 224-229). `tx_count` is not incremented. Test: `send_frame_body_too_short_discarded`. |
| MD-0209 | SET\_CHANNEL error reporting | Must | ✅ Pass | `espnow.rs:set_channel()` returns `Err` for ch == 0 or ch > 14. `bridge.rs:handle_set_channel()` sends `ERROR(MODEM_ERR_CHANNEL_SET_FAILED)`. Test: `set_channel_invalid_returns_error`. |

### 2.3  Reliability and Reset (MD-0300 – MD-0303)

| Req | Title | Pri | Verdict | Evidence |
|-----|-------|-----|---------|----------|
| MD-0300 | Reset command | Must | ✅ Pass | `bridge.rs:handle_reset()` → `radio.reset_state()` (clear peers, channel 1, drain RX), `ble.disable()`, `counters.reset()`, `decoder.reset()`, `send_modem_ready()`. Tests: `reset_sends_modem_ready`, `reset_clears_counters`, `reset_clears_channel_to_default`, `repeated_reset_sends_modem_ready_each_time`. |
| MD-0301 | USB disconnection handling | Must | ✅ Pass | `usb_cdc.rs`: `AtomicBool` connected flag. `raw_recv_cb()` checks flag, discards ESP-NOW frames when USB disconnected. `usb_cdc.rs:read()` returns `reconnected = true` on state transition. `bridge.rs:poll()` sends `MODEM_READY` on reconnect and resets decoder. Tests: `usb_reconnect_triggers_modem_ready`, `usb_reconnect_clears_decoder_state`. |
| MD-0302 | Watchdog timer | Should | ✅ Pass | `bin/modem.rs` lines 80-92: `esp_task_wdt_reconfigure` with 10 s timeout, `trigger_panic: true`. Main loop feeds with `esp_task_wdt_reset()`. `sdkconfig.defaults.esp32s3`: `CONFIG_ESP_TASK_WDT_EN=y`, `CONFIG_ESP_TASK_WDT_TIMEOUT_S=10`. |
| MD-0303 | Status reporting | Must | ✅ Pass | `status.rs:ModemCounters` — `tx_count`, `rx_count`, `tx_fail_count` (atomics), `uptime_s` (Instant-based). `bridge.rs:handle_get_status()` reads all counters into `ModemStatus`. Reset zeros all counters and restarts uptime epoch. Tests: full unit test suite in `status.rs` + `get_status_response`, `status_reflects_tx_and_rx_counts`. |

### 2.4  BLE Pairing Relay (MD-0400 – MD-0414)

| Req | Title | Pri | Verdict | Evidence |
|-----|-------|-----|---------|----------|
| MD-0400 | Gateway Pairing Service | Must | ✅ Pass | `ble.rs`: `GATEWAY_SERVICE_UUID = 0xFE60`, `GATEWAY_COMMAND_UUID = 0xFE61`, characteristic created with `Write \| Indicate` properties (line 307-309). Write handler forwards as `BleEvent::Recv`; indications via `indicate()`. Tests: `ble_gatt_setup_via_enable_and_connect`, `ble_recv_forwarded_to_gateway`, `ble_indicate_dispatched`. |
| MD-0401 | BLE ↔ USB-CDC message relay | Must | ✅ Pass | GATT write → `BleEvent::Recv` → `BLE_RECV` serial msg. `BLE_INDICATE` → `ble.indicate()` → GATT indication. Opaque relay: modem doesn't inspect payload. Tests: `ble_write_to_usb_relay`, `usb_to_ble_indication_relay`, `ble_relay_round_trip`, `ble_opaque_relay_no_inspection`. |
| MD-0402 | ATT MTU negotiation | Must | ⚠️ **D10** | MTU enforcement exists: `on_authentication_complete` disconnects if `mtu < BLE_MTU_MIN` (247). Protocol codec also rejects `BleConnected` with mtu < 247 at encode time. **However**, no `CONFIG_BT_NIMBLE_ATT_PREFERRED_MTU` is set in sdkconfig — relies on NimBLE's implicit default (256). See §4.1. |
| MD-0403 | Indication fragmentation | Must | ⚠️ **D10** | Fragmentation implemented in `ble.rs:indicate()` — chunks of `(MTU − 3)` bytes, queued in `indication_queue`. One chunk sent per poll cycle via `advance_indication()`. **However**, the `awaiting_confirm` flag is a per-poll rate limiter, not tied to actual ATT Handle Value Confirmation. See §4.2. |
| MD-0404 | BLE LESC pairing | Must | ⚠️ **D10** | LESC configured: `AuthReq::all()`, `SecurityIOCap::DisplayYesNo`. `on_confirm_pin` relays passkey. Just Works path handled. **However**, `on_confirm_pin` always returns `true` immediately (accepting at BLE stack level before operator approval). See §4.3. |
| MD-0405 | BLE connection lifecycle | Must | ✅ Pass | `on_connect`: rejects second client (`connected_count() > 1`). `on_disconnect`: cleans up all state, emits `BleEvent::Disconnected`. Concurrent BLE + ESP-NOW verified. Test: `ble_and_espnow_concurrent`. |
| MD-0406 | *(Superseded)* | — | N/A | Superseded by MD-0410 and MD-0411. |
| MD-0407 | BLE advertising | Must | ✅ Pass | `BleState::new()` — `advertising: false`. `enable()` starts advertising with service UUID. `disable()` stops + disconnects. `advertise_on_disconnect(true)` for auto-re-advertise. Tests: `ble_enable_starts_advertising`, `ble_disable_stops_advertising`, `ble_disabled_after_reset`. |
| MD-0408 | BLE\_INDICATE relay | Must | ✅ Pass | `bridge.rs:dispatch()` routes to `handle_ble_indicate()` → `ble.indicate()`. Empty data discarded (codec rejects at decode time; `indicate()` also guards). No client → `mtu == 0` → silent discard. Tests: `ble_indicate_dispatched`, `ble_indicate_empty_body_silently_discarded`, `ble_indicate_no_ble_client_silent_discard`. |
| MD-0409 | BLE\_RECV forwarding | Must | ✅ Pass | `on_write` callback: empty writes discarded, non-empty → `BleEvent::Recv`. Bridge sends `BLE_RECV` serial message. Write Long reassembly by NimBLE. Tests: `ble_recv_forwarded_to_gateway`, `ble_write_to_usb_relay`. |
| MD-0410 | BLE\_CONNECTED notification | Must | ✅ Pass | `on_authentication_complete`: emits `BleEvent::Connected { peer_addr, mtu }` (immediately for Just Works, deferred for Numeric Comparison until operator confirms). Bridge sends `BLE_CONNECTED`. Tests: `ble_connected_forwarded_to_gateway`, `ble_mtu_negotiation_reported`. |
| MD-0411 | BLE\_DISCONNECTED notification | Must | ⚠️ **D10** | `on_disconnect` emits `BleEvent::Disconnected { peer_addr, reason }`. **However**, the HCI reason code is approximated as `0x16` (local) or `0x13` (remote) — not the actual HCI code. See §4.4. |
| MD-0412 | BLE advertising default off | Must | ✅ Pass | `BleState::new()` sets `advertising: false`. `handle_reset()` calls `ble.disable()`. `bin/modem.rs` initializes BLE before ESP-NOW, does not call `enable()`. Test: `ble_disabled_after_reset`. |
| MD-0413 | BLE\_ENABLE / BLE\_DISABLE | Must | ✅ Pass | `dispatch()` routes `BleEnable`/`BleDisable`. `enable()` configures and starts advertising. `disable()` stops advertising and disconnects any client. Idempotent: `enable()` reconfigures ads, `disable()` on stopped is safe. Tests: `ble_enable_starts_advertising`, `ble_disable_stops_advertising`. |
| MD-0414 | Numeric Comparison pin relay | Must | ⚠️ **D8** | Passkey relay: ✅ (`on_confirm_pin` → `BleEvent::PairingConfirm`). Accept/reject: ✅ (`pairing_confirm_reply()`). **30-second timeout: ❌ NOT IMPLEMENTED.** No timer exists to reject pairing if `BLE_PAIRING_CONFIRM_REPLY` doesn't arrive within 30 s. See §3.1. |

---

## 3  D8 Findings (Missing Implementation)

### 3.1  MD-0414 AC#4 — 30-second pairing timeout

**Requirement:** "If no reply is received within 30 seconds, the modem MUST reject the pairing."

**Finding:** No timer or timeout mechanism exists in `ble.rs` or `bridge.rs` for the Numeric Comparison confirm/reply flow. The `pairing_pending` flag is set in `on_confirm_pin` and only cleared by an explicit `BLE_PAIRING_CONFIRM_REPLY` or a disconnect event. If the gateway never replies, the modem holds the pairing in limbo indefinitely.

The bridge test `ble_pairing_confirm_no_auto_reply` explicitly confirms: *"timeout is BLE stack's job."* While NimBLE's SMP layer has its own pairing timeout (~30 s by default), MD-0414 places this responsibility on the modem firmware explicitly. The BLE stack timeout is coincidental, not a designed guarantee.

**Impact:** A lost or delayed `BLE_PAIRING_CONFIRM_REPLY` leaves the modem in a state where the encrypted link is established (due to the tentative accept) but the `authenticated` flag is never set, blocking GATT writes indefinitely. The BLE stack's SMP timeout may eventually clean up, but the modem doesn't enforce the 30 s deadline itself.

**Recommendation:** Add a monotonic timestamp when `pairing_pending` is set. In `bridge.rs:poll()`, check elapsed time and call `ble.pairing_confirm_reply(false)` if 30 s has passed without a reply.

---

## 4  D10 Findings (Constraint Violations)

### 4.1  MD-0402 — ATT preferred MTU not explicitly configured

**Requirement:** "The modem MUST negotiate ATT MTU ≥ 247 bytes."

**Finding:** The modem enforces MTU ≥ 247 post-negotiation (disconnects clients whose MTU is too low) but does not explicitly set the server-side preferred MTU. No `CONFIG_BT_NIMBLE_ATT_PREFERRED_MTU` appears in `crates/sonde-modem/sdkconfig.defaults`. NimBLE's compiled-in default is 256, which satisfies the threshold, but the project's own coding guidelines state: *"sdkconfig.defaults must explicitly set every value the code depends on."*

**Impact:** If NimBLE's default changes in a future ESP-IDF update, the modem could silently start disconnecting all BLE clients without an obvious configuration entry to diagnose.

**Recommendation:** Add `CONFIG_BT_NIMBLE_ATT_PREFERRED_MTU=512` to `crates/sonde-modem/sdkconfig.defaults`.

### 4.2  MD-0403 AC#3 — Indication pacing not tied to ATT confirmation

**Requirement:** "The modem MUST wait for ATT Handle Value Confirmation before sending the next chunk."

**Finding:** `ble.rs:advance_indication()` uses an `awaiting_confirm` boolean as a per-poll-cycle rate limiter. The flag is set when a chunk is sent and unconditionally cleared on the next `advance_indication()` call (~1 ms later). There is no ATT Handle Value Confirmation callback registered; pacing relies on NimBLE's internal queue management via `ble_gatts_indicate_custom`.

**Mitigation:** NimBLE's `ble_gatts_indicate_custom` internally handles ATT confirmation pacing — it will not send the next queued indication until the previous one is confirmed. The modem's rate limiting adds a further 1 ms delay between chunks, which is conservative. In practice, the combination is likely correct.

**Residual risk:** Low. The spec letter says "MUST wait for confirmation" but the behaviour is equivalent. However, a future refactor that replaces `notify_with` with a different API could silently break the confirmation guarantee if the awaiting\_confirm flag is assumed to provide it.

### 4.3  MD-0404/MD-0414 — Numeric Comparison accepted before operator approval

**Requirement:** "The modem … waits for `BLE_PAIRING_CONFIRM_REPLY` before accepting or rejecting the pairing."

**Finding:** NimBLE's `on_confirm_pin` callback requires a synchronous boolean return value and cannot block waiting for the gateway's asynchronous reply. The modem returns `true` immediately, allowing the BLE stack to complete the LESC key exchange and establish the encrypted link before the operator has confirmed the passkey.

**Mitigations (documented in code):**
1. `BleEvent::Connected` is deferred until operator accepts.
2. GATT writes are gated on the `authenticated` flag.
3. NVS bond persistence is disabled (`CONFIG_BT_NIMBLE_NVS_PERSIST=n`).
4. On rejection, the client is disconnected immediately.

**Residual risk:** Medium. An attacker who MITMs the Numeric Comparison could send data over the encrypted link before the operator rejects. The write-gating mitigates application-layer data relay but the encrypted BLE link itself is active. This is an architectural limitation of NimBLE's synchronous callback model, documented in the code comments.

### 4.4  MD-0411 — Disconnect reason code is approximated

**Requirement:** "The modem MUST send `BLE_DISCONNECTED` … containing the peer BLE address and HCI disconnect reason code."

**Finding:** `ble.rs:on_disconnect` (lines 190-196) maps the reason to one of two fixed values:
- `0x16` (`BLE_ERR_CONN_TERM_LOCAL`) if `reason.is_ok()`
- `0x13` (`BLE_ERR_REM_USER_CONN_TERM`) if `reason.is_err()`

The `esp32-nimble` crate's `BLEError` wraps the raw NimBLE error code but does not expose a public accessor to extract the actual HCI reason code. The modem cannot pass through the true reason.

**Impact:** Low. The gateway receives a coarse disconnect reason instead of the specific HCI code. Diagnostic precision is reduced for edge cases (e.g., link loss vs. supervision timeout vs. authentication failure).

**Recommendation:** File an upstream issue or PR against `esp32-nimble` to expose the raw error code from `BLEError`. Once available, replace the fixed mapping with the actual HCI code.

### 4.5  MD-0407/MD-0413 — `advertise_on_disconnect(true)` may conflict with `BLE_DISABLE`

**Requirement:** "`BLE_DISABLE` stops advertising and disconnects any active BLE client."

**Finding:** `ble.rs:new()` (line 151) sets `ble_server.advertise_on_disconnect(true)`, which tells NimBLE to automatically restart advertising after any disconnection. In `disable()`, the modem first stops advertising, then disconnects the client. NimBLE's auto-re-advertise handler may fire on the disconnect event and restart advertising, defeating the `stop()` call.

**Execution order in `disable()`:**
1. `ble_advertising.lock().stop()` → advertising stops
2. `server.disconnect(handle)` → triggers NimBLE disconnect event
3. NimBLE disconnect handler → may call `ble_gap_adv_start()` (auto re-advertise)
4. Modem sets `s.advertising = false` → state flag is wrong if NimBLE restarted ads

**Impact:** Medium. After `BLE_DISABLE` with an active client, the modem may silently resume BLE advertising, violating MD-0407 and MD-0413.

**Recommendation:** Disable `advertise_on_disconnect` in `disable()` before disconnecting the client, or call `stop()` again after `disconnect()` returns. Alternatively, set `advertise_on_disconnect(false)` globally and manually restart advertising in the `on_disconnect` callback only when `s.advertising` is true.

---

## 5  D9 Findings (Undocumented Behaviour)

### 5.1  RX ring buffer capacity and silent frame drops

**Code:** `espnow.rs` — `RX_RING_CAP = 16`. `raw_recv_cb()` drops frames when the ring is full (`drop_count` incremented) or when the mutex is contended (`contention_drops` incremented).

**Observation:** No requirement covers the radio-side receive buffer depth or the silent-drop behavior. Under sustained burst traffic (>16 frames between polls), frames are silently lost. The modem logs warnings for full-drops and contention-drops, but the gateway has no visibility into these losses.

### 5.2  Per-poll processing caps

**Code:** `bridge.rs` — `MAX_RX_FRAMES_PER_POLL = 16`, `MAX_BLE_EVENTS_PER_POLL = 16`.

**Observation:** These caps prevent starvation of the serial decode path under burst traffic but introduce a throughput ceiling that is not specified in any requirement. Remaining frames are processed on subsequent poll iterations.

### 5.3  BLE event and indication queue limits

**Code:** `ble.rs` — `MAX_BLE_EVENT_QUEUE = 32`, `MAX_INDICATION_CHUNKS = 64`.

**Observation:** Events exceeding 32 entries and indication payloads requiring >64 chunks are silently dropped with a warning log. No requirement specifies these limits. Large indication payloads (>64 × (MTU−3) ≈ 15 KB) would be silently truncated.

### 5.4  GATT write gating on `authenticated` flag

**Code:** `ble.rs:on_write` (lines 322-329) — GATT writes are silently rejected with a warning if `s.authenticated` is false.

**Observation:** MD-0409 says "the modem MUST forward the complete reassembled write payload" without conditioning on authentication state. The write-gating is a security measure supporting MD-0414's deferred acceptance, but this interaction is not documented in the requirements. A phone that writes before the operator confirms will have its data silently dropped.

### 5.5  Tentative LESC accept model

**Code:** `ble.rs:on_confirm_pin` (line 252) — always returns `true`.

**Observation:** The encrypted BLE link is established before operator approval. This architectural decision is well-documented in code comments with four explicit mitigations, but it is not described in the requirements or design spec as an accepted deviation. The design doc §15.2 states the modem "waits for `BLE_PAIRING_CONFIRM_REPLY`" without mentioning the tentative accept.

### 5.6  Watchdog timeout discrepancy in sdkconfig

**Code:** `crates/sonde-modem/sdkconfig.defaults` line 26: `CONFIG_ESP_TASK_WDT_TIMEOUT_S=35`. `sdkconfig.defaults.esp32s3` line 24: `CONFIG_ESP_TASK_WDT_TIMEOUT_S=10`. `bin/modem.rs` line 82: runtime reconfiguration to 10,000 ms.

**Observation:** The crate-level sdkconfig specifies a 35 s watchdog timeout while the workspace-level file and the runtime code both use 10 s. The runtime call overrides the sdkconfig value, so behavior is correct, but the crate-level value is misleading.

---

## 6  Coverage Metrics

### 6.1  Requirement coverage

| Category | Total | Pass | D8 | D10 | Coverage |
|----------|-------|------|----|-----|----------|
| USB-CDC (MD-01xx) | 5 | 5 | 0 | 0 | 100% |
| ESP-NOW (MD-02xx) | 9 | 9 | 0 | 0 | 100% |
| Reliability (MD-03xx) | 4 | 4 | 0 | 0 | 100% |
| BLE pairing (MD-04xx) | 13* | 8 | 1 | 4 | 62% |
| **Total** | **27** | **22** | **1** | **4** | **81%** |

\* MD-0406 excluded (superseded). Four D10 findings affect five requirements (MD-0402, MD-0403, MD-0404/MD-0414, MD-0411, MD-0407/MD-0413).

### 6.2  Priority breakdown

| Priority | Total | Fully compliant | Gaps |
|----------|-------|-----------------|------|
| Must | 25 | 20 | 5 |
| Should | 2 | 2 | 0 |

### 6.3  Test coverage (bridge.rs unit tests)

The bridge module contains **36 unit tests** covering:
- Serial framing: valid frames, max length, oversized length, unknown types, framing error recovery
- Radio bridging: send, receive, ordering, counters, channel change, scanning
- Reset: counter clearing, channel reset, repeated resets, USB reconnect
- BLE relay: enable/disable, indicate, recv, connected/disconnected, pairing confirm/reply, round-trip, concurrent ESP-NOW + BLE, MTU enforcement, empty body handling

---

## 7  Summary of Findings

| ID | Severity | Requirement | Finding |
|----|----------|-------------|---------|
| D8-1 | **High** | MD-0414 AC#4 | 30-second pairing timeout not implemented |
| D10-1 | Medium | MD-0402 | ATT preferred MTU not explicitly configured in sdkconfig |
| D10-2 | Low | MD-0403 AC#3 | Indication pacing relies on NimBLE internals, not explicit ATT confirmation callback |
| D10-3 | Medium | MD-0404/MD-0414 | Numeric Comparison accepted at BLE stack level before operator approval |
| D10-4 | Low | MD-0411 | HCI disconnect reason code approximated (0x16/0x13 only) |
| D10-5 | Medium | MD-0407/MD-0413 | `advertise_on_disconnect(true)` may restart advertising after `BLE_DISABLE` |
| D9-1 | Info | — | RX ring buffer silent drops (capacity 16, contention drops) |
| D9-2 | Info | — | Per-poll processing caps (16 frames, 16 BLE events) |
| D9-3 | Info | — | BLE event/indication queue limits (32 events, 64 chunks) |
| D9-4 | Info | — | GATT writes gated on `authenticated` flag (not in MD-0409) |
| D9-5 | Info | — | Tentative LESC accept model not documented in spec |
| D9-6 | Info | — | Watchdog sdkconfig discrepancy (35 s vs 10 s) |

---

## 8  Recommended Actions (Priority Order)

1. **D8-1:** Implement 30 s pairing timeout timer in bridge/BLE layer.
2. **D10-5:** Fix `advertise_on_disconnect` interaction with `BLE_DISABLE`.
3. **D10-3:** Document the tentative accept model in the design spec as an accepted deviation, or implement a FreeRTOS task-based approach to block until operator reply.
4. **D10-1:** Add `CONFIG_BT_NIMBLE_ATT_PREFERRED_MTU=512` to `sdkconfig.defaults.esp32s3`.
5. **D10-4:** File upstream issue against `esp32-nimble` for raw HCI error code access.
6. **D9-6:** Align crate-level `sdkconfig.defaults` watchdog timeout to 10 s.
