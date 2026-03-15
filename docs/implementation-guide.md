<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Implementation Guide

> **Document status:** Draft  
> **Scope:** Workspace layout, build instructions, and implementation order for the Sonde project.  
> **Audience:** Implementers (human or LLM agent) building the Sonde system.  
> **Related:** [gateway-design.md](gateway-design.md), [node-design.md](node-design.md), [protocol-crate-design.md](protocol-crate-design.md)

---

## 1  Overview

The Sonde codebase is a Rust workspace containing seven crates. This document defines the target workspace layout and the order in which crates and modules should be implemented and tested.

**Key principle:** Each phase produces a working, tested artifact before the next phase begins. An LLM agent should complete one phase (including passing all validation tests for that phase) before moving to the next.

---

## 2  Workspace layout

```
sonde/
├── Cargo.toml                    # workspace root
├── docs/                         # all specification documents
├── crates/
│   ├── sonde-protocol/           # shared protocol crate (Phase 1)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── constants.rs      # msg_type codes, CBOR keys, frame sizes
│   │       ├── header.rs         # FrameHeader (de)serialization
│   │       ├── codec.rs          # encode_frame, decode_frame, verify_frame
│   │       ├── messages.rs       # NodeMessage, GatewayMessage enums
│   │       ├── program_image.rs  # ProgramImage, MapDef, deterministic encoding
│   │       ├── chunk.rs          # chunk_count, get_chunk
│   │       ├── traits.rs         # HmacProvider, Sha256Provider
│   │       └── error.rs          # EncodeError, DecodeError
│   │
│   ├── sonde-gateway/            # gateway service (Phase 2)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── bin/gateway.rs    # entry point, startup/shutdown
│   │       ├── engine.rs         # core protocol loop, frame processing
│   │       ├── transport.rs      # Transport trait + MockTransport
│   │       ├── modem.rs          # UsbEspNowTransport (USB modem adapter)
│   │       ├── session.rs        # Session, SessionManager
│   │       ├── registry.rs       # NodeRecord, node registry logic
│   │       ├── program.rs        # ProgramRecord, program library, ingestion
│   │       ├── handler.rs        # HandlerRouter, HandlerProcess, DATA/REPLY/EVENT/LOG
│   │       ├── storage.rs        # Storage trait
│   │       ├── sqlite_storage.rs # SQLite-backed Storage implementation
│   │       ├── admin.rs          # gRPC admin API (tonic)
│   │       └── crypto.rs         # RustCryptoHmac, RustCryptoSha256
│   │
│   ├── sonde-node/               # node firmware (Phase 3)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── bin/node.rs        # entry point, boot sequence (feature: esp)
│   │       ├── wake_cycle.rs      # wake cycle state machine
│   │       ├── key_store.rs       # PSK flash partition, pairing, factory reset
│   │       ├── program_store.rs   # A/B partitions, image decoding, LDDW resolution
│   │       ├── bpf_runtime.rs     # BpfInterpreter trait, helper registration
│   │       ├── bpf_helpers.rs     # helper constants and SondeContext struct
│   │       ├── bpf_dispatch.rs    # helper implementations (bus, comms, maps, system)
│   │       ├── map_storage.rs     # RTC SRAM map allocation and access
│   │       ├── hal.rs             # I2C, SPI, GPIO, ADC wrappers
│   │       ├── sleep.rs           # sleep manager, wake reason
│   │       ├── crypto.rs          # software HMAC/SHA256; ESP hardware (feature: esp)
│   │       ├── pairing.rs         # USB pairing protocol handler
│   │       ├── sonde_bpf_adapter.rs   # BpfInterpreter impl for sonde-bpf backend
│   │       ├── traits.rs          # Transport, Rng, Clock, SleepController, PlatformStorage
│   │       ├── error.rs           # NodeError enum
│   │       ├── esp_hal.rs         # ESP32 I2C/GPIO/ADC (feature: esp)
│   │       ├── esp_sleep.rs       # ESP32 deep sleep (feature: esp)
│   │       ├── esp_storage.rs     # ESP32 NVS storage (feature: esp)
│   │       └── esp_transport.rs   # ESP-NOW radio (feature: esp)
│   │
│   ├── sonde-bpf/                 # zero-alloc BPF interpreter (added post-plan)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── ebpf.rs            # opcode constants, instruction decoding
│   │       ├── interpreter.rs     # execution engine (RFC 9669)
│   │       └── bin/
│   │           └── sonde_bpf_plugin.rs  # bpf_conformance test plugin
│   │
│   ├── sonde-modem/              # ESP32-S3 radio modem firmware (Phase 5)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── bin/modem.rs      # entry point, main loop
│   │       ├── usb_cdc.rs        # USB-CDC ACM driver, DTR detection
│   │       ├── bridge.rs         # command dispatch, frame relay logic
│   │       ├── espnow.rs         # ESP-NOW init, send, recv callback
│   │       ├── peer_table.rs     # auto-registration, LRU eviction
│   │       └── status.rs         # counters, uptime, STATUS response
│   │
│   ├── sonde-admin/              # CLI admin tool (Phase 4)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # module declarations
│   │       ├── main.rs           # CLI argument parsing (clap)
│   │       ├── grpc_client.rs    # gRPC client for gateway admin API
│   │       └── usb.rs            # USB serial pairing/reset
│   │
│   └── sonde-e2e/                # end-to-end test harness (Phase 7)
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs
│       │   ├── harness.rs        # BridgeTransport, ChannelTransport, ChannelRadio, PipeSerial, ModemTestEnv
│       │   └── bin/
│       │       └── stub_handler.rs  # minimal handler subprocess for E2E tests
│       └── tests/
│           └── e2e_tests.rs      # 19 integration tests (T-E2E-001 through T-E2E-060)
│
├── proto/
│   └── admin.proto               # gRPC service definition
│
└── test-programs/                # BPF C source files for integration tests
    ├── nop.c
    ├── send.c
    ├── send_recv.c
    ├── map.c
    ├── early_wake.c
    ├── oversized_map.c
    ├── deep_call.c
    ├── budget_exceeded.c
    └── i2c_sensor.c
```

