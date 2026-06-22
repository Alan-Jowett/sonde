<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Safe BPF Interpreter Requirements Specification

> **Document status:** Draft
> **Source:** Extracted from [safe-bpf-interpreter.md](safe-bpf-interpreter.md) (design), [bpf-environment.md](bpf-environment.md) (execution environment), [why-bpf.md](why-bpf.md) (rationale), and `crates/sonde-bpf/` (implementation).
> **Scope:** This document covers the `sonde-bpf` crate — the BPF interpreter with tagged register safety model. Node-level requirements (program transfer, storage, scheduling) are covered by [node-requirements.md](node-requirements.md).
> **Related:** [safe-bpf-interpreter.md](safe-bpf-interpreter.md), [safe-bpf-interpreter-validation.md](safe-bpf-interpreter-validation.md), [bpf-environment.md](bpf-environment.md), [node-requirements.md](node-requirements.md)

---

## 1  Definitions

| Term | Definition |
|---|---|
| **BPF** | The instruction set defined by [RFC 9669](https://www.rfc-editor.org/rfc/rfc9669.html). |
| **Tagged register** | A 64-bit value paired with an optional `Region` descriptor that tracks pointer provenance. |
| **Region** | Metadata recording the tag (kind), base (inclusive), and end (exclusive) of a valid memory area. |
| **Scalar** | A register with no region — an arithmetic value that cannot be dereferenced. |
| **Pointer** | A register whose region tag is `Stack`, `Context`, `Memory`, or `MapValue` — dereferenceable within bounds. |
| **Handle** | A register whose region tag is `MapDescriptor` — an opaque map reference, not dereferenceable. |
| **Choke point** | One of the small set of functions (`mem_load`, `mem_load_sign_extend`, `mem_store`, `mem_atomic32`, `mem_atomic64`) that are the only sites containing `unsafe` pointer dereferences. |
| **Spill** | Storing a pointer-tagged register to the BPF stack and later reloading it. |
| **Helper** | A host-provided function callable from BPF via the CALL instruction. |
| **Instruction budget** | The maximum number of instruction slots the interpreter executes before termination. |
| **Context pointer field** | A u64 offset within the context buffer that holds an embedded pointer to a separately-described memory region. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`SBPF-XXXX`).
- **Title** — Short name.
- **Description** — What the interpreter MUST / SHOULD / MAY do.
- **Acceptance criteria** — Observable, testable conditions.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Document section or code location that motivates the requirement.
- **Confidence** — **High** (directly evidenced), **Medium** (inferred from patterns), **Low** (needs confirmation).

---

## 3  Overview

The `sonde-bpf` crate provides a zero-allocation BPF interpreter that enforces memory safety through tagged registers. Every register carries provenance metadata that bounds-checks every pointer dereference at a small number of choke-point functions, eliminating scattered `unsafe` blocks. The interpreter is `#![no_std]`-compatible and runs on constrained targets (ESP32-C3/S3 with 8–16 KB task stacks).

---

## 4  Scope

### 4.1  In scope

- BPF instruction set execution (RFC 9669)
- Tagged register safety model
- Memory access validation (loads, stores, atomics)
- ALU pointer arithmetic rules
- Helper function integration with return-type descriptors
- Stack spill tracking
- BPF-to-BPF call frame management
- Instruction budgeting / metering
- Context pointer field tagging
- Error model

### 4.2  Out of scope

- BPF program verification (handled by Prevail at the gateway)
- Program transfer and storage (node-level: ND-0500 – ND-0503)
- Helper function implementations (provided by the host environment)
- Map storage allocation and persistence (node-level: ND-0606)
- ELF parsing and program image decoding (gateway-level)

---

## 5  Requirements

### 5.1  Instruction Set Compliance

#### SBPF-0100  RFC 9669 instruction set

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4, interpreter.rs, ebpf.rs
**Confidence:** High

**Description:**
The interpreter MUST implement the complete BPF instruction set as defined by RFC 9669, including:
- ALU32 and ALU64 arithmetic (ADD, SUB, MUL, DIV, MOD, OR, AND, XOR, LSH, RSH, ARSH, NEG, MOV)
- Signed division and modulo (SDIV, SMOD) via `off != 0` encoding
- Sign-extension moves (MOVSX) for 8, 16, and 32-bit widths
- Byte swap instructions (LE, BE, BSWAP)
- JMP and JMP32 conditional and unconditional branches
- CALL (helper functions and BPF-to-BPF local calls) and EXIT
- Load/Store MEM and sign-extension loads (MEMSX)
- 64-bit immediate load (LD_DW_IMM, occupying two instruction slots)
- Atomic operations (ADD, OR, AND, XOR, XCHG, CMPXCHG, with optional FETCH)

