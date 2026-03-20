<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Protocol Crate Specification Audit — Investigation Report

## 1. Executive Summary

This report presents a systematic traceability audit of the Sonde **protocol crate** specification set: the protocol specification (`protocol.md`, serving as requirements), the crate design (`protocol-crate-design.md`), and the validation plan (`protocol-crate-validation.md`).

The audit identified **14 findings** across forward traceability (requirements → design → tests), backward traceability (tests → design → requirements), and cross-document consistency. The specification set is fundamentally sound — frame format, HMAC authentication, CBOR encoding rules, program image handling, and chunking logic are consistently specified across all three documents and well-covered by tests. However, the audit uncovered:

- **3 message/command variants** defined in the protocol spec with no dedicated validation test (PROGRAM_ACK, RUN_EPHEMERAL, REBOOT), reducing message-type test coverage to **67%** (8 of 12 distinct variants).
- **4 of 8 design error variants** (50%) have no test exercising them, including `DecodeError::TooLong` and `DecodeError::InvalidFieldType`.
- **No wire-format conformance tests** for the COMMAND message's nested payload structure (key 5 omission for NOP/REBOOT, nested map for UPDATE_PROGRAM).
- A **design-level concern** where `GatewayMessage::Command` carries a redundant `command_type` field alongside the typed `CommandPayload` enum, creating an inconsistency risk with no validation.

No fabricated test references or phantom requirements were found. All design elements trace back to protocol assertions. The 41 test cases claimed in the validation document were confirmed by enumeration.

---

## 2. Problem Statement

The protocol crate is the shared foundation for all Sonde components — gateway, node firmware, admin CLI, and test harnesses. Gaps between the protocol specification, the crate's design, and its test plan create risks:

- **Untested message types** may harbor encoding/decoding bugs that surface only during integration, wasting hardware test cycles.
- **Untested error paths** undermine the "silent discard" security posture — a panic on malformed input is a denial-of-service vector on embedded firmware.
- **Missing wire-format conformance tests** risk interoperability failures if independent implementations (or future refactors) encode COMMAND payloads differently while passing round-trip tests.

This audit aims to close those gaps before implementation proceeds.

---

## 3. Investigation Scope

| Artifact | Role | Location |
|---|---|---|
| `protocol.md` | Requirements (wire-level protocol specification) | `docs/protocol.md` |
| `protocol-crate-design.md` | Design (crate architecture, API, types) | `docs/protocol-crate-design.md` |
| `protocol-crate-validation.md` | Validation (test plan, 41 test cases) | `docs/protocol-crate-validation.md` |

**In scope:** Forward and backward traceability across all three documents; cross-document consistency of terminology, constraints, and structure.

**Out of scope:** Implementation code review; `security.md` and `gateway-design.md` content (referenced but not audited); behavioral/timing requirements (§8–§9 of protocol.md) that belong to gateway/node crates, not the protocol crate.

---

## 4. Findings

### F-001 — Protocol specification lacks formal requirement IDs

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D7 — Structural / Process |
| **Location** | `protocol.md` (entire document) |

**Description:** The protocol specification does not assign formal requirement identifiers (e.g., `REQ-P-001`) to its testable assertions. Requirements are expressed as prose, tables, and diagrams scattered across §3–§10.

**Evidence:** No `REQ-*` or equivalent identifiers appear anywhere in the 579-line document. Testable assertions such as "every frame carries an HMAC-SHA256 tag" (§1), "key_hint: 2 bytes, big-endian" (§3.1.1), and "Unknown keys in inbound messages are ignored" (§10) lack formal IDs.

**Root Cause:** The document was authored as a design-oriented specification, not a formal requirements document.

**Impact:** Traceability between requirements, design, and tests must be inferred from content matching rather than ID cross-references. This increases audit effort and makes it easy to miss coverage gaps.

**Remediation:** Add a requirement ID prefix to each testable assertion (e.g., `[P-FRAME-01]` for frame format assertions, `[P-MSG-01]` for message definitions). Update the validation document's test cases with a `Validates:` field referencing these IDs.

