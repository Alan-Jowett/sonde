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
**Category:** New automated (CLI process test)

**Procedure:**
1. Invoke `sonde-admin --help`.
2. Assert: exit code is 0.
3. Assert: stdout contains all top-level subcommands (`node`, `program`, `schedule`, `reboot`, `ephemeral`, `status`, `state`, `modem`, `pairing`, `handler`).
4. Invoke `sonde-admin node --help`.
5. Assert: exit code is 0.
6. Assert: stdout contains nested subcommands (`list`, `get`, `register`, `remove`, `factory-reset`).
7. Invoke `sonde-admin invalid-subcommand`.
8. Assert: exit code is non-zero and stderr contains a clap-generated error message.

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

### T-0101a  Gateway connection — CLI socket override and error message

**Validates:** ADMIN-0101
**Category:** New automated (CLI process test)

**Procedure:**
1. Invoke `sonde-admin --socket /nonexistent/path node list`.
2. Assert: exit code is non-zero.
3. Assert: stderr contains the endpoint path `/nonexistent/path`.
4. Start an admin server on a known endpoint.
5. Invoke `sonde-admin --socket <endpoint> node list`.
6. Assert: exit code is 0.

---

### T-0101b  Gateway connection — default endpoint and platform retry

**Validates:** ADMIN-0101
**Category:** Structural

**Procedure:**
1. Verify by code inspection that `AdminClient::connect()` uses the
   platform-default endpoint (`/var/run/sonde/admin.sock` on Unix,
   `\\.\pipe\sonde-admin` on Windows) when `--socket` is not specified.
2. On Windows (`#[cfg(windows)]`): verify by code inspection that
   `ERROR_PIPE_BUSY` (OS error 231) triggers a retry loop with 50ms
   intervals for up to 5 seconds before returning a timeout error.
3. On Unix: verify that the connection uses the OS default socket connect
   timeout (no custom retry logic).

---

### T-0103  JSON output format

**Validates:** ADMIN-0102
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node via the test harness.
2. Invoke `sonde-admin --socket <endpoint> --format json node list`.
3. Assert: exit code is 0.
4. Assert: stdout is valid JSON.
5. Assert: JSON contains `node_id` and `key_hint` fields.
6. Invoke `sonde-admin --socket <endpoint> node list`.
7. Assert: stdout contains the node ID in human-readable text (not JSON).

---

### T-0104  Version string contains git SHA

**Validates:** ADMIN-0106
**Category:** Structural

**Procedure:**
1. Run `sonde-admin --version`.
2. Assert: output matches pattern `<version> (<1-7-char-hex-or-unknown>)`.

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

### T-0105a  Timestamp formatting — visible in CLI text output

**Validates:** ADMIN-0107
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server, register a node, and process a WAKE so that
   `last_seen_ms` is populated with a known timestamp.
2. Invoke `sonde-admin --socket <endpoint> node list`.
3. Assert: the text output contains a `YYYY-MM-DD HH:MM:SS UTC` formatted
   timestamp for the node's last seen field.
4. Invoke `sonde-admin --socket <endpoint> --format json node list`.
5. Assert: the JSON output contains a numeric `last_seen_ms` field (not a
   formatted string).

### T-0107  Destructive command confirmation — interactive

**Validates:** ADMIN-0103
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with a registered node.
2. Invoke `sonde-admin node remove <node-id>` with stdin connected to a PTY.
3. Assert: stderr contains `[y/N]:` prompt text.
4. Write `n\n` to stdin.
5. Assert: exit code is non-zero and the node is not removed.
6. Re-invoke `sonde-admin node remove <node-id>` with stdin connected to a PTY.
7. Write `y\n` to stdin.
8. Assert: exit code is 0 and the node is removed.
9. Re-register the node and re-invoke with stdin connected to a PTY.
10. Write `Y\n` to stdin.
11. Assert: exit code is 0 and the node is removed (uppercase accepted).
12. Re-register the node and re-invoke with `--yes`.
13. Assert: exit code is 0 and the node is removed without a prompt.

---

### T-0108  Non-interactive mode refusal

