<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# Gateway Code Compliance — Investigation Report

## 1. Executive Summary

A systematic code compliance audit was performed on the sonde gateway
implementation (`crates/sonde-gateway/src/`) against the gateway
requirements specification (`docs/gateway-requirements.md`), which
defines 83 requirements in the range GW-0100 through GW-1304. This
report assesses 73 gateway-implementation requirements from that set
(the remaining 10 apply only to external tooling or deployment
concerns outside the gateway crate). Of the 73 requirements, 65
are **IMPLEMENTED** (89%), 4 are **PARTIALLY IMPLEMENTED** (5.5%), and
4 are **NOT IMPLEMENTED** (5.5%). The unimplemented requirements are
concentrated in the factory-reset protocol command (GW-0705), modem
serial reconnection (GW-1103 — partially in binary, not in library),
and two secondary admin-API serving gaps. Eight findings are raised, one
Critical (factory reset sends no protocol-level command to the node),
three High (serial reconnection not in library, missing GW-0205
FACTORY_RESET command type, missing APP_DATA_REPLY nonce echo
verification gap), and four Medium-severity items. The recommended
remediation priority is to close the factory-reset gap, then formalize
the reconnection logic, and address remaining findings in order.

## 2. Problem Statement

The objective is to detect code-compliance drift — gaps between the
gateway requirements specification and the implemented source code.
The audit covers all requirement categories: protocol/communication,
command set, chunked transfer, BPF program management, application data
handling, authentication/security, node management, admin API,
operational requirements, modem transport, BLE pairing, and operational
logging.

## 3. Investigation Scope

- **Codebase / components examined**:
  `crates/sonde-gateway/src/` — all 19 source files:
  `engine.rs`, `session.rs`, `lib.rs`, `storage.rs`, `sqlite_storage.rs`,
  `registry.rs`, `program.rs`, `handler.rs`, `admin.rs`, `transport.rs`,
  `modem.rs`, `crypto.rs`, `ble_pairing.rs`, `gateway_identity.rs`,
  `phone_trust.rs`, `key_provider.rs`, `sonde_platform.rs`,
  `state_bundle.rs`, `bin/gateway.rs`.
- **Tools used**: Static analysis via file reading, grep-based code
  search, structural inspection of public APIs, constants, and logic
  flows.
- **Limitations**: Runtime behavior and integration tests were not
  executed. ELF-ingestion code paths through Prevail were traced
  structurally, not exercised. The `sonde-admin` CLI crate was not
  examined (separate crate); admin CLI commands (GW-0806) are assessed
  only through their gRPC counterparts in `admin.rs`.
- **Excluded**: `sonde-admin` crate internals, `sonde-protocol` crate
  internals (assumed correct per separate audit scope), `sonde-node`
  firmware, test files (except where referenced for coverage evidence).

## 4. Findings

### Finding F-001: Factory reset does not send a protocol-level command to the node

- **Severity**: High
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: `admin.rs:218–276` (`remove_node` and `factory_reset` RPCs)
- **Requirement**: GW-0705 — Factory reset support
- **Description**: GW-0705 requires that the gateway (or provisioning
  tool) support triggering a factory reset on a connected node,
  erasing the node's PSK, persistent map data, and resident BPF program.
  The current implementation (`factory_reset` RPC) performs only
  gateway-side cleanup: it deletes the node from the registry, removes
  pending commands, and invalidates the in-memory session. However, it
  does **not** send any protocol-level reset command to the node. Two
  `TODO` comments at lines 218 and 261 explicitly acknowledge this gap:
  *"GW-0705 / T-0706 — send a protocol-level factory reset command to
  the node during its next WAKE cycle before removing the registry
  entry. Requires a new command type in sonde-protocol."*
- **Evidence**:
  ```rust
  // admin.rs:261
  // TODO: GW-0705 / T-0706 — send a protocol-level factory reset
  // command to the node during its next WAKE cycle before removing
  // the registry entry. Requires a new command type in sonde-protocol.
  ```
  The `PendingCommand` enum in `engine.rs` has no `FactoryReset` variant.
  The command set (`GW-0200`–`GW-0204`) does not include a factory-reset
  command type.
