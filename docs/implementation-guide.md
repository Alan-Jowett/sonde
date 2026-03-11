<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Implementation Guide

> **Document status:** Draft  
> **Scope:** Workspace layout, build instructions, and implementation order for the Sonde project.  
> **Audience:** Implementers (human or LLM agent) building the Sonde system.  
> **Related:** [gateway-design.md](gateway-design.md), [node-design.md](node-design.md), [protocol-crate-design.md](protocol-crate-design.md)

---

## 1  Overview

The Sonde codebase is a Rust workspace that will contain five crates. This document defines the target workspace layout and the order in which crates and modules should be implemented and tested.

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
│   │       ├── main.rs           # entry point, config loading, startup
│   │       ├── transport.rs      # Transport trait + ESP-NOW adapter
│   │       ├── modem.rs          # UsbEspNowTransport (USB modem adapter)
│   │       ├── session.rs        # Session, SessionManager
│   │       ├── registry.rs       # NodeRecord, node registry logic
│   │       ├── program.rs        # ProgramRecord, program library, ingestion
│   │       ├── handler.rs        # HandlerRouter, HandlerProcess, DATA/REPLY/EVENT/LOG
│   │       ├── storage.rs        # Storage trait
│   │       ├── admin.rs          # gRPC admin API (tonic)
│   │       ├── crypto.rs         # RustCryptoHmac, RustCryptoSha256
│   │       └── config.rs         # configuration structs, TOML loading
│   │
│   ├── sonde-node/               # node firmware (Phase 3)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs           # entry point, boot sequence
│   │       ├── wake_cycle.rs     # wake cycle state machine
│   │       ├── key_store.rs      # PSK flash partition, pairing, factory reset
│   │       ├── program_store.rs  # A/B partitions, image decoding, LDDW resolution
│   │       ├── bpf_runtime.rs    # BpfInterpreter trait, helper registration
│   │       ├── bpf_helpers.rs    # helper implementations (bus, comms, maps, system)
│   │       ├── map_storage.rs    # RTC SRAM map allocation and access
│   │       ├── hal.rs            # I2C, SPI, GPIO, ADC wrappers
│   │       ├── sleep.rs          # sleep manager, wake reason
│   │       ├── crypto.rs         # ESP-IDF hardware HMAC/SHA256 HmacProvider impl
│   │       └── transport.rs      # ESP-NOW send/receive (shared with sonde-modem)
│   │
│   ├── sonde-modem/              # ESP32-S3 radio modem firmware (Phase 5)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs           # entry point, main loop
│   │       ├── usb_cdc.rs        # USB-CDC ACM driver, DTR detection
│   │       ├── bridge.rs         # command dispatch, frame relay logic
│   │       ├── espnow.rs         # ESP-NOW init, send, recv callback (shared with sonde-node)
│   │       ├── peer_table.rs     # auto-registration, LRU eviction
│   │       └── status.rs         # counters, uptime, STATUS response
│   │
│   └── sonde-admin/              # CLI admin tool (Phase 4)
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs           # CLI argument parsing (clap)
│           ├── grpc_client.rs    # gRPC client for gateway admin API
│           └── usb.rs            # USB serial pairing/reset
│
├── proto/
│   └── admin.proto               # gRPC service definition
│
└── test-programs/                # pre-compiled BPF test program sources
    ├── nop.c
    ├── send.c
    ├── send_recv.c
    ├── map.c
    ├── early_wake.c
    ├── oversized_map.c
    ├── deep_call.c
    └── budget_exceeded.c
```

### 2.1  Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/sonde-protocol",
    "crates/sonde-gateway",
    "crates/sonde-node",
    "crates/sonde-modem",
    "crates/sonde-admin",
]
```

### 2.2  Crate dependencies

