<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
# Protocol Crate Trifecta Audit — Investigation Report

## 1. Executive Summary

A systematic traceability audit of the `sonde-protocol` crate across three
specification documents (protocol.md, protocol-crate-design.md,
protocol-crate-validation.md) identified **11 findings**: 0 Critical, 3 High,
5 Medium, and 3 Low. The core protocol (frame format, message types,
program images, HMAC authentication) is well-traced across all three
documents with 88% forward traceability. The primary gaps are: (1) two
message types (`PEER_REQUEST` 0x05, `PEER_ACK` 0x84) defined in the
protocol spec but absent from both design and validation; (2) three
design-document modules (`key_hint_from_psk`, modem serial codec, BLE
envelope codec) with zero test coverage in the validation plan; and (3) the
validation plan claims 66 test cases but only 62 are enumerated. No
constraint violations (D6) or acceptance-criteria mismatches (D7) were
found. Recommended action: add the four missing test cases and reconcile the
test count.

## 2. Problem Statement

This audit evaluates the internal consistency of the `sonde-protocol`
specification stack. The protocol spec (`protocol.md`) serves as the
requirements document; `protocol-crate-design.md` is the design document;
`protocol-crate-validation.md` is the validation plan. The goal is to
detect specification drift — gaps, conflicts, and divergence — across
these three artifacts before implementation proceeds further.

## 3. Investigation Scope

- **Documents examined:**
  - Requirements: `docs/protocol.md` (§1–§10, draft)
  - Design: `docs/protocol-crate-design.md` (§1–§11, draft)
  - Validation: `docs/protocol-crate-validation.md` (§1–§7, draft, 62 enumerated test cases)
- **Time period:** Round 2 audit
- **Tools used:** Manual cross-document traceability analysis, identifier enumeration, structural comparison
- **Limitations:**
  - `security.md`, `modem-protocol.md`, and `ble-pairing-protocol.md` are referenced by the audited documents but were **not** included as audit inputs. Findings that depend on those documents are noted.
  - The protocol spec uses prose assertions, not formal REQ-IDs. Traceability is based on section numbers (e.g., `§3.1`, `§5.2.1`), as the validation plan itself acknowledges.
  - Timing/retry behavior (protocol.md §9) and gateway/node verification procedures (§7.2–§7.3) are node/gateway concerns, not protocol-crate scope. These were excluded from the design/validation traceability check by design.

### Artifact Inventory Summary

**Requirements (protocol.md) — 17 protocol-crate-scoped requirement areas:**

| # | Section | Requirement area |
|---|---------|-----------------|
| 1 | §3, §3.1 | Frame header structure, fixed binary layout, big-endian encoding |
| 2 | §3.2 | HMAC trailer (32-byte HMAC-SHA256 over header ∥ payload) |
| 3 | §3.3 | Frame size budget (max 250, header 11, HMAC 32, payload 207) |
| 4 | §4 | Message type direction-bit convention (0x01–0x7F / 0x80–0xFF) |
| 5 | §4.1 | Node→Gateway types: WAKE, GET_CHUNK, PROGRAM_ACK, APP_DATA, PEER_REQUEST |
| 6 | §4.2 | Gateway→Node types: COMMAND, CHUNK, APP_DATA_REPLY, PEER_ACK |
| 7 | §5 | CBOR integer key mapping (14 keys) and forward-compatibility rule |
| 8 | §5.1 | WAKE message fields (firmware_abi_version, program_hash, battery_mv) |
| 9 | §5.2 | COMMAND message structure (command_type, starting_seq, timestamp_ms, nested payload) |
| 10 | §5.2.1 | UPDATE_PROGRAM / RUN_EPHEMERAL payload fields |
| 11 | §5.2.2 | UPDATE_SCHEDULE payload fields |
| 12 | §5.3–5.4 | GET_CHUNK / CHUNK message fields |
| 13 | §5.5–5.7 | PROGRAM_ACK, APP_DATA, APP_DATA_REPLY message fields |
| 14 | §5 (image) | Program image CBOR format, separate keyspace, map definitions |
| 15 | §5 (image) | Deterministic encoding (RFC 8949 §4.2), program_hash = SHA-256 of image |
| 16 | §7.1 | HMAC-SHA256 computation |
| 17 | §10 | Protocol evolution: unknown keys ignored, firmware_abi_version gating |

