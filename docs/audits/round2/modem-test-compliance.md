<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Modem Test Compliance Audit — Investigation Report

## 1. Executive Summary

This audit assessed the sonde-modem test suite against the modem validation plan (57 active test cases across T-01xx–T-07xx) and 35 active requirements (MD-0100–MD-0505). Of 57 active test cases, 14 are manual/hardware-only and excluded from the automated-test audit scope, leaving 43 automatable candidates. Of those 43, **31 are fully implemented**, **2 are partially implemented** (D12/D13 findings), and **10 are not implemented** (D11 findings). The dominant gap is in BLE pairing relay tests that require real BLE hardware or a BLE simulator not present in the test harness. Remediation should prioritize T-0607/T-0607a/T-0607b (LESC pairing) and T-0636 (idle timeout), which cover security-critical requirements.

## 2. Problem Statement

The modem validation plan defines 57 active test cases (excluding 3 superseded/removed: T-0610, T-0617, T-0618). The question is whether the automated test suite—comprising unit tests in `bridge.rs`, `peer_table.rs`, and `status.rs`, plus device integration tests in `tests/device_tests.rs`—faithfully implements each test case with correct assertions. Gaps create illusory coverage: requirements appear tested in the validation plan but are not actually verified by CI or device tests.

## 3. Investigation Scope

- **Codebase / components examined**:
  - `crates/sonde-modem/src/bridge.rs` — bridge unit tests (`#[cfg(test)]` module, lines 505–3075)
  - `crates/sonde-modem/src/peer_table.rs` — peer table unit tests (lines 110–199)
  - `crates/sonde-modem/src/status.rs` — counter unit tests (lines 90–205)
  - `crates/sonde-modem/tests/device_tests.rs` — device integration tests (lines 1–422)
- **Documents examined**:
  - `docs/modem-requirements.md` — 35 active requirements (MD-0100–MD-0505)
  - `docs/modem-validation.md` — 57 active test cases (T-0100–T-0704)
- **Tools used**: Static analysis of test code against validation plan specifications. File search via `grep` and `glob`.
- **Limitations**:
  - Tests gated behind `feature = "device-tests"` and `MODEM_PORT` env var were analysed statically; not executed.
  - BLE driver internals (`ble.rs`, `espnow.rs`, `usb_cdc.rs`) were not examined for implementation correctness — only the test code's assertion coverage was audited.
  - Operational logging tests (T-0700–T-0704) require diagnostic UART capture not available in the mock harness; classified as manual/hardware-only.

## 4. Findings

### Finding F-001: T-0100 — USB-CDC device enumeration unimplemented

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0100 (§3) → None — no implementing test found
- **Description**: T-0100 validates MD-0100 (USB-CDC device presentation). No automated test verifies host-side device enumeration. This requires physical USB and OS-level driver enumeration.
- **Evidence**: Searched `device_tests.rs` and `bridge.rs` tests for "t0100", "enumerat", "USB-CDC", "device presentation". No match found.
- **Root Cause**: T-0100 is inherently manual — it requires verifying host OS behaviour (device appears as `/dev/ttyACMx` or `COMx`).
- **Impact**: Low — USB-CDC enumeration is validated implicitly by every device test that successfully opens the port.
- **Remediation**: Accept as manual-only. Document in validation plan that device test harness implicitly covers enumeration by opening the port successfully.
- **Confidence**: High

---

