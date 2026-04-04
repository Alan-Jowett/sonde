<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Post-AEAD Migration Maintenance Audit

> **Date:** 2026-07-21
> **Auditor:** Copilot (automated)
> **Scope:** Full workspace — all documentation, code, and dependencies
> **Trigger:** Completion of AEAD migration (PRs #624–#629), ~15 000 lines of HMAC/ECDH code removed

---

## 1  Executive Summary

The AEAD migration (issue #495) successfully replaced HMAC-SHA256 frame authentication with AES-256-GCM across all runtime code paths. However, **documentation and prompt artifacts were not uniformly updated**. This audit found **16 findings** across the workspace:

| Severity | Count |
|----------|-------|
| High     | 3     |
| Medium   | 8     |
| Low      | 5     |

The highest-impact issues are:

1. **`copilot-instructions.md`** (which governs all AI-assisted development) still describes the protocol wire format as "CBOR payload + 32-byte HMAC-SHA256" and lists `HmacProvider` as a platform trait — directly misleading every future PR.
2. **Three prompt workflow files** (`bootstrap.md`, `maintain.md`, `evolve.md`) and the **PR review workflow** still describe HMAC-SHA256 as the frame authentication mechanism.
3. **Gateway `Cargo.toml`** retains the `hmac`, `hkdf`, `ed25519-dalek`, and `x25519-dalek` dependencies. `hmac` is transitively used only by `pbkdf2_hmac` (state-bundle passphrase derivation) — the direct crate dependency is unnecessary. `hkdf` is completely unused. `x25519-dalek` is used only in `gateway_identity.rs` for Ed25519→X25519 conversion, which is RETIRED per the requirements.
4. **Round-2 audit documents** for gateway, node, and protocol crate still describe HMAC-SHA256 as the current implementation status.
5. **Node validation traceability table** references test functions that no longer exist (e.g., `t_e2e_002_hmac_round_trip`, `test_invalid_hmac_discarded`, `peer_ack_tampered_hmac`).

No **runtime code bugs** were found — the migration itself is clean. All findings are documentation/dependency drift.

---

## 2  Problem Statement

After removing ~15 000 lines of HMAC/ECDH code across PRs #624–#629, do any artifacts — documentation, dependencies, prompt instructions, test traceability — still reference the deleted functionality in a way that would mislead future development or create incorrect test coverage tracking?

---

## 3  Investigation Scope

| Category | Artifacts examined |
|----------|--------------------|
| **Code** | All `*.rs` files in `crates/` |
| **Docs** | All `*.md` files in `docs/`, `docs/audits/`, `docs/audits/round2/` |
| **Prompts** | `prompts/workflows/`, `prompts/software/`, `.github/copilot-instructions.md` |
| **Dependencies** | All `Cargo.toml` files in `crates/` |
| **Search patterns** | `HmacProvider`, `encode_frame`/`decode_frame`/`verify_frame` (old codec), `ECDH`/`ecdh`/`HKDF`/`hkdf`/`X25519`, `aes-gcm-codec`, `pair_with_gateway[^_]`, `provision_node[^_]`, `process_frame[^_]`, `run_wake_cycle[^_]`, `hmac`/`HMAC` |

### Protocols applied

- **D1–D4:** Requirements-to-design traceability
- **D5–D6:** Cross-document consistency
- **D8–D9:** Spec-to-code traceability
- **D11–D13:** Test-to-validation traceability

---

## 4  Findings

### F-001 — `copilot-instructions.md` describes HMAC wire format

| Field | Value |
|-------|-------|
| **Severity** | High |
| **Category** | D5 (cross-document consistency) |
| **Location** | `.github/copilot-instructions.md` lines 60, 75, 78 |
| **Description** | Three statements in the copilot instructions are stale: (1) Line 60 says sonde-protocol injects crypto via `HmacProvider`/`Sha256Provider` traits — `HmacProvider` no longer exists; the trait is `AeadProvider`. (2) Line 75 describes the wire format as "CBOR payload + 32-byte HMAC-SHA256" — it is now "AES-256-GCM ciphertext + 16-byte GCM tag". (3) Line 78 lists `HmacProvider` in the platform traits — should be `AeadProvider`. |
| **Evidence** | `grep -n "HmacProvider" .github/copilot-instructions.md` → lines 60, 78. `grep -n "HMAC-SHA256" .github/copilot-instructions.md` → line 75. |
| **Root Cause** | Migration PRs updated code and spec docs but not the repo-level AI instructions file. |
| **Impact** | Every AI-assisted PR will receive incorrect context about the protocol wire format and available crypto traits. High risk of generating code that references deleted APIs. |
| **Confidence** | Certain |
| **Remediation** | Update lines 60, 75, and 78 to reflect AES-256-GCM and `AeadProvider`/`Sha256Provider`. |

---

### F-002 — Prompt workflow files describe HMAC-SHA256 auth

| Field | Value |
|-------|-------|
| **Severity** | High |
| **Category** | D5 (cross-document consistency) |
| **Location** | `prompts/workflows/bootstrap.md` lines 1206, 1242, 1245; `prompts/workflows/maintain.md` lines 1418, 1454, 1457; `prompts/workflows/evolve.md` lines 1542, 1578, 1581 |
| **Description** | All three workflow prompts contain identical stale context: (1) sonde-protocol described as "HMAC-SHA256 auth", (2) `HmacProvider` listed as a platform trait, (3) wire format described as "HMAC-SHA256 authenticated, 250-byte ESP-NOW frames". These are template-injected contexts used by all AI workflows. |
| **Evidence** | `grep -rn "HmacProvider\|HMAC-SHA256" prompts/workflows/` → 9 hits across 3 files. |
| **Root Cause** | Workflow prompt templates were not updated alongside protocol changes. |
| **Impact** | All maintain/evolve/bootstrap workflows receive incorrect crypto context. |
| **Confidence** | Certain |
| **Remediation** | Replace `HMAC-SHA256 auth` → `AES-256-GCM AEAD`, `HmacProvider` → `AeadProvider`, `HMAC-SHA256 authenticated` → `AES-256-GCM authenticated` in all three files. |

---

### F-003 — PR review workflow describes HMAC-SHA256 convention

| Field | Value |
|-------|-------|
| **Severity** | High |
| **Category** | D5 (cross-document consistency) |
| **Location** | `prompts/software/sonde-pr-review-workflow.md` line 420 |
| **Description** | The conventions section says "HMAC-SHA256 authentication". This is injected into every PR review prompt. |
| **Evidence** | `grep -n "HMAC" prompts/software/sonde-pr-review-workflow.md` → line 420. |
| **Root Cause** | Same as F-001/F-002. |
| **Impact** | PR reviews will enforce a convention that no longer exists and may flag correct AES-256-GCM code as non-conformant. |
| **Confidence** | Certain |
| **Remediation** | Change `HMAC-SHA256 authentication` → `AES-256-GCM authenticated encryption`. |

---

### F-004 — Gateway `Cargo.toml` retains unused `hkdf` dependency

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D9 (spec-to-code traceability — dead dependency) |
| **Location** | `crates/sonde-gateway/Cargo.toml` line 12 |
| **Description** | The `hkdf = "0.12"` dependency has zero `use` statements in any gateway source file. It was used for ECDH-based key derivation which is now RETIRED. |
| **Evidence** | `grep -rn "use hkdf\|hkdf::" crates/sonde-gateway/src/` → 0 hits. |
| **Root Cause** | Dependency not cleaned up during AEAD migration. |
| **Impact** | Unnecessary compile-time and supply-chain surface. Low functional risk but contributes to audit noise. |
| **Confidence** | Certain |
| **Remediation** | Remove `hkdf = "0.12"` from `Cargo.toml`. |

---

### F-005 — Gateway `Cargo.toml` has unnecessary direct `hmac` dependency

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D9 (dead dependency) |
| **Location** | `crates/sonde-gateway/Cargo.toml` line 11 |
| **Description** | The only usage of `hmac` in gateway code is `pbkdf2::pbkdf2_hmac::<Sha256>(...)` in `state_bundle.rs`. The `hmac` crate is a transitive dependency of `pbkdf2` and does not need to be listed directly. No `use hmac` or `hmac::` appears anywhere. |
| **Evidence** | `grep -rn "use hmac\|hmac::" crates/sonde-gateway/src/` → 0 direct hits. Only `pbkdf2_hmac` in `state_bundle.rs:286`. |
| **Root Cause** | Formerly used for frame HMAC computation; now only a transitive dep. |
| **Impact** | Minor — no functional issue, but misleading in dependency audit. |
| **Confidence** | High (verify `pbkdf2` re-exports `hmac` before removing) |
| **Remediation** | Verify `pbkdf2_hmac` compiles without a direct `hmac` dep, then remove. |

---

### F-006 — Gateway retains `x25519-dalek` and `ed25519-dalek` for RETIRED identity code

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D8 (spec-to-code — code implements retired requirement) |
| **Location** | `crates/sonde-gateway/Cargo.toml` lines 26–27; `crates/sonde-gateway/src/gateway_identity.rs`; `crates/sonde-gateway/src/ble_pairing.rs` |
| **Description** | Requirements GW-1200 (keypair generation), GW-1201 (`gateway_id`), and GW-1202 (Ed25519→X25519) are all marked RETIRED in `gateway-requirements.md`. However, `gateway_identity.rs` still implements the full Ed25519 keypair, `gateway_id`, and X25519 conversion. `ble_pairing.rs` still implements `REQUEST_GW_INFO` challenge-response signing. The `x25519-dalek` and `ed25519-dalek` crates serve this retired code. |
| **Evidence** | `gateway-requirements.md` lines 1123, 1131, 1139 — all RETIRED. `gateway_identity.rs` lines 4–11 — references GW-1200/1201/1202. `ble_pairing.rs` line 239 — `handle_request_gw_info`. |
| **Root Cause** | The BLE pairing E2E tests still use `REQUEST_GW_INFO`/`GW_INFO_RESPONSE` flow. The code was left in place to avoid breaking the E2E test suite. This is intentional technical debt from the phased migration. |
| **Impact** | ~400 lines of dead code; 2 unnecessary crate dependencies; confusing audit trail. The `ed25519-dalek` dep also appears in `sonde-e2e/Cargo.toml`. |
| **Confidence** | High |
| **Remediation** | Phase 2 of migration should remove `gateway_identity.rs`, `handle_request_gw_info`, `REQUEST_GW_INFO` constants, and associated E2E test helper code. Remove `x25519-dalek` and `ed25519-dalek` from gateway and E2E `Cargo.toml`. File as a follow-up issue. |

---

### F-007 — Round-2 audit: gateway compliance table says GW-0600 = "HMAC-SHA256"

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D5 (cross-document consistency) |
| **Location** | `docs/audits/round2/gateway-code-compliance.md` line 519 |
| **Description** | The compliance table entry for GW-0600 says "HMAC-SHA256 authentication" with evidence "`crypto.rs` — `RustCryptoHmac`; `engine.rs` — verify all frames". GW-0600 was rewritten to "AES-256-GCM message authentication" in `gateway-requirements.md`. The `RustCryptoHmac` struct no longer exists. |
| **Evidence** | `gateway-requirements.md:520` → "GW-0600 AES-256-GCM message authentication". `round2/gateway-code-compliance.md:519` → still says HMAC. |
| **Root Cause** | Round-2 audit was completed before the AEAD migration. |
| **Impact** | Future auditors referencing round-2 results will see false compliance for a deleted implementation. |
| **Confidence** | Certain |
| **Remediation** | Add a banner to round-2 audit docs marking them as pre-AEAD-migration snapshots, or update the relevant rows. |

---

### F-008 — Round-2 audit: node compliance table says ND-0300/0301 = "HMAC"

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D5 (cross-document consistency) |
| **Location** | `docs/audits/round2/node-code-compliance.md` lines 188, 194–195 |
| **Description** | ND-0102 says "11-byte header + payload + 32-byte HMAC". ND-0300 says "HMAC-SHA256 authentication". ND-0301 says "Inbound HMAC verification". The current requirements are AES-256-GCM. |
| **Evidence** | `node-requirements.md:186` → "ND-0300 AES-256-GCM authenticated encryption". `round2/node-code-compliance.md:194` → "HMAC-SHA256 authentication". |
| **Root Cause** | Same as F-007. |
| **Impact** | Same as F-007. |
| **Confidence** | Certain |
| **Remediation** | Same as F-007. |

---

### F-009 — Round-2 protocol crate audit tables reference HMAC trailer and `HmacProvider`

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D5 (cross-document consistency) |
| **Location** | `docs/audits/round2/protocol-crate-trifecta-audit.md` lines 51, 65, 74, 444; `docs/audits/round2/protocol-crate-code-compliance.md` line 384; `docs/audits/round2/protocol-crate-test-compliance.md` line 195 |
| **Description** | Multiple tables describe "HMAC trailer (32-byte HMAC-SHA256)", list `HmacProvider` as a design API contract, and reference `test_p069` as tracking that `verify_frame` calls `HmacProvider::verify`. These APIs no longer exist. |
| **Evidence** | `grep -rn "HmacProvider\|HMAC" docs/audits/round2/protocol-crate-*` → 6 hits. |
| **Root Cause** | Same as F-007. |
| **Impact** | Same as F-007. |
| **Confidence** | Certain |
| **Remediation** | Same as F-007. |

---

### F-010 — Node validation traceability table references deleted test functions

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D11 (test-to-validation traceability) |
| **Location** | `docs/node-validation.md` lines 1843–1845, 1913 |
| **Description** | The "Implementing Tests" traceability table references test functions that no longer exist: `t_e2e_002_hmac_round_trip`, `test_invalid_hmac_discarded`, `test_outbound_frame_format`, `t_e2e_003_wrong_psk_rejected` (no such e2e test name), `t_e2e_040_unknown_node`, `t_e2e_053_bridged_wrong_psk`, `t_n941_exchange_peer_ack_corrupted_hmac_discarded`, `peer_ack_tampered_hmac`. None of these function names appear in the current codebase. |
| **Evidence** | `grep -rn "t_e2e_002_hmac\|test_invalid_hmac\|test_outbound_frame_format\|peer_ack_tampered_hmac" crates/` → 0 hits. The AEAD E2E tests use `t_e2e_050_nop_wake_cycle`, `t_e2e_052_wrong_psk_rejected`, etc. Node tests use `wake_command_exchange_round_trip`, `verify_peer_ack_valid`, etc. |
| **Root Cause** | Test functions were renamed during AEAD migration but the validation traceability table was not updated. |
| **Impact** | The traceability table reports false coverage — it claims tests exist that don't. This makes gap analysis unreliable. |
| **Confidence** | Certain |
| **Remediation** | Update lines 1843–1845 and 1913 to reference the actual AEAD test function names. |

---

### F-011 — E2E test comment describes "ECDH key exchange" in `t_e2e_061`

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D9 (spec-to-code — stale doc comment) |
| **Location** | `crates/sonde-e2e/tests/e2e_tests.rs` lines 92–93 |
| **Description** | The doc comment for `t_e2e_061_phone_registration` says "REQUEST_GW_INFO challenge-response, REGISTER_PHONE with ECDH key exchange". The actual test code at lines 721–728 shows the AEAD path (phone sends PSK directly). The comment is stale. |
| **Evidence** | Line 92: `/// Full Phase 1 round-trip: REQUEST_GW_INFO challenge-response, REGISTER_PHONE` — line 93: `/// with ECDH key exchange, and verification that:`. Code at line 721: `// Phase 1b: REGISTER_PHONE (AEAD — phone sends PSK directly)`. |
| **Root Cause** | Comment not updated when implementation changed. |
| **Impact** | Misleads readers about what the test validates. Low severity since the code itself is correct. |
| **Confidence** | Certain |
| **Remediation** | Update the doc comment to say "AEAD phone PSK exchange" instead of "ECDH key exchange". |

---

### F-012 — E2E harness `simulate_phone_registration` still uses `REQUEST_GW_INFO`

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D8 (spec-to-code — code implements retired requirement) |
| **Location** | `crates/sonde-e2e/src/harness.rs` lines 660–719 |
| **Description** | The `simulate_phone_registration` helper sends `REQUEST_GW_INFO`, receives `GW_INFO_RESPONSE`, verifies the Ed25519 signature, and asserts `gateway_id`. This exercises the fully RETIRED Phase 1a protocol. All BLE pairing E2E tests (`t_e2e_061` through `t_e2e_070`) go through this helper. |
| **Evidence** | `harness.rs:677` — `// Phase 1a: REQUEST_GW_INFO`. `harness.rs:703` — `use ed25519_dalek::`. `ble-pairing-protocol.md:135` — `REQUEST_GW_INFO` RETIRED. |
| **Root Cause** | Same as F-006 — Phase 1a BLE pairing was left intact for E2E test stability during phased migration. |
| **Impact** | E2E tests validate a protocol flow that will never be used in production. When the retired code is removed, all BLE E2E tests will break and need rewriting. |
| **Confidence** | Certain |
| **Remediation** | Track as part of the same follow-up issue as F-006. |

---

### F-013 — Round-1 audit docs (`docs/audits/`) entirely pre-AEAD

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D5 (cross-document consistency) |
| **Location** | `docs/audits/node-code-compliance.md`, `docs/audits/protocol-crate-code-compliance.md`, `docs/audits/gateway-code-compliance.md`, `docs/audits/protocol-crate-trifecta-audit.md` |
| **Description** | All round-1 audit documents describe the pre-AEAD state (HMAC frame format, `HmacProvider` trait, `encode_frame`/`decode_frame`/`verify_frame` codec). These are historical records but lack a "superseded" banner. |
| **Evidence** | `docs/audits/node-code-compliance.md:30` — "32-byte HMAC". `docs/audits/protocol-crate-code-compliance.md:71` — lists `encode_frame`, `decode_frame`, `verify_frame`. |
| **Root Cause** | Historical audit documents naturally become stale. |
| **Impact** | Low — these are clearly dated historical records. Risk is that someone treats them as current. |
| **Confidence** | Certain |
| **Remediation** | Add a "⚠️ Pre-AEAD migration snapshot" banner to each round-1 and round-2 audit document. |

---

### F-014 — `protocol-crate-design.md` still references `encode_frame` / `decode_frame` (non-AEAD)

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D6 (design consistency) |
| **Location** | `docs/protocol-crate-design.md` lines 148, 174 |
| **Description** | The design document shows function signatures `pub fn encode_frame(...)` and `pub fn decode_frame(...)`. The actual functions in code are `encode_frame` and `decode_frame`. However, the design doc describes AES-256-GCM semantics correctly (lines 152–159, 171–181), suggesting it was partially updated but the function names were not changed to match the `_aead` suffix in the implementation. |
| **Evidence** | `protocol-crate-design.md:148` — `pub fn encode_frame(`. Code: `crates/sonde-protocol/src/aead_codec.rs:60` — `pub fn encode_frame(`. |
| **Root Cause** | The design doc describes the target API (without `_aead` suffix, as the eventual name post-migration). The code uses `_aead` suffix during the transition period. This is an intentional naming divergence, but creates traceability confusion. |
| **Impact** | Low — the protocol-crate-validation.md references `encode_frame()` / `decode_frame()` / `open_frame()` consistently with the design, so internal consistency is maintained. The code diverges. |
| **Confidence** | High |
| **Remediation** | Either rename the code functions to drop the `_aead` suffix (now that HMAC is fully removed), or update the design doc to use the `_aead` suffix. The former is cleaner. |

---

### F-015 — `sonde-e2e/Cargo.toml` retains `ed25519-dalek` dependency

| Field | Value |
|-------|-------|
| **Severity** | Low |
| **Category** | D9 (dead dependency) |
| **Location** | `crates/sonde-e2e/Cargo.toml` line 14 |
| **Description** | `ed25519-dalek = "2"` is used only in `harness.rs:703` to verify the Ed25519 signature in the RETIRED `REQUEST_GW_INFO` flow. When F-006/F-012 are resolved, this dependency will become unused. |
| **Evidence** | `grep -rn "ed25519_dalek" crates/sonde-e2e/` → 1 hit in `harness.rs:703`. |
| **Root Cause** | Same phased migration as F-006. |
| **Impact** | Unnecessary dependency; will be resolved with F-006. |
| **Confidence** | Certain |
| **Remediation** | Remove alongside F-006/F-012. |

---

### F-016 — Gateway `GW-0603` audit row says "32 HMAC" overhead

| Field | Value |
|-------|-------|
| **Severity** | Medium |
| **Category** | D5 (cross-document consistency) |
| **Location** | `docs/audits/round2/gateway-code-compliance.md` line 524 |
| **Description** | The GW-0603 compliance row says "43-byte overhead (11 header + 32 HMAC)". Post-AEAD, the overhead is 27 bytes (11 header + 16 GCM tag). The requirement itself was updated in `gateway-requirements.md`, but the audit table was not. |
| **Evidence** | Line 524: `"sonde_protocol — 43-byte overhead (11 header + 32 HMAC)"`. Actual: `MIN_FRAME_SIZE = HEADER_SIZE + AEAD_TAG_SIZE = 11 + 16 = 27`. |
| **Root Cause** | Same as F-007. |
| **Impact** | Misleading overhead calculation could affect future capacity planning. |
| **Confidence** | Certain |
| **Remediation** | Same as F-007 — mark as pre-AEAD snapshot. |

---

## 5  Root Cause Analysis

All 16 findings share a common root cause: **the AEAD migration PRs (#624–#629) correctly updated the specification documents (requirements, design, validation, protocol, security) and the runtime code, but did not update:**

1. **AI instruction files** (`.github/copilot-instructions.md`, `prompts/workflows/`, `prompts/software/`) — these files are outside the standard spec/code review loop.
2. **Historical audit documents** (`docs/audits/`, `docs/audits/round2/`) — these are point-in-time snapshots that were not flagged as needing update.
3. **Validation traceability tables** — the "Implementing Tests" table at the bottom of `node-validation.md` references test function names that were renamed during migration.
4. **Dependencies** — crate dependencies for removed functionality (`hkdf`, direct `hmac`, `x25519-dalek`) were not cleaned up.
5. **Phase 1a BLE code** (`GatewayIdentity`, `REQUEST_GW_INFO`, Ed25519 signing) — intentionally deferred to a follow-up phase but not tracked as an issue.

---

## 6  Remediation Plan

### Immediate (should be done now)

| ID | Finding | Action | Effort |
|----|---------|--------|--------|
| R-01 | F-001 | Update `.github/copilot-instructions.md` lines 60, 75, 78 | Trivial |
| R-02 | F-002 | Update `prompts/workflows/{bootstrap,maintain,evolve}.md` HMAC refs | Trivial |
| R-03 | F-003 | Update `prompts/software/sonde-pr-review-workflow.md` line 420 | Trivial |
| R-04 | F-004 | Remove `hkdf = "0.12"` from `crates/sonde-gateway/Cargo.toml` | Trivial |
| R-05 | F-010 | Update `docs/node-validation.md` traceability table (lines 1843–1845, 1913) | Small |
| R-06 | F-011 | Fix doc comment in `crates/sonde-e2e/tests/e2e_tests.rs:92–93` | Trivial |

### Short-term (next sprint)

| ID | Finding | Action | Effort |
|----|---------|--------|--------|
| R-07 | F-007–F-009, F-013, F-016 | Add "⚠️ Pre-AEAD migration snapshot" banner to all round-1 and round-2 audit docs | Small |
| R-08 | F-005 | Test removing direct `hmac` dep; remove if `pbkdf2_hmac` still compiles | Trivial |
| R-09 | F-014 | Rename `encode_frame`/`decode_frame`/`open_frame` → drop `_aead` suffix now that HMAC codec is fully removed | Medium |

### Follow-up issue (Phase 2 cleanup)

| ID | Finding | Action | Effort |
|----|---------|--------|--------|
| R-10 | F-006, F-012, F-015 | Remove `gateway_identity.rs`, `handle_request_gw_info`, RETIRED BLE Phase 1a code and E2E helpers. Remove `ed25519-dalek`, `x25519-dalek` deps from gateway and E2E | Large |

---

## 7  Prevention

1. **Add AI instruction files to the migration checklist.** When a protocol-level change is made, `.github/copilot-instructions.md` and `prompts/` must be updated in the same PR or an immediately following PR.
2. **Automate stale-reference detection.** A CI check that greps for `HmacProvider` (excluding `RETIRED` markers and audit docs) would have caught F-001–F-003 at PR time.
3. **Validation traceability tables should be generated.** The "Implementing Tests" table in validation docs should cross-reference actual test function names via a script, not be maintained manually.
4. **Historical audit docs should be immutable.** Add a front-matter `snapshot_date` field and a convention that audits are never edited — new audits supersede old ones.

---

## 8  Open Questions

1. **Should `GatewayIdentity` be removed now or deferred?** The BLE pairing protocol still uses `REQUEST_GW_INFO`/`GW_INFO_RESPONSE` in the E2E tests even though the protocol doc marks them RETIRED. Is there a production deployment that still relies on Phase 1a? If not, removal should be prioritized.

2. **Should `_aead` function suffixes be dropped?** The design doc uses `encode_frame`/`decode_frame` (no suffix) and the code uses `encode_frame`/`decode_frame`. With HMAC codec fully deleted, there's no ambiguity — the `_aead` suffix is redundant. But renaming touches every call site. Is this worth the churn?

3. **Is the `hmac` crate still needed as a direct dep for `pbkdf2_hmac`?** The `pbkdf2` crate may or may not re-export `hmac`. This needs a compile test.

4. **Should round-2 audit documents be updated or left as snapshots?** Updating them creates a false impression that the audit was conducted against the current code. Leaving them creates confusion about current compliance status. A "superseded" banner is the recommended middle ground.

---

## 9  Revision History

| Date | Author | Changes |
|------|--------|---------|
| 2026-07-21 | Copilot (automated) | Initial audit — 16 findings across post-AEAD migration drift |
