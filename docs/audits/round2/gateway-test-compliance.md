<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# Gateway Test Compliance Audit — Investigation Report

## 1. Executive Summary

This audit examined 131 test cases from the gateway validation
plan (`gateway-validation.md`) — 130 automatable and 1 manual/deferred — against 311 test functions in the
`sonde-gateway` crate.**112 test cases are fully implemented, 5 are
partially implemented with assertion gaps, and 14 have no implementing
test.** The primary gaps are in admin CLI integration tests (T-0812,
T-0816, T-0817), modem admin API tests (T-0813–T-0815), serial
reconnection tests (T-1104a/b), and the export plaintext-leakage test
(T-1005). Recommended action: implement the 14 missing tests (D11
findings) and close the 5 assertion gaps (D12/D13 findings) before the
next release.

---

## 2. Problem Statement

The validation plan defines the authoritative set of test cases for the
gateway component. Test compliance drift — where automated tests diverge
from what the validation plan specifies — creates false confidence in
test coverage. This audit systematically traces every validation plan
test case to its implementing test function and verifies that assertions
match the specified expected results.

**Impact:** Unimplemented or partially implemented tests leave
requirements unverified in CI. Assertion mismatches create illusory
coverage where the traceability matrix shows a requirement as tested but
the actual test checks something different.

---

## 3. Investigation Scope

- **Codebase examined:**
  - `crates/sonde-gateway/tests/` — 14 test files (311 total test
    functions across integration and unit tests)
  - `crates/sonde-gateway/src/` — `#[cfg(test)]` modules in 9 source
    files (152 unit tests)
- **Documents examined:**
  - `docs/gateway-validation.md` — 131 test cases (130 automatable + 1 manual/deferred)
    (T-0100 through T-1304)
  - `docs/gateway-requirements.md` — 83 requirements (GW-0100 through
    GW-1304, including GW-0601a/GW-0601b)
- **Tools used:** Grep/glob pattern search, source file inspection,
  cross-reference of test function names against T-XXXX identifiers
- **Limitations:**
  - Platform-specific tests (T-0603f DPAPI on Windows, T-0603h/i Secret
    Service on Linux) were verified structurally but not executed.
  - T-1301 (GW-1301, modem transport state logging) is marked in the
    validation plan as "verified by integration/manual testing" and is
    excluded from automated test audit.
  - T-1304 (build metadata in `--version`) is a build/binary-level test
    that requires running compiled binaries; structural presence was
    checked.

---

## 4. Findings

### Finding F-001: T-0303 — Invalid chunk_index in GET_CHUNK

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §5 (T-0303); Test code: None — no
  implementing test found
- **Description**: T-0303 specifies sending a `GET_CHUNK` with
  `chunk_index=4` for a program with `chunk_count=4` and asserting
  silent discard (no CHUNK response), then sending `chunk_index=3`
  and asserting a valid CHUNK response. No test function implements
  this behavior.
- **Evidence**: Searched for `t0303`, `invalid_chunk`, `chunk_index`,
  `out_of_range` across all test files. The chunked transfer tests
  (`t0300`, `t0301`, `t0302` in `phase2b.rs`) only test the happy path
  and resumption — none send an out-of-range index.
- **Root Cause**: Gap in test implementation; out-of-bound chunk
  requests are untested.
- **Impact**: A regression that causes the gateway to respond to
  invalid chunk indices (or crash) would not be caught by CI.
- **Remediation**: Add a test in `phase2b.rs` that sends
  `GET_CHUNK { chunk_index: chunk_count }` and asserts no CHUNK
  response, then sends `GET_CHUNK { chunk_index: chunk_count - 1 }`
  and asserts a valid response.
- **Confidence**: High

---

### Finding F-002: T-0812 — Admin CLI integration

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §9A (T-0812); Test code: None — no
  implementing test found
- **Description**: T-0812 specifies running `sonde-admin` CLI commands
  (`node list`, `node register`, `node remove`) against a running
  gateway and verifying JSON output and exit codes. No test implements
  this end-to-end CLI integration test.
- **Evidence**: Searched for `t0812`, `cli_integration`, `sonde-admin`
  in test invocations. The admin API tests in `phase2c_admin.rs` test
  the gRPC layer programmatically but do not invoke the CLI binary.
- **Root Cause**: CLI integration tests typically require a compiled
  binary and a running gateway; these are harder to automate in unit
  test suites.
- **Impact**: CLI argument parsing, output formatting, and error
  reporting regressions would not be caught.