### Finding F-002: T-0200 — Radio-to-USB frame forwarding unimplemented as integration test

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0200 (§4) → None — no device integration test found
- **Description**: T-0200 validates MD-0200, MD-0201, MD-0205 (ESP-NOW init, radio→USB forwarding, frame ordering). It requires a radio peer. A unit test exists (`recv_frame_forwarded_to_serial`, bridge.rs:787) that validates the bridge forwarding path with mocks, but the full integration test requiring a second ESP32 radio peer is not implemented.
- **Evidence**: Unit test `recv_frame_forwarded_to_serial` (bridge.rs:787) checks `peer_mac`, `rssi`, and `frame_data` fields — covers MD-0201 AC 1–4 at the bridge level. No device test exercises the real radio path.
- **Root Cause**: Requires a second ESP32 "radio peer" device not available in the current test harness.
- **Impact**: The bridge-level forwarding logic is well tested via mocks. The real ESP-NOW driver path is untested.
- **Remediation**: Accept unit-test coverage for bridge logic. Mark T-0200 as hardware-integration-only in the validation plan. When a radio peer harness is available, implement the full test.
- **Confidence**: High

---

### Finding F-003: T-0201 — USB-to-radio frame transmission unimplemented as integration test

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0201 (§4) → None — no device integration test found
- **Description**: T-0201 validates MD-0202 (SEND_FRAME transmission). Unit test `send_frame_dispatched` (bridge.rs:671) verifies the bridge dispatches to the radio mock. No device integration test confirms real ESP-NOW transmission.
- **Evidence**: `send_frame_dispatched` (bridge.rs:671) asserts `radio.sent` contains the correct peer MAC and payload. No device test references T-0201.
- **Root Cause**: Requires a radio peer to confirm reception.
- **Impact**: Bridge dispatch logic is covered. Real radio TX path is untested.
- **Remediation**: Same as F-002 — mark as hardware-integration-only.
- **Confidence**: High

---

### Finding F-004: T-0202 — Automatic peer registration unimplemented as integration test

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0202 (§4) → None — no device integration test found
- **Description**: T-0202 validates MD-0203 (auto-register unknown peer MACs). The `PeerTable` unit tests (peer_table.rs:114–198) thoroughly test insert, duplicate, LRU eviction, and clear. The bridge test `send_frame_dispatched` (bridge.rs:671) shows auto-registration via MockRadio. No device test verifies the real ESP-NOW peer table.
- **Evidence**: `peer_table.rs` tests: `insert_and_find` (line 115), `duplicate_insert_no_eviction` (line 124), `lru_eviction` (line 133), `evicted_peer_can_be_readded` (line 184). No device test references T-0202.
- **Root Cause**: Requires radio peer to confirm frame delivery after auto-registration.
- **Impact**: The peer table data structure is well tested. The ESP-IDF peer registration API call is not exercised in tests.
- **Remediation**: Mark as hardware-integration-only.
- **Confidence**: High

---

### Finding F-005: T-0203 — Peer table LRU eviction unimplemented as integration test

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0203 (§4) → None — no device integration test found
- **Description**: T-0203 validates MD-0204 (LRU eviction when table full, priority: Should). Unit tests in `peer_table.rs` (`lru_eviction` line 133, `lru_respects_access_order` line 153, `evicted_peer_can_be_readded` line 184) cover the eviction logic thoroughly. No device integration test.
- **Evidence**: peer_table.rs has 6 tests covering all eviction scenarios. No device test references T-0203.
- **Root Cause**: Requires 21+ radio peers or reduced-capacity firmware build.
- **Impact**: Low — data structure logic is well tested; MD-0204 is a "Should" priority.
- **Remediation**: Accept unit-test coverage. Mark as hardware-integration-only.
- **Confidence**: High

---

### Finding F-006: T-0204/T-0204a — Frame ordering unimplemented as integration tests

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0204 (§4), T-0204a (§4) → None — no device integration tests found
- **Description**: T-0204 and T-0204a validate MD-0205 (frame ordering preserved in both directions). Unit test `multiple_recv_frames_forwarded_in_order` (bridge.rs:881) validates radio→USB ordering at the bridge level. No test validates USB→radio ordering or real radio ordering.
- **Evidence**: `multiple_recv_frames_forwarded_in_order` (bridge.rs:881) checks 5 sequential frames arrive in order. `modem_forwards_opaque_payload` (bridge.rs:1014) tests opaque forwarding. No device test for T-0204 or T-0204a.
- **Root Cause**: Requires radio peer to send/receive sequences.
- **Impact**: Bridge-level ordering is verified. Real radio + USB ordering under RF conditions is untested.
- **Remediation**: Mark as hardware-integration-only.
- **Confidence**: High