**Confidence:** High — confirmed by full-text search of the document.

---

### F-002 — Validation test cases lack traceability links

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D7 — Structural / Process |
| **Location** | `protocol-crate-validation.md` (all test cases) |

**Description:** None of the 41 test cases includes a `Validates:` field (or equivalent) linking it to a specific protocol specification assertion. Traceability is implicit — the test title hints at what it covers, but there is no machine-readable or reviewer-auditable forward/backward link.

**Evidence:** Inspection of all test case headings (T-P001 through T-P062). Each has `Procedure:` and `Assert:` blocks, but no `Validates:`, `Covers:`, or `Traces-to:` field.

**Root Cause:** Same as F-001 — without requirement IDs in the protocol spec, test cases cannot reference them.

**Impact:** Coverage gaps (F-003 through F-009) went undetected because no structured coverage matrix exists.

**Remediation:** After F-001 is addressed, add a `Validates:` line to each test case referencing one or more requirement IDs.

**Confidence:** High.

---

### F-003 — Missing test for `RUN_EPHEMERAL` command round-trip

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D2 — Requirement → Validation gap |
| **Location** | `protocol.md` §5.2.1, §6.3; `protocol-crate-validation.md` §4 |

**Description:** The protocol spec defines `RUN_EPHEMERAL` (command code `0x02`) with the same payload format as `UPDATE_PROGRAM` but a different `command_type` discriminator. The validation plan has no test for it. T-P023 covers `UPDATE_PROGRAM` only.

**Evidence:** T-P023 creates `CommandPayload::UpdateProgram`. No test creates `CommandPayload::RunEphemeral`. The design doc (§6.2, §3) defines `CMD_RUN_EPHEMERAL = 0x02` and the `RunEphemeral` variant.

**Root Cause:** The payload structure is identical to `UPDATE_PROGRAM`, which may have led to the assumption that one test suffices. However, the `command_type` wire encoding differs.

**Impact:** A bug in encoding/decoding `command_type = 0x02` (e.g., incorrect match arm, off-by-one in command dispatch) would go undetected until gateway–node integration.

**Remediation:** Add a test `T-P023b` (or similar) that round-trips `GatewayMessage::Command` with `CMD_RUN_EPHEMERAL` and `CommandPayload::RunEphemeral`, asserting `command_type = 0x02` after decode.

**Confidence:** High — confirmed by enumerating all test cases and matching against design §3 command codes.

---

### F-004 — Missing test for `REBOOT` command round-trip

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D2 — Requirement → Validation gap |
| **Location** | `protocol.md` §5.2 (command type `0x04`); `protocol-crate-validation.md` §4 |

**Description:** `REBOOT` (command code `0x04`) is defined in the protocol spec as carrying no payload (key 5 omitted). The design defines `CommandPayload::Reboot` and `CMD_REBOOT = 0x04`. No test exercises this variant.

**Evidence:** T-P022 tests `NOP` (also no payload). T-P023 tests `UPDATE_PROGRAM`. T-P024 tests `UPDATE_SCHEDULE`. No test creates `CommandPayload::Reboot`.

**Root Cause:** `REBOOT` is structurally similar to `NOP` (no payload), and may have been considered implicitly covered. However, the `command_type` discriminator on the wire is different (`0x04` vs `0x00`).

**Impact:** Encoding or decoding bugs specific to command code `0x04` would go undetected.

**Remediation:** Add test `T-P022b` that round-trips `GatewayMessage::Command` with `CMD_REBOOT` and `CommandPayload::Reboot`.

**Confidence:** High.

---

### F-005 — Missing test for `PROGRAM_ACK` message round-trip

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D2 — Requirement → Validation gap |
| **Location** | `protocol.md` §5.5; `protocol-crate-design.md` §6.1; `protocol-crate-validation.md` §4 |

**Description:** The protocol spec defines `PROGRAM_ACK` (msg_type `0x03`) with a required `program_hash` field. The design defines `NodeMessage::ProgramAck { program_hash: Vec<u8> }`. No dedicated test exercises this message type's encode/decode path.