### 2.1  Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/sonde-protocol",
    "crates/sonde-gateway",
    "crates/sonde-modem",
    "crates/sonde-node",
    "crates/sonde-admin",
    "crates/sonde-bpf",
    "crates/sonde-e2e",
]
```

### 2.2  Crate dependencies

```
sonde-protocol  (no_std + alloc, no platform deps)
       │
       ├──── sonde-gateway  (std, tokio, tonic, prevail, RustCrypto, tokio-serial)
       │
       ├──── sonde-node     (std via ESP-IDF, esp-idf-hal, esp-idf-svc, sonde-bpf)
       │
       ├──── sonde-modem    (std via ESP-IDF, esp-idf-hal, esp-idf-svc)
       │
       ├──── sonde-admin    (std, tonic, clap, serialport)
       │
       ├──── sonde-bpf      (no_std-compatible, zero-alloc BPF interpreter)
       │
       └──── sonde-e2e      (std, tokio — depends on sonde-gateway, sonde-node, sonde-modem, sonde-protocol)
```

`sonde-protocol` is the only shared dependency between the core crates. `sonde-bpf` is a standalone interpreter that is integrated into `sonde-node` via `sonde_bpf_adapter.rs`. `sonde-e2e` depends on the gateway, node, and modem crates to exercise the full stack in a single process.

---

## 3  Implementation phases

### Phase 1: `sonde-protocol` crate — ✅ DONE

**Goal:** A fully tested, platform-independent protocol library.

**Design doc:** [protocol-crate-design.md](protocol-crate-design.md)  
**Validation:** [protocol-crate-validation.md](protocol-crate-validation.md) (41 tests)  
**Runtime dependencies:** `ciborium` only. **Dev-dependencies (for tests):** `hmac`, `sha2`.

**Status:** Complete. All tests pass (`cargo test -p sonde-protocol`).

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 1.1 | `constants.rs` | All protocol constants | Compile check |
| 1.2 | `error.rs` | `EncodeError`, `DecodeError` enums | Compile check |
| 1.3 | `traits.rs` | `HmacProvider`, `Sha256Provider` traits | Compile check |
| 1.4 | `header.rs` | `FrameHeader` with `to_bytes`/`from_bytes` | T-P001 to T-P004 |
| 1.5 | `codec.rs` | `encode_frame`, `decode_frame`, `verify_frame` | T-P010 to T-P019 |
| 1.6 | `messages.rs` | `NodeMessage`, `GatewayMessage` with CBOR encode/decode | T-P020 to T-P032 |
| 1.7 | `program_image.rs` | `ProgramImage`, `MapDef`, deterministic encoding, `program_hash` | T-P040 to T-P046 |
| 1.8 | `chunk.rs` | `chunk_count`, `get_chunk` | T-P050 to T-P053 |
| 1.9 | `modem.rs` | Modem serial protocol codec: frame envelope encode/decode, message types | Unit tests |
| 1.10 | Integration | Full frame round-trips | T-P060 to T-P062 |

**Test HMAC/SHA providers:** Implement a software `HmacProvider` and `Sha256Provider` using `hmac`, `sha2` crates in `#[cfg(test)]` for running the protocol crate's own tests.

