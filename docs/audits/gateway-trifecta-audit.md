<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Specification Trifecta Audit — Investigation Report

## 1. Executive Summary

A systematic traceability audit of the Sonde gateway specification trifecta (76 requirements, design document, 112 test cases) reveals **moderate specification integrity** with two systemic gaps. First, 23 BLE pairing requirements (GW-1200–GW-1222) have **no design realization** in `gateway-design.md` — the design doc does not describe how the gateway processes any BLE pairing protocol messages. Second, 9 admin API and key-protection requirements are **functionally addressed** in design sections 10a and 13 but **not traced by requirement ID**, creating broken traceability links. Five requirements (GW-0601a, GW-0806, GW-1203, GW-1204, GW-1205) have **no concrete test case** — the validation plan defers them to unspecified external tests. Forward traceability to validation is strong at 93%, but design traceability is only 58% explicit (70% including implicit coverage). **Recommended action:** Add a BLE pairing design section (§17), insert explicit GW-ID cross-references in sections 10a and 13, and define concrete test cases for the 5 deferred requirements.

## 2. Problem Statement

**What was observed:** Cross-document specification drift between `gateway-requirements.md`, `gateway-design.md`, and `gateway-validation.md`.

**Expected behavior:** Every requirement should trace forward to both a design section (explaining *how*) and at least one test case (verifying *that*). Every design element should trace backward to a requirement. Every test case should trace to a valid requirement.

**Impact:** Untraced requirements risk silent omission during implementation. Requirements with no test specification risk unverified deployment. Implicit design coverage without ID references breaks automated traceability tooling and makes coverage audits unreliable.

## 3. Investigation Scope

- **Documents examined:**
  - `docs/gateway-requirements.md` — 76 requirements (GW-0100 through GW-1222, including sub-IDs GW-0601a, GW-0601b)
  - `docs/gateway-design.md` — 16 top-level sections covering module architecture, transport, codec, sessions, registry, programs, handlers, storage, admin API, configuration, startup, and shutdown
  - `docs/gateway-validation.md` — 112 test cases (T-0100 through T-1222, including sub-IDs T-0603a through T-0603k)
- **Tools used:** Regex-based identifier extraction, cross-document reference matching, manual semantic analysis
- **Limitations:**
  - Semantic verification of acceptance-criteria-to-test-case alignment was performed on a sample basis, not exhaustively for all 76 requirements.
  - The separate `ble-pairing-protocol.md` and `ble-pairing-tool-design.md` documents were not included in this audit — only the gateway trifecta was assessed.
  - Implementation code was not examined. This audit covers specification-level drift only.

## 4. Findings

### Finding F-001: 23 BLE Pairing Requirements Have No Design Realization

- **Severity**: High
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**: `gateway-requirements.md` §12 (GW-1200–GW-1222); `gateway-design.md` (entire document)
- **Description**: Requirements GW-1200 through GW-1222 define 23 Must-priority requirements covering Ed25519 keypair generation, gateway identity, BLE GATT server operation, phone registration, PEER_REQUEST processing, timestamp validation, and node provisioning via BLE. The gateway design document contains **no dedicated section** addressing how the gateway processes these BLE pairing protocol messages. The only BLE pairing reference in the design doc is a parenthetical note at line 663: *"(In the BLE pairing flow, node registration happens automatically via `PEER_REQUEST` processing — see ble-pairing-protocol.md §7.3.)"* — which is a cross-reference, not a design realization.
- **Evidence**:
  - Requirements GW-1200–GW-1222 (23 items) in `gateway-requirements.md` §12
  - `gateway-design.md` section headings: §1–§16. None is titled or dedicated to BLE pairing, phone registration, PEER_REQUEST handling, or gateway identity management.
  - `grep -c "GW-12" gateway-design.md` returns 0 matches for explicit GW-12xx requirement IDs.
  - The design doc mentions Ed25519 and phone PSKs only in §10a (master key provider context), not as a design section.
- **Root Cause**: The BLE pairing requirements were likely added to `gateway-requirements.md` after the design document was finalized. The protocol-level design exists in `ble-pairing-protocol.md`, but the gateway-specific design realization (which modules handle which messages, state machine, storage interactions) was never added to `gateway-design.md`.
- **Impact**: Implementers consulting `gateway-design.md` will find no guidance on how to build BLE pairing support. The 23 requirements have no architectural decomposition — it is unclear which module (Session Manager? Node Registry? A new BLE module?) should handle each message type.
- **Remediation**: Add a new section (e.g., §17 "BLE Pairing Protocol Handler") to `gateway-design.md` that:
  1. Describes which module handles each BLE pairing message type
  2. Defines the state machine for the pairing flow (gateway side)
  3. Explains storage interactions (Ed25519 seed, phone PSKs, node registration)
  4. References GW-1200–GW-1222 explicitly