```
sonde-protocol  (no_std + alloc, no platform deps)
       │
       ├──── sonde-gateway  (std, tokio, tonic, prevail-rust, RustCrypto, tokio-serial)
       │
       ├──── sonde-node     (std via ESP-IDF, esp-idf-hal, esp-idf-svc, rbpf/ubpf)
       │          │
       │          └── shared ESP-NOW driver ──┐
       │                                      │
       ├──── sonde-modem    (std via ESP-IDF, esp-idf-hal, esp-idf-svc)
       │          │                           │
       │          └── shared ESP-NOW driver ──┘
       │
       └──── sonde-admin    (std, tonic, clap, serialport)
```

`sonde-protocol` is the only shared dependency. `sonde-node` and `sonde-modem` share ESP-NOW driver code but do not depend on each other. The other crates do not depend on each other.

---

## 3  Implementation phases

### Phase 1: `sonde-protocol` crate

**Goal:** A fully tested, platform-independent protocol library.

**Design doc:** [protocol-crate-design.md](protocol-crate-design.md)  
**Validation:** [protocol-crate-validation.md](protocol-crate-validation.md) (41 tests)  
**Runtime dependencies:** `ciborium` only. **Dev-dependencies (for tests):** `hmac`, `sha2`.

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

**Exit criteria:** `cargo test -p sonde-protocol` passes all 41 tests.

---

### Phase 2: `sonde-gateway` crate

**Goal:** A working gateway service that can authenticate nodes, manage sessions, serve programs, and route application data.

**Design doc:** [gateway-design.md](gateway-design.md)  
**Validation:** [gateway-validation.md](gateway-validation.md)  
**Dependencies:** `sonde-protocol`, `tokio`, `tonic`, `prevail-rust`, `hmac`, `sha2`, `ciborium`, `toml`.

Phase 2 is split into three sub-phases, each producing a testable artifact:

#### Phase 2A: Foundation (steps 2.1–2.6)

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

#### Phase 2B: Protocol engine (steps 2.7–2.9)

Connect the foundation modules into the main frame-processing loop. The gateway can authenticate nodes, dispatch commands, and serve program chunks.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.7 | Core protocol loop | Frame recv → auth → session → dispatch → response | T-0101 to T-0106, T-0600 to T-0603, T-0609 |
| 2.8 | Command handling | NOP, UPDATE_PROGRAM, RUN_EPHEMERAL, UPDATE_SCHEDULE, REBOOT | T-0200 to T-0205 |
| 2.9 | Chunked transfer | GET_CHUNK → CHUNK serving, PROGRAM_ACK | T-0300 to T-0302 |

**Exit criteria (2B):** A node can complete a full wake cycle (WAKE → COMMAND → chunked transfer → PROGRAM_ACK → APP_DATA) against the gateway using the mock transport. All protocol and command tests pass.

#### Phase 2C: Handler API and admin (steps 2.10–2.12)

Application data routing, handler process management, gRPC admin API, configuration, and startup/shutdown. Phase 2C is split into three sub-phases:

#### Phase 2C-i: Handler router (step 2.10)

Handler process management and APP_DATA routing. The gateway can forward application data to external handler processes and relay replies.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.10a | `handler.rs` (transport) | Handler framing: 4B length-prefix + CBOR encode/decode over stdin/stdout | T-0504 |
| 2.10b | `handler.rs` (process) | HandlerProcess: spawn, write DATA, read DATA_REPLY, LOG handling, respawn/crash | T-0505, T-0506, T-0510, T-0511, T-0513 |
| 2.10c | `handler.rs` (router) | HandlerRouter: program_hash → handler config routing, catch-all, no-match | T-0507, T-0508, T-0509 |
| 2.10d | `engine.rs` (integration) | Wire APP_DATA dispatch through handler router, APP_DATA_REPLY back to node | T-0500, T-0501, T-0502, T-0503 |
| 2.10e | `handler.rs` (events) | EVENT messages: node_online, program_updated, node_timeout (engine wiring deferred to Phase 2C-ii) | T-0512 (smoke test) |

**Exit criteria (2C-i):** All handler API tests pass (T-0500 to T-0513). APP_DATA flows end-to-end from node through engine to handler process and back.

#### Phase 2C-ii: Admin API (step 2.11)