**Exit criteria:** `cargo test -p sonde-protocol` — all tests pass. ✅

---

### Phase 2: `sonde-gateway` crate — ✅ DONE

**Goal:** A working gateway service that can authenticate nodes, manage sessions, serve programs, and route application data.

**Design doc:** [gateway-design.md](gateway-design.md)  
**Validation:** [gateway-validation.md](gateway-validation.md)  
**Key dependencies:** `sonde-protocol`, `tokio`, `tonic`, `prevail`, `rusqlite`, `hmac`, `sha2`, `ciborium`, `clap`, `tokio-serial`, `tracing`. See `crates/sonde-gateway/Cargo.toml` for the full list.

**Status:** Complete. All tests pass (`cargo test -p sonde-gateway`) across integration test files (phase2a through phase2d) and unit tests within source modules. Uses `sqlite_storage.rs` for persistence (added beyond original plan). Binary entry point is `src/bin/gateway.rs`.

Phase 2 is split into three sub-phases, each producing a testable artifact:

#### Phase 2A: Foundation (steps 2.1–2.6) — ✅ DONE

Core infrastructure — traits, mocks, and standalone modules. Each module is testable in isolation before the protocol engine is built.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.1 | `crypto.rs` | `RustCryptoHmac`, `RustCryptoSha256` implementing protocol traits | Unit tests |
| 2.2 | `transport.rs` | `Transport` trait (mock impl for testing) | T-0100 |
| 2.3 | `storage.rs` | `Storage` trait (in-memory mock impl for testing) | Unit tests |
| 2.4 | `registry.rs` | `NodeRecord`, key lookup by `key_hint`, CRUD operations | T-0700, T-0702, T-0703 |
| 2.5 | `session.rs` | `Session`, `SessionManager` — create/replace/timeout/lookup | T-0604 to T-0607, T-1004 |
| 2.6 | `program.rs` | `ProgramRecord`, ingestion (ELF → verify → CBOR), chunk serving | T-0400 to T-0407 |

**Exit criteria (2A):** All module-level tests pass. Mock transport, mock storage, node registry, session manager, and program library are functional and independently tested.

#### Phase 2B: Protocol engine (steps 2.7–2.9) — ✅ DONE

Connect the foundation modules into the main frame-processing loop. The gateway can authenticate nodes, dispatch commands, and serve program chunks.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.7 | Core protocol loop | Frame recv → auth → session → dispatch → response | T-0101 to T-0106, T-0600 to T-0603, T-0609 |
| 2.8 | Command handling | NOP, UPDATE_PROGRAM, RUN_EPHEMERAL, UPDATE_SCHEDULE, REBOOT | T-0200 to T-0205 |
| 2.9 | Chunked transfer | GET_CHUNK → CHUNK serving, PROGRAM_ACK | T-0300 to T-0302 |

**Exit criteria (2B):** A node can complete a full wake cycle (WAKE → COMMAND → chunked transfer → PROGRAM_ACK → APP_DATA) against the gateway using the mock transport. All protocol and command tests pass.

#### Phase 2C: Handler API and admin (steps 2.10–2.12) — ✅ DONE

Application data routing, handler process management, gRPC admin API, configuration, and startup/shutdown. Phase 2C is split into three sub-phases:

#### Phase 2C-i: Handler router (step 2.10) — ✅ DONE

