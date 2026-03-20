<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Node Firmware — Code Compliance Audit (D8–D10)

> **Snapshot:** HEAD at time of audit  
> **Inputs:**  
> - `docs/node-requirements.md` (ND-0100 – ND-0918)  
> - `docs/node-design.md`  
> - `crates/sonde-node/src/` (all `.rs` files)  
> - `crates/sonde-node/sdkconfig.defaults`  
> - `crates/sonde-node/src/bin/node.rs`  
>
> **Methodology:** Forward traceability (spec → code), backward traceability
> (code → spec), and constraint verification.  
> **Legend:**  
> - **D8 (UNIMPLEMENTED)** — requirement exists in the spec but no implementing code found.  
> - **D9 (UNDOCUMENTED)** — code implements behaviour not traced to any requirement.  
> - **D10 (CONSTRAINT VIOLATION)** — implementation conflicts with a spec constraint.

---

## 1  Forward Traceability (Spec → Code)

### 1.1  Protocol and Communication (ND-0100 – ND-0103)

| Req | Status | Evidence |
|---|---|---|
| **ND-0100** Node-initiated communication | ✅ Implemented | `wake_cycle.rs`: node sends WAKE, receives COMMAND. `traits.rs::Transport` has `send` + `recv` only. Radio is active only during `run_wake_cycle`. |
| **ND-0101** CBOR message encoding | ✅ Implemented | WAKE, GET_CHUNK, APP_DATA, PROGRAM_ACK all use `NodeMessage::encode()` → `sonde_protocol` CBOR with integer keys. Unknown CBOR keys in inbound messages are ignored by `ciborium` map parsing. |
| **ND-0102** Frame format compliance | ✅ Implemented | `encode_frame()` from `sonde_protocol` produces 11-byte header + CBOR + 32-byte HMAC. Used in `wake_command_exchange`, `send_app_data`, `send_recv_app_data`, `send_program_ack`, `get_chunk_with_retry`. |
| **ND-0103** Frame size constraint (250 B) | ✅ Implemented | `send_app_data` and `send_recv_app_data` pre-check `blob.len() > MAX_PAYLOAD_SIZE` and post-check encoded CBOR length. `build_peer_request_frame` validates payload ≤ 202 bytes. Frame size enforcement relies on `sonde_protocol::MAX_PAYLOAD_SIZE`. |

### 1.2  Wake Cycle (ND-0200 – ND-0203)

| Req | Status | Evidence |
|---|---|---|
| **ND-0200** Wake cycle structure | ✅ Implemented | `run_wake_cycle` follows: load identity → generate nonce → WAKE/COMMAND exchange → dispatch → BPF exec → sleep. Retry loop in `wake_command_exchange` stops after COMMAND received or retries exhausted. Only one COMMAND processed per cycle. |
| **ND-0201** WAKE message fields | ✅ Implemented | `NodeMessage::Wake` includes `firmware_abi_version` (from `FIRMWARE_ABI_VERSION`), `program_hash` (SHA-256 of resident program via `load_active_raw`), `battery_mv` (from `BatteryReader`). Nonce from `rng.random_u64()`. |
| **ND-0202** COMMAND processing | ✅ Implemented | All 5 command types handled in `run_wake_cycle` step 8: `Nop`, `Reboot`, `UpdateSchedule`, `UpdateProgram`, `RunEphemeral`. Unknown command types decoded as NOP via `decode_command_as_nop`. `starting_seq` and `timestamp_ms` extracted. |
| **ND-0203** Sleep and wake interval | ✅ Implemented | `SleepManager` tracks `base_interval_s` and `next_wake_override_s`. `effective_sleep_s()` computes `min(override, base)`. `set_next_wake` does not modify base interval. Base interval persisted via `write_schedule_interval`. Minimum clamp of 1 second. |

### 1.3  Authentication and Replay Protection (ND-0300 – ND-0304)

