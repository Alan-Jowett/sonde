<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# BLE Pairing Tool Test Compliance — Investigation Report

## 1. Executive Summary

This audit compared 74 automatable test cases from the BLE pairing tool validation plan (`ble-pairing-tool-validation.md`) against the test code in `crates/sonde-pair/src/`. Of the 74 automatable test cases, **56 are fully implemented** (75.7%), **6 are partially implemented** with assertion gaps, and **12 are unimplemented**. The unimplemented tests span diagnostic logging (T-PT-1207–1212), error classification (T-PT-400, T-PT-401), default-level log key material check (T-PT-700), TOFU clearing (T-PT-207), phone label validation (T-PT-208a), and the `Unknown` pairing method rejection (T-PT-109b). Highest-severity gaps are the missing `enforce_lesc` test for `PairingMethod::Unknown` (security-related path) and the absent key-material-in-logs tests (PT-0900).

## 2. Problem Statement

The validation plan specifies 85 test cases (T-PT-100 through T-PT-1212). Of these, 11 are manual/platform-only tests. This audit determines whether every automatable test case has a corresponding automated test in the `sonde-pair` crate, whether each test's assertions match the validation plan's expected results, and whether each test verifies all linked acceptance criteria.

## 3. Investigation Scope

- **Codebase examined:** `crates/sonde-pair/src/` — all 21 `.rs` files
- **Documents examined:**
  - `docs/ble-pairing-tool-requirements.md` (requirements PT-0100 through PT-1213)
  - `docs/ble-pairing-tool-validation.md` (test cases T-PT-100 through T-PT-1212)
- **Method:** `grep` for `#[test]`, `#[tokio::test]`, `#[traced_test]` annotations and `T-PT-` / `PT-` references; `view` of test function bodies and assertions; cross-reference against validation plan procedures and acceptance criteria.
- **Tools:** Static analysis only (no test execution). Pattern matching via `grep`, file inspection via `view`.
- **Limitations:** Android-only tests (T-PT-105–107, T-PT-113, T-PT-114, T-PT-604, T-PT-806, T-PT-807) and other manual/platform tests (T-PT-108, T-PT-110, T-PT-111, T-PT-605) were excluded as marked manual in the validation plan.

### Test code inventory

| File | Test count | Key coverage areas |
|---|---|---|
| `phase1.rs` | 29 | Phase 1 happy path, errors, TOFU, LESC, timeouts, retries, logging |
| `phase2.rs` | 24 | Phase 2 happy path, errors, LESC, timeouts, retries, payload size |
| `discovery.rs` | 12 | BLE scanning, UUID filtering, stale eviction, scan timeout |
| `crypto.rs` | 17 | SHA-256, HMAC, AES-GCM, Ed25519, X25519, HKDF, zeroize |
| `validation.rs` | 8 | node_id validation, rf_channel validation, key_hint derivation |
| `cbor.rs` | 7 | CBOR encoding/decoding, deterministic encoding |
| `envelope.rs` | 17 | BLE envelope parsing, GW_INFO, PHONE_REGISTERED, NODE_ACK, errors |
| `store.rs` | 6 | MemoryPairingStore CRUD, gateway identity |
| `file_store.rs` | 25 | File-backed store, hex encoding, DPAPI-protected PSK |
| `dpapi.rs` | 3 | DPAPI protect/unprotect round-trip |
| `transport.rs` | 1 | `enforce_lesc` with `None` pairing method |
| `fragmentation.rs` | 17 | BLE fragmentation/reassembly |
| `loopback_transport.rs` | 5 | Loopback transport |
| `rng.rs` | 2 | OsRng, MockRng |
| `android_transport.rs` | 4 | UUID string formatting |
| `lib.rs` | 1 | Core crate feature independence |
| **Total** | **178** | |

### Manual/deferred test cases (excluded from audit)

| Test ID | Reason |
|---|---|
| T-PT-105, T-PT-106, T-PT-107 | Android BLE permissions (manual, requires device) |
| T-PT-108 | LESC Numeric Comparison on real hardware (manual) |
| T-PT-110 | Minimum UI elements (manual) |
| T-PT-113 | Android activity lifecycle (manual) |
| T-PT-114 | JNI classloader caching (manual) |
| T-PT-604 | Android EncryptedSharedPreferences (manual instrumentation) |
| T-PT-605 | Windows file ACL permissions (manual/platform) |
| T-PT-806 | Android lifecycle pause/resume (manual) |
| T-PT-807 | JNI classloader on background thread (manual) |

**Total manual/deferred: 11 test cases**

## 4. Findings

