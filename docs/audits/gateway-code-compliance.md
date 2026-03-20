<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Code-Compliance Audit Report

> **Audit scope:** D8 (forward traceability — spec → code), D9 (backward
> traceability — code → spec), D10 (constraint verification).
>
> **Snapshot:** Pre-remediation baseline.  All source references are to the
> commit checked-out at time of audit and may shift after fixes land.
>
> **Inputs:**
>
> | Artifact | Path |
> |----------|------|
> | Requirements | `docs/gateway-requirements.md` (76 requirements) |
> | Design | `docs/gateway-design.md` |
> | Gateway source | `crates/sonde-gateway/src/*.rs` |
> | Proto definition | `crates/sonde-gateway/proto/admin.proto` |
> | Admin CLI source | `crates/sonde-admin/src/*.rs` |
> | Protocol crate | `crates/sonde-protocol/src/` |

---

## 1  Executive summary

76 requirements were evaluated.  **65 are fully implemented**, 7 are
**partially implemented**, 2 are **not implemented**, and 2 have
**constraint violations**.  Additionally 5 undocumented behaviours (D9)
were identified.

| Category | Count |
|----------|------:|
| Fully implemented | 65 |
| Partially implemented (D8-partial) | 7 |
| Not implemented (D8-missing) | 2 |
| Constraint violation (D10) | 2 |
| Undocumented behaviour (D9) | 5 |

**Must-priority gaps:** 4 findings affect Must-priority requirements
(F-001, F-002, F-005, F-006).

---

## 2  Findings

### F-001 — D8 | GW-0400, GW-0401 | Admin API bypasses ELF ingestion and Prevail verification

**Priority:** Must

**Requirement (GW-0400):**  The gateway MUST accept BPF programs as
pre-compiled ELF files and extract bytecode + map definitions into a
CBOR program image at ingestion time.

**Requirement (GW-0401):**  The gateway MUST verify all BPF programs
using the Prevail verifier before distributing them.

**Evidence:**  `admin.rs` lines 207–229 call
`program_library.ingest_unverified(req.image_data, profile)`.  The
comment on lines 207–208 explicitly states: *"ELF→CBOR
extraction/verification will be added in a future phase; callers must
supply pre-encoded CBOR for now."*

The `ingest_elf()` function exists in `program.rs` (line 137) and
performs full Prevail verification, but it is **not wired to the admin
API**.  The admin proto (`IngestProgramRequest.image_data`) accepts raw
bytes documented as a CBOR image, not an ELF.

**Impact:**  Programs can be distributed without Prevail safety checks.
An operator must manually verify programs offline or trust the
pre-encoded CBOR.

**Remediation:**  Wire `ingest_elf()` into the admin API (add a flag or
auto-detect ELF magic), or at minimum run verification on the
pre-encoded CBOR image within `ingest_unverified()`.

---

### F-002 — D8 | GW-0705 | Factory reset not implemented

**Priority:** Must

**Requirement:**  The gateway MUST support triggering a factory reset on
a connected node, erasing the node's PSK, maps, and resident program.
After reset the gateway removes the node from its registry.

**Evidence:**  No code matching `factory_reset`, `factory`, or
`FactoryReset` exists in `crates/sonde-gateway/` or
`crates/sonde-admin/`.  No RPC in `admin.proto`.  No CLI subcommand.

**Impact:**  Operators cannot remotely wipe compromised or
decommissioned nodes.

**Remediation:**  Add a `FactoryReset` RPC that sends a factory-reset
command to the node (new command type) and removes the node from the
registry on acknowledgement.  Add `sonde-admin node factory-reset`
CLI command.

---

### F-003 — D8-partial | GW-0702 | Battery level tracking — no historical data

**Priority:** Should

**Requirement:**  The gateway SHOULD record `battery_mv` for monitoring.
Acceptance criterion 2: *"Historical battery data is available for trend
analysis."*

**Evidence:**  `NodeRecord.last_battery_mv` (`registry.rs` line 31) is
a single `Option<u32>` overwritten on every WAKE
(`update_telemetry()`, line 62).  The SQLite schema has only
`last_battery_mv INTEGER` — no history table.

**Impact:**  Operators cannot detect battery degradation trends.