- **Remediation**: Add a CLI integration test (possibly in `sonde-e2e`)
  that starts a gateway, runs `sonde-admin` commands, and asserts exit
  codes and output. Linked requirement: GW-0806.
- **Confidence**: High

---

### Finding F-003: T-0816 — Admin CLI JSON output

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §9A (T-0816); Test code: None — no
  implementing test found
- **Description**: T-0816 specifies running `sonde-admin node list
  --format json` and `sonde-admin program list --format json` and
  verifying the output is valid JSON containing the registered node /
  ingested program. No test exists.
- **Evidence**: Searched for `t0816`, `cli_json`, `format json` in all
  test files. No matches.
- **Root Cause**: Same as F-002 — CLI tests require binary invocation.
- **Impact**: JSON serialization regressions in CLI output would not be
  caught.
- **Remediation**: Include in the CLI integration test recommended in
  F-002. Linked requirement: GW-0806.
- **Confidence**: High

---

### Finding F-004: T-0817 — Admin CLI error handling

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §9A (T-0817); Test code: None — no
  implementing test found
- **Description**: T-0817 specifies running `sonde-admin node get
  nonexistent-node` and asserting non-zero exit code and meaningful
  error message. No test exists.
- **Evidence**: Searched for `t0817`, `cli_error` in all test files.
  No matches.
- **Root Cause**: Same as F-002.
- **Impact**: Missing error handling could result in silent failures or
  confusing exit codes.
- **Remediation**: Include in the CLI integration test recommended in
  F-002. Linked requirement: GW-0806.
- **Confidence**: High

---

### Finding F-005: T-0813 — Modem status via admin API

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §9A (T-0813); Test code: None — no
  implementing test found
- **Description**: T-0813 specifies calling `GetModemStatus` via the
  admin API and asserting the response contains radio channel, counters,
  and uptime. No test implements this admin API call against a modem.
- **Evidence**: Searched for `t0813`, `GetModemStatus`,
  `modem_status.*admin` across all test files. The modem transport tests
  in `modem_transport.rs` and `phase2d.rs` test the transport layer
  directly but not through the gRPC admin API.
- **Root Cause**: Admin API modem endpoints may not yet be fully wired
  in tests.
- **Impact**: Admin API regressions for modem status reporting would not
  be caught. Linked requirement: GW-0807.
- **Remediation**: Add integration tests that call modem admin RPCs
  through the gRPC API with a mock modem backend.
- **Confidence**: High

---

### Finding F-006: T-0814 — Modem channel change via admin API

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §9A (T-0814); Test code: None — no
  implementing test found
- **Description**: T-0814 specifies calling `SetModemChannel(6)` and
  verifying the channel change propagates to the modem. No test
  implements this. `gw1106_change_channel_success` in `src/modem.rs`
  tests channel change at the transport layer but not through admin API.
- **Evidence**: Searched for `t0814`, `SetModemChannel` in test files.
  No admin API invocation found.
- **Root Cause**: Same as F-005.
- **Impact**: Same as F-005. Linked requirement: GW-0807.
- **Remediation**: Same as F-005.
- **Confidence**: High

---

### Finding F-007: T-0815 — Modem channel scan via admin API

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §9A (T-0815); Test code: None — no
  implementing test found
- **Description**: T-0815 specifies calling `ScanModemChannels` and
  asserting per-channel AP counts and RSSI. No test exists.
- **Evidence**: Searched for `t0815`, `ScanModemChannels` in test files.
  No matches.
- **Root Cause**: Same as F-005.
- **Impact**: Same as F-005. Linked requirement: GW-0807.
- **Remediation**: Same as F-005.
- **Confidence**: High

---

### Finding F-008: T-1005 — Export plaintext key leakage

- **Severity**: High
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §10 (T-1005); Test code: None — no
  implementing test found
- **Description**: T-1005 specifies registering nodes with known PSKs,
  exporting state with a passphrase, inspecting raw export bytes to
  confirm no PSK appears as a contiguous substring, then verifying that
  import without the correct passphrase fails. No test directly inspects
  the export bundle bytes for plaintext PSK leakage.
- **Evidence**: `state_bundle.rs` tests cover round-trip correctness,
  wrong-passphrase rejection, and tamper detection. The export tests in
  `phase2c_admin.rs` (`t0810_export_state_returns_encrypted_bundle`)
  verify the bundle is non-empty and import round-trips correctly, but
  do NOT scan the raw bytes for plaintext PSK presence.