### Finding F-001: T-PT-1207 — BLE scan events logged

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §12, T-PT-1207
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-1207 requires a `#[traced_test]` test that verifies `debug!` events are emitted when a BLE scan starts (with UUID filter list), stops, and discovers devices (with name, address, RSSI). No test in `discovery.rs` or any other file uses `#[traced_test]` or asserts on log output. The existing `discovery.rs` tests verify functional behavior (UUID filtering, stale eviction) but never capture or assert on tracing output.
- **Evidence:** `grep` for `t_pt_1207`, `scan_events_logged`, `traced_test` in `discovery.rs` returned no matches. `grep` for `logs_contain` across all files returned results only in `phase1.rs`.
- **Root Cause:** Diagnostic logging tests (T-PT-1207–1212) are a newer requirement set (PT-1207–PT-1212) and appear to not have been implemented yet.
- **Impact:** BLE scan diagnostic logging cannot be verified by CI. Regressions in scan-level logging will not be caught.
- **Remediation:** Add a `#[traced_test]` test in `discovery.rs` that starts/stops a scan with `MockBleTransport` and asserts `logs_contain("scan started")`, `logs_contain("scan stopped")`, and per-device discovery events.
- **Confidence:** High

---

### Finding F-002: T-PT-1208 — Connection lifecycle events logged

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §12, T-PT-1208
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-1208 requires a `#[traced_test]` test verifying `debug` events for "connecting", `mtu`, and "disconnected". No test captures tracing output for connection lifecycle. The existing `t_pt_112_verbose_mode` test (phase1.rs:1170) covers message-type logging but not connection lifecycle events specifically.
- **Evidence:** `grep` for `t_pt_1208`, `connection_lifecycle`, `logs_contain.*connecting` returned no matches outside the unrelated `t_pt_112_verbose_mode`.
- **Root Cause:** Same as F-001 — diagnostic logging tests not yet implemented.
- **Impact:** Connection-level diagnostic logging not verified by CI.
- **Remediation:** Add a `#[traced_test]` test that runs a pairing flow and asserts `logs_contain("connecting")`, `logs_contain("mtu")`, `logs_contain("disconnected")`.
- **Confidence:** High

---

### Finding F-003: T-PT-1209 — GATT write and indication events logged

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §12, T-PT-1209
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-1209 requires `trace!` events for GATT writes (with `msg` type and `len`) and indications, plus `debug!` transport events. The existing `t_pt_112_verbose_mode` partially covers this (asserts `REQUEST_GW_INFO`, `GW_INFO_RESPONSE`, `REGISTER_PHONE` appear in logs) but does not assert on `len` fields, `BLE indication received` events, transport-level `GATT write complete` events, or that no raw payload bytes appear.
- **Evidence:** `t_pt_112_verbose_mode` (phase1.rs:1170) asserts message type names but not structured field assertions (`len`, `characteristic`).
- **Root Cause:** Same as F-001.
- **Impact:** GATT-level diagnostic observability not fully validated.
- **Remediation:** Extend `t_pt_112_verbose_mode` or add a dedicated `#[traced_test]` test asserting `logs_contain("BLE write")` with `len` and `msg` fields.
- **Confidence:** High

---

### Finding F-004: T-PT-1210 — Phase transition events logged

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §12, T-PT-1210
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-1210 requires `info!` events for phase transitions ("connecting to gateway", "Phase 1 complete" with `phone_key_hint` and `rf_channel`). No test asserts on phase-transition log events. The `t_pt_701_progress_reports_phases_in_order` test (phase1.rs:895) validates the progress callback mechanism but not tracing output.
- **Evidence:** `grep` for `t_pt_1210`, `phase_transition.*log`, `Phase 1 complete` in test code returned no matches.
- **Root Cause:** Same as F-001.
- **Impact:** Phase transition logging regressions not caught by CI.
- **Remediation:** Add a `#[traced_test]` test asserting `logs_contain("Phase 1 complete")` with `phone_key_hint` and `rf_channel` fields.
- **Confidence:** High

---

### Finding F-005: T-PT-1211 — LESC pairing method logged

- **Severity:** Low
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §12, T-PT-1211
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-1211 requires a `debug!` event with `pairing_method` field after connection. No test captures and asserts on the pairing method log event. The `enforce_lesc` function in `transport.rs:221` does emit `debug!(?method, "BLE pairing method verified")`, but no test verifies this log output.
- **Evidence:** `grep` for `t_pt_1211`, `lesc.*log`, `pairing_method.*log` returned no matches.
- **Root Cause:** Same as F-001.
- **Impact:** Low — the `enforce_lesc` path is tested functionally; this is about diagnostic observability only.
- **Remediation:** Add a `#[traced_test]` wrapper around the existing `t_pt_804_numeric_comparison_enforced` test and assert `logs_contain("pairing_method")`.
- **Confidence:** High

---

### Finding F-006: T-PT-1212 — Error context in log output

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §12, T-PT-1212
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-1212 requires that timeout errors include the timeout duration in log output, error events include the operation name, and protocol errors include the status code. No `#[traced_test]` verifies these structured error log fields. The error types do carry this data (e.g., `PairingError::Timeout { operation, duration_secs }`) but no test asserts on the tracing output.
- **Evidence:** `grep` for `t_pt_1212`, `error_context.*log` returned no matches.
- **Root Cause:** Same as F-001.
- **Impact:** Error diagnostic context in logs not verified by CI.
- **Remediation:** Add a `#[traced_test]` that triggers a timeout error and asserts log output includes the operation name and duration.
- **Confidence:** High