**Remediation:**  Add a `battery_history` table
(`node_id, battery_mv, recorded_at`) and insert a row on each WAKE.
Expose via a `GetBatteryHistory` RPC.

---

### F-004 — D8-partial | GW-1002 | Unknown-node discard not logged

**Priority:** Must

**Requirement:**  Messages from unregistered nodes are silently
discarded (criterion 1 ✓), and *"the event is logged for operational
monitoring"* (criterion 2).

**Evidence:**  `engine.rs` lines 144–147:

```rust
let candidates = self.storage.get_nodes_by_key_hint(key_hint).await.ok()?;
if candidates.is_empty() {
    return None;  // no log statement
}
```

No `warn!`, `info!`, or `debug!` call on this path.

**Impact:**  Operators have no visibility into authentication failures
from unknown or decommissioned nodes.

**Remediation:**  Add `debug!` or `warn!` before the early return:
`warn!(key_hint, "no candidate keys for key_hint — discarding frame");`

---

### F-005 — D8-partial | GW-0101 | Malformed CBOR — no diagnostic logged

**Priority:** Must

**Requirement (criterion 3):**  The gateway detects malformed inbound
CBOR, *"logs an internal error (or equivalent diagnostic)"*, drops the
message, and does not crash.

**Evidence:**  Throughout `engine.rs`, CBOR decode failures use the
`.ok()?` pattern (e.g., line 217: `ciborium::from_reader(...).ok()?`),
which silently converts errors to `None`.  The message IS dropped and
the gateway does NOT crash, but no diagnostic is emitted.

**Impact:**  Silent drops of malformed messages make protocol debugging
harder.

**Remediation:**  Replace `.ok()?` with explicit match arms that call
`warn!` or `debug!` before returning `None`.

---

### F-006 — D8-partial | GW-1208 | Registration window — no physical button support

**Priority:** Must

**Requirement:**  The gateway MUST open the registration window via
*"a physical button hold (≥ 2 s)"* or the admin API.

**Evidence:**  The admin API path works (`BlePairingController::open_window()`
in `ble_pairing.rs`).  A search for `button` or `gpio` in the gateway
crate yields no results.  No GPIO/interrupt code exists.

**Impact:**  Registration window can only be opened via CLI/gRPC, not
via hardware button as specified for field-deployment scenarios.

**Remediation:**  Add a GPIO button-monitoring task (platform-specific;
may be ESP32-only or require a companion daemon) that calls
`ble_controller.open_window()` on long-press.  If the gateway is
always remote (no local GPIO), document this as a deployment constraint
and accept the deviation.

---

### F-007 — D8-partial | GW-0805, GW-1001 | State export omits handler routing configuration

**Priority:** Should

**Requirement (GW-1001 criterion 1):**  The gateway can export its
state to a portable format.  GW-0805 lists *"handler routing
configuration"* among the items.

**Evidence:**  `state_bundle.rs` lines 28–31: *"Handler routing
configuration is not included and must be restored separately (deferred
per Phase 2C-iii)."*  The bundle includes nodes, programs, gateway
identity, and phone PSKs, but **not** handler configs.

**Impact:**  After import on a failover gateway, handlers must be
reconfigured manually.

**Remediation:**  Include handler routing YAML content in the bundle
(new CBOR root key).

---

### F-008 — D8-partial | GW-0802 | Admin API program ingestion accepts CBOR, not ELF

**Priority:** Must

**Requirement:**  `IngestProgram` accepts an ELF binary and
verification profile.

**Evidence:**  Same root cause as F-001.  `admin.proto` line 71:
`bytes image_data = 1;` documented as CBOR image data.
`admin.rs` line 217 calls `ingest_unverified()`.

**Impact:**  Operators must pre-process ELF → CBOR offline.

**Remediation:**  Accept ELF as input format (auto-detect via ELF magic
`\x7fELF`).  Fall through to `ingest_elf()`.

---

### F-009 — D10 | GW-0602 | `starting_seq` uses `rand::rng()` instead of `getrandom`

**Priority:** Must

**Requirement:**  The gateway assigns a random starting sequence number
in the COMMAND response.  Project convention: *"Use `getrandom::fill()`
for cryptographic randomness, not `rand::rng()`."*