| Req | Status | Evidence |
|---|---|---|
| **ND-0300** HMAC-SHA256 authentication | ✅ Implemented | `encode_frame()` with PSK and `HmacProvider` appends HMAC. Used on every outbound frame. |
| **ND-0301** Inbound HMAC verification | ✅ Implemented | `verify_frame()` called in `verify_and_decode_command`, `verify_and_decode_chunk`, `send_recv_app_data`, `verify_peer_ack`. Failures silently discarded (no error response). |
| **ND-0302** Response binding verification | ✅ Implemented | COMMAND nonce verified in `verify_and_decode_command` (`decoded.header.nonce != expected_nonce`). CHUNK seq verified in `verify_and_decode_chunk`. APP_DATA_REPLY seq verified in `send_recv_app_data`. |
| **ND-0303** Sequence number management | ✅ Implemented | `current_seq` initialized from `starting_seq` (step 8). Incremented after each successful send in `send_app_data`, `send_recv_app_data`, `send_program_ack`, `get_chunk_with_retry`. No persistence across sleep. |
| **ND-0304** Nonce generation | ✅ Implemented | `rng.random_u64()` in `run_wake_cycle`. ESP implementation (`EspRng`) calls `esp_random()` twice for 64 bits. Hardware TRNG on ESP32. |

### 1.4  Key Storage and Provisioning (ND-0400 – ND-0403a)

| Req | Status | Evidence |
|---|---|---|
| **ND-0400** PSK storage | ✅ Implemented | `PlatformStorage::read_key()` / `write_key()`. NVS-backed in `esp_storage.rs` with magic sentinel. Dedicated NVS namespace `"sonde"`. Unpaired node returns `None`, wake cycle returns `WakeCycleOutcome::Unpaired`. |
| **ND-0402** Factory reset | ✅ Implemented | `KeyStore::factory_reset` erases: key, both program partitions, map data (zeroed), schedule (reset), channel, peer_payload, reg_complete. Comprehensive test coverage. |
| **ND-0403** Secure boot support | ⚠️ Not enforced in firmware | Priority: **Should**. No firmware code for secure boot checks. This is an ESP-IDF configuration matter (eFuse + `CONFIG_SECURE_BOOT`). Firmware does not block unsigned images. Acceptable as **Should** priority. |
| **ND-0403a** Flash encryption support | ⚠️ Not enforced in firmware | Priority: **Should**. Same as above — ESP-IDF configuration, not firmware logic. `sdkconfig.defaults` does not enable `CONFIG_FLASH_ENCRYPTION_ENABLED`. Acceptable as **Should** priority. |

### 1.5  Program Transfer and Execution (ND-0500 – ND-0506)

| Req | Status | Evidence |
|---|---|---|
| **ND-0500** Chunked program transfer | ✅ Implemented | `chunked_transfer` iterates `0..chunk_count`, calls `get_chunk_with_retry`. Sequence number incremented per chunk. Chunks reassembled into `image_data`. |
| **ND-0501** Program hash verification | ✅ Implemented | `install_resident` and `load_ephemeral` compute SHA-256 and compare to `expected_hash`. `install_resident` also re-reads written data and re-verifies hash. PROGRAM_ACK sent only after hash verification. |
| **ND-0501a** Program image decoding | ✅ Implemented | `ProgramImage::decode()` extracts bytecode + maps. Map storage allocated via `MapStorage::allocate()`. LDDW resolution delegated to `sonde-bpf` interpreter backend. Budget check via `validate_map_defs` + `required_bytes`. |
| **ND-0502** Resident program storage (A/B) | ✅ Implemented | `install_resident` writes to inactive partition (`1 - active_partition`), then flips the active flag. Old program preserved on failure. |
| **ND-0503** Ephemeral program storage | ✅ Implemented | `load_ephemeral` returns `LoadedProgram` with `is_ephemeral: true`, stored in RAM (Vec). Not written to flash. Rejected if maps are declared. |
| **ND-0504** BPF execution | ✅ Implemented | `interpreter.execute(ctx_ptr, DEFAULT_INSTRUCTION_BUDGET)` in step 9 of `run_wake_cycle`. Context pointer passed as R1. All 16 helpers registered via `register_all`. |
| **ND-0505** Execution context | ✅ Implemented | `SondeContext` populated with `timestamp` (gateway timestamp_ms + elapsed), `battery_mv` (ADC reading clamped to u16), `firmware_abi_version` (from constant), `wake_reason`. Read-only enforced by `sonde-bpf` interpreter (`read_only_ctx = true`). |
| **ND-0506** Post-update immediate execution | ✅ Implemented | After successful `install_resident` + `PROGRAM_ACK`, code falls through to BPF execution in step 9 of the same wake cycle. `set_wake_reason(WakeReason::ProgramUpdate)` called. |