**Validates:** ADMIN-0104
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with a registered node.
2. Invoke `sonde-admin node remove <node-id>` with stdin piped (not a TTY) and without `--yes`.
3. Assert: exit code is non-zero.
4. Assert: stderr contains "non-interactive" or "--yes".
5. Assert: the node is not removed.
6. Re-invoke `sonde-admin --yes node remove <node-id>` with stdin piped (not a TTY).
7. Assert: exit code is 0 and the node is removed.

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

### T-0109a  Verbose node-status output includes hashes alongside filenames

**Validates:** ADMIN-0105, ADMIN-0200, ADMIN-0201, ADMIN-0403
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with node state such that a node's assigned/current
   program hashes reference a stored program record with
   `source_filename = "temp-reader.o"`.
2. Invoke `sonde-admin --verbose node list`.
3. Assert: the node's assigned/current program fields include both
   `temp-reader.o` and the underlying hash.
4. Invoke `sonde-admin --verbose node get <node-id>`.
5. Assert: the assigned/current program fields include both
   `temp-reader.o` and the underlying hash.
6. Invoke `sonde-admin --verbose status <node-id>`.
7. Assert: the current program field includes both `temp-reader.o` and the
   underlying hash.

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

### T-0201a  Human-readable `node list` and `node get` prefer `source_filename` and fall back to hash

**Validates:** ADMIN-0200, ADMIN-0201
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with two nodes whose assigned/current program hashes
   reference stored program records:
   - node A has `source_filename = "temp-reader.o"`.
   - node B has no `source_filename`.
2. Invoke `sonde-admin node list`.
3. Assert: node A's assigned/current program fields show `temp-reader.o`,
   never a full path.
4. Assert: node B's assigned/current program fields show the hash.
5. Invoke `sonde-admin node get <node-a-id>`.
6. Assert: node A's assigned/current program fields show `temp-reader.o`,
   never a full path.

---

### T-0201b  `node list` and `node get` show runtime-only battery

**Validates:** ADMIN-0200, ADMIN-0201
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with a registered node and process a valid WAKE carrying `battery_mv = 3300`.
2. Invoke `sonde-admin node list`.
3. Assert: the node entry includes `3300 mV`.
4. Invoke `sonde-admin node get <node-id>`.
5. Assert: the node detail output includes `3300 mV`.
6. Restart the gateway against the same database without another WAKE.
7. Invoke `sonde-admin node list` and `node get <node-id>` again.
8. Assert: battery is omitted until the node completes another WAKE.

---

### T-0202  Register node — invalid PSK length

**Validates:** ADMIN-0202
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server.
2. Invoke `sonde-admin --socket <endpoint> node register test-node 0x1234 aabbccdd` (16 hex chars, 8 bytes).
3. Assert: exit code is non-zero.
4. Assert: stderr contains "32 bytes" or a clear error about PSK length.
5. Invoke `sonde-admin --socket <endpoint> node register test-node 0x1234 ZZZZ` (invalid hex).
6. Assert: exit code is non-zero and stderr indicates invalid hex input.

---

### T-0200a  List nodes — CLI output format

**Validates:** ADMIN-0200
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with no registered nodes.
2. Invoke `sonde-admin --socket <endpoint> node list`.
3. Assert: exit code is 0.
4. Assert: stdout contains "No nodes registered."
5. Register a node via the test harness.
6. Invoke `sonde-admin --socket <endpoint> node list`.
7. Assert: stdout contains the node ID and key hint.
8. Invoke `sonde-admin --socket <endpoint> --format json node list`.
9. Assert: stdout is valid JSON containing the node ID and key hint.

---

### T-0201c  Get node — CLI output format

**Validates:** ADMIN-0201
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> node get <node-id>`.
3. Assert: exit code is 0.
4. Assert: stdout contains the node ID and key hint.
5. Invoke `sonde-admin --socket <endpoint> node get nonexistent-node`.
6. Assert: exit code is non-zero (gRPC error).
7. Invoke `sonde-admin --socket <endpoint> --format json node get <node-id>`.
8. Assert: stdout is valid JSON containing node details.

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

### T-0203a  Remove node — CLI confirmation and output

**Validates:** ADMIN-0203
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> node remove <node-id>` with stdin
   connected to a PTY.
