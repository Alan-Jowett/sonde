<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Admin CLI Requirements Specification

> **Document status:** Draft
> **Source:** Extracted from existing `sonde-admin` implementation (issue #749).
> **Scope:** This document covers the `sonde-admin` **CLI tool** only — the thin
> command-line wrapper around the gateway's gRPC admin API. API semantics
> (what each RPC does) are specified in [gateway-requirements.md](gateway-requirements.md)
> §9A (GW-0800–GW-0808). This document specifies CLI-specific behavior:
> argument parsing, output formatting, confirmation prompts, transport
> selection, and error presentation.
> **Related:** [gateway-requirements.md](gateway-requirements.md),
> [gateway-design.md](gateway-design.md) §13,
> [admin-design.md](admin-design.md),
> [admin-validation.md](admin-validation.md)
> **Refines:** GW-0806 (Admin CLI tool)

---

## 1  Definitions

| Term | Definition |
|------|------------|
| **Gateway** | The `sonde-gateway` service exposing the gRPC admin API. |
| **Admin API** | The local gRPC service defined in [gateway-requirements.md](gateway-requirements.md) §9A. |
| **Destructive command** | A CLI command that requires explicit user confirmation before execution because it may irreversibly delete data or overwrite state (e.g., `node remove`, `state import`, `factory-reset`). The complete list is defined in ADMIN-0103. |
| **PSK** | Pre-shared key — a 32-byte AES-256-GCM key used for node authentication. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`ADMIN-XXYY`).
- **Title** — Short name.
- **Description** — What the CLI must do.
- **Acceptance criteria** — Observable, testable conditions.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Gateway requirement or implementation section that motivates this requirement.

---

## 3  General CLI framework

### ADMIN-0100  CLI entry point and subcommand structure

**Priority:** Must
**Source:** GW-0806

**Description:**
The `sonde-admin` binary MUST provide a clap-based CLI with the following
top-level subcommands: `node`, `program`, `schedule`, `reboot`, `ephemeral`,
`status`, `state`, `modem`, `pairing`, `handler`. Each subcommand that has
multiple operations MUST use nested subcommands (e.g., `node list`, `node get`).

**Acceptance criteria:**

1. Running `sonde-admin --help` lists all top-level subcommands.
2. Running `sonde-admin <subcommand> --help` lists nested subcommands where applicable.
3. Unknown subcommands produce a clap-generated error message.

---

### ADMIN-0101  Gateway connection — transport

**Priority:** Must
**Source:** GW-0800

**Description:**
The CLI MUST connect to the gateway admin API over a platform-native local
transport: a Unix domain socket on Linux/macOS, or a named pipe on Windows.
The `--socket` global flag MUST allow overriding the default endpoint. Default
endpoints are `/var/run/sonde/admin.sock` (Unix) and `\\.\pipe\sonde-admin`
(Windows).

On Windows, if the named pipe is busy (error code 231 / `ERROR_PIPE_BUSY`),
the client MUST retry for up to 5 seconds before failing with a timeout error.
On Unix, the connection attempt uses the OS default socket connect timeout.

**Acceptance criteria:**

1. The CLI connects to the default endpoint when `--socket` is not specified.
2. The CLI connects to a custom endpoint when `--socket` is specified.
3. On Windows, a busy named pipe is retried for up to 5 seconds.
4. A connection failure produces a clear error message including the endpoint path.

---

### ADMIN-0102  Output format

**Priority:** Must
**Source:** GW-0806 AC2

**Description:**
The CLI MUST support `--format text` (default) and `--format json` as a global
flag. In text mode, output is human-readable. In JSON mode, output is
`serde_json::to_string_pretty`. The `pairing start` subcommand is exempt
from `--format json` because it is interactive and uses server-streaming RPC
with TTY-based passkey confirmation.

**Acceptance criteria:**

1. All subcommands (except `pairing start`) produce valid JSON when `--format json` is specified.
2. Text mode is the default when `--format` is omitted.
3. JSON output preserves the same semantic information as text output. Fields that are absent in text mode (e.g., null optionals) MAY appear as `null` in JSON.

---

### ADMIN-0103  Destructive action confirmation

**Priority:** Must
**Source:** Implementation (safety guard)

**Description:**
Destructive commands MUST prompt the user for confirmation before executing.
The `--yes` / `-y` global flag MUST skip the prompt (auto-confirm). The
following commands are destructive: `node remove`, `node factory-reset`,
`program remove`, `state import`, `pairing stop`, `pairing revoke-phone`.

Commands that delete easily-recreated configuration without cryptographic
material (e.g., `handler remove`) are intentionally exempt.

**Acceptance criteria:**