gRPC admin API for node/program management and operational commands.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.11a | `proto/admin.proto` | gRPC service definition | Compile check |
| 2.11b | `admin.rs` | gRPC service: node CRUD, program ingestion, schedule/reboot/ephemeral queueing, status, export/import | T-0800 to T-0810 |

**Exit criteria (2C-ii):** All admin API tests pass (T-0800 to T-0810).

#### Phase 2C-iii: Config and startup (step 2.12)

Configuration loading, startup/shutdown sequence, and operational tests.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.12a | `config.rs` | Configuration structs, TOML loading | Unit tests |
| 2.12b | `main.rs` | Startup/shutdown sequence | T-1000 to T-1004 |

**Exit criteria (2C):** `cargo test -p sonde-gateway` passes all gateway validation tests. The gateway is a complete, runnable service.

#### Phase 2D: Modem transport adapter (step 2.13)

USB modem serial transport. The gateway can communicate with nodes via an ESP32-S3 radio modem attached over USB-CDC.

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.13 | `modem.rs` | `UsbEspNowTransport`: serial reader task, message demux, startup sequence (RESET → MODEM_READY → SET_CHANNEL), health monitor, error handling | T-1100 to T-1108 |

**Design doc:** [gateway-design.md §4.2](gateway-design.md)
**Validation:** [gateway-validation.md §11](gateway-validation.md)
**Dependencies:** `tokio-serial` (async serial port), `sonde-protocol::modem` (shared codec).

**Test approach:** All tests use a PTY-based `MockModem` — no physical modem hardware required. The mock modem speaks the serial protocol on a PTY slave and simulates modem behavior (MODEM_READY, RECV_FRAME injection, SEND_FRAME capture, STATUS responses).

**Exit criteria (2D):** All modem transport tests pass (T-1100 to T-1108). A full wake cycle works end-to-end over the PTY mock transport.

---

### Phase 3: `sonde-node` crate

**Goal:** Working node firmware for ESP32-C3/S3.

**Design doc:** [node-design.md](node-design.md)  
**Validation:** [node-validation.md](node-validation.md)  
**Dependencies:** `sonde-protocol`, `esp-idf-hal`, `esp-idf-svc`, BPF interpreter (rbpf or uBPF).

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 3.1 | `crypto.rs` | ESP-IDF hardware HMAC/SHA `HmacProvider` impl | Unit tests |
| 3.2 | `transport.rs` | ESP-NOW send/receive | T-N100, T-N102 |
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

**Exit criteria:** All 55 node validation tests pass (host-based where possible, hardware-in-the-loop for the rest).

---

### Phase 4: `sonde-admin` CLI tool

**Goal:** A CLI that wraps the gateway gRPC API and handles USB pairing.

**Design doc:** [gateway-design.md §13](gateway-design.md)  
**Requirements:** GW-0806  
**Dependencies:** `tonic` (gRPC client), `clap` (CLI parsing), `serialport` (USB serial).

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 4.1 | `grpc_client.rs` | Connect to gateway, call all admin RPCs | Integration test against running gateway |
| 4.2 | `usb.rs` | USB serial: write PSK, factory reset | Manual test with hardware |
| 4.3 | `main.rs` | CLI argument parsing, command dispatch, JSON output | CLI smoke tests |

**Exit criteria:** All `sonde-admin` commands work against a running gateway instance. USB pairing tested with hardware.

---

### Phase 5: `sonde-modem` crate

**Goal:** Working ESP32-S3 radio modem firmware that bridges USB-CDC to ESP-NOW.