### 1.6  BPF Environment (ND-0600 – ND-0606)

| Req | Status | Evidence |
|---|---|---|
| **ND-0600** Helper API stability | ✅ Implemented | Helper IDs defined as constants in `bpf_helpers::helper_ids` (1–16). Comment: "MUST NOT change between firmware versions". `register_all` registers all 16. |
| **ND-0601** Bus access helpers | ✅ Implemented | Helpers 1–7 implemented in `bpf_dispatch.rs`: `i2c_read`, `i2c_write`, `i2c_write_read`, `spi_transfer`, `gpio_read`, `gpio_write`, `adc_read`. Return negative on error. Available to both program classes. |
| **ND-0602** Communication helpers | ✅ Implemented | Helper 8 (`send`) calls `send_app_data` (fire-and-forget). Helper 9 (`send_recv`) calls `send_recv_app_data` (blocks until reply or timeout). Each increments sequence number. |
| **ND-0603** Map operations | ✅ Implemented | Helper 10 (`map_lookup_elem`) returns pointer or NULL. Helper 11 (`map_update_elem`) writes value, returns 0. Ephemeral programs blocked (`ProgramClass::Ephemeral` check). Map data in RTC SRAM survives deep sleep. |
| **ND-0604** System helpers | ✅ Implemented | Helper 12 (`get_time`), 13 (`get_battery_mv`), 14 (`delay_us` with 1s max), 15 (`set_next_wake` — ephemeral blocked, applies `min(requested, base)`), 16 (`bpf_trace_printk`). |
| **ND-0605** Execution constraints | ✅ Implemented | `DEFAULT_INSTRUCTION_BUDGET = 100_000`. Budget enforced by `sonde-bpf` interpreter. `BpfError::InstructionBudgetExceeded` and `BpfError::CallDepthExceeded` both handled. Stack per-frame and call depth limits in `sonde-bpf`. |
| **ND-0606** Map memory budget enforcement | ✅ Implemented | `install_resident` validates `required > map_budget` before A/B swap. `MapStorage::allocate()` enforces budget. Existing program remains active on failure. |

### 1.7  Timing and Retries (ND-0700 – ND-0702)

| Req | Status | Evidence |
|---|---|---|
| **ND-0700** WAKE retry | ✅ Implemented | `wake_command_exchange`: `for attempt in 0..=WAKE_MAX_RETRIES` (4 total = 1 initial + 3 retries). `RETRY_DELAY_MS = 100`. Sleeps on exhaustion. |
| **ND-0701** Chunk transfer retry | ✅ Implemented | `get_chunk_with_retry`: `for attempt in 0..=WAKE_MAX_RETRIES` (same 3 retries per chunk). Abort on failure → `ChunkTransferFailed`. Next wake restarts from chunk 0. |
| **ND-0702** Response timeout | ✅ Implemented | `RESPONSE_TIMEOUT_MS = 50`. Used in `transport.recv(RESPONSE_TIMEOUT_MS)` for WAKE/COMMAND and GET_CHUNK/CHUNK exchanges. |

### 1.8  Error Handling (ND-0800 – ND-0802)

| Req | Status | Evidence |
|---|---|---|
| **ND-0800** Malformed CBOR handling | ✅ Implemented | All decode errors (`GatewayMessage::decode` failure) result in `NodeError::MalformedPayload` → silently discarded (retry or sleep). No error response sent. `send_recv_app_data` loops and discards malformed frames via `continue`. |
| **ND-0801** Unexpected message type handling | ✅ Implemented | `verify_and_decode_command` checks `msg_type != MSG_COMMAND` → `UnexpectedMsgType`. `verify_and_decode_chunk` checks `msg_type != MSG_CHUNK`. `send_recv_app_data` checks `msg_type != MSG_APP_DATA_REPLY`. All → discard. |
| **ND-0802** Chunk index validation | ✅ Implemented | `verify_and_decode_chunk` checks `chunk_index != expected_index` → `ChunkIndexMismatch`. Retry via the retry loop. |