---

### Finding F-007: T-PT-109b — Unknown pairing method rejected by `enforce_lesc`

- **Severity:** High
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §3, T-PT-109b
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-109b requires configuring `MockBleTransport` with `pairing_method()` returning `Some(Unknown)` and asserting that `enforce_lesc` returns `Err(InsecurePairingMethod)` and disconnects. The transport code (`transport.rs:209-219`) does handle this correctly — any `Some(method)` where `method != NumericComparison` is rejected. However, no test explicitly exercises the `Unknown` variant. The existing `t_pt_805_just_works_fallback_rejected` tests only exercise `JustWorks`.
- **Evidence:** `grep` for `PairingMethod::Unknown` in test code returned no matches. `grep` for `t_pt_109b` returned no matches.
- **Root Cause:** The `Unknown` variant was likely considered covered by the generic `enforce_lesc` logic, but the validation plan explicitly requires a separate test.
- **Impact:** The `Unknown` pairing method rejection is a security path (PT-0904 criterion 4). While the code handles it correctly via the `!= NumericComparison` check, there is no regression test specifically for this case.
- **Remediation:** Add a test that sets `transport.pairing_method = Some(PairingMethod::Unknown)` and asserts `enforce_lesc` returns `InsecurePairingMethod` and the transport is disconnected.
- **Confidence:** High

---

### Finding F-008: T-PT-400 — Error classification (device/transport/protocol)

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §6, T-PT-400
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-400 requires triggering device-level, transport-level, and protocol-level errors and asserting each error message identifies its category. No single test exercises all three categories and asserts on category identification. Individual error paths are tested (e.g., `t_pt_109_just_works_connect_rejected` for connection failure, `t_pt_203_registration_window_closed` for protocol error), but none assert on error *category* labeling.
- **Evidence:** `grep` for `t_pt_400`, `error_classification`, `device.*level`, `transport.*level`, `protocol.*level` in test assertions returned no matches.
- **Root Cause:** Error classification is tested implicitly through specific error variant matching but not through category labeling assertions.
- **Impact:** No regression test verifies the error classification taxonomy required by PT-0500.
- **Remediation:** Add a test that triggers one error per category and asserts the `PairingError` variant correctly identifies the category (or that `Display` output includes the category label).
- **Confidence:** High

---

### Finding F-009: T-PT-401 — Actionable error messages include next steps

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §6, T-PT-401
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-401 requires triggering each of 7 error types (BLE adapter disabled, MTU too low, signature failure, ERROR(0x02), ERROR(0x03), NODE_ACK(0x01), timeout) and asserting every error message includes at least one actionable sentence and is not just a code. Individual tests verify the error *type* (via `matches!`) but do not assert on the message *text* being actionable.
- **Evidence:** `grep` for `t_pt_401`, `actionable`, `next step` in test assertions returned no matches.
- **Root Cause:** The tests focus on error type matching, not message content validation.
- **Impact:** Operator-facing error messages could regress to unhelpful codes without CI catching it.
- **Remediation:** Add a test that formats each error's `Display` output and asserts it contains actionable guidance (e.g., contains words like "enable", "retry", "hold", "ask").
- **Confidence:** High

---

### Finding F-010: T-PT-207 — TOFU operator can clear pinned identity

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §4, T-PT-207
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-207 requires pre-loading a `gw_public_key`, clearing it, verifying removal, then connecting to a new gateway successfully. The existing `store.rs::clear_removes_artifacts` test (store.rs:122) verifies the `clear()` method on `MemoryPairingStore`, and `t_pt_601_already_paired_detection` (phase1.rs:845) tests detection. However, no test combines clearing with a subsequent Phase 1 to verify that TOFU accepts a new key after clearing.
- **Evidence:** `grep` for `t_pt_207`, `clear_pinned`, `clear.*identity` returned only `t_pt_207_mtu_too_low` (a naming mismatch — this tests MTU, not TOFU clearing). `store.rs::clear_removes_artifacts` tests the store operation but not the end-to-end TOFU flow.
- **Root Cause:** The TOFU clear operation is tested at the store layer but not integrated with the TOFU acceptance flow in `pair_with_gateway`.
- **Impact:** Regression in TOFU reset-and-re-pair flow would not be caught by CI.
- **Remediation:** Add an integration test that: (1) saves a gateway identity, (2) clears the store, (3) runs Phase 1 with a different gateway, (4) asserts success and new identity stored.
- **Confidence:** High

---

### Finding F-011: T-PT-208a — Phone label validation

