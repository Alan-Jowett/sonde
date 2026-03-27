<!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->

# Protocol Crate Code Compliance Audit — Investigation Report

## 1. Executive Summary

The `sonde-protocol` crate was audited against the protocol requirements
(`protocol.md`) and the crate design specification
(`protocol-crate-design.md`). The implementation is substantially
compliant: frame codec, header serialization, all seven core message
types, program image encoding/decoding, chunking helpers, key-hint
derivation, BLE envelope codec, and modem serial codec are implemented
correctly. Ten findings were identified — no critical issues. Three
medium-severity constraint violations concern API signature drift and
missing validation. Two medium-severity gaps concern unimplemented
`PEER_REQUEST`/`PEER_ACK` message types defined in the protocol spec.
Five low/informational findings document minor undocumented additions
and cosmetic deviations. Recommended actions: add `PEER_REQUEST`/`PEER_ACK`
message variants, align `encode_deterministic` return type, and add
NOP/REBOOT payload-presence validation.

## 2. Problem Statement

This audit determines whether the `sonde-protocol` crate source code
faithfully implements the behaviors, types, constraints, and API contracts
specified in:

- **Requirements**: `docs/protocol.md` — wire-level protocol between
  nodes and the gateway.
- **Design**: `docs/protocol-crate-design.md` — architecture and API of
  the shared `sonde-protocol` Rust crate.

The audit focuses on all protocol requirements: frame format, message
types, CBOR encoding, program images, chunking, error types, traits,
key-hint derivation, and supplementary codecs (modem, BLE envelope).

## 3. Investigation Scope

- **Codebase / components examined**:
  - `crates/sonde-protocol/Cargo.toml`
  - `crates/sonde-protocol/src/lib.rs`
  - `crates/sonde-protocol/src/constants.rs`
  - `crates/sonde-protocol/src/header.rs`
  - `crates/sonde-protocol/src/codec.rs`
  - `crates/sonde-protocol/src/messages.rs`
  - `crates/sonde-protocol/src/program_image.rs`
  - `crates/sonde-protocol/src/error.rs`
  - `crates/sonde-protocol/src/traits.rs`
  - `crates/sonde-protocol/src/chunk.rs`
  - `crates/sonde-protocol/src/modem.rs`
  - `crates/sonde-protocol/src/ble_envelope.rs`
- **Specification documents examined**:
  - `docs/protocol.md` (requirements)
  - `docs/protocol-crate-design.md` (design)
- **Tools used**: Manual static analysis, line-by-line comparison of
  specifications against source code.
- **Limitations**: Runtime behavior and cross-crate integration were not
  tested. Modem serial codec was examined for structural completeness
  against design §10 but not against `modem-protocol.md` (separate audit
  scope). BLE pairing protocol (`ble-pairing-protocol.md`) was not
  examined — only the BLE envelope codec documented in design §11.

## 4. Findings

### Finding F-001: `ProgramImage::encode_deterministic` returns `Result` instead of infallible `Vec<u8>`

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**:
  - Spec: `protocol-crate-design.md` §7.2
  - Code: `crates/sonde-protocol/src/program_image.rs:31`
- **Description**: The design specifies `encode_deterministic` with
  signature `pub fn encode_deterministic(&self) -> Vec<u8>` (infallible).
  The implementation returns `Result<Vec<u8>, EncodeError>`, which changes
  the API contract for every caller.
- **Evidence**:
  - Design §7.2: `pub fn encode_deterministic(&self) -> Vec<u8> { ... }`
  - Code: `pub fn encode_deterministic(&self) -> Result<Vec<u8>, EncodeError>`
- **Root Cause**: The implementation wraps ciborium serialization errors,
  which are theoretically possible but should not occur for well-formed
  in-memory `ProgramImage` values. The design assumed infallibility.
- **Impact**: All callers must handle an error path that the design says
  cannot occur. This complicates gateway and test code and diverges from
  the documented API contract.
- **Remediation**: Either (a) make the function infallible by panicking
  on ciborium errors (which indicates a library bug, not a user error),
  matching the design; or (b) update the design spec to document the
  fallible return type and rationale.
- **Confidence**: High

### Finding F-002: COMMAND decode does not validate absence of `payload` (key 5) for NOP and REBOOT

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**:
  - Spec: `protocol-crate-design.md` §6.3
  - Code: `crates/sonde-protocol/src/messages.rs:320–326`
