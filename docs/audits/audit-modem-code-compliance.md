# Sonde Code Compliance Audit — Investigation Report

**Crate:** `sonde-modem`
**Date:** 2025-07-18

---

## 1. Executive Summary

The sonde-modem firmware implementation demonstrates **strong alignment** with its requirements specification (27 of 28 active MD- requirements implemented). No critical gaps were found. The primary findings are: (1) the design doc's serial codec dispatch table (§5.2) omits BLE message types that the code handles and other design sections describe, (2) a 60-second BLE idle connection timeout exists in code with no tracing requirement, (3) the watchdog timeout value in the design doc (35 s) does not match the runtime-configured value (10 s) in the firmware entry point, and (4) the D10-5 `advertise_on_disconnect` race condition is mitigated but the mitigation has a timing window. Seven findings total — one High, three Medium, three Low.

---

## 2. Problem Statement

This audit checks whether the `sonde-modem` crate's source code faithfully implements the modem firmware requirements (modem-requirements.md), as designed (modem-design.md), and whether the code contains behavior not covered by the specification. The primary concern is **backward traceability** — finding undocumented code behavior (D9 findings). Forward traceability (D8) and constraint verification (D10) are also performed.

---

## 3. Investigation Scope

- **Codebase / components examined:**
  - `crates/sonde-modem/src/lib.rs` — module structure
  - `crates/sonde-modem/src/bridge.rs` — bridge logic, dispatch, `SerialPort`/`Radio`/`Ble` traits, 2685 lines (incl. ~2100 lines of tests)
  - `crates/sonde-modem/src/usb_cdc.rs` — USB-CDC driver (107 lines)
  - `crates/sonde-modem/src/espnow.rs` — ESP-NOW driver (451 lines)
  - `crates/sonde-modem/src/peer_table.rs` — peer table with LRU eviction (200 lines)
  - `crates/sonde-modem/src/status.rs` — counters and uptime (206 lines)
  - `crates/sonde-modem/src/ble.rs` — BLE GATT server driver (729 lines)
  - `crates/sonde-modem/src/bin/modem.rs` — firmware entry point (122 lines)
  - `crates/sonde-modem/sdkconfig.defaults` — modem sdkconfig
  - `sdkconfig.defaults.esp32s3` — chip-level sdkconfig
- **Specification documents:**
  - `docs/modem-requirements.md` — 28 active requirements (MD-0406 superseded)
  - `docs/modem-design.md` — 15 sections + 6 D9/D10 self-identified drift items
  - `docs/modem-validation.md` — 47 test cases
- **Tools used:** Static analysis via manual code review, grep, file inspection
- **Limitations:**
  - ESP-IDF platform APIs (USB-CDC, WiFi, NimBLE) cannot be exercised statically. Behavior that depends on runtime ESP-IDF callbacks (e.g., actual DTR detection, NimBLE `on_connect` timing) is assessed structurally.
  - The `sonde-protocol::modem` codec is not in scope (separate crate); its correctness is assumed.

---

## 4. Findings

### Finding F-001: Design doc §5.2 dispatch table omits BLE message types

- **Severity:** Medium
- **Category:** D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location:** modem-design.md §5.2 "Inbound decoding" dispatch table (lines 112–119)
- **Code Location:** `crates/sonde-modem/src/bridge.rs:300-316` (`dispatch()` function)
- **Description:**
  The design doc's §5.2 dispatch table lists only five gateway→modem message types: `RESET` (0x01), `SEND_FRAME` (0x02), `SET_CHANNEL` (0x03), `GET_STATUS` (0x04), `SCAN_CHANNELS` (0x05), plus an "Unknown" catch-all. The code's `dispatch()` function also handles four additional BLE message types: `BleIndicate` (0x20), `BleEnable` (0x21), `BleDisable` (0x22), and `BlePairingConfirmReply` (0x23). These BLE types ARE described in the design doc's §15 (BLE pairing relay) and in the requirements, but they are absent from the §5.2 dispatch table that is the canonical reference for inbound message routing.
