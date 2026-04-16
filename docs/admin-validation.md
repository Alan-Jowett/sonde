<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Admin CLI Validation Specification

> **Document status:** Draft
> **Scope:** Test plan for the `sonde-admin` CLI tool.
> **Audience:** Implementers (human or LLM agent) writing admin CLI tests.
> **Related:** [admin-requirements.md](admin-requirements.md),
> [admin-design.md](admin-design.md),
> [gateway-validation.md](gateway-validation.md)

---

## 1  Overview

This document defines test cases that validate the `sonde-admin` CLI against
the requirements in [admin-requirements.md](admin-requirements.md). Each test
case is traceable to one or more requirements.

**Scope:** These tests cover the CLI layer — argument parsing, output
formatting, confirmation prompts, transport selection, and error presentation.
The underlying gRPC API semantics are validated in
[gateway-validation.md](gateway-validation.md).

**Test categories:**

- **Existing automated** — tests already implemented in `crates/sonde-admin/tests/integration.rs` or `crates/sonde-admin/src/lib.rs`.
- **New automated** — tests to be implemented.
- **Structural** — verified by code inspection or build-time checks.

**Test layers:**

Tests are organized in two layers:

1. **Client-wrapper tests** — exercise the `AdminClient` typed RPC wrappers
   against a real `AdminService`. These validate that the client correctly
   calls the gRPC API and interprets responses. Most existing tests are in
   this layer.
2. **CLI process tests** — invoke the `sonde-admin` binary via
   `std::process::Command` (or `assert_cmd`) and assert on stdout, stderr,
   and exit codes. These validate argument parsing, output formatting,
   confirmation prompts, and error presentation. Tests in this layer are
   marked accordingly.

---

## 2  Test environment

### 2.1  Integration test harness

Tests spin up a real `AdminService` backed by `InMemoryStorage` on a
platform-native transport (Unix domain socket on Linux, named pipe on
Windows). An `AdminClient` connects to the server. This harness already
exists in `crates/sonde-admin/tests/integration.rs`.

Each test uses a unique endpoint name (incorporating the test name and PID)
to avoid collisions when tests run in parallel.

### 2.2  Test helpers

- `unique_endpoint(test_name)` — generates a unique socket/pipe path.
- `start_server_and_connect(test_name)` — starts the admin server in a background task, retries connection for up to 5 seconds.

---

## 3  General CLI framework tests

### T-0100  Subcommand help output

**Validates:** ADMIN-0100
**Category:** New automated

**Procedure:**
1. Run `sonde-admin --help`.
2. Assert: output contains all top-level subcommands (`node`, `program`, `schedule`, `reboot`, `ephemeral`, `status`, `state`, `modem`, `pairing`, `handler`).
3. Run `sonde-admin node --help`.
4. Assert: output contains nested subcommands (`list`, `get`, `register`, `remove`, `factory-reset`).

---

### T-0101  Gateway connection — default transport

**Validates:** ADMIN-0101
**Category:** Existing automated (partially — `start_server_and_connect` validates platform transport)

**Procedure:**
1. Start an admin server on the platform default transport.
2. Connect `AdminClient` using the same endpoint.
3. Assert: connection succeeds.
4. Call `list_nodes()`.
5. Assert: returns an empty list (no error).

---

### T-0102  Gateway connection — failure

**Validates:** ADMIN-0101
**Category:** New automated

**Procedure:**
1. Attempt to connect to a non-existent endpoint.
2. Assert: connection fails with an error.

---

### T-0103  JSON output format

**Validates:** ADMIN-0102
**Category:** New automated

**Procedure:**
1. Register a node via the test harness.
2. Call `node list` with `--format json`.
3. Assert: output is valid JSON.
4. Assert: JSON contains `node_id` and `key_hint` fields.

---

### T-0104  Version string contains git SHA

**Validates:** ADMIN-0106
**Category:** Structural

**Procedure:**
1. Run `sonde-admin --version`.
2. Assert: output matches pattern `<version> (<7-char-hex-or-unknown>)`.

---

### T-0105  Timestamp formatting — valid

**Validates:** ADMIN-0107
**Category:** Existing automated (`test_format_known_timestamp`, `test_format_epoch_zero`)

**Procedure:**
1. Call `format_epoch_ms(1_774_670_595_000)`.
2. Assert: returns `"2026-03-28 04:03:15 UTC"`.
3. Call `format_epoch_ms(0)`.
4. Assert: returns `"1970-01-01 00:00:00 UTC"`.