- **Description**: The design states that `decode()` must validate that
  NOP and REBOOT commands do not contain key 5 (`payload`). The code
  silently ignores any key 5 present in NOP/REBOOT COMMAND messages
  without returning an error.
- **Evidence**:
  - Design §6.3: "validates that the nested `payload` (key 5) structure
    is consistent (e.g., `CMD_NOP` and `CMD_REBOOT` must not contain
    key 5)"
  - Code (messages.rs:320–326):
    ```rust
    CMD_NOP | CMD_REBOOT => {
        if command_type == CMD_REBOOT {
            CommandPayload::Reboot
        } else {
            CommandPayload::Nop
        }
    }
    ```
    No check for the presence of `KEY_PAYLOAD` in `fields`.
- **Root Cause**: The implementation follows the general "ignore unknown
  keys" forward-compatibility pattern but does not distinguish between
  truly unknown keys and structurally invalid payload presence.
- **Impact**: A malformed COMMAND with `command_type = NOP` but a
  spurious `payload` field would be silently accepted. In a security
  context, this could mask injection of unexpected data into a NOP frame.
  Practical impact is low since NOP processing ignores the payload, but
  the design constraint is explicit.
- **Remediation**: After matching `CMD_NOP | CMD_REBOOT`, check whether
  `get_field(&fields, KEY_PAYLOAD).is_ok()` and if so return a
  `DecodeError` (e.g., `InvalidFieldType(KEY_PAYLOAD)` or a new variant).
- **Confidence**: High

### Finding F-003: Cargo.toml feature structure omits `alloc` feature gate

- **Severity**: Medium
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**:
  - Spec: `protocol-crate-design.md` §2
  - Code: `crates/sonde-protocol/Cargo.toml:7–9`
- **Description**: The design specifies three features:
  `default = ["alloc"]`, `alloc = []`, and `std = ["alloc"]`. The code
  has only `default = []` and `std = []` with no `alloc` feature. The
  crate unconditionally uses `extern crate alloc;` (lib.rs:6), making
  `alloc` a hard requirement rather than an opt-in feature.
- **Evidence**:
  - Design §2:
    ```toml
    [features]
    default = ["alloc"]
    alloc = []
    std = ["alloc"]
    ```
  - Code Cargo.toml:
    ```toml
    [features]
    default = []
    std = []
    ```
  - Code lib.rs:6: `extern crate alloc;` (unconditional)
- **Root Cause**: The design envisioned conditional alloc support, but
  since both the gateway (std) and node (ESP-IDF std) always have an
  allocator, the feature gate was never implemented.
- **Impact**: The crate cannot be used in a `no_std` + `no_alloc`
  environment. This violates the design's stated configurability, though
  the practical impact is low since all current consumers have an
  allocator. Additionally, the ciborium dependency is specified without
  `features = ["alloc"]` (design specifies this), which could cause
  issues if ciborium changes its default feature set.
- **Remediation**: Either (a) add the `alloc` feature gate per the
  design and gate `extern crate alloc` behind `#[cfg(feature = "alloc")]`,
  or (b) update the design to document that alloc is always required and
  remove the feature gate from the spec.
- **Confidence**: High

### Finding F-004: PEER_REQUEST (0x05) message type has no encode/decode implementation

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**:
  - Spec: `protocol.md` §4.1 — msg_type table row for `0x05 PEER_REQUEST`
  - Code: `crates/sonde-protocol/src/messages.rs` — `NodeMessage` enum
    (lines 13–29) and `NodeMessage::decode` (lines 207–226)
- **Description**: The protocol specification defines PEER_REQUEST
  (0x05) as a node → gateway message type for BLE pairing peer requests.
  The constant `MSG_PEER_REQUEST` exists in `constants.rs:21`, and CBOR
  key constants (`PEER_REQ_KEY_PAYLOAD`) are defined at line 57. However,
  the `NodeMessage` enum has no `PeerRequest` variant, and
  `NodeMessage::decode` returns `InvalidMsgType(0x05)` for this
  message type.
- **Evidence**:
  - Protocol §4.1: "`0x05` | `PEER_REQUEST` | BLE pairing peer request."
  - constants.rs:21: `pub const MSG_PEER_REQUEST: u8 = 0x05;`
  - constants.rs:57: `pub const PEER_REQ_KEY_PAYLOAD: u64 = 1;`
  - messages.rs:224: `_ => Err(DecodeError::InvalidMsgType(msg_type))` —
    catches 0x05.
