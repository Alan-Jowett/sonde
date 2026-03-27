<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# Node Firmware Code Compliance — Investigation Report

## 1. Executive Summary

A code compliance audit of the sonde-node firmware was performed against
the Node Requirements Specification (`node-requirements.md`, 51
requirements ND-0100 through ND-1012) and the Node Design Specification
(`node-design.md`). The implementation is broadly compliant: **48 of 51
requirements are fully implemented**, with **1 partially implemented
(ND-1008)** and **2 "Should"-priority requirements not yet implemented
(ND-0403, ND-0403a)**. Three findings are reported below, all Medium
severity or lower. No Critical or High severity gaps were identified.

## 2. Problem Statement

The goal is to determine whether the sonde-node firmware source code
faithfully implements the behaviors, constraints, and interfaces
specified in the requirements and design documents. Gaps in either
direction — specified-but-not-built (D8) or built-but-not-specified (D9)
— are findings. Constraint violations in code (D10) are also findings.

## 3. Investigation Scope

- **Codebase / components examined**:
  - `crates/sonde-node/src/` — all 21 source files (lib.rs,
    wake_cycle.rs, crypto.rs, error.rs, traits.rs, sleep.rs, hal.rs,
    bpf_runtime.rs, bpf_dispatch.rs, bpf_helpers.rs, program_store.rs,
    map_storage.rs, key_store.rs, sonde_bpf_adapter.rs, ble_pairing.rs,
    esp_ble_pairing.rs, esp_transport.rs, esp_storage.rs, esp_sleep.rs,
    esp_hal.rs, peer_request.rs)
  - `crates/sonde-node/Cargo.toml` — feature flags and log-level
    configuration
  - `crates/sonde-node/sdkconfig.defaults` — ESP-IDF build-time
    configuration
  - `sdkconfig.defaults.esp32s3` — S3-specific ESP-IDF configuration
  - `crates/sonde-protocol/src/` — shared constants, codec, messages
    (referenced for ND-0101, ND-0102, ND-0103)
  - `crates/sonde-node/src/bin/node.rs` — firmware entry point (ND-1000,
    ND-0901)
- **Specification documents examined**:
  - `docs/node-requirements.md` (51 requirements)
  - `docs/node-design.md` (17 sections)
- **Tools used**: Static code analysis via grep, glob, and targeted
  file reading. No runtime testing performed.
- **Limitations**: ESP-IDF hardware-dependent behavior (hardware HMAC,
  hardware SHA, ADC readings, deep-sleep entry) cannot be verified by
  static analysis — compliance is assessed by checking that the correct
  ESP-IDF APIs are called with correct arguments. The `bin/node.rs`
  entry point is behind the `esp` feature gate and was examined for
  boot-related requirements.

## 4. Findings

### Finding F-001: BLE pairing mode exit log omits outcome

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `crates/sonde-node/src/esp_ble_pairing.rs:252`
- **Requirement**: ND-1008 AC2 — "On pairing mode exit (disconnect,
  timeout, or failure), a log is emitted indicating that pairing mode
  has exited and the outcome (success, timeout, disconnect, or failure)."
- **Description**: The BLE pairing mode exit log emits:
  ```rust
  info!("BLE: disconnect detected -- exiting pairing mode");
  ```
  This log identifies "disconnect" as the trigger but does **not**
  include the outcome of the pairing exchange — whether a
  `NODE_PROVISION` was successfully processed during the session or not.
  The requirement explicitly asks for the outcome (success, timeout,
  disconnect, or failure) to be part of the exit log.
- **Evidence**: The function `run_ble_pairing_mode()` exits the main
  loop only when a BLE disconnect is detected (line 250–253). The exit
  log at line 252 is a static string with no reference to provisioning
  state. While individual `NODE_PROVISION` successes are logged at
  line 285 (`info!("BLE: NODE_PROVISION handled, status=0x{:02x}")`),
  the exit log itself does not summarize the session outcome. There is
  also no timeout-based exit from the BLE pairing loop; the only exit
  path is disconnect.
