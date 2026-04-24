<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Admin CLI Design Specification

> **Document status:** Draft
> **Scope:** Architecture and internal design of the `sonde-admin` CLI tool.
> **Audience:** Implementers (human or LLM agent) maintaining the admin CLI.
> **Related:** [admin-requirements.md](admin-requirements.md),
> [admin-validation.md](admin-validation.md),
> [gateway-design.md](gateway-design.md) §13,
> [gateway-requirements.md](gateway-requirements.md) §9A

---

## 1  Overview

`sonde-admin` is a thin CLI wrapper around the gateway's gRPC admin API
(GW-0800). It translates human-friendly command-line arguments into gRPC calls
and formats RPC responses for terminal or machine consumption. The CLI itself
contains no business logic — all operational semantics live in the gateway.

The tool has three responsibilities:

1. **Argument parsing** — validate and transform CLI inputs (hex decoding, file I/O, passphrase resolution).
2. **RPC dispatch** — connect to the gateway and invoke the appropriate gRPC method.
3. **Output formatting** — present results as human-readable text or machine-readable JSON.

---

## 2  Technology choices

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust | Shared toolchain with all Sonde crates |
| CLI framework | `clap` 4 (derive) | Declarative, type-safe argument parsing with built-in help generation |
| gRPC client | `tonic` 0.14 | Same stack as the gateway server; generates client stubs from `admin.proto` |
| Serialization | `serde_json` | JSON output format |
| Hex codec | `hex` 0.4 | PSK and program hash encoding/decoding |
| Passphrase input | `rpassword` 7.x | Cross-platform no-echo TTY input |
| Timestamp formatting | `chrono` 0.4 | UTC date formatting for `last_seen_ms` fields |
| Build metadata | `build.rs` | Injects git commit SHA at compile time (GW-1303) |

---

## 3  Module architecture

```
┌────────────────────────────────────────────────────┐
│  sonde-admin                                       │
│                                                    │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────┐  │
│  │  main.rs     │──│ grpc_client  │──│  tonic   │──── gateway
│  │  (clap CLI)  │  │  .rs         │  │  channel │  │  (gRPC)
│  └──────┬───────┘  └──────────────┘  └──────────┘  │
│         │                                          │
│  ┌──────┴───────┐                                  │
│  │  lib.rs      │                                  │
│  │  (utilities) │                                  │
│  └──────────────┘                                  │
└────────────────────────────────────────────────────┘
```

### 3.1  Module responsibilities

| Module | Responsibility | Requirements covered |
|--------|---------------|---------------------|
| **`main.rs`** | CLI argument definition (clap derive structs), subcommand dispatch, output formatting, confirmation prompts, passphrase resolution, error presentation | ADMIN-0100, ADMIN-0102, ADMIN-0103, ADMIN-0104, ADMIN-0105, ADMIN-0106, ADMIN-0107, ADMIN-02XX–ADMIN-08XX |
| **`grpc_client.rs`** | `AdminClient` struct wrapping `tonic::GatewayAdminClient`, platform-specific `connect()`, typed RPC wrappers | ADMIN-0101 |
| **`lib.rs`** | Shared utilities: `format_epoch_ms()`, protobuf module re-export | ADMIN-0107 |
| **`build.rs`** | Proto compilation, git SHA injection | ADMIN-0106 |

---

## 4  Transport layer

### 4.1  Platform-specific connection

The `AdminClient::connect()` method uses compile-time `#[cfg]` to select
the transport:

- **Unix** (`#[cfg(unix)]`): Connects via `tokio::net::UnixStream` to a
  Unix domain socket. The URI passed to tonic is a placeholder
  (`http://[::]:50051`) — the actual connection uses the `UnixStream`.

- **Windows** (`#[cfg(windows)]`): Connects via
  `tokio::net::windows::named_pipe::ClientOptions`. If the pipe returns
  `ERROR_PIPE_BUSY` (OS error 231), the client retries every 50ms for up
  to 5 seconds before returning a timeout error.

- **Other platforms**: A `compile_error!` prevents compilation on unsupported
  platforms.

### 4.2  Connection wrapper

Both paths use `tower::service_fn` as a tonic connector, wrapping the
platform stream in `hyper_util::rt::TokioIo` for HTTP/2 framing.

---

