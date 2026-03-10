# Implementation Guide

> **Document status:** Draft  
> **Scope:** Workspace layout, build instructions, and implementation order for the Sonde project.  
> **Audience:** Implementers (human or LLM agent) building the Sonde system.  
> **Related:** [gateway-design.md](gateway-design.md), [node-design.md](node-design.md), [protocol-crate-design.md](protocol-crate-design.md)

---

## 1  Overview

The Sonde codebase is a Rust workspace containing four crates. This document defines the workspace layout and the order in which crates and modules should be implemented and tested.

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
│   │       └── transport.rs      # ESP-NOW send/receive
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
    "crates/sonde-admin",
]
```

### 2.2  Crate dependencies

```
sonde-protocol  (no_std + alloc, no platform deps)
       │
       ├──── sonde-gateway  (std, tokio, tonic, prevail-rust, RustCrypto)
       │
       ├──── sonde-node     (std via ESP-IDF, esp-idf-hal, esp-idf-svc, rbpf/ubpf)
       │
       └──── sonde-admin    (std, tonic, clap, serialport)
```

`sonde-protocol` is the only shared dependency. The other three crates do not depend on each other.

---

## 3  Implementation phases

### Phase 1: `sonde-protocol` crate

**Goal:** A fully tested, platform-independent protocol library.

**Design doc:** [protocol-crate-design.md](protocol-crate-design.md)  
**Validation:** [protocol-crate-validation.md](protocol-crate-validation.md) (41 tests)  
**Dependencies:** `ciborium` only.

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
| 1.9 | Integration | Full frame round-trips | T-P060 to T-P062 |

**Test HMAC/SHA providers:** Implement a software `HmacProvider` and `Sha256Provider` using `hmac`, `sha2` crates in `#[cfg(test)]` for running the protocol crate's own tests.

**Exit criteria:** `cargo test -p sonde-protocol` passes all 41 tests.

---

### Phase 2: `sonde-gateway` crate

**Goal:** A working gateway service that can authenticate nodes, manage sessions, serve programs, and route application data.

**Design doc:** [gateway-design.md](gateway-design.md)  
**Validation:** [gateway-validation.md](gateway-validation.md) (61 tests)  
**Dependencies:** `sonde-protocol`, `tokio`, `tonic`, `prevail-rust`, `hmac`, `sha2`, `ciborium`, `toml`.

**Module order:**

| Step | Module | What to build | Test with |
|---|---|---|---|
| 2.1 | `crypto.rs` | `RustCryptoHmac`, `RustCryptoSha256` implementing protocol traits | Unit tests |
| 2.2 | `transport.rs` | `Transport` trait (mock impl for testing) | T-0100 |
| 2.3 | `storage.rs` | `Storage` trait (in-memory mock impl for testing) | Unit tests |
| 2.4 | `registry.rs` | `NodeRecord`, key lookup by `key_hint`, CRUD operations | T-0700, T-0702, T-0703 |
| 2.5 | `session.rs` | `Session`, `SessionManager` — create/replace/timeout/lookup | T-0604 to T-0607, T-1004 |
| 2.6 | `program.rs` | `ProgramRecord`, ingestion (ELF → verify → CBOR), chunk serving | T-0400 to T-0407 |
| 2.7 | Core protocol loop | Frame recv → auth → session → dispatch → response | T-0101 to T-0106, T-0600 to T-0603, T-0609 |
| 2.8 | Command handling | NOP, UPDATE_PROGRAM, RUN_EPHEMERAL, UPDATE_SCHEDULE, REBOOT | T-0200 to T-0205 |
| 2.9 | Chunked transfer | GET_CHUNK → CHUNK serving, PROGRAM_ACK | T-0300 to T-0302 |
| 2.10 | `handler.rs` | Handler router, process lifecycle, DATA/REPLY/EVENT/LOG | T-0500 to T-0513 |
| 2.11 | `admin.rs` | gRPC admin API | T-0800 to T-0810 |
| 2.12 | `config.rs` + `main.rs` | Configuration, startup/shutdown | T-1000 to T-1003 |

**Exit criteria:** `cargo test -p sonde-gateway` passes all 61 tests.

---

### Phase 3: `sonde-node` crate

**Goal:** Working node firmware for ESP32-C3/S3.

**Design doc:** [node-design.md](node-design.md)  
**Validation:** [node-validation.md](node-validation.md) (55 tests)  
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

# Build admin CLI
cargo build -p sonde-admin
```

### 4.1  CI pipeline

The CI pipeline should run:

1. `cargo fmt --check --workspace` — formatting.
2. `cargo clippy --workspace` — lint.
3. `cargo test -p sonde-protocol` — protocol crate tests (fast, no deps).
4. `cargo test -p sonde-gateway` — gateway tests (mock transport/storage).
5. `cargo build -p sonde-node --target riscv32imc-esp-espidf` — node firmware compiles.
6. `cargo build -p sonde-admin` — admin CLI compiles.

Node firmware tests that require hardware run in a separate hardware-in-the-loop CI stage.

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