- **Evidence:**
  Design doc §5.2 table (line 112–119):
  ```
  | Type | Handler |
  | 0x01 RESET         | → handle_reset()           |
  | 0x02 SEND_FRAME    | → handle_send_frame(body)  |
  | 0x03 SET_CHANNEL   | → handle_set_channel(body) |
  | 0x04 GET_STATUS    | → handle_get_status()      |
  | 0x05 SCAN_CHANNELS | → handle_scan_channels()   |
  | Unknown            | → silently discard          |
  ```
  Code dispatch (bridge.rs:300-316):
  ```rust
  ModemMessage::BleIndicate(ind) => self.handle_ble_indicate(ind.ble_data),
  ModemMessage::BleEnable => self.handle_ble_enable(),
  ModemMessage::BleDisable => self.handle_ble_disable(),
  ModemMessage::BlePairingConfirmReply(reply) => self.handle_ble_pairing_confirm_reply(reply),
  ```
- **Impact:** A developer referencing only §5.2 to understand message routing would not know about BLE message dispatch. The table is incomplete as a specification artifact.
- **Remediation:** Add the four BLE message types (0x20, 0x21, 0x22, 0x23) to the §5.2 dispatch table.
- **Confidence:** High

---

### Finding F-002: BLE idle connection timeout (60 s) is undocumented

- **Severity:** Medium
- **Category:** D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location:** None — no matching requirement identified. Searched: modem-requirements.md (all MD- IDs), modem-design.md (all sections), modem-validation.md (all T- IDs). No mention of "idle timeout", "60 second", or "BLE_IDLE_TIMEOUT".
- **Code Location:** `crates/sonde-modem/src/ble.rs:67` (constant definition), `ble.rs:630-687` (`check_pairing_timeout()` method)
- **Description:**
  The BLE driver implements a 60-second idle timeout (`BLE_IDLE_TIMEOUT`) that disconnects BLE clients who connect but never initiate pairing. This prevents a client from holding the single-connection slot (MD-0405) indefinitely. No requirement or design section specifies this behavior; the 30-second pairing timeout (MD-0414) is a separate timer.
- **Evidence:**
  ```rust
  // ble.rs:67
  const BLE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
  ```
  ```rust
  // ble.rs:655-657 (inside check_pairing_timeout)
  } else if let Some(start) = s.connection_start {
      start.elapsed() >= BLE_IDLE_TIMEOUT
  }
  ```
  The modem-requirements.md MD-0414 specifies only a 30-second timeout for Numeric Comparison confirmation. The 60-second idle timeout is an independent timer not covered by any requirement.
- **Impact:** The undocumented timeout could disconnect a legitimate slow client. If the timeout value changes, no spec or test gates the behavior. It IS reasonable infrastructure supporting MD-0405 (single connection), but should be specified.
- **Remediation:** Add a requirement (e.g., MD-0415) or add to MD-0405 acceptance criteria: "Clients that connect but do not initiate pairing within 60 seconds are disconnected."
- **Confidence:** High

---

### Finding F-003: Watchdog timeout — design doc says 35 s, code configures 10 s at runtime

- **Severity:** Medium
- **Category:** D10_CONSTRAINT_VIOLATION_IN_CODE
- **Spec Location:** modem-design.md §11 (Watchdog, line 272–277, D9-6 note), modem-requirements.md MD-0302
- **Code Location:** `crates/sonde-modem/src/bin/modem.rs:79-92`
- **Description:**
  The design doc §11 states the watchdog timeout is 35 seconds (set via `CONFIG_ESP_TASK_WDT_TIMEOUT_S=35` in `crates/sonde-modem/sdkconfig.defaults`) and explains the reasoning: "The longer timeout accommodates BLE stack operations." However, `modem.rs` calls `esp_task_wdt_reconfigure()` at runtime with `timeout_ms: 10_000`, which **overrides** the sdkconfig value to 10 seconds. The code comment references "MD-0302" and 10 seconds, which matches the requirement ("10 second timeout"), but contradicts the design doc's stated 35-second value and its BLE-accommodation rationale.
