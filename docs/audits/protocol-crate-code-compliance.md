<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->

# Protocol Crate Code Compliance Audit (D8–D10)

> **Pre-remediation snapshot.** This report captures the state of the
> `sonde-protocol` crate at the time of audit. Findings should be
> re-verified after any remediation work.

---

## 1. Executive Summary

The `sonde-protocol` crate implements the majority of the protocol wire
format specified in `protocol.md` and `protocol-crate-design.md`.  Frame
encoding/decoding, message types, HMAC plumbing, program image handling,
and chunking helpers are all present and functionally correct.

However, the audit identified **11 findings** across three categories:

| Category | Count | Severity Breakdown |
|---|---|---|
| D8 — Not Implemented | 1 | 1 High |
| D9 — Undocumented Behavior | 6 | 1 Medium, 5 Low |
| D10 — Constraint Violation | 4 | 1 High, 1 Medium, 2 Low |

The most impactful finding is the missing `key_hint_from_psk` function
(F-001, D8 High), which forces every consumer (gateway, node, admin CLI)
to independently re-implement the key-hint derivation formula — a
recipe for divergence bugs.  The second high-severity finding is the
Cargo feature-flag mismatch (F-002, D10 High), where the crate's
`Cargo.toml` omits the `alloc` feature specified in the design, breaking
the intended `no_std`-without-alloc compilation target.

---

## 2. Problem Statement

This audit answers: *Does the `sonde-protocol` crate faithfully implement
every testable claim in `protocol.md` and `protocol-crate-design.md`?*

It covers three axes:

- **D8 — Forward traceability:** every spec claim maps to implemented
  code.
- **D9 — Backward traceability:** every significant code behavior is
  covered by a spec claim.
- **D10 — Constraint verification:** security, size, and encoding
  constraints are correctly enforced.

---

## 3. Investigation Scope

### 3.1 Files examined

**Specification documents:**

| Document | Role |
|---|---|
| `docs/protocol.md` (§1–§10, 579 lines) | Wire format, message defs, auth, replay |
| `docs/protocol-crate-design.md` (§1–§9, 413 lines) | Crate API, types, functions |

**Source files (all under `crates/sonde-protocol/src/`):**

| File | Lines | Purpose |
|---|---|---|
| `lib.rs` | 27 | Crate root, module declarations, re-exports |
| `constants.rs` | 67 | All protocol and program-image CBOR key constants |
| `header.rs` | 41 | `FrameHeader` struct, `to_bytes`/`from_bytes` |
| `codec.rs` | 80 | `encode_frame`, `decode_frame`, `verify_frame`, `DecodedFrame` |
| `messages.rs` | 383 | `NodeMessage`, `GatewayMessage`, `CommandPayload`, CBOR encode/decode |
| `error.rs` | 56 | `EncodeError`, `DecodeError` enums |
| `traits.rs` | 15 | `HmacProvider`, `Sha256Provider` traits |
| `program_image.rs` | 163 | `ProgramImage`, `MapDef`, `encode_deterministic`, `program_hash` |
| `chunk.rs` | 31 | `chunk_count`, `get_chunk` helpers |
| `ble_envelope.rs` | 85 | BLE message envelope codec |
| `modem.rs` | ~900 | Modem serial protocol codec |

**Also consulted:** `crates/sonde-protocol/Cargo.toml`,
`crates/sonde-protocol/tests/validation.rs` (for test coverage context).

### 3.2 Method

1. Extracted every testable claim from both spec documents.
2. Read every source file line-by-line and mapped implementations to
   claims.
3. Performed forward traceability (spec → code) and backward
   traceability (code → spec).
4. Verified security, size, and encoding constraints against the code.

### 3.3 Limitations

- The `HmacProvider` trait only *documents* that implementations MUST use
  constant-time comparison; the crate cannot enforce this at the type
  level.  Whether actual provider implementations (in `sonde-gateway`,
  `sonde-node`) comply is outside this audit's scope.
- CBOR deterministic encoding for program images relies on ciborium's
  output for integer keys supplied in ascending order; no independent
  byte-level verification was performed.
- The modem and BLE envelope modules are governed by separate spec
  documents (`modem-protocol.md`, `ble-pairing-protocol.md`) not
  included in this audit's input set.

---

## 4. Findings

### F-001 — `key_hint_from_psk` function not implemented

| Attribute | Value |
|---|---|
| **Severity** | High |
| **Category** | D8 — Not Implemented |
| **Spec ref** | `protocol-crate-design.md` §5.5; `protocol.md` §3.1.1 |
| **Code location** | Not present in any source file |
| **Confidence** | Definite |

