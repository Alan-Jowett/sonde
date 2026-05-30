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
| `--verbose` / `-v` | `bool` | `false` | Show full error diagnostics and extra node-status hash detail |

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
`hex::encode()` whenever a hash is shown.

### 6.4  Node display

The `print_node()` helper displays: node ID, key hint, assigned/current
program identifiers, battery (mV), last seen (formatted), and schedule
interval. For human-readable output, assigned/current program identifiers are
resolved from stored program metadata: use the program's `source_filename`
basename when available, otherwise display the hash. In `--verbose` mode, show
the hash alongside any displayed filename. Battery and last seen come from
runtime node-status data rather than durable node storage, so they are omitted
until the node completes a WAKE in the current gateway process and disappear
again after gateway restart until the next WAKE. Optional fields are omitted
when absent. JSON output remains hash-based.

To preserve the existing hash-based admin API, human-readable node-status
commands resolve filenames client-side. The CLI fetches the node-oriented RPC
response (`ListNodes`, `GetNode`, or `GetNodeStatus`) and, for text output
only, also queries `ListPrograms` to build a hash → `source_filename` map. It
then renders each assigned/current program field as:

1. `source_filename` basename when present in the program map
2. otherwise the hash from the node-oriented RPC response

JSON output does not perform this substitution and continues to serialize the
hash fields returned by the node-oriented RPC response.

### 6.5  Command → RPC → output matrix

| Command | gRPC RPC | Confirmation | JSON fields | Text format |
|---------|----------|-------------|-------------|-------------|
| `node list` | `ListNodes` | — | `[{node_id, key_hint, ...}]` | Per-node detail block (filenames by default; hashes also with `--verbose`) |
| `node get` | `GetNode` | — | `{node_id, key_hint, ...}` | Detail block (filenames by default; hashes also with `--verbose`) |
| `node register` | `RegisterNode` | — | `{node_id}` | "Registered node: {id}" |
| `node remove` | `RemoveNode` | Yes | `{removed}` | "Removed node: {id}" |
| `node factory-reset` | `FactoryReset` | Yes | `{factory_reset}` | "Factory reset node: {id}" |
| `program ingest` | `IngestProgram` | — | `{program_hash, program_size}` | "Ingested program: {hash} ({size} bytes)" |
| `program list` | `ListPrograms` | — | `[{hash, size, profile, source_filename, has_decoder}]` | Per-program line |
| `program assign` | `AssignProgram` | — | `{assigned: true}` | "Assigned program {hash} to node {id}" |
| `program remove` | `RemoveProgram` | Yes | `{removed}` | "Removed program: {hash}" |
| `schedule set` | `SetSchedule` | — | `{node_id, interval_s}` | "Set schedule for {id}: {s}s" |
| `reboot` | `QueueReboot` | — | `{queued, node_id}` | "Queued reboot for node: {id}" |
| `ephemeral` | `QueueEphemeral` | — | `{queued, node_id, program_hash}` | "Queued ephemeral program ..." |
| `status` | `GetNodeStatus` | — | `{node_id, current_program_hash, ...}` | Multi-line status (filename by default; hash also with `--verbose`) |
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

## 6a  Pairing start — interactive event loop

The `pairing start` subcommand differs from all other subcommands because it
uses a **server-streaming RPC** (`OpenBlePairing`) and includes an interactive
confirmation step. The event loop works as follows:

1. **RPC initiation** — the CLI sends an `OpenBlePairingRequest` with the
   requested `duration_s` (default 120) and begins iterating the response
   stream.
2. **Event dispatch** — each `BlePairingEvent` variant maps to a user-visible
   action:

   | Event variant | CLI action |
   |---------------|-----------|
   | `WindowOpened` | Print "BLE pairing window opened for {duration_s}s" to stdout |
   | `Passkey { passkey }` | Print "Passkey: {passkey:06}" to stdout and prompt `Confirm pairing? (y/n):` on stderr |
   | `PhoneConnected { mtu }` | Print "Phone connected (MTU={mtu})" to stdout |
   | `PhoneDisconnected` | Print "Phone disconnected" to stdout |
   | `PhoneRegistered { label, phone_key_hint }` | Print "Phone registered: {label} (key_hint=0x{phone_key_hint:04x})" to stdout |
   | `WindowClosed` | Print "BLE pairing window closed" to stdout; break the event loop |

