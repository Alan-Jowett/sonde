# Copilot Instructions for Sonde

## Build and test

```bash
# Build all crates
cargo build --workspace

# Lint all crates
cargo clippy --workspace -- -D warnings

# Test all crates
cargo test --workspace

# Test protocol crate (fast, no deps — run this first)
cargo test -p sonde-protocol

# Run a single test
cargo test -p sonde-protocol test_p001

# Build ESP32-C3 firmware (requires esp toolchain).
# ESP-IDF generates deeply nested build paths that exceed Windows MAX_PATH (260 chars).
# Use a short CARGO_TARGET_DIR at the root of the current drive to avoid this.
# Pick a single-letter directory name not already in use (e.g., E:\b, F:\t, C:\z).
# Example (adjust drive letter and dir name to your environment):
CARGO_TARGET_DIR=F:\t cargo +esp build -p sonde-node --bin node --features esp --profile firmware --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort

# Build sonde-pair for Android (requires Android dev container or local NDK).
# NOTE: sonde-pair crate is planned — see issue #163.
# docker run --rm -v .:/sonde -w /sonde ghcr.io/alan-jowett/sonde-android-dev:latest \
#   cargo ndk -t arm64-v8a build -p sonde-pair --release

# Build ESP32 firmware using the dev container (no local toolchain needed):
docker run --rm -v "$(pwd)":/sonde -w /sonde ghcr.io/alan-jowett/sonde-esp-dev:latest \
    cargo +esp build -p sonde-node --bin node --features esp --profile firmware \
    --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
```

## Architecture

Sonde is a programmable sensor node platform. Nodes run BPF programs distributed by a gateway over ESP-NOW radio. The workspace contains the following crates:

- **`sonde-protocol`** — Shared `no_std` protocol crate. Frame codec, CBOR messages, program image format. Used by all other crates. No platform dependencies; all crypto is injected via `HmacProvider`/`Sha256Provider` traits.
- **`sonde-gateway`** — Async gateway service (tokio). Authenticates nodes, manages sessions, distributes BPF programs, routes app data to handler processes via stdin/stdout. Admin interface via local gRPC.
- **`sonde-node`** — ESP32-C3/S3 firmware (Rust + ESP-IDF). Cyclic state machine: wake → WAKE/COMMAND → BPF execution → sleep. BPF interpreter behind a `BpfInterpreter` trait.
- **`sonde-modem`** — ESP32-S3 USB-CDC modem firmware. Bridges ESP-NOW radio and BLE GATT to the gateway over serial. Hosts the Gateway Pairing Service for BLE-based node provisioning.
- **`sonde-admin`** — CLI tool wrapping the gateway gRPC API. Handles USB-mediated node pairing.
- **`sonde-pair`** *(planned)* — BLE pairing tool (Tauri v2 app). Will target Android (`aarch64-linux-android`), Windows, and Linux. Cross-compiled using the Android dev container (`ghcr.io/alan-jowett/sonde-android-dev`). See `.devcontainer/android/` for VS Code / Codespaces setup.
- **`sonde-bpf`** — Safe BPF interpreter with tagged register tracking. Used by `sonde-node`.
- **`sonde-e2e`** — End-to-end test harness.

The implementation order is: protocol → gateway → node → admin. See `docs/implementation-guide.md` for the full phased build plan with module-by-module ordering and test references.

## Key conventions

- **SPDX headers** on all `.rs` files: `// SPDX-License-Identifier: MIT` + `// Copyright (c) 2026 sonde contributors`
- **Use backticks** (not backslash-escaped quotes) to wrap identifiers in PR descriptions and commit messages.
- **Protocol wire format** uses a fixed 11-byte binary header (`key_hint` 2B BE + `msg_type` 1B + `nonce` 8B BE) + CBOR payload + 32-byte HMAC-SHA256. The `nonce` field carries a random nonce for WAKE, and a gateway-assigned sequence number for all post-WAKE messages.
- **CBOR maps use integer keys** (not strings) for compactness. Protocol message keys and program image keys are separate keyspaces — both start at 1 but are unrelated.
- **Program images** are CBOR-encoded (bytecode + map definitions), not raw ELF. The gateway extracts from ELF at ingestion time. `program_hash` = SHA-256 of the CBOR image. Deterministic CBOR encoding (RFC 8949 §4.2) is required.
- **Platform-specific behavior** is always injected via traits (`HmacProvider`, `Sha256Provider`, `Transport`, `Storage`, `BpfInterpreter`), never hard-coded.
- **Error handling on the radio protocol** is silent discard — no error responses are ever sent. This is a security design decision.

## Code quality guidelines

These guidelines address the most common review feedback patterns. Following them will significantly reduce PR round-trips.

### Database and schema changes