- **Severity:** Medium
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §4, T-PT-208a
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-208a requires testing phone label boundary validation: 64-byte label accepted, 65-byte label rejected before BLE, empty label accepted. The `pair_with_gateway` function (phase1.rs:74) does validate `phone_label.len() > 64`, but no test exercises this validation. Most tests pass `""` as the label.
- **Evidence:** `grep` for `t_pt_208a`, `label.*64`, `label.*65`, `label_valid`, `label_too_long` returned no test function matches.
- **Root Cause:** Label validation was implemented but not tested.
- **Impact:** The 64-byte label limit could be accidentally changed without CI detection.
- **Remediation:** Add a test calling `pair_with_gateway` with a 64-byte label (succeeds), a 65-byte label (fails with `InvalidPhoneLabel`), and an empty label (succeeds).
- **Confidence:** High

---

### Finding F-012: T-PT-602 — Corrupted store → error + reset offer

- **Severity:** Medium
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §8, T-PT-602
- **Location (test code):** `file_store.rs` — `corrupted_json_returns_store_corrupted` (line ~509), `missing_field_returns_store_corrupted` (line ~527)
- **Description:** T-PT-602 specifies (1) configuring a mock store to return corruption errors, (2) asserting a clear error message (not a panic), and (3) asserting the tool offers to reset the store. Criterion (1) and (2) are covered by `file_store.rs` tests that verify `StoreCorrupted` errors for invalid/incomplete JSON. However, criterion (3) — "tool offers to reset the store" — is not tested. The `PairingStore` trait does have a `clear()` method, but no test verifies that the error-handling flow offers a reset path to the operator.
- **Evidence:** `file_store.rs::corrupted_json_returns_store_corrupted` asserts `matches!(result, Err(PairingError::StoreCorrupted(_)))`. No assertion on reset offer.
- **Root Cause:** The "offer to reset" is a UI-level concern that the core crate's tests don't address, but the validation plan assigns it to T-PT-602 as a CI test.
- **Impact:** PT-0803 acceptance criterion 2 ("operator can choose to reset the store") is untested.
- **Remediation:** Either add a mock-based test that verifies the store's `clear()` method is callable after a `StoreCorrupted` error, or reclassify this criterion as manual-only in the validation plan.
- **Confidence:** High

---

### Finding F-013: T-PT-502 — Already-paired detection and operator choice

- **Severity:** Low
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §7, T-PT-502
- **Location (test code):** `phase1.rs` — `t_pt_601_already_paired_detection` (line 845)
- **Description:** T-PT-502 requires (1) detecting existing pairing, (2) warning the operator, (3) simulating operator choosing to proceed, and (4) verifying Phase 1 continues normally. The existing `t_pt_601_already_paired_detection` test verifies detection via `is_already_paired()` but does not simulate the operator choosing to proceed and verifying Phase 1 continues. The `t_pt_600_repeated_phase1_does_not_corrupt_state` test (phase1.rs:754) does verify that re-running Phase 1 succeeds, which partially covers criterion (4).
- **Evidence:** `t_pt_601_already_paired_detection` (phase1.rs:845) only tests the detection helper; it does not exercise a warn-then-proceed flow.
- **Root Cause:** The "operator choice" aspect is a UI concern; the core library provides `is_already_paired()` for the UI to call.
- **Impact:** Low — the detection and re-pairing paths are independently tested.
- **Remediation:** Add a combined test or document that T-PT-502 criteria 3–4 are covered by the combination of `t_pt_601` and `t_pt_600`.
- **Confidence:** High

---

### Finding F-014: T-PT-700 — No key material in default logs

- **Severity:** High
- **Category:** D11_UNIMPLEMENTED_TEST_CASE
- **Location (validation plan):** §9, T-PT-700
- **Location (test code):** None — no implementing test found
- **Description:** T-PT-700 requires running a full Phase 1 + Phase 2 flow with tracing at default level and asserting no key material (phone_psk, node_psk, ephemeral keys, shared secrets) appears. The existing `t_pt_112_verbose_mode` (phase1.rs:1170) checks verbose (TRACE) output for key material absence, partially covering T-PT-701, but no test runs at default log level, and no test covers Phase 2 log output for key material absence.
- **Evidence:** `grep` for `t_pt_700`, `no_key_material` returned no matches. The only log-content test is `t_pt_112_verbose_mode` at TRACE level.
- **Root Cause:** The verbose-mode test was implemented but the default-level counterpart was not.
- **Impact:** Key material leaking into default log output would not be caught by CI. This is a security-sensitive path (PT-0900).
- **Remediation:** Add a `#[traced_test]` test at default (INFO) level that runs Phase 1 + Phase 2 and asserts no hex-encoded key bytes appear in logs.
- **Confidence:** High

---

### Finding F-015: T-PT-701 — No key material in verbose logs