**Acceptance criteria:**

1. All instruction opcodes listed in `ebpf.rs` — except legacy packet-access opcodes (`LD_ABS_*`, `LD_IND_*`), which are not supported — are handled in the interpreter dispatch loop. Legacy packet-access opcodes are rejected as `BpfError::UnknownOpcode`.
2. Division by zero returns 0 (not a trap), per RFC 9669 §5.2.
3. Shift amounts are masked to 5 bits (ALU32) or 6 bits (ALU64).
4. LD_DW_IMM consumes two instruction slots and charges both to the instruction budget.
5. Atomic operations support both 32-bit and 64-bit widths.
6. Unknown opcodes return `BpfError::UnknownOpcode`.

---

#### SBPF-0101  Program bytecode validation

**Priority:** Must
**Source:** interpreter.rs:694–696
**Confidence:** High

**Description:**
The interpreter MUST reject programs whose bytecode length is not a multiple of 8 bytes (the BPF instruction size).

**Acceptance criteria:**

1. `execute_program` returns `BpfError::OutOfBounds` when `prog.len() % 8 != 0`.

---

#### SBPF-0102  Program counter bounds checking

**Priority:** Must
**Source:** interpreter.rs:309–317, safe-bpf-interpreter.md §4.5
**Confidence:** High

**Description:**
The interpreter MUST validate all jump targets and MUST return `BpfError::OutOfBounds` if the program counter would move outside the bytecode range. Falling off the end of the program without an EXIT instruction MUST also return `BpfError::OutOfBounds`.

**Acceptance criteria:**

1. Forward jumps past the last instruction return `BpfError::OutOfBounds`.
2. Backward jumps to negative offsets return `BpfError::OutOfBounds`.
3. Execution past the last instruction (no EXIT) returns `BpfError::OutOfBounds`.

---

### 5.2  Tagged Register Model

#### SBPF-0200  Three-state register invariant

**Priority:** Must
**Source:** safe-bpf-interpreter.md §2.3
**Confidence:** High

**Description:**
Every register MUST be in exactly one of three states at all times during execution:

| State | `region` | Dereferenceable? | ALU permitted? |
|-------|----------|-------------------|----------------|
| Scalar | `None` | No | All ops |
| Pointer | `Some(Stack\|Context\|Memory\|MapValue)` | Yes (within bounds) | Limited (§4.3) |
| Handle | `Some(MapDescriptor)` | No | MOV only |

**Acceptance criteria:**

1. Registers R0, R2–R9 start as scalars (region `None`, value 0).
2. R1 starts as a Context or Memory pointer (when ctx is non-empty) or scalar (when ctx is empty).
3. R10 starts as a Stack pointer.
4. No instruction produces a register in an undefined or fourth state.

---

#### SBPF-0201  Register initialization

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.1, interpreter.rs:722–751
**Confidence:** High

**Description:**
At program start, the interpreter MUST initialize registers as follows:

| Register | Value | Region |
|----------|-------|--------|
| R1 | `ctx.as_ptr()` | Context (if `read_only_ctx` and ctx non-empty), Memory (if `!read_only_ctx` and ctx non-empty), None (if ctx empty) |
| R2 | `ctx.len()` | None (scalar) |
| R10 | `stack_base + STACK_SIZE` | Stack (base=stack_base, end=stack_base+STACK_SIZE) |
| R0, R3–R9 | 0 | None (scalar) |

**Acceptance criteria:**

1. R1 is tagged Context when `read_only_ctx` is true and ctx is non-empty.
2. R1 is tagged Memory when `read_only_ctx` is false and ctx is non-empty.
3. R1 is scalar (None) when ctx is empty.
4. R10's Stack region spans the entire 4 KB stack allocation.
5. All region `end` values are computed with `checked_add`; overflow panics before execution begins.

---

#### SBPF-0202  Empty context handling

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.1 (Empty context note)
**Confidence:** High

**Description:**
When the context buffer is empty (`ctx.is_empty()`), R1 MUST be tagged as scalar (None). Any attempt to dereference R1 MUST return `BpfError::NonDereferenceableAccess`.

**Acceptance criteria:**