**Evidence:** The validation plan §4 tests: Wake (T-P020, T-P021), GetChunk (T-P025), AppData (T-P027). ProgramAck is absent. Integration tests T-P060 and T-P061 do not include ProgramAck either.

**Root Cause:** Likely an oversight — ProgramAck is structurally simple (single field), but every message type deserves at least one round-trip test.

**Impact:** A bug in the `ProgramAck` encode/decode path (e.g., wrong CBOR key for `program_hash` in the ProgramAck context, or incorrect `msg_type` matching) would be invisible until chunked-transfer integration testing.

**Remediation:** Add test `T-P025b` (or renumber) that creates `NodeMessage::ProgramAck { program_hash: vec![0xBB; 32] }`, round-trips it, and asserts the hash matches.

**Confidence:** High.

---

### F-006 — Missing test for `DecodeError::TooLong` on `decode_frame`

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D3 — Design → Validation gap |
| **Location** | `protocol-crate-design.md` §5.3 (step 2); `protocol-crate-validation.md` §3 |

**Description:** The design specifies that `decode_frame()` returns `DecodeError::TooLong` when `raw.len() > MAX_FRAME_SIZE` (250). The validation plan tests `TooShort` (T-P016) and encode-side `FrameTooLarge` (T-P018), but no test exercises the decode-side `TooLong` error.

**Evidence:** T-P016 procedure: "Call `decode_frame()` with 42 bytes … Assert: `DecodeError::TooShort`." T-P018 procedure: "Call `encode_frame()` with a payload that would make the total exceed 250 bytes. Assert: `EncodeError::FrameTooLarge`." No test calls `decode_frame()` with >250 bytes.

**Root Cause:** T-P018 tests the encode guard for oversized frames but not the corresponding decode guard. These are independent code paths.

**Impact:** An implementation that omits the `TooLong` check in `decode_frame` would pass all tests, but would accept oversized frames from the wire — a potential buffer or logic issue if downstream code assumes `MAX_FRAME_SIZE` compliance.

**Remediation:** Add test `T-P018b`: call `decode_frame()` with 251 bytes, assert `DecodeError::TooLong`.

**Confidence:** High.

---

### F-007 — Missing test for `DecodeError::InvalidFieldType`

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D3 — Design → Validation gap |
| **Location** | `protocol-crate-design.md` §8; `protocol-crate-validation.md` §4 |

**Description:** The design defines `DecodeError::InvalidFieldType(u64)` for CBOR values with the wrong type (e.g., a string where a uint is expected). No test exercises this error variant.

**Evidence:** T-P030 tests `MissingField`. T-P031 tests `InvalidMsgType`. No test constructs CBOR with a correct key but wrong value type.

**Root Cause:** The validation plan focuses on missing fields and invalid msg_types but does not test type mismatches within CBOR map values.

**Impact:** Low — CBOR deserializers typically produce errors naturally on type mismatches. But having an explicit test documents the expected behavior and ensures the error variant maps correctly.

**Remediation:** Add test `T-P030b`: manually construct CBOR for a Wake message where `firmware_abi_version` (key 1) is a text string instead of uint. Assert `DecodeError::InvalidFieldType(1)`.

**Confidence:** High.

---

### F-008 — Missing tests for `CborError` variants

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D3 — Design → Validation gap |
| **Location** | `protocol-crate-design.md` §8; `protocol-crate-validation.md` §4 |

**Description:** The design defines `EncodeError::CborError(String)` and `DecodeError::CborError(String)` for CBOR serialization/deserialization failures. No test exercises either variant.

**Evidence:** No test in the validation plan passes garbled (non-CBOR) bytes to `NodeMessage::decode()`, `GatewayMessage::decode()`, or `ProgramImage::decode()`.

**Root Cause:** The validation plan tests well-formed and missing-field CBOR but not completely malformed byte sequences.