**Design doc:** [modem-design.md](modem-design.md)
**Validation:** [modem-validation.md](modem-validation.md) (20 tests)
**Requirements:** [modem-requirements.md](modem-requirements.md) (17 requirements, MD-0100 to MD-0303)
**Dependencies:** `sonde-protocol` (modem codec), `esp-idf-hal`, `esp-idf-svc`.

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 5.1 | `usb_cdc.rs` | USB-CDC ACM init, read/write, DTR disconnect detection | T-0100, T-0101 |
| 5.2 | `bridge.rs` (codec) | Serial frame decode/dispatch, outbound encoding (uses `sonde-protocol::modem`) | T-0102, T-0103, T-0104 |
| 5.3 | `espnow.rs` | ESP-NOW init, recv callback → RECV_FRAME, send path | T-0200, T-0201 |
| 5.4 | `peer_table.rs` | Auto peer registration, LRU eviction | T-0202, T-0203 |
| 5.5 | `bridge.rs` (commands) | SET_CHANNEL, GET_STATUS, SCAN_CHANNELS dispatch | T-0205, T-0206 |
| 5.6 | `status.rs` | Counters, uptime tracking, STATUS response | T-0302 |
| 5.7 | `main.rs` | Main loop, RESET handling, MODEM_READY, watchdog | T-0300, T-0301, T-0303 |
| 5.8 | Error handling | Invalid frames, bad channel, short body | T-0400, T-0401, T-0402 |
| 5.9 | Integration | Frame ordering, content transparency | T-0204, T-0500 |

**Shared code with `sonde-node`:** The ESP-NOW driver (`espnow.rs`) should be extracted into a shared module or internal crate that both `sonde-modem` and `sonde-node` depend on. This covers WiFi/ESP-NOW init, send with peer management, and receive callback registration.

**Test approach:** Tests T-0100 to T-0104 and T-0300 to T-0303 can be run with only a USB connection (no radio peer). Tests T-0200 to T-0206 and T-0500 require a second ESP32 acting as a radio peer. Tests T-0400 to T-0402 can be run with USB only.

**Exit criteria:** All 20 modem validation tests pass. The modem bridges a full gateway wake cycle (WAKE → COMMAND → chunked transfer → PROGRAM_ACK) between the gateway and a real sensor node.

---

## 4  Build and test commands

```bash
# Build everything
cargo build --workspace

# Test protocol crate (Phase 1 — runs anywhere)
cargo test -p sonde-protocol

# Test gateway (Phase 2 — runs anywhere, uses mocks)
cargo test -p sonde-gateway

# Build node firmware for ESP32-C3
cargo build -p sonde-node --target riscv32imc-esp-espidf

# Build node firmware for ESP32-S3
cargo build -p sonde-node --target xtensa-esp32s3-espidf

# Build modem firmware for ESP32-S3
cargo build -p sonde-modem --target xtensa-esp32s3-espidf

# Build admin CLI
cargo build -p sonde-admin
```

### 4.1  CI pipeline

The CI pipeline should run:

1. `cargo fmt --check --workspace` — formatting.
2. `cargo clippy --workspace` — lint.
3. `cargo test -p sonde-protocol` — protocol crate tests (fast, no deps).
4. `cargo test -p sonde-gateway` — gateway tests (mock transport/storage, PTY modem mock).
5. `cargo build -p sonde-node --target riscv32imc-esp-espidf` — node firmware compiles.
6. `cargo build -p sonde-modem --target xtensa-esp32s3-espidf` — modem firmware compiles.
7. `cargo build -p sonde-admin` — admin CLI compiles.

Node and modem firmware tests that require hardware run in a separate hardware-in-the-loop CI stage.

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

The `proto/admin.proto` file defines the gRPC admin API. Both `sonde-gateway` (server) and `sonde-admin` (client) use `tonic-build` to generate Rust code from it. The proto file is the single source of truth for the admin API wire format.

### 5.3  Test program compilation

The `test-programs/` directory contains BPF C source files. A build script (or Makefile) compiles them to ELF, then the gateway's ingestion pipeline converts them to CBOR program images. The resulting images are used by both gateway and node validation tests.

```bash
# Compile test programs (requires BPF toolchain)
clang -target bpf -O2 -c test-programs/nop.c -o test-programs/nop.o

# Ingest via gateway (or a standalone tool) to produce CBOR images
sonde-admin program ingest test-programs/nop.o --profile resident
```