3. Assert: stderr contains the node ID in the prompt text.
4. Assert: stderr contains `[y/N]:`.
5. Write `y\n` to stdin.
6. Assert: exit code is 0.
7. Assert: stdout contains "Removed node:" and the node ID.
8. Invoke `sonde-admin --socket <endpoint> node list`.
9. Assert: stdout contains "No nodes registered."

---

### T-0204a  Factory reset — CLI confirmation and output

**Validates:** ADMIN-0204
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> node factory-reset <node-id>` with
   stdin piped (not a TTY) and without `--yes`.
3. Assert: exit code is non-zero (non-interactive refusal).
4. Invoke `sonde-admin --socket <endpoint> --yes node factory-reset <node-id>`
   with stdin piped.
5. Assert: exit code is 0.
6. Assert: stdout contains "Factory reset node:" and the node ID.

---

## 5  Program management tests

### T-0300  Ingest and list program

**Validates:** ADMIN-0300, ADMIN-0301
**Category:** Existing automated (`grpc_ingest_list_program`, debug-only)

**Procedure:**
1. Prepare a valid BPF ELF object file (e.g., from `test-programs/`).
2. Call `ingest_program()` with profile `Resident`.
3. Assert: returned hash is non-empty and size is non-zero.
4. Call `list_programs()`.
5. Assert: list contains one program with matching hash.

---

### T-0300a  List program `has_decoder` indicator

**Validates:** ADMIN-0301 (AC-4)
**Category:** New automated (`t0802e_has_decoder_round_trip`, debug-only)

**Procedure:**
1. Ingest a program without a decoder image.
2. Store a program with a decoder image via storage.
3. Call `list_programs()`.
4. Assert: program without decoder has `has_decoder = false`.
5. Assert: program with decoder has `has_decoder = true`.

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

### T-0300b  Ingest program — CLI file-I/O error

**Validates:** ADMIN-0300
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server.
2. Invoke `sonde-admin --socket <endpoint> program ingest /nonexistent/file.o --profile resident`.
3. Assert: exit code is non-zero.
4. Assert: stderr contains an I/O error message referencing the file path.

---

### T-0300c  List programs — CLI output format

**Validates:** ADMIN-0301
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with no programs.
2. Invoke `sonde-admin --socket <endpoint> program list`.
3. Assert: exit code is 0.
4. Assert: stdout contains "No programs stored."
5. Ingest a valid BPF ELF program via the test harness.
6. Invoke `sonde-admin --socket <endpoint> program list`.
7. Assert: stdout contains the program hash, size, and profile.
8. Invoke `sonde-admin --socket <endpoint> --format json program list`.
9. Assert: stdout is valid JSON containing `hash`, `size`, `profile`,
   `source_filename`, and `has_decoder` fields.

---

### T-0301a  Assign program — CLI output

**Validates:** ADMIN-0302
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server, register a node, and ingest a program.
2. Invoke `sonde-admin --socket <endpoint> program assign <node-id> <program-hash>`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Assigned program" and both the hash and node ID.
5. Invoke with an invalid hex hash.
6. Assert: exit code is non-zero and stderr indicates invalid hex input.

---

### T-0302a  Remove program — CLI confirmation and output

**Validates:** ADMIN-0303
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and ingest a program.
2. Invoke `sonde-admin --socket <endpoint> program remove <program-hash>` with
   stdin piped (not a TTY) and without `--yes`.
3. Assert: exit code is non-zero (non-interactive refusal).
4. Invoke `sonde-admin --socket <endpoint> --yes program remove <program-hash>`.
5. Assert: exit code is 0.
6. Assert: stdout contains "Removed program:" and the hash.
7. Invoke `sonde-admin --socket <endpoint> program list`.
8. Assert: stdout contains "No programs stored."

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

### T-0400a  Set schedule — CLI output

**Validates:** ADMIN-0400
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> schedule set <node-id> 120`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Set schedule for" and the node ID and "120".
5. Invoke `sonde-admin --socket <endpoint> --format json schedule set <node-id> 60`.
6. Assert: stdout is valid JSON containing `node_id` and `interval_s`.

---

### T-0401a  Queue reboot — CLI output