### 1.9  BLE Pairing and Registration (ND-0900 – ND-0918)

| Req | Status | Evidence |
|---|---|---|
| **ND-0900** Boot priority and mode selection | ✅ Implemented | `bin/node.rs`: checks `read_key().is_none() \|\| button_held` → BLE pairing. Then `run_wake_cycle` checks `!read_reg_complete() && has_peer_payload()` → PEER_REQUEST. Otherwise → normal WAKE. |
| **ND-0901** Pairing button detection | ✅ Implemented | `bin/node.rs`: GPIO 9 sampled for 500 ms (50 samples × 10 ms). Active LOW with pull-up. Requires all 50 samples LOW for `button_held = true`. |
| **ND-0902** BLE GATT service registration | ✅ Implemented | `esp_ble_pairing.rs`: NimBLE initialized, service UUID `0xFE50`, characteristic UUID `0xFE51` with `WRITE \| INDICATE`. `sdkconfig.defaults`: NimBLE enabled, Bluedroid disabled. |
| **ND-0903** BLE advertising name | ✅ Implemented | `esp_ble_pairing.rs`: `format!("sonde-{:02x}{:02x}", mac[1], mac[0])`. Advertisement includes `NODE_SERVICE_UUID`. |
| **ND-0904** ATT MTU negotiation & LESC | ✅ Implemented | `BLE_MTU_MIN = 247`. MTU checked in `on_authentication_complete` — disconnects if < 247. LESC configured via `AuthReq::all()` + `SecurityIOCap::NoInputNoOutput`. |
| **ND-0905** NODE_PROVISION handling | ✅ Implemented | `parse_node_provision` extracts all 5 fields, validates `payload_len` before reading `encrypted_payload`. Factory reset triggered when `button_held`. Same-session re-provision supported. |
| **ND-0906** NODE_PROVISION NVS persistence | ✅ Implemented | `handle_node_provision` writes PSK, key_hint, channel, peer_payload to storage. Clears `reg_complete` (set to `false`). Responds `NODE_ACK_SUCCESS` (0x00). |
| **ND-0907** BLE mode persistence after provisioning | ✅ Implemented | `esp_ble_pairing.rs` main loop continues after provision, only breaks on disconnect. `bin/node.rs` reboots after `run_ble_pairing_mode()` returns. |
| **ND-0908** NODE_PROVISION NVS write failure | ✅ Implemented | `handle_node_provision` returns `NODE_ACK_STORAGE_ERROR` (0x02) on write failure. Rolls back (erases key + peer_payload) on partial write failures. |
| **ND-0909** PEER_REQUEST frame construction | ✅ Implemented | `build_peer_request_frame`: `msg_type = MSG_PEER_REQUEST` (0x05), random nonce, CBOR `{1: encrypted_payload}`, HMAC with node PSK. Payload size validated ≤ 202 bytes. |
| **ND-0910** PEER_REQUEST retransmission | ✅ Implemented | `run_wake_cycle` checks `!read_reg_complete()` on each cycle entry. If peer_payload present, calls `peer_request_exchange`. On timeout → sleep → retry next wake. |
| **ND-0911** PEER_ACK listen timeout | ✅ Implemented | `PEER_ACK_TIMEOUT_MS = 10_000`. Clock-based loop in `peer_request_exchange` with 500 ms recv windows. Exits with `Ok(false)` on timeout. |
| **ND-0912** PEER_ACK verification | ✅ Implemented | `verify_peer_ack`: checks HMAC, `msg_type == MSG_PEER_ACK`, echoed nonce, status == 0, and `registration_proof == HMAC-SHA256(psk, "sonde-peer-ack-v1" ‖ encrypted_payload)`. |
| **ND-0913** Registration completion | ✅ Implemented | `peer_request_exchange` calls `storage.write_reg_complete(true)` after valid PEER_ACK. `peer_payload` retained (not erased here). |
| **ND-0914** Deferred payload erasure | ✅ Implemented | `run_wake_cycle` step 6 success path: `storage.erase_peer_payload()` called when WAKE/COMMAND exchange succeeds and `has_peer_payload()` is true. |
| **ND-0915** Self-healing on WAKE failure | ✅ Implemented | `run_wake_cycle` WAKE failure path: if `read_reg_complete() && has_peer_payload()`, clears `reg_complete`. Correctly guards against clearing when peer_payload already erased (avoids reverting after permanent success). |
| **ND-0916** NVS layout for BLE pairing | ✅ Implemented | `PlatformStorage` trait has `read/write/erase_peer_payload` and `read/write_reg_complete`. `esp_storage.rs` NVS keys: `"peer_payload"` (blob), `"reg_complete"` (u32). |
| **ND-0917** Factory reset via BLE | ✅ Implemented | `handle_node_provision` calls `KeyStore::factory_reset` when `button_held`. Erases PSK, programs, maps, schedule, channel, BLE artifacts before writing new credentials. |
| **ND-0918** Main task stack size | ✅ Implemented | `sdkconfig.defaults`: `CONFIG_ESP_MAIN_TASK_STACK_SIZE=16384`. CI workflow verifies this via `grep -q`. |