- **Evidence:**
  Design doc §11 (D9-6 note):
  > "the modem crate's `crates/sonde-modem/sdkconfig.defaults` sets it to 35 ... the effective watchdog timeout for the modem is 35 seconds. The longer timeout accommodates BLE stack operations"

  Code (modem.rs:81-82):
  ```rust
  let wdt_config = esp_idf_sys::esp_task_wdt_config_t {
      timeout_ms: 10_000,
  ```
  The requirement MD-0302 says "10 second timeout". The sdkconfig sets 35 s. The runtime code overrides to 10 s.
- **Impact:** The design doc's rationale for a 35-second timeout (BLE operations can stall the main loop) suggests the 10-second runtime override may be too aggressive, potentially causing spurious watchdog resets during BLE pairing. Alternatively, the design doc is stale. Either way, the documents are inconsistent with each other and with the code.
- **Remediation:** Align the design doc and code. If 10 s is correct (per MD-0302), update design doc §11 and the D9-6 note to reflect the runtime reconfiguration. If 35 s is correct (per BLE rationale), update modem.rs and MD-0302.
- **Confidence:** High

---

### Finding F-004: Per-poll RX cap (MAX_RX_FRAMES_PER_POLL = 16) is undocumented in requirements

- **Severity:** Low
- **Category:** D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location:** None — no matching requirement identified. Searched: modem-requirements.md (all MD- IDs). The design doc §9 main-loop section (D9-2 note) describes this behavior but no requirement covers it.
- **Code Location:** `crates/sonde-modem/src/bridge.rs:30` (constant), `bridge.rs:256-266` (loop)
- **Description:**
  The bridge limits radio→USB forwarding to 16 frames per `poll()` call to prevent starvation. This is described in the design doc (§9, D9-2 note) and is reasonable infrastructure behavior, but no requirement specifies frame-forwarding rate limits. MD-0205 (frame ordering) requires ordered forwarding but does not mention batching or rate limits.
- **Evidence:**
  ```rust
  // bridge.rs:30
  const MAX_RX_FRAMES_PER_POLL: usize = 16;
  ```
  Design doc §9 (D9-2): "BLE events are drained up to `MAX_BLE_EVENTS_PER_POLL` (16)..."
- **Impact:** Low — this is reasonable infrastructure. A requirement could constrain the minimum throughput but the current behavior is a defensible design choice.
- **Remediation:** Acknowledge in requirements or design as a non-functional constraint, or leave as implementation detail. No immediate action needed.
- **Confidence:** High

---

### Finding F-005: BLE event queue and indication queue caps are undocumented in requirements

- **Severity:** Low
- **Category:** D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location:** None — no matching requirement identified. Searched: modem-requirements.md. The design doc §9 (D9-3 note) describes these limits.
- **Code Location:** `crates/sonde-modem/src/ble.rs:54` (`MAX_BLE_EVENT_QUEUE = 32`), `ble.rs:57` (`MAX_INDICATION_CHUNKS = 64`)
- **Description:**
  The BLE driver imposes bounded queue sizes: 32 events and 64 indication chunks. Events and indications exceeding these limits are silently dropped with warning logs. The design doc's D9-3 note describes this behavior, but no requirement specifies queue capacity constraints or documents the drop behavior.
- **Evidence:**
  ```rust
  const MAX_BLE_EVENT_QUEUE: usize = 32;
  const MAX_INDICATION_CHUNKS: usize = 64;
  ```
  Events dropped at `ble.rs:271` (disconnect event), `ble.rs:314` (pairing confirm), `ble.rs:401` (GATT write), `ble.rs:362` (connected event). Indications rejected at `ble.rs:546-551`.
- **Impact:** Low — these are embedded resource management decisions. However, the silent drop behavior could be surprising to gateway integrators who expect all BLE events to be delivered.
- **Remediation:** Document the queue limits and drop behavior in a new requirement or in MD-0401/MD-0408 acceptance criteria. The design doc's D9-3 note already describes it; elevate to requirements.
- **Confidence:** High

---

### Finding F-006: `advertise_on_disconnect` race window (D10-5) — mitigation present but has timing gap