**Evidence:**  `engine.rs` line 479:

```rust
let starting_seq: u64 = rand::rng().random();
```

`getrandom::fill()` is not used anywhere in `engine.rs`.

**Impact:**  While modern `rand` versions delegate to OS CSPRNG, the
project convention exists because `rand::rng()` API stability is not
guaranteed across versions.  A future `rand` update could silently
change the entropy source.

**Remediation:**  Replace with:

```rust
let mut buf = [0u8; 8];
getrandom::fill(&mut buf).expect("OS CSPRNG unavailable");
let starting_seq = u64::from_le_bytes(buf);
```

---

### F-010 — D10 | Security | `rand::rng()` used in PEER_REQUEST pairing nonce path

**Priority:** Must

**Requirement:**  Project convention requires `getrandom::fill()` for
cryptographic randomness.

**Evidence:**  `ble_pairing.rs` uses `rand::rng()` for generating the
phone PSK AES-GCM nonce (12-byte IV).  Same concern as F-009.

**Remediation:**  Replace with `getrandom::fill()`.

---

### F-011 — D9 | Undocumented | Program deletion protection

**Evidence:**  `admin.rs` prevents deleting a program that is assigned
to any node or referenced by a pending `RunEphemeral` command.  This is
not specified in GW-0802 or any requirement.

**Assessment:**  Positive safety net.  Consider documenting as an
acceptance criterion of GW-0802.

---

### F-012 — D9 | Undocumented | State import rejects active sessions

**Evidence:**  `admin.rs` `import_state()` returns an error if
`session_manager.active_count() > 0`.  Not specified in GW-0805.

**Assessment:**  Prevents data corruption during import.  Document in
GW-0805 or design doc.

---

### F-013 — D9 | Undocumented | Modem RESET retry logic

**Evidence:**  `modem.rs` `UsbEspNowTransport::new()` retries the
`RESET` → `MODEM_READY` handshake up to 3 times over 15 seconds and
handles stale `MODEM_READY` from prior sessions.  GW-1101 does not
mention retries.

**Assessment:**  Robust startup behaviour.  Document in GW-1101.

---

### F-014 — D9 | Undocumented | Handler LOG drain limit

**Evidence:**  `handler.rs` `drain_stdout()` reads at most 16 LOG
messages per drain call with a 50 ms peek timeout and 2 s frame
timeout.  Handler timeout for DATA replies is 30 seconds.  Neither
limit is specified.

**Assessment:**  Prevents unbounded blocking.  Document as
implementation constraints in GW-0502/GW-0508.

---

### F-015 — D9 | Undocumented | SlotGuard cancellation-safety pattern

**Evidence:**  `modem.rs` uses a `SlotGuard<T>` pattern that clears a
mutex slot on drop, preventing timeout-then-late-response races for
`poll_status()`, `change_channel()`, and `scan_channels()`.  Not
documented anywhere.

**Assessment:**  Correct concurrency pattern.  Worth noting in design
docs for maintainability.

---

## 3  Forward traceability matrix (spec → code)