**Description:**
The design specification defines a `key_hint_from_psk` function:

```rust
pub fn key_hint_from_psk(psk: &[u8; 32], sha: &impl Sha256Provider) -> u16 {
    let hash = sha.hash(psk);
    u16::from_be_bytes([hash[30], hash[31]])
}
```

This function is absent from the crate.  A `grep` for `key_hint_from_psk`
across the entire `crates/` tree returns zero results.  It is also not
re-exported from `lib.rs`.

**Evidence:**
```
$ rg key_hint_from_psk crates/sonde-protocol/
(no results)
```

**Impact:**
Every consumer that needs to derive a `key_hint` from a PSK must
independently implement the formula
`u16::from_be_bytes(SHA-256(PSK)[30..32])`.  Independent
re-implementations risk using the wrong byte offsets (e.g., `[0..2]`
instead of `[30..32]`), breaking node–gateway interoperability.

**Remediation:**
Add the function to `codec.rs` (or a new `key_hint.rs` module) and
re-export from `lib.rs`.  Add a unit test that verifies the derivation
against a known PSK/hash pair.

---

### F-002 — Cargo.toml feature flags deviate from design spec

| Attribute | Value |
|---|---|
| **Severity** | High |
| **Category** | D10 — Constraint Violation |
| **Spec ref** | `protocol-crate-design.md` §2 |
| **Code location** | `crates/sonde-protocol/Cargo.toml:7-9` |
| **Confidence** | Definite |

**Description:**
The design specifies three feature flags:

```toml
[features]
default = ["alloc"]
alloc = []       # enables Vec<u8> in message types
std = ["alloc"]
```

The actual `Cargo.toml` has:

```toml
[features]
default = []
std = []
```

The `alloc` feature is entirely absent.  The crate unconditionally uses
`extern crate alloc;` in `lib.rs:6`, making it impossible to compile
for targets without an allocator — contradicting the design intent of
a feature-gated `alloc` capability.

Additionally, the ciborium dependency is missing the `features = ["alloc"]`
that the design specifies:

- **Design:** `ciborium = { version = "0.2", default-features = false, features = ["alloc"] }`
- **Actual:** `ciborium = { version = "0.2", default-features = false }`

**Impact:**
The crate cannot be compiled without an allocator, preventing potential
future use on truly minimal `no_std` targets.  The `std` feature has no
effect (does not enable `alloc`).

**Remediation:**
Add the `alloc` feature to `Cargo.toml`, gate `extern crate alloc` behind
`#[cfg(feature = "alloc")]`, set `default = ["alloc"]`, and make `std`
depend on `alloc`.  Add `features = ["alloc"]` to the ciborium dependency.

---

### F-003 — `EncodeError::CommandTypeMismatch` variant not in spec

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D9 — Undocumented Behavior |
| **Spec ref** | `protocol-crate-design.md` §8 |
| **Code location** | `crates/sonde-protocol/src/error.rs:10` |
| **Confidence** | Definite |
| **Status** | **Resolved** — removed in #375 |

**Description:**
The `EncodeError` enum contains a `CommandTypeMismatch` variant:

```rust
CommandTypeMismatch { command_type: u8, expected: u8 },
```

This variant is not listed in the design spec's §8 error types, which
specifies only `FrameTooLarge` and `CborError(String)`.

**Evidence:**
Design §8 defines:
```rust
pub enum EncodeError {
    FrameTooLarge,
    CborError(String),
}
```

The code adds `CommandTypeMismatch`.

**Impact:**
Low.  The variant appears unused in the current codebase (the
`command_type` is derived from `CommandPayload` automatically, so a
mismatch cannot occur with the current API).  Its presence adds API
surface without spec backing.

**Resolution:** The `CommandTypeMismatch` variant was removed. Since
`command_type` is derived from `CommandPayload::command_type()`, a
mismatch is structurally impossible and the variant was dead code.

**Remediation:**
Either add the variant to the design spec with rationale, or remove it
from the code if unused.

---

### F-004 — `DecodeError::InvalidCommandType` variant not in spec

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D9 — Undocumented Behavior |
| **Spec ref** | `protocol-crate-design.md` §8 |
| **Code location** | `crates/sonde-protocol/src/error.rs:36` |
| **Confidence** | Definite |

**Description:**
The `DecodeError` enum contains an `InvalidCommandType(u8)` variant not
listed in the design spec.

**Evidence:**
Design §8 defines `InvalidMsgType(u8)` but not `InvalidCommandType(u8)`.
The code uses this at `messages.rs:355`:
```rust
_ => return Err(DecodeError::InvalidCommandType(command_type)),
```