## 5  CLI argument parsing

### 5.1  Global flags

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--socket` | `String` | Platform-dependent (see §4.1) | Gateway endpoint |
| `--format` | `text \| json` | `text` | Output format |
| `--yes` / `-y` | `bool` | `false` | Skip confirmation prompts |
| `--verbose` / `-v` | `bool` | `false` | Show full error diagnostics |

### 5.2  Subcommand tree

```
sonde-admin
├── node
│   ├── list
│   ├── get <node-id>
│   ├── register <node-id> <key-hint:u16> <psk-hex>
│   ├── remove <node-id>
│   └── factory-reset <node-id>
├── program
│   ├── ingest <file> --profile resident|ephemeral
│   ├── list
│   ├── assign <node-id> <program-hash>
│   └── remove <program-hash>
├── schedule
│   └── set <node-id> <interval-s:u32>
├── reboot <node-id>
├── ephemeral <node-id> <program-hash>
├── status <node-id>
├── state
│   ├── export <file> [--passphrase <pass>]
│   └── import <file> [--passphrase <pass>]
├── modem
│   ├── status
│   ├── set-channel <channel:1-14>
│   ├── scan
│   └── display <line> [<line> ...]
├── pairing
│   ├── start [--duration-s <seconds>]
│   ├── stop
│   ├── list-phones
│   └── revoke-phone <phone-id:u32>
└── handler
    ├── add <program-hash> <command> [args...] [--working-dir] [--reply-timeout-ms]
    ├── remove <program-hash>
    └── list