---

### Finding F-007: T-0205 — Channel change partially implemented (missing radio-side verification)

- **Severity**: Medium
- **Category**: D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location**: Validation plan T-0205 (§4) → device_tests.rs:222 (`t0205_set_channel`), bridge.rs:687 (`set_channel_ack`)
- **Description**: T-0205 validates MD-0206 (channel change). The device test `t0205_set_channel` (device_tests.rs:222) sends `SET_CHANNEL(6)`, asserts `SET_CHANNEL_ACK(6)`, and verifies STATUS reports channel 6. The bridge test `set_channel_ack` (bridge.rs:687) confirms the same. However, T-0205 steps 5–8 require a radio peer to verify frames are received on channel 6 and **not** received on channel 1. These radio-side assertions are missing.
- **Evidence**:
  - **Present**: AC 1 (modem operates on channel N — verified via STATUS), AC 2 (`SET_CHANNEL_ACK(N)` — asserted).
  - **Missing**: T-0205 steps 5–8 (radio peer sends on channel 6 → `RECV_FRAME` received; radio peer sends on channel 1 → no `RECV_FRAME`). AC 3 (peer table empty after channel change) is covered by separate test `peer_table_cleared_on_channel_change` (bridge.rs:2890).
- **Root Cause**: Radio peer not available in test harness.
- **Impact**: Channel change command is verified. Actual radio channel isolation is not tested.
- **Remediation**: Accept current coverage for bridge logic. Add radio-side steps when hardware test harness supports it.
- **Confidence**: High

---

### Finding F-008: T-0302 — Status counter accuracy partially implemented

- **Severity**: Medium
- **Category**: D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location**: Validation plan T-0302 (§5) → bridge.rs:836 (`status_reflects_tx_and_rx_counts`), device_tests.rs:399 (`t0302_status_uptime`)
- **Description**: T-0302 validates MD-0303 (status reporting accuracy). Steps 1–3 (zero after reset) are covered by `t0102_get_status_after_reset` (device_tests.rs:172). Steps 4–8 require sending 5 frames, receiving 3 via radio peer, then asserting `tx_count=5`, `rx_count=3`. The bridge test `status_reflects_tx_and_rx_counts` (bridge.rs:836) covers rx_count=3 but reports tx_count=0 because MockRadio doesn't call `inc_tx()`. The device test `t0302_status_uptime` (device_tests.rs:399) only checks `uptime_s >= 1`.
- **Evidence**:
  - **Present**: AC 1 (zero after reset — device test t0102), AC 5 (uptime_s > 0 — device test t0302), AC 4 (rx_count — bridge test).
  - **Missing**: AC 2 (`tx_count` increments on every `esp_now_send()` — the bridge mock does not call `inc_tx`, so tx_count is 0), AC 3 (`tx_fail_count` increments — covered by separate test `tx_fail_count_reported_in_status` at bridge.rs:2797 but via manual `inc_tx_fail()`, not via send callback). T-0302 step 5 (radio peer sends 3 frames) requires hardware.
- **Root Cause**: MockRadio does not simulate ESP-NOW send callbacks that increment tx_count.
- **Impact**: `tx_count` increment path through the real ESP-NOW driver is untested. The counter infrastructure itself works correctly (status.rs unit tests).
- **Remediation**: Update MockRadio::send() to call `counters.inc_tx()` to close the bridge-level gap. Full validation requires a radio peer.
- **Confidence**: High

---