3. **Passkey confirmation** — when the `Passkey` event is received:
   - The CLI prints the passkey formatted as `{:06}` (six-digit, zero-padded).
   - A confirmation prompt `Confirm pairing? (y/n):` is written to stderr
     (not stdout) to avoid contaminating piped output.
   - The CLI reads a single line from stdin. Only `y` or `Y` is accepted.
   - The CLI calls the `ConfirmBlePairing` unary RPC with the user's
     response (`accept: true` or `accept: false`).
4. **Loop exit** — the event loop terminates when `WindowClosed` is received
   or the stream ends. The CLI exits with code 0 on normal completion.

This subcommand always produces text output because it uses an interactive
stderr prompt for passkey confirmation. Although `--format` is a global flag
accepted by clap, the `pairing start` handler ignores it. The current
implementation does not detect non-TTY stdin; `read_line()` will consume
piped input if available (e.g., `echo y | sonde-admin …` accepts the
pairing). If stdin is at EOF or the pipe provides no data, the read returns
an empty line and the CLI sends `accept: false`.

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

## 11  Key management subcommands

### 11.1  CLI structure

The admin CLI adds a `key` top-level subcommand with three nested operations:

```
sonde-admin key rotate [--gateway-url <url>]
sonde-admin key fingerprint [--gateway-url <url>]
sonde-admin key status [--gateway-url <url>]
```

`key rotate` is interactive. `key fingerprint` and `key status` are read-only.
The optional `--gateway-url` flag selects the target gateway admin endpoint for
these subcommands.

### 11.2  gRPC API usage

All three key-management subcommands begin by calling `GetGatewayState` to read
gateway ACTUAL_STATE from the local gateway admin API.

- `key fingerprint` reads `fingerprint_words` and prints the 6-word BIP-39
  fingerprint.
- `key status` reads and displays `master_key_epoch`, `master_key_id`
  (32-byte SHA-256 hex), and `rotation_in_progress`.
- `key rotate` reads the current gateway ACTUAL_STATE before building the
  rotation payload, then calls `SubmitRotation` to send the serialized
  `RotationPayloadV1` binary blob to the gateway.

After `SubmitRotation`, `key rotate` polls `GetGatewayState` until
`master_key_epoch` increments or the command times out.

### 11.3  Rotation flow

The `key rotate` handler performs the following sequence:

1. Call `GetGatewayState` and extract the gateway fingerprint,
   `x25519_public_key`, and `master_key_epoch`.
2. Display the BIP-39 fingerprint and prompt the user to confirm that it
   matches the modem display before continuing.
3. Prompt for the rotation code on stdin and normalize the input to uppercase
   `[A-Z0-9]` before payload construction.
4. Prompt for the passphrase using masked terminal input. Reject any passphrase
   shorter than 20 characters and fewer than 6 words.
5. Prompt for the deployment label. Reject empty labels.
6. Derive salt = `SHA-256("sonde-kdf-v1:" || utf8(deployment_label))[0..16]`.
7. Derive the new master key with Argon2id v1 (m_cost=65536, t_cost=3,
   p_cost=1, output_len=32) using the passphrase and derived salt.
8. Build `RotationPayloadV1` and send it with `SubmitRotation`.
9. Poll `GetGatewayState` until `master_key_epoch` increments, then report
   success.

### 11.4  `RotationPayloadV1` construction

The CLI constructs the same `RotationPayloadV1` binary envelope used by gateway
DESIRED_STATE rotation delivery.

1. Generate an ephemeral X25519 keypair.
2. Compute the shared secret with the gateway's `x25519_public_key`.
3. Decode the gateway identifier returned by `GetGatewayState` to the raw
   16-byte `gateway_id_raw` required by the rotation envelope.
4. Derive the AES-256-GCM content-encryption key with HKDF-SHA-256 using:
   - `salt = b"sonde-rotation-v1"`
   - `info = gateway_id_raw || current_master_key_epoch_be64`
5. Encode the plaintext as `{1: new_master_key, 2: rotation_code}`. Keys 3–5
   are RESERVED.
6. Encrypt the plaintext with AES-256-GCM and serialize the final
   `RotationPayloadV1` as `version || sender_ephemeral_public || nonce || ciphertext_and_tag`.

### 11.5  Sensitive material handling

All sensitive material used by `key rotate` is kept in `Zeroizing` wrappers,
including the passphrase, Argon2id output, derived symmetric keys, and the new
master key. The CLI must avoid logging these values. `key fingerprint` and
`key status` do not perform key derivation or key-encryption work.

---