```

### 5.3  Client-side input validation

The CLI validates the following inputs before sending RPCs:

| Input | Validation | Requirement |
|-------|-----------|-------------|
| `psk-hex` | `hex::decode` + length == 32 bytes | ADMIN-0202 |
| `program-hash` | `hex::decode` for commands that send a binary hash (`program assign`, `program remove`, `ephemeral`); handler commands pass through `*` or the provided string without local validation (gateway enforces) | ADMIN-0302, ADMIN-0800 |
| `channel` | clap `value_parser!(u32).range(1..=14)` | ADMIN-0601 |
| `display` lines | variadic positional argument with clap `num_args = 1..=4`; each argument maps to one display line | ADMIN-0603 |
| `passphrase` | Non-empty check | ADMIN-0502 |

---

## 6  Output formatting

### 6.1  Dual-path pattern

Every subcommand handler follows a consistent pattern:

```rust
if json {
    print_json(&serde_json::json!({ ... }))?;
} else {
    println!("Human-readable text");
}
```

`print_json` uses `serde_json::to_string_pretty` for readability.

### 6.2  Timestamp formatting

The `format_epoch_ms()` function in `lib.rs` converts millisecond Unix
timestamps to `YYYY-MM-DD HH:MM:SS UTC` format using `chrono`. Invalid
or out-of-range values produce `<invalid timestamp: {value}>`.

### 6.3  Hex encoding

Program hashes and PSKs are displayed as lowercase hex strings via
`hex::encode()`.

### 6.4  Node display

The `print_node()` helper displays: node ID, key hint, assigned program hash,
current program hash, battery (mV), last seen (formatted), and schedule
interval. Optional fields are omitted when absent.

### 6.5  Command → RPC → output matrix

| Command | gRPC RPC | Confirmation | JSON fields | Text format |
|---------|----------|-------------|-------------|-------------|
| `node list` | `ListNodes` | — | `[{node_id, key_hint, ...}]` | Per-node detail block |
| `node get` | `GetNode` | — | `{node_id, key_hint, ...}` | Detail block |
| `node register` | `RegisterNode` | — | `{node_id}` | "Registered node: {id}" |
| `node remove` | `RemoveNode` | Yes | `{removed}` | "Removed node: {id}" |
| `node factory-reset` | `FactoryReset` | Yes | `{factory_reset}` | "Factory reset node: {id}" |
| `program ingest` | `IngestProgram` | — | `{program_hash, program_size}` | "Ingested program: {hash} ({size} bytes)" |
| `program list` | `ListPrograms` | — | `[{hash, size, profile, source_filename}]` | Per-program line |
| `program assign` | `AssignProgram` | — | `{assigned: true}` | "Assigned program {hash} to node {id}" |
| `program remove` | `RemoveProgram` | Yes | `{removed}` | "Removed program: {hash}" |
| `schedule set` | `SetSchedule` | — | `{node_id, interval_s}` | "Set schedule for {id}: {s}s" |
| `reboot` | `QueueReboot` | — | `{queued, node_id}` | "Queued reboot for node: {id}" |
| `ephemeral` | `QueueEphemeral` | — | `{queued, node_id, program_hash}` | "Queued ephemeral program ..." |
| `status` | `GetNodeStatus` | — | `{node_id, current_program_hash, ...}` | Multi-line status |
| `state export` | `ExportState` | — | `{exported_bytes, file}` | "Exported {n} bytes to {file}" |
| `state import` | `ImportState` | Yes | `{imported: true, file}` | "Imported state from {file}" |
| `modem status` | `GetModemStatus` | — | `{channel, tx_count, ...}` | Multi-line status |
| `modem set-channel` | `SetModemChannel` | — | `{channel}` | "Set modem channel to {ch}" |
| `modem scan` | `ScanModemChannels` | — | `[{channel, ap_count, strongest_rssi}]` | Table with headers |
| `modem display` | `ShowModemDisplayMessage` | — | `{lines, duration_s}` | "Displayed modem message for 60s" |
| `pairing start` | `OpenBlePairing` (stream) | — | N/A (interactive) | Event-by-event text |
| `pairing stop` | `CloseBlePairing` | Yes | `{status}` | "BLE pairing window closed" |
| `pairing list-phones` | `ListPhones` | — | `[{phone_id, ...}]` | Table with headers |
| `pairing revoke-phone` | `RevokePhone` | Yes | `{phone_id, status}` | "Phone {id} revoked" |
| `handler add` | `AddHandler` | — | `{added, program_hash}` | "Added handler for program {hash}" |
| `handler remove` | `RemoveHandler` | — | `{removed}` | "Removed handler for program {hash}" |
| `handler list` | `ListHandlers` | — | `[{program_hash, command, ...}]` | Per-handler line |

---

## 7  Error handling

### 7.1  Connection errors

If `AdminClient::connect()` fails, the CLI prints an error to stderr
including the endpoint path and exits with code 1.

### 7.2  gRPC errors

The `run()` function returns `Result<(), Box<dyn Error>>`. The `main()`
function inspects the error:

1. If it downcasts to `tonic::Status`, extract the message.
2. If the message contains newlines (multi-line diagnostics):
   - **Default mode**: print summary line + first error + "run with --verbose" hint.
   - **Verbose mode**: print full message.
3. If single-line: print the full message.
4. If not a `tonic::Status`: print the error with `Display`.
5. Exit with code 1.

### 7.3  Local validation errors

Client-side validation errors (hex decode, PSK length, empty passphrase)
are returned as `Box<dyn Error>` and follow the same exit-code-1 path.

---

## 8  Passphrase resolution

The `resolve_passphrase()` function implements the priority chain:

```
CLI --passphrase arg (or SONDE_PASSPHRASE env via clap)
    └─→ rpassword::read_password() (TTY prompt, no echo)
        └─→ Error if empty or unavailable
```

The `Passphrase:` prompt is written to stderr (not stdout) to avoid
contaminating piped output.

---

## 9  Confirmation prompts

The `confirm()` function implements:

1. If `--yes`: return `Ok(())` immediately.
2. If stdin is not a TTY: return `Err` with a message directing the user to use `--yes`.
3. Otherwise: print `{message} [y/N]:` to stderr, read one line from stdin.
4. Accept only `y` or `Y`; anything else (including empty input) aborts.

---

## 10  Build metadata

### 10.1  Build script

`build.rs` performs two tasks:

1. **Proto compilation**: Compiles `admin.proto` from the `sonde-gateway` crate using `tonic-prost-build`, generating client-only stubs.
2. **Git SHA injection**: Sets `SONDE_GIT_COMMIT` via `cargo:rustc-env`. Prefers the `SONDE_GIT_COMMIT` environment variable (set by CI) over running `git rev-parse --short HEAD`. Truncates to 7 characters.

### 10.2  Version string

The clap `#[command]` attribute concatenates the crate version and git SHA:
`{CARGO_PKG_VERSION} ({SONDE_GIT_COMMIT})`.

---