- **Root Cause**: The exit log was written as a simple disconnect
  notification without tracking provisioning state across the BLE
  session.
- **Impact**: Operators reviewing logs cannot determine from the exit
  log alone whether a BLE pairing session ended with successful
  provisioning or was an abandoned/failed connection. They must correlate
  with earlier `NODE_PROVISION handled` logs.
- **Remediation**: Track whether at least one `NODE_PROVISION` was
  successfully handled (status 0x00) during the session. Include the
  outcome in the exit log, e.g.:
  ```
  info!("BLE: exiting pairing mode reason=disconnect outcome=provisioned");
  info!("BLE: exiting pairing mode reason=disconnect outcome=no_provision");
  ```
  Also consider adding a configurable BLE advertising timeout
  (e.g., 5 minutes) so the node does not advertise indefinitely if no
  client connects.
- **Confidence**: High

---

### Finding F-002: Secure boot support not implemented

- **Severity**: Low
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: `crates/sonde-node/sdkconfig.defaults`,
  `sdkconfig.defaults.esp32s3`
- **Requirement**: ND-0403 — "The node SHOULD support secure boot to
  prevent unauthorized firmware from running." (Priority: Should)
- **Description**: Neither `sdkconfig.defaults` file contains
  `CONFIG_SECURE_BOOT_ENABLED` or any secure boot configuration. No
  code references secure boot APIs.
- **Evidence**: Searched all `sdkconfig.defaults*` files and all `.rs`
  source files for `SECURE_BOOT`, `secure_boot`, and `signed_firmware`.
  No matches found.
- **Root Cause**: Secure boot is a "Should" priority feature that has
  not yet been implemented. This is likely intentional for the current
  development phase.
- **Impact**: Without secure boot, a physical attacker could flash
  unauthorized firmware to extract the PSK from the key partition.
  This is mitigated partially by flash encryption (also not yet
  implemented — see F-003).
- **Remediation**: When ready, enable
  `CONFIG_SECURE_BOOT_V2_ENABLED=y` in `sdkconfig.defaults` and
  configure signing keys. Document the secure boot key management
  process.
- **Confidence**: High

---

### Finding F-003: Flash encryption support not implemented

- **Severity**: Low
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: `crates/sonde-node/sdkconfig.defaults`,
  `sdkconfig.defaults.esp32s3`
- **Requirement**: ND-0403a — "The node SHOULD support flash encryption
  to prevent physical extraction of the PSK from the flash partition."
  (Priority: Should)
- **Description**: Neither `sdkconfig.defaults` file contains
  `CONFIG_FLASH_ENCRYPTION_ENABLED` or flash encryption configuration.
  No code references flash encryption APIs.
- **Evidence**: Searched all `sdkconfig.defaults*` files and all `.rs`
  source files for `FLASH_ENCRYPTION`, `flash_encryption`, and
  `encrypt`. No matches found for flash encryption configuration.
- **Root Cause**: Flash encryption is a "Should" priority feature that
  has not yet been implemented. This is likely intentional for the
  current development phase.
- **Impact**: Without flash encryption, a physical attacker with JTAG
  or flash-reading hardware could extract the PSK from the key
  partition. Combined with absence of secure boot (F-002), this leaves
  the PSK exposed to physical attacks.
- **Remediation**: When ready, enable
  `CONFIG_FLASH_ENCRYPTION_MODE_DEVELOPMENT=y` (or `RELEASE` for
  production) in `sdkconfig.defaults`. The firmware's key-reading code
  in `esp_storage.rs` should work transparently with flash encryption
  enabled, as ESP-IDF handles transparent decryption.
- **Confidence**: High

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|--------|-------|
| Total requirements | 51 |
| Fully implemented | 48 (94.1%) |
| Partially implemented | 1 (ND-1008) |
| Not implemented (Should priority) | 2 (ND-0403, ND-0403a) |
| D8 findings (unimplemented) | 2 |
| D9 findings (undocumented behavior) | 0 |
| D10 findings (constraint violation) | 1 |
| Must-priority compliance | 48/49 (98.0%) |
| Should-priority compliance | 0/2 (0%) |