---

### T-0106  Timestamp formatting — out of range

**Validates:** ADMIN-0107
**Category:** Existing automated (`test_format_out_of_range`)

**Procedure:**
1. Call `format_epoch_ms(u64::MAX)`.
2. Assert: returns `"<invalid timestamp: {u64::MAX}>"`.

---

### T-0107  Destructive command confirmation — interactive

**Validates:** ADMIN-0103
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with a registered node.
2. Invoke `sonde-admin node remove <node-id>` with stdin connected to a PTY.
3. Write `n\n` to stdin.
4. Assert: exit code is non-zero and the node is not removed.
5. Re-invoke with `--yes`.
6. Assert: exit code is 0 and the node is removed.

---

### T-0108  Non-interactive mode refusal

**Validates:** ADMIN-0104
**Category:** New automated (CLI process test)

**Procedure:**
1. Invoke `sonde-admin node remove <node-id>` with stdin piped (not a TTY) and without `--yes`.
2. Assert: exit code is non-zero.
3. Assert: stderr contains "non-interactive" or "--yes".

---

### T-0109  Verbose error diagnostics

**Validates:** ADMIN-0105
**Category:** New automated (CLI process test)

**Procedure:**
1. Ingest an invalid BPF program that triggers multi-line verifier diagnostics.
2. Invoke `sonde-admin program ingest <file> --profile resident` without `--verbose`.
3. Assert: stderr shows summary line + hint "run with --verbose".
4. Re-invoke with `--verbose`.
5. Assert: stderr shows the full multi-line error.
---

## 4  Node management tests

### T-0200  List nodes — empty

**Validates:** ADMIN-0200
**Category:** Existing automated (`grpc_list_nodes_empty`)

**Procedure:**
1. Connect to a fresh gateway.
2. Call `list_nodes()`.
3. Assert: returns an empty list.

---

### T-0201  Register, list, and get node

**Validates:** ADMIN-0200, ADMIN-0201, ADMIN-0202
**Category:** Existing automated (`grpc_register_list_get_node`)

**Procedure:**
1. Register a node with ID `"test-node"`, key_hint `0x1234`, PSK `[0xAA; 32]`.
2. Assert: returned node ID is `"test-node"`.
3. Call `list_nodes()`.
4. Assert: list contains one node with matching ID and key_hint.
5. Call `get_node("test-node")`.
6. Assert: returned node matches.

---

### T-0202  Register node — invalid PSK length

**Validates:** ADMIN-0202
**Category:** New automated

**Procedure:**
1. Attempt to register a node with a 16-byte PSK (32 hex chars).
2. Assert: CLI rejects with an error message containing "32 bytes".

---

### T-0203  Remove node

**Validates:** ADMIN-0203
**Category:** Existing automated (`grpc_register_remove_node`)

**Procedure:**
1. Register a node.
2. Assert: `list_nodes()` returns 1 node.
3. Remove the node.
4. Assert: `list_nodes()` returns 0 nodes.

---

### T-0204  Factory reset node

**Validates:** ADMIN-0204
**Category:** New automated

**Procedure:**
1. Register a node.
2. Call `factory_reset("node-id")`.
3. Assert: call succeeds.
4. Assert: node is no longer in the registry.

---

## 5  Program management tests

### T-0300  Ingest and list program

**Validates:** ADMIN-0300, ADMIN-0301
**Category:** Existing automated (`grpc_ingest_list_program`, debug-only)

**Procedure:**
1. Build a minimal CBOR program image (bytecode + empty maps).
2. Call `ingest_program()` with profile `Resident`.
3. Assert: returned hash is non-empty and size is non-zero.
4. Call `list_programs()`.
5. Assert: list contains one program with matching hash.

---

### T-0301  Assign program to node

**Validates:** ADMIN-0302
**Category:** New automated

**Procedure:**
1. Register a node and ingest a program.
2. Call `assign_program(node_id, program_hash)`.
3. Assert: call succeeds.

---

### T-0302  Remove program

**Validates:** ADMIN-0303
**Category:** New automated

**Procedure:**
1. Ingest a program.
2. Call `remove_program(program_hash)`.
3. Assert: call succeeds.
4. Assert: `list_programs()` returns empty.

---

## 6  Operational subcommand tests

### T-0400  Set schedule

**Validates:** ADMIN-0400
**Category:** Existing automated (`grpc_set_schedule`)