**Impact:**
Low.  The variant is functionally necessary — when decoding a COMMAND
message, an unrecognized `command_type` value needs a distinct error.
`InvalidMsgType` is semantically wrong for this case.

**Remediation:**
Add `InvalidCommandType(u8)` to the design spec §8.

---

### F-005 — `MSG_PEER_REQUEST` / `MSG_PEER_ACK` and associated constants not in spec

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D9 — Undocumented Behavior |
| **Spec ref** | `protocol.md` §4; `protocol-crate-design.md` §3 |
| **Code location** | `crates/sonde-protocol/src/constants.rs:21-22, 52-59` |
| **Confidence** | Definite |

**Description:**
The constants file defines two additional message types and their CBOR
keys that are absent from both spec documents:

```rust
pub const MSG_PEER_REQUEST: u8 = 0x05;    // constants.rs:21
pub const MSG_PEER_ACK: u8 = 0x84;        // constants.rs:22

pub const PEER_REQ_KEY_PAYLOAD: u64 = 1;  // constants.rs:57
pub const PEER_ACK_KEY_STATUS: u64 = 1;   // constants.rs:58
pub const PEER_ACK_KEY_PROOF: u64 = 2;    // constants.rs:59
```

These follow the direction-bit convention (`0x05` = node→gateway,
`0x84` = gateway→node) but are not documented in `protocol.md` §4's
message type tables or in the design spec's constants section.

**Impact:**
Medium.  Undocumented message types in the protocol crate create a risk
of interoperability surprises.  Consumers may encounter these constants
without understanding their semantics or wire format.

**Remediation:**
Either add PEER_REQUEST / PEER_ACK to `protocol.md` (with full message
definitions and CBOR key mapping), or move these constants to a separate
module/feature that clearly indicates they are part of a different
protocol extension (e.g., BLE pairing).

---

### F-006 — `ProgramImage::encode_deterministic` return type differs from spec

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D10 — Constraint Violation |
| **Spec ref** | `protocol-crate-design.md` §7.2 |
| **Code location** | `crates/sonde-protocol/src/program_image.rs:31` |
| **Confidence** | Definite |

**Description:**
The design specifies:
```rust
pub fn encode_deterministic(&self) -> Vec<u8> { ... }
```

The implementation returns a `Result`:
```rust
pub fn encode_deterministic(&self) -> Result<Vec<u8>, EncodeError> { ... }
```

**Impact:**
Low.  The `Result` return type is arguably safer — CBOR serialization can
theoretically fail.  However, consumers written to the spec signature
will not compile against the actual API.

**Remediation:**
Update the design spec §7.2 to reflect `Result<Vec<u8>, EncodeError>`.

---

### F-007 — `chunk_count` check order differs from spec (behavioral difference)

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D10 — Constraint Violation |
| **Spec ref** | `protocol-crate-design.md` §9 |
| **Code location** | `crates/sonde-protocol/src/chunk.rs:7-16` |
| **Confidence** | Definite |

**Description:**
The design checks `image_size == 0` **before** `chunk_size == 0`:

```rust
// Design spec
if image_size == 0 { return Some(0); }
if chunk_size == 0 { return None; }
```

The code checks `chunk_size == 0` **first**:

```rust
// Actual code
if chunk_size == 0 { return None; }
if image_size == 0 { return Some(0); }
```

This produces a behavioral difference for the edge case
`chunk_count(0, 0)`:

| Input | Design result | Code result |
|---|---|---|
| `(0, 0)` | `Some(0)` | `None` |

Additionally, the code uses `u32::try_from(count).ok()` instead of
`as u32`, returning `None` on overflow rather than silently truncating.

**Impact:**
Low.  The `(0, 0)` edge case is unlikely in practice (zero chunk size is
always invalid).  The code's behavior is arguably more correct.  The
overflow-safe cast is a strict improvement.

**Remediation:**
Update the design spec §9 to match the code's check order and the safer
`u32::try_from` conversion.

---

### F-008 — `modem` module not covered by audited specifications

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D9 — Undocumented Behavior |
| **Spec ref** | `protocol-crate-design.md` (not mentioned) |
| **Code location** | `crates/sonde-protocol/src/modem.rs` (~900 lines) |
| **Confidence** | Definite |

**Description:**
The `modem.rs` module implements a complete serial protocol codec
(length-prefixed framing, ~20 message types, a streaming `FrameDecoder`)
that is neither mentioned in `protocol.md` nor in
`protocol-crate-design.md`.

The module's doc comment references `modem-protocol.md`, which is a
separate specification not included in this audit's input set.

**Impact:**
Low for this audit (the module is governed by a separate spec).  However,
`protocol-crate-design.md` should acknowledge the module's existence as
part of the crate's public API.