| Req ID | Title | Priority | Status | Notes |
|--------|-------|----------|--------|-------|
| GW-0100 | Node-initiated communication | Must | ✅ Implemented | Gateway only responds; never initiates frames |
| GW-0101 | CBOR message encoding | Must | ⚠️ **F-005** | Encode/decode ✓; malformed-CBOR logging missing |
| GW-0102 | WAKE reception | Must | ✅ Implemented | `engine.rs` decodes all WAKE fields |
| GW-0103 | COMMAND response | Must | ✅ Implemented | `starting_seq`, `timestamp_ms`, `command_type` present |
| GW-0104 | Frame size constraint | Must | ✅ Implemented | `MAX_FRAME_SIZE = 250` enforced in codec |
| GW-0200 | NOP command | Must | ✅ Implemented | Default fallthrough in `select_command()` |
| GW-0201 | UPDATE_PROGRAM | Must | ✅ Implemented | Hash mismatch → chunked transfer |
| GW-0202 | RUN_EPHEMERAL | Must | ✅ Implemented | 2 KB limit enforced (`MAX_EPHEMERAL_SIZE`) |
| GW-0203 | UPDATE_SCHEDULE | Must | ✅ Implemented | `PendingCommand::UpdateSchedule` |
| GW-0204 | REBOOT | Must | ✅ Implemented | `PendingCommand::Reboot` |
| GW-0300 | Chunk serving | Must | ✅ Implemented | GET_CHUNK → CHUNK with nonce echo |
| GW-0301 | Transfer resumption | Must | ✅ Implemented | Stateless chunk serving; re-request OK |
| GW-0302 | Program ACK | Must | ✅ Implemented | `upsert_node()` persists `current_program_hash` |
| GW-0400 | ELF ingestion | Must | ❌ **F-001** | `ingest_elf()` exists but admin API bypasses it |
| GW-0401 | Prevail verification | Must | ❌ **F-001** | Verification code exists; admin path uses `ingest_unverified()` |
| GW-0402 | Program identity by hash | Must | ✅ Implemented | SHA-256 of deterministic CBOR image |
| GW-0403 | Program size enforcement | Should | ✅ Implemented | 4 KB resident, 2 KB ephemeral limits |
| GW-0500 | APP_DATA reception | Must | ✅ Implemented | Authenticated, routed to handler |
| GW-0501 | APP_DATA_REPLY response | Must | ✅ Implemented | Zero-length → no reply; nonce echoed |
| GW-0502 | Handler transport | Must | ✅ Implemented | stdin/stdout, 4-byte BE prefix, 1 MB max |
| GW-0503 | Handler lifecycle | Must | ✅ Implemented | Exit 0 → respawn; non-zero → log error |
| GW-0504 | Handler routing | Must | ✅ Implemented | Exact-match + catch-all (`*`) |
| GW-0505 | Handler DATA message | Must | ✅ Implemented | All 6 fields (msg_type, request_id, node_id, program_hash, data, timestamp) |
| GW-0506 | DATA_REPLY processing | Must | ✅ Implemented | request_id mismatch logged + discarded |
| GW-0507 | Handler EVENT messages | Should | ✅ Implemented | `node_online`, `program_updated`, `node_timeout` |
| GW-0508 | Handler LOG messages | Should | ✅ Implemented | Routes through `tracing`; preserves 4 levels |
| GW-0600 | HMAC-SHA256 authentication | Must | ✅ Implemented | Constant-time verify; all outbound tagged |
| GW-0601 | Per-node key management | Must | ✅ Implemented | key_hint lookup; add/remove nodes |
| GW-0601a | Key store encryption at rest | Should | ✅ Implemented | AES-256-GCM; legacy plaintext migration |
| GW-0601b | KeyProvider backends | Should | ✅ Implemented | File, Env, DPAPI (Win), SecretService (Linux) |
| GW-0602 | Replay protection | Must | ⚠️ **F-009** | Implemented; `starting_seq` RNG source non-compliant |
| GW-0603 | Auth overhead budget | Must | ✅ Implemented | 43 bytes (11 + 32) correctly accounted |
| GW-0700 | Node registry | Must | ✅ Implemented | Persistent SQLite; all required fields |
| GW-0701 | Stale program detection | Must | ✅ Implemented | Hash comparison in `select_command()` |
| GW-0702 | Battery level tracking | Should | ⚠️ **F-003** | Latest reading ✓; no history table |
| GW-0703 | Firmware ABI awareness | Must | ✅ Implemented | ABI check blocks incompatible distribution |
| GW-0705 | Factory reset | Must | ❌ **F-002** | Not implemented |
| GW-0800 | Admin gRPC API | Must | ✅ Implemented | Unix socket + Windows named pipe |
| GW-0801 | Admin — node management | Must | ✅ Implemented | List, Get, Register, Remove |
| GW-0802 | Admin — program management | Must | ⚠️ **F-008** | Accepts CBOR, not ELF |
| GW-0803 | Admin — schedule/commands | Must | ✅ Implemented | SetSchedule, QueueReboot, QueueEphemeral |
| GW-0804 | Admin — node status | Should | ✅ Implemented | GetNodeStatus with latest WAKE data |
| GW-0805 | Admin — state export/import | Should | ⚠️ **F-007** | Handler routing config not included |
| GW-0806 | Admin CLI tool | Must | ✅ Implemented | All commands; `--format json` support |
| GW-0807 | Admin — modem management | Must | ✅ Implemented | Status, SetChannel, Scan RPCs + CLI |
| GW-1000 | Gateway failover | Must | ✅ Implemented | Identity = key database; no hardware binding |
| GW-1001 | Exportable state | Should | ⚠️ **F-007** | Handler routing config not included |
| GW-1002 | Unknown-node handling | Must | ⚠️ **F-004** | Silent discard ✓; logging missing |
| GW-1003 | Concurrent node handling | Should | ✅ Implemented | Async Tokio; per-node state isolation |
| GW-1004 | Program hash consistency | Must | ✅ Implemented | Deterministic CBOR encoding |
| GW-1100 | Modem transport | Must | ✅ Implemented | `UsbEspNowTransport` |
| GW-1101 | Modem startup sequence | Must | ✅ Implemented | RESET → MODEM_READY → SET_CHANNEL |
| GW-1102 | Modem health monitoring | Should | ✅ Implemented | `spawn_health_monitor()`; tx_fail + reboot detection |
| GW-1103 | Modem error handling | Must | ✅ Implemented | ERROR messages logged |
| GW-1200 | Ed25519 keypair generation | Must | ✅ Implemented | CSPRNG seed; encrypted at rest |
| GW-1201 | Gateway identity generation | Must | ✅ Implemented | 16-byte random; persisted |
| GW-1202 | Ed25519 → X25519 conversion | Must | ✅ Implemented | 12 low-order points rejected |
| GW-1203 | Ed25519 seed replication | Must | ✅ Implemented | Via state bundle export/import |
| GW-1204 | BLE GATT server | Must | ✅ Implemented | Modem-relay mode; BLE_RECV/BLE_INDICATE |
| GW-1205 | ATT MTU negotiation | Must | ✅ Implemented | `BLE_MTU_MIN = 247`; fragmentation in modem FW |
| GW-1206 | REQUEST_GW_INFO | Must | ✅ Implemented | Ed25519 sign(challenge ‖ gateway_id) |
| GW-1207 | Registration window enforcement | Must | ✅ Implemented | ERROR(0x02) when closed |
| GW-1208 | Registration window activation | Must | ⚠️ **F-006** | Admin API ✓; physical button missing |
| GW-1209 | REGISTER_PHONE | Must | ✅ Implemented | ECDH + HKDF + AES-256-GCM |
| GW-1210 | Phone PSK storage/revocation | Must | ✅ Implemented | Label, timestamp, active/revoked status |
| GW-1211 | PEER_REQUEST key-hint bypass | Must | ✅ Implemented | Special `MSG_PEER_REQUEST` path in `process_frame` |
| GW-1212 | PEER_REQUEST decryption | Must | ✅ Implemented | ECDH + HKDF + AES-256-GCM; GCM fail → discard |
| GW-1213 | Phone HMAC verification | Must | ✅ Implemented | Tries non-revoked candidates |
| GW-1214 | PEER_REQUEST frame HMAC | Must | ✅ Implemented | Verified with extracted `node_psk` |
| GW-1215 | PairingRequest timestamp | Must | ✅ Implemented | ±86 400 s window |
| GW-1216 | Node ID uniqueness check | Must | ✅ Implemented | `insert_node_if_not_exists()` |
| GW-1217 | Key hint consistency check | Must | ✅ Implemented | Header vs CBOR `node_key_hint` |
| GW-1218 | Node registration from PEER_REQUEST | Must | ✅ Implemented | All fields stored incl. `registered_by` |
| GW-1219 | PEER_ACK generation | Must | ✅ Implemented | `registration_proof`; nonce echoed |
| GW-1220 | Silent-discard error model | Must | ✅ Implemented | No error frames transmitted |
| GW-1221 | Random nonces for PEER_REQUEST/ACK | Must | ✅ Implemented | No sequence-number checks on 0x05/0x84 |
| GW-1222 | Admin — BLE pairing session | Must | ✅ Implemented | Open/Close/Confirm RPCs; BLE_ENABLE/DISABLE |
| GW-1223 | Admin — phone listing | Must | ✅ Implemented | ListPhones RPC + CLI |
| GW-1224 | Admin — phone revocation | Must | ✅ Implemented | RevokePhone RPC + CLI |