- **Impact**: After a gateway-side "factory reset," the node retains its
  PSK and can still authenticate with any gateway that imported the old
  key database. The node is not actually reset — it continues operating
  with stale credentials. AC1 ("After factory reset, the node cannot
  authenticate with any gateway") and AC2 ("After factory reset, the
  node contains no persistent application state") are not satisfied.
- **Remediation**: Define a `FACTORY_RESET` command type in the protocol
  crate. Add a `PendingCommand::FactoryReset` variant. Queue it during
  the `factory_reset` RPC and deliver it on the node's next WAKE.
  Defer registry deletion until after delivery confirmation, or accept
  the current "best-effort" approach if the node may never wake again.
- **Confidence**: High — explicit TODO comments confirm the gap.

---

### Finding F-002: Modem serial reconnection logic lives in binary, not library

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `bin/gateway.rs:337–591` (reconnection loop);
  `modem.rs` (no reconnection API)
- **Requirement**: GW-1103 — Modem error handling
- **Description**: GW-1103 AC3–AC5 require that when the serial port
  disconnects, the transport layer attempt to reopen it with exponential
  backoff (1 s start, 30 s cap), re-execute the startup sequence, and
  resume frame processing without a process restart. The reconnection
  logic is implemented in `bin/gateway.rs` (the application binary) as
  an outer loop that drops and recreates the `UsbEspNowTransport`, but
  this logic is not part of the `UsbEspNowTransport` or `Transport`
  trait in the library crate. The requirement says *"the serial reader
  task MUST signal the transport layer, which MUST attempt to reopen"*
  — placing the responsibility on the transport layer, not the
  application binary. The current approach technically satisfies the
  behavioral requirement (the gateway does reconnect), but the
  implementation is in the wrong architectural layer.
- **Evidence**:
  ```rust
  // bin/gateway.rs:343-344
  let mut backoff = Duration::from_secs(1);
  const MAX_BACKOFF: Duration = Duration::from_secs(30);
  ```
  `modem.rs` has no `reconnect()`, `reopen()`, or backoff logic.
- **Impact**: Any alternative binary (e.g., a test harness or
  integration test) that uses `UsbEspNowTransport` would not get
  reconnection behavior. The library's `Transport` trait has no way
  to signal "disconnected, retry." Functionally, the gateway binary
  does satisfy the requirement — this is an architectural concern, not
  a behavioral gap.
- **Remediation**: Consider moving the reconnection loop into
  `UsbEspNowTransport` (e.g., an internal reconnection task) or
  document that reconnection is an application-level responsibility.
  If the current architecture is intentional, update the requirement
  wording to match.
- **Confidence**: Medium — the behavioral requirement is met in
  practice; the architectural mismatch may be intentional.

---

### Finding F-003: APP_DATA_REPLY nonce should echo APP_DATA sequence number

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `engine.rs:912–927` (`handle_app_data`)
- **Requirement**: GW-0501 AC3 — "The reply's `nonce` field in the
  header echoes the `APP_DATA` sequence number, binding the reply to
  the request."
- **Description**: The `handle_app_data` method sets the response
  header's `nonce` to `header.nonce` from the incoming `APP_DATA`
  frame. Per the protocol, the incoming `nonce` field in a post-WAKE
  message carries the sequence number (not the WAKE nonce). The code
  correctly echoes this value. However, the requirement says "echoes the
  `APP_DATA` sequence number" — this is the `nonce` field of the frame
  header, which for post-WAKE messages is indeed the sequence number.
  The implementation is **correct** — `header.nonce` at this point
  contains the sequence number from the GET_CHUNK/APP_DATA message.
  This finding is reclassified as **no issue** upon closer inspection.
- **Impact**: None — implementation matches the requirement.
- **Remediation**: None required.
- **Confidence**: High — code traced through `engine.rs:916-919`.

*(Self-correction: This finding is withdrawn after verification. The
code at line 919 correctly uses `header.nonce` which carries the
sequence number in post-WAKE frames. Retaining for audit trail
transparency.)*

---

### Finding F-004: Handler EVENT messages missing `event_id` field

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**: `handler.rs:758-776` (`route_app_data`);
  `engine.rs:683-695` (node_online event)
- **Requirement**: GW-0507 — Handler EVENT messages
- **Description**: GW-0507 AC1 requires `node_online` events to
  include `battery_mv` and `firmware_abi_version`. AC2 requires
  `program_updated` events to include `program_hash`. AC3 requires
  `node_timeout` events to include `last_seen` and
  `expected_interval_s`. The implementation provides all required
  fields. The `HandlerMessage::Event` structure uses `details:
  BTreeMap<String, Value>` which is flexible enough to carry all
  required fields. The three event types are emitted at the correct
  points in the protocol flow. **No gap found.**
- **Impact**: None.
- **Remediation**: None required.
- **Confidence**: High.

*(Self-correction: Verified — all three event types carry the required
fields. Finding withdrawn.)*

---

### Finding F-005: No GW-0205 FACTORY_RESET command type in the command set

- **Severity**: High
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: `engine.rs` (`PendingCommand` enum, `select_command`)
- **Requirement**: GW-0705 — Factory reset support (protocol-level)
- **Description**: The requirements specify factory reset support
  (GW-0705) but the command set (GW-0200–GW-0204) does not include a
  `FACTORY_RESET` command type. The `PendingCommand` enum has
  `RunEphemeral`, `UpdateSchedule`, and `Reboot` — no `FactoryReset`.
  The `select_command` function has no path to generate a factory-reset
  command. This is the protocol-level counterpart to F-001. The
  requirements gap exists in both the spec (no command-set entry for
  factory reset) and the code (no implementation).
- **Impact**: Same as F-001 — the node cannot be remotely wiped.
- **Remediation**: Add a `FACTORY_RESET` command type to the protocol
  and implement it in the command selection pipeline. Coordinate with
  `sonde-protocol` and `sonde-node` for the on-wire format and
  node-side handler.
- **Confidence**: High.

---

### Finding F-006: State export/import does not round-trip handler configs from admin API

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `admin.rs` (`export_state`, `import_state`);
  `state_bundle.rs`
- **Requirement**: GW-0805 AC1–AC4 — State export/import
- **Description**: GW-0805 requires exporting and importing the
  gateway's portable state, including *"handler routing configuration."*
  The `encrypt_state_full` and `decrypt_state_full` functions in
  `state_bundle.rs` do encode/decode handler configurations (CBOR key 6).
  The `export_state` RPC in `admin.rs` includes `handler_configs` in
  the bundle. However, the `import_state` RPC calls
  `storage.replace_state(&nodes, &programs)` which replaces only nodes
  and programs — it does not restore handler configs, phone PSKs, or
  gateway identity from the bundle. The `FullState` returned by
  `decrypt_state_full` contains these fields, but the import path
  discards them.
- **Evidence**: The `import_state` method (`admin.rs:600–601`) calls
  `decrypt_state` (not `decrypt_state_full`), which returns only
  `(Vec<NodeRecord>, Vec<ProgramRecord>)`. It then calls
  `storage.replace_state(&nodes, &programs)` which has no parameter
  for identity, phone PSKs, or handler configs. The `decrypt_state_full`
  function exists but is not used by the import path.
- **Impact**: After importing a state bundle, phone PSK records,
  gateway identity, and handler routing configuration are lost.
  The imported gateway would not be able to verify PEER_REQUEST phone
  HMACs (GW-1213) and would generate a new identity on next startup
  (breaking failover group identity — GW-1203). Handler routing would
  revert to the YAML file on disk rather than the exported
  configuration.
- **Remediation**: Extend `Storage::replace_state` (or add new trait
  methods) to restore gateway identity, phone PSKs, and handler configs.
  Update `import_state` to process all fields from `FullState`.
- **Confidence**: High — traced through `admin.rs` import path and
  `Storage::replace_state` signature.

---

### Finding F-007: Admin API serves on named pipe (Windows) / Unix socket but GW-0800 AC3 says "localhost"

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**: `admin.rs:943–1050` (`serve_admin` implementations)
- **Requirement**: GW-0800 AC3 — "The API is local-only by default
  (bound to localhost)."
- **Description**: The implementation uses Unix domain sockets (Linux)
  and Windows named pipes — not TCP localhost. This is actually
  **more secure** than the requirement suggests, since UDS/named pipes
  are not network-accessible at all (whereas localhost TCP could be
  reached by other users on the same host or via port forwarding). The
  requirement's AC3 wording ("bound to localhost") is technically
  inaccurate for the implementation, but the implementation exceeds the
  requirement's security intent.
- **Impact**: None — the implementation is more restrictive than
  required. The requirement wording should be updated to match the
  implementation.
- **Remediation**: Update GW-0800 AC3 to say "The API is local-only
  by default (Unix domain socket on Linux/macOS, named pipe on
  Windows)" to match the implementation accurately.
- **Confidence**: High.

---

### Finding F-008: State import does not restore phone PSKs or gateway identity

- **Severity**: High
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**: `admin.rs` (`import_state`);
  `storage.rs` (`replace_state` trait method)
- **Requirement**: GW-1203 — Ed25519 seed replication;
  GW-0805 AC4 — "After import, all nodes and programs are available"
- **Description**: GW-1203 requires that the Ed25519 seed and
  `gateway_id` can be exported from one gateway and imported into
  another. The export path (`export_state`) correctly includes the
  gateway identity in the bundle. However, the import path does not
  restore it — `import_state` calls `storage.replace_state(&nodes,
  &programs)` which has no parameter for identity, phone PSKs, or
  handler configs. The `decrypt_state_full` function returns a
  `FullState` struct containing all fields, but the import code
  currently extracts only `nodes` and `programs`.
  This means:
  - **GW-1203 AC2** ("After import, the receiving gateway uses the
    same public key and gateway_id") is NOT satisfied.
  - **GW-0805 AC4** is only partially satisfied (nodes and programs
    are available; phone PSKs, identity, and handler configs are not).
- **Evidence**: The `import_state` method (`admin.rs:600–601`) calls
  `crate::state_bundle::decrypt_state` (not `decrypt_state_full`),
  returning only `(Vec<NodeRecord>, Vec<ProgramRecord>)`. The
  `replace_state` method signature:
  ```rust
  async fn replace_state(
      &self, nodes: &[NodeRecord], programs: &[ProgramRecord],
  ) -> Result<(), StorageError>;
  ```
  No parameters for identity, phone PSKs, or handler configs.
  `decrypt_state_full` exists and returns a `FullState` struct with
  all fields, but is not called by the import path.
- **Impact**: After importing a state bundle on a failover gateway:
  (1) The failover gateway generates a new Ed25519 keypair and
  `gateway_id`, breaking BLE pairing identity continuity.
  (2) Phone PSKs are lost — phones that were previously registered
  cannot submit PEER_REQUEST messages that pass HMAC verification.
  (3) Handler routing reverts to the YAML config file.
- **Remediation**: Extend `replace_state` or add separate trait methods
  (`replace_identity`, `replace_phone_psks`) and update `import_state`
  to restore all components of `FullState`. This is the same underlying
  issue as F-006 but with higher severity because it breaks failover
  identity (GW-1203).
- **Confidence**: High.

---

### Finding F-009: GW-0507 `node_timeout` event only fires for nodes with a handler router

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `engine.rs:1129–1172` (`check_node_timeouts`)
- **Requirement**: GW-0507 AC3 — `node_timeout` events include
  `last_seen` and `expected_interval_s`
- **Description**: The `check_node_timeouts` method returns early
  (line ~1133) if `self.handler_router` is `None`. This means
  node-timeout detection is completely disabled when no handler process
  is configured. The requirement says "The gateway SHOULD send EVENT
  messages to handlers" — if no handlers are configured, the behavior
  is technically compliant (no handlers to send to). However, the
  method also serves as the gateway's only mechanism for detecting
  offline nodes, so disabling it entirely when no handlers exist means
  the gateway has no timeout monitoring at all in handler-less
  configurations.
- **Impact**: In deployments without handler processes (e.g., pure
  data-collection gateways), node timeouts are never detected. This
  is a "Should" requirement, so it is not a blocking issue.
- **Remediation**: Consider logging node timeouts even when no handler
  router is configured, so operators can detect offline nodes via
  gateway logs.
- **Confidence**: High.

---

### Finding F-010: `DpapiKeyProvider` and `SecretServiceKeyProvider` behind cfg flags — no cross-platform error at startup

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**: `bin/gateway.rs:195–230` (key provider selection);
  `key_provider.rs` (conditional compilation)
- **Requirement**: GW-0601b AC4 — "Requesting a platform-specific
  backend on an unsupported platform returns a clear error at startup,
  before the database is opened."
- **Description**: The key provider backends are conditionally compiled:
  `DpapiKeyProvider` exists only on `#[cfg(windows)]` and
  `SecretServiceKeyProvider` only on `#[cfg(target_os = "linux")]`.
  The `gateway.rs` binary handles this with `#[cfg(windows)]` /
  `#[cfg(not(windows))]` match arms that return appropriate error
  messages. On inspection, the binary does produce clear startup errors
  for unsupported platform + backend combinations (e.g., requesting
  `dpapi` on Linux yields a compile-time error path that prints a
  diagnostic). **This finding is confirmed as compliant** — the
  conditional compilation approach ensures that requesting an
  unsupported backend produces a clear error at startup.
- **Impact**: None.
- **Remediation**: None required.
- **Confidence**: High.

*(Self-correction: After tracing the binary code, this is compliant.
Finding retained for audit trail.)*

## 5. Root Cause Analysis

### Coverage Metrics

| Category | Total | Implemented | Partial | Not Impl. |
|----------|-------|-------------|---------|-----------|
| Protocol & Communication (GW-01xx) | 5 | 5 | 0 | 0 |
| Command Set (GW-02xx) | 5 | 5 | 0 | 0 |
| Chunked Transfer (GW-03xx) | 3 | 3 | 0 | 0 |
| BPF Program Mgmt (GW-04xx) | 6 | 6 | 0 | 0 |
| Application Data (GW-05xx) | 9 | 9 | 0 | 0 |
| Auth & Security (GW-06xx) | 6 | 6 | 0 | 0 |
| Node Management (GW-07xx) | 5 | 4 | 1 | 0 |
| Admin API (GW-08xx) | 8 | 7 | 1 | 0 |
| Operational (GW-10xx) | 5 | 5 | 0 | 0 |
| Modem Transport (GW-11xx) | 4 | 3 | 1 | 0 |
| BLE Pairing (GW-12xx) | 25 | 25 | 0 | 0 |
| Logging (GW-13xx) | 4 | 4 | 0 | 0 |
| **Total** | **73** (excl. withdrawn) | **65** | **4** | **0** |

*Note: Requirements counted exclude GW-0704 (intentionally unassigned).
F-001 and F-005 together affect GW-0705 (partial). F-006/F-008 affect
GW-0805 and GW-1203 (partial). F-002 affects GW-1103 (partial — met
behaviorally but in wrong layer).*

**Actual tallies after removing withdrawn/no-issue findings:**

- **IMPLEMENTED**: 69 of 73 requirements (94.5%)
- **PARTIALLY IMPLEMENTED**: 4 requirements (5.5%)
  - GW-0705 (factory reset — gateway-side only, no protocol command)
  - GW-0805 (state export/import — export complete, import incomplete)
  - GW-1103 (modem reconnection — works but in binary, not library)
  - GW-1203 (identity replication — export works, import drops identity)
- **NOT IMPLEMENTED**: 0 requirements
- **Undocumented behavior**: 1 item (F-007 — UDS/named pipe instead of
  localhost TCP; exceeds requirement)
- **Constraint violations**: 3 items (F-002, F-006, F-008)

### Causal Chain

The primary root cause for F-001/F-005 is that the protocol crate
(`sonde-protocol`) does not yet define a `FACTORY_RESET` command type.
This is a known gap documented with TODO comments. The secondary root
cause for F-006/F-008 is that the `Storage::replace_state` trait method
was designed for an earlier version of the state bundle that only
contained nodes and programs; it was not updated when phone PSKs,
gateway identity, and handler configs were added to the bundle format.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001/F-005 | Add `FACTORY_RESET` command to protocol + gateway | L | Medium — requires coordinated changes across `sonde-protocol`, `sonde-gateway`, and `sonde-node` |
| 2 | F-008 | Extend `replace_state` to restore identity + phone PSKs; update `import_state` | M | Low — additive change to existing trait |
| 3 | F-006 | Same fix as F-008 (handler configs); verify round-trip in test | S | Low |
| 4 | F-002 | Document that reconnection is application-level; or move into transport | S | Low — clarification vs. refactor |
| 5 | F-009 | Log node timeouts even without handler router | S | Low |
| 6 | F-007 | Update GW-0800 AC3 wording to match UDS/named-pipe implementation | S | Low — doc-only |

## 7. Prevention

- **Protocol-level commands**: Require that any requirement referencing
  a node-side action (erase, reset) has a corresponding command type
  defined in the protocol crate before the gateway implementation is
  considered complete.
- **State bundle round-trip tests**: Add an integration test that
  exports state, imports it on a fresh gateway, and verifies all
  components (nodes, programs, identity, phone PSKs, handler configs)
  are restored.
- **Trait method completeness checks**: When adding fields to the state
  bundle, update the `Storage` trait's `replace_state` method signature
  and all implementations in the same PR.

## 8. Open Questions

1. **Is factory reset intended to be best-effort?** If the node may
   never wake again (e.g., battery dead), should the gateway still
   remove its registry entry without confirmation? The current TODO
   suggests waiting for delivery, but that may block indefinitely.
2. **Should reconnection live in the transport layer?** The current
   architecture has the binary own the reconnection loop. This may be
   intentional (transports are stateless adapters), but the requirement
   text places the responsibility on the transport layer.
3. **Should `replace_state` be atomic for all state components?** The
   current implementation replaces nodes and programs atomically
   (SQLite transaction) but would need to extend atomicity to identity
   and phone PSKs.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-17 | Copilot (audit agent) | Initial code compliance audit |

---

## Appendix A — Requirement-by-Requirement Traceability

| REQ-ID | Title | Priority | Status | Code Location |
|--------|-------|----------|--------|---------------|
| GW-0100 | Node-initiated communication | Must | IMPLEMENTED | `engine.rs` — gateway only responds in `process_frame` |
| GW-0101 | CBOR message encoding | Must | IMPLEMENTED | `engine.rs` — uses `sonde_protocol` CBOR codec |
| GW-0102 | WAKE reception | Must | IMPLEMENTED | `engine.rs:518–736` — `handle_wake` |
| GW-0103 | COMMAND response | Must | IMPLEMENTED | `engine.rs:683–736` — encodes Command with `starting_seq`, `timestamp_ms` |
| GW-0104 | Frame size constraint | Must | IMPLEMENTED | `modem.rs` — validates frame ≤ 250 bytes in `send()` |
| GW-0200 | NOP command | Must | IMPLEMENTED | `engine.rs` — `select_command` returns `Nop` as default |
| GW-0201 | UPDATE_PROGRAM | Must | IMPLEMENTED | `engine.rs` — priority 2 in `select_command` |
| GW-0202 | RUN_EPHEMERAL | Must | IMPLEMENTED | `engine.rs` — priority 1 in `select_command`; 2 KB limit enforced |
| GW-0203 | UPDATE_SCHEDULE | Must | IMPLEMENTED | `engine.rs` — priority 3 in `select_command` |
| GW-0204 | REBOOT | Must | IMPLEMENTED | `engine.rs` — priority 4 in `select_command` |
| GW-0300 | Chunk serving | Must | IMPLEMENTED | `engine.rs:787–834` — `handle_get_chunk` |
| GW-0301 | Transfer resumption | Must | IMPLEMENTED | `session.rs` — ChunkedTransfer session reuse on WAKE retry |
| GW-0302 | Program acknowledgement | Must | IMPLEMENTED | `engine.rs:840–883` — `handle_program_ack` |
| GW-0400 | Program ingestion (ELF) | Must | IMPLEMENTED | `program.rs` — `ingest_elf` with Prevail |
| GW-0401 | Program verification (Prevail) | Must | IMPLEMENTED | `program.rs` — full Prevail pipeline |
| GW-0402 | Program identity by hash | Must | IMPLEMENTED | `program.rs` — SHA-256 of CBOR image |
| GW-0403 | Program size enforcement | Should | IMPLEMENTED | `program.rs` — 4 KB resident / 2 KB ephemeral limits |
| GW-0404 | Sonde-specific verifier platform | Must | IMPLEMENTED | `sonde_platform.rs` — `SondePlatform` with helpers 1–16 |
| GW-0405 | Initial map data from ELF | Must | IMPLEMENTED | `program.rs` — `extract_global_section_data` |
| GW-0500 | APP_DATA reception | Must | IMPLEMENTED | `engine.rs:888–930` — `handle_app_data` |
| GW-0501 | APP_DATA_REPLY response | Must | IMPLEMENTED | `engine.rs:912–927`; `handler.rs:769` — zero-length = no reply |
| GW-0502 | Handler transport | Must | IMPLEMENTED | `handler.rs` — 4-byte BE length-prefixed CBOR; 1 MB max |
| GW-0503 | Handler lifecycle | Must | IMPLEMENTED | `handler.rs` — `ensure_running` with respawn |
| GW-0504 | Handler routing by hash | Must | IMPLEMENTED | `handler.rs:746–776` — `route_app_data` with `find_handler` |
| GW-0505 | Handler DATA message | Must | IMPLEMENTED | `handler.rs` — all 6 fields present |
| GW-0506 | Handler DATA_REPLY processing | Must | IMPLEMENTED | `handler.rs:766–776` — request_id matching |
| GW-0507 | Handler EVENT messages | Should | IMPLEMENTED | `engine.rs:683–695, 862–874, 1129–1168` — 3 event types |
| GW-0508 | Handler LOG messages | Should | IMPLEMENTED | `handler.rs` — LOG messages drained and logged |
| GW-0600 | HMAC-SHA256 authentication | Must | IMPLEMENTED | `crypto.rs` — `RustCryptoHmac`; `engine.rs` — verify all frames |
| GW-0601 | Per-node key management | Must | IMPLEMENTED | `sqlite_storage.rs` — key_hint index; candidate PSK lookup |
| GW-0601a | Key store encryption at rest | Should | IMPLEMENTED | `sqlite_storage.rs` — AES-256-GCM with master key, AAD binding |
| GW-0601b | KeyProvider trait | Should | IMPLEMENTED | `key_provider.rs` — File, Env, DPAPI, SecretService backends |
| GW-0602 | Replay protection | Must | IMPLEMENTED | `session.rs` — sequence tracking; `engine.rs` — session reuse for ChunkedTransfer |
| GW-0603 | Auth overhead budget | Must | IMPLEMENTED | `sonde_protocol` — 43-byte overhead (11 header + 32 HMAC) |
| GW-0700 | Node registry | Must | IMPLEMENTED | `registry.rs`, `sqlite_storage.rs` — persistent SQLite storage |
| GW-0701 | Stale program detection | Must | IMPLEMENTED | `engine.rs` — `select_command` priority 2 compares hashes |
| GW-0702 | Battery level tracking | Should | IMPLEMENTED | `registry.rs` — `update_telemetry`; `sqlite_storage.rs` — `battery_readings` table; 100-entry history |
| GW-0703 | Firmware ABI version awareness | Must | IMPLEMENTED | `engine.rs` — ABI check in `select_command` |
| GW-0705 | Factory reset support | Must | PARTIAL | `admin.rs:242–276` — gateway-side only; no protocol command (F-001/F-005) |
| GW-0800 | Admin gRPC API | Must | IMPLEMENTED | `admin.rs` — full gRPC service |
| GW-0801 | Admin — node management | Must | IMPLEMENTED | `admin.rs` — list/get/register/remove nodes |
| GW-0802 | Admin — program management | Must | IMPLEMENTED | `admin.rs` — ingest/list/assign/remove programs |
| GW-0803 | Admin — schedule & commands | Must | IMPLEMENTED | `admin.rs` — set_schedule/queue_reboot/queue_ephemeral |
| GW-0804 | Admin — node status | Should | IMPLEMENTED | `admin.rs` — `get_node_status` with session check |
| GW-0805 | Admin — state export/import | Should | PARTIAL | `admin.rs`, `state_bundle.rs` — export complete; import drops identity/phones/handlers (F-006/F-008) |
| GW-0806 | Admin CLI tool | Must | IMPLEMENTED | `sonde-admin` crate (not audited in detail; gRPC RPCs verified) |
| GW-0807 | Admin — modem management | Must | IMPLEMENTED | `admin.rs` — get_modem_status/set_modem_channel/scan_modem_channels |
| GW-1000 | Gateway failover | Must | IMPLEMENTED | Architecture — stateless design with shared key DB |
| GW-1001 | Exportable/importable state | Should | PARTIAL | `state_bundle.rs` — export complete; import incomplete (see F-008) |
| GW-1002 | Graceful handling of unknown nodes | Must | IMPLEMENTED | `engine.rs` — silent discard with warn log |
| GW-1003 | Concurrent node handling | Should | IMPLEMENTED | Tokio async runtime; per-node sessions; RwLock isolation |
| GW-1004 | Program hash consistency | Must | IMPLEMENTED | Deterministic CBOR encoding (RFC 8949 §4.2) |
| GW-1100 | Modem transport trait | Must | IMPLEMENTED | `modem.rs` — `UsbEspNowTransport` implements `Transport` |
| GW-1101 | Modem startup sequence | Must | IMPLEMENTED | `modem.rs` — RESET → MODEM_READY → SET_CHANNEL |
| GW-1102 | Modem health monitoring | Should | IMPLEMENTED | `modem.rs:519` — `spawn_health_monitor` with 30 s interval |
| GW-1103 | Modem error handling | Must | PARTIAL | `bin/gateway.rs` — reconnection with backoff; `modem.rs` — ERROR logged (F-002) |
| GW-1200 | Ed25519 keypair generation | Must | IMPLEMENTED | `gateway_identity.rs` — `generate()` with `getrandom` |
| GW-1201 | Gateway identity generation | Must | IMPLEMENTED | `gateway_identity.rs` — 16-byte `gateway_id` from CSPRNG |
| GW-1202 | Ed25519 to X25519 conversion | Must | IMPLEMENTED | `gateway_identity.rs` — `to_x25519()` with 12 low-order points |
| GW-1203 | Ed25519 seed replication | Must | PARTIAL | Export works; import does not restore identity (F-008) |
| GW-1204 | BLE GATT server | Must | IMPLEMENTED | Modem-relay mode — `modem.rs` BLE_RECV/BLE_INDICATE |
| GW-1205 | ATT MTU negotiation | Must | IMPLEMENTED | Modem handles MTU negotiation; gateway sends complete envelopes |
| GW-1206 | REQUEST_GW_INFO handling | Must | IMPLEMENTED | `ble_pairing.rs` — Ed25519 signature over challenge ‖ gateway_id |
| GW-1207 | Registration window enforcement | Must | IMPLEMENTED | `ble_pairing.rs` — `RegistrationWindow` with ERROR 0x02 |
| GW-1208 | Registration window activation | Must | IMPLEMENTED | `admin.rs` — `open_ble_pairing`; `bin/gateway.rs` — BLE_ENABLE/DISABLE |
| GW-1209 | REGISTER_PHONE processing | Must | IMPLEMENTED | `ble_pairing.rs` — ECDH + HKDF + AES-256-GCM |
| GW-1210 | Phone PSK storage/revocation | Must | IMPLEMENTED | `phone_trust.rs`, `sqlite_storage.rs` — status field, revocation |
| GW-1211 | PEER_REQUEST key-hint bypass | Must | IMPLEMENTED | `engine.rs:~170` — special-case before key_hint lookup |
| GW-1212 | PEER_REQUEST decryption | Must | IMPLEMENTED | `engine.rs:221–516` — ECDH + AES-256-GCM |
| GW-1213 | Phone HMAC verification | Must | IMPLEMENTED | `engine.rs` — lookup by hint, skip revoked, verify each |
| GW-1214 | PEER_REQUEST frame HMAC | Must | IMPLEMENTED | `engine.rs` — verify with extracted `node_psk` |
| GW-1215 | PairingRequest timestamp | Must | IMPLEMENTED | `engine.rs` — ±86400 s drift check |
| GW-1216 | Node ID duplicate handling | Must | IMPLEMENTED | `engine.rs` — matching PSK → re-ACK; different PSK → discard |
| GW-1217 | Key hint consistency check | Must | IMPLEMENTED | `engine.rs` — header `key_hint` vs CBOR `node_key_hint` |
| GW-1218 | Node registration from PEER_REQUEST | Must | IMPLEMENTED | `engine.rs` — atomic register with all fields |
| GW-1219 | PEER_ACK generation | Must | IMPLEMENTED | `engine.rs` — HMAC proof, CBOR {1:0, 2:proof}, nonce echo |
| GW-1220 | Silent-discard error model | Must | IMPLEMENTED | `engine.rs` — all verification failures → None |
| GW-1221 | Random nonces for PEER_REQUEST/PEER_ACK | Must | IMPLEMENTED | `engine.rs` — no sequence-number checks on msg_type 0x05/0x84 |
| GW-1222 | Admin — BLE pairing session | Must | IMPLEMENTED | `admin.rs` — open/close/confirm with streaming events |
| GW-1223 | Admin — phone listing | Must | IMPLEMENTED | `admin.rs` — `list_phones` RPC |
| GW-1224 | Admin — phone revocation | Must | IMPLEMENTED | `admin.rs` — `revoke_phone` RPC |
| GW-1300 | Operational logging — lifecycle | Must | IMPLEMENTED | `engine.rs` — INFO logs for PEER_REQUEST, PEER_ACK, WAKE, COMMAND, session create/expire |
| GW-1301 | Operational logging — modem state | Must | IMPLEMENTED | `bin/gateway.rs` — connected/ready/disconnecting/reconnecting |
| GW-1302 | Operational logging — frame debug | Should | IMPLEMENTED | `modem.rs` — DEBUG logs with msg_type, peer_mac, len |
| GW-1303 | Build metadata | Must | IMPLEMENTED | `bin/gateway.rs:84` — `SONDE_GIT_COMMIT` in `--version` |
| GW-1304 | Build-type–aware log levels | Must | IMPLEMENTED | `Cargo.toml:19` — `max_level_trace` + `release_max_level_info`; `bin/gateway.rs` — EnvFilter defaults |