- **Confidence**: High — verified by exhaustive section-heading review and full-text search of the design document.

---

### Finding F-002: Admin API Design Section Missing Requirement ID References

- **Severity**: Medium
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**: `gateway-requirements.md` §9 (GW-0800–GW-0806); `gateway-design.md` §13
- **Description**: Requirements GW-0800 through GW-0806 define the admin gRPC API, node management, program management, operational commands, node status, state export/import, and the CLI tool. Design section 13 ("Admin API") **functionally addresses** all of these — it defines the gRPC service proto, key operations, and CLI commands. However, section 13 contains **zero** explicit GW-08xx requirement ID references. The module responsibility table in §3.1 does not list the admin API module at all.
- **Evidence**:
  - `grep "GW-080" gateway-design.md` returns 0 matches.
  - Design §13 describes `GatewayAdmin` gRPC service with methods mapping to GW-0801 (RegisterNode, RemoveNode), GW-0802 (IngestProgram), GW-0803 (AssignProgram, QueueReboot, QueueEphemeral), GW-0804 (GetNodeStatus), GW-0805 (ExportState, ImportState), GW-0806 (CLI tool at §13.3).
  - Module table at §3.1 (line 66–78) lists 8 modules; none is "Admin API" or references GW-08xx.
- **Root Cause**: The admin API section was written as an architectural description without systematic traceability. The module table was not updated to include the admin module.
- **Impact**: Automated traceability tools will report GW-0800–GW-0806 as untraced. Manual auditors cannot verify coverage without reading §13 in full and mentally mapping methods to requirements.
- **Remediation**:
  1. Add `GW-0800` through `GW-0806` references to §13 (e.g., as a mapping table or inline annotations).
  2. Add an "Admin API" row to the module responsibility table in §3.1.
- **Confidence**: High

---

### Finding F-003: GW-0601b and GW-1100 Implicitly Covered but Not Traced

- **Severity**: Low
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**: `gateway-requirements.md` (GW-0601b, GW-1100); `gateway-design.md` §10a, §4.2
- **Description**: Two requirements are functionally addressed by design sections but not referenced by their requirement IDs:
  - **GW-0601b** (OS-native master key protection via `KeyProvider`): Design §10a describes the `KeyProvider` trait, DPAPI backend, Secret Service backend, `EnvKeyProvider`, `FileKeyProvider` — which is the exact design realization of GW-0601b. But the ID "GW-0601b" does not appear in the design doc.
  - **GW-1100** (USB modem transport trait implementation): Design §4.2 describes `UsbEspNowTransport` in detail — recv/send behavior, serial protocol, demux architecture. But "GW-1100" does not appear in the design doc. The module table references only GW-1101, GW-1102, GW-1103.
- **Evidence**:
  - `grep "GW-0601b" gateway-design.md` returns 0 matches; §10a.1–10a.6 describes the KeyProvider system.
  - `grep "GW-1100" gateway-design.md` returns 0 matches; §4.2 describes UsbEspNowTransport.
- **Root Cause**: These are sub-requirements or top-level umbrella requirements whose IDs were omitted from design cross-references while the child/detail requirements were referenced.
- **Impact**: Minor traceability gap. The design coverage is real but not discoverable via ID search.
- **Remediation**: Add "GW-0601b" reference to §10a header and "GW-1100" to §4.2 header or the module table.
- **Confidence**: High

---

### Finding F-004: Five Requirements Deferred Without Concrete Test Cases

- **Severity**: High
- **Category**: D2_UNTESTED_REQUIREMENT
- **Location**: `gateway-validation.md` traceability matrix (final section)
- **Description**: Five requirements have **no T-NNNN test case** defined in the validation plan. Instead, the traceability matrix contains italicized deferral notes:

  | Requirement | Priority | Deferral Note |
  |---|---|---|
  | GW-0601a (Key store encryption at rest) | Should | *"verified by storage implementation tests"* |
  | GW-0806 (Admin CLI tool) | Must | *"validated by CLI integration tests against a running gateway"* |
  | GW-1203 (Ed25519 seed replication for failover) | Must | *"validated by seed export/import integration tests"* |
  | GW-1204 (BLE GATT server) | Must | *"validated by BLE integration tests against physical hardware"* |
  | GW-1205 (ATT MTU negotiation and fragmentation) | Must | *"validated by BLE integration tests against physical hardware"* |

  Four of these are **Must** priority. The deferral notes reference tests that do not exist as specified test cases in this document.