**Impact:** Low — these are defensive error paths that catch corruption. But on embedded firmware, an unhandled CBOR parse error could panic; having a test confirms the error is caught and returned gracefully.

**Remediation:** Add test `T-P031b`: call `NodeMessage::decode(MSG_WAKE, &[0xFF, 0xFF, 0xFF])` (invalid CBOR). Assert `DecodeError::CborError(...)`. Similarly, add a test for `ProgramImage::decode` with garbage bytes.

**Confidence:** High.

---

### F-009 — Missing test for `GatewayMessage::decode` with invalid `msg_type`

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D3 — Design → Validation gap |
| **Location** | `protocol-crate-design.md` §6.2; `protocol-crate-validation.md` §4 (T-P031) |

**Description:** T-P031 tests `NodeMessage::decode(0xFF, ...)` and asserts `InvalidMsgType(0xFF)`. No equivalent test exists for `GatewayMessage::decode` with an invalid or out-of-range `msg_type`.

**Evidence:** T-P031 procedure explicitly calls `NodeMessage::decode`. The design defines both `NodeMessage::decode(msg_type, cbor)` and `GatewayMessage::decode(msg_type, cbor)` with independent msg_type matching.

**Root Cause:** Likely an oversight — the test was written for one direction and not duplicated for the other.

**Impact:** A bug in `GatewayMessage::decode`'s msg_type validation (e.g., accepting node-direction msg_types like `0x01`) would be undetected.

**Remediation:** Add test `T-P031c`: call `GatewayMessage::decode(0x01, &valid_cbor)`, assert `DecodeError::InvalidMsgType(0x01)`. Also test with `0xFF`.

**Confidence:** High.

---

### F-010 — Redundant `command_type` field in `GatewayMessage::Command` creates inconsistency risk

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D5 — Internal design consistency |
| **Location** | `protocol-crate-design.md` §6.2 |

**Description:** `GatewayMessage::Command` carries both a `command_type: u8` field and a typed `payload: CommandPayload` enum. The command type is fully determined by the `CommandPayload` variant (`Nop` → `0x00`, `UpdateProgram` → `0x01`, etc.), making `command_type` redundant. This creates a risk: a caller can construct a `Command` where `command_type` and `payload` disagree (e.g., `command_type: CMD_NOP` with `payload: CommandPayload::UpdateProgram { ... }`).

**Evidence:** Design §6.2:
```rust
Command {
    command_type: u8,
    starting_seq: u64,
    timestamp_ms: u64,
    payload: CommandPayload,
}
```
The `command_type` is also defined separately as constants `CMD_NOP = 0x00` through `CMD_REBOOT = 0x04` (design §3).

**Root Cause:** The field exists because on decode, `command_type` must be read from the wire before the payload variant can be determined. Including it in the struct makes it available for inspection after decode. But it also makes it writable on construction.

**Impact:** An encode path that writes `command_type` from the field (rather than deriving it from the `CommandPayload` variant) would silently produce malformed frames when the two disagree. No validation test checks for this — all tests construct matching pairs.

**Remediation:** Either: (a) derive `command_type` from `payload` during encoding and remove the separate field, adding a `command_type()` getter method; or (b) keep the field but add a consistency assertion in `encode()` and a test that attempts to encode a mismatched pair, asserting an error.

**Confidence:** High — confirmed by inspecting the struct definition and all test cases.

---

### F-011 — No `key_hint` derivation utility in protocol crate design

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D1 — Requirement → Design gap |
| **Location** | `protocol.md` §3.1.1; `protocol-crate-design.md` §3 |

**Description:** The protocol spec defines `key_hint` as "an optimization hint that lets the gateway quickly narrow down which key(s) to try" (§3.1.1). The project's coding conventions specify the derivation formula: `key_hint = u16::from_be_bytes(SHA-256(PSK)[30..32])`. The protocol crate design includes `OFFSET_KEY_HINT` but provides no `key_hint_from_psk()` utility function.