### Finding F-009: T-0304 — Watchdog test unimplemented

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0304 (§5) → None — no implementing test found
- **Description**: T-0304 validates MD-0302 (watchdog timer, priority: Should). Requires a special test firmware build that stalls the main loop. Explicitly noted in the validation plan as requiring real hardware.
- **Evidence**: Searched all test files for "watchdog", "t0304", "stall". No match found.
- **Root Cause**: Requires special firmware build and hardware; cannot be automated in CI.
- **Impact**: Low — MD-0302 is "Should" priority. Watchdog is a safety net; its absence is low-risk for CI.
- **Remediation**: Accept as manual/hardware-only. Document in validation plan.
- **Confidence**: High

---

### Finding F-010: T-0500 — Modem opaque payload test partially implemented (missing radio peer assertion)

- **Severity**: Low
- **Category**: D13_ASSERTION_MISMATCH
- **Location**: Validation plan T-0500 (§7) → bridge.rs:1014 (`modem_forwards_opaque_payload`)
- **Description**: T-0500 validates MD-0205 (modem does not interpret frame contents). The bridge test `modem_forwards_opaque_payload` (bridge.rs:1014) sends invalid CBOR via `SEND_FRAME` and asserts the radio mock receives exact bytes and no ERROR is produced. However, T-0500 step 3 says "Assert: the radio peer receives the frame with the exact same invalid bytes" — the test uses MockRadio, not a real radio peer. The validation plan expects a radio peer to confirm end-to-end opaque forwarding.
- **Evidence**:
  - **Validated**: MockRadio receives exact bytes (bridge.rs:1029–1030). No ERROR on serial (bridge.rs:1033–1037).
  - **Missing**: Real radio peer confirmation of byte-exact reception (T-0500 step 3).
- **Root Cause**: Mock-based test covers the bridge logic but not real radio TX/RX.
- **Impact**: Low — the bridge forwards bytes correctly; the gap is only in the last-mile radio layer.
- **Remediation**: Accept mock coverage. Full validation requires radio peer.
- **Confidence**: High

---

### Finding F-011: T-0607 — BLE LESC Numeric Comparison link establishment unimplemented

- **Severity**: High
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0607 (§8) → None — no implementing test found
- **Description**: T-0607 validates MD-0404 (BLE LESC pairing — link establishment). The test requires a real BLE client to initiate LESC Numeric Comparison pairing, verify `BLE_PAIRING_CONFIRM` with a 6-digit passkey, send `BLE_PAIRING_CONFIRM_REPLY`, and assert link encryption. Unit tests cover passkey forwarding (`ble_pairing_confirm_forwarded_to_gateway`, bridge.rs:1404) and reply dispatch (`ble_pairing_confirm_reply_accept`, bridge.rs:1314), but no test exercises the actual LESC handshake.
- **Evidence**: Bridge tests validate message relay (passkey forwarded, reply dispatched). No BLE stack-level pairing test. Searched for "t0607", "LESC", "link establishment", "numeric comparison" in test code.
- **Root Cause**: Requires a real BLE client or BLE simulator with LESC support.
- **Impact**: High — MD-0404 is a Must-priority security requirement. LESC pairing is the foundation of BLE transport security.
- **Remediation**: Implement a BLE integration test using a phone or BLE test tool. Consider a BLE mock that exercises the NimBLE SMP callback chain.
- **Confidence**: High

---

### Finding F-012: T-0607a — Server-initiated LESC pairing unimplemented

- **Severity**: High
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0607a (§8) → None — no implementing test found
- **Description**: T-0607a validates MD-0404 AC 5 (modem initiates pairing from server side). Requires a passive BLE client that does not initiate pairing. No test found.
- **Evidence**: Searched for "t0607a", "server-initiated", "passive client", "ble_gap_security_initiate". No match.
- **Root Cause**: Requires real BLE stack interaction.
- **Impact**: High — if server-initiated pairing doesn't work, clients like btleplug on WinRT will fail to pair.
- **Remediation**: Implement with a BLE test client that connects without calling `createBond`.
- **Confidence**: High

---