- **Evidence**: Traceability matrix entries for GW-0601a, GW-0806, GW-1203, GW-1204, GW-1205 contain italicized prose instead of T-NNNN identifiers.
- **Root Cause**: These requirements involve either external tooling (CLI), physical hardware (BLE), or cross-cutting concerns (encryption at rest) that are harder to test in the mock-transport test harness described in §2.
- **Impact**: These 5 requirements will not be verified by the validation plan. Implementers have no test specification to write against. The 93% test coverage rate is slightly inflated — actual concrete coverage is 71/76 = 93% but the 5 gaps include 4 Must-priority requirements.
- **Remediation**: Define concrete test cases for each:
  - **GW-0601a**: T-0603l — Write a PSK via storage, read raw DB, assert PSK bytes are not plaintext.
  - **GW-0806**: T-0811 — Invoke each CLI command against a test gateway, verify output and side effects.
  - **GW-1203**: T-1223 — Export Ed25519 seed from gateway A, import into gateway B, verify both produce identical GW_INFO_RESPONSE signatures.
  - **GW-1204/GW-1205**: T-1224/T-1225 — Define mock BLE transport tests or document hardware-in-the-loop test procedure with pass/fail criteria.
- **Confidence**: High

---

### Finding F-005: Requirement Numbering Gap — GW-0704 Missing

- **Severity**: Informational
- **Category**: D5_ASSUMPTION_DRIFT (numbering inconsistency)
- **Location**: `gateway-requirements.md` §8 (between GW-0703 and GW-0705)
- **Description**: The node registry requirement series goes GW-0700, GW-0701, GW-0702, GW-0703, GW-0705. GW-0704 is absent. This may indicate a requirement that was removed or renumbered without updating the sequence.
- **Evidence**: `grep "GW-0704" gateway-requirements.md` returns 0 matches.
- **Root Cause**: [UNKNOWN: Whether GW-0704 was intentionally removed, deferred, or never existed.]
- **Impact**: Minimal — numbering gaps are cosmetic. However, if GW-0704 was removed, any downstream references to it in other documents would be orphaned.
- **Remediation**: Either assign GW-0704 to a new requirement, or add a comment noting the intentional gap.
- **Confidence**: High (the gap exists; the reason is unknown)

---

### Finding F-006: Design Module Table Incomplete — Missing Admin API and BLE Pairing Modules

- **Severity**: Medium
- **Category**: D3_ORPHANED_DESIGN_DECISION (inverse — missing module entries)
- **Location**: `gateway-design.md` §3.1 (lines 66–78)
- **Description**: The module responsibility table in §3.1 lists 8 modules (Transport, Protocol Codec, Session Manager, Node Registry, Program Library, Handler Router, Handler Process, Storage). Two significant functional areas present in the design are absent from this table:
  1. **Admin API** (described in §13) — no module entry, no GW-08xx references
  2. **BLE Pairing Handler** (not described anywhere) — no module entry
  
  Additionally, the USB modem adapter (§4.2) is described as part of the Transport module but its specific requirement (GW-1100) is not listed in the Transport module's "Requirements covered" column.
- **Evidence**: Module table contains exactly 8 rows. §13 exists but is not represented. BLE pairing has 23 requirements but no module.
- **Root Cause**: The module table was created before the admin API and BLE pairing requirements were finalized.
- **Impact**: The module table is the primary architectural map. Its incompleteness means developers cannot use it to find which module implements which requirement for admin and BLE pairing functionality.
- **Remediation**: Add rows for "Admin API" (GW-0800–0806) and "BLE Pairing Handler" (GW-1200–1222) to the module table. Add GW-1100 to the Transport module's coverage list.
- **Confidence**: High

---

### Finding F-007: Validation Traceability Matrix References Non-Standard Sub-ID GW-0601b

- **Severity**: Informational
- **Category**: D5_ASSUMPTION_DRIFT (ID convention inconsistency)
- **Location**: `gateway-requirements.md` (GW-0601a, GW-0601b); `gateway-validation.md` (T-0603a through T-0603k)
- **Description**: The requirements document uses alphanumeric sub-IDs (GW-0601a, GW-0601b) which deviate from the standard 4-digit numeric pattern (GW-NNNN) described in §2. The validation plan follows this convention with sub-test-case IDs (T-0603a through T-0603k). While internally consistent, this is a departure from the stated format that could confuse automated tooling expecting strict `GW-\d{4}` patterns.
- **Evidence**: §2 defines format as `GW-XXXX` with no mention of alphabetic suffixes. GW-0601a and GW-0601b exist in the requirements. T-0603a through T-0603k exist in validation.
- **Root Cause**: Sub-requirements were added after the numbering scheme was established, and alphabetic suffixes were used to avoid renumbering.
- **Impact**: Low — the convention is internally consistent but undocumented.
- **Remediation**: Either document the sub-ID convention in §2, or renumber to GW-0604/GW-0605 (and update validation references).
- **Confidence**: High

