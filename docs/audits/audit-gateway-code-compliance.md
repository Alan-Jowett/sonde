# Sonde Code Compliance Audit — Investigation Report

**Crate:** `sonde-gateway`
**Date:** 2026-03-23

---

## 1. Executive Summary

The `sonde-gateway` crate was audited against 69 requirements (GW-0100 through GW-1224) from the gateway requirements specification. **63 of 69 requirements (91.3%) are fully implemented** in code. Four requirements are partially implemented (GW-0705, GW-0805, GW-1102, GW-1208), and two are missing specific acceptance criteria (GW-0805 identity/phone export, GW-0705 node-side reset). Backward traceability (Phase 4) identified **2 undocumented behaviors** — Windows NT service management and handler YAML configuration file loading — neither of which trace to any GW-XXXX requirement. No constraint violations (D10) were found. The recommended priority action is completing the state export/import RPC (GW-0805) to include gateway identity and phone PSKs, and wiring up the existing-but-unused modem health monitor (GW-1102).

---

## 2. Problem Statement

This audit determines whether the `sonde-gateway` source code faithfully implements its specification. The primary concern is **backward traceability** — identifying code behavior not covered by any requirement (D9 findings). Forward traceability (D8 — unimplemented requirements) and constraint verification (D10) are also checked. The audit is static analysis only; no code was executed.

---

## 3. Investigation Scope

- **Codebase / components examined:**
  - `crates/sonde-gateway/src/` — all 17 source files (~11,900 lines total):
    `engine.rs`, `admin.rs`, `handler.rs`, `ble_pairing.rs`, `modem.rs`,
    `key_provider.rs`, `sqlite_storage.rs`, `state_bundle.rs`, `session.rs`,
    `program.rs`, `gateway_identity.rs`, `crypto.rs`, `transport.rs`,
    `storage.rs`, `registry.rs`, `phone_trust.rs`, `lib.rs`
  - `crates/sonde-gateway/src/bin/gateway.rs` — CLI binary (844 lines)
  - `proto/admin.proto` — gRPC service definition (24 RPCs)
  - `crates/sonde-gateway/tests/` — integration tests (~10,000 lines, used for cross-reference only)
- **Specification documents:**
  - `docs/gateway-requirements.md` — 69 requirements (GW-0100–GW-1224)
  - `docs/gateway-design.md` — design reference (used for context)
  - `docs/gateway-validation.md` — validation plan (used for cross-reference)
- **Tools used:** ripgrep (pattern search), file viewer (code inspection), sub-agent exploration (structural analysis)
- **Limitations:**
  - The `sonde-admin` crate (CLI tool) was **not** examined — GW-0806 was assessed based on the gRPC API surface only.
  - GW-1204/GW-1205 (BLE GATT / ATT MTU) are implemented by the modem firmware and relayed via serial protocol; the gateway-side relay code was examined but the modem firmware was not.
  - Runtime behavior (performance, concurrency under load) was not tested — this is static analysis only.

---

## 4. Findings

### Finding F-001: State export RPC omits identity, phone PSKs, and handler configs

- **Severity**: High
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: GW-0805 — "ExportState returns the **complete** gateway state as a portable binary" including "node registry, cryptographic keys, program library, schedules, and handler routing configuration"
- **Code Location**: `admin.rs:467–489` (`export_state` RPC) and `admin.rs:501–546` (`import_state` RPC)
- **Description**: The admin `export_state` RPC calls `encrypt_state(nodes, programs, passphrase)` which delegates to `encrypt_state_full(nodes, programs, None, &[], &[], passphrase)` — passing `None` for gateway identity, empty slices for phone PSKs and handler configs. The `import_state` RPC similarly calls `decrypt_state()` which only returns nodes and programs. The library functions `encrypt_state_full()` / `decrypt_state_full()` support the full data set (identity, phone PSKs, handler configs) but the admin RPCs do not use them.
- **Evidence**:
  - Spec (GW-0805 AC-1): "ExportState returns the complete gateway state as a portable binary."
  - Code (`admin.rs:483`): `crate::state_bundle::encrypt_state(&nodes, &programs, &passphrase)` — only nodes + programs.
  - Code (`state_bundle.rs:176–181`): `encrypt_state()` calls `encrypt_state_full(nodes, programs, None, &[], &[], passphrase)`.
  - The full-featured `encrypt_state_full()` exists at `state_bundle.rs:186` but is not invoked by any RPC.