**Validates:** ADMIN-0401
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> reboot <node-id>`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Queued reboot for node:" and the node ID.

---

### T-0402  Queue ephemeral

**Validates:** ADMIN-0402
**Category:** New automated

**Procedure:**
1. Register a node and ingest an ephemeral program.
2. Call `queue_ephemeral(node_id, program_hash)`.
3. Assert: call succeeds.

---

### T-0402a  Queue ephemeral — CLI output

**Validates:** ADMIN-0402
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server, register a node, and ingest an ephemeral program.
2. Invoke `sonde-admin --socket <endpoint> ephemeral <node-id> <program-hash>`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Queued ephemeral program" and the node ID.

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

### T-0403a  `status` shows runtime-only battery

**Validates:** ADMIN-0403
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with a registered node and process a valid WAKE carrying `battery_mv = 3300`.
2. Invoke `sonde-admin status <node-id>`.
3. Assert: the text output includes `3300 mV`.
4. Restart the gateway against the same database without another WAKE.
5. Invoke `sonde-admin status <node-id>` again.
6. Assert: battery is omitted until the node completes another WAKE.

---

### T-0403b  Human-readable `status` prefers `source_filename` and falls back to hash

**Validates:** ADMIN-0403
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with two nodes whose current program hashes reference
   stored program records:
   - node A has `source_filename = "temp-reader.o"`.
   - node B has no `source_filename`.
2. Invoke `sonde-admin status <node-a-id>`.
3. Assert: the current program field shows `temp-reader.o`, never a full path.
4. Invoke `sonde-admin status <node-b-id>`.
5. Assert: the current program field shows the hash.
6. Invoke `sonde-admin --format json status <node-a-id>`.
7. Assert: JSON output remains hash-based.

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

### T-0500a  State import — CLI confirmation, file read, and output

**Validates:** ADMIN-0501
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Export state to a temporary file via the client-wrapper test harness.
3. Invoke `sonde-admin --socket <endpoint> state import <file> --passphrase test`
   with stdin piped (not a TTY) and without `--yes`.
4. Assert: exit code is non-zero (non-interactive refusal).
5. Invoke `sonde-admin --socket <endpoint> --yes state import <file> --passphrase test`.
6. Assert: exit code is 0.
7. Assert: stdout contains "Imported state from" and the file path.

---

### T-0500b  State export — CLI output

**Validates:** ADMIN-0500
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> state export <temp-file> --passphrase test`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Exported" and "bytes to" and the file path.
5. Assert: the output file exists and is non-empty.

---

### T-0501a  Passphrase resolution — priority order and TTY fallback

**Validates:** ADMIN-0502
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and register a node.
2. Invoke `sonde-admin --socket <endpoint> state export <file1> --passphrase cli-pass`.
3. Assert: exit code is 0 (CLI argument accepted).
4. Set `SONDE_PASSPHRASE=env-pass` and invoke
   `sonde-admin --socket <endpoint> state export <file2>` without `--passphrase`.
5. Assert: exit code is 0 (env var is used as fallback).
6. Set `SONDE_PASSPHRASE=env-pass` and invoke
   `sonde-admin --socket <endpoint> state export <file3> --passphrase cli-pass`.
7. Assert: exit code is 0.
8. Import `<file3>` with `--passphrase cli-pass`.
9. Assert: import succeeds (proving CLI arg took priority over env var).
10. Unset `SONDE_PASSPHRASE` and invoke with stdin piped (not a TTY) and
    without `--passphrase`.
11. Assert: exit code is non-zero.
12. Assert: stderr indicates that a passphrase is required (no TTY available).

---

## 8  Modem management tests

### T-0600  Modem status — no modem configured

**Validates:** ADMIN-0600
**Category:** New automated

**Procedure:**
1. Call `get_modem_status()` against the default test harness (no modem transport configured).
2. Assert: returns a gRPC `UNAVAILABLE` error with message containing "no modem transport configured".

---

### T-0601  Set modem channel — no modem configured

**Validates:** ADMIN-0601
**Category:** New automated

**Procedure:**
1. Call `set_modem_channel(6)` against the default test harness (no modem transport configured).
2. Assert: returns a gRPC `UNAVAILABLE` error.

---

### T-0602  Modem scan — no modem configured

**Validates:** ADMIN-0602
**Category:** New automated