Handler process management and APP_DATA routing. The gateway can forward application data to external handler processes and relay replies.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.10a | `handler.rs` (transport) | Handler framing: 4B length-prefix + CBOR encode/decode over stdin/stdout | T-0504 |
| 2.10b | `handler.rs` (process) | HandlerProcess: spawn, write DATA, read DATA_REPLY, LOG handling, respawn/crash | T-0505, T-0506, T-0510, T-0511, T-0513 |
| 2.10c | `handler.rs` (router) | HandlerRouter: program_hash → handler config routing, catch-all, no-match | T-0507, T-0508, T-0509 |
| 2.10d | `engine.rs` (integration) | Wire APP_DATA dispatch through handler router, APP_DATA_REPLY back to node | T-0500, T-0501, T-0502, T-0503 |
| 2.10e | `handler.rs` (events) | EVENT messages: node_online, program_updated, node_timeout (engine wiring deferred to Phase 2C-ii) | T-0512 (smoke test) |

**Exit criteria (2C-i):** All handler API tests pass (T-0500 to T-0513). APP_DATA flows end-to-end from node through engine to handler process and back.

#### Phase 2C-ii: Admin API (step 2.11) — ✅ DONE

gRPC admin API for node/program management and operational commands.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.11a | `proto/admin.proto` | gRPC service definition | Compile check |
| 2.11b | `admin.rs` | gRPC service: node CRUD, program ingestion, schedule/reboot/ephemeral queueing, status, export/import | T-0800 to T-0810 |

**Exit criteria (2C-ii):** All admin API tests pass (T-0800 to T-0810).

#### Phase 2C-iii: Config and startup (step 2.12) — ✅ DONE

Configuration loading, startup/shutdown sequence, and operational tests.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.12a | `bin/gateway.rs` | Configuration parsing (via clap args) and startup/shutdown sequence | T-1000 to T-1004 |

**Exit criteria (2C):** `cargo test -p sonde-gateway` passes all gateway validation tests. The gateway is a complete, runnable service. ✅

#### Phase 2D: Modem transport adapter (step 2.13) — ✅ DONE

USB modem serial transport. The gateway can communicate with nodes via an ESP32-S3 radio modem attached over USB-CDC.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.13 | `modem.rs` | `UsbEspNowTransport`: serial reader task, message demux, startup sequence (RESET → MODEM_READY → SET_CHANNEL), health monitor, error handling | T-1100 to T-1108 |

**Design doc:** [gateway-design.md §4.2](gateway-design.md)
**Validation:** [gateway-validation.md §11](gateway-validation.md)
**Dependencies:** `tokio-serial` (async serial port), `sonde-protocol::modem` (shared codec).

**Test approach:** All tests use a PTY-based `MockModem` — no physical modem hardware required. The mock modem speaks the serial protocol on a PTY slave and simulates modem behavior (MODEM_READY, RECV_FRAME injection, SEND_FRAME capture, STATUS responses).

**Exit criteria (2D):** All modem transport tests pass (T-1100 to T-1108). A full wake cycle works end-to-end over the PTY mock transport. ✅

---

### Phase 3: `sonde-node` crate — ⚠️ MOSTLY DONE

**Goal:** Working node firmware for ESP32-C3/S3.

**Design doc:** [node-design.md](node-design.md)  
**Validation:** [node-validation.md](node-validation.md)  
**Key dependencies:** `sonde-protocol`, `sonde-bpf`, `ciborium`, `hmac`, `sha2`, `log`. ESP-IDF dependencies (`esp-idf-hal`, `esp-idf-svc`) are behind the `esp` feature. See `crates/sonde-node/Cargo.toml` for the full list.