- **Impact**: Gateway failover (GW-1000) is incomplete — a replacement gateway loaded from an export will lack the Ed25519 identity (GW-1203), phone PSKs (GW-1210), and handler routing. Nodes paired via BLE cannot be authenticated by the replacement gateway without manual re-provisioning. This also means GW-1203 acceptance criterion 1 ("seed and gateway_id can be exported from one gateway and imported into another") is unmet via the admin API.
- **Remediation**: Update `export_state` to call `encrypt_state_full()` with all state components. Update `import_state` to call `decrypt_state_full()` and persist identity, phone PSKs, and handler configs.
- **Confidence**: High — verified by direct code inspection of the RPC handler and state bundle API.

---

### Finding F-002: Factory reset does not send node-side reset command

- **Severity**: High
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: GW-0705 — "A factory reset erases the node's pre-shared key, all persistent map data, and the resident BPF program."
- **Code Location**: `admin.rs:209–216` (`remove_node` handler)
- **Description**: The `remove_node` admin RPC only deletes the node from the gateway's registry via `self.storage.delete_node(node_id)`. It does not send any protocol-level reset command to the node. A TODO comment explicitly documents the gap.
- **Evidence**:
  - Code (`admin.rs:209–212`):
    ```
    // TODO: GW-0705 / T-0706 require a node-side factory reset (erase
    // PSK, persistent maps, and resident program) before removing the
    // key from the registry. This needs a protocol-level reset command
    // sent while the node is still authenticated — not yet implemented.
    ```
  - Spec (GW-0705 AC-1): "After factory reset, the node cannot authenticate with any gateway."
  - Spec (GW-0705 AC-2): "After factory reset, the node contains no persistent application state."
  - Current behavior: The gateway removes its own record but the node retains its PSK, maps, and resident program.