**Evidence:** Protocol spec §3.1.1 describes semantics. Design §3 defines `pub const OFFSET_KEY_HINT: usize = 0` but no derivation function. The formula appears only in the project-level coding conventions (`.copilot-instructions`), not in any specification document.

**Root Cause:** The derivation formula may have been considered a gateway/node concern rather than a protocol crate concern. However, the protocol crate is described as "the single source of truth for frame encoding, decoding, message types, and constants" (design §1), and `key_hint` is a frame-level field.

**Impact:** Without a canonical derivation function in the protocol crate, gateway and node implementations must independently implement the same formula, risking divergence.

**Remediation:** Add `pub fn key_hint_from_psk(psk: &[u8], sha: &impl Sha256Provider) -> u16` to the protocol crate design. Document the derivation formula in the protocol spec §3.1.1.

**Confidence:** Medium — the spec is deliberately silent on derivation; this may be intentional. The formula in coding conventions is authoritative but is not part of the audited specification set.

---

### F-012 — No wire-format test for COMMAND payload nesting (key 5 behavior)

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D2 — Requirement → Validation gap |
| **Location** | `protocol.md` §5.2; `protocol-crate-validation.md` §4 |

**Description:** The protocol spec mandates specific CBOR nesting for COMMAND messages: "The top-level CBOR map contains `command_type` (key 4), `starting_seq` (key 13), and `timestamp_ms` (key 14). Command-specific fields are nested inside `payload` (key 5) as a separate CBOR map. For `NOP` and `REBOOT`, key 5 is omitted entirely." No test inspects the CBOR bytes to verify this nesting structure.

**Evidence:** T-P022 (NOP) and T-P023 (UPDATE_PROGRAM) verify round-trip value equality but do not inspect the encoded CBOR bytes. T-P032 inspects Wake CBOR bytes for integer keys, but no equivalent exists for COMMAND messages. The protocol spec §5.2 explicitly defines the nesting as a distinct structural requirement.

**Root Cause:** Round-trip tests verify value preservation but not wire-format conformance. An encoder that flattens all fields into the top-level map (keys 2, 4, 5, 6, 7, 8, 13, 14 all at the same level) would pass round-trip tests if the decoder accepts the same flat structure.

**Impact:** If the protocol crate encodes COMMAND payloads with a non-conformant structure, other implementations (or future independent decoders) will fail to interoperate. This is especially critical because the protocol spec explicitly defines the nesting.

**Remediation:** Add two tests:
1. `T-P032b`: Encode a COMMAND with `UpdateProgram` payload. Inspect the CBOR bytes. Assert: top-level map has keys {4, 5, 13, 14}; the value at key 5 is itself a CBOR map containing keys {2, 6, 7, 8}.
2. `T-P032c`: Encode a COMMAND with `Nop` payload. Inspect the CBOR bytes. Assert: top-level map has keys {4, 13, 14}; key 5 is absent.

**Confidence:** High — the nesting requirement is explicitly stated in protocol.md §5.2.

---

### F-013 — No test for `msg_type` direction-bit convention

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D2 — Requirement → Validation gap |
| **Location** | `protocol.md` §4; `protocol-crate-design.md` §3; `protocol-crate-validation.md` |

**Description:** The protocol spec §4 defines: "The high bit indicates direction: `0x01–0x7F` — Node → Gateway; `0x80–0xFF` — Gateway → Node." The design defines msg_type constants that follow this convention. No test verifies the convention holds for all defined constants, nor does the design provide a helper function (e.g., `is_node_message(msg_type: u8) -> bool`).

**Evidence:** Design §3: `MSG_WAKE = 0x01`, `MSG_GET_CHUNK = 0x02`, `MSG_PROGRAM_ACK = 0x03`, `MSG_APP_DATA = 0x04` (all < 0x80); `MSG_COMMAND = 0x81`, `MSG_CHUNK = 0x82`, `MSG_APP_DATA_REPLY = 0x83` (all ≥ 0x80). No test asserts `MSG_WAKE & 0x80 == 0` or `MSG_COMMAND & 0x80 == 0x80`.