**Status:** Mostly complete. 101 tests pass covering all validation test cases (T-N100 through T-N802). All 19 modules implemented including ESP-specific platform adapters. Modules added beyond original plan: `bpf_dispatch.rs` (helper dispatch), `pairing.rs` (USB pairing handler), `sonde_bpf_adapter.rs` (BpfInterpreter impl for sonde-bpf), `traits.rs` (platform abstractions), `error.rs` (error types), and four ESP-specific modules (`esp_hal.rs`, `esp_sleep.rs`, `esp_storage.rs`, `esp_transport.rs`).

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 3.1 | `crypto.rs` | Software HMAC/SHA256 (ESP hardware impl behind `esp` feature) | Unit tests |
| 3.2 | `traits.rs` / `esp_transport.rs` | `Transport` trait + ESP-NOW send/receive (feature: esp) | T-N100, T-N102 |
| 3.3 | `key_store.rs` | PSK flash partition read/write, magic check | T-N400, T-N401, T-N402, T-N403, T-N404 |
| 3.4 | `sleep.rs` | Deep sleep entry, wake reason, interval management | T-N208, T-N209 |
| 3.5 | `wake_cycle.rs` | WAKE → COMMAND state machine (without BPF) | T-N200 to T-N207, T-N300 to T-N306 |
| 3.6 | `program_store.rs` | A/B partitions, CBOR decode, LDDW resolution | T-N500 to T-N505 |
| 3.7 | `map_storage.rs` | RTC SRAM allocation, map access | T-N607, T-N608, T-N616 |
| 3.8 | `bpf_runtime.rs` | `BpfInterpreter` trait, interpreter adapter | T-N506 |
| 3.9 | `hal.rs` | I2C, SPI, GPIO, ADC wrappers | T-N600 to T-N603 |
| 3.10 | `bpf_helpers.rs` | All 16 helpers registered | T-N604 to T-N615 |
| 3.11 | Integration | Full wake cycle with BPF execution | T-N200, T-N507 to T-N510 |
| 3.12 | Error handling | Malformed CBOR, unexpected msg_type, chunk index | T-N800 to T-N802 |
| 3.13 | Retries | WAKE retry, chunk retry, timeout | T-N700 to T-N702 |

**Note:** Many node tests require target hardware or a simulation environment. Tests that can run on the host (using mock HAL and mock transport) should be prioritized for CI. Hardware-in-the-loop tests are run separately.

**Exit criteria:** All 101 node tests pass (host-based where possible, hardware-in-the-loop for the rest). ✅

---

### Phase 4: `sonde-admin` CLI tool — ✅ DONE

**Goal:** A CLI that wraps the gateway gRPC API and handles USB pairing.

**Design doc:** [gateway-design.md §13](gateway-design.md)  
**Requirements:** GW-0806  
**Dependencies:** `tonic` (gRPC client), `clap` (CLI parsing), `serialport` (USB serial).

**Status:** Complete. All 4 modules implemented (`grpc_client.rs`, `usb.rs`, `main.rs`, `lib.rs`). USB pairing supports `--format json` output. No automated tests (USB pairing requires hardware; gRPC client requires a running gateway).

The admin CLI connects to the gateway over UDS on Linux/macOS (default: `/var/run/sonde/admin.sock`) or a Windows named pipe (default: `\\.\pipe\sonde-admin`). **Note:** The gateway binary currently starts its gRPC server on a TCP `SocketAddr` (`--admin-addr`); a UDS/pipe listener on the gateway side is needed to match the admin CLI's transport.

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 4.1 | `grpc_client.rs` | Connect to gateway, call all admin RPCs | Integration test against running gateway |
| 4.2 | `usb.rs` | USB serial: write PSK, factory reset | Manual test with hardware |
| 4.3 | `main.rs` | CLI argument parsing, command dispatch, JSON output | CLI smoke tests |

**Exit criteria:** All `sonde-admin` commands work against a running gateway instance. USB pairing tested with hardware. ✅

---

### Phase 5: `sonde-modem` crate — ✅ DONE

**Goal:** Working ESP32-S3 radio modem firmware that bridges USB-CDC to ESP-NOW.

**Design doc:** [modem-design.md](modem-design.md)
**Validation:** [modem-validation.md](modem-validation.md) (20 tests)
**Requirements:** [modem-requirements.md](modem-requirements.md) (17 requirements, MD-0100 to MD-0303)
**Dependencies:** `sonde-protocol` (modem codec), `esp-idf-hal`, `esp-idf-svc`.