- **Severity:** High
- **Category:** D10_CONSTRAINT_VIOLATION_IN_CODE
- **Spec Location:** modem-requirements.md MD-0407 AC4, MD-0413 AC2. modem-design.md §15.8 (D10-5 note)
- **Code Location:** `crates/sonde-modem/src/ble.rs:193-194` (advertise_on_disconnect set to true), `ble.rs:483-525` (disable method)
- **Description:**
  The design doc (D10-5 note) explicitly warns that `ble_server.advertise_on_disconnect(true)` (ble.rs:194) can cause advertising to restart after `BLE_DISABLE` if a disconnect occurs between the first `stop()` and the second `stop()`. The code mitigates this with a second `ble_advertising.stop()` call after disconnect (ble.rs:503), but there remains a **timing window**: the `disconnect()` call (ble.rs:497) is asynchronous — NimBLE may process it on a different task after the second `stop()` has already executed, causing `advertise_on_disconnect` to restart advertising with no further `stop()` to catch it.

  The design doc itself states: "Implementations **must** provide a mitigation in code (for example, clearing `advertise_on_disconnect` before initiating the disconnect)..." The code does NOT clear `advertise_on_disconnect` before disconnecting — it relies solely on a post-disconnect `stop()` which has a race.
- **Evidence:**
  ```rust
  // ble.rs:194 — set once during construction, never cleared
  ble_server.advertise_on_disconnect(true);
  ```
  ```rust
  // ble.rs:496-504 — disable() method
  if let Some(handle) = conn_handle {
      let _ = ble_device.get_server().disconnect(handle);
  }
  // Second stop to catch advertise_on_disconnect restart
  if let Err(e) = ble_advertising.lock().stop() { ... }
  ```
  The design doc's own D10-5 note suggests clearing `advertise_on_disconnect` before the disconnect, but the code does not implement this approach.
- **Impact:** If the timing race triggers, advertising remains active after `BLE_DISABLE`, violating MD-0407 ("BLE_DISABLE stops advertising") and MD-0413 AC2 ("BLE_DISABLE disconnects any active BLE client"). A phone could connect to a modem the gateway believes is BLE-disabled, creating a state mismatch.
- **Remediation:** Call `ble_server.advertise_on_disconnect(false)` before `disconnect()` in the `disable()` method, then restore it to `true` in the `enable()` method. This eliminates the race window entirely, as the design doc suggests.
- **Confidence:** Medium — the race is real but requires precise timing (NimBLE disconnect callback running between the two `stop()` calls). Testing on real hardware would confirm exploitability.

---

### Finding F-007: GATT write buffering (`pending_write`) — partially documented

- **Severity:** Low
- **Category:** D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location:** modem-requirements.md MD-0409 AC5 specifies buffering behavior. modem-design.md §15.2.1 (D9-4 note) describes the `authenticated` flag gating.
- **Code Location:** `crates/sonde-modem/src/ble.rs:128` (`pending_write` field), `ble.rs:404-413` (buffering in on_write), `ble.rs:357-359` and `ble.rs:580-584` (flush on auth complete)
- **Description:**
  MD-0409 AC5 was added to the requirements to cover pre-authentication write buffering, and the design doc §15.2.1 describes the mechanism. However, the **single-write limit** of the buffer is only implicitly documented in the design doc ("The buffer is cleared on disconnect") and not stated as a constraint in the requirement. If a client sends two writes before authentication, only the second is retained — the first is silently overwritten.
- **Evidence:**
  ```rust
  // ble.rs:413 — overwrites any previously buffered write
  s.pending_write = Some(value.to_vec());
  ```
  Design doc §15.2.1: "buffers **one** pre-authentication write" — this is documented in design but not in MD-0409 AC5 which says "buffer the write" without specifying the one-write limit.