**Root Cause:** The convention is structural (baked into constant values) and unlikely to break. But without a test, a future addition of a new msg_type with the wrong high bit would go unnoticed.

**Impact:** Low — existing constants are correct. A direction helper and a test would guard against regressions when new message types are added.

**Remediation:** Add a test that asserts all `MSG_*` node-to-gateway constants have bit 7 clear and all gateway-to-node constants have bit 7 set. Optionally add a `pub fn is_gateway_message(msg_type: u8) -> bool` helper to the design.

**Confidence:** High.

---

### F-014 — No integration test covering `PROGRAM_ACK` in the chunked-transfer flow

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D2 — Requirement → Validation gap |
| **Location** | `protocol.md` §6.2; `protocol-crate-validation.md` §7 |

**Description:** The protocol spec §6.2 defines the complete chunked-transfer flow as: WAKE → COMMAND{UPDATE_PROGRAM} → GET_CHUNK/CHUNK × N → PROGRAM_ACK → APP_DATA exchanges → sleep. The integration tests (T-P060, T-P061, T-P062) cover Wake, Command, and program-image chunking, but none includes a PROGRAM_ACK message in the flow.

**Evidence:** T-P060 covers Wake frame flow. T-P061 covers Command frame flow. T-P062 covers program image → chunk → reassemble → hash → decode. None creates or encodes a `NodeMessage::ProgramAck` frame.

**Root Cause:** The validation plan treats the chunked-transfer flow as a composition of independently tested primitives, but omits PROGRAM_ACK from both unit tests (F-005) and integration tests.

**Impact:** PROGRAM_ACK is the only message type with zero test coverage at any level. Combined with F-005, this is a complete blind spot.

**Remediation:** Either extend T-P062 to include encoding a PROGRAM_ACK frame with the computed hash, or add a new integration test `T-P063` covering the full flow including PROGRAM_ACK.

**Confidence:** High.

---

## 5. Root Cause Analysis

The findings cluster into three root causes:

### RC-1: Incomplete message-variant enumeration in the test plan

Findings F-003, F-004, F-005, and F-014 all stem from the validation plan not systematically enumerating every message type and command variant from the protocol spec. The plan covers 8 of 12 distinct message/command variants, missing `RUN_EPHEMERAL`, `REBOOT`, and `PROGRAM_ACK` entirely.

### RC-2: Round-trip tests used as proxy for wire-format conformance

Findings F-012 stems from relying exclusively on encode→decode round-trip tests. These verify value preservation but not wire-format structure. The protocol spec defines explicit nesting rules for COMMAND messages that round-trip tests cannot verify (an encoder/decoder pair with matching bugs would pass). T-P032 (CBOR integer keys) is the only wire-format inspection test, and it covers only Wake messages.

### RC-3: Absence of formal traceability infrastructure

Findings F-001 and F-002 are structural. Without requirement IDs or `Validates:` links, coverage gaps can only be found by manual content comparison — which is error-prone and does not scale as the specification grows.

### Coverage Metrics

| Metric | Covered | Total | Percentage |
|---|---|---|---|
| Node→Gateway message types tested | 3 (WAKE, GET_CHUNK, APP_DATA) | 4 | 75% |
| Gateway→Node message types tested | 3 (COMMAND, CHUNK, APP_DATA_REPLY) | 3 | 100% |
| Command variants tested | 3 (NOP, UPDATE_PROGRAM, UPDATE_SCHEDULE) | 5 | 60% |
| **Combined message/command variants** | **8** | **12** | **67%** |
| Design error variants tested | 4 (FrameTooLarge, TooShort, InvalidMsgType, MissingField) | 8 | 50% |
| Wire-format inspection tests | 1 (T-P032, Wake only) | ≥3 needed (Wake, Command/NOP, Command/Update) | 33% |
| Protocol spec sections with test coverage | §3 (frame), §4 (partial), §5.1–§5.4, §5.6–§5.7, §5 program image, §10 | §5.5, §5.2 REBOOT/RUN_EPHEMERAL uncovered | ~85% |