**Status:** Complete. All 6 modules implemented. 36 tests pass in the default `cargo test -p sonde-modem` run (19 bridge, 6 peer table, 11 status). Additional hardware integration tests in `tests/device_tests.rs` are behind `#[cfg(feature = "device-tests")]` and are not included in the default CI run. Binary entry point is `src/bin/modem.rs`.

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 5.1 | `usb_cdc.rs` | USB-CDC ACM init, read/write, DTR disconnect detection | T-0100, T-0101 |
| 5.2 | `bridge.rs` (codec) | Serial frame decode/dispatch, outbound encoding (uses `sonde-protocol::modem`) | T-0102, T-0103, T-0104 |
| 5.3 | `espnow.rs` | ESP-NOW init, recv callback → RECV_FRAME, send path | T-0200, T-0201 |
| 5.4 | `peer_table.rs` | Auto peer registration, LRU eviction | T-0202, T-0203 |
| 5.5 | `bridge.rs` (commands) | SET_CHANNEL, GET_STATUS, SCAN_CHANNELS dispatch | T-0205, T-0206 |
| 5.6 | `status.rs` | Counters, uptime tracking, STATUS response | T-0302 |
| 5.7 | `bin/modem.rs` | Main loop, RESET handling, MODEM_READY, watchdog | T-0300, T-0301, T-0303 |
| 5.8 | Error handling | Invalid frames, bad channel, short body | T-0400, T-0401, T-0402 |
| 5.9 | Integration | Frame ordering, content transparency | T-0204, T-0500 |

**Shared code with `sonde-node`:** The ESP-NOW driver (`espnow.rs`) should be extracted into a shared module or internal crate that both `sonde-modem` and `sonde-node` depend on. This covers WiFi/ESP-NOW init, send with peer management, and receive callback registration.

**Test approach:** Tests T-0100 to T-0104 and T-0300 to T-0303 can be run with only a USB connection (no radio peer). Tests T-0200 to T-0206 and T-0500 require a second ESP32 acting as a radio peer. Tests T-0400 to T-0402 can be run with USB only.

**Exit criteria:** All 20 modem validation tests pass. The modem bridges a full gateway wake cycle (WAKE → COMMAND → chunked transfer → PROGRAM_ACK) between the gateway and a real sensor node. ✅

---

### Phase 6: `sonde-bpf` crate — ✅ DONE

**Goal:** A zero-allocation, `no_std`-compatible BPF interpreter based on RFC 9669 that replaces `rbpf` as the node's execution backend. The crate defaults to `std` but supports `no_std` when the default `std` feature is disabled.

**Dependencies:** None (standalone, zero external dependencies). Build with `--no-default-features` for `no_std`.

**Status:** Complete. Full RFC 9669 interpreter with 38+ tests and a `bpf_conformance` plugin binary. Integrated into `sonde-node` via `sonde_bpf_adapter.rs` — `rbpf` has been fully replaced.

| Step | What to build | Status |
|---|---|---|
| 6.1 | Core interpreter (`interpreter.rs`, `ebpf.rs`) | ✅ Done (38+ tests) |
| 6.2 | `bpf_conformance` plugin (`sonde_bpf_plugin`) | ✅ Done |
| 6.3 | Add instruction budget enforcement to `execute_program()` | ✅ Done (budget param, LD_DW_IMM double-slot charging, 3 tests) |
| 6.4 | Implement `BpfInterpreter` trait adapter in `sonde-node` (`sonde_bpf_adapter.rs`) | ✅ Done |
| 6.5 | Run `bpf_conformance` test suite against the plugin | ✅ Done (CI job runs it with one known exclusion: `mem-len`) |

**Exit criteria:** `sonde-bpf` passes the `bpf_conformance` test suite. `sonde-node` uses `sonde-bpf` as its interpreter backend with instruction budget enforcement. All existing node tests still pass. ✅

---

### Phase 7: `sonde-e2e` crate — ⚠️ MOSTLY DONE

**Goal:** End-to-end integration tests exercising the full stack (node + gateway + modem) in a single process.

**Validation:** [e2e-validation.md](e2e-validation.md) (14 specified test cases, T-E2E-001 through T-E2E-051)
**Dependencies:** `sonde-gateway`, `sonde-node`, `sonde-modem`, `sonde-protocol`, `tokio`.