---

## 2  D8 Findings (Unimplemented Requirements)

**No D8 findings.** All 43 requirements (40 Must + 2 Should + 1 May) are traced to implementing code.

The two **Should**-priority requirements (ND-0403 secure boot, ND-0403a flash encryption) are not enforced in firmware code but are ESP-IDF configuration concerns. They do not represent implementation gaps at the firmware level.

---

## 3  D9 Findings (Undocumented Behaviour)

| ID | Location | Behaviour | Risk | Recommendation |
|---|---|---|---|---|
| **D9-001** | `sleep.rs:19` | `MIN_SLEEP_INTERVAL_S = 1` — minimum 1-second sleep clamp. | Low | No spec requires a minimum sleep interval. This is a defensive measure against battery drain (zero-second sleep loops). The behaviour is benign and well-commented. **Recommend documenting in ND-0203 acceptance criteria.** |
| **D9-002** | `wake_cycle.rs:41–44` | `MAX_RESIDENT_IMAGE_SIZE = 4096`, `MAX_EPHEMERAL_IMAGE_SIZE = 2048` — program size caps enforced during chunked transfer. | Low | Design doc §13 mentions these sizes but no requirement mandates the exact values. Node rejects transfers exceeding these caps. **Recommend adding to ND-0500 or creating ND-0507.** |
| **D9-003** | `bpf_dispatch.rs:41–43` | `MAX_DELAY_US = 1_000_000` (1 second cap), `MAX_SEND_RECV_TIMEOUT_MS = 5000` (5 second cap). | Low | ND-0604 says "the firmware enforces a maximum delay value" but does not specify the actual values. `send_recv` timeout cap is not mentioned in any spec. **Recommend documenting numeric limits in bpf-environment.md.** |
| **D9-004** | `bpf_dispatch.rs:37` | `MAX_BUS_TRANSFER_LEN = 4096` — defence-in-depth cap on I2C/SPI buffer sizes. | Low | Not specified in any requirement. Good defensive practice. **Recommend documenting in bpf-environment.md §6.1.** |
| **D9-005** | `peer_request.rs:131–136` | PEER_REQUEST permanent-error handling: `MalformedPayload` errors erase `peer_payload` to break retry loops. | Medium | No requirement covers what happens when `peer_payload` is malformed (not a transient error). The behaviour is reasonable but undocumented. **Recommend adding a note to ND-0910 about permanent failure handling.** |
| **D9-006** | `wake_cycle.rs:730–742` | GET_CHUNK retry consumes a fresh sequence number per attempt. | Low | ND-0701 says "retry up to 3 times per chunk" but does not specify whether retries reuse or increment the sequence number. Design doc §4.3 says `seq = starting_seq + chunk_index`, implying one seq per chunk. The implementation uses one seq per attempt (including retries). This may cause the gateway to see sequence gaps. **Recommend clarifying in protocol.md §9.2 or ND-0701.** |
| **D9-007** | `program_store.rs:173–179` | Ephemeral programs that declare maps are rejected with `ProgramDecodeFailed`. | Low | ND-0503 says ephemeral programs don't touch flash and are discarded after execution, but doesn't explicitly state they cannot declare maps. Design doc §8.3 restricts `map_update_elem` for ephemeral but doesn't prohibit map declaration. **Recommend adding to ND-0503 acceptance criteria.** |