- **Root Cause**: The design document (`protocol-crate-design.md`) was
  written before PEER_REQUEST was added to the protocol spec, so it does
  not include this message type. The constants were added to keep the
  code forward-compatible, but the message type implementation was
  deferred.
- **Impact**: The protocol crate cannot encode or decode PEER_REQUEST
  messages, blocking BLE pairing support that depends on this message
  type.
- **Remediation**: Add a `PeerRequest { encrypted_payload: Vec<u8> }`
  variant to `NodeMessage` with encode/decode support using the existing
  `PEER_REQ_KEY_PAYLOAD` constant. Update the design doc to include
  this message type.
- **Confidence**: High

### Finding F-005: PEER_ACK (0x84) message type has no encode/decode implementation

- **Severity**: Medium
- **Category**: D8_UNIMPLEMENTED_REQUIREMENT
- **Location**:
  - Spec: `protocol.md` §4.2 — msg_type table row for `0x84 PEER_ACK`
  - Code: `crates/sonde-protocol/src/messages.rs` — `GatewayMessage`
    enum (lines 65–79) and `GatewayMessage::decode` (lines 311–372)
- **Description**: The protocol specification defines PEER_ACK (0x84)
  as a gateway → node message type for BLE pairing peer acknowledgement.
  The constant `MSG_PEER_ACK` exists in `constants.rs:27`, and CBOR key
  constants (`PEER_ACK_KEY_STATUS`, `PEER_ACK_KEY_PROOF`) are defined at
  lines 58–59. However, `GatewayMessage` has no `PeerAck` variant, and
  `GatewayMessage::decode` returns `InvalidMsgType(0x84)`.
- **Evidence**:
  - Protocol §4.2: "`0x84` | `PEER_ACK` | BLE pairing peer
    acknowledgement."
  - constants.rs:27: `pub const MSG_PEER_ACK: u8 = 0x84;`
  - constants.rs:58–59: `PEER_ACK_KEY_STATUS`, `PEER_ACK_KEY_PROOF`
  - messages.rs:371: `_ => Err(DecodeError::InvalidMsgType(msg_type))` —
    catches 0x84.
- **Root Cause**: Same as F-004 — deferred implementation.
- **Impact**: The protocol crate cannot encode or decode PEER_ACK
  messages, blocking BLE pairing support.
- **Remediation**: Add a `PeerAck { status: u8, registration_proof: Vec<u8> }`
  variant to `GatewayMessage` with encode/decode support using the
  existing CBOR key constants. Update the design doc.
- **Confidence**: High

### Finding F-006: `DecodeError::InvalidCommandType` variant not specified in design

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**:
  - Spec: `protocol-crate-design.md` §8
  - Code: `crates/sonde-protocol/src/error.rs:27`
- **Description**: The design's `DecodeError` enum lists six variants:
  `TooShort`, `TooLong`, `InvalidMsgType(u8)`, `MissingField(u64)`,
  `InvalidFieldType(u64)`, `CborError(String)`. The code adds a seventh
  variant `InvalidCommandType(u8)` used when COMMAND decode encounters
  an unrecognized `command_type` value.
- **Evidence**:
  - Design §8: lists exactly six `DecodeError` variants.
  - Code error.rs:27: `InvalidCommandType(u8)`
  - Code messages.rs:355:
    `_ => return Err(DecodeError::InvalidCommandType(command_type))`
- **Root Cause**: The design did not anticipate a separate error for
  invalid command types within a valid COMMAND message. The
  implementation correctly distinguishes between invalid `msg_type`
  (frame-level) and invalid `command_type` (COMMAND payload-level).
- **Impact**: Consumers pattern-matching on `DecodeError` must handle
  this additional variant. This is a reasonable extension that improves
  error diagnostics, but it deviates from the documented API.
- **Remediation**: Update the design document §8 to include
  `InvalidCommandType(u8)` in the `DecodeError` enum.
- **Confidence**: High

### Finding F-007: PEER_REQUEST/PEER_ACK CBOR key constants defined without message type support

- **Severity**: Low
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**:
  - Spec: `protocol-crate-design.md` §3 — does not list these constants
  - Code: `crates/sonde-protocol/src/constants.rs:52–59`