- **Impact:** Low — the pairing protocol's design (client sends `REQUEST_GW_INFO`, waits for response) means sending more than one pre-auth write is abnormal. But the silent overwrite could cause subtle data loss in edge cases.
- **Remediation:** Add to MD-0409 AC5: "Only one pre-authentication write is buffered; subsequent pre-auth writes overwrite the buffer."
- **Confidence:** High

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|--------|-------|
| **Total requirements** | 28 active (MD-0406 superseded) |
| **Implemented** | 27 (96.4%) |
| **Partially implemented** | 0 |
| **Not implemented** | 0 |
| **Unverifiable** | 1 (MD-0100: USB-CDC enumeration — platform behavior, not verifiable via static analysis) |
| **D8 findings** | 0 |
| **D9 findings** | 5 (F-001, F-002, F-004, F-005, F-007) |
| **D10 findings** | 2 (F-003, F-006) |
| **Constraints verified** | 6 of 8 |
| **Constraints violated** | 1 (F-003 — watchdog timeout mismatch) |
| **Constraints with race condition** | 1 (F-006 — advertise_on_disconnect timing) |

### Requirement-by-Requirement Status

| REQ-ID | Status | Evidence |
|--------|--------|----------|
| MD-0100 | IMPLEMENTED | `usb_cdc.rs` — `UsbSerialDriver::new()` with CDC-ACM config |
| MD-0101 | IMPLEMENTED | `bridge.rs:229-250` — `FrameDecoder` from `sonde-protocol::modem` |
| MD-0102 | IMPLEMENTED | `bridge.rs:234-241` — `FrameTooLarge` triggers decoder reset |
| MD-0103 | IMPLEMENTED | `bridge.rs:313` — `ModemMessage::Unknown` silently discarded |
| MD-0104 | IMPLEMENTED | `modem.rs:96-105` — retry MODEM_READY for up to 2 seconds |
| MD-0200 | IMPLEMENTED | `espnow.rs:220-262` — WiFi station mode, ESP-NOW init, channel 1 |
| MD-0201 | IMPLEMENTED | `bridge.rs:256-266` — radio frames forwarded as `RecvFrame` |
| MD-0202 | IMPLEMENTED | `espnow.rs:297-317` — `esp_now_send()` with `inc_tx()` |
| MD-0203 | IMPLEMENTED | `espnow.rs:299-311` — `ensure_peer()` + `add_peer()` |
| MD-0204 | IMPLEMENTED | `peer_table.rs:57-78` — LRU eviction when full |
| MD-0205 | IMPLEMENTED | `bridge.rs:256-266` — FIFO order preserved via ring buffer drain |
| MD-0206 | IMPLEMENTED | `espnow.rs:362-373` — `set_channel()` clears peers, sends ACK |
| MD-0207 | IMPLEMENTED | `espnow.rs:382-413` — `scan_channels()` with channel restore |
| MD-0208 | IMPLEMENTED | Handled by `sonde-protocol::modem` codec (`BodyTooShort` error) |
| MD-0209 | IMPLEMENTED | `bridge.rs:346-359` — invalid channel → `ERROR(CHANNEL_SET_FAILED)` |
| MD-0300 | IMPLEMENTED | `bridge.rs:318-340` — full reset sequence including BLE disable |
| MD-0301 | IMPLEMENTED | `espnow.rs:171-174` — `usb_connected` flag checked in callback |
| MD-0302 | IMPLEMENTED | `modem.rs:79-92` — `esp_task_wdt_reconfigure()` + feed in loop |
| MD-0303 | IMPLEMENTED | `status.rs` — all four counters with reset support |
| MD-0400 | IMPLEMENTED | `ble.rs:383-388` — GATT service + characteristic created |
| MD-0401 | IMPLEMENTED | `ble.rs:395-418` (write→Recv), `ble.rs:527-559` (indicate) |
| MD-0402 | IMPLEMENTED | `ble.rs:341-345` — MTU check in on_authentication_complete |
| MD-0403 | IMPLEMENTED | `ble.rs:540-558` — chunk fragmentation, `ble.rs:615-623` — pacing |
| MD-0404 | IMPLEMENTED | `ble.rs:183-187` — LESC security config; `ble.rs:233-235` — server-initiated |
| MD-0405 | IMPLEMENTED | `ble.rs:209-213` — second connection rejected; disconnect cleanup |
| MD-0407 | IMPLEMENTED | `ble.rs:460-481` — enable; `ble.rs:483-525` — disable |
| MD-0408 | IMPLEMENTED | `ble.rs:527-559` — indicate with empty/no-client guards |
| MD-0409 | IMPLEMENTED | `ble.rs:395-418` — GATT write forwarded; empty discarded |
| MD-0410 | IMPLEMENTED | `ble.rs:348-368` and `ble.rs:576-586` — BLE_CONNECTED after pairing |
| MD-0411 | IMPLEMENTED | `ble.rs:259-277` — BLE_DISCONNECTED on every disconnect |
| MD-0412 | IMPLEMENTED | `bridge.rs:162-173` — BLE disabled at construction; `bridge.rs:322` — disabled on RESET |
| MD-0413 | IMPLEMENTED | `bridge.rs:391-413` — enable/disable handlers; idempotent |
| MD-0414 | IMPLEMENTED | `ble.rs:308-318` — passkey relay; `ble.rs:630-687` — 30 s timeout |