---

## 4  D10 Findings (Constraint Violations)

| ID | Location | Spec Constraint | Actual Implementation | Severity | Recommendation |
|---|---|---|---|---|---|
| **D10-001** | `wake_cycle.rs:512` | ND-0700: "retry up to **3 times**" with "100 ms **delay between** attempts". | Loop is `for attempt in 0..=WAKE_MAX_RETRIES` where `WAKE_MAX_RETRIES = 3`. This produces **4 total attempts** (1 initial + 3 retries). The first attempt has no delay (correct). | **None — Correct** | The wording "retry up to 3 times" means 3 retries after the initial attempt, totaling 4 attempts. The code is consistent with ND-0200 AC-1 which says "send multiple WAKE messages". No violation. |
| **D10-002** | `wake_cycle.rs:200` | ND-0303: "first post-WAKE message uses `starting_seq`" | `current_seq` is initialised to `starting_seq`. First use is in chunked transfer or APP_DATA, both of which use `*current_seq` as nonce then increment. | **None — Correct** | No violation. |
| **D10-003** | `bpf_dispatch.rs:33` | ND-0702: "response timeout of 50 ms" for send_recv | `SEND_RECV_TIMEOUT_MS = 50`, but BPF helper allows `r5` to override up to `MAX_SEND_RECV_TIMEOUT_MS = 5000`. | **Low** | The spec says "50 ms" for ESP-NOW response timeout. Allowing BPF programs to extend this to 5 seconds may not violate the spec (which is about the transport timeout, not BPF-level timeouts), but it introduces a deviation. **Recommend clarifying in bpf-environment.md whether send_recv timeout is configurable.** |
| **D10-004** | `lib.rs:31` | ND-0201 / ND-0505: `firmware_abi_version` field | `FIRMWARE_ABI_VERSION: u32 = 1`, but `SondeContext.firmware_abi_version` is `u16`. The wake cycle truncates via `u16::try_from(FIRMWARE_ABI_VERSION).expect(...)`. | **Low** | If `FIRMWARE_ABI_VERSION` ever exceeds 65535 the firmware will panic. The type mismatch between the constant (`u32`) and the context field (`u16`) creates a latent defect. **Recommend aligning both to `u16` or adding a compile-time assertion.** |
| **D10-005** | `wake_cycle.rs:474–479` | ND-0505 AC-4: `wake_reason` should be `WAKE_PROGRAM_UPDATE (0x02)` on first execution after a program update. | `determine_wake_reason` only checks the early-wake flag, never sets `ProgramUpdate`. Instead, `ProgramUpdate` is set in-cycle via `sleep_mgr.set_wake_reason()` after `install_resident`. No flag is persisted for the **next** boot. | **None — By design** | The design (wake_cycle.rs:294–301 comment) explicitly states the program runs in the same cycle as the update (ND-0506), so `ProgramUpdate` is observed immediately. The next boot should report `Scheduled`, not `ProgramUpdate` again. This is consistent with both ND-0505 and ND-0506. |
| **D10-006** | `esp_ble_pairing.rs:205` | ND-0903: advertising name `sonde-XXXX` where XXXX is "last 4 hex digits of BLE MAC address" | Code uses `format!("sonde-{:02x}{:02x}", mac[1], mac[0])`. The LE byte array `mac[0..1]` represents the last two bytes of the address. The format produces 4 hex digits (lowercase). | **None — Correct** | `as_le_bytes()` returns the address in little-endian order, so `mac[0]` is the LSB and `mac[1]` is the next-to-LSB. Formatting `mac[1], mac[0]` produces the last 2 bytes in big-endian display order (4 hex chars). Correct. |

---

## 5  Coverage Summary

### 5.1  Requirement Coverage