1. `execute_program` with an empty ctx slice tags R1 as scalar.
2. A program that loads from R1 with empty ctx gets `NonDereferenceableAccess`.

---

### 5.3  Memory Access Safety

#### SBPF-0300  Choke-point architecture

**Priority:** Must
**Source:** safe-bpf-interpreter.md §3, §3.4
**Confidence:** High

**Description:**
ALL pointer dereferences — loads, stores, and atomic operations — MUST be routed through exactly five choke-point functions: `mem_load`, `mem_load_sign_extend`, `mem_store`, `mem_atomic32`, and `mem_atomic64`. These MUST be the ONLY functions in the interpreter containing `unsafe` pointer dereference code.

**Acceptance criteria:**

1. The interpreter contains exactly 5 functions with `unsafe` blocks for memory access.
2. No load, store, or atomic instruction in the dispatch loop contains inline `unsafe` dereferences.
3. Each choke-point function performs the three-step validation: (a) confirm pointer provenance, (b) reject non-dereferenceable tags, (c) bounds-check `[addr, addr+N)` against `[region.base, region.end)`.

---

#### SBPF-0301  Scalar dereference rejection

**Priority:** Must
**Source:** safe-bpf-interpreter.md §3.1, §2.3
**Confidence:** High

**Description:**
Any attempt to dereference a scalar register (region `None`) MUST return `BpfError::NonDereferenceableAccess`.

**Acceptance criteria:**

1. Load via scalar → `NonDereferenceableAccess`.
2. Store via scalar → `NonDereferenceableAccess`.
3. Atomic op via scalar → `NonDereferenceableAccess`.

---

#### SBPF-0302  MapDescriptor dereference rejection

**Priority:** Must
**Source:** safe-bpf-interpreter.md §3.1, §2.3
**Confidence:** High

**Description:**
Any attempt to dereference a register tagged `MapDescriptor` MUST return `BpfError::NonDereferenceableAccess`. MapDescriptor is an opaque handle, not a memory address.

**Acceptance criteria:**

1. Load via MapDescriptor → `NonDereferenceableAccess`.
2. Store via MapDescriptor → `NonDereferenceableAccess`.
3. Atomic op via MapDescriptor → `NonDereferenceableAccess`.

---

#### SBPF-0303  Bounds-checked memory access

**Priority:** Must
**Source:** safe-bpf-interpreter.md §3.1, §3.2
**Confidence:** High

**Description:**
Every memory access MUST validate that the effective address range `[addr, addr+N)` falls entirely within `[region.base, region.end)`. Out-of-bounds accesses MUST return `BpfError::MemoryAccessViolation`. Address overflow (`addr + N` wrapping past `u64::MAX`) MUST be detected via `checked_add` and treated as `MemoryAccessViolation`.

**Acceptance criteria:**

1. Access at exactly `region.end - N` succeeds (last valid position).
2. Access at `region.end - N + 1` returns `MemoryAccessViolation`.
3. Access where `addr + N` overflows u64 returns `MemoryAccessViolation`.
4. Access below `region.base` returns `MemoryAccessViolation`.

---

#### SBPF-0304  Read-only context writes silently ignored

**Priority:** Must
**Source:** safe-bpf-interpreter.md §3.2, ND-0505 AC6
**Confidence:** High

**Description:**
When a register is tagged `Context` (read-only), store and atomic write operations to that region MUST be silently ignored — the write has no effect on memory, but bounds validation is still performed, and execution continues normally. This applies to all store widths (1/2/4/8 bytes) and all atomic operations.

**Acceptance criteria:**

1. Store to Context region: bounds are validated, no memory modification occurs, no error returned.
2. Atomic ADD to Context region: bounds validated, memory unchanged, FETCH semantics (old value loaded into src register) are preserved if applicable, no error returned.
3. CMPXCHG on Context region: bounds validated, memory unchanged, R0 receives the old value, no error returned.
4. Out-of-bounds write to Context region still returns `MemoryAccessViolation`.

---

#### SBPF-0305  Unaligned memory access support

**Priority:** Must
**Source:** interpreter.rs:339–342 (read_unaligned), RFC 9669
**Confidence:** High

**Description:**
The interpreter MUST support unaligned memory accesses for all load and store widths. Multi-byte loads and stores MUST use unaligned read/write operations (`read_unaligned` / `write_unaligned`).

**Acceptance criteria:**

1. A 4-byte load from an odd-aligned address within a valid region succeeds.
2. An 8-byte store to a 2-byte-aligned address within a valid region succeeds.