- **Description**: Three CBOR key constants are defined for the
  PEER_REQUEST and PEER_ACK message types:
  - `PEER_REQ_KEY_PAYLOAD: u64 = 1`
  - `PEER_ACK_KEY_STATUS: u64 = 1`
  - `PEER_ACK_KEY_PROOF: u64 = 2`

  These are not in the design document's §3 constants list and have no
  corresponding message type variants (see F-004 and F-005). They
  represent partial forward preparation for BLE pairing support.
- **Evidence**:
  - Code constants.rs:52–59: three constants with doc comments
    referencing PEER_REQUEST / PEER_ACK semantics.
  - Design §3: no mention of PEER_REQ or PEER_ACK keys.
- **Root Cause**: Constants were added in anticipation of PEER_REQUEST/
  PEER_ACK implementation but the message types were never completed.
- **Impact**: Dead code that could mislead developers into thinking
  PEER_REQUEST/PEER_ACK are supported. No runtime impact.
- **Remediation**: When implementing F-004/F-005, these constants will
  become live. Until then, add a doc comment noting they are reserved
  for future use.
- **Confidence**: High

### Finding F-008: ciborium dependency missing `features = ["alloc"]`

- **Severity**: Low
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**:
  - Spec: `protocol-crate-design.md` §2
  - Code: `crates/sonde-protocol/Cargo.toml:12`
- **Description**: The design specifies
  `ciborium = { version = "0.2", default-features = false, features = ["alloc"] }`.
  The code specifies
  `ciborium = { version = "0.2", default-features = false }` without the
  `features = ["alloc"]` attribute.
- **Evidence**:
  - Design §2 Cargo.toml snippet shows `features = ["alloc"]`.
  - Code Cargo.toml:12 omits features entirely.
- **Root Cause**: The alloc feature was not explicitly enabled, relying
  on ciborium's behavior when default-features are disabled.
- **Impact**: Low. The crate compiles and works correctly, suggesting
  ciborium 0.2 exposes the needed APIs without the explicit feature.
  However, a ciborium minor version update could change this behavior.
- **Remediation**: Add `features = ["alloc"]` to the ciborium dependency
  to match the design and ensure forward compatibility.
- **Confidence**: High

### Finding F-009: `chunk_count` validation order and arithmetic differ from design

- **Severity**: Informational
- **Category**: D10_CONSTRAINT_VIOLATION_IN_CODE
- **Location**:
  - Spec: `protocol-crate-design.md` §9
  - Code: `crates/sonde-protocol/src/chunk.rs:7–16`
- **Description**: Two deviations from the design:
  1. **Check order**: Design checks `image_size == 0` first, then
     `chunk_size == 0`. Code checks `chunk_size == 0` first, then
     `image_size == 0`. Functionally equivalent for all inputs.
  2. **Arithmetic**: Design uses
     `((image_size + chunk_size - 1) / chunk_size) as u32` which can
     overflow on large inputs and silently truncates via `as u32`. Code
     uses `image_size.div_ceil(chunk_size)` with
     `u32::try_from(count).ok()`, which is safer — returns `None`
     instead of wrapping.
- **Evidence**:
  - Design §9 code snippet shows `image_size == 0` guard first and
    `as u32` cast.
  - Code chunk.rs:8–9 checks `chunk_size == 0` first; line 14–15 uses
    `div_ceil` and `try_from`.
- **Root Cause**: The implementation improved on the design's arithmetic
  safety.
- **Impact**: None negative. The code is strictly safer than the design.
- **Remediation**: Update the design to match the improved
  implementation.
- **Confidence**: High

### Finding F-010: Minor trait derivation additions beyond design spec

- **Severity**: Informational
- **Category**: D9_UNDOCUMENTED_BEHAVIOR
- **Location**:
  - Spec: `protocol-crate-design.md` §7.1 (`MapDef`), §5.3
    (`DecodedFrame`)
  - Code: `crates/sonde-protocol/src/program_image.rs:14`,
    `crates/sonde-protocol/src/codec.rs:11`
- **Description**: Two types derive additional traits beyond the design:
  - `MapDef` derives `Copy` (design specifies only
    `Debug, Clone, PartialEq`).
  - `DecodedFrame` derives `Clone` (design specifies only `Debug`).