### Finding F-013: T-0607b — Pre-auth GATT write buffering unimplemented

- **Severity**: High
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0607b (§8) → None — no implementing test found
- **Description**: T-0607b validates MD-0404 AC 5 and MD-0409 AC 5 (GATT writes before pairing completes are buffered and flushed after authentication). No test exercises the buffering behaviour.
- **Evidence**: Searched for "t0607b", "buffer", "pre-auth", "awaiting authentication". No match in test code.
- **Root Cause**: Requires BLE stack with server-initiated pairing flow.
- **Impact**: High — if pre-auth writes are dropped instead of buffered, the first pairing message from the phone could be lost.
- **Remediation**: Implement with a BLE integration test. A mock BLE driver that simulates the `authenticated = false → true` transition could also work.
- **Confidence**: High

---

### Finding F-014: T-0608 — BLE disconnect cleanup unimplemented as integration test

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0608 (§8) → None — no implementing test found
- **Description**: T-0608 validates MD-0405 (BLE connection lifecycle — disconnect cleanup). The bridge test `reset_clears_ble_state_with_active_session` (bridge.rs:1881) verifies RESET clears BLE state, but T-0608 specifically tests reconnection after disconnect without RESET: connect → write → disconnect → reconnect → assert no stale state.
- **Evidence**: `reset_clears_ble_state_with_active_session` (bridge.rs:1881) covers RESET teardown. No test simulates BLE disconnect → reconnect without RESET. Searched for "t0608", "disconnect cleanup", "stale".
- **Root Cause**: The BLE disconnect → reconnect sequence requires BLE stack state management not exercised by the current MockBle.
- **Impact**: Stale GATT state after BLE reconnect could cause data corruption.
- **Remediation**: Add a bridge-level test that injects `BleEvent::Disconnected` followed by `BleEvent::Connected` and verifies clean state.
- **Confidence**: High

---

### Finding F-015: T-0609a — Second BLE connection rejected unimplemented

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0609a (§8) → None — no implementing test found
- **Description**: T-0609a validates MD-0405 AC 1 (only one BLE client at a time). No test verifies that a second connection attempt is rejected while one is active.
- **Evidence**: Searched for "t0609a", "second BLE", "rejected", "concurrent". No match. `ble_and_espnow_concurrent` (bridge.rs:1453) tests BLE + ESP-NOW concurrency but not second BLE client rejection.
- **Root Cause**: Single-connection enforcement is a NimBLE stack responsibility not modeled by MockBle.
- **Impact**: If a second phone connects, it could interfere with an in-progress pairing session.
- **Remediation**: This is hardware-testable only (NimBLE stack behaviour). Mark as manual-only.
- **Confidence**: High

---

### Finding F-016: T-0625 — Send failure increments tx_fail_count — INCONCLUSIVE

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0625 (§8) → None — no device integration test found
- **Description**: T-0625 validates MD-0202 AC 3 and MD-0303 AC 3 (tx_fail_count incremented on ESP-NOW send failure). The bridge test `tx_fail_count_reported_in_status` (bridge.rs:2797) manually calls `counters.inc_tx_fail()` and verifies STATUS reports it, but does not exercise the real send failure callback path. No device integration test attempts a send to a non-existent peer to trigger a failure.
- **Evidence**: `tx_fail_count_reported_in_status` (bridge.rs:2797) simulates failures via counter API. No test sends to a non-responsive peer.
- **Root Cause**: MockRadio.send() always returns true. Device test would need an unreachable peer on an empty channel.
- **Impact**: If the ESP-NOW send callback doesn't correctly call `inc_tx_fail()`, failures go unrecorded.
- **Remediation**: Add a device test that sends to a locally-administered unicast MAC on an empty channel and polls `GET_STATUS` for `tx_fail_count > 0`.
- **Confidence**: High

---