---

### 5.4  ALU Pointer Arithmetic Rules

#### SBPF-0400  Pointer arithmetic tag propagation

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.3
**Confidence:** High

**Description:**
The interpreter MUST enforce the following tag propagation rules for 64-bit ALU operations:

| Operation | dst | src | Result |
|-----------|-----|-----|--------|
| ADD | pointer | scalar | pointer (dst's region) |
| ADD | scalar | pointer | pointer (src's region) |
| ADD | scalar | scalar | scalar |
| ADD | pointer | pointer | **error** (`InvalidPointerArithmetic`) |
| SUB | pointer | scalar | pointer (dst's region) |
| SUB | pointer(A) | pointer(A) | scalar (same region → difference) |
| SUB | pointer(A) | pointer(B) | **error** (different regions) |
| SUB | scalar | pointer | **error** |
| SUB | scalar | scalar | scalar |
| MUL, DIV, MOD, LSH, RSH, ARSH, NEG | any | any | scalar (provenance destroyed) |
| AND, OR, XOR | scalar | scalar | scalar |
| AND, OR, XOR | pointer | any | **error** (`InvalidPointerArithmetic`) |
| MOV (reg) | — | any | inherits src tag |
| MOV (imm) | — | — | scalar |

**Acceptance criteria:**

1. `pointer + pointer` → `InvalidPointerArithmetic`.
2. `scalar - pointer` → `InvalidPointerArithmetic`.
3. `pointer(A) - pointer(B)` where A ≠ B → `InvalidPointerArithmetic`.
4. `pointer(A) - pointer(A)` → scalar result.
5. `AND/OR/XOR` with pointer dst → `InvalidPointerArithmetic`.
6. `MUL/DIV` on pointer → scalar (tag cleared), subsequent dereference fails.
7. `NEG` on pointer → scalar.
8. `MOV r2, r1` where R1 is pointer → R2 inherits pointer tag.

---

#### SBPF-0401  MapDescriptor ALU restriction

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.3 (MapDescriptor precedence rule)
**Confidence:** High

**Description:**
Before consulting the pointer arithmetic table, the interpreter MUST check whether either operand carries a `MapDescriptor` tag. If so, the ONLY permitted ALU operation is `MOV` (reg-to-reg copy). All other operations MUST return `InvalidPointerArithmetic`.

**Acceptance criteria:**

1. `ADD MapDescriptor, scalar` → `InvalidPointerArithmetic`.
2. `ADD scalar, MapDescriptor` → `InvalidPointerArithmetic`.
3. `MOV r2, r1` where R1 is MapDescriptor → R2 inherits MapDescriptor (no error).

---

#### SBPF-0402  ALU32 unconditional tag clearing

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.3 (32-bit ALU note)
**Confidence:** High

**Description:**
ALL 32-bit ALU operations MUST unconditionally clear the pointer tag on the destination register, producing a scalar result. This applies regardless of the operation type or operand tags — even `MOV32` clears the tag.

**Acceptance criteria:**

1. `ADD32 r2, 0` where R2 is pointer → R2 becomes scalar; subsequent dereference fails with `NonDereferenceableAccess`.
2. `MOV32 r2, r1` where R1 is pointer → R2 is scalar (not pointer).

---

### 5.5  LD_DW_IMM Map Relocation

#### SBPF-0500  Map descriptor loading

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.2, interpreter.rs:789–827
**Confidence:** High

**Description:**
`LD_DW_IMM` with `src=1` MUST perform map descriptor relocation:
1. The `imm` field (signed i32) MUST be validated: negative values MUST be rejected with `InvalidMapIndex`.
2. The non-negative imm value MUST be checked against `maps.len()`: out-of-bounds MUST be rejected with `InvalidMapIndex`.
3. The destination register MUST be tagged `MapDescriptor { map_index }` with the relocated pointer value from `maps[imm]`.

`LD_DW_IMM` with `src=0` MUST load a plain 64-bit immediate as scalar.

**Acceptance criteria:**

1. `LD_DW_IMM src=1, imm=-1` → `InvalidMapIndex { index: -1 }`.
2. `LD_DW_IMM src=1, imm=N` where N ≥ maps.len() → `InvalidMapIndex`.
3. `LD_DW_IMM src=1, imm=0` with valid maps → register tagged MapDescriptor, value = maps[0].relocated_ptr.
4. `LD_DW_IMM src=0` → scalar with 64-bit immediate value.
5. Unknown src values → `UnknownOpcode`.

#### SBPF-0501  Direct map-value relocation

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.2, interpreter.rs `LD_DW_IMM src=6`
**Confidence:** High

**Description:**
`LD_DW_IMM` with `src=6` MUST perform direct map-value relocation:
1. The `imm` field (signed i32) MUST be validated exactly like `src=1`: negative values and indices ≥ `maps.len()` MUST be rejected with `InvalidMapIndex`.
2. The interpreter MUST compute `value_base = maps[imm].data_start + maps[imm].key_size` using checked arithmetic.
3. The interpreter MUST compute `value_end = value_base + maps[imm].value_size` using checked arithmetic.
4. The interpreter MUST reject the relocation if the declared entry 0 value region does not fit within the caller-provided backing range: `value_base` MUST be ≥ `data_start` and `value_end` MUST be ≤ `data_end`.
5. The second wide-instruction slot's `imm` field MUST be treated as a signed constant offset from `value_base`.
6. The computed pointer MUST satisfy `value_base <= value_addr < value_end`; one-past-end pointers MUST be rejected with `MemoryAccessViolation`.
7. On success, the destination register MUST be tagged `MapValue { value_size }` over the range `[value_base, value_end)`.

`MapRegion.key_size` therefore becomes part of the runtime contract: callers that store key/value entries contiguously MUST set it to the number of key bytes to skip, while callers that store value bytes densely with no key prefix MUST set it to 0.

**Acceptance criteria:**

1. `LD_DW_IMM src=6, imm=-1` → `InvalidMapIndex { index: -1 }`.
2. `LD_DW_IMM src=6, imm=N` where N ≥ maps.len() → `InvalidMapIndex`.
3. `LD_DW_IMM src=6` with `next.imm == 0` and a valid map → register tagged `MapValue`, value = `data_start + key_size`.
4. `LD_DW_IMM src=6` with `value_base + value_size > data_end` → `MemoryAccessViolation`.
5. `LD_DW_IMM src=6` with `next.imm == value_size` → `MemoryAccessViolation`.
6. `LD_DW_IMM src=6` with a dense backing layout (`key_size == 0`) reads from the first value byte, not an assumed key prefix.

---

### 5.6  Helper Integration

#### SBPF-0600  Helper descriptor framework

**Priority:** Must
**Source:** safe-bpf-interpreter.md §5.1
**Confidence:** High

**Description:**
Helpers MUST be registered via `HelperDescriptor` structs that include:
- `id: u32` — the helper function ID used in CALL instructions.
- `func: Helper` — the function pointer `fn(u64, u64, u64, u64, u64) -> u64`.
- `ret: HelperReturn` — return-type descriptor for tag propagation.

The interpreter MUST look up helpers by ID. Unknown helper IDs MUST return `BpfError::UnknownHelper`.

**Acceptance criteria:**

1. A helper call with a registered ID dispatches correctly and returns R0.
2. A helper call with an unregistered ID returns `UnknownHelper`.

---

#### SBPF-0601  Helper return tagging — Scalar

**Priority:** Must
**Source:** safe-bpf-interpreter.md §5.2
**Confidence:** High

**Description:**
When a helper's return type is `HelperReturn::Scalar`, R0 MUST be tagged as scalar after the call.

**Acceptance criteria:**

1. After calling a Scalar-returning helper, R0 is scalar (dereference fails with `NonDereferenceableAccess`).

---

#### SBPF-0602  Helper return tagging — MapValueOrNull

**Priority:** Must
**Source:** safe-bpf-interpreter.md §5.2, interpreter.rs:1749–1797
**Confidence:** High

**Description:**
When a helper's return type is `HelperReturn::MapValueOrNull { map_arg }`:
1. If the return value is 0 (NULL), R0 MUST be tagged scalar.
2. If the return value is non-zero, the interpreter MUST:
   a. Verify that `reg[map_arg]` carries a `MapDescriptor` tag; otherwise return `InvalidHelperArgument`.
   b. Look up the map's `value_size` from the `maps` slice.
   c. Validate the returned pointer: `result >= map.data_start` and `result + value_size <= map.data_end`.
   d. If validation passes, tag R0 as `MapValue` with `base = result`, `end = result + value_size`.
   e. If validation fails (pointer outside map bounds, or `result + value_size` overflows), return `MemoryAccessViolation`.

**Acceptance criteria:**

1. Helper returns 0 → R0 is scalar, dereference fails.
2. Helper returns valid pointer → R0 is MapValue, dereference within value_size succeeds.
3. Helper returns out-of-bounds pointer → `MemoryAccessViolation`.
4. `reg[map_arg]` is scalar (not MapDescriptor) → `InvalidHelperArgument`.

---

#### SBPF-0603  Helper call register clobbering

**Priority:** Must
**Source:** safe-bpf-interpreter.md §4.6, interpreter.rs:1801–1804
**Confidence:** High

**Description:**
After a helper call (src=0 or src=2), the interpreter MUST clear the region tags on registers R1–R5 (set to `None`). The raw u64 values are left as-is. This prevents stale pointer provenance from leaking across call boundaries.

**Acceptance criteria:**

1. After a helper call, R3 (previously holding a Context pointer) is scalar — dereference fails with `NonDereferenceableAccess`.
2. R6–R9 retain their tags across helper calls (callee-saved registers are unaffected).

---

### 5.7  Stack Spill Tracking

#### SBPF-0700  Pointer spill and restore

**Priority:** Must
**Source:** safe-bpf-interpreter.md §6
**Confidence:** High

**Description:**
The interpreter MUST track pointer provenance for registers spilled to the BPF stack:
1. On `STX_DW` to the stack: if the source register has pointer provenance AND the access is 8-byte aligned, the interpreter MUST record the region metadata in a shadow spill tracker.
2. On `LDX_DW` from the stack: if the spill tracker has metadata for the loaded slot, the destination register MUST inherit the spilled region (restoring pointer provenance).
3. Otherwise, loaded values MUST be tagged as scalar.

**Acceptance criteria:**

1. Spill pointer to stack → reload → dereference succeeds (pointer tag restored).
2. Clobber register to scalar → reload from spill slot → dereference succeeds.
3. Store scalar to stack → reload → dereference fails (no pointer tag).

---

#### SBPF-0701  Partial overwrite invalidation

**Priority:** Must
**Source:** safe-bpf-interpreter.md §6.3
**Confidence:** High

**Description:**
Any store to the stack (any size, any alignment) that overlaps an 8-byte slot with a recorded spill MUST invalidate that spill entry. This prevents a partial overwrite from leaving stale pointer metadata.

**Acceptance criteria:**

1. Spill pointer → partial overwrite with `STX_B` → reload → dereference fails (spill invalidated).
2. Spill pointer → full overwrite with `STX_DW` (scalar) → reload → dereference fails.

---

#### SBPF-0702  Spill table overflow — safe fallback

**Priority:** Must
**Source:** safe-bpf-interpreter.md §6.3, §6.4
**Confidence:** High

**Description:**
The spill tracker has a fixed capacity (`MAX_SPILL_SLOTS = 32`). If the table is full when a new pointer spill occurs, the interpreter MUST clear the bitmap bit for that slot (so no stale metadata is found on reload) and the reloaded value becomes scalar. This is a safe fallback: the program may fault with `NonDereferenceableAccess`, but it can never cause an unsound memory access.

**Acceptance criteria:**

1. Spill 33 distinct pointers → reload the 33rd → dereference fails with `NonDereferenceableAccess`.

---

### 5.8  BPF-to-BPF Call Frames

#### SBPF-0800  Call frame save and restore

**Priority:** Must
**Source:** safe-bpf-interpreter.md §7.1, interpreter.rs:1810–1847
**Confidence:** High

**Description:**
On BPF-to-BPF call (CALL src=1):
1. R6–R9 values AND region tags MUST be saved in the call frame.
2. R1–R5 values and tags MUST be retained (they are the callee's arguments).
3. R10 (frame pointer) MUST be adjusted by `STACK_SIZE_PER_FRAME` (512 bytes). The Stack tag MUST be preserved with the same base/end (the entire stack is one region).

On EXIT (with frames remaining):
1. R6–R9 values and tags MUST be restored from the call frame.
2. R10 MUST be restored.
3. R0 retains its current value and tag (return value propagation).

**Acceptance criteria:**

1. R6 holding a Context pointer → BPF-to-BPF call → callee clobbers R6 → EXIT → R6 pointer tag restored, dereference succeeds.
2. R10 Stack tag is maintained in both caller and callee frames.
3. Stack stores in different frames target different physical memory (R10 adjusted).

---

#### SBPF-0801  Maximum call depth

**Priority:** Must
**Source:** ebpf.rs:15, interpreter.rs:1811
**Confidence:** High

**Description:**
The interpreter MUST support a maximum BPF-to-BPF call depth of `MAX_CALL_DEPTH` (8). Exceeding this depth MUST return `BpfError::CallDepthExceeded`.

**Acceptance criteria:**

1. 8 nested calls succeed.
2. The 9th nested call returns `CallDepthExceeded`.

---

### 5.9  Instruction Budget

#### SBPF-0900  Instruction metering

**Priority:** Must
**Source:** safe-bpf-interpreter.md §11.2, interpreter.rs:602–606, 757–760
**Confidence:** High

**Description:**
The interpreter MUST count each instruction slot executed. When the count exceeds `instruction_budget`, the interpreter MUST return `BpfError::InstructionBudgetExceeded`. The sentinel value `UNLIMITED_BUDGET` (`u64::MAX`) MUST disable metering.

LD_DW_IMM MUST charge 2 instruction slots (it occupies two 8-byte slots).

**Acceptance criteria:**

1. A program with N instructions and budget N completes successfully.
2. A program with N instructions and budget N−1 returns `InstructionBudgetExceeded`.
3. `UNLIMITED_BUDGET` allows arbitrary execution length.
4. LD_DW_IMM charges 2 slots against the budget.

---

### 5.10  Context Pointer Field Tagging

#### SBPF-1000  Context pointer field descriptors

**Priority:** Must
**Source:** interpreter.rs:161–178, 858–884
**Confidence:** High

**Description:**
The interpreter MUST support `ContextPointerField` descriptors that declare u64 offsets within the context buffer that hold embedded pointers. When `LDX_DW` loads from the context at a matching offset, the loaded value MUST be tagged with the described region instead of being treated as scalar. This enables C-compiled BPF programs that use the standard `ctx->data` / `ctx->data_end` pointer access pattern.

**Acceptance criteria:**

1. A `ContextPointerField` at offset 24 with a Memory region → `LDX_DW` from ctx at offset 24 tags the loaded value as a Memory pointer.
2. `LDX_DW` from ctx at a non-matching offset produces a scalar.
3. The match is computed as `effective_address - ctx_base`, so it works regardless of pointer arithmetic on the source register.

---

### 5.11  Error Model

#### SBPF-1100  Fatal error semantics

**Priority:** Must
**Source:** safe-bpf-interpreter.md §8
**Confidence:** High

**Description:**
All interpreter errors MUST be fatal — the program is terminated immediately and the error is returned to the caller. The interpreter MUST NOT attempt recovery or continuation after an error (except for silently ignored Context writes per SBPF-0304).

The following error variants MUST be supported:

| Variant | Trigger |
|---------|---------|
| `OutOfBounds` | PC out of bytecode range or invalid bytecode length |
| `UnknownOpcode` | Unrecognized instruction opcode |
| `UnknownHelper` | CALL to unregistered helper ID |
| `CallDepthExceeded` | BPF-to-BPF call depth > MAX_CALL_DEPTH |
| `MemoryAccessViolation` | Access outside region bounds or address overflow |
| `NonDereferenceableAccess` | Dereference of scalar or MapDescriptor |
| `InvalidHelperArgument` | Helper expects MapDescriptor but register has wrong tag |
| `ReadOnlyWrite` | Reserved for future hard write-rejection |
| `InvalidPointerArithmetic` | Pointer arithmetic rule violation |
| `InvalidMapIndex` | LD_DW_IMM src=1 or src=6 with negative or out-of-bounds index |
| `InstructionBudgetExceeded` | Instruction count exceeds budget |

**Acceptance criteria:**

1. Each error variant — except `ReadOnlyWrite` (reserved for future hard write-rejection) — is reachable via a specific test scenario.
2. All error variants include the `pc` field identifying the faulting instruction.
3. `BpfError` implements `Display` for all variants.
4. `BpfError` implements `std::error::Error` when the `std` feature is enabled.

---

### 5.12  Platform and Non-Functional Requirements

#### SBPF-1200  Zero-allocation guarantee

**Priority:** Must
**Source:** safe-bpf-interpreter.md §9.3, lib.rs:7
**Confidence:** High

**Description:**
The interpreter MUST NOT perform any heap allocation (`Vec`, `Box`, or other allocator-backed types) during program execution. All interpreter state — registers, call stack, BPF stack, spill tracker — MUST live on the Rust call stack.

**Acceptance criteria:**

1. `execute_program` compiles and runs correctly with `#[global_allocator]` replaced by a panicking allocator (in `no_std` builds).
2. The `sonde-bpf` crate compiles with `default-features = false` (no `std`).

---

#### SBPF-1201  `no_std` compatibility

**Priority:** Must
**Source:** lib.rs:27, Cargo.toml
**Confidence:** High

**Description:**
The `sonde-bpf` crate MUST be `#![no_std]`-compatible when compiled without the `std` feature. The `std` feature MUST be opt-in (default enabled for convenience).

**Acceptance criteria:**

1. `cargo build -p sonde-bpf --no-default-features` succeeds.
2. The crate uses only `core::` types in the `no_std` path.

---

#### SBPF-1202  Interpreter state size budget

**Priority:** Should
**Source:** safe-bpf-interpreter.md §9.1
**Confidence:** Medium

**Description:**
The total interpreter state (registers, BPF stack, spill tracker, call frames) SHOULD fit within approximately 7–8 KB, enabling execution on constrained targets with 8–16 KB task stacks.

**Acceptance criteria:**

1. `core::mem::size_of` for the aggregate interpreter state is ≤ 8 KB.

---

#### SBPF-1203  Safe wrapper for map-free programs

**Priority:** Must
**Source:** interpreter.rs:608–634
**Confidence:** High

**Description:**
The crate MUST provide a safe wrapper function `execute_program_no_maps` that calls `execute_program` with empty `maps` and `ctx_ptrs` slices. Since no raw pointer invariants need to be upheld, this wrapper MUST NOT require `unsafe` at the call site.

**Acceptance criteria:**

1. `execute_program_no_maps` is callable without `unsafe`.
2. It produces identical results to `execute_program` with `maps = &[]` and `ctx_ptrs = &[]`.

---

#### SBPF-1204  Concurrency model

**Priority:** Must
**Source:** safe-bpf-interpreter.md §3.3 (Concurrency model note)
**Confidence:** High

**Description:**
The interpreter MUST execute BPF programs single-threaded. Atomic operations (ADD, OR, AND, XOR, XCHG, CMPXCHG) implement RFC 9669 semantics but are emulated as non-atomic read/write sequences. This is correct for single-threaded execution.

**Acceptance criteria:**

1. Atomic operations produce correct results in single-threaded execution.
2. No `core::sync::atomic` or hardware atomics are used in the current implementation.

---

## 6  Dependencies

### DEP-001  RFC 9669

The interpreter depends on the BPF instruction set specification defined by RFC 9669.

### DEP-002  Host environment

The interpreter depends on the host environment (e.g., `sonde-node`, gateway decoder) to:
- Provide valid context buffers and map regions.
- Register helper functions with correct `HelperDescriptor` metadata.
- Ensure `MapRegion` pointers are valid and live for the duration of execution.

---

## 7  Assumptions

### ASM-001  Prevail verification

Programs are verified by the Prevail static verifier before reaching the interpreter. The tagged register model provides defense-in-depth against verifier bugs, not a replacement for verification.

### ASM-002  Host-provided memory safety

The caller of `execute_program` upholds the `unsafe` contract: `MapRegion` and `ContextPointerField` descriptors point to valid, live allocations that remain valid for the duration of execution and do not alias the BPF stack.

### ASM-003  Single-threaded execution

Only one BPF program runs at a time on a given interpreter instance. No concurrent access to shared map memory occurs during execution.

---

## 8  Risks

### RISK-001  Conservative rejection

The tagged interpreter is strictly more conservative than a raw interpreter. It may reject programs that the raw interpreter would execute successfully (e.g., spill table overflow, unusual pointer arithmetic patterns). This manifests as `NonDereferenceableAccess` faults for valid programs.

**Mitigation:** Such rejections indicate either a verifier bug or an interpreter assumption mismatch. The fallback is always safe — no unsound memory access occurs.

### RISK-002  Interpreter state size growth

Adding new region tags or increasing MAX_SPILL_SLOTS increases stack usage. On constrained targets (8 KB task stacks), this may cause stack overflow.

**Mitigation:** Monitor `size_of` in CI. Consider the compact `TaggedReg` representation (safe-bpf-interpreter.md §12.5) if state exceeds budget.

---

## 9  Revision history

| Date | Author | Description |
|------|--------|-------------|
| 2026-05-29 | Extracted by Copilot | Initial extraction from safe-bpf-interpreter.md design doc, interpreter.rs implementation, and safe-bpf-interpreter-validation.md test plan. |
