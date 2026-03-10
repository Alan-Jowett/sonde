# Copilot Instructions for Sonde

## Build and test

```bash
# Build all crates
cargo build --workspace

# Test protocol crate (fast, no deps — run this first)
cargo test -p sonde-protocol

# Run a single test
cargo test -p sonde-protocol test_p001

# Future crates (not yet implemented)
cargo build -p sonde-gateway
cargo build -p sonde-node --target riscv32imc-esp-espidf
cargo build -p sonde-admin
```

## Architecture

Sonde is a programmable sensor node platform. Nodes run BPF programs distributed by a gateway over ESP-NOW radio. The system has four crates in a Cargo workspace:

- **`sonde-protocol`** — Shared `no_std` protocol crate. Frame codec, CBOR messages, program image format. Used by all other crates. No platform dependencies; all crypto is injected via `HmacProvider`/`Sha256Provider` traits.
- **`sonde-gateway`** (planned) — Async gateway service (tokio). Authenticates nodes, manages sessions, distributes BPF programs, routes app data to handler processes via stdin/stdout. Admin interface via local gRPC.
- **`sonde-node`** (planned) — ESP32-C3/S3 firmware (Rust + ESP-IDF). Cyclic state machine: wake → WAKE/COMMAND → BPF execution → sleep. BPF interpreter behind a `BpfInterpreter` trait (backend TBD).
- **`sonde-admin`** (planned) — CLI tool wrapping the gateway gRPC API. Handles USB-mediated node pairing.

The implementation order is: protocol → gateway → node → admin. See `docs/implementation-guide.md` for the full phased build plan with module-by-module ordering and test references.

## Key conventions

- **SPDX headers** on all `.rs` files: `// SPDX-License-Identifier: MIT` + `// Copyright (c) 2026 sonde contributors`
- **Use backticks** (not backslash-escaped quotes) to wrap identifiers in PR descriptions and commit messages.
- **Protocol wire format** uses a fixed 11-byte binary header (`key_hint` 2B BE + `msg_type` 1B + `nonce` 8B BE) + CBOR payload + 32-byte HMAC-SHA256. The `nonce` field carries a random nonce for WAKE, and a gateway-assigned sequence number for all post-WAKE messages.
- **CBOR maps use integer keys** (not strings) for compactness. Protocol message keys and program image keys are separate keyspaces — both start at 1 but are unrelated.
- **Program images** are CBOR-encoded (bytecode + map definitions), not raw ELF. The gateway extracts from ELF at ingestion time. `program_hash` = SHA-256 of the CBOR image. Deterministic CBOR encoding (RFC 8949 §4.2) is required.
- **Platform-specific behavior** is always injected via traits (`HmacProvider`, `Sha256Provider`, `Transport`, `Storage`, `BpfInterpreter`), never hard-coded.
- **Error handling on the radio protocol** is silent discard — no error responses are ever sent. This is a security design decision.

## Documentation structure

The `docs/` directory contains the complete specification stack. When implementing a feature, read the relevant docs in this order:

1. **Requirements** (`gateway-requirements.md` or `node-requirements.md`) — what to build
2. **Design** (`gateway-design.md`, `node-design.md`, or `protocol-crate-design.md`) — how to build it
3. **Validation** (`gateway-validation.md`, `node-validation.md`, or `protocol-crate-validation.md`) — test cases to implement and pass
4. **Protocol** (`protocol.md`) and **Security** (`security.md`) — wire format and security model reference