---

## 6. Remediation Plan

Findings are ordered by priority (severity × implementation effort). All remediations are additive — they add tests or design elements without changing existing correct content.

| Priority | Finding(s) | Action | Effort |
|---|---|---|---|
| 1 | F-005, F-014 | Add `PROGRAM_ACK` unit test (T-P025b) and include it in a chunked-transfer integration test (T-P063) | Small |
| 2 | F-003 | Add `RUN_EPHEMERAL` round-trip test (T-P023b) | Small |
| 3 | F-004 | Add `REBOOT` round-trip test (T-P022b) | Small |
| 4 | F-012 | Add COMMAND wire-format inspection tests (T-P032b, T-P032c) for payload nesting and key-5 omission | Small |
| 5 | F-006 | Add `decode_frame` TooLong test (T-P018b) | Trivial |
| 6 | F-010 | Review `GatewayMessage::Command` for `command_type` redundancy; add consistency check or derive from variant | Medium |
| 7 | F-009 | Add `GatewayMessage::decode` invalid msg_type test (T-P031c) | Trivial |
| 8 | F-007 | Add `InvalidFieldType` test (T-P030b) | Trivial |
| 9 | F-008 | Add `CborError` test for garbled input (T-P031b) | Trivial |
| 10 | F-013 | Add direction-bit assertion test for all MSG_* constants | Trivial |
| 11 | F-011 | Add `key_hint_from_psk()` to protocol crate design; document derivation in protocol spec §3.1.1 | Small |
| 12 | F-001, F-002 | Add requirement IDs to protocol.md; add `Validates:` fields to validation test cases | Medium |

After remediation, the test count would increase from 41 to approximately 51, and message/command variant coverage would reach 100%.

---

## 7. Prevention

To prevent similar gaps in future specification work:

1. **Enumeration checklist:** When writing test plans for message-oriented protocols, create an explicit checklist of all `msg_type` × `command_type` combinations and ensure each has at least one dedicated test. A coverage matrix (message variant × test case) should be included in the validation document.

2. **Wire-format inspection tests alongside round-trips:** For every message type, include at least one test that inspects the raw encoded bytes — not just round-trip value equality. This catches structural conformance issues that symmetric encode/decode bugs hide.

3. **Error variant coverage:** For every variant in `EncodeError` and `DecodeError` enums, include at least one test that triggers it. A simple checklist at the end of the validation document would suffice.

4. **Requirement IDs from the start:** Assign requirement IDs when writing the protocol spec, even in draft form. This costs minutes but saves hours of audit effort.

5. **Structural redundancy review:** When a struct field duplicates information already encoded in a type (like an enum variant), flag it during design review and decide whether to derive it or validate consistency.

---

## 8. Open Questions

1. **Is `key_hint` derivation intentionally unspecified in the protocol spec?** The formula `u16::from_be_bytes(SHA-256(PSK)[30..32])` appears in the project's coding conventions but not in `protocol.md` or `security.md`. Should it be formalized in the spec, or is it an implementation detail left to each deployment?

2. **Should the protocol crate enforce `command_type`/`CommandPayload` consistency?** This is a design decision. Option (a) — derive from variant — is cleaner but changes the API. Option (b) — validate on encode — is backward-compatible but adds a new error variant.

3. **Should `decode_frame` enforce `MAX_FRAME_SIZE`?** The protocol spec defines 250 as the "reference ESP-NOW transport" maximum. If other transports with different MTUs are supported in the future, should `decode_frame` accept a configurable max, or is 250 a hard protocol limit?

4. **Should the validation plan include negative tests for CBOR string keys?** The protocol spec mandates integer keys. A test that encodes a message with string keys and verifies it either fails to decode or is handled gracefully would guard against accidental use of string keys in the implementation.

---

## 9. Revision History

| Version | Date | Author | Description |
|---|---|---|---|
| 1.0 | 2026-03-20 | Copilot (specification analyst) | Initial audit |