**Procedure:**
1. Call `scan_modem_channels()` against the default test harness (no modem transport configured).
2. Assert: returns a gRPC `UNAVAILABLE` error.

---

### T-0600a  Modem status — CLI error output

**Validates:** ADMIN-0600
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server without a modem transport.
2. Invoke `sonde-admin --socket <endpoint> modem status`.
3. Assert: exit code is non-zero.
4. Assert: stderr contains an error message about modem unavailability.

---

### T-0601a  Modem set-channel — CLI output and local validation

**Validates:** ADMIN-0601
**Category:** New automated (CLI process test)

**Procedure:**
1. Invoke `sonde-admin modem set-channel 0`.
2. Assert: exit code is non-zero (clap rejects out-of-range locally).
3. Invoke `sonde-admin modem set-channel 15`.
4. Assert: exit code is non-zero (clap rejects out-of-range locally).
5. Start an admin server without a modem transport.
6. Invoke `sonde-admin --socket <endpoint> modem set-channel 6`.
7. Assert: exit code is non-zero (server returns UNAVAILABLE).
8. Assert: stderr contains an error message.

---

### T-0602a  Modem scan — CLI output

**Validates:** ADMIN-0602
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server without a modem transport.
2. Invoke `sonde-admin --socket <endpoint> modem scan`.
3. Assert: exit code is non-zero.
4. Assert: stderr contains an error message about modem unavailability.

---

### T-0603  Modem display — line-count validation

**Validates:** ADMIN-0603
**Category:** New automated (CLI process test)

**Procedure:**
1. Invoke `sonde-admin modem display one two three four five`.
2. Assert: clap rejects the command locally.
3. Assert: stderr indicates that too many positional arguments were supplied.

---

### T-0604  Modem display — RPC mapping and output

**Validates:** ADMIN-0603
**Category:** Structural/manual until a display-capable modem harness is documented

**Procedure:**
1. Perform structural verification of the CLI path in `main.rs` and `grpc_client.rs`.
2. Assert: the `modem display` subcommand accepts between 1 and 4 positional line arguments, forwards the provided lines unchanged to the `ShowModemDisplayMessage` RPC wrapper, and does not block for 60 seconds after the RPC succeeds.
3. Assert: text output reports a 60-second transient display request.
4. Assert: JSON output contains the requested lines and `duration_s = 60`.
5. If a modem-equipped manual setup is available, invoke both `sonde-admin modem display "Device login"` and `sonde-admin modem display "Device login" "Use browser" "Code" "ABCD-EFGH"` and verify that the requested text appears on the modem display and the CLI returns immediately in both cases.

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
**Category:** Structural/manual until a BLE-pairing CLI test harness is documented

**Procedure:**
1. Perform structural verification of the pairing-start CLI path.
2. Assert: the implementation uses the `OpenBlePairing` server-streaming RPC
   and iterates over `BlePairingEvent` variants.
3. Assert: when a `WindowOpened` event is received, the CLI prints
   "BLE pairing window opened".
4. Assert: when a `WindowClosed` event is received, the CLI prints
   "BLE pairing window closed" and exits the event loop.
5. If a manual BLE-capable modem setup is available, invoke
   `sonde-admin pairing start --duration-s 5` against it and verify the same
   user-visible messages end-to-end.

**Note:** Do not implement this as an automated CLI process test until this
document defines a BLE-pairing harness, including where the modem mock lives,
how it is wired into the admin server, and which deterministic open/close
events it emits. Full interactive passkey confirmation testing requires a BLE
peer simulator. Structural verification that the passkey prompt calls
`ConfirmBlePairing` is acceptable as an interim measure.

---

### T-0703a  Pairing start — acceptance criteria coverage

**Validates:** ADMIN-0700
**Category:** Structural

**Procedure:**
This test validates all six acceptance criteria from ADMIN-0700 by structural
code inspection until a BLE-pairing harness is available.

1. **AC-1 (duration):** Assert: the `pairing start` CLI path sends
   `duration_s` from the `--duration-s` flag (default 120) to
   `OpenBlePairingRequest`.
2. **AC-2 (passkey format):** Assert: the `PasskeyDisplay` event handler
   formats the passkey with `{:06}` (6-digit, zero-padded).