---

## 4  Coverage metrics

| Metric | Value |
|--------|------:|
| Total requirements | 76 |
| Fully compliant | 65 (85.5 %) |
| Partially compliant | 7 (9.2 %) |
| Not implemented | 2 (2.6 %) |
| Constraint violations | 2 (2.6 %) |
| **Must-priority gaps** | **4** |
| **Should-priority gaps** | **3** |

### Source file → functional area map

| File | Functional area | Key requirements |
|------|----------------|------------------|
| `engine.rs` | Protocol state machine | GW-0100–0103, GW-0200–0204, GW-0300–0302, GW-0500–0501, GW-0600, GW-0602, GW-0701, GW-0703, GW-1211–1221 |
| `session.rs` | Session management | GW-0602 |
| `handler.rs` | Handler lifecycle & routing | GW-0502–0508 |
| `program.rs` | Program ingestion & verification | GW-0400–0403 |
| `crypto.rs` | HMAC/SHA-256 providers | GW-0600, GW-0603 |
| `key_provider.rs` | Master key backends | GW-0601b |
| `storage.rs` / `sqlite_storage.rs` | Persistent storage | GW-0601a, GW-0700, GW-0702 |
| `registry.rs` | Node record model | GW-0700, GW-0702, GW-0703 |
| `state_bundle.rs` | Export/import | GW-0805, GW-1001, GW-1203 |
| `admin.rs` | gRPC admin service | GW-0800–0807, GW-1222–1224 |
| `modem.rs` | Modem transport | GW-1100–1103 |
| `ble_pairing.rs` | BLE pairing | GW-1206–1209 |
| `gateway_identity.rs` | Ed25519 identity | GW-1200–1202 |
| `phone_trust.rs` | Phone PSK model | GW-1210 |
| `admin.proto` | gRPC schema | GW-0800 |