**Remediation:**
Add a section to `protocol-crate-design.md` noting the modem module and
referencing `modem-protocol.md` for its specification.

---

### F-009 — `ble_envelope` module not covered by audited specifications

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D9 — Undocumented Behavior |
| **Spec ref** | `protocol-crate-design.md` (not mentioned) |
| **Code location** | `crates/sonde-protocol/src/ble_envelope.rs` (85 lines) |
| **Confidence** | Definite |

**Description:**
The `ble_envelope.rs` module implements a BLE message envelope codec
(TYPE + LEN + BODY) that is not mentioned in the audited spec documents.
The module's doc comment references `ble-pairing-protocol.md §4`.

**Impact:**
Same as F-008.  The module is small and well-tested, but its existence
should be acknowledged in the design spec.

**Remediation:**
Add a section to `protocol-crate-design.md` noting the BLE envelope
module and referencing `ble-pairing-protocol.md`.

---

### F-010 — COMMAND CBOR key ordering is non-canonical

| Attribute | Value |
|---|---|
| **Severity** | Medium |
| **Category** | D10 — Constraint Violation |
| **Spec ref** | `protocol.md` §5, CBOR key mapping table |
| **Code location** | `crates/sonde-protocol/src/messages.rs:246-293` |
| **Confidence** | Definite |

**Description:**
When encoding `GatewayMessage::Command`, the CBOR map keys are emitted
in the order: **4, 13, 14, 5** (command_type, starting_seq,
timestamp_ms, then payload):

```rust
// messages.rs:246-249
let mut p = alloc::vec![
    (KEY_COMMAND_TYPE, u8_val(payload.command_type())),   // key 4
    (KEY_STARTING_SEQ, uint_val(*starting_seq)),          // key 13
    (KEY_TIMESTAMP_MS, uint_val(*timestamp_ms)),          // key 14
];
// ...
p.push((KEY_PAYLOAD, pv));  // key 5 — appended AFTER 13 and 14
```

The canonical ascending order would be: **4, 5, 13, 14**.

While `protocol.md` only explicitly requires deterministic CBOR encoding
for program images (not protocol messages), the CBOR key mapping table
in §5 implies integer keys, and the design spec §6.4 states "All payloads
are CBOR maps with integer keys."  Non-canonical key ordering may cause
issues with strict CBOR decoders or future interoperability requirements.

**Impact:**
Medium.  No current functionality is broken (ciborium accepts any key
order on decode).  However, if deterministic encoding is later required
for protocol messages (e.g., for signing or caching), this ordering will
need correction.

**Remediation:**
Reorder the key emission to ascending order (4, 5, 13, 14) — insert the
`payload` entry at position index 1 rather than appending.

---

### F-011 — `MapDef` derives `Copy` beyond spec

| Attribute | Value |
|---|---|
| **Severity** | Low |
| **Category** | D9 — Undocumented Behavior |
| **Spec ref** | `protocol-crate-design.md` §7.1 |
| **Code location** | `crates/sonde-protocol/src/program_image.rs:14` |
| **Confidence** | Definite |