**Design (protocol-crate-design.md) — 9 modules:**

| # | Section | Module |
|---|---------|--------|
| 1 | §3 | Constants (frame sizes, msg_type codes, command codes, CBOR keys) |
| 2 | §4 | Frame header (`FrameHeader`, `to_bytes`, `from_bytes`) |
| 3 | §5 | Frame codec (`encode_frame`, `decode_frame`, `verify_frame`, `HmacProvider` trait) |
| 4 | §5.5 | `key_hint_from_psk()` derivation function |
| 5 | §6 | Message types (`NodeMessage`, `GatewayMessage`, `CommandPayload`) |
| 6 | §7 | Program image (`ProgramImage`, `MapDef`, `encode_deterministic`, `program_hash`, `Sha256Provider` trait) |
| 7 | §8 | Error types (`EncodeError`, `DecodeError`) |
| 8 | §9 | Chunking helpers (`chunk_count`, `get_chunk`) |
| 9 | §10 | Modem serial codec (`modem.rs`) |
| 10 | §11 | BLE envelope codec (`ble_envelope.rs`) |

**Validation (protocol-crate-validation.md) — 62 test cases enumerated (66 claimed):**

| Section | ID range | Count | Area |
|---------|----------|-------|------|
| §2 | T-P001 – T-P004 | 4 | Frame header |
| §3 | T-P010 – T-P019c | 13 | Frame codec |
| §4 | T-P020 – T-P039 | 20 | Message encoding |
| §5 | T-P040 – T-P049 | 10 | Program image |
| §6 | T-P050 – T-P055 | 6 | Chunking helpers |
| §7 | T-P060 – T-P066 | 7 | Full integration |
| §8 | T-P070 – T-P071 | 2 | Additional |
| | **Total** | **62** | |

## 4. Findings

### Finding F-001: `key_hint_from_psk()` has zero test coverage

- **Severity:** High
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:**
  - Design: `protocol-crate-design.md` §5.5 — defines `key_hint_from_psk(psk: &[u8; 32], sha: &impl Sha256Provider) -> u16`
  - Validation: no test case references `key_hint_from_psk` or `key_hint` derivation
- **Description:** The design document defines a cryptographic derivation
  function that computes `key_hint = u16::from_be_bytes(SHA-256(PSK)[30..32])`.
  This function is used by both the gateway and node to derive the 2-byte
  key_hint from a pre-shared key. No test case in the validation plan
  exercises this function.
- **Evidence:** Searched all 62 test cases (T-P001 through T-P071) for
  references to `key_hint_from_psk`, `key_hint` derivation, `SHA-256(PSK)`,
  or bytes `[30..32]`. No matches found. The only `key_hint` references in
  the validation plan are in T-P001 through T-P004 (header round-trip), which
  use literal `key_hint` values, not derived ones.
- **Root Cause:** The function was added to the design as a consolidation
  convenience (design §5.5 rationale) but a corresponding test case was
  never added to the validation plan.
- **Impact:** A bug in the derivation formula (e.g., wrong byte range,
  wrong endianness) would cause gateway and node to compute different
  `key_hint` values, breaking key lookup for all nodes. This is a
  high-impact, low-probability defect class.
- **Remediation:** Add test cases covering:
  (a) known-answer test with a fixed PSK and expected `key_hint`;
  (b) two different PSKs producing different `key_hint` values;
  (c) verify that bytes [30..32] (not [0..2]) of the SHA-256 hash are used.
- **Confidence:** High

---

### Finding F-002: Modem serial codec has zero test coverage in validation plan