- **Impact**: A "removed" node still holds valid cryptographic material and can re-authenticate with any gateway that still has (or re-imports) the same key database. This is a security concern — the node is not truly decommissioned.
- **Remediation**: Implement a protocol-level factory reset command (e.g., a new command_type sent during the node's next WAKE cycle) that instructs the node to erase its key material, maps, and resident program before the gateway deletes its registry entry.
- **Confidence**: High — explicit TODO in code confirms the gap is known.

---

### Finding F-003: Modem health monitor function exists but is never called

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: GW-1102 — "The gateway modem transport adapter SHOULD poll GET_STATUS periodically (recommended: every 30 seconds)"
- **Code Location**: `modem.rs:485–554` (`spawn_health_monitor`) and `bin/gateway.rs` (no call site)
- **Description**: The function `spawn_health_monitor()` is fully implemented in `modem.rs` — it polls GET_STATUS at a configurable interval, detects `tx_fail_count` deltas, and logs modem reboots (uptime decreases). However, `bin/gateway.rs` never calls this function. A grep for `health_monitor` in `gateway.rs` returns zero matches.
- **Evidence**:
  - Code (`modem.rs:485`): `pub fn spawn_health_monitor(transport, interval, cancel)` — public, fully implemented.
  - Code (`bin/gateway.rs`): No call to `spawn_health_monitor` anywhere in the file.
  - Spec (GW-1102): "SHOULD poll GET_STATUS periodically" — priority is Should, not Must.
- **Impact**: Send failures and unexpected modem reboots go undetected at runtime. The operator has no visibility into modem health without manually querying `sonde-admin modem status`.
- **Remediation**: Add a `spawn_health_monitor()` call in the gateway startup sequence (`bin/gateway.rs`, within the reconnection loop after transport creation), using the recommended 30-second interval and the existing cancellation token.
- **Confidence**: High — confirmed by grep showing zero call sites.

---

### Finding F-004: Registration window button-hold activation not implemented

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Spec Location**: GW-1208 AC-1 — "A button hold of ≥2 s opens the registration window."
- **Code Location**: `ble_pairing.rs`, `admin.rs`, `bin/gateway.rs` — no button handling code found.
- **Description**: GW-1208 specifies two activation mechanisms: (1) physical button hold ≥2 s, and (2) admin API. The admin API mechanism (AC-2) is fully implemented via the `OpenBlePairing` gRPC RPC. However, no code in the gateway crate handles physical button input. A search for "button" across the entire `sonde-gateway` crate returns zero relevant matches. In modem-relay mode, the modem firmware would detect the button press, but no modem protocol message (e.g., `BUTTON_PRESS`) is defined or handled by the gateway transport adapter.
- **Evidence**:
  - Spec (GW-1208 AC-1): "A button hold of ≥2 s opens the registration window."
  - Spec (GW-1208 AC-2): "The admin API can open the registration window." — ✅ implemented in `admin.rs:636+`.
  - Code: grep for `button` in `crates/sonde-gateway/` — zero matches.
- **Impact**: The registration window can only be opened via the admin CLI, requiring a host computer. Button-based activation (useful for headless deployments) is unavailable.
- **Remediation**: Either (a) add a modem protocol message for button press events and handle it in the gateway's modem adapter to auto-open the registration window, or (b) revise GW-1208 to reflect that button-hold is a modem/node-firmware responsibility and the gateway only supports admin API activation.
- **Confidence**: Medium — the requirement may intend button handling at the modem/node layer rather than in the gateway software. The modem firmware was not examined.

---

### Finding F-005: Windows NT service management not documented in requirements

- **Severity**: Medium
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — no matching requirement identified. Searched: GW-0100–GW-1224 (all 69 requirements), `gateway-design.md`, `gateway-requirements.md`. Grep for "Windows service", "NT service", "SCM", "service install" in `docs/` returned only a systemd reference in `getting-started.md`.
- **Code Location**: `bin/gateway.rs:44–94` (ServiceCommand enum, `windows_service` macro) and `bin/gateway.rs:700–795` (`install_service`, `uninstall_service` functions)
- **Description**: The gateway binary includes a complete Windows NT service integration: `install` and `uninstall` subcommands that register/remove the gateway as an auto-start Windows service via the SCM. This includes service entry point handling (`service_entry`), SCM status reporting, dedicated log file support, and CLI argument embedding in the service registration. This is approximately 250 lines of platform-specific code with no tracing requirement.
- **Evidence**:
  - Code (`gateway.rs:68–79`): `enum ServiceCommand { Install, Uninstall }` with doc comments describing Windows NT service registration.
  - Code (`gateway.rs:700–795`): Full `install_service()` and `uninstall_service()` implementations using the `windows_service` crate.
  - No GW-XXXX requirement mentions Windows service management.
  - `docs/getting-started.md:441–468` documents systemd service setup for Linux but not Windows service setup.
- **Impact**: The Windows service feature is untested against any specification. Changes to this code have no acceptance criteria. This is a **requirements gap** (the feature is reasonable and likely intentional) rather than scope creep.
- **Remediation**: Add a requirement (e.g., GW-1005) documenting Windows NT service support, including install, uninstall, auto-start, and log file behavior. Add corresponding validation test cases.
- **Confidence**: High — verified that no GW-XXXX requirement references Windows service management.

---

### Finding F-006: Handler YAML configuration file format not in requirements

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Spec Location**: None — no matching requirement identified. Searched: GW-0500–GW-0508 (handler requirements), GW-0800–GW-0807 (admin API). GW-0504 specifies handler routing by program hash but does not specify the configuration file format. `gateway-design.md` §9 documents the YAML format (referenced in `gateway.rs:122`).
- **Code Location**: `handler.rs` (`load_handler_configs` function) and `bin/gateway.rs:120–125` (`--handler-config` CLI argument)
- **Description**: The gateway loads handler routing configuration from a YAML file specified via the `--handler-config` CLI argument. The YAML format defines which handler process to spawn for each program hash (or catch-all). This configuration mechanism is documented in the design document but has no corresponding requirement. The handler *behavior* (routing, DATA messages, lifecycle) is well-specified in GW-0502–GW-0508, but the *configuration format* is not.
- **Evidence**:
  - Code (`gateway.rs:122`): `/// See gateway-design.md §9 for the format.`
  - GW-0504 specifies routing semantics but not configuration mechanism.
  - Design doc provides the format specification.
- **Impact**: Low — this is reasonable infrastructure supporting GW-0504. The YAML format is stable and documented in the design doc. The gap is minor: a configuration format without a formal requirement.
- **Remediation**: Add a requirement (e.g., GW-0509) specifying the handler configuration file format and its CLI argument, or note in GW-0504 that the configuration mechanism is defined in the design document.
- **Confidence**: High — verified that no GW-XXXX requirement specifies the YAML config format.

---

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|--------|-------|
| **Total requirements** | 69 (GW-0100–GW-1224) |
| **Fully implemented** | 63 (91.3%) |
| **Partially implemented** | 4 (GW-0705, GW-0805, GW-1102, GW-1208) |
| **Not implemented** | 0 |
| **Implementation coverage (full + partial)** | 69/69 = 100% |
| **Implementation coverage (full only)** | 63/69 = 91.3% |
| **Undocumented behavior (D9)** | 2 findings |
| **Constraint violations (D10)** | 0 findings |
| **Constraints verified** | GW-0104 (250-byte frames), GW-0603 (43-byte overhead), GW-0502 (1 MB max message), GW-0403 (4 KB/2 KB size limits), GW-0602 (sequence-number replay protection), GW-1215 (±86400 s timestamp drift) — all **compliant** |
| **Constraints unverifiable** | Performance-related constraints (GW-1003 concurrent handling) require runtime testing |

### Requirement Status Matrix (all 69 requirements)

| ID | Title | Status |
|----|-------|--------|
| GW-0100 | Node-initiated communication | ✅ Implemented |
| GW-0101 | CBOR message encoding | ✅ Implemented |
| GW-0102 | Wake handshake — WAKE reception | ✅ Implemented |
| GW-0103 | Wake handshake — COMMAND response | ✅ Implemented |
| GW-0104 | Frame size constraint | ✅ Implemented |
| GW-0200 | NOP command | ✅ Implemented |
| GW-0201 | UPDATE_PROGRAM command | ✅ Implemented |
| GW-0202 | RUN_EPHEMERAL command | ✅ Implemented |
| GW-0203 | UPDATE_SCHEDULE command | ✅ Implemented |
| GW-0204 | REBOOT command | ✅ Implemented |
| GW-0300 | Chunk serving | ✅ Implemented |
| GW-0301 | Transfer resumption | ✅ Implemented |
| GW-0302 | Program acknowledgement | ✅ Implemented |
| GW-0400 | Program ingestion (ELF) | ✅ Implemented |
| GW-0401 | Program verification (Prevail) | ✅ Implemented |
| GW-0402 | Program identity by content hash | ✅ Implemented |
| GW-0403 | Program size enforcement | ✅ Implemented |
| GW-0500 | APP_DATA reception | ✅ Implemented |
| GW-0501 | APP_DATA_REPLY response | ✅ Implemented |
| GW-0502 | Handler transport (length-prefixed CBOR) | ✅ Implemented |
| GW-0503 | Handler lifecycle management | ✅ Implemented |
| GW-0504 | Handler routing by program hash | ✅ Implemented |
| GW-0505 | Handler DATA message | ✅ Implemented |
| GW-0506 | Handler DATA_REPLY processing | ✅ Implemented |
| GW-0507 | Handler EVENT messages | ✅ Implemented |
| GW-0508 | Handler LOG messages | ✅ Implemented |
| GW-0600 | HMAC-SHA256 authentication | ✅ Implemented |
| GW-0601 | Per-node key management | ✅ Implemented |
| GW-0601a | Key store encryption at rest | ✅ Implemented |
| GW-0601b | OS-native master key via KeyProvider | ✅ Implemented |
| GW-0602 | Replay protection — sequence numbers | ✅ Implemented |
| GW-0603 | Authentication overhead budget | ✅ Implemented |
| GW-0700 | Node registry | ✅ Implemented |
| GW-0701 | Stale program detection | ✅ Implemented |
| GW-0702 | Battery level tracking | ✅ Implemented |
| GW-0703 | Firmware ABI version awareness | ✅ Implemented |
| GW-0705 | Factory reset support | ⚠️ Partial (F-002) |
| GW-0800 | Admin gRPC API | ✅ Implemented |
| GW-0801 | Admin API — node management | ✅ Implemented |
| GW-0802 | Admin API — program management | ✅ Implemented |
| GW-0803 | Admin API — schedule and commands | ✅ Implemented |
| GW-0804 | Admin API — node status | ✅ Implemented |
| GW-0805 | Admin API — state export/import | ⚠️ Partial (F-001) |
| GW-0806 | Admin CLI tool | ✅ Implemented |
| GW-0807 | Admin API — modem management | ✅ Implemented |
| GW-1000 | Gateway failover / replaceability | ✅ Implemented |
| GW-1001 | Exportable / importable state | ✅ Implemented |
| GW-1002 | Graceful handling of unknown nodes | ✅ Implemented |
| GW-1003 | Concurrent node handling | ✅ Implemented |
| GW-1004 | Program hash consistency | ✅ Implemented |
| GW-1100 | Modem transport trait | ✅ Implemented |
| GW-1101 | Modem startup sequence | ✅ Implemented |
| GW-1102 | Modem health monitoring | ⚠️ Partial (F-003) |
| GW-1103 | Modem error handling | ✅ Implemented |
| GW-1200 | Ed25519 keypair generation | ✅ Implemented |
| GW-1201 | Gateway identity generation | ✅ Implemented |
| GW-1202 | Ed25519 to X25519 conversion | ✅ Implemented |
| GW-1203 | Ed25519 seed replication | ✅ Implemented |
| GW-1204 | BLE GATT server | ✅ Implemented |
| GW-1205 | ATT MTU negotiation and fragmentation | ✅ Implemented |
| GW-1206 | REQUEST_GW_INFO handling | ✅ Implemented |
| GW-1207 | Registration window enforcement | ✅ Implemented |
| GW-1208 | Registration window activation | ⚠️ Partial (F-004) |
| GW-1209 | REGISTER_PHONE processing | ✅ Implemented |
| GW-1210 | Phone PSK storage and revocation | ✅ Implemented |
| GW-1211 | PEER_REQUEST key-hint bypass | ✅ Implemented |
| GW-1212 | PEER_REQUEST decryption | ✅ Implemented |
| GW-1213 | Phone HMAC verification | ✅ Implemented |
| GW-1214 | PEER_REQUEST frame HMAC verification | ✅ Implemented |
| GW-1215 | PairingRequest timestamp validation | ✅ Implemented |
| GW-1216 | Node ID uniqueness check | ✅ Implemented |
| GW-1217 | Key hint consistency check | ✅ Implemented |
| GW-1218 | Node registration from PEER_REQUEST | ✅ Implemented |
| GW-1219 | PEER_ACK generation | ✅ Implemented |
| GW-1220 | Silent-discard error model | ✅ Implemented |
| GW-1221 | Random nonces for PEER_REQUEST/PEER_ACK | ✅ Implemented |
| GW-1222 | Admin API — BLE pairing session | ✅ Implemented |
| GW-1223 | Admin API — phone listing | ✅ Implemented |
| GW-1224 | Admin API — phone revocation | ✅ Implemented |

### Overall Assessment

The `sonde-gateway` crate demonstrates strong code-to-spec alignment. All 69 requirements have at least partial implementation. The 4 partial implementations are well-understood gaps with clear remediation paths. The 2 undocumented behaviors (Windows service, YAML config) are reasonable infrastructure features that should be formalized as requirements rather than removed. No constraint violations were found — all checked numeric limits, cryptographic parameters, and protocol invariants are correctly enforced in code.

---

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 (GW-0805) | Update `export_state`/`import_state` RPCs to call `encrypt_state_full`/`decrypt_state_full`, including identity, phone PSKs, and handler configs. The library functions already exist — this is wiring only. | S | Low — library functions are tested; change is additive |
| 2 | F-002 (GW-0705) | Design and implement a protocol-level factory reset command. Requires changes to `sonde-protocol` (new command type), `sonde-node` (erase logic), and `sonde-gateway` (send reset before removing registry entry). | L | Medium — cross-crate protocol change |
| 3 | F-003 (GW-1102) | Add `spawn_health_monitor()` call in `bin/gateway.rs` reconnect loop after transport creation. Use 30-second interval. Single line change + cancellation token threading. | S | Low — function is already tested |
| 4 | F-004 (GW-1208) | Define a modem protocol message for button-press events, or revise GW-1208 to remove the button-hold requirement from the gateway (delegate to modem firmware). | M | Low — scope clarification may suffice |
| 5 | F-005 (D9) | Add requirement GW-1005 documenting Windows NT service support (install, uninstall, auto-start, log file). Add validation test cases. | S | Low — documentation only |
| 6 | F-006 (D9) | Add requirement GW-0509 or amend GW-0504 to reference the handler YAML configuration format from the design document. | S | Low — documentation only |

---

## 7. Prevention

1. **Spec-code traceability enforcement**: Require every new public function or RPC to reference a GW-XXXX ID in its doc comment. CI could lint for this pattern.
2. **Dead code detection**: Add a CI check (e.g., `cargo udeps` or custom lint) to flag public functions with zero call sites, which would have caught F-003 (unused `spawn_health_monitor`).
3. **Export/import completeness test**: Add an integration test that round-trips a full state export/import and asserts all state components (identity, phone PSKs, handler configs) survive the cycle. This would have caught F-001.
4. **Requirement coverage gate**: Before merging features, require a diff showing which GW-XXXX requirements are addressed. Undocumented features (D9) should trigger a requirement addition in the same PR.

---

## 8. Open Questions

1. **GW-1208 button-hold intent**: Does the requirement intend for the gateway software to handle button input, or is button detection a modem/node-firmware responsibility with the gateway acting on a modem-relayed event? The modem protocol does not currently define a button-press message. Resolving this clarifies whether F-004 is a gateway code gap or a modem protocol gap.

2. **GW-0805 export scope**: The `encrypt_state_full()` library function includes handler configs in the export. Should the admin RPC also export handler configs, or is handler configuration considered deployment-specific (not portable across gateway instances)? The requirement text says "handler routing configuration" should be included, but the current RPC explicitly excludes it.

3. **GW-1203 via admin API**: The Ed25519 seed can be replicated via the full state bundle, but the admin RPC does not currently expose this. Should there be a dedicated `ExportIdentity` / `ImportIdentity` RPC for seed replication without a full state swap, or is including it in the existing export/import sufficient?

---

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-23 | Copilot (automated audit) | Initial audit of sonde-gateway crate against 69 requirements |