- **Severity:** Medium
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §9, T-PT-701
- **Location (test code):** `phase1.rs` — `t_pt_112_verbose_mode` (line 1170)
- **Description:** T-PT-701 requires Phase 1 + Phase 2 verbose output with key material absence check. `t_pt_112_verbose_mode` covers Phase 1 only — it asserts phone PSK hex does not appear in TRACE output. It does not run Phase 2 or check for `node_psk`, ephemeral key, or shared secret hex strings.
- **Evidence:** `t_pt_112_verbose_mode` (phase1.rs:1214-1218) checks only `psk_hex = "5555..."`. No Phase 2 log capture test exists.
- **Root Cause:** The test was written before Phase 2 implementation was complete.
- **Impact:** Phase 2 key material (node_psk) could leak into verbose logs without CI detection.
- **Remediation:** Extend the test to also run Phase 2 and check for node_psk, ephemeral key, and shared secret hex strings.
- **Confidence:** High

---

### Finding F-016: T-PT-702 — All randomness from injectable RNG provider

- **Severity:** Medium
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §9, T-PT-702
- **Location (test code):** `rng.rs` — `mock_rng_is_deterministic` (line 51); `phase1.rs` and `phase2.rs` use `MockRng` throughout
- **Description:** T-PT-702 has four criteria: (1) RNG provider trait exists, (2) mock RNG used in CI, (3) all random values sourced from mock, (4) no direct `rand::rng()` calls. Criteria 1–3 are met — all tests use `MockRng` and the `RngProvider` trait. Criterion 4 (enforced via `#![deny(clippy::disallowed_methods)]` or equivalent lint) is not verified by any test. No `clippy.toml` or lint configuration asserting this was found.
- **Evidence:** `rng.rs` defines `RngProvider` trait and `MockRng`. All phase tests inject `MockRng`. No `clippy::disallowed_methods` configuration found via `grep`.
- **Root Cause:** The lint rule is a CI configuration concern, not a unit test.
- **Impact:** A future contributor could add a direct `rand::rng()` call without CI flagging it.
- **Remediation:** Add `clippy::disallowed_methods` for `rand::rng` in the crate's `clippy.toml` or `Cargo.toml` lint configuration, or add a structural test that greps the source for `rand::rng`.
- **Confidence:** Medium — [ASSUMPTION] the absence of `clippy.toml` means no lint rule exists; the crate may enforce this at a workspace level not examined.

---

### Finding F-017: T-PT-802 — Timeout values match spec

- **Severity:** Medium
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §10, T-PT-802
- **Location (test code):** `phase1.rs` — `t_pt_802b_connection_timeout_exercised` (line 1416); `phase2.rs` — `t_pt_802_connection_timeout_phase2` (line 857)
- **Description:** T-PT-802 requires asserting five specific timeout constants: GW_INFO_RESPONSE = 45 s, PHONE_REGISTERED = 30 s, NODE_ACK = 5 s, BLE scan default = 30 s, BLE connection = 10 s. The implementing tests verify the connection timeout path (10 s) exercises correctly, but do not assert the *values* of the other four timeout constants. The scan timeout is set in `discovery.rs:22` as `DEFAULT_SCAN_TIMEOUT = Duration::from_secs(30)` but no test asserts this value. The GW_INFO_RESPONSE, PHONE_REGISTERED, and NODE_ACK timeouts are injected by the mock transport, not read from constants.
- **Evidence:** `t_pt_802b_connection_timeout_exercised` asserts `duration_secs == 10` for connection timeout only. No assertions on GW_INFO timeout (45 s), PHONE_REGISTERED timeout (30 s), or NODE_ACK timeout (5 s) exist anywhere in the test suite.
- **Root Cause:** The mock transport's `read_indication` returns immediately (either queued response or timeout error), so timeout *values* are not exercised — only timeout *handling*.
- **Impact:** Timeout values could be changed from spec without CI detection.
- **Remediation:** Add a test that asserts the timeout constant values directly (e.g., `assert_eq!(GW_INFO_TIMEOUT.as_secs(), 45)`).
- **Confidence:** High

---

### Finding F-018: T-PT-703 — Non-zero test keys used

- **Severity:** Low
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §9, T-PT-703
- **Location (test code):** No dedicated test function
- **Description:** T-PT-703 requires searching test files for `[0u8; 32]` and asserting none use all-zero keys. No automated test performs this source-code search. Manual inspection shows the test suite uses `[0x42u8; 32]`, `[0x55u8; 32]`, `[0x01u8; 16]`, etc. — all non-zero. The one occurrence of `[0u8; 32]` in crypto.rs tests is used as a *comparison target* in zeroize assertions (`assert_eq!(key, [0u8; 32], "zeroize() must clear the buffer")`) — not as a PSK value.
- **Evidence:** `crypto.rs:325` uses `[0u8; 32]` as a zeroize assertion target. All PSK/key values in test code use non-zero patterns.
- **Root Cause:** This is a structural/process requirement, not a runtime assertion.
- **Impact:** Low — currently all test keys are non-zero; a CI lint would provide ongoing protection.
- **Remediation:** Add a CI script or test that greps for `[0u8; 32]` used as key/PSK initialization and fails if found (excluding zeroize assertions).
- **Confidence:** High