### Requirement-by-Requirement Traceability

| Req ID | Title | Priority | Status | Evidence |
|--------|-------|----------|--------|----------|
| ND-0100 | Node-initiated communication | Must | ✅ Implemented | `wake_cycle.rs` — node initiates WAKE; `traits.rs` Transport trait is send/recv only |
| ND-0101 | CBOR message encoding | Must | ✅ Implemented | `sonde-protocol/src/constants.rs` integer key constants; `messages.rs` `cbor_encode_map()` |
| ND-0102 | Frame format compliance | Must | ✅ Implemented | `sonde-protocol/src/codec.rs` `encode_frame()` — 11-byte header + payload + 32-byte HMAC |
| ND-0103 | Frame size constraint | Must | ✅ Implemented | `sonde-protocol/src/constants.rs:8` `MAX_FRAME_SIZE=250`; enforced in `codec.rs` and `wake_cycle.rs` `send_app_data`/`send_recv_app_data` |
| ND-0200 | Wake cycle structure | Must | ✅ Implemented | `wake_cycle.rs` `run_wake_cycle()` — WAKE→COMMAND→dispatch→BPF→sleep |
| ND-0201 | WAKE message | Must | ✅ Implemented | `wake_cycle.rs` `wake_command_exchange()` — includes `firmware_abi_version`, `program_hash`, `battery_mv`, random nonce |
| ND-0202 | COMMAND processing | Must | ✅ Implemented | `wake_cycle.rs` lines 254–380 — all 5 command types handled; unknown types treated as NOP via `decode_command_as_nop()` |
| ND-0203 | Sleep and wake interval | Must | ✅ Implemented | `sleep.rs` `effective_sleep_s()` — `min(override, base)` clamped to `MIN_SLEEP_INTERVAL_S=1` |
| ND-0300 | HMAC-SHA256 authentication | Must | ✅ Implemented | `sonde-protocol/src/codec.rs` `encode_frame()` — HMAC over header+payload |
| ND-0301 | Inbound HMAC verification | Must | ✅ Implemented | `wake_cycle.rs` `verify_and_decode_command()` — HMAC verified; failures return `AuthFailure`, silently discarded |
| ND-0302 | Response binding verification | Must | ✅ Implemented | `wake_cycle.rs` — COMMAND nonce vs WAKE nonce (line 654), CHUNK nonce vs seq (line 888), APP_DATA_REPLY nonce vs seq (line 1080) |
| ND-0303 | Sequence number management | Must | ✅ Implemented | `wake_cycle.rs` — `starting_seq` from COMMAND; `current_seq` incremented per outbound message; not persisted across sleep |
| ND-0304 | Nonce generation | Must | ✅ Implemented | `wake_cycle.rs` line 128 `rng.health_check()`; `traits.rs` `Rng::random_u64()`; `crypto.rs` `EspRng` uses hardware TRNG |
| ND-0400 | PSK storage | Must | ✅ Implemented | `traits.rs` `PlatformStorage::read_key()` / `write_key()`; `esp_storage.rs` dedicated NVS partition |
| ND-0402 | Factory reset | Must | ✅ Implemented | `key_store.rs` `factory_reset()` — erases PSK, both program partitions, map data, schedule, peer_payload, reg_complete |
| ND-0403 | Secure boot support | Should | ❌ Not implemented | See Finding F-002 |
| ND-0403a | Flash encryption support | Should | ❌ Not implemented | See Finding F-003 |
| ND-0500 | Chunked program transfer | Must | ✅ Implemented | `wake_cycle.rs` `chunked_transfer()` — sequential GET_CHUNK requests; `MAX_RESIDENT_IMAGE_SIZE=4096`, `MAX_EPHEMERAL_IMAGE_SIZE=2048` enforced |
| ND-0501 | Program hash verification | Must | ✅ Implemented | `program_store.rs` `install_resident()` / `load_ephemeral()` — SHA-256 hash compared to COMMAND `program_hash` |
| ND-0501a | Program image decoding | Must | ✅ Implemented | `program_store.rs` — CBOR decode; `sonde_bpf_adapter.rs` `load()` resolves LDDW src=1 via `MapRegion` descriptors |
| ND-0502 | Resident program storage (A/B) | Must | ✅ Implemented | `wake_cycle.rs` — writes to inactive partition, swaps active after hash verification |
| ND-0503 | Ephemeral program storage | Must | ✅ Implemented | `wake_cycle.rs` lines 422–430 — ephemeral stored in RAM; maps rejected if declared; discarded after execution |
| ND-0504 | BPF execution | Must | ✅ Implemented | `wake_cycle.rs` step 9 — `interpreter.execute()`; errors swallowed; context provided |
| ND-0505 | Execution context | Must | ✅ Implemented | `bpf_helpers.rs` `SondeContext` struct; `wake_cycle.rs` — timestamp=gateway_ts+elapsed; `sonde_bpf_adapter.rs` `read_only_ctx=true` |
| ND-0506 | Post-update immediate execution | Must | ✅ Implemented | `wake_cycle.rs` — after PROGRAM_ACK, sets `WakeReason::ProgramUpdate` and falls through to BPF execution |
| ND-0600 | Helper API stability | Must | ✅ Implemented | `bpf_dispatch.rs` — 16 helpers registered with fixed numeric IDs (1–16) |
| ND-0601 | Bus access helpers | Must | ✅ Implemented | `bpf_dispatch.rs` helpers 1–7 — i2c_read/write/write_read, spi_transfer, gpio_read/write, adc_read |
| ND-0602 | Communication helpers | Must | ✅ Implemented | `bpf_dispatch.rs` helpers 8–9; `send_app_data()` is fire-and-forget (no recv); `send_recv_app_data()` waits for reply; both increment seq |
| ND-0603 | Map operations | Must | ✅ Implemented | `bpf_dispatch.rs` helpers 10–11; ephemeral `map_update_elem` returns -1; map data survives deep sleep in RTC SRAM |
| ND-0604 | System helpers | Must | ✅ Implemented | `bpf_dispatch.rs` helpers 12–16; `delay_us` capped at `MAX_DELAY_US=1_000_000`; ephemeral `set_next_wake` returns -1; `bpf_trace_printk` at INFO level |
| ND-0605 | Execution constraints | Must | ✅ Implemented | `sonde_bpf_adapter.rs` — `instruction_budget` passed to interpreter; `CallDepthExceeded` and `RuntimeError` mapped from `sonde-bpf` |
| ND-0606 | Map memory budget enforcement | Must | ✅ Implemented | `map_storage.rs` `MAP_BUDGET=4096`; `required_bytes()` with checked arithmetic; `program_store.rs` rejects if over budget |
| ND-0700 | WAKE retry | Must | ✅ Implemented | `wake_cycle.rs` `WAKE_MAX_RETRIES=3`, `RETRY_DELAY_MS=400`; sleep after exhaustion |
| ND-0701 | Chunk transfer retry | Must | ✅ Implemented | `wake_cycle.rs` `get_chunk_with_retry()` — 3 retries per chunk; abort on failure |
| ND-0702 | Response timeout | Must | ✅ Implemented | `wake_cycle.rs` `RESPONSE_TIMEOUT_MS=200`, `RETRY_DELAY_MS=400` |
| ND-0800 | Malformed CBOR handling | Must | ✅ Implemented | `wake_cycle.rs` — `MalformedPayload` error silently discarded; `send_recv_app_data` deadline loop discards junk |
| ND-0801 | Unexpected message type handling | Must | ✅ Implemented | `wake_cycle.rs` — `UnexpectedMsgType` discarded; `send_recv_app_data` continues on wrong msg_type |
| ND-0802 | Chunk index validation | Must | ✅ Implemented | `wake_cycle.rs` `verify_and_decode_chunk()` — chunk_index validated; mismatch returns `ChunkIndexMismatch` |
| ND-0900 | Boot priority and mode selection | Must | ✅ Implemented | `bin/node.rs` and `wake_cycle.rs` — three-way check: no PSK/button→BLE, PSK+no reg→PEER_REQUEST, PSK+reg→WAKE |
| ND-0901 | Pairing button detection | Must | ✅ Implemented | `bin/node.rs` — GPIO 9 sampled 50× over 500 ms at 10 ms intervals |
| ND-0902 | BLE GATT service registration | Must | ✅ Implemented | `esp_ble_pairing.rs` — UUID `0xFE50` service, `0xFE51` characteristic (Write+Indicate) |
| ND-0903 | BLE advertising name | Must | ✅ Implemented | `esp_ble_pairing.rs` line 219 — `sonde-{:02x}{:02x}` from BLE MAC; `set_device_name()` called |
| ND-0904 | ATT MTU and LESC pairing | Must | ✅ Implemented | `esp_ble_pairing.rs` — MTU≥247; `ble_gap_security_initiate()` in on_connect; pre-auth writes buffered |
| ND-0905 | NODE_PROVISION handling | Must | ✅ Implemented | `ble_pairing.rs` — parses 5 fields; validates `payload_len` before read; factory reset on button hold |
| ND-0906 | NODE_PROVISION NVS persistence | Must | ✅ Implemented | `ble_pairing.rs` `handle_node_provision()` — writes PSK, key_hint, channel, peer_payload; clears reg_complete |
| ND-0907 | BLE mode persistence after provisioning | Must | ✅ Implemented | `esp_ble_pairing.rs` — stays in loop after provision; reboots on disconnect |
| ND-0908 | NODE_PROVISION NVS write failure | Must | ✅ Implemented | `ble_pairing.rs` — returns `NODE_ACK_STORAGE_ERROR` (0x02); rolls back partial writes |
| ND-0909 | PEER_REQUEST frame construction | Must | ✅ Implemented | `peer_request.rs` — msg_type 0x05, random nonce, CBOR `{1: encrypted_payload}`, HMAC with node_psk |
| ND-0910 | PEER_REQUEST retransmission | Must | ✅ Implemented | `wake_cycle.rs` — retries each wake cycle; `peer_request.rs` erases malformed payload to break loops |
| ND-0911 | PEER_ACK listen timeout | Must | ✅ Implemented | `peer_request.rs` `PEER_ACK_TIMEOUT_MS=10_000` enforced in listen loop |
| ND-0912 | PEER_ACK verification | Must | ✅ Implemented | `peer_request.rs` — HMAC verify, nonce match, registration_proof `HMAC(psk, "sonde-peer-ack-v1" ‖ payload)` |
| ND-0913 | Registration completion | Must | ✅ Implemented | `peer_request.rs` — sets `reg_complete` flag on valid PEER_ACK; retains `peer_payload` |
| ND-0914 | Deferred payload erasure | Must | ✅ Implemented | `wake_cycle.rs` lines 228–233 — erases `peer_payload` after first successful WAKE/COMMAND |
| ND-0915 | Self-healing on WAKE failure | Must | ✅ Implemented | `wake_cycle.rs` lines 239–250 — clears `reg_complete` if WAKE fails with peer_payload present |
| ND-0916 | NVS layout for BLE pairing | Must | ✅ Implemented | `traits.rs` `PlatformStorage` — `peer_payload` (blob) and `reg_complete` (bool); existing keys unaffected |
| ND-0917 | Factory reset via BLE | Must | ✅ Implemented | `ble_pairing.rs` — calls `factory_reset()` before writing new credentials when button was held |
| ND-0918 | Main task stack size | Must | ✅ Implemented | `sdkconfig.defaults` `CONFIG_ESP_MAIN_TASK_STACK_SIZE=24576` (≥16384 required) |
| ND-1000 | Boot reason logging | Must | ✅ Implemented | `bin/node.rs` — `info!("boot_reason={}")` distinguishing `power_on` vs `deep_sleep_wake` via `esp_reset_reason()` |
| ND-1001 | Wake cycle started logging | Must | ✅ Implemented | `wake_cycle.rs` line 150 — `info!("wake cycle started key_hint=... wake_reason=...")` |
| ND-1002 | WAKE frame sent logging | Must | ✅ Implemented | `wake_cycle.rs` line 608 — `info!("WAKE sent key_hint=... nonce=... attempt=...")` |
| ND-1003 | COMMAND received logging | Must | ✅ Implemented | `wake_cycle.rs` lines 254–276 — logs each `command_type` name; includes `interval_s` for UpdateSchedule |
| ND-1004 | PEER_REQUEST sent logging | Must | ✅ Implemented | `peer_request.rs` line 212 — `info!("PEER_REQUEST sent key_hint=...")` |
| ND-1005 | PEER_ACK received logging | Must | ✅ Implemented | `peer_request.rs` line 238 — `info!("PEER_ACK received — registration complete")` |
| ND-1006 | BPF program execution logging | Must | ✅ Implemented | `wake_cycle.rs` — program_hash at INFO before exec; result at INFO after; `bpf_trace_printk` flushed at INFO |
| ND-1007 | Deep sleep entered logging | Must | ✅ Implemented | `wake_cycle.rs` line 87 — `info!("entering deep sleep duration_seconds=... reason=...")` |
| ND-1008 | BLE pairing mode logging | Must | ⚠️ Partial | See Finding F-001 — entry logged, exit log omits outcome |
| ND-1009 | Error condition logging | Must | ✅ Implemented | `wake_cycle.rs` — `warn!` for RNG failure (line 134), retries exhausted (line 238), HMAC mismatch (line 622), install failure (line 387), chunk failure (line 394) |
| ND-1010 | BPF helper I/O logging | Should | ✅ Implemented | `bpf_dispatch.rs` — DEBUG logs on all 9 I/O helpers; non-I/O helpers have no DEBUG logs |
| ND-1011 | Chunk transfer logging | Must | ✅ Implemented | `wake_cycle.rs` — DEBUG for GET_CHUNK (chunk_index, attempt) and CHUNK received (chunk_index, len) |
| ND-1012 | Build-type–aware log levels | Must | ✅ Implemented | `Cargo.toml` — `quiet` (default) → `release_max_level_warn`; `verbose` → `release_max_level_debug`; mutually exclusive |