1. Destructive commands prompt `[y/N]:` on stderr when `--yes` is not set and stdin is a TTY.
2. Entering anything other than `y` (case-insensitive) aborts the operation.
3. `--yes` bypasses the prompt.

---

### ADMIN-0104  Non-interactive mode detection

**Priority:** Must
**Source:** Implementation (safety guard)

**Description:**
When stdin is not a terminal (e.g., piped input), the CLI MUST refuse to
execute destructive commands unless `--yes` is explicitly provided. This
prevents accidental execution in scripts that forget to pass `--yes`.

**Acceptance criteria:**

1. Without `--yes`, destructive commands fail with an error message when stdin is not a TTY.
2. With `--yes`, destructive commands succeed regardless of TTY status.

---

### ADMIN-0105  Verbose error diagnostics

**Priority:** Should
**Source:** GW-1305 (verifier diagnostics)

**Description:**
The CLI SHOULD support `--verbose` / `-v` as a global flag. When a gRPC error
contains multi-line diagnostics (e.g., Prevail verifier invariants), the
default (non-verbose) output shows only the summary line and first error, plus
a hint to re-run with `--verbose`. In verbose mode, the full error message is
displayed. Single-line error messages are always displayed in full regardless
of `--verbose`.

**Acceptance criteria:**

1. Without `--verbose`, multi-line gRPC errors show summary + first error + hint.
2. With `--verbose`, multi-line gRPC errors show the complete message.
3. Single-line errors display identically in both modes.

---

### ADMIN-0106  Build metadata in version string

**Priority:** Must
**Source:** GW-1303

**Description:**
The CLI MUST display the crate version and a short git commit SHA in its
`--version` output. The commit SHA is injected at build time via `build.rs`.

**Acceptance criteria:**

1. `sonde-admin --version` outputs a string containing the crate version and a short git SHA (up to 7 characters, or `unknown` if git is not available).

---

### ADMIN-0107  Human-readable timestamp formatting

**Priority:** Must
**Source:** GW-0806 AC3

**Description:**
In text output mode, timestamps MUST be formatted as human-readable UTC dates
(`YYYY-MM-DD HH:MM:SS UTC`), not raw milliseconds. In JSON output mode,
timestamps MUST be emitted as numeric `_ms` fields for machine consumption.
Out-of-range timestamps MUST produce an `<invalid timestamp: {value}>` marker
rather than crashing.

**Acceptance criteria:**

1. Text output shows `YYYY-MM-DD HH:MM:SS UTC` for valid timestamps.
2. JSON output retains numeric millisecond fields.
3. Out-of-range values produce the `<invalid timestamp: ...>` marker.

---

## 4  Node management subcommands

### ADMIN-0200  node list

**Priority:** Must
**Source:** GW-0801

**Description:**
`sonde-admin node list` MUST list all registered nodes. Text output shows
node ID, key hint, assigned/current program hashes, battery, last seen when
known, and schedule. An empty registry displays "No nodes registered."

**Acceptance criteria:**

1. Lists all nodes with metadata in text mode.
2. JSON mode returns an array of node objects.
3. Empty registry prints "No nodes registered." in text mode.
4. Optional fields such as battery and last seen are omitted from text output when absent.

---

### ADMIN-0201  node get

**Priority:** Must
**Source:** GW-0801

**Description:**
`sonde-admin node get <node-id>` MUST display details for a single node.

**Acceptance criteria:**

1. Returns node details matching the specified ID.
2. Non-existent node ID returns a gRPC error.

---

### ADMIN-0202  node register

**Priority:** Must
**Source:** GW-0801

**Description:**
`sonde-admin node register <node-id> <key-hint> <psk-hex>` MUST register a
node. The CLI validates that the PSK hex string decodes to exactly 32 bytes
before sending the RPC. `key-hint` is a `u16` (0–65535).

**Acceptance criteria:**

1. A valid registration succeeds and prints the registered node ID.
2. A PSK that is not exactly 64 hex characters (32 bytes) is rejected locally with a clear error message.
3. Invalid hex input is rejected locally.

---

### ADMIN-0203  node remove

**Priority:** Must
**Source:** GW-0801

**Description:**
`sonde-admin node remove <node-id>` MUST remove a node from the registry.
This is a destructive command (ADMIN-0103).

**Acceptance criteria:**

1. Prompts for confirmation (unless `--yes`).
2. On confirmation, removes the node and reports success.

---

### ADMIN-0204  node factory-reset

**Priority:** Must
**Source:** GW-0705

**Description:**
`sonde-admin node factory-reset <node-id>` MUST trigger a factory reset for
the specified node. This is a destructive command (ADMIN-0103).

**Acceptance criteria:**

1. Prompts for confirmation (unless `--yes`).
2. On confirmation, executes the factory reset RPC and reports success.

---

## 5  Program management subcommands

### ADMIN-0300  program ingest