3. **AC-3 (prompt on stderr):** Assert: the passkey confirmation prompt
   `Confirm passkey? [y/N]:` is written to stderr, not stdout.
4. **AC-4 (ConfirmBlePairing):** Assert: after reading the user's `y`/`n`
   response, the CLI calls `ConfirmBlePairing` with `confirmed: true` or
   `confirmed: false`.
5. **AC-5 (event printing):** Assert: each `BlePairingEvent` variant
   (`WindowOpened`, `PhoneConnected`, `PhoneDisconnected`, `PasskeyDisplay`,
   `PhoneRegistered`, `WindowClosed`) produces a distinct stdout message.
6. **AC-6 (loop exit):** Assert: the event loop breaks on `WindowClosed`.

---

### T-0700a  List phones — CLI output format

**Validates:** ADMIN-0702
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with no registered phones.
2. Invoke `sonde-admin --socket <endpoint> pairing list-phones`.
3. Assert: exit code is 0.
4. Assert: stdout indicates no phones are registered.
5. Invoke `sonde-admin --socket <endpoint> --format json pairing list-phones`.
6. Assert: stdout is valid JSON containing an empty array.

---

### T-0701a  Pairing stop — CLI confirmation and output

**Validates:** ADMIN-0701
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server.
2. Invoke `sonde-admin --socket <endpoint> pairing stop` with stdin piped
   (not a TTY) and without `--yes`.
3. Assert: exit code is non-zero (non-interactive refusal per ADMIN-0104).
4. Invoke `sonde-admin --socket <endpoint> --yes pairing stop`.
5. Assert: the command completes (exit code 0 or gRPC error are both
   acceptable when no window is open, per T-0701).
6. Assert: `--yes` bypassed the confirmation prompt (no prompt on stderr).

**Note:** This test validates that `pairing stop` is wired as a destructive
command (confirmation required, `--yes` bypass works). Testing the positive
close-an-open-window path requires a BLE-pairing harness and is deferred
until such a harness is defined.

---

### T-0702a  Revoke phone — positive path

**Validates:** ADMIN-0703
**Category:** Structural/manual until a phone registration harness is documented

**Procedure:**
1. Perform structural verification that the `pairing revoke-phone` CLI path:
   a. Calls `confirm()` before executing (destructive command per ADMIN-0103).
   b. Invokes the `RevokePhone` RPC with the provided `phone_id`.
   c. On success, prints "Phone {id} revoked" to stdout (text mode) or
      `{"phone_id": <id>, "status": "revoked"}` (JSON mode).
2. Assert: `--yes` bypasses the confirmation prompt.
3. Assert: the CLI exits with code 0 on success.

**Note:** Automated positive-path testing requires a phone registration
harness (a way to register a phone without real BLE). When such a harness
is available, this test should be promoted to a CLI process test that
registers a phone, revokes it with `--yes`, and asserts on stdout.

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

### T-0800a  Handler add — CLI output

**Validates:** ADMIN-0800
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server.
2. Invoke `sonde-admin --socket <endpoint> handler add "*" echo hello`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Added handler for program" and `*`.
5. Invoke `sonde-admin --socket <endpoint> --format json handler add "aabbccdd..." echo world`.
6. Assert: stdout is valid JSON containing `added` and `program_hash` fields.

---

### T-0800b  Handler list — CLI output format

**Validates:** ADMIN-0802
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server with no handlers.
2. Invoke `sonde-admin --socket <endpoint> handler list`.
3. Assert: exit code is 0.
4. Assert: stdout contains "No handlers configured."
5. Add a handler via the test harness.
6. Invoke `sonde-admin --socket <endpoint> handler list`.
7. Assert: stdout contains the program hash, command, and arguments.
8. Invoke `sonde-admin --socket <endpoint> --format json handler list`.
9. Assert: stdout is valid JSON array containing handler objects.

---

### T-0801a  Handler remove — CLI output

**Validates:** ADMIN-0801
**Category:** New automated (CLI process test)

**Procedure:**
1. Start an admin server and add a handler.
2. Invoke `sonde-admin --socket <endpoint> handler remove "*"`.
3. Assert: exit code is 0.
4. Assert: stdout contains "Removed handler for program" and `*`.

---