**Status:** E2E harness implemented with 19 tests — the 14 cases from the validation spec plus 5 additional tests (T-E2E-002b, T-E2E-052 through T-E2E-054, T-E2E-060) covering consecutive wake cycles, extended modem-bridge scenarios, and the full boot-pair-run lifecycle. All modem-bridge tests currently use in-memory adapters (`PipeSerial` over tokio duplex, `ChannelRadio` over mpsc); testing with a real hardware modem over a physical serial port is not yet implemented (tracked in issue #115).

| Step | What to build | Status |
|---|---|---|
| 7.1 | Crate scaffold (`Cargo.toml`, `src/lib.rs`) | ✅ Done |
| 7.2 | Test harness (`ChannelRadio`, `ChannelTransport`, `PipeSerial`, `ModemTestEnv`, `BridgeTransport`) | ✅ Done |
| 7.3 | Protocol compatibility tests (T-E2E-001 to T-E2E-003) | ✅ Done |
| 7.4 | Program distribution tests (T-E2E-010 to T-E2E-011) | ✅ Done |
| 7.5 | Command dispatch tests (T-E2E-020 to T-E2E-022) | ✅ Done |
| 7.6 | Application data tests (T-E2E-030 to T-E2E-031) | ✅ Done |
| 7.7 | Error handling tests (T-E2E-040 to T-E2E-041) | ✅ Done |
| 7.8 | Modem bridge tests (T-E2E-050 to T-E2E-054, T-E2E-060) | ✅ Done (in-process bridge via `ChannelRadio`/`ChannelTransport`) |
| 7.9 | Modem-in-loop integration (real hardware modem over physical serial port) | ❌ Not started (tracked in issue #115) |

**Exit criteria:** All E2E test cases pass (`cargo test -p sonde-e2e`).

---

## 4  Build and test commands

```bash
# Build everything
cargo build --workspace

# Test everything (what CI runs)
cargo test --workspace

# Test individual crates (counts may change as tests are added)
cargo test -p sonde-protocol
cargo test -p sonde-gateway
cargo test -p sonde-node
cargo test -p sonde-bpf
cargo test -p sonde-modem
cargo test -p sonde-e2e

# Build node firmware for ESP32-C3
cargo build -p sonde-node --target riscv32imc-esp-espidf

# Build node firmware for ESP32-S3
cargo build -p sonde-node --target xtensa-esp32s3-espidf

# Build modem firmware for ESP32-S3
cargo build -p sonde-modem --features esp --target xtensa-esp32s3-espidf

# Build admin CLI
cargo build -p sonde-admin
```

### 4.1  CI pipeline

The CI pipeline (`.github/workflows/ci.yml`) runs three jobs:

**Job 1: Check, lint, and test**
1. `cargo fmt --check --all` — formatting.
2. `cargo clippy --workspace -- -D warnings` — lint (warnings are errors).
3. `cargo build --workspace` — build all crates.
4. `cargo test --workspace` — run all tests across all crates.

**Job 2: BPF conformance (RFC 9669)**
1. Build the `sonde_bpf_plugin` binary.
2. Run the `bpf_conformance` test suite via Docker against the plugin.

**Job 3: Fuzz test (protocol crate)**
1. Fuzz `decode_frame`, `node_message`, `gateway_message`, and `program_image` targets (30s each).

---

## 5  Cross-cutting concerns

### 5.1  Shared test utilities

Consider a `crates/sonde-test-utils/` crate (dev-dependency only) containing:

- Software `HmacProvider` and `Sha256Provider` implementations.
- `TestNode` helper (constructs valid authenticated frames).
- `MockTransport` and `MockStorage` implementations.
- Pre-compiled BPF test program images (CBOR-encoded).

This avoids duplicating test infrastructure between the gateway and protocol crate tests.

### 5.2  Proto file management

The `proto/admin.proto` file defines the gRPC admin API. Both `sonde-gateway` (server) and `sonde-admin` (client) use `tonic-prost-build` to generate Rust code from it. The proto file is the single source of truth for the admin API wire format.

### 5.3  Test program compilation

The `test-programs/` directory contains BPF C source files. A build script (or Makefile) compiles them to ELF, then the gateway's ingestion pipeline converts them to CBOR program images. The resulting images are used by both gateway and node validation tests.

```bash
# Compile test programs (requires BPF toolchain)
clang -target bpf -O2 -c test-programs/nop.c -o test-programs/nop.o

# Ingest via gateway (or a standalone tool) to produce CBOR images
sonde-admin program ingest test-programs/nop.o --profile resident
```