## 5. Root Cause Analysis

The findings cluster into two root causes:

### Root Cause 1: Late-Added Requirement Blocks Without Design Updates

GW-0800–GW-0806 (admin API) and GW-1200–GW-1222 (BLE pairing) appear to have been added to the requirements document after the design document was substantially complete. The design doc *does* address admin API functionality (§13) but without explicit requirement ID tracing. BLE pairing has no design coverage at all — the protocol-level design exists in `ble-pairing-protocol.md` but the gateway-specific design realization was never written.

**Causal chain:** New requirements added → design doc not updated with ID references → design doc not extended with new sections → traceability broken.

### Root Cause 2: Deferral of Hardware-Dependent and Cross-Cutting Tests

Five requirements involve testing conditions that fall outside the mock-transport test harness (BLE hardware, CLI integration, encryption internals). Rather than defining test procedures with explicit pass/fail criteria, the validation plan defers them with prose notes. This creates a false sense of coverage in the traceability matrix.

### Coverage Metrics

| Metric | Value |
|---|---|
| Total requirements | 76 |
| Total test cases | 112 |
| Requirements → Design (explicit ID trace) | 44/76 (58%) |
| Requirements → Design (including implicit coverage) | 53/76 (70%) |
| Requirements → Design (completely missing) | 23/76 (30%) |
| Requirements → Validation (concrete test cases) | 71/76 (93%) |
| Requirements → Validation (deferred, no T-NNNN) | 5/76 (7%) |
| Backward: Test cases → valid requirement | 112/112 (100%) |
| Backward: Design sections → requirements | 14/16 sections traced (88%) |
| Orphaned test cases | 0 |
| Numbering gaps | 1 (GW-0704) |

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 | Add §17 "BLE Pairing Protocol Handler" to `gateway-design.md` covering GW-1200–GW-1222 | L | Med — requires understanding ble-pairing-protocol.md decomposition into gateway modules |
| 2 | F-004 | Define 5 concrete test cases (T-0603l, T-0811, T-1223, T-1224, T-1225) for deferred requirements | M | Low |
| 3 | F-002 | Add GW-0800–GW-0806 ID references to design §13 and add Admin API row to §3.1 module table | S | Low |
| 4 | F-006 | Update module table in §3.1 with Admin API and BLE Pairing modules | S | Low |
| 5 | F-003 | Add GW-0601b and GW-1100 ID references to design §10a and §4.2 | S | Low |
| 6 | F-005 | Document GW-0704 gap or assign new requirement | S | Low |
| 7 | F-007 | Document sub-ID convention in requirements §2 | S | Low |

## 7. Prevention

- **Process**: When adding new requirement blocks to `*-requirements.md`, create a corresponding design section stub in `*-design.md` and at least one test case stub in `*-validation.md` in the same PR.
- **Tooling**: Add a CI check that extracts all `GW-NNNN` IDs from `gateway-requirements.md` and verifies each appears at least once in `gateway-design.md` and `gateway-validation.md`. Flag IDs that appear only in deferral notes (italicized text) separately from concrete test case mappings.
- **Code review checklist**: When reviewing PRs that modify requirements, check: (1) design doc updated? (2) validation plan updated? (3) module table updated? (4) traceability matrix updated?

## 8. Open Questions

1. **Was GW-0704 intentionally omitted?** If it was a removed requirement, was it ever referenced in design or validation documents? A `git log` search for GW-0704 would resolve this.

2. **Should BLE pairing design live in `gateway-design.md` or a separate doc?** Given that `ble-pairing-protocol.md` already describes the protocol and `ble-pairing-tool-design.md` covers the client tool, the gateway-side design could either be a new section in `gateway-design.md` or a new `ble-pairing-gateway-design.md`. The former is recommended for consistency with the trifecta pattern.

3. **Are the 5 deferred test cases tested elsewhere?** The deferral notes reference "storage implementation tests," "CLI integration tests," and "BLE integration tests." If these tests exist in code but not in the validation specification, they should be retroactively documented as test cases.

4. **Acceptance criteria semantic verification**: A sample-based check of test case procedures against requirement acceptance criteria was performed for T-0500/GW-0505 and T-0105/GW-0103 (both passed). A full exhaustive semantic verification of all 71 concrete test cases was not performed. A follow-up audit could check for D7_ACCEPTANCE_CRITERIA_MISMATCH findings.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-20 | Copilot (specification analyst) | Initial audit |