- **Root Cause**: The security-critical raw-byte inspection step was not
  implemented.
- **Impact**: A regression that accidentally includes plaintext keys in
  the export bundle (e.g., a logging or debugging change that bypasses
  encryption) would not be caught. This is a security requirement
  (GW-1001 AC3).
- **Remediation**: Add a test that registers a node with a known PSK
  (e.g., `[0x42; 32]`), exports state, and asserts the 32-byte PSK
  does not appear as a contiguous substring in the export bytes. Also
  test that import with the wrong passphrase is rejected.
- **Confidence**: High

---

### Finding F-009: T-1104a — Serial disconnect reconnection with backoff

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §11 (T-1104a); Test code: None — no
  implementing test found
- **Description**: T-1104a specifies simulating a USB-CDC disconnect by
  closing the PTY slave fd, verifying the transport enters a
  reconnection loop with exponential backoff, reopening the PTY, and
  verifying the startup sequence re-executes. No test implements this
  full disconnect/reconnect cycle.
- **Evidence**: Searched for `t1104a`, `reconnect`, `backoff`,
  `disconnect` in test files. `t1104_startup_modem_ready_timeout` tests
  startup timeout but not disconnect/reconnect.
  `t1107a_modem_reset_recovery` tests ERROR recovery but not serial
  disconnect.
- **Root Cause**: Serial disconnect simulation requires PTY
  manipulation that may be complex to automate cross-platform.
- **Impact**: A regression in the reconnection logic (e.g., the gateway
  exiting on serial disconnect instead of reconnecting) would not be
  caught. Linked requirement: GW-1103 (criteria 3–5).
- **Remediation**: Implement a PTY-based test on Linux that closes the
  slave fd, verifies reconnection behavior, and confirms frame
  processing resumes after reconnection.
- **Confidence**: High

---

### Finding F-010: T-1104b — Serial disconnect frame loop survives

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §11 (T-1104b); Test code: None — no
  implementing test found
- **Description**: T-1104b specifies starting a full gateway instance
  with a PTY-based MockModem, disconnecting the modem, and asserting
  the frame processing and BLE event loops do not exit. No test
  implements this.
- **Evidence**: Same search as F-009. No matches.
- **Root Cause**: Same as F-009.
- **Impact**: The gateway process exiting on modem disconnect would
  require manual restart, violating GW-1103 AC5.
- **Remediation**: Same as F-009 — can be combined into a single test.
- **Confidence**: High

---

### Finding F-011: T-1218b — Duplicate PEER_REQUEST with different PSK

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §12 (T-1218b); Test code: None — no
  implementing test found
- **Description**: T-1218b specifies submitting a `PEER_REQUEST` with
  the same `node_id` as an already-registered node but a **different**
  `node_psk`, and asserting the frame is silently discarded. No test
  exercises this specific scenario with a different PSK.
- **Evidence**: `peer_request_duplicate_node_id` (line 546 in
  `peer_request.rs`) and `t_1216_duplicate_node_id_rejected` (line
  1065) both test the matching-PSK case (T-1218a) but neither sends a
  duplicate with a different PSK.
- **Root Cause**: The duplicate-with-different-PSK negative path was
  not implemented.
- **Impact**: A regression that accepts conflicting PSKs for the same
  node_id would not be caught. Linked requirement: GW-1218 AC5.
- **Remediation**: Add a test in `peer_request.rs` that registers a
  node with PSK A, then submits a PEER_REQUEST with the same node_id
  but PSK B, and asserts no PEER_ACK is sent.
- **Confidence**: High

---

### Finding F-012: T-1227 — Phone listing via admin API

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §12 (T-1227); Test code: None — no
  implementing test found at the admin API layer
- **Description**: T-1227 specifies calling `ListPhones` via the admin
  API after registering two phones and asserting both appear with
  correct metadata. While `test_phone_psk_store_and_list` in
  `sqlite_storage.rs` tests the storage layer, no test exercises the
  gRPC `ListPhones` RPC.
- **Evidence**: Searched for `t1227`, `ListPhones`, `list_phones` in
  integration test files. Only storage-layer tests exist.
- **Root Cause**: Admin API phone management RPCs are not yet tested
  through the gRPC layer.
- **Impact**: Regressions in the gRPC serialization or routing of phone
  listing would not be caught. Linked requirement: GW-1223.
- **Remediation**: Add an integration test in `phase2c_admin.rs` that
  calls `ListPhones` via gRPC after registering phones.