### Overall Assessment

The implementation is **well-aligned** with the specification. All 28 active requirements have corresponding code implementations. The design document is thorough and includes self-identified drift notes (D9-1 through D9-6, D10-4, D10-5) that acknowledge implementation decisions exceeding the requirements. The test suite in `bridge.rs` is extensive (~2100 lines) and covers most requirements at the bridge-logic level.

The main areas of concern are:
1. **Design doc internal consistency** — the §5.2 dispatch table is incomplete (F-001) and the watchdog timeout is contradicted by code (F-003).
2. **A potential race condition** in the `advertise_on_disconnect` handling (F-006) that the design doc itself flags but the code does not fully resolve.
3. **Undocumented behavior** that is reasonable but should be elevated to requirements for traceability (F-002, F-004, F-005).

---

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-006 | Call `advertise_on_disconnect(false)` before `disconnect()` in `disable()`, restore in `enable()`. | S | Medium — requires testing BLE behavior on hardware |
| 2 | F-003 | Align design doc §11 with the runtime 10 s watchdog config, or change modem.rs to 35 s if BLE operations need it. Test on hardware. | S | Medium — wrong value could cause watchdog resets or missed stalls |
| 3 | F-001 | Add BLE message types (0x20–0x23) to modem-design.md §5.2 dispatch table. | S | Low — doc-only change |
| 4 | F-002 | Add idle timeout requirement (e.g., MD-0415) or extend MD-0405 with the 60 s idle timeout. Add validation test. | S | Low — doc + test |
| 5 | F-007 | Add single-write-buffer constraint to MD-0409 AC5 text. | S | Low — doc-only |
| 6 | F-004 | Document per-poll caps in a non-functional requirement or design rationale section. | S | Low — doc-only |
| 7 | F-005 | Elevate D9-3 queue limits from design note to requirements. | S | Low — doc-only |

---

## 7. Prevention

- **Spec update process:** When adding new message types (as BLE was added), update ALL affected design doc sections — not just the feature-specific section but also the dispatch table, message type enumeration, and error handling sections.
- **Runtime vs. compile-time config:** When code uses `reconfigure()` to override sdkconfig values, the design doc must document the runtime override, not just the sdkconfig value.
- **Design doc D-tags:** The team's practice of using D9/D10 tags in the design doc to flag known drift is excellent. Establish a process to close these tags by either (a) adding requirements to cover them or (b) removing the code, within one release cycle.
- **CI check:** Consider a CI step that verifies all message types in the `sonde-protocol::modem` codec's `ModemMessage` enum are listed in the design doc's dispatch table.

---

## 8. Open Questions

1. **Watchdog timeout value (F-003):** Is the correct timeout 10 s (per MD-0302) or 35 s (per design doc BLE rationale)? The runtime override to 10 s may cause spurious resets during BLE pairing on real hardware. Hardware testing needed.
2. **`advertise_on_disconnect` race (F-006):** Is the timing window between `disconnect()` and the second `stop()` exploitable on real hardware? The NimBLE disconnect callback may run synchronously or on a deferred task depending on ESP-IDF version. Test with concurrent BLE_DISABLE and active BLE connection.
3. **MD-0100 verification:** USB-CDC device enumeration (MD-0100) cannot be verified via static analysis. Confirm via hardware test (T-0100).

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-18 | Copilot (automated audit) | Initial audit report |