- **Always add migrations** when changing database schemas. Adding a column to `CREATE TABLE` does not update existing databases. Use `PRAGMA table_info()` + `ALTER TABLE ADD COLUMN` in `open()` for SQLite.
- **Wrap multi-row mutations in a transaction.** Especially for migrations — a crash mid-loop must not leave a partially-migrated database.

### Input validation and error handling

- **Validate all input lengths and ranges before use.** Hex parsers should validate ASCII-only before byte-indexing (non-ASCII UTF-8 causes panics on `&s[i..i+2]`). Validate expected sizes (e.g., SHA-256 hashes must be exactly 32 bytes / 64 hex chars).
- **Use checked/saturating arithmetic** for any size or offset calculation that feeds into `unsafe` code or allocation. Wrapping overflow can violate safety invariants.
- **Don't mask errors.** Matching all non-OK error codes as a single "already exists" case hides real failures. Handle specific error codes explicitly.
- **Don't silently truncate I/O.** If `write()` returns 0 bytes written (timeout), treat it as an error or retry — don't return `Ok(())` with unsent data.
- **Validate array indices** from external input (e.g., `map_arg`, partition indices) before indexing. Out-of-bounds panics crash the firmware.

### Cryptography and security

- **Use `getrandom::fill()` for cryptographic randomness**, not `rand::rng()`. The `rand::rng()` API is unstable across versions and doesn't explicitly guarantee OS CSPRNG.
- **Wrap key material in `zeroize::Zeroizing<[u8; N]>`** to ensure it's zeroed on drop. Pass by reference (`&[u8; 32]`) to internal functions to minimize copies.
- **`key_hint` derivation** is `u16::from_be_bytes(SHA-256(PSK)[30..32])` — the **lower** 16 bits (least-significant bytes), not the first two bytes.
- **Never accept plaintext fallbacks in decryption paths.** Legacy format handling must be confined to explicit migration functions that run once at startup, not in the general read path where it creates an injection vector.

### Testing

- **Every new code path needs a test.** If the PR description mentions a test, it must exist in the diff.
- **Test boundary conditions**, not just the happy path. For budget/limit features, test exact-boundary (succeeds) and boundary+1 (fails).
- **Test new API fields end-to-end.** If you add `abi_version` to the proto/storage, add a test that ingests with a value and asserts it round-trips through list/get.
- **Capture tracing output** when asserting that warnings/errors are logged. Use `tracing-test` with `#[traced_test]` or a per-test subscriber — don't claim "verified structurally."
- **Use clearly non-zero test keys.** `[0x42u8; 32]` not `[0u8; 32]` — avoids normalizing insecure patterns in example code.

### Documentation and API consistency

- **Keep doc comments in sync with implementation.** If behavior changes (e.g., function gains a parameter, return conditions change), update the doc comment in the same commit.
- **Update crate-level docs** when public API changes. If `HelperDescriptor` replaces `(id, fn)` registration, the crate docs must reflect the new interface.
- **New public API parameters need doc comments** explaining their purpose and how to use the common case (e.g., `UNLIMITED_BUDGET` constant for opt-out).

### Embedded / ESP-IDF specific

- **Never hard-code FreeRTOS tick rates.** If the code depends on `CONFIG_FREERTOS_HZ`, either derive the value from ESP-IDF at build time or pin it in `sdkconfig.defaults` and document the coupling.
- **`sdkconfig.defaults` must explicitly set every value the code depends on.** Don't assume ESP-IDF defaults match your expectations (e.g., stack size, tick rate, console UART).
- **Minimize heap allocations in loops.** Pre-compute constant frames, pass slices instead of cloning with `to_vec()`, and only allocate `Vec`s when the data is truly variable.
- **Avoid `format!()` in error paths on embedded.** `format!()` allocates on the heap. Use `&'static str` or `Cow<'static, str>` for error messages in `no_std`-adjacent code.

### Unsafe code

- **Minimize the `unsafe` surface.** If only one code path needs `unsafe` (e.g., map regions), provide a safe wrapper for the common case (e.g., `execute_program_no_maps`).
- **Don't update `self` state before validation is complete.** Build results into temporaries and commit only after all checks pass — a failed `load()` should not leave the interpreter in a partially-initialized state.
- **Validate raw pointer inputs** (null check, range check) at the `unsafe` boundary, not deep inside the call chain.

## Documentation structure

The `docs/` directory contains the complete specification stack. When implementing a feature, read the relevant docs in this order:

1. **Requirements** (`gateway-requirements.md` or `node-requirements.md`) — what to build
2. **Design** (`gateway-design.md`, `node-design.md`, or `protocol-crate-design.md`) — how to build it
3. **Validation** (`gateway-validation.md`, `node-validation.md`, or `protocol-crate-validation.md`) — test cases to implement and pass
4. **Protocol** (`protocol.md`) and **Security** (`security.md`) — wire format and security model reference