- **Confidence**: High

---

### Finding F-013: T-1228 — Phone revocation via admin API

- **Severity**: Medium
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §12 (T-1228); Test code: None — no
  implementing test found at the admin API layer
- **Description**: T-1228 specifies calling `RevokePhone` via the admin
  API and verifying subsequent `PEER_REQUEST` with the revoked PSK is
  silently discarded. While `test_phone_psk_revocation` in
  `sqlite_storage.rs` tests the storage layer, no test exercises the
  gRPC `RevokePhone` RPC end-to-end.
- **Evidence**: Searched for `t1228`, `RevokePhone` in integration test
  files. Only storage-layer tests exist.
- **Root Cause**: Same as F-012.
- **Impact**: Revocation flow regressions at the API layer would not be
  caught. Linked requirement: GW-1224.
- **Remediation**: Add an integration test in `phase2c_admin.rs` that
  calls `RevokePhone` via gRPC and then submits a PEER_REQUEST with the
  revoked PSK.
- **Confidence**: High

---

### Finding F-014: T-1304 — Build metadata in `--version` output

- **Severity**: Low
- **Category**: D11_UNIMPLEMENTED_TEST_CASE
- **Location**: Validation plan §13 (T-1304); Test code: None — no
  implementing test found
- **Description**: T-1304 specifies building `sonde-gateway` and
  `sonde-admin` from a git checkout and asserting `--version` output
  matches the pattern `<semver> (<7-char-hash>)`. No automated test
  verifies this.
- **Evidence**: Searched for `t1304`, `build_metadata`, `version` in
  test files. No test invokes the compiled binary with `--version`.
- **Root Cause**: Version string tests require running the compiled
  binary, which is better suited for CI pipeline tests than unit tests.
- **Impact**: Low — build metadata regressions are typically caught
  during release preparation.
- **Remediation**: Add a CI-level test or integration test that builds
  the binary and asserts the version output format.
- **Confidence**: High

---

### Finding F-015: T-0512 — EVENT messages (node_online, program_updated)

- **Severity**: High
- **Category**: D13_ASSERTION_MISMATCH
- **Location**: Validation plan §7 (T-0512);
  `tests/phase2c.rs::t0512_handler_no_crash_on_wake` (line 1740)
- **Description**: T-0512 requires asserting that the handler receives
  an EVENT with `event_type="node_online"` containing `battery_mv` and
  `firmware_abi_version` after WAKE, and an EVENT with
  `event_type="program_updated"` containing `program_hash` after
  PROGRAM_ACK. The implementing test only asserts that WAKE returns a
  `Nop` command and APP_DATA produces a reply — it does not verify any
  EVENT messages.
- **Evidence**: The test function (line 1740) asserts
  `CommandPayload::Nop` and `GatewayMessage::AppDataReply` only. A code
  comment near line 1737 states "EVENT forwarding from engine to handler
  is not wired in Phase 2C-i." No EVENT content is checked.
- **Root Cause**: EVENT forwarding to handlers was deferred; the test
  was written as a smoke test placeholder.
- **Impact**: EVENT message delivery regressions (node_online,
  program_updated) are not caught. Linked requirement: GW-0507.
- **Remediation**: Update the test to verify EVENT messages are
  delivered to the handler with correct `event_type`, `battery_mv`,
  `firmware_abi_version`, and `program_hash` fields.
- **Confidence**: High

---

### Finding F-016: T-0513 — LOG messages from handler

- **Severity**: Medium
- **Category**: D13_ASSERTION_MISMATCH
- **Location**: Validation plan §7 (T-0513);
  `tests/phase2c.rs::t0513_log_messages_no_crash` (line 1775)
- **Description**: T-0513 requires asserting that a handler LOG message
  (`level: "info"`, `message: "test log"`) appears in the gateway log
  output with the correct level. The implementing test only asserts the
  gateway does not crash and that a subsequent APP_DATA reply echoes
  correctly. It does not verify any log output content.
- **Evidence**: The test function (line 1775) asserts `reply_blob ==
  blob` (data echo correctness) but does not use `#[traced_test]` or
  check `logs_contain()` for the handler's LOG message in gateway
  output.
- **Root Cause**: LOG message routing from handler to gateway logging
  may not have been fully wired at the time the test was written.
- **Impact**: LOG message handling regressions would not be caught.
  Linked requirement: GW-0508.
- **Remediation**: Add `#[traced_test]` and assert that the gateway
  log output contains the handler's log message with the correct level.