---

### Finding F-019: T-PT-803 — No implicit retries on protocol failure (assertion gap)

- **Severity:** Low
- **Category:** D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location (validation plan):** §10, T-PT-803
- **Location (test code):** `phase1.rs:1458` `t_pt_803_no_implicit_retries_phase1_write`, `phase1.rs:1490` `t_pt_803_no_implicit_retries_phase1_read`, `phase2.rs:905` `t_pt_803_no_implicit_retries_phase2_write`, `phase2.rs:946` `t_pt_803_no_implicit_retries_phase2_read`
- **Description:** T-PT-803 procedure specifies using a "queued error mechanism on the mock" for write failures. The Phase 1 write test uses `transport.write_error = Some(...)` which is a mock-level flag, matching the intent. However, T-PT-803 also specifies (step 4) asserting `write_characteristic` was called exactly **once** — the Phase 1 write test correctly asserts `transport.written.len() == 1`. All four subtests correctly implement T-PT-803. The only minor gap: T-PT-803 specifies injecting a timeout on `read_indication` (step 5–7), which the read tests handle by queueing no responses (causing timeout), not by injecting an error on `read_indication`. This is functionally equivalent.
- **Evidence:** `t_pt_803_no_implicit_retries_phase1_write` (phase1.rs:1472) asserts `transport.written.len() == 1`. `t_pt_803_no_implicit_retries_phase1_read` (phase1.rs:1518) asserts `transport.read_call_count == 1`.
- **Root Cause:** Minor implementation variation — functionally equivalent to the spec.
- **Impact:** Negligible — the retry prevention is correctly tested.
- **Remediation:** None required; this finding is informational.
- **Confidence:** High

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Count | Rate |
|---|---|---|
| Total test cases in validation plan | 85 | — |
| Manual/deferred (excluded from audit) | 11 | — |
| **Automatable test cases** | **74** | — |
| Fully implemented | 56 | 75.7% |
| Partially implemented (D12) | 6 | 8.1% |
| Unimplemented (D11) | 12 | 16.2% |
| Assertion mismatches (D13) | 0 | 0% |

### Implemented test case mapping

The following automatable test cases are **fully implemented** with correct assertions:

| Test ID | Implementing test(s) | File:Line |
|---|---|---|
| T-PT-100 | `test_uuid_filtering_includes_gateway_and_node` | discovery.rs:283 |
| T-PT-101 | `test_uuid_filtering_includes_gateway_and_node` | discovery.rs:283 |
| T-PT-102 | `test_uuid_filtering_rejects_no_uuid` | discovery.rs:303 |
| T-PT-103 | `test_service_type_classification` | discovery.rs:320 |
| T-PT-104 | `test_stale_device_eviction`, `test_scan_timeout` | discovery.rs:342, 388 |
| T-PT-109 | `t_pt_109_just_works_connect_rejected` | phase1.rs:1045 |
| T-PT-109a | `enforce_lesc_allows_os_enforced_pairing` | transport.rs:233 |
| T-PT-111 | `t_pt_701_progress_reports_phases_in_order` | phase1.rs:895 |
| T-PT-112 | `t_pt_112_verbose_mode` | phase1.rs:1170 |
| T-PT-200 | `t_pt_200_happy_path`, `t_pt_207_mtu_too_low` | phase1.rs:436, 721 |
| T-PT-201 | `t_pt_207_mtu_too_low` | phase1.rs:721 |
| T-PT-202 | `t_pt_200_happy_path` | phase1.rs:436 |
| T-PT-203 | `t_pt_201_signature_verification_failure` | phase1.rs:488 |
| T-PT-204 | `t_pt_204_timeout_gw_info` | phase1.rs:636 |
| T-PT-205 | `t_pt_200_happy_path` | phase1.rs:436 |
| T-PT-206 | `t_pt_202_tofu_mismatch`, `t_pt_209_tofu_gateway_id_mismatch` | phase1.rs:530, 1626 |
| T-PT-208 | `t_pt_200_happy_path` | phase1.rs:436 |
| T-PT-209 | `t_pt_203_registration_window_closed` | phase1.rs:571 |
| T-PT-210 | `t_pt_203b_already_paired_with_gateway` | phase1.rs:605 |
| T-PT-211 | `t_pt_205_timeout_phone_registered` | phase1.rs:656 |
| T-PT-212 | `t_pt_206_decryption_failure` | phase1.rs:686 |
| T-PT-213 | `ephemeral_key_zeroed_on_drop`, `ecdh_shared_secret_zeroed_on_drop` | crypto.rs:314, 341 |
| T-PT-300 | `t_pt_301_not_paired` | phase2.rs:321 |
| T-PT-301 | `t_pt_300_happy_path`, `t_pt_500d_mtu_too_low` | phase2.rs:285, 1277 |
| T-PT-302 | via `MockRng` injection in all Phase 2 tests | phase2.rs:285+ |
| T-PT-303 | `key_hint_uses_last_two_bytes`, `key_hint_deterministic` | validation.rs:88, 80 |
| T-PT-304 | `deterministic_encoding`, `round_trip_pairing_request` | cbor.rs (tests) |
| T-PT-305 | `empty_node_id`, `node_id_too_long`, `valid_node_id` | validation.rs:54, 60, 45 |
| T-PT-306 | `valid_rf_channels`, `invalid_rf_channels` | validation.rs:66, 73 |
| T-PT-307 | Phase 2 happy path constructs HMAC-authenticated payload | phase2.rs:285 |
| T-PT-308 | Phase 2 happy path performs ECDH+HKDF+AES-GCM encryption | phase2.rs:285 |
| T-PT-309 | `t_pt_309_low_order_point_rejected`, `t_pt_309_valid_key_succeeds` | crypto.rs:282, 298 |
| T-PT-310 | `t_pt_307_payload_too_large` | phase2.rs:510 |
| T-PT-311 | `t_pt_300_happy_path` | phase2.rs:285 |
| T-PT-312 | `t_pt_303_already_paired` | phase2.rs:375 |
| T-PT-313 | `t_pt_304_storage_error` | phase2.rs:409 |
| T-PT-314 | `t_pt_305_timeout` | phase2.rs:443 |
| T-PT-315 | Structural (Zeroizing types in phase2 code) | crypto.rs:314 |
| T-PT-402 | `t_pt_402_disconnect_on_phase1_failure`, `t_pt_402_disconnect_on_phase2_failure`, `t_pt_402_disconnect_on_phase2_success` | phase1.rs:999, phase2.rs:563, 603 |
| T-PT-500 | `t_pt_600_repeated_phase1_does_not_corrupt_state` | phase1.rs:754 |
| T-PT-501 | `t_pt_303_already_paired` | phase2.rs:375 |
| T-PT-600 | `save_and_load_round_trip` | store.rs:100, file_store.rs:485 |
| T-PT-601 | `save_and_load_round_trip` (uses MockPairingStore) | store.rs:100 |
| T-PT-603 | `no_node_psk_in_json` | file_store.rs (tests) |
| T-PT-606 | `t_pt_606_dpapi_round_trip`, `t_pt_606_dpapi_tampered_data_fails`, `t_pt_606_dpapi_different_keys_differ` | dpapi.rs:148, 169, 186 |
| T-PT-800 | `t_pt_800_phase1_connection_dropped_during_read`, `t_pt_800_phase2_disconnect_mid_provision` | phase1.rs:1228, phase2.rs:732 |
| T-PT-801 | `t_pt_801_no_resource_leaks_phase1`, `t_pt_801_no_resource_leaks_phase2` | phase1.rs:1288, phase2.rs:774 |
| T-PT-803 | `t_pt_803_no_implicit_retries_phase1_write`, `...read`, `...phase2_write`, `...phase2_read` | phase1.rs:1458, 1490; phase2.rs:905, 946 |
| T-PT-804 | `t_pt_804_numeric_comparison_enforced`, `t_pt_804_numeric_comparison_enforced_phase2` | phase1.rs:1076, phase2.rs:646 |
| T-PT-805 | `t_pt_805_just_works_fallback_rejected`, `t_pt_805_just_works_rejected_phase2` | phase1.rs:1125, phase2.rs:684 |
| T-PT-900 | `hkdf_deterministic`, `cross_phase_hkdf_info_swap_fails_decryption` | crypto.rs:249, 367 |
| T-PT-901 | `hkdf_different_info_differs`, `cross_phase_hkdf_info_swap_fails_decryption` | crypto.rs:260, 367 |
| T-PT-902 | `aes256gcm_round_trip`, `aes256gcm_wrong_aad_fails` | crypto.rs:172, 195 |
| T-PT-903 | `deterministic_encoding`, `definite_length_cbor_containers` | cbor.rs (tests) |
| T-PT-1004 | `t_pt_1004_core_types_available_without_features` | lib.rs:58 |

### Unmatched test functions (backward traceability)

The following test functions in the test code do not map directly to a specific T-PT-NNN in the validation plan:

| Test function | File | Classification |
|---|---|---|
| `test_scan_start_stop_lifecycle` | discovery.rs:246 | Exploratory — scanner lifecycle |
| `test_start_while_scanning_returns_error` | discovery.rs:260 | Exploratory — edge case |
| `test_stop_when_not_scanning_is_noop` | discovery.rs:273 | Exploratory — edge case |
| `test_service_type_gateway_takes_precedence` | discovery.rs:331 | Exploratory — priority rule |
| `test_refresh_updates_device_info` | discovery.rs:367 | Exploratory — RSSI update |
| `test_into_transport_returns_transport` | discovery.rs:402 | Exploratory — API |
| `test_start_clears_known_devices` | discovery.rs:412 | Exploratory — reset behavior |
| All `envelope.rs` tests (17) | envelope.rs | Exploratory — wire format parsing |
| All `fragmentation.rs` tests (17) | fragmentation.rs | Exploratory — BLE fragmentation |
| All `file_store.rs` tests (25) | file_store.rs | Regression — file store coverage |
| All `loopback_transport.rs` tests (5) | loopback_transport.rs | Exploratory — loopback transport |
| All `android_transport.rs` tests (4) | android_transport.rs | Exploratory — UUID formatting |
| `t_pt_210_error_0x01_at_gw_info` | phase1.rs:1669 | Exploratory — generic error |
| `t_pt_211_error_0x01_at_phone_registered` | phase1.rs:1699 | Exploratory — generic error |
| `t_pt_213_request_gw_info_body_is_32_bytes` | phase1.rs:1841 | Exploratory — wire format |
| `t_pt_208_challenge_uniqueness` | phase1.rs:1525 | Exploratory — RNG independence |
| `t_pt_212_fresh_ephemeral_per_attempt` | phase1.rs:1741 | Exploratory — ephemeral key freshness |
| `t_pt_309_fresh_ephemeral_per_attempt` | phase2.rs:1064 | Exploratory — Phase 2 ephemeral key |
| `t_pt_500_unknown_node_ack_status` | phase2.rs:1174 | Exploratory — unknown status |
| `t_pt_500b_node_error_empty_diagnostic` | phase2.rs:1206 | Exploratory — empty diagnostic |
| `t_pt_500c_unexpected_msg_type` | phase2.rs:1242 | Exploratory — invalid msg type |
| `t_pt_0408_error_path_no_panic` | phase2.rs:992 | Exploratory — error path |
| `t_pt_0408_timeout_no_panic` | phase2.rs:1031 | Exploratory — timeout path |

**Total unmatched tests: ~100+** (mostly exploratory/regression tests extending validation plan coverage).

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|---|---|---|---|---|
| 1 | F-007 (T-PT-109b) | Add `enforce_lesc` test with `PairingMethod::Unknown` | S | Security path untested |
| 2 | F-014 (T-PT-700) | Add `#[traced_test]` at default level, assert no key material in Phase 1 + Phase 2 logs | M | Security logging gap |
| 3 | F-015 (T-PT-701) | Extend `t_pt_112_verbose_mode` to cover Phase 2 and additional key patterns | S | Security logging gap |
| 4 | F-008 (T-PT-400) | Add error classification test triggering 3 error categories | S | PT-0500 untested |
| 5 | F-009 (T-PT-401) | Add actionable error message content assertions | M | Operator UX |
| 6 | F-010 (T-PT-207) | Add TOFU clear-and-re-pair integration test | S | TOFU flow gap |
| 7 | F-011 (T-PT-208a) | Add phone label boundary validation test (64, 65, empty) | S | Validation gap |
| 8 | F-017 (T-PT-802) | Add timeout constant value assertions | S | Spec compliance |
| 9 | F-001–F-006 (T-PT-1207–1212) | Add 6 `#[traced_test]` tests for diagnostic logging | L | Diagnostic coverage |
| 10 | F-012 (T-PT-602) | Add corruption-reset test or reclassify as manual | S | Persistence |
| 11 | F-016 (T-PT-702) | Add `clippy::disallowed_methods` for `rand::rng` | S | Process guard |
| 12 | F-018 (T-PT-703) | Add source-grep test or CI lint for `[0u8; 32]` PSK usage | S | Convention |

## 7. Prevention

- **Process:** Add a T-PT-NNN comment to every test function that implements a validation plan test case. This enables automated traceability verification.
- **CI:** Add a CI step that greps for all T-PT-NNN IDs in the validation plan and verifies each has at least one test function referencing it.
- **Tooling:** Configure `#![deny(clippy::disallowed_methods)]` for `rand::rng` in `sonde-pair` to enforce CSPRNG-only randomness.
- **Code review:** When adding new PT-NNNN requirements with PT-12xx (diagnostic logging) patterns, ensure corresponding `#[traced_test]` tests are created in the same PR.

## 8. Open Questions

1. **Timeout constant assertions (F-017):** The mock transport does not exercise real timeout durations. Should a dedicated constants test be added, or should the transport mock be enhanced to verify timeout values passed to it?
2. **T-PT-702 lint enforcement (F-016):** Is `clippy::disallowed_methods` configured at the workspace level (outside the files examined)? If so, criterion 4 of T-PT-702 may already be satisfied.
3. **T-PT-602 reset offer (F-012):** Is the "offer to reset" behavior a core library concern or a UI concern? If UI-only, should T-PT-602 criterion 3 be reclassified as manual in the validation plan?

## 9. Revision History

| Version | Date | Author | Changes |
|---|---|---|---|
| 1.0 | 2025-07-12 | Audit (automated) | Initial audit report — Round 2 |