- **Evidence**:
  - Design §7.1: `#[derive(Debug, Clone, PartialEq)]` for `MapDef`.
  - Code program_image.rs:14:
    `#[derive(Debug, Clone, Copy, PartialEq)]`.
  - Design §5.3: `#[derive(Debug)]` for `DecodedFrame`.
  - Code codec.rs:11: `#[derive(Debug, Clone)]`.
- **Root Cause**: Additions for ergonomics — `Copy` on `MapDef` avoids
  explicit clones for a small struct, `Clone` on `DecodedFrame` allows
  consumers to duplicate frames.
- **Impact**: None negative. Both additions are backward-compatible and
  do not change semantics. `Copy` on `MapDef` is appropriate since all
  fields are `u32`.
- **Remediation**: Update the design to document the additional derives.
- **Confidence**: High

## 5. Root Cause Analysis

### Coverage Metrics

| Metric | Value |
|---|---|
| Protocol message types specified (protocol.md §4) | 9 (WAKE, GET_CHUNK, PROGRAM_ACK, APP_DATA, PEER_REQUEST, COMMAND, CHUNK, APP_DATA_REPLY, PEER_ACK) |
| Protocol message types implemented (encode + decode) | 7 of 9 (78%) |
| Protocol message types with constants only | 2 (PEER_REQUEST, PEER_ACK) |
| Design API contracts specified | 17 (constants, header, codec encode/decode/verify, key_hint, NodeMessage encode/decode, GatewayMessage encode/decode, ProgramImage encode/decode, program_hash, chunk_count, get_chunk, HmacProvider, Sha256Provider, error types) |
| Design API contracts implemented correctly | 14 of 17 (82%) |
| Design API contracts with deviations | 3 (encode_deterministic return type, feature gates, ciborium features) |
| Constraint violations in code (D10) | 5 (3 medium, 1 low, 1 informational) |
| Unimplemented requirements (D8) | 2 (both medium) |
| Undocumented behavior (D9) | 3 (2 low, 1 informational) |

### Thematic Analysis

The findings cluster into two themes:

1. **BLE pairing deferred** (F-004, F-005, F-007): The protocol spec
   defines PEER_REQUEST/PEER_ACK message types for BLE pairing, but
   neither the design nor the code implements them beyond constants. This
   is a planned feature gap, not an oversight.

2. **Minor API drift** (F-001, F-002, F-003, F-006, F-008, F-009,
   F-010): Small deviations between design-specified APIs and
   implementation. Most are improvements (safer arithmetic, additional
   derives) or pragmatic simplifications (unconditional alloc). None
   affect correctness.

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-004, F-005 | Add `PeerRequest` and `PeerAck` variants to `NodeMessage`/`GatewayMessage` with encode/decode support | M | Low |
| 2 | F-001 | Make `encode_deterministic` infallible or update design to document `Result` return | S | Low |
| 3 | F-002 | Add key-5 absence check in NOP/REBOOT decode path | S | Low |
| 4 | F-003, F-008 | Add `alloc` feature to Cargo.toml; add `features = ["alloc"]` to ciborium dep; or update design | S | Low |
| 5 | F-006, F-007, F-009, F-010 | Update design document to match implementation | S | None |

## 7. Prevention

- **Spec–code synchronization**: When adding new message types to
  `protocol.md`, create a tracking issue that includes updating both the
  design doc and the protocol crate implementation.
- **API contract review**: Use the design document's code snippets as
  the canonical API reference. PR reviews should compare function
  signatures against the design spec when modifying public APIs.
- **Feature gate testing**: Add a CI job that builds the crate with
  `--no-default-features` to verify the feature gate structure works
  as documented.

## 8. Open Questions

1. **PEER_REQUEST/PEER_ACK scope**: These message types reference
   `ble-pairing-protocol.md`, which was not examined in this audit.
   The exact payload structure and CBOR key semantics for these messages
   should be verified against that document before implementation.

2. **Deterministic CBOR enforcement**: The `cbor_encode_map` helper
   outputs keys in the order provided by callers. The current callers
   emit keys in ascending order, but there is no compile-time or
   runtime enforcement. If a future caller passes unsorted keys,
   deterministic encoding would silently break. Consider adding a
   debug assertion or using a sorted container.

3. **`encode_deterministic` fallibility**: Is ciborium serialization
   truly infallible for well-formed `ProgramImage` values? If so, the
   design's infallible signature is correct and the `Result` wrapper
   should be removed. If not, the design should be updated.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2025-07-15 | Copilot (audit agent) | Initial audit |