- **Confidence**: High

---

### Finding F-017: T-0206 — Ephemeral size budget exceeded at dispatch

- **Severity**: Medium
- **Category**: D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location**: Validation plan §4 (T-0206);
  `tests/phase2b.rs::t0202b_ephemeral_size_budget_rejected` (line 510)
- **Description**: T-0206 specifies that when an ephemeral program
  exceeds the 2 KB budget, the gateway does NOT issue RUN_EPHEMERAL,
  logs an error, and on the next WAKE falls through to the next pending
  command (or NOP). The test verifies rejection (NOP returned) and
  logging, but does not test fall-through to a next queued command — it
  only verifies NOP when ephemeral is the sole queued command.
- **Evidence**: `t0202b_ephemeral_size_budget_rejected` (line 510)
  queues only the oversized ephemeral. T-0206 step 5 says "gateway
  falls through to next pending command (or NOP)" — the "(or NOP)" case
  is tested but not the "next pending command" case.
- **Root Cause**: The test covers the simpler NOP fallback but not the
  command-priority fall-through scenario.
- **Impact**: A regression where the gateway gets stuck on the rejected
  ephemeral instead of falling through to the next command would not be
  caught.
- **Remediation**: Extend the test to also queue a schedule change or
  program update alongside the oversized ephemeral, and verify the
  gateway falls through to the next command.
- **Confidence**: High

---

### Finding F-018: T-0515 — Long-running handler persistence (PID check)

- **Severity**: Low
- **Category**: D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location**: Validation plan §7 (T-0515);
  `tests/phase2c.rs::gw0503_ac3_persistent_handler_stays_alive`
  (line 2073)
- **Description**: T-0515 specifies asserting that the same handler
  process receives both messages — "same PID when using a subprocess,
  or the same test-assigned instance ID for an in-process mock." The
  implementing test proves persistence through incrementing counter
  state (counter goes 1→2→3, proving no respawn) but does not
  explicitly assert a stable PID or instance ID.
- **Evidence**: Lines 2104–2110 assert counter values but not process
  identity. The behavioral proof is equivalent but not structurally
  identical to the validation plan's requirement.
- **Root Cause**: The test uses state persistence as an implicit proxy
  for handler identity, which is functionally valid but not the explicit
  check specified.
- **Impact**: Low — the state-based proof is arguably stronger than a
  PID check since it verifies actual process state continuity.
- **Remediation**: Consider adding an explicit handler instance ID
  assertion to match the validation plan exactly, or update the
  validation plan to accept state-based proof as equivalent.
- **Confidence**: Medium — the existing test is functionally sufficient
  but structurally different from the plan.

---

### Finding F-019: T-0516 — Handler hang timeout (no blocking assertion)

- **Severity**: Low
- **Category**: D12_UNTESTED_ACCEPTANCE_CRITERION
- **Location**: Validation plan §7 (T-0516);
  `tests/phase2c.rs::t0503c_handler_reply_timeout` (line 1869)
- **Description**: T-0516 specifies asserting that the gateway does not
  block processing for other nodes when a handler hangs. The test
  verifies timeout behavior (no reply returned after 2 seconds) but
  does not test concurrent node processing during the timeout period.
- **Evidence**: Lines 1895–1900 assert `process_frame()` returns `None`
  after timeout. No second node is tested concurrently.
- **Root Cause**: Testing concurrent non-blocking behavior requires a
  multi-node test setup, which is more complex.
- **Impact**: Low — the async architecture makes blocking unlikely, but
  a regression that serializes handler processing would not be caught.
- **Remediation**: Extend the test to process a second node's frame
  during the first node's handler timeout and assert the second node
  receives a timely response.
- **Confidence**: High

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Count | Rate |
|--------|------:|-----:|
| Total validation plan test cases | 131 | — |
| Automatable test cases | 130 | — |
| Manual/deferred test cases | 1 (T-1301, GW-1301) | — |
| **Implemented (full)** | **112** | **86.2%** |
| **Partially implemented** | **5** | **3.8%** |
| **Not implemented (D11)** | **14** | **10.8%** |
| Test cases with complete assertions | 112 | 95.7% of implemented |
| Test cases with assertion gaps | 5 | 4.3% of implemented |
| Unmatched test functions (not in validation plan) | ~128 | — |

### Acceptance Criteria Coverage