**Description:**
The design specifies `#[derive(Debug, Clone, PartialEq)]` for `MapDef`.
The code additionally derives `Copy`:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapDef { ... }
```

**Impact:**
Minimal.  `MapDef` contains only `u32` fields, so `Copy` is valid and
useful.  This is a harmless enhancement.

**Remediation:**
Update the design spec §7.1 to include `Copy` in the derive list.

---

## 5. Root Cause Analysis

### 5.1 Coverage Metrics

**Forward traceability (spec → code):**

| Spec claim category | Total | Implemented | Partial | Not Impl. |
|---|---|---|---|---|
| Constants (§3) | 30 | 30 | 0 | 0 |
| Frame header (§4) | 3 | 3 | 0 | 0 |
| Frame codec (§5.1–5.4) | 5 | 5 | 0 | 0 |
| `key_hint_from_psk` (§5.5) | 1 | 0 | 0 | **1** |
| Message types (§6) | 9 | 9 | 0 | 0 |
| Program image (§7) | 5 | 5 | 0 | 0 |
| Error types (§8) | 2 | 2 | 0 | 0 |
| Chunking helpers (§9) | 2 | 2 | 0 | 0 |
| Cargo metadata (§2) | 1 | 0 | **1** | 0 |
| **Total** | **58** | **56** | **1** | **1** |

**Forward coverage: 96.6%** (56/58 fully implemented).

**Backward traceability (code → spec):**

| Undocumented behavior | Finding |
|---|---|
| ~~`EncodeError::CommandTypeMismatch`~~ | F-003 (resolved) |
| `DecodeError::InvalidCommandType` | F-004 |
| `MSG_PEER_REQUEST` / `MSG_PEER_ACK` constants | F-005 |
| `modem` module | F-008 |
| `ble_envelope` module | F-009 |
| `MapDef` derives `Copy` | F-011 |

**Undocumented code behaviors (open): 5** (1 Medium, 4 Low). **Resolved:** 1.

**Constraint compliance:**

| Constraint | Status | Finding |
|---|---|---|
| 250-byte max frame size | ✅ Enforced | — |
| HMAC covers header + payload | ✅ Correct | — |
| Constant-time HMAC verify | ⚠️ Documented in trait, not enforceable at crate level | — |
| key_hint = SHA-256(PSK)[30..32] | ❌ Not implemented | F-001 |
| Deterministic CBOR for program images | ✅ Keys in ascending order | — |
| CBOR integer keys for messages | ✅ All messages use integer keys | — |
| Feature flags match spec | ❌ `alloc` feature missing | F-002 |
| API signatures match spec | ⚠️ Minor deviation | F-006, F-007 |
| COMMAND key ordering canonical | ⚠️ Non-ascending | F-010 |

### 5.2 Root Causes

1. **Incremental development:** The `key_hint_from_psk` function and
   `alloc` feature appear to have been deferred during initial
   implementation and never revisited.

2. **Spec lag:** The `modem` and `ble_envelope` modules and the
   `PEER_REQUEST`/`PEER_ACK` message types were added to the code as
   the system evolved, but the design spec was not updated to reflect
   them.

3. **Defensive improvements:** Several code behaviors (F-004, F-006,
   F-007, F-011) improve on the spec's original design but the spec
   was not back-updated to match.

---

## 6. Remediation Plan

| Priority | Finding | Action | Effort |
|---|---|---|---|
| P0 | F-001 | Implement `key_hint_from_psk` in `codec.rs`, export from `lib.rs`, add test | Small |
| P0 | F-002 | Add `alloc` feature to `Cargo.toml`, gate `extern crate alloc`, fix ciborium features | Small |
| P1 | F-005 | Document `PEER_REQUEST`/`PEER_ACK` in `protocol.md` or move to separate module | Medium |
| P1 | F-010 | Reorder COMMAND CBOR keys to ascending (4, 5, 13, 14) | Small |
| P2 | F-003 | ~~Remove unused `CommandTypeMismatch`~~ — **Resolved** (#375) | Trivial |
| P2 | F-004 | Add `InvalidCommandType` to design spec §8 | Trivial |
| P2 | F-006 | Update design spec §7.2 return type to `Result` | Trivial |
| P2 | F-007 | Update design spec §9 to match code's check order | Trivial |
| P2 | F-008 | Add modem module reference to design spec | Trivial |
| P2 | F-009 | Add BLE envelope module reference to design spec | Trivial |
| P2 | F-011 | Add `Copy` to design spec §7.1 derive list | Trivial |

---

## 7. Prevention

1. **Spec-code sync check:** Add a CI step that runs a lightweight
   traceability check (e.g., grep for every public function listed in the
   design spec and verify it exists in the source).

2. **Spec-first workflow:** When adding new modules or message types,
   update the design spec *before* merging the implementation PR.

3. **Feature-flag tests:** Add a CI matrix entry that builds the crate
   with `--no-default-features` to catch unconditional `alloc` usage.

4. **Canonical encoding linter:** Add a test that encodes every message
   type and verifies CBOR map keys are in ascending order, catching
   ordering regressions.

---

## 8. Open Questions

1. **Q1:** Should `protocol-crate-design.md` be expanded to cover the
   modem and BLE envelope modules, or should those remain documented
   only in their respective spec files (`modem-protocol.md`,
   `ble-pairing-protocol.md`)?

2. **Q2:** Is there a plan to require deterministic CBOR encoding for
   *all* protocol messages (not just program images)?  If so, F-010
   should be P0.

3. **Q3:** ~~The `EncodeError::CommandTypeMismatch` variant appears unused.~~
   **Resolved:** The variant was dead code and has been removed (#375).
   Since `command_type` is derived from `CommandPayload::command_type()`,
   a mismatch is structurally impossible.

4. **Q4:** Should the `chunk_count(0, 0)` edge case be defined
   authoritatively?  The code's behavior (`None`) is safer, but the
   spec says `Some(0)`.

---

## 9. Revision History

| Date | Author | Description |
|---|---|---|
| 2026-03-20 | Copilot (audit agent) | Initial audit — pre-remediation snapshot |