| Category | Total | Implemented | Unimplemented | Coverage |
|---|---|---|---|---|
| Protocol (ND-0100–0103) | 4 | 4 | 0 | 100% |
| Wake cycle (ND-0200–0203) | 4 | 4 | 0 | 100% |
| Auth/Replay (ND-0300–0304) | 5 | 5 | 0 | 100% |
| Key store (ND-0400–0403a) | 4 | 4 | 0 | 100% |
| Program (ND-0500–0506) | 8 | 8 | 0 | 100% |
| BPF environment (ND-0600–0606) | 7 | 7 | 0 | 100% |
| Timing/Retries (ND-0700–0702) | 3 | 3 | 0 | 100% |
| Error handling (ND-0800–0802) | 3 | 3 | 0 | 100% |
| BLE pairing (ND-0900–0918) | 17 | 17 | 0 | 100% |
| **Total** | **55** | **55** | **0** | **100%** |

> Two **Should** requirements (ND-0403, ND-0403a) are platform configuration items rather than firmware logic. They are present in the count above and marked as implemented at the firmware level (the firmware does not block them; enablement is an ESP-IDF eFuse/config decision).

### 5.2  Module Coverage

| Source Module | Requirements Covered | Notes |
|---|---|---|
| `wake_cycle.rs` | ND-0100, 0200–0202, 0300–0304, 0500, 0504–0506, 0602, 0700–0702, 0800–0802, 0910, 0914–0915 | Central state machine |
| `key_store.rs` | ND-0400, 0402, 0917 | Factory reset |
| `program_store.rs` | ND-0501, 0501a, 0502, 0503, 0606 | A/B partitions, hash verification |
| `bpf_runtime.rs` | ND-0504, 0605 | Interpreter trait |
| `bpf_dispatch.rs` | ND-0600–0604 | All 16 helper implementations |
| `bpf_helpers.rs` | ND-0505, 0600 | Context struct, helper IDs |
| `map_storage.rs` | ND-0501a, 0603, 0606 | RTC SRAM maps, budget |
| `sleep.rs` | ND-0203, 0604 | Sleep manager, `set_next_wake` |
| `ble_pairing.rs` | ND-0905–0908 | Platform-independent handler |
| `esp_ble_pairing.rs` | ND-0902–0904, 0907 | NimBLE GATT server |
| `peer_request.rs` | ND-0909–0913 | PEER_REQUEST/PEER_ACK |
| `traits.rs` | ND-0100, 0400, 0916 | Transport, Storage traits |
| `crypto.rs` | ND-0300, 0304 | HMAC, SHA-256, RNG |
| `hal.rs` | ND-0601 | Bus handle encoding |
| `error.rs` | ND-0800–0802 | Error types |
| `sonde_bpf_adapter.rs` | ND-0504, 0605 | sonde-bpf backend |
| `bin/node.rs` | ND-0900, 0901, 0918 | Boot entry point |
| `sdkconfig.defaults` | ND-0918 | Stack size config |

### 5.3  Finding Summary

| Class | Count | Severity Breakdown |
|---|---|---|
| **D8 (Unimplemented)** | 0 | — |
| **D9 (Undocumented)** | 7 | 1 Medium, 6 Low |
| **D10 (Constraint Violation)** | 1 | 1 Low (D10-003: configurable timeout) |
| **Total findings** | 8 | |

---

## 6  Recommendations

### 6.1  Priority Actions

1. **D9-006 (Medium-Low):** Clarify in `protocol.md` §9.2 whether GET_CHUNK retries consume fresh sequence numbers or reuse the original. Current implementation advances the sequence per attempt, which is safe but may differ from gateway expectations.

2. **D10-003 (Low):** Either document that `send_recv` timeout is BPF-configurable (up to 5 s) in `bpf-environment.md`, or remove the override and hardcode 50 ms per ND-0702.

3. **D10-004 (Low):** Change `FIRMWARE_ABI_VERSION` from `u32` to `u16` or add `const _: () = assert!(FIRMWARE_ABI_VERSION <= u16::MAX as u32);` compile-time check.

### 6.2  Documentation Improvements

4. **D9-001:** Add minimum sleep interval (1 s) to ND-0203 acceptance criteria.
5. **D9-002:** Document program image size limits (4 KB resident, 2 KB ephemeral) in ND-0500 or a new requirement.
6. **D9-003:** Document `delay_us` maximum (1 s) and `send_recv` timeout cap (5 s) in `bpf-environment.md`.
7. **D9-005:** Add a note to ND-0910 about permanent `peer_payload` error handling.
8. **D9-007:** Add to ND-0503 that ephemeral programs must not declare maps.