**Procedure:**
1. Register a node.
2. Call `set_schedule(node_id, 120)`.
3. Assert: call succeeds.

---

### T-0401  Queue reboot

**Validates:** ADMIN-0401
**Category:** Existing automated (`grpc_queue_reboot`)

**Procedure:**
1. Register a node.
2. Call `queue_reboot(node_id)`.
3. Assert: call succeeds.

---

### T-0402  Queue ephemeral

**Validates:** ADMIN-0402
**Category:** New automated

**Procedure:**
1. Register a node and ingest an ephemeral program.
2. Call `queue_ephemeral(node_id, program_hash)`.
3. Assert: call succeeds.

---

### T-0403  Get node status

**Validates:** ADMIN-0403
**Category:** New automated

**Procedure:**
1. Register a node.
2. Call `get_node_status(node_id)`.
3. Assert: returned status contains the node ID.
4. Assert: `has_active_session` is `false` (no WAKE has occurred).

---

## 7  State export/import tests

### T-0500  Export and import state

**Validates:** ADMIN-0500, ADMIN-0501
**Category:** Existing automated (`grpc_export_import_state`)

**Procedure:**
1. Register a node.
2. Call `export_state("test-passphrase")`.
3. Assert: returned data is non-empty.
4. Call `import_state(data, "test-passphrase")`.
5. Assert: call succeeds.
6. Assert: `list_nodes()` still contains the original node.

---

### T-0501  Passphrase — empty rejection

**Validates:** ADMIN-0502
**Category:** New automated

**Procedure:**
1. Call `resolve_passphrase` with `Some("")`.
2. Assert: returns an error containing "must not be empty".

---

## 8  Modem management tests

### T-0600  Modem status

**Validates:** ADMIN-0600
**Category:** New automated

**Procedure:**
1. Call `get_modem_status()`.
2. Assert: returns a `ModemStatus` with default values (no modem attached in test harness — gateway may return an error or default status depending on implementation).

---

### T-0601  Set modem channel — valid

**Validates:** ADMIN-0601
**Category:** New automated

**Procedure:**
1. Call `set_modem_channel(6)`.
2. Assert: call succeeds or returns a modem-not-connected error (acceptable in test harness without physical modem).

---

### T-0602  Modem scan

**Validates:** ADMIN-0602
**Category:** New automated

**Procedure:**
1. Call `scan_modem_channels()`.
2. Assert: returns a list (possibly empty if no modem is attached).

---

## 9  BLE pairing tests

### T-0700  List phones — empty

**Validates:** ADMIN-0702
**Category:** Existing automated (`grpc_list_phones_empty`)

**Procedure:**
1. Connect to a fresh gateway.
2. Call `list_phones()`.
3. Assert: returns an empty list.

---

### T-0701  Close pairing when not open

**Validates:** ADMIN-0701
**Category:** Existing automated (`grpc_close_ble_pairing_when_not_open`)

**Procedure:**
1. Call `close_ble_pairing()` without opening a pairing window.
2. Assert: call does not panic (may succeed or return an error).

---

### T-0702  Revoke non-existent phone

**Validates:** ADMIN-0703
**Category:** Existing automated (`grpc_revoke_nonexistent_phone`)

**Procedure:**
1. Call `revoke_phone(999)`.
2. Assert: returns an error.

---

### T-0703  Pairing start — event streaming

**Validates:** ADMIN-0700
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with a modem mock that supports BLE pairing.
2. Invoke `sonde-admin pairing start --duration-s 5`.
3. Assert: stdout contains "BLE pairing window opened".
4. Wait for the window to close.
5. Assert: stdout contains "BLE pairing window closed".
6. Assert: exit code is 0.

**Note:** Full interactive passkey confirmation testing requires a BLE peer
simulator. Structural verification that the passkey prompt calls
`ConfirmBlePairing` is acceptable as an interim measure.

---

## 10  Handler management tests

### T-0800  Add and list handler

**Validates:** ADMIN-0800, ADMIN-0802
**Category:** New automated

**Procedure:**
1. Call `add_handler("*", "echo", vec!["hello"], None, None)`.
2. Assert: call succeeds.
3. Call `list_handlers()`.
4. Assert: list contains one handler with `program_hash = "*"` and `command = "echo"`.

---

### T-0801  Remove handler

**Validates:** ADMIN-0801
**Category:** New automated

**Procedure:**
1. Add a handler.
2. Call `remove_handler("*")`.
3. Assert: call succeeds.
4. Assert: `list_handlers()` returns empty.

---