No D9 (undocumented behavior) findings were identified. All significant
code behaviors trace to a requirement or design section. Infrastructure
code (error types, trait definitions, test utilities) supports
requirements indirectly and does not constitute undocumented behavior.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 (ND-1008) | Track provisioning state in BLE loop; include outcome in exit log | S | Low |
| 2 | F-002 (ND-0403) | Enable `CONFIG_SECURE_BOOT_V2_ENABLED` in sdkconfig; set up signing keys | M | Medium — requires key management process |
| 3 | F-003 (ND-0403a) | Enable `CONFIG_FLASH_ENCRYPTION_MODE` in sdkconfig; test transparent PSK read | M | Medium — one-way operation in production mode |

## 7. Prevention

- **Logging completeness**: Add a review checklist item: "For every
  state-machine exit path, verify the exit log includes the reason AND
  outcome." This would have caught F-001 during code review.
- **Security hardening tracking**: Create a tracking issue for ND-0403
  and ND-0403a to ensure they are addressed before production
  deployment. These are "Should" priority but represent the primary
  physical-attack mitigation.
- **Automated log verification**: Consider adding a test that runs a
  mock wake cycle and asserts the expected log lines are emitted in
  sequence (boot reason, wake start, WAKE sent, COMMAND received, BPF
  execution, deep sleep). This would catch regressions in logging
  requirements.

## 8. Open Questions

1. **BLE pairing timeout**: The BLE pairing loop in
   `esp_ble_pairing.rs` has no timeout — it advertises indefinitely
   until a client connects and disconnects. ND-1008 mentions "timeout"
   as a possible exit reason, but no timeout is implemented. Clarify
   whether a BLE advertising timeout should be added as a separate
   requirement.
2. **Secure boot / flash encryption timeline**: ND-0403 and ND-0403a
   are "Should" priority. Confirm whether these should be promoted to
   "Must" before any production deployment, or if they remain
   aspirational for the current project phase.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-09 | Copilot (audit agent) | Initial audit — 51 requirements traced, 3 findings |