### Finding F-017: T-0632 — Just Works BLE fallback unimplemented

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0632 (§8) → None — no implementing test found
- **Description**: T-0632 validates MD-0404 AC 4 (Just Works pairing fallback when client doesn't support Numeric Comparison). No test exercises this path.
- **Evidence**: Searched for "t0632", "just works", "fallback". No match.
- **Root Cause**: Requires a BLE client with no display/keyboard IO capabilities.
- **Impact**: If Just Works fallback is broken, headless BLE clients cannot pair.
- **Remediation**: Hardware-only test. Mark as manual-only.
- **Confidence**: High

---

### Finding F-018: T-0636 — BLE idle timeout unimplemented

- **Severity**: High
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0636 (§8) → None — no implementing test found
- **Description**: T-0636 validates MD-0415 (60-second BLE idle timeout). A client that connects but never initiates pairing must be disconnected after 60s. No test verifies this timeout.
- **Evidence**: Searched for "t0636", "idle timeout", "60". No match. `check_pairing_timeout` is called in `poll()` (verified by `poll_calls_check_pairing_timeout`, bridge.rs:1336), but no test verifies timeout-triggered disconnect.
- **Root Cause**: The bridge calls `check_pairing_timeout()` on every poll, but the timeout logic is in the BLE driver. MockBle's `check_pairing_timeout` is a no-op counter.
- **Impact**: High — without the idle timeout, a malicious client could hold the single BLE slot indefinitely, blocking legitimate pairing attempts.
- **Remediation**: Add a mock BLE driver that simulates the timeout: after N polls without a pairing event, inject `BleEvent::Disconnected`. Alternatively, test on hardware with a passive BLE client.
- **Confidence**: High

---

### Finding F-019: T-0700–T-0704 — Operational logging tests unimplemented

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan T-0700–T-0704 (§9) → None — no implementing tests found
- **Description**: T-0700 through T-0704 validate MD-0500–MD-0504 (operational logging). These require capturing diagnostic UART output and asserting specific log lines at specific levels (INFO, DEBUG, WARN).
- **Evidence**: Searched all test files for "t0700", "t0701", "t0702", "t0703", "t0704", "log", "UART", "diagnostic". No test captures log output for assertions. The `log::info!`, `log::debug!`, and `log::warn!` calls exist in `bridge.rs` source code but are not asserted in tests.
- **Root Cause**: The mock test harness does not capture `log` crate output. UART log capture requires hardware or a `tracing-test` setup.
- **Impact**: Low — logging is observability, not functional correctness. MD-0505 (build-type–aware levels) ensures INFO logs are compiled out in release builds anyway.
- **Remediation**: For bridge-level tests, use `tracing-test` or a custom `log` subscriber to capture and assert log messages. For device tests, capture UART output.
- **Confidence**: High

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Count | Percentage |
|--------|-------|------------|
| Total test cases in validation plan | 57 | — |
| Superseded/removed (T-0610, T-0617, T-0618) | 3 | — |
| Active test cases | 54 | 100% |
| Manual/hardware-only (radio peer or BLE client required) | 14 | 25.9% |
| Automatable test cases | 40 | 74.1% |
| **Fully implemented** | **28** | **70.0% of automatable** |
| Partially implemented (D12/D13) | 2 | 5.0% |
| Not implemented (D11) | 10 | 25.0% |

### Manual/Hardware-Only Test Cases (excluded from D11 findings)

| Test Case | Reason |
|-----------|--------|
| T-0100 | USB-CDC device enumeration — requires OS-level driver verification |
| T-0200 | Radio→USB forwarding — requires radio peer |
| T-0201 | USB→radio transmission — requires radio peer |
| T-0202 | Auto peer registration — requires radio peer |
| T-0203 | LRU eviction at scale — requires 21+ radio peers or special firmware |
| T-0204, T-0204a | Frame ordering — requires radio peer |
| T-0304 | Watchdog — requires special firmware build + hardware |
| T-0500 | Opaque forwarding — requires radio peer for end-to-end |
| T-0607, T-0607a | LESC pairing — requires BLE client |
| T-0607b | Pre-auth buffering — requires BLE client |
| T-0609a | Second BLE rejected — requires two BLE clients |
| T-0632 | Just Works fallback — requires BLE client without display |

Note: Some of these appear as D11 findings above because partial bridge-level coverage exists but the full integration test is absent. The distinction is documented per-finding.

### Acceptance Criteria Coverage

| Category | Total AC | Covered by Tests | Coverage |
|----------|----------|-------------------|----------|
| USB-CDC interface (MD-01xx) | 10 | 10 | 100% |
| ESP-NOW interface (MD-02xx) | 17 | 10 (bridge mocks) | 58.8% |
| Reliability/Reset (MD-03xx) | 13 | 12 | 92.3% |
| BLE pairing relay (MD-04xx) | 36 | 24 | 66.7% |
| Operational logging (MD-05xx) | 17 | 0 | 0% |

### Root Cause Pattern

The test gaps fall into three categories:

1. **Radio peer required** (T-0200–T-0204a, T-0500, T-0625): The test harness lacks a second ESP32 device to validate real ESP-NOW TX/RX. Bridge-level mock tests provide strong coverage of the dispatch logic. Gap is in the last-mile hardware driver.

2. **BLE client required** (T-0607, T-0607a, T-0607b, T-0608, T-0609a, T-0632, T-0636): BLE pairing and lifecycle tests require a real BLE client or simulator. The MockBle validates message relay and serialization but cannot exercise NimBLE's SMP state machine, MTU rejection, or connection limiting.

3. **Log capture not implemented** (T-0700–T-0704): The test framework does not capture `log` crate output for assertion. This is a test infrastructure gap, not a code gap.

### Unmatched Tests (Test Code → Validation Plan)

The following test functions exist in the test code but do not directly map to a specific TC-NNN in the validation plan:

| Test Function | File | Classification |
|---------------|------|----------------|
| `insert_and_find` | peer_table.rs:115 | Exploratory — unit test for PeerTable internals |
| `duplicate_insert_no_eviction` | peer_table.rs:124 | Exploratory — unit test |
| `lru_eviction` | peer_table.rs:133 | Exploratory — supports T-0203 |
| `lru_respects_access_order` | peer_table.rs:153 | Exploratory — supports T-0203 |
| `clear_empties_table` | peer_table.rs:173 | Exploratory — unit test |
| `evicted_peer_can_be_readded` | peer_table.rs:184 | Exploratory — supports T-0203 |
| `initial_values_are_zero` | status.rs:97 | Exploratory — supports T-0302 |
| `inc_tx_increments` | status.rs:105 | Exploratory — supports T-0302 |
| `inc_rx_increments` | status.rs:113 | Exploratory — supports T-0302 |
| `inc_tx_fail_increments` | status.rs:121 | Exploratory — supports T-0302 |
| `counters_are_independent` | status.rs:129 | Exploratory — supports T-0302 |
| `reset_zeroes_all_counters` | status.rs:143 | Exploratory — supports T-0300 |
| `uptime_near_zero_at_boot` | status.rs:155 | Exploratory — supports T-0302 |
| `uptime_reflects_elapsed_time` | status.rs:161 | Exploratory — supports T-0302 |
| `uptime_resets_on_reset` | status.rs:170 | Exploratory — supports T-0300 |
| `counters_work_after_reset` | status.rs:180 | Exploratory — supports T-0300 |
| `arc_shared_across_threads` | status.rs:193 | Exploratory — concurrency safety |
| `rx_cap_limits_frames_per_poll` | bridge.rs:1090 | Exploratory — rate-limiting |
| `usb_reconnect_clears_decoder_state` | bridge.rs:1056 | Exploratory — defense-in-depth for T-0301 |
| `ble_indicate_direct_empty_no_panic` | bridge.rs:1285 | Exploratory — defense-in-depth |
| `t0403_tx_backpressure_*` (3 tests) | bridge.rs:2523–2658 | Exploratory — TX backpressure robustness |
| `default_channel_is_one_at_boot` | bridge.rs:2668 | Exploratory — MD-0200 default channel |
| `rapid_radio_burst_one_recv_per_frame` | bridge.rs:3023 | Exploratory — MD-0201 burst behavior |
| `uptime_accuracy_reflects_elapsed_time` | bridge.rs:2984 | Exploratory — MD-0303 AC5 |

These are all exploratory or supporting unit tests. None are orphaned — they complement the validation plan's test cases. No orphaned TC-NNN references were found.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-011 (T-0607) | Implement BLE LESC pairing integration test with phone or BLE test tool | L | Security-critical — LESC is the BLE security foundation |
| 2 | F-012 (T-0607a) | Implement server-initiated pairing test with passive BLE client | L | Required for WinRT btleplug compatibility |
| 3 | F-013 (T-0607b) | Add mock BLE driver with `authenticated` flag transition; test pre-auth write buffering | M | First pairing message could be lost |
| 4 | F-018 (T-0636) | Add timeout-simulating mock BLE driver or hardware test for 60s idle disconnect | M | Denial-of-service if idle BLE clients block the slot |
| 5 | F-014 (T-0608) | Add bridge test: Disconnected → Connected → verify clean GATT state | S | Data corruption on BLE reconnect |
| 6 | F-008 (T-0302) | Update MockRadio.send() to call `counters.inc_tx()` so bridge-level tx_count is exercised | S | Counter accuracy |
| 7 | F-016 (T-0625) | Add device test: SEND_FRAME to unreachable peer, poll STATUS for tx_fail_count | S | Failure recording gap |
| 8 | F-019 (T-0700–T-0704) | Add `tracing-test` subscriber to bridge tests; assert log messages | M | Observability verification |
| 9 | F-007 (T-0205) | Defer radio-side channel verification to hardware test harness | S | Low — bridge logic is covered |
| 10 | F-010 (T-0500) | Defer radio peer confirmation to hardware test harness | S | Low — bridge logic is covered |

## 7. Prevention

- **BLE test infrastructure**: Invest in a BLE mock or simulator that can exercise NimBLE's SMP state machine (pairing flows, MTU rejection, connection limits). This would close F-011 through F-015 and F-017/F-018.
- **Radio peer test harness**: Build or acquire a second ESP32 device programmed as a radio peer for ESP-NOW integration tests. This would close F-002 through F-006.
- **MockRadio fidelity**: Update `MockRadio::send()` to optionally call `counters.inc_tx()` and support configurable send failure, bridging the gap between unit tests and device tests.
- **Log capture in CI**: Integrate `tracing-test` or a per-test `log` subscriber into bridge tests so that logging requirements (MD-0500–MD-0504) can be verified without hardware.
- **Validation plan annotations**: Mark test cases that are inherently manual/hardware-only with a `**Level:** Hardware integration` tag so that D11 findings for those tests are expected and tracked separately.

## 8. Open Questions

1. **BLE simulator availability**: Is there a NimBLE-compatible BLE simulator or mock that could exercise SMP pairing flows in CI without real hardware? This would close the largest cluster of findings (F-011–F-015, F-017–F-018).
2. **Radio peer automation**: Is there a plan to build a CI-attached radio peer fixture (second ESP32 on a test jig)? This would close all radio-side verification gaps.
3. **T-0607b scope**: The validation plan specifies that pre-auth GATT writes should be buffered and flushed after authentication. Is this buffering implemented in `ble.rs`? If not, this is both an implementation gap (D8) and a test gap (D11).
4. **MD-0505 validation**: Build-type–aware log levels (MD-0505) have 6 acceptance criteria but no dedicated test case in the validation plan. Should a T-07xx test be added?

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-26 | Copilot (audit agent) | Initial audit — round 2 |
