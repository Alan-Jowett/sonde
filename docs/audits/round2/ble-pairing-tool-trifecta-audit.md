<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# BLE Pairing Tool Specification Trifecta Audit — Investigation Report

## 1. Executive Summary

A trifecta audit of the BLE pairing tool specification set (requirements, design, validation) examined all 55 requirements (PT-0100 through PT-1214) for cross-document traceability, consistency, and completeness. The specification set is in strong health: 98.2% of requirements have validation coverage, 100% are substantively addressed in the design, and no constraint violations (D6) were found. Six findings were identified — one medium-severity untested requirement (PT-1213, build-type–aware log levels), one medium-severity assumption drift (Just Works pairing permitted for node BLE connections without explicit requirements justification), one medium-severity acceptance-criteria mismatch, two low-severity traceability table gaps, and one low-severity orphaned design decision. Recommended action: add a test case for PT-1213, clarify the node BLE pairing-method policy in the requirements, and update the design traceability table.

## 2. Problem Statement

The BLE pairing tool specification set consists of three documents that must be internally consistent and mutually traceable:

- **Requirements** (`ble-pairing-tool-requirements.md`): 55 requirements (PT-0100 through PT-1214)
- **Design** (`ble-pairing-tool-design.md`): architecture, module design, and platform-specific considerations
- **Validation** (`ble-pairing-tool-validation.md`): test cases (T-PT-100 through T-PT-1214b)

This audit systematically checks for specification drift — gaps, conflicts, and divergence — across all three documents, using the D1–D7 drift taxonomy.

## 3. Investigation Scope

- **Documents examined:**
  - `docs/ble-pairing-tool-requirements.md` (55 requirements, sections 3–15)
  - `docs/ble-pairing-tool-design.md` (15 sections including traceability table §15)
  - `docs/ble-pairing-tool-validation.md` (12 sections, 70+ test cases, Appendix A traceability matrix)
- **Focus areas:** All requirements PT-0100 through PT-1214
- **Tools used:** Manual cross-document traceability analysis, systematic identifier enumeration
- **Limitations:** This audit examines specification-level consistency only. Source code compliance (D8–D10) and test code compliance (D11–D13) are out of scope. Domain correctness of individual requirements is not assessed.

### 3.1 Artifact Inventory

**Requirements (55 total):**

| Category | IDs | Count |
|---|---|---|
| Platform & architecture | PT-0100–PT-0108 | 9 |
| Device discovery | PT-0200–PT-0202 | 3 |
| Phase 1 — Gateway pairing | PT-0300–PT-0304 | 5 |
| Phase 2 — Node provisioning | PT-0400–PT-0408 | 9 |
| Error handling | PT-0500–PT-0502 | 3 |
| Idempotency & safety | PT-0600–PT-0601 | 2 |
| User experience | PT-0700–PT-0702 | 3 |
| Persistence & local state | PT-0800–PT-0804 | 5 |
| Security | PT-0900–PT-0904 | 5 |
| Non-functional | PT-1000–PT-1004 | 5 |
| Cryptographic | PT-1100–PT-1103 | 4 |
| Testing (meta) | PT-1200–PT-1206 | 7 (meta-requirements specifying what tests must exist) |
| Diagnostic logging | PT-1207–PT-1214 | 8 |

**Design document:** 15 sections, explicit traceability table in §15 mapping requirements to design sections.

**Validation plan:** 70+ test cases (T-PT-100 through T-PT-1214b), meta-requirement traceability table in §1, Appendix A traceability matrix.

## 4. Findings

### Finding F-001: PT-1213 has no test case

- **Severity**: Medium
- **Category**: D2_UNTESTED_REQUIREMENT
- **Location**: Requirements §15 PT-1213; Validation plan §1 meta-requirements table, §12, and Appendix A (absent)
- **Description**: PT-1213 (Build-type–aware log levels) specifies six acceptance criteria covering compile-time gating of `trace!`/`debug!` call-sites in release builds, runtime default `EnvFilter` differences between debug and release, `RUST_LOG` override behavior, and `tracing` Cargo feature configuration. No test case (e.g., `T-PT-1213`) exists in the validation plan. PT-1213 is absent from both the meta-requirements traceability table (§1) and Appendix A.
- **Evidence**:
  - PT-1213 defines 6 acceptance criteria (requirements doc, §15, lines 1126–1136).
  - The validation plan's meta-requirements table (§1, lines 22–38) lists PT-1000–PT-1206 but omits PT-1213.
  - Appendix A (validation doc, lines 1219–1308) contains no `T-PT-1213` entry.
  - The validation plan's §12 (Diagnostic logging tests) covers T-PT-1207 through T-PT-1212 and T-PT-1214a/b, skipping PT-1213.