**Priority:** Must
**Source:** GW-0802

**Description:**
`sonde-admin program ingest <file> --profile resident|ephemeral` MUST read a
BPF ELF object file from disk and send it to the gateway for verification and
storage. The CLI extracts the source filename from the file path and sends it
as metadata.

**Acceptance criteria:**

1. A valid ELF file is ingested and the program hash and size are displayed.
2. A missing or unreadable file produces a local I/O error.
3. A file that fails verification produces a gRPC error (see ADMIN-0105 for verbose diagnostics).

---

### ADMIN-0301  program list

**Priority:** Must
**Source:** GW-0802

**Description:**
`sonde-admin program list` MUST list all stored programs. Text output shows
hash, source filename (if available), size, and verification profile. An empty
library displays "No programs stored."

**Acceptance criteria:**

1. Lists programs with metadata.
2. JSON mode returns an array of program objects including `hash`, `size`, `profile`, and `source_filename`.
3. Empty library prints "No programs stored." in text mode.

---

### ADMIN-0302  program assign

**Priority:** Must
**Source:** GW-0802

**Description:**
`sonde-admin program assign <node-id> <program-hash>` MUST assign a program
to a node. The program hash is hex-decoded before sending.

**Acceptance criteria:**

1. Assigns the program and reports success.
2. Invalid hex input is rejected locally.

---

### ADMIN-0303  program remove

**Priority:** Must
**Source:** GW-0802

**Description:**
`sonde-admin program remove <program-hash>` MUST remove a program from the
library. This is a destructive command (ADMIN-0103).

**Acceptance criteria:**

1. Prompts for confirmation (unless `--yes`).
2. On confirmation, removes the program and reports success.

---

## 6  Operational subcommands

### ADMIN-0400  schedule set

**Priority:** Must
**Source:** GW-0803

**Description:**
`sonde-admin schedule set <node-id> <interval-s>` MUST set the wake interval
for a node.

**Acceptance criteria:**

1. Sets the schedule and reports the node ID and interval.

---

### ADMIN-0401  reboot

**Priority:** Must
**Source:** GW-0803

**Description:**
`sonde-admin reboot <node-id>` MUST queue a reboot command for a node.

**Acceptance criteria:**

1. Queues the reboot and reports success.

---

### ADMIN-0402  ephemeral

**Priority:** Must
**Source:** GW-0803

**Description:**
`sonde-admin ephemeral <node-id> <program-hash>` MUST queue an ephemeral
diagnostic program for a node. The program hash is hex-decoded.

**Acceptance criteria:**

1. Queues the ephemeral program and reports success.
2. Invalid hex input is rejected locally.

---

### ADMIN-0403  status

**Priority:** Should
**Source:** GW-0804

**Description:**
`sonde-admin status <node-id>` MUST display the current status of a node
including: node ID, current program hash, battery voltage (mV), firmware ABI
version, runtime last seen timestamp (formatted per ADMIN-0107), and active
session indicator. `last seen` is absent until the node completes a WAKE in
the current gateway process.

**Acceptance criteria:**

1. Displays all status fields.
2. Optional fields (battery, ABI, last seen) are omitted from text output when absent.
3. JSON mode includes all fields with null for absent optional values.

---

## 7  State export/import subcommands

### ADMIN-0500  state export

**Priority:** Should
**Source:** GW-0805

**Description:**
`sonde-admin state export <file> [--passphrase <pass>]` MUST export the
gateway's encrypted state bundle to a file. The passphrase is resolved per
ADMIN-0502.

**Acceptance criteria:**

1. Writes the exported bytes to the specified file.
2. Reports the number of bytes exported.

---

### ADMIN-0501  state import

**Priority:** Should
**Source:** GW-0805

**Description:**
`sonde-admin state import <file> [--passphrase <pass>]` MUST import gateway
state from a previously exported file. This is a destructive command
(ADMIN-0103). The passphrase is resolved per ADMIN-0502.

**Acceptance criteria:**

1. Prompts for confirmation (unless `--yes`).
2. Reads the file and sends it to the gateway.
3. Reports success on completion.

---

### ADMIN-0502  Passphrase resolution

**Priority:** Must
**Source:** GW-0805, GW-0601a

**Description:**
For commands that require a passphrase (`state export`, `state import`), the
CLI MUST resolve the passphrase in the following priority order:

1. `--passphrase` CLI argument (which also reads the `SONDE_PASSPHRASE` environment variable via clap's `env` attribute).
2. Interactive TTY prompt (using `rpassword` for no-echo input).

An empty passphrase MUST be rejected in all cases.

**Acceptance criteria:**

1. `--passphrase <value>` is used when provided.
2. `SONDE_PASSPHRASE` env var is used when the flag is omitted.
3. If neither is available, the user is prompted on the TTY.
4. An empty passphrase is rejected with a clear error message.
5. If no TTY is available and no passphrase is provided, the command fails.

---

## 8  Modem management subcommands

### ADMIN-0600  modem status

**Priority:** Must
**Source:** GW-0807

**Description:**
`sonde-admin modem status` MUST display modem status: radio channel, TX count,
RX count, TX fail count, and uptime in seconds.

**Acceptance criteria:**

1. Displays all status fields in text mode.
2. JSON mode returns an object with all fields.

---

### ADMIN-0601  modem set-channel

**Priority:** Must
**Source:** GW-0807

**Description:**
`sonde-admin modem set-channel <channel>` MUST set the ESP-NOW radio channel.
The channel number MUST be validated locally to the range 1–14 by clap's
`value_parser`.

**Acceptance criteria:**

1. Valid channel (1–14) is accepted and sent to the gateway.
2. Out-of-range channel is rejected locally by clap.

---

### ADMIN-0602  modem scan

**Priority:** Must
**Source:** GW-0807

**Description:**
`sonde-admin modem scan` MUST scan all WiFi channels for AP activity and
display a tabular summary with channel, AP count, and strongest RSSI.

**Acceptance criteria:**

1. Text mode displays a table with headers: Channel, APs, Best RSSI.
2. JSON mode returns an array of per-channel objects.

---

## 9  BLE pairing subcommands

### ADMIN-0700  pairing start

**Priority:** Must
**Source:** GW-1222

**Description:**
`sonde-admin pairing start [--duration-s <seconds>]` MUST open the BLE phone
registration window via the `OpenBlePairing` server-streaming RPC. The default
duration is 120 seconds. The CLI streams events to stdout (window opened,
phone connected/disconnected, passkey display, phone registered, window
closed). When a passkey event is received, the CLI prompts the user to confirm
(`y/n`) and calls `ConfirmBlePairing` with the result.

This command is interactive-only and does not support `--format json`.

**Acceptance criteria:**

1. Opens the BLE pairing window for the specified duration.
2. Displays passkey as a 6-digit zero-padded number.
3. Prompts for passkey confirmation on stderr.
4. Sends the user's confirmation to `ConfirmBlePairing`.
5. Prints each event type as it arrives.
6. Exits the event loop when `WindowClosed` is received.

---

### ADMIN-0701  pairing stop

**Priority:** Must
**Source:** GW-1222

**Description:**
`sonde-admin pairing stop` MUST close the BLE pairing window. This is a
destructive command (ADMIN-0103).

**Acceptance criteria:**

1. Prompts for confirmation (unless `--yes`).
2. Closes the pairing window.

---

### ADMIN-0702  pairing list-phones

**Priority:** Must
**Source:** GW-1222

**Description:**
`sonde-admin pairing list-phones` MUST list all registered phones with their
ID, key hint, label, status, and issue timestamp (formatted per ADMIN-0107).

**Acceptance criteria:**

1. Text mode displays a tabular listing with headers.
2. JSON mode returns an array of phone objects.

---

### ADMIN-0703  pairing revoke-phone

**Priority:** Must
**Source:** GW-1222

**Description:**
`sonde-admin pairing revoke-phone <phone-id>` MUST revoke a phone's PSK.
This is a destructive command (ADMIN-0103).

**Acceptance criteria:**

1. Prompts for confirmation (unless `--yes`).
2. Revokes the phone and reports success.

---

## 10  Handler management subcommands

### ADMIN-0800  handler add

**Priority:** Must
**Source:** GW-1403

**Description:**
`sonde-admin handler add <program-hash> <command> [args...] [--working-dir <path>] [--reply-timeout-ms <ms>]`
MUST register a handler for a program hash. The `program-hash` argument
accepts either a 64-character hex hash or `*` for the catch-all handler.
Trailing arguments after `<command>` are passed as the handler's arguments.

**Acceptance criteria:**

1. Registers a handler and reports success.
2. `*` is accepted as the catch-all program hash.
3. `--working-dir` and `--reply-timeout-ms` are optional.

---

### ADMIN-0801  handler remove

**Priority:** Must
**Source:** GW-1403

**Description:**
`sonde-admin handler remove <program-hash>` MUST remove a handler by program
hash (or `*` for catch-all).

**Acceptance criteria:**

1. Removes the handler and reports success.

---

### ADMIN-0802  handler list

**Priority:** Must
**Source:** GW-1403

**Description:**
`sonde-admin handler list` MUST list all configured handlers showing program
hash, command, arguments, and working directory.

**Acceptance criteria:**

1. Text mode shows `hash → command args (cwd=path)` format.
2. JSON mode returns an array of handler objects.
3. Empty list prints "No handlers configured." in text mode.

---