---

## 5  Remediation plan

Findings are ordered by priority (Must before Should) then by
estimated effort.

| Finding | Priority | Effort | Remediation |
|---------|----------|--------|-------------|
| **F-004** GW-1002 logging | Must | S (< 1 h) | Add `warn!` on unknown key_hint discard in `engine.rs` |
| **F-005** GW-0101 CBOR logging | Must | S (< 1 h) | Replace `.ok()?` with match + `warn!` in CBOR decode paths |
| **F-009** GW-0602 RNG source | Must | S (< 1 h) | Replace `rand::rng().random()` with `getrandom::fill()` in `engine.rs` |
| **F-010** RNG in BLE pairing | Must | S (< 1 h) | Replace `rand::rng()` with `getrandom::fill()` in `ble_pairing.rs` |
| **F-001/F-008** GW-0400/0401/0802 ELF ingestion | Must | M (1–3 d) | Wire `ingest_elf()` into admin API; auto-detect ELF magic |
| **F-002** GW-0705 factory reset | Must | L (3–5 d) | New command type, RPC, CLI; node-side erase protocol |
| **F-006** GW-1208 button support | Must | M (1–3 d) | Platform-specific GPIO task or document deployment constraint |
| **F-003** GW-0702 battery history | Should | M (1–2 d) | New `battery_history` table + migration + RPC |
| **F-007** GW-0805/1001 handler routing in bundle | Should | S (< 1 d) | Add YAML content as new CBOR root key in `state_bundle.rs` |

---

## 6  Undocumented behaviours (D9 — for spec update consideration)

| Finding | Behaviour | Recommendation |
|---------|-----------|----------------|
| F-011 | Program deletion blocked when assigned to nodes or pending commands | Add to GW-0802 acceptance criteria |
| F-012 | State import rejected when active sessions exist | Add to GW-0805 acceptance criteria |
| F-013 | Modem RESET retries 3× over 15 s; handles stale MODEM_READY | Document in GW-1101 |
| F-014 | Handler LOG drain capped at 16 messages; handler DATA reply timeout 30 s | Document in GW-0502/GW-0508 |
| F-015 | `SlotGuard` cancellation-safety pattern in modem transport | Document in design doc (modem transport section) |

---

*End of report.*