- **Severity:** High
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:**
  - Design: `protocol-crate-design.md` §10 — defines `modem.rs` module with `ModemMessage`, `encode_modem_frame`, `decode_modem_frame`, `FrameDecoder`
  - Validation: no test case references modem, serial, or `FrameDecoder`
- **Description:** The design document describes an entire module
  (`modem.rs`) implementing the gateway ↔ modem serial framing protocol.
  This module includes encoding/decoding functions, a streaming frame
  decoder, and multiple message types. The validation plan contains zero
  test cases for any of this functionality.
- **Evidence:** Searched validation plan for "modem", "serial",
  "FrameDecoder", "MODEM_MSG". No matches found.
- **Root Cause:** The modem serial codec's requirements originate from
  `modem-protocol.md` (referenced in design §10), not from `protocol.md`.
  The validation plan appears scoped to `protocol.md` requirements only, but
  the design document places this module in the same crate, creating a
  coverage gap.
- **Impact:** Wire-format incompatibilities between gateway and modem would
  not be caught by the validation plan. Since this codec carries all
  node ↔ gateway traffic over the USB bridge, defects here would break the
  entire radio path.
- **Remediation:** Either (a) add modem codec test cases to
  `protocol-crate-validation.md`, or (b) create a separate
  `modem-protocol-validation.md` and reference it from the protocol crate
  validation plan with an explicit scope note.
- **Confidence:** High

---

### Finding F-003: BLE envelope codec has zero test coverage in validation plan

- **Severity:** High
- **Category:** D2_UNTESTED_REQUIREMENT
- **Location:**
  - Design: `protocol-crate-design.md` §11 — defines `ble_envelope.rs` with `parse_ble_envelope` and `encode_ble_envelope`
  - Validation: no test case references BLE, envelope, or TLV
- **Description:** The design defines a BLE GATT envelope codec (Type-Length-Value
  format) used for pairing protocol messages. The validation plan contains
  no test cases for `parse_ble_envelope()` or `encode_ble_envelope()`.
- **Evidence:** Searched validation plan for "ble", "envelope", "TLV",
  "parse_ble", "encode_ble". No matches found.
- **Root Cause:** Same pattern as F-002 — requirements originate from
  `ble-pairing-protocol.md`, not `protocol.md`, but the module lives in the
  protocol crate.
- **Impact:** Defects in envelope parsing (e.g., endianness of the 2-byte
  length field, off-by-one in body extraction) would break BLE pairing.