| Requirement area | Criteria tested | Criteria total | Rate |
|------------------|----------------:|---------------:|-----:|
| Protocol/comms (GW-0100–0104) | 10 | 10 | 100% |
| Command set (GW-0200–0204) | 9 | 9 | 100% |
| Chunked transfer (GW-0300–0302) | 6 | 7 | 86% |
| BPF program mgmt (GW-0400–0405) | 16 | 16 | 100% |
| App data/handlers (GW-0500–0508) | 17 | 21 | 81% |
| Auth/security (GW-0600–0603) | 13 | 13 | 100% |
| Node management (GW-0700–0705) | 9 | 10 | 90% |
| Admin API (GW-0800–0807) | 15 | 22 | 68% |
| Operational (GW-1000–1003) | 6 | 6 | 100% |
| Modem transport (GW-1100–1103) | 10 | 12 | 83% |
| BLE pairing (GW-1200–1224) | 37 | 39 | 95% |
| Logging (GW-1300–1304) | 5 | 8 | 63% |

### Systemic Patterns

The 14 D11 findings cluster into three categories:

1. **Admin CLI integration tests** (F-002, F-003, F-004): All three
   require invoking the `sonde-admin` binary against a running gateway.
   These are harder to automate in the Rust `#[test]` framework and are
   likely candidates for a dedicated CLI integration test suite or
   `sonde-e2e`.

2. **Admin API modem endpoints** (F-005, F-006, F-007): The modem admin
   RPCs (`GetModemStatus`, `SetModemChannel`, `ScanModemChannels`) are
   tested at the transport layer but not through the gRPC admin API.

3. **Serial reconnection** (F-009, F-010): PTY-based disconnect
   simulation is platform-specific and complex to automate.

---

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-008 | Add T-1005 export plaintext key leakage test | S | Security gap |
| 2 | F-015 | Fix T-0512 to verify EVENT messages (node_online, program_updated) | M | Illusory coverage |
| 3 | F-011 | Add T-1218b duplicate PEER_REQUEST different PSK test | S | Security gap |
| 4 | F-016 | Fix T-0513 to verify LOG message appears in gateway logs | S | Illusory coverage |
| 5 | F-001 | Add T-0303 invalid chunk_index test | S | Protocol gap |
| 6 | F-017 | Extend T-0206 to test fall-through to next command | S | Partial coverage |
| 7 | F-005–007 | Add T-0813/T-0814/T-0815 modem admin API tests | M | Admin API gap |
| 8 | F-012–013 | Add T-1227/T-1228 phone admin API tests | M | Admin API gap |
| 9 | F-009–010 | Add T-1104a/T-1104b serial reconnection tests | L | Platform-specific |
| 10 | F-002–004 | Add T-0812/T-0816/T-0817 CLI integration tests | L | Test infrastructure |
| 11 | F-014 | Add T-1304 build metadata version test | S | Low impact |
| 12 | F-018 | Add explicit handler instance ID to T-0515 | S | Low impact |
| 13 | F-019 | Add concurrent node test to T-0516 | M | Low impact |

---

## 7. Prevention

- **Code changes:** Require test case IDs in test function names or doc
  comments (e.g., `/// T-0303`) to enable automated traceability
  scanning.
- **Process changes:** Add a pre-merge CI check that extracts T-XXXX
  IDs from the validation plan and verifies each has a matching test
  function. Flag new validation plan entries without implementing tests.
- **Tooling:** Consider a script that parses `gateway-validation.md`
  for T-XXXX entries and greps the test code for corresponding test
  functions, reporting any unmatched IDs. This audit's search patterns
  can serve as the basis.

---

## 8. Open Questions

1. **T-0512 EVENT forwarding status:** The code comment says "EVENT
   forwarding from engine to handler is not wired in Phase 2C-i." Is
   this forwarding now wired? If not, the test cannot be fixed until
   the implementation is complete. Verify by checking whether
   `GatewayMessage::Event` messages are delivered to handlers.

2. **CLI integration test infrastructure:** Is there an existing test
   harness for running `sonde-admin` against a live gateway, or does
   one need to be built? The `sonde-e2e` crate may be the appropriate
   home.

3. **Serial reconnection test feasibility on Windows:** T-1104a/b
   specify PTY-based testing. Is PTY simulation available on Windows,
   or should these tests be Linux-only (`#[cfg(target_os = "linux")]`)?

4. **T-1301 (modem transport state logging):** The validation plan
   marks this as "verified by integration/manual testing." Should an
   automated test be added, or is manual verification acceptable for
   this requirement?

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-26 | Copilot (audit) | Initial audit — round 2 |