- **Root Cause**: PT-1213 was likely added to the requirements after the initial validation plan was written (it references issue #496) and was not back-ported to the validation plan.
- **Impact**: The compile-time gating and runtime log-level defaults will not be verified by the test suite. A misconfiguration (e.g., missing `release_max_level_info` feature) would ship DEBUG/TRACE call-sites in release builds, increasing binary size and potentially exposing diagnostic information in production.
- **Remediation**: Add test case `T-PT-1213` to the validation plan §12 with procedures to verify:
  1. In a release build, `debug!` and `trace!` macros are compile-time no-ops (verify via `tracing` feature flags in `Cargo.toml`).
  2. The default `EnvFilter` is `sonde_pair=warn,sonde_pair_ui=warn` in release and `sonde_pair=info,sonde_pair_ui=info` in debug.
  3. `RUST_LOG` overrides the default within compile-time limits.
  Add PT-1213 to the meta-requirements traceability table and Appendix A.
- **Confidence**: High

---

### Finding F-002: Design assumes Just Works is acceptable for node BLE connections

- **Severity**: Medium
- **Category**: D5_ASSUMPTION_DRIFT
- **Location**: Design §5.1 `BleTransport` trait documentation (line 300–302); Requirements PT-0401 (§6), PT-0904 (§11), PT-0106 (§3)
- **Description**: The design document (§5.1, `connect()` doc comment) explicitly states: "Just Works is acceptable only for node provisioning connections." The requirements are silent on the BLE association model for node connections — PT-0401 mandates "accept BLE LESC pairing" but does not specify whether Numeric Comparison is required (unlike PT-0300 and PT-0904, which explicitly require Numeric Comparison for modem/gateway connections). This is a security-relevant design assumption that extends the requirements: the `NODE_PROVISION` body carries `node_psk` (32 bytes) in plaintext over the BLE link, so a Just Works connection (no MITM protection) could expose the node PSK to an active attacker within BLE range.
- **Evidence**:
  - Design §5.1 `connect()` comment: "Numeric Comparison is required for gateway connections (PT-0300) — a Just Works fallback MUST be treated as a connection failure. Just Works is acceptable only for node provisioning connections."
  - PT-0401 (requirements, §6): "the tool MUST connect, negotiate ATT MTU ≥ 247, and accept BLE LESC pairing" — no association model specified.
  - PT-0904 scope is "The BLE link between the phone and modem" — modem connections only.
  - PT-0106 scope is "BLE connections to the modem" — modem connections only.
  - NODE_PROVISION wire format (design §4.1): `node_key_hint[2] ‖ node_psk[32] ‖ rf_channel[1] ‖ …` — node_psk is plaintext.
- **Root Cause**: The requirements address BLE pairing method enforcement for modem/gateway connections (PT-0106, PT-0904) but are silent on the association model for node provisioning connections. The design fills this gap with an explicit policy decision that is not traced to a requirement.
- **Impact**: If the design intention is correct (Just Works acceptable for nodes), the requirements should document this policy and its security rationale. If Numeric Comparison should be required for all connections, the design needs correction and additional test coverage.
- **Remediation**: Add a requirement statement (or a note in PT-0401) that explicitly specifies the permitted BLE pairing method for node provisioning connections and documents the security rationale. If Just Works is acceptable, document that the encrypted_payload provides defense-in-depth and that node provisioning is a physically-proximate operation.
- **Confidence**: High

---

### Finding F-003: T-PT-502 does not test the cancel path for PT-0601

- **Severity**: Medium
- **Category**: D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location**: Requirements §8 PT-0601 AC 2; Validation §7 T-PT-502 (lines 796–804)
- **Description**: PT-0601 acceptance criterion 2 states: "The operator can choose to proceed **or cancel**." Test case T-PT-502 only exercises the "proceed" path — it simulates the operator choosing to proceed and asserts Phase 1 continues. The "cancel" path (operator declines to re-pair, Phase 1 aborts without error) is not tested.
- **Evidence**:
  - PT-0601 AC 2 (requirements, §8): "The operator can choose to proceed or cancel."
  - T-PT-502 procedure (validation, §7): steps 4–5 test "proceed" only: "Simulate operator choosing to proceed. Assert: Phase 1 continues normally." No step tests the cancel scenario.
- **Root Cause**: The test case was written to verify the primary path (proceed) but omitted the alternative path (cancel).
- **Impact**: The cancel path creates an illusory sense of coverage — the traceability matrix shows PT-0601 as tested, but one of its two acceptance criteria is not verified. A bug in the cancel flow (e.g., partial state persisted, BLE connection leaked) would not be caught.
- **Remediation**: Add steps to T-PT-502 (or create T-PT-502a) that simulate the operator choosing to cancel after the already-paired warning. Assert: Phase 1 does not proceed, no state changes occur, and the BLE connection is not initiated.
- **Confidence**: High

---

### Finding F-004: PT-1213 missing from design traceability table

- **Severity**: Low
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**: Requirements §15 PT-1213; Design §14.1 (substantive content) and §15 (traceability table, absent)
- **Description**: PT-1213 (Build-type–aware log levels) is substantively addressed in design §14.1, which describes compile-time gating, default `EnvFilter` configuration, `RUST_LOG` override behavior, and the required `tracing` Cargo features. However, PT-1213 is not listed in the design's §15 traceability table. The table's §14 entry covers "PT-0702, PT-0900, PT-1207–PT-1212" — stopping at PT-1212.
- **Evidence**:
  - Design §15 (line 976): "§14 Diagnostic logging | PT-0702, PT-0900, PT-1207–PT-1212" — PT-1213 absent.
  - Design §14.1 (lines 906–931): contains substantive content covering all PT-1213 acceptance criteria (compile-time gating, default filter, RUST_LOG, tracing features).
- **Root Cause**: PT-1213 was added to the requirements after the design traceability table was last updated.
- **Impact**: Low — the design content exists and is correct; only the formal traceability entry is missing. An implementer searching the traceability table for PT-1213 would not find it, but searching the document body would.
- **Remediation**: Add PT-1213 to the §15 traceability table under §14 Diagnostic logging.
- **Confidence**: High

---

### Finding F-005: PT-1214 missing from design traceability table

- **Severity**: Low
- **Category**: D1_UNTRACED_REQUIREMENT
- **Location**: Requirements §15 PT-1214; Design §4.1 (substantive content) and §15 (traceability table, absent)
- **Description**: PT-1214 (Board pin configuration in NODE_PROVISION) is substantively addressed in design §4.1, which describes the `pin_config_cbor` trailing field in the NODE_PROVISION wire format, including the CBOR key mapping and backward compatibility. However, PT-1214 is not listed in the design's §15 traceability table.
- **Evidence**:
  - Design §15 (lines 962–976): PT-1214 does not appear in any row of the traceability table.
  - Design §4.1 (lines 233–254): describes `pin_config_cbor` field with CBOR keys, types, and defaults matching PT-1214 acceptance criteria.
- **Root Cause**: PT-1214 was added to the requirements after the design traceability table was last updated.
- **Impact**: Low — identical to F-004. The design content exists; only the formal table entry is missing.
- **Remediation**: Add PT-1214 to the §15 traceability table under §4 Architecture.
- **Confidence**: High

---

### Finding F-006: Linux Secret Service storage backend has no originating requirement

- **Severity**: Low
- **Category**: D3_ORPHANED_DESIGN_DECISION
- **Location**: Design §7.3 "Linux (`FilePairingStore` + `SecretServicePskProtector`)" (lines 565–571)
- **Description**: The design document describes a Linux storage backend with Secret Service keyring integration (`SecretServicePskProtector`) for protecting the phone PSK via D-Bus. This does not trace to any requirement: PT-0100 scopes the initial release to Windows and Android only, and PT-0801 acceptance criteria list Android Keystore and Windows `%APPDATA%` — Linux is not mentioned. The Linux backend is likely intentional preparation for future platform support (consistent with PT-0100's stipulation "The design MUST NOT preclude adding iOS later"), but it represents design scope beyond the stated requirements.
- **Evidence**:
  - PT-0100 (requirements, §3): "The initial release MUST support Windows (desktop, native Bluetooth stack) and Android (physical devices, Android BLE API)."
  - PT-0801 AC (requirements, §10): "On Android, phone_psk is protected by the Android Keystore or equivalent. On Windows, phone_psk is stored in %APPDATA%\sonde\ with restricted file permissions." No Linux criterion.
  - Design §7.3 (lines 565–571): describes `SecretServicePskProtector` for Linux with GNOME Keyring / KWallet integration, enabled via the `secret-service-store` Cargo feature.
- **Root Cause**: The design proactively extends platform support beyond the initial release scope.
- **Impact**: Low — the Linux backend is behind a feature flag and does not affect Windows or Android. However, it consumes design and implementation effort not justified by a requirement.
- **Remediation**: Either add a requirement for Linux support (e.g., update PT-0100 or create PT-0109) or annotate the design section as forward-looking with no current requirement. No code changes needed.
- **Confidence**: High

---

## 5. Root Cause Analysis

### Coverage Metrics

**Forward traceability — Requirements → Design:**

| Metric | Value |
|---|---|
| Total requirements | 55 |
| Substantively addressed in design | 55 (100%) |
| Formally traced in §15 traceability table | 53 (96.4%) |
| Missing from traceability table | 2 (PT-1213, PT-1214) |

**Forward traceability — Requirements → Validation:**

| Metric | Value |
|---|---|
| Total requirements | 55 |
| Requirements with direct test cases | 47 (85.5%) |
| Requirements with structural/CI/infrastructure coverage | 7 (PT-0100–PT-0104, PT-1100, PT-1200) |
| Requirements with no validation coverage | 1 (PT-1213) |
| Effective validation coverage | 54/55 (98.2%) |

**Backward traceability — Validation → Requirements:**

| Metric | Value |
|---|---|
| Test cases in Appendix A | 70+ |
| Test cases mapping to valid requirement IDs | 100% |
| Orphaned test cases (D4) | 0 |

**Acceptance criteria coverage:**

| Metric | Value |
|---|---|
| Requirements with all ACs covered by test steps | 53/55 |
| Requirements with partial AC coverage (D7) | 1 (PT-0601 — cancel path untested) |
| Requirements with no AC coverage | 1 (PT-1213 — no test case) |

**Assumption consistency:**

| Metric | Value |
|---|---|
| Assumptions aligned across documents | High (terminology, constraints, protocols) |
| Assumptions extended by design (D5) | 1 (Just Works for node connections) |
| Constraint violations (D6) | 0 |

**Overall assessment:** High confidence — the specification set has strong traceability and consistency. The six findings are localized gaps, not systemic issues. All critical and high-priority security and cryptographic requirements are fully traced and tested. The one untested requirement (PT-1213) is important but addresses build configuration, not runtime security. The assumption drift finding (F-002) warrants requirements clarification but does not indicate a design error.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-001 (D2) | Add T-PT-1213 test case to validation plan §12; add PT-1213 to meta-requirements table and Appendix A | S | Low |
| 2 | F-002 (D5) | Add explicit statement in PT-0401 (or new requirement) specifying the permitted BLE pairing method for node connections with security rationale | S | Low |
| 3 | F-003 (D7) | Add cancel-path steps to T-PT-502 or create T-PT-502a | S | Low |
| 4 | F-004 (D1) | Add PT-1213 to design §15 traceability table under §14 | S | None |
| 5 | F-005 (D1) | Add PT-1214 to design §15 traceability table under §4 | S | None |
| 6 | F-006 (D3) | Add Linux requirement or annotate design §7.3 as forward-looking | S | None |

## 7. Prevention

- **Process:** When a new requirement is added (e.g., PT-1213 from issue #496), update the design traceability table and validation plan in the same commit or PR to prevent traceability gaps.
- **Checklist:** Add a PR review checklist item: "If a new PT-XXXX requirement is introduced, verify it appears in (1) design §15 traceability table, (2) validation plan meta-requirements table or Appendix A, and (3) at least one test case."
- **Tooling:** Consider a CI script that extracts all PT-XXXX IDs from the requirements doc and checks they appear in the design traceability table and validation plan Appendix A.

## 8. Open Questions

1. **Node BLE pairing method policy (F-002):** Should Numeric Comparison be required for node provisioning connections (matching the gateway policy), or is Just Works acceptable given the physical-proximity threat model? This requires a product/security decision. The current design permits Just Works; the requirements are silent.
2. **PT-1213 test implementation:** Should T-PT-1213 be a CI test that inspects `Cargo.toml` features and default filter constants, or a runtime test that captures tracing output in debug vs. release configurations? The former is simpler; the latter provides stronger assurance.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-18 | Specification audit (Copilot) | Initial audit of all 55 requirements (PT-0100–PT-1214) |