- **Remediation:** Add test cases for: (a) round-trip encode/parse,
  (b) empty body, (c) max-length body (`u16::MAX`), (d) truncated input
  rejection, (e) trailing bytes rejection (per design: "Rejects truncated
  or trailing-byte inputs").
- **Confidence:** High

---

### Finding F-004: `PEER_REQUEST` (0x05) and `PEER_ACK` (0x84) absent from design and untested

- **Severity:** Medium
- **Category:** D2_UNTESTED_REQUIREMENT (primary), D1_UNTRACED_REQUIREMENT (secondary)
- **Location:**
  - Requirements: `protocol.md` §4.1 (`PEER_REQUEST` 0x05), §4.2 (`PEER_ACK` 0x84)
  - Design: `protocol-crate-design.md` §3 constants — absent; §6.1 `NodeMessage` — no `PeerRequest` variant; §6.2 `GatewayMessage` — no `PeerAck` variant
  - Validation: no test case references PEER_REQUEST, PEER_ACK, 0x05, or 0x84
- **Description:** The protocol spec defines two BLE-pairing message types
  (`PEER_REQUEST` and `PEER_ACK`) in the message type tables. These are
  absent from both the design constants and the message enums, and have no
  test coverage.
- **Evidence:**
  - `protocol.md` §4.1: `| 0x05 | PEER_REQUEST | BLE pairing peer request. See ble-pairing-protocol.md. |`
  - `protocol.md` §4.2: `| 0x84 | PEER_ACK | BLE pairing peer acknowledgement. See ble-pairing-protocol.md. |`
  - Design §3 constants list: `MSG_APP_DATA: u8 = 0x04` is the last node→gateway constant; `MSG_APP_DATA_REPLY: u8 = 0x83` is the last gateway→node constant. No `0x05` or `0x84`.
  - Design §6 enums: `NodeMessage` has 4 variants (Wake, GetChunk, ProgramAck, AppData); `GatewayMessage` has 3 variants (Command, Chunk, AppDataReply). No peer variants.
- **Root Cause:** The BLE pairing protocol is specified in a separate
  document (`ble-pairing-protocol.md`), and the protocol crate design
  appears to have been scoped to the core protocol only. However, the
  design *does* include the BLE envelope codec (§11), creating an
  inconsistency: the transport-level BLE encoding is designed but the
  message-level types are not.
- **Impact:** The protocol crate will reject `msg_type = 0x05` and
  `msg_type = 0x84` as `InvalidMsgType` during decoding, blocking BLE
  pairing functionality.
- **Remediation:** Add `MSG_PEER_REQUEST: u8 = 0x05` and
  `MSG_PEER_ACK: u8 = 0x84` to constants. Add `PeerRequest` and `PeerAck`
  variants (with fields defined in `ble-pairing-protocol.md`) to the message
  enums. Add corresponding round-trip and boundary test cases.
- **Confidence:** High

---

### Finding F-005: CBOR `uint` narrowed to Rust `u32` without specification basis

- **Severity:** Medium
- **Category:** D5_ASSUMPTION_DRIFT
- **Location:**
  - Requirements: `protocol.md` §5.1–§5.7 — all numeric payload fields specified as CBOR `uint`
  - Design: `protocol-crate-design.md` §6 — `firmware_abi_version: u32`, `battery_mv: u32`, `chunk_index: u32`, `program_size: u32`, `chunk_size: u32`, `chunk_count: u32`, `interval_s: u32`, `map_type: u32`, `key_size: u32`, `value_size: u32`, `max_entries: u32`
- **Description:** The protocol spec uses CBOR type `uint` (unbounded
  unsigned integer) for 11 payload fields. The design narrows all of these
  to Rust `u32`. Only `starting_seq` and `timestamp_ms` use `u64` (matching
  their 64-bit semantic requirements). The narrowing is undocumented — no
  assumption or constraint in either document states that these fields will
  never exceed 2³²−1.
- **Evidence:** Protocol.md §5.1: `firmware_abi_version | uint | Yes`.
  Design §6.1: `firmware_abi_version: u32`. The design does not note the
  narrowing or justify the choice. T-P039 tests `battery_mv = u32::MAX` to
  confirm the boundary, but no test verifies behavior when a received CBOR
  value exceeds `u32::MAX`.
- **Root Cause:** The design implicitly assumes all field values fit in 32
  bits, which is reasonable for current usage but creates an undocumented
  contract.
- **Impact:** Low for current usage (no realistic field exceeds 2³²−1), but
  a future extension (e.g., sub-millisecond battery readings, larger chunk
  counts) could introduce silent truncation bugs if the assumption is
  forgotten.
- **Remediation:** Add a note to the design document (§6, "CBOR encoding
  rules" or a new "Type mapping" subsection) documenting that protocol
  `uint` fields are represented as `u32` in Rust, and that decoding a value
  exceeding `u32::MAX` should return `DecodeError::InvalidFieldType`. Add a
  test case verifying this behavior.
- **Confidence:** High

---

### Finding F-006: `key_hint` derivation formula absent from protocol spec

- **Severity:** Medium
- **Category:** D3_ORPHANED_DESIGN_DECISION
- **Location:**
  - Requirements: `protocol.md` §3.1.1 — describes `key_hint` semantics (optimization hint, collision handling) but does **not** specify how `key_hint` is derived from the PSK
  - Design: `protocol-crate-design.md` §5.5 — defines `key_hint = u16::from_be_bytes(SHA-256(PSK)[30..32])`
- **Description:** The design introduces a specific cryptographic derivation
  formula for `key_hint` that has no originating requirement in the protocol
  spec. The protocol spec describes what `key_hint` is used for but never
  specifies how it is computed.
- **Evidence:** Protocol.md §3.1.1 full text discusses `key_hint` semantics
  and collision handling but contains no formula, no SHA-256 reference, and
  no mention of bytes [30..32]. The design §5.5 introduces the full formula
  with the rationale "This consolidates the key_hint derivation formula."
- **Root Cause:** The derivation formula was likely specified in
  `security.md` or the gateway requirements and was not backported to
  `protocol.md`.
- **Impact:** Medium — without the formula in the protocol spec, an
  independent implementer of a Sonde-compatible node could derive
  `key_hint` differently (e.g., using SHA-256 bytes [0..2] instead of
  [30..31]), causing authentication lookup failures.
- **Remediation:** Add a `key_hint` derivation subsection to `protocol.md`
  §3.1.1 specifying the formula:
  `key_hint = u16::from_be_bytes(SHA-256(PSK)[30..32])`.
- **Confidence:** High

---

### Finding F-007: Modem serial codec in design has no requirements in protocol spec

- **Severity:** Medium
- **Category:** D3_ORPHANED_DESIGN_DECISION
- **Location:**
  - Requirements: `protocol.md` — no mention of modem, serial, or USB-CDC framing
  - Design: `protocol-crate-design.md` §10 — full module specification for `modem.rs`
- **Description:** The design document includes a complete module
  specification for the modem serial codec, but the protocol spec
  (`protocol.md`) does not define this as a requirement. The design
  references `modem-protocol.md` as the source, which was not provided as
  an audit input.
- **Evidence:** Searched `protocol.md` for "modem", "serial", "USB",
  "length-prefixed". No matches except transport-level references in §3.3
  (ESP-NOW frame budget) and §9.3 (ESP-NOW with USB-CDC modem bridge
  timeout).
- **Root Cause:** The modem protocol is specified in a separate requirements
  document, but the implementation lives in the `sonde-protocol` crate.
  This creates a scope mismatch: the design doc covers the full crate, but
  the requirements doc covers only the node ↔ gateway protocol.
- **Impact:** Low — this is reasonable architectural infrastructure
  (shared codec for wire-format compatibility). The design decision is
  sound, but the scope boundary should be explicit.
- **Remediation:** Add a scope note to `protocol-crate-design.md` §10
  stating that this module's requirements come from `modem-protocol.md`,
  not `protocol.md`. Alternatively, add a brief reference in `protocol.md`
  acknowledging the modem framing layer.
- **Confidence:** High

---

### Finding F-008: BLE envelope codec in design has no requirements in protocol spec

- **Severity:** Medium
- **Category:** D3_ORPHANED_DESIGN_DECISION
- **Location:**
  - Requirements: `protocol.md` — no mention of BLE envelope, TLV, or GATT framing
  - Design: `protocol-crate-design.md` §11 — `ble_envelope.rs` module specification
- **Description:** Same pattern as F-007. The BLE envelope codec is
  designed as a protocol crate module, but its requirements come from
  `ble-pairing-protocol.md` (not provided as audit input), not from
  `protocol.md`.
- **Evidence:** Searched `protocol.md` for "BLE", "envelope", "GATT",
  "TLV". The only BLE references are in §4.1 (`PEER_REQUEST`: "See
  `ble-pairing-protocol.md`") and §4.2 (`PEER_ACK`: same).
- **Root Cause:** Same scope mismatch as F-007.
- **Impact:** Low — reasonable infrastructure for shared codec.
- **Remediation:** Same as F-007: add explicit scope references.
- **Confidence:** High

---

### Finding F-009: Validation plan claims 66 test cases but only 62 are enumerated

- **Severity:** Low
- **Category:** D5_ASSUMPTION_DRIFT
- **Location:**
  - Validation: `protocol-crate-validation.md` §1 — "There are 66 test cases total"
  - Validation: §2–§8 — 62 test cases enumerated (T-P001–T-P004, T-P010–T-P019c, T-P020–T-P039, T-P040–T-P049, T-P050–T-P055, T-P060–T-P066, T-P070–T-P071)
- **Description:** The overview section states the document contains 66 test
  cases, but a complete enumeration yields only 62 distinct test case IDs.
  The 4-test discrepancy is unexplained.
- **Evidence:** Count by section:
  - §2 Frame header: 4 (T-P001–T-P004)
  - §3 Frame codec: 13 (T-P010–T-P019c)
  - §4 Message encoding: 20 (T-P020–T-P039)
  - §5 Program image: 10 (T-P040–T-P049)
  - §6 Chunking helpers: 6 (T-P050–T-P055)
  - §7 Integration: 7 (T-P060–T-P066)
  - §8 Additional: 2 (T-P070–T-P071)
  - Total: 62
- **Root Cause:** [INFERRED] The count may reflect a previous revision that
  included 4 additional test cases (possibly for key_hint derivation, modem,
  or BLE modules) that were subsequently removed or deferred without
  updating the count.
- **Impact:** Stakeholders relying on the stated count may overestimate test
  coverage.
- **Remediation:** Either update the count to 62, or add the 4 missing test
  cases (candidates: key_hint_from_psk tests, modem codec tests, BLE
  envelope tests — see F-001, F-002, F-003).
- **Confidence:** High (count is mechanically verifiable)

---

### Finding F-010: Design inconsistency — BLE envelope codec included but BLE message types excluded

- **Severity:** Low
- **Category:** D5_ASSUMPTION_DRIFT
- **Location:**
  - Design: `protocol-crate-design.md` §11 — includes `ble_envelope.rs` module
  - Design: `protocol-crate-design.md` §3, §6 — excludes `PEER_REQUEST`/`PEER_ACK` constants and enum variants
- **Description:** The design document includes the BLE envelope codec
  (transport-level encoding for BLE GATT messages) but omits the
  `PEER_REQUEST` and `PEER_ACK` message types that use it. This creates an
  internal inconsistency: the lower-layer encoding is designed but the
  message-layer types that ride on top of it are not.
- **Evidence:**
  - §11 includes `parse_ble_envelope` and `encode_ble_envelope`
  - §3 msg_type constants end at `MSG_APP_DATA (0x04)` and `MSG_APP_DATA_REPLY (0x83)`
  - §6 `NodeMessage` has no `PeerRequest` variant; `GatewayMessage` has no `PeerAck` variant
- **Root Cause:** [INFERRED] The BLE envelope codec was added to the design
  in a later pass to support shared wire-format code, but the corresponding
  message types were deferred to a future design iteration.
- **Impact:** Low — the envelope codec is usable independently, and the
  message types can be added later. But the inconsistency may confuse
  implementers about whether BLE pairing is in scope.
- **Remediation:** Either (a) add `PEER_REQUEST`/`PEER_ACK` message types
  to the design (per F-004), or (b) add a note to §11 stating that the BLE
  message types are defined in a separate design document.
- **Confidence:** High

---

### Finding F-011: No test for `u32` overflow when decoding CBOR `uint` fields

- **Severity:** Low
- **Category:** D7_ACCEPTANCE_CRITERIA_MISMATCH
- **Location:**
  - Design: `protocol-crate-design.md` §6 — fields typed as `u32`
  - Design: `protocol-crate-design.md` §8 — `DecodeError::InvalidFieldType(u64)`
  - Validation: T-P039 — tests `u32::MAX` boundary but not overflow beyond it
- **Description:** T-P039 validates that `battery_mv = u32::MAX` round-trips
  correctly, confirming the upper boundary. However, no test verifies the
  decoder's behavior when a CBOR `uint` value exceeds `u32::MAX` is received
  for a `u32`-typed field. The design's `DecodeError::InvalidFieldType`
  variant exists to report type mismatches, but no test exercises the
  overflow-to-error path.
- **Evidence:** T-P039 procedure step 1: "Encode a Wake with
  `battery_mv = u32::MAX`." No test encodes a value like `u32::MAX as u64 + 1`
  for a `u32` field and asserts error behavior.
- **Root Cause:** The acceptance criterion for integer field decoding
  implicitly includes overflow handling, but the validation plan only tests
  the valid boundary, not the invalid boundary+1.
- **Impact:** Low — a decoder that silently truncates an oversized value
  (instead of returning an error) would corrupt field data.
- **Remediation:** Add a test case: encode a WAKE with `battery_mv` as a
  raw CBOR `u64` value exceeding `u32::MAX`, then decode and assert
  `DecodeError::InvalidFieldType(KEY_BATTERY_MV)`.
- **Confidence:** Medium — depends on whether `ciborium` auto-truncates or
  errors on integer overflow. Verify by testing.

## 5. Root Cause Analysis

### Coverage Metrics

**Forward traceability (Requirements → Design):**

| Requirement area | Section | Traced to design? |
|-----------------|---------|:-:|
| Frame header structure | §3, §3.1 | ✓ |
| HMAC trailer | §3.2 | ✓ |
| Frame size budget | §3.3 | ✓ |
| Direction-bit convention | §4 | ✓ |
| WAKE, GET_CHUNK, PROGRAM_ACK, APP_DATA | §4.1 | ✓ |
| **PEER_REQUEST** | **§4.1** | **✗** |
| COMMAND, CHUNK, APP_DATA_REPLY | §4.2 | ✓ |
| **PEER_ACK** | **§4.2** | **✗** |
| CBOR integer keys + forward compat | §5 | ✓ |
| Message fields (§5.1–§5.7) | §5.1–5.7 | ✓ |
| Program image format | §5 (image) | ✓ |
| Deterministic CBOR encoding | §5 (image) | ✓ |
| HMAC-SHA256 computation | §7.1 | ✓ |
| Protocol evolution | §10 | ✓ |

**Result: 15/17 = 88% traced to design.**

**Forward traceability (Requirements → Tests):**

Same 17 areas. 15 of 17 have at least one test case. PEER_REQUEST and
PEER_ACK have zero test cases.

**Result: 15/17 = 88% traced to tests.**

**Backward traceability (Design → Requirements):**

| Design module | Section | Traced to protocol.md? |
|--------------|---------|:-:|
| Constants | §3 | ✓ |
| Frame header | §4 | ✓ |
| Frame codec | §5 | ✓ |
| **`key_hint_from_psk`** | **§5.5** | **✗** (no requirement in protocol.md) |
| Message types | §6 | ✓ |
| Program image | §7 | ✓ |
| Error types | §8 | ✓ |
| Chunking helpers | §9 | ✓ |
| **Modem serial codec** | **§10** | **✗** (requirements in modem-protocol.md) |
| **BLE envelope codec** | **§11** | **✗** (requirements in ble-pairing-protocol.md) |

**Result: 7/10 = 70% traced to protocol.md.** The 3 untraced modules
reference other specification documents, so this rate reflects the scope
mismatch between protocol.md (node ↔ gateway wire protocol) and the crate
(which also hosts modem/BLE codecs).

**Backward traceability (Tests → Requirements):**

All 62 enumerated test cases include a "Validates" field referencing
`protocol.md` section numbers.

**Result: 62/62 = 100% traced to requirements.**

**Design modules with zero test coverage in validation plan:**

| Module | Design section | Test cases |
|--------|---------------|:----------:|
| `key_hint_from_psk` | §5.5 | 0 |
| Modem serial codec | §10 | 0 |
| BLE envelope codec | §11 | 0 |

**Assumption consistency:**

| # | Assumption | Requirements | Design | Aligned? |
|---|-----------|:------------:|:------:|:--------:|
| 1 | PSK is 32 bytes | Not stated | §5.5: `psk: &[u8; 32]` | [ASSUMPTION] — likely from security.md |
| 2 | Numeric fields fit in u32 | Not stated (CBOR `uint`) | §6: u32 types | ✗ Drift (F-005) |
| 3 | Unknown CBOR keys ignored | §10 | §6.4 | ✓ |
| 4 | Constant-time HMAC comparison | Not stated (implied by §7) | §5.1: "MUST use constant-time" | ✓ |

**Overall assessment:** High confidence — the core protocol specification
stack is internally consistent with 2 untraced message types, 3 untested
design modules, and several minor documentation gaps. No constraint
violations or acceptance-criteria mismatches of high severity were found.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|:--------:|---------|-----------------|:------:|------|
| 1 | F-001 | Add `key_hint_from_psk` test cases (known-answer, byte-range, multi-PSK) | S | Low — pure function, easy to test |
| 2 | F-004 | Add `PEER_REQUEST`/`PEER_ACK` to design constants + message enums + test cases | M | Low — requires reading `ble-pairing-protocol.md` for field defs |
| 3 | F-006 | Add `key_hint` derivation formula to `protocol.md` §3.1.1 | S | Low — documentation only |
| 4 | F-002 | Add modem serial codec test cases to validation plan (or separate doc) | M | Low |
| 5 | F-003 | Add BLE envelope codec test cases to validation plan (or separate doc) | S | Low |
| 6 | F-005 | Document u32 narrowing assumption in design; add overflow test | S | Low |
| 7 | F-009 | Correct test case count in validation plan §1 (66 → 62, or add missing tests) | S | Low |
| 8 | F-007 | Add scope note to design §10 referencing `modem-protocol.md` | S | Low |
| 9 | F-008 | Add scope note to design §11 referencing `ble-pairing-protocol.md` | S | Low |
| 10 | F-010 | Resolve BLE scope inconsistency (add message types or add scope note) | S | Low |
| 11 | F-011 | Add u32 overflow decode test case | S | Low |

## 7. Prevention

- **Specification template:** Add a "Scope boundary" section to all design
  and validation documents, explicitly listing which requirements documents
  they cover. This prevents the scope mismatch seen with modem/BLE modules.
- **Traceability matrix:** Add a formal traceability matrix to the
  validation plan mapping each requirement area to its test case(s). The
  current per-test "Validates" references are good but do not make gaps
  visible at a glance.
- **Test count automation:** Add a CI check that counts `### T-P` headings
  in the validation plan and compares to the stated total, preventing
  count drift (F-009).
- **New module checklist:** When adding a new module to the design
  document, require at minimum: (a) a link to its requirements source,
  (b) at least one test case in the validation plan, and (c) msg_type
  constants if the module introduces new message types.

## 8. Open Questions

1. **Where is the `key_hint` derivation formula specified?** The protocol
   spec does not include it. It may be in `security.md` or
   `gateway-requirements.md`. Resolving this determines whether F-006 is a
   requirements gap (add to `protocol.md`) or a cross-reference gap (add a
   pointer to the other document). **Resolution:** Check `security.md` for
   the derivation formula.

2. **Are the 4 missing test cases (66 − 62) intentionally removed or
   planned?** The discrepancy may reflect deferred test cases for
   key_hint, modem, or BLE functionality. **Resolution:** Check version
   control history of `protocol-crate-validation.md` for removed test cases.

3. **Should `PEER_REQUEST`/`PEER_ACK` be designed and tested in the
   protocol crate or in a separate BLE pairing crate?** The answer
   affects whether F-004 is a real gap or a scope decision. **Resolution:**
   Decide with the project team whether the protocol crate is the right
   home for BLE pairing message types.

4. **What should the decoder do when a CBOR `uint` value exceeds the Rust
   `u32` range for a `u32`-typed field?** The design's `InvalidFieldType`
   error is the obvious choice, but the ciborium library may handle this
   differently (e.g., returning a parse error vs. truncating). **Resolution:**
   Write the test case from F-011 and observe ciborium's behavior.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-26 | Specification Analyst (AI-assisted) | Initial audit — Round 2 |
