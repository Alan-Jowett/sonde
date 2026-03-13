<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Safe BPF Interpreter — Tagged Register Design

> **Document status:** Draft  
> **Scope:** A design for a Rust BPF interpreter that eliminates scattered `unsafe` by embedding memory-region provenance in every register.  
> **Audience:** Contributors implementing or reviewing the BPF interpreter.

---

## 1  Motivation

A typical Rust BPF interpreter stores registers as bare `u64` values and validates memory accesses by scanning a list of allowed regions (`mem` and `stack`) before each pointer dereference.  This works, but has two weaknesses:

1. **`unsafe` is scattered.**  Every load, store, and atomic instruction contains its own `unsafe` block — over a dozen scattered sites across `interpreter.rs`.  Each site must independently get the bounds-check-then-dereference sequence right.  A mistake in any one site is a soundness hole.

2. **Region identity is lost.**  The `check_mem` function knows that an address falls *somewhere* in a valid region, but not *which* region.  A pointer into the context could be used to write into map memory if the arithmetic happens to land in range.  Cross-region confusion is not caught.

### 1.1  Key insight

RFC 9669 requires registers to hold 64-bit values, but it does not prohibit the interpreter from storing additional metadata alongside each value.  By tagging every register with the **provenance**, **base**, and **bound** of the memory region it points to (if any), we can:

- Validate every memory access against a specific, known region — not a linear scan of all regions.
- Confine *all* pointer dereferences to a small set of choke-point functions (`mem_load`, `mem_load_sign_extend`, `mem_store`, `mem_atomic32`, `mem_atomic64`) — reducing the unsafe surface to 5 auditable sites.
- Make helper return types (e.g., the pointer from `map_lookup_elem`) carry machine-checked metadata that the interpreter enforces on subsequent use.

---

## 2  Tagged Register Model

### 2.1  Register layout

Each register becomes a **tagged register** — a `u64` value plus a region descriptor:

```rust
#[derive(Clone, Copy)]
struct TaggedReg {
    /// The 64-bit value visible to BPF instructions.
    value: u64,
    /// Memory-region metadata.  `None` means the value is a scalar
    /// (arithmetic result, immediate, length, etc.) and cannot be
    /// used as a pointer for load/store.
    region: Option<Region>,
}

#[derive(Clone, Copy)]
struct Region {
    /// What kind of memory this pointer refers to.
    tag: RegionTag,
    /// Inclusive lower bound of the valid address range.
    base: u64,
    /// Exclusive upper bound of the valid address range.
    end: u64,
}
```

The interpreter holds `reg: [TaggedReg; 11]` instead of `[u64; 11]`.  All arithmetic on `value` proceeds exactly as before — the tag is metadata that rides alongside it.

### 2.2  Region tags

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegionTag {
    /// Pointer into the BPF stack (R10-derived).
    Stack,
    /// Pointer into the input memory / context buffer (R1-derived).
    Context,
    /// Pointer into a map value returned by `map_lookup_elem`.
    /// `value_size` records the size of the individual value.
    MapValue { value_size: u32 },
    /// Opaque map descriptor loaded by LD_DW_IMM src=1.
    /// Not dereferenceable — only valid as an argument to map helpers.
    MapDescriptor { map_index: u32 },
}
```

A register with `region: None` is a **scalar**.  Scalars cannot be dereferenced.  Attempting to use a scalar as the base address of a load or store is a `NonDereferenceableAccess` error (§8).  The same error applies to `MapDescriptor` registers, which are opaque handles that cannot be directly read from or written to.

### 2.3  Scalar vs. pointer invariant

At any point during execution, every register is in exactly one of three states:

| State | `region` | Can be dereferenced? | Can be used in ALU? |
|-------|----------|---------------------|---------------------|
| **Scalar** | `None` | No | Yes (all ops) |
| **Pointer** | `Some(Region { tag: Stack \| Context \| MapValue })` | Yes (within bounds) | Limited (see §4.3) |
| **Handle** | `Some(Region { tag: MapDescriptor })` | No (opaque) | MOV only (see §4.3) |

`Pointer` and `Handle` both use `Some(Region { .. })`, but they differ in what operations are allowed.  A `Handle` is an opaque value that can only be copied (MOV) and passed to helpers — it cannot be dereferenced or participate in arithmetic.

This mirrors the type system that a BPF static verifier enforces at load time.  The tagged interpreter enforces it dynamically as a second line of defense.

---

## 3  Memory Access Choke Point

All pointer dereferences — loads, stores, and atomics — are routed through a small set of functions.  These are the **only** functions in the interpreter that contain `unsafe` code.

### 3.1  `mem_load`

```rust
/// Read `N` bytes from the region that `base_reg` points to, at
/// the signed offset `off`.  Returns the value zero-extended to u64.
///
/// This is one of two functions that perform unsafe memory reads
/// (the other is `mem_load_sign_extend`).  See §3.4 for the full
/// unsafe budget.
fn mem_load<const N: usize>(
    base_reg: &TaggedReg,
    off: i16,
    pc: usize,
) -> Result<u64, BpfError> {
    let region = base_reg.region.ok_or(BpfError::NonDereferenceableAccess { pc })?;

    // MapDescriptor is not dereferenceable.
    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }

    let addr = (base_reg.value as i64).wrapping_add(off as i64) as u64;
    let end  = addr.checked_add(N as u64)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: N })?;

    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: N });
    }

    // SAFETY: bounds validated above against a trusted region descriptor.
    // Region descriptors originate from:
    //   - Initialization: base/end derived from caller-provided slices
    //     (ctx, stack) that are guaranteed live for this call.
    //   - ALU propagation: inherits a previously validated region.
    //   - Helper returns (MapValue): the returned pointer is validated
    //     against the known map address range before tagging (§5.2),
    //     so it cannot point outside allocated map storage.
    // In all cases, the underlying memory is guaranteed live for the
    // duration of execute_program().
    let val = unsafe {
        match N {
            1 => *(addr as *const u8) as u64,
            2 => (addr as *const u16).read_unaligned() as u64,
            4 => (addr as *const u32).read_unaligned() as u64,
            8 => (addr as *const u64).read_unaligned(),
            _ => unreachable!(),
        }
    };
    Ok(val)
}
```

An identical `mem_load_sign_extend` variant handles LDSX instructions, casting through the signed type before widening.

### 3.2  `mem_store`

```rust
/// Write `N` bytes from `val` to the region that `base_reg` points to,
/// at the signed offset `off`.
///
/// This is the only function that performs a non-atomic unsafe memory
/// write.  Atomic writes go through `mem_atomic32` / `mem_atomic64`
/// (§3.3).  See §3.4 for the full unsafe budget.
fn mem_store<const N: usize>(
    base_reg: &TaggedReg,
    off: i16,
    val: u64,
    pc: usize,
) -> Result<(), BpfError> {
    let region = base_reg.region.ok_or(BpfError::NonDereferenceableAccess { pc })?;

    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }

    // Context memory is read-only.
    if matches!(region.tag, RegionTag::Context) {
        return Err(BpfError::ReadOnlyWrite { pc });
    }

    let addr = (base_reg.value as i64).wrapping_add(off as i64) as u64;
    let end  = addr.checked_add(N as u64)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: N })?;

    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: N });
    }

    // SAFETY: same argument as mem_load.
    unsafe {
        match N {
            1 => *(addr as *mut u8) = val as u8,
            2 => (addr as *mut u16).write_unaligned(val as u16),
            4 => (addr as *mut u32).write_unaligned(val as u32),
            8 => (addr as *mut u64).write_unaligned(val),
            _ => unreachable!(),
        }
    }
    Ok(())
}
```

### 3.3  `mem_atomic32` / `mem_atomic64`

Atomic read-modify-write operations (ADD, OR, AND, XOR, XCHG, CMPXCHG) are routed through two width-specific choke-point functions — `mem_atomic32` for 32-bit and `mem_atomic64` for 64-bit operations.  Each performs the bounds check once, then does the read-modify-write in a single `unsafe` block.  The signatures mirror the current `execute_atomic32` / `execute_atomic64` but accept a `&TaggedReg` for address validation.

> **Concurrency model:** BPF programs execute single-threaded — only one program runs at a time on a given interpreter instance.  The "atomic" operations implement the RFC 9669 instruction semantics (RFC 9669 §5.3) but are emulated as non-atomic `read_unaligned` / `write_unaligned` sequences.  This is correct for single-threaded execution.  If a future interpreter needs to support concurrent BPF-to-BPF execution with shared map memory, these operations would need to use `core::sync::atomic` or equivalent hardware atomics.

The same pre-checks apply as for `mem_store`: scalars and `MapDescriptor` are rejected via `NonDereferenceableAccess`, and `Context` (read-only) is rejected via `ReadOnlyWrite`.

### 3.4  Unsafe budget

| Function | `unsafe` blocks | Purpose |
|----------|:-:|---------|
| `mem_load` | 1 | Read N bytes from validated region |
| `mem_load_sign_extend` | 1 | Read + sign-extend |
| `mem_store` | 1 | Write N bytes to validated region |
| `mem_atomic32` | 1 | 32-bit atomic RMW |
| `mem_atomic64` | 1 | 64-bit atomic RMW |
| **Total** | **5** | vs. many scattered sites in a typical interpreter |

Every `unsafe` block is preceded by the same three-step validation: (1) confirm pointer provenance, (2) reject non-dereferenceable tags, (3) bounds-check `[addr, addr+N)` against `[region.base, region.end)`.  This pattern is auditable in one place.

---

## 4  Instruction Semantics

### 4.1  Initialization

At program start, three registers carry pointer provenance:

| Register | Value | Region |
|----------|-------|--------|
| R1 | `ctx.as_ptr() as u64` | `Some(Region { tag: Context, base: ctx.as_ptr() as u64, end: (ctx.as_ptr() as u64).checked_add(ctx.len() as u64).unwrap() })` |
| R2 | `ctx.len() as u64` | `None` (scalar — length, not a pointer) |
| R10 | `stack.as_ptr() as u64 + STACK_SIZE as u64` | `Some(Region { tag: Stack, base: stack.as_ptr() as u64, end: (stack.as_ptr() as u64).checked_add(STACK_SIZE as u64).unwrap() })` |
| R0, R3–R9 | 0 | `None` (scalar) |

> **Overflow safety:** All region `end` values must be computed with `checked_add`.  An overflow indicates a logic error in the caller (impossible memory layout) and should panic or return an error before execution begins.

### 4.2  LD_DW_IMM (64-bit immediate load)

| `src` field | Semantics | Result tag |
|-------------|-----------|------------|
| 0 | Load 64-bit immediate | Scalar |
| 1 | Map descriptor relocation | `MapDescriptor { map_index: imm }` |

For src=1, the interpreter resolves the map index and loads the relocated map pointer.  The `imm` field is a signed `i32` in the instruction encoding (see `ebpf.rs`); negative values are invalid and must be rejected with `InvalidMapIndex` before any cast or indexing.  After validation, the non-negative `imm` is used as the index into the `maps` slice.  The result is tagged `MapDescriptor` — it is an opaque handle, valid only as an argument to `map_lookup_elem` or `map_update_elem`.  It is **not dereferenceable**.

### 4.3  ALU operations and pointer arithmetic

BPF ALU instructions have the form `dst = dst OP src` (or `dst = dst OP imm`).  The table below defines how the **dst** and **src** tags interact to determine the result tag.  In this table, "pointer" means a dereferenceable pointer (`Stack`, `Context`, or `MapValue`).  Immediates are always scalar.

| Operation | `dst` tag | `src` tag | Result tag written to `dst` |
|-----------|-----------|-----------|----------------------------|
| ADD | pointer | scalar | pointer (same region as dst) |
| ADD | scalar | pointer | pointer (same region as src) |
| ADD | scalar | scalar | scalar |
| ADD | pointer | pointer | **error** (`InvalidPointerArithmetic`) |
| SUB | pointer | scalar | pointer (same region as dst) |
| SUB | pointer(A) | pointer(A) | scalar (difference within same region) |
| SUB | pointer(A) | pointer(B) | **error** (`InvalidPointerArithmetic`) |
| SUB | scalar | scalar | scalar |
| SUB | scalar | pointer | **error** (`InvalidPointerArithmetic`) |
| MUL, DIV, MOD, LSH, RSH, ARSH | any | any | scalar |
| NEG | any | — | scalar |
| AND, OR, XOR | scalar | scalar | scalar |
| AND, OR, XOR | pointer | any | **error** (`InvalidPointerArithmetic`) |
| MOV (reg) | — | any | inherits src tag |
| MOV (imm) | — | — | scalar |

**`MapDescriptor` (Handle) precedence rule:**  `MapDescriptor` is an opaque handle, not a dereferenceable address.  **Before consulting the table above**, check whether either `dst` or `src` carries a `MapDescriptor` tag.  If so, the only permitted operation is `MOV` (reg-to-reg copy) — all other ALU operations return `InvalidPointerArithmetic`.  The "any" entries in the table (e.g., `MUL any any → scalar`) apply only to scalars and dereferenceable pointers, not to handles.

**Rationale:**  Only ADD and SUB have defined meaning for pointers.  All other arithmetic destroys provenance.  A BPF static verifier already enforces these rules at load time; the interpreter enforces them dynamically as defense-in-depth.

When a pointer participates in a valid ADD or SUB, the result inherits the same `region` (same `tag`, `base`, and `end`).  The `value` changes but the valid bounds do not — so a subsequent dereference will still be checked against the original region.

**32-bit ALU (ALU32):** 32-bit operations always produce scalars.  A pointer that passes through a 32-bit ALU instruction loses its tag because the upper 32 bits are zeroed, invalidating the address.

### 4.4  Load and store instructions

Load and store instructions call into the choke-point functions from §3:

```
LDX_B:   reg[dst] = scalar(mem_load::<1>(&reg[src], off, pc)?)
LDX_H:   reg[dst] = scalar(mem_load::<2>(&reg[src], off, pc)?)
LDX_W:   reg[dst] = scalar(mem_load::<4>(&reg[src], off, pc)?)
LDX_DW:  reg[dst] = scalar(mem_load::<8>(&reg[src], off, pc)?)
         // then: if src is Stack, check spill tracker (§6)

LDSX_B:  reg[dst] = scalar(mem_load_sign_extend::<1>(&reg[src], off, pc)?)
LDSX_H:  reg[dst] = scalar(mem_load_sign_extend::<2>(&reg[src], off, pc)?)
LDSX_W:  reg[dst] = scalar(mem_load_sign_extend::<4>(&reg[src], off, pc)?)

ST_B_IMM:  mem_store::<1>(&reg[dst], off, imm as u64, pc)?
ST_H_IMM:  mem_store::<2>(&reg[dst], off, imm as u64, pc)?
ST_W_IMM:  mem_store::<4>(&reg[dst], off, imm as u64, pc)?
ST_DW_IMM: mem_store::<8>(&reg[dst], off, imm as u64, pc)?

STX_B:   mem_store::<1>(&reg[dst], off, reg[src].value, pc)?
STX_H:   mem_store::<2>(&reg[dst], off, reg[src].value, pc)?
STX_W:   mem_store::<4>(&reg[dst], off, reg[src].value, pc)?
STX_DW:  mem_store::<8>(&reg[dst], off, reg[src].value, pc)?
         // then: if dst is Stack and src has pointer provenance,
         //        record in spill tracker (§6)
```

Values loaded from memory are tagged as **scalar** by default.  The one exception is `LDX_DW` from a stack address that the spill tracker (§6) recognises as a spilled pointer — in that case, the destination register inherits the spilled pointer's provenance.  See §6.3 for details.

### 4.5  Jumps and comparisons

Jump instructions compare `reg[dst].value` against `reg[src].value` (or an immediate).  Tag metadata is not involved — comparisons are always on the raw 64-bit value.  No tags are created or consumed.

### 4.6  CALL and EXIT

**Helper call (src=0 or src=2):**

1. Pass `reg[1..=5].value` to the helper function (raw u64 arguments).
2. Receive the raw u64 return value.
3. Tag R0 according to the helper's return-type descriptor (see §5).
4. Clobber R1–R5 to scalar — clear their region tags (set to `None`).  The raw u64 values are left as-is (the helper may have read them, but the caller must not rely on them).  This matches the BPF calling convention where R1–R5 are caller-saved and undefined after a call.

> **Note — behavioral change:** The current interpreter uses bare `[u64; 11]` registers (no tags) and leaves R1–R5 values unchanged after helper calls (only R0 is written).  The tagged design introduces tag-clearing on R1–R5 as a safety measure to prevent stale pointer provenance from leaking across call boundaries.  This is stricter than the current implementation but consistent with the BPF calling convention.  Existing tests that rely on R1–R5 values surviving a helper call may need adjustment.

**BPF-to-BPF call (src=1):**

1. Save R6–R9 values *and* tags in the call frame (see §7).
2. R1–R5 are **retained** — they are the callee's arguments and must keep their provenance.  (The caller treats them as clobbered by convention, but the interpreter does not force-clear them on entry.)
3. Adjust R10 (frame pointer) — the Stack tag is preserved with the same `base`/`end` (the entire stack is one region).

**EXIT:**

1. Restore R6–R9 values and tags from the call frame.
2. Restore R10.
3. R0 retains whatever tag it had (return value propagation).

---

## 5  Helper Integration

### 5.1  Helper descriptors

Each registered helper carries a **return-type descriptor** that tells the interpreter how to tag R0 after the call:

```rust
enum HelperReturn {
    /// R0 is a plain scalar value (most helpers).
    Scalar,
    /// R0 is a pointer into a map value, or NULL (0).
    /// The map is identified by the value in the specified argument
    /// register at call time (typically R1).
    MapValueOrNull { map_arg: u8 },
}

struct HelperDescriptor {
    id: u32,
    func: Helper,
    ret: HelperReturn,
}
```

### 5.2  Return-type resolution

After a helper call, the interpreter examines the descriptor's `ret` field:

| `HelperReturn` | R0 value | Tag applied to R0 |
|----------------|----------|-------------------|
| `Scalar` | any | `None` (scalar) |
| `MapValueOrNull` | 0 | `None` (scalar — NULL means not found) |
| `MapValueOrNull` | non-zero | `MapValue { value_size }` with `base = R0`, `end = R0.checked_add(value_size)` (overflow → fatal error) — **only after validation** (see below) |

For `MapValueOrNull`, the interpreter resolves the map's `value_size` from the map definitions provided at load time.  The argument register identified by `map_arg` **must** carry a `MapDescriptor { map_index }` tag (set by LD_DW_IMM relocation, §4.2).  If `reg[map_arg]` does not have a `MapDescriptor` tag, the call is rejected with `InvalidHelperArgument` — this indicates a program bug (e.g., passing a scalar or wrong pointer type to `map_lookup_elem`).

**Helper return validation:**  Helper functions are part of the host environment and could be buggy.  To prevent a faulty helper from returning an arbitrary pointer that the interpreter then trusts, the returned pointer must be validated against the known map address range before tagging:

1. Look up the `MapRegion` for the map identified by `map_index`.
2. Compute `end = R0.checked_add(value_size)`.  If this overflows, return a fatal `MemoryAccessViolation` error.
3. Verify that `R0 >= map_region.data_start` and `end <= map_region.data_end`.
4. If the pointer falls outside the map's allocated storage, return a fatal `MemoryAccessViolation` error — do not tag it.

This requires `MapRegion` to carry the map's backing storage bounds (see §10.1).  With this check, the safety argument for `mem_load` / `mem_store` is closed: region descriptors are either derived from caller-provided slices (init), inherited from a previously validated region (ALU), or validated against known allocations (helper returns).

### 5.3  Example helper classifications

The table below shows how a typical BPF environment would classify its helpers:

| ID | Helper | Return type |
|----|--------|-------------|
| 1 | `map_lookup_elem` | `MapValueOrNull { map_arg: 1 }` |
| 2 | `map_update_elem` | `Scalar` |
| 3+ | Application-specific helpers | `Scalar` (unless they return pointers) |

In most BPF environments, `map_lookup_elem` is the only helper that returns a pointer.  All other helpers return scalar values.  The `HelperDescriptor` framework is extensible — new `HelperReturn` variants can be added for helpers that return pointers to other region types.

---

## 6  Stack Spill Tracking

### 6.1  The problem

Compilers routinely spill registers to the stack when they run out of physical registers.  If a pointer register is stored to the stack (STX_DW) and later loaded back (LDX_DW), the loaded value is a bare `u64` — its provenance is lost.

Without spill tracking, the reloaded value would be tagged as scalar and could not be used as a pointer.  This would break valid, verifier-approved programs.

### 6.2  Shadow slot table

The interpreter maintains a compact **shadow table** that records provenance metadata for stack slots that contain spilled pointers.

```rust
/// Tracks pointer provenance for spilled stack slots.
struct SpillTracker {
    /// Bitmap: 1 bit per 8-byte-aligned stack slot.
    /// Bit set = this slot holds a spilled pointer.
    bitmap: [u8; STACK_SIZE / 64],          // 64 bytes for 4 KB stack

    /// Metadata for slots that contain pointers.
    entries: [SpillEntry; MAX_SPILL_SLOTS], // small fixed-size table
    count: u8,
}

struct SpillEntry {
    /// Absolute byte offset from the base of the full stack allocation
    /// (`stack.as_ptr()`), NOT relative to the current frame's R10.
    /// This ensures spills from different call frames never collide
    /// even though R10 is adjusted by `STACK_SIZE_PER_FRAME` on
    /// BPF-to-BPF calls.  The bitmap index for a given access is
    /// computed as `(addr - stack_base) / 8`.
    stack_offset: u16,
    /// The provenance that was spilled.
    region: Region,
}

/// Maximum concurrent pointer spills tracked.
/// Typical BPF programs spill 2–4 pointers; 32 is generous.
const MAX_SPILL_SLOTS: usize = 32;
```

**Total overhead:** 64 B (bitmap) + (32 entries × 32 B each = 1,024 B) + 1 B (count) ≈ **1,089 bytes** — well within the interpreter's stack budget.

### 6.3  Operations

**On STX_DW to the stack:** If the source register has pointer provenance AND the access is 8-byte aligned:

1. Set the corresponding bit in `bitmap`.
2. Insert or update an entry in `entries` with the slot offset and the source register's `Region`.
3. If `entries` is full, clear the bitmap bit and skip (the reloaded value will be scalar — a safe fallback that may reject valid programs but never accepts invalid ones).

**On LDX_DW from the stack:** If the bitmap bit for this 8-byte-aligned slot is set:

1. Look up the `SpillEntry` for this offset.
2. Tag the destination register with the spilled `Region` and the loaded `value`.

Otherwise, the loaded value is tagged as scalar.

**On any store to the stack (any size, any alignment):** If the write overlaps any 8-byte slot with a set bitmap bit:

1. Clear the bitmap bit.
2. Remove the entry from `entries`.

This prevents a partial overwrite from leaving stale pointer metadata in the shadow table.

### 6.4  Correctness argument

- **No false positives:** A slot is only marked as a pointer when the interpreter itself sees a pointer-tagged register stored there.  The metadata is copied from the register, which was set by trusted interpreter code.
- **Safe fallback:** If the spill table overflows, reloaded values become scalars.  This may cause the program to fault (`NonDereferenceableAccess`), but it can never cause an unsound memory access.
- **Partial-overwrite safety:** Any write that touches a pointer slot invalidates it, preventing use of stale metadata.

---

## 7  Call Frame Metadata

### 7.1  Saving and restoring

The current `CallFrame` saves the u64 values of R6–R9.  In the tagged model, it must also save their `Region` metadata:

```rust
struct CallFrame {
    saved_regs: [u64; 4],              // r6–r9 values (matches current field name)
    saved_regions: [Option<Region>; 4], // r6–r9 tags (new)
    return_pc: usize,
    frame_size: u64,
}
```

On BPF-to-BPF CALL: save R6–R9 values *and* regions.  
On EXIT: restore both.

The frame pointer R10 always carries the `Stack` tag.  Its `base` and `end` span the entire stack allocation and do not change across call frames — only the `value` (which moves by `STACK_SIZE_PER_FRAME` per frame) changes.

---

## 8  Error Model

The tagged interpreter introduces five new error variants:

```rust
pub enum BpfError {
    // ... existing variants ...

    /// A register was dereferenced but does not carry a dereferenceable
    /// tag (e.g., it is a scalar or a `MapDescriptor` handle).
    NonDereferenceableAccess { pc: usize },

    /// A helper argument register does not carry the expected tag —
    /// e.g., the `map_arg` register is not a `MapDescriptor` when
    /// calling `map_lookup_elem`.
    InvalidHelperArgument { pc: usize, arg: u8 },

    /// Attempted to write to a read-only region (e.g., Context).
    ReadOnlyWrite { pc: usize },

    /// Pointer arithmetic that violates provenance rules
    /// (e.g., pointer + pointer, bitwise op on pointer).
    InvalidPointerArithmetic { pc: usize },

    /// LD_DW_IMM src=1 referenced a map index that is out of range
    /// of the provided `maps` slice, or the `imm` field is negative.
    InvalidMapIndex { pc: usize, index: i32 },
}
```

The existing `MemoryAccessViolation` is retained for out-of-bounds accesses within a valid region.

**Error handling policy:**  All errors are fatal — the program is terminated immediately.  This is consistent with standard BPF interpreter behavior.

---

## 9  Performance Analysis

### 9.1  Memory overhead

Size estimates below are approximate and based on typical Rust layout for `x86_64` targets.  Actual sizes may vary by compiler version, target architecture, and optimization level — use `core::mem::size_of` to verify on a specific platform.

`TaggedReg`: `value`(8) + `Option<Region>`(~32) ≈ **40 bytes**.  
`CallFrame` (tagged): `saved_regs`(32) + `saved_regions`(4 × ~32 = ~128) + `return_pc`(8) + `frame_size`(8) ≈ **176 bytes**.

| Component | Current | Tagged |
|-----------|---------|--------|
| Registers (11) | 88 B | 440 B |
| BPF stack | 4,096 B | 4,096 B |
| Spill tracker | — | ~1,100 B |
| Call frames (8) | 384 B | 1,408 B |
| **Total interpreter state** | **~4,568 B** | **~7,044 B** |

An increase of ~2.5 KB.  The tagged interpreter still fits in a single Rust stack frame, even on constrained targets with 8–16 KB task stacks.

> **Note:** These sizes can be reduced by reordering struct fields, using `#[repr(C)]` with manual layout, or encoding `Option<Region>` as a sentinel tag value.  A compact representation could bring `TaggedReg` down to 32 bytes (see §12.5).

### 9.2  Instruction overhead

| Operation | Current cost | Tagged cost | Delta |
|-----------|-------------|-------------|-------|
| ALU (scalar) | ALU op | ALU op + tag clear | +1 write |
| ALU (ptr + scalar) | ALU op | ALU op + tag copy | +1 copy |
| Load/Store | `check_mem` (scan 2 regions) + deref | Tag check + bounds check + deref | Similar or faster (no scan) |
| Helper call | call + set R0 | call + set R0 + tag R0 | +1 write |

The per-instruction overhead is a handful of tag copies and comparisons — well within the noise for an interpreter that already does instruction decode, register indexing, and bounds checking on every memory access.

The region scan in `check_mem` (current: compare against `mem` *and* `stack` pointer ranges) is replaced by a single bounds check against the register's own `base`/`end`.  With map memory added as a third region, the tagged approach becomes *faster* than a linear scan.

### 9.3  Zero-allocation guarantee

The tagged interpreter maintains the zero-allocation property.  All state — `TaggedReg` array, `SpillTracker`, `CallFrame` array — lives on the Rust call stack.  No `Vec`, `Box`, or heap allocation occurs during execution.

---

## 10  Migration Path

The redesign changes the `execute_program` public API.  This is an intentional breaking change — the new signature encodes the safety invariants that the tagged interpreter requires.

### 10.1  `execute_program` signature

```rust
// Current
pub fn execute_program(
    prog: &[u8],
    mem: &mut [u8],
    helpers: &[(u32, Helper)],
) -> Result<u64, BpfError>;

// Tagged — context is read-only; map definitions added
pub fn execute_program(
    prog: &[u8],
    ctx: &[u8],
    helpers: &[HelperDescriptor],
    maps: &[MapRegion],
) -> Result<u64, BpfError>;
```

The `mem` parameter is renamed to `ctx` and changed from `&mut [u8]` to `&[u8]`.  The tagged interpreter enforces Context as read-only (§3.2), so the API should reflect this.  If a future use case requires a mutable input region, a separate parameter (e.g., `scratch: &mut [u8]`) can be added with its own `RegionTag` variant.

**Migration steps for existing callers:**

1. Change `mem: &mut [u8]` → `ctx: &[u8]` at call sites.
2. Replace `&[(u32, Helper)]` with `&[HelperDescriptor]`, adding `ret: HelperReturn::Scalar` for most helpers and `ret: HelperReturn::MapValueOrNull { map_arg: 1 }` for `map_lookup_elem`.
3. Provide a `maps: &[MapRegion]` slice with relocated pointer, value size, and backing storage bounds for each map.
4. Update tests that rely on R1–R5 surviving helper calls (§4.6 behavioral change).

Where `MapRegion` provides the metadata needed to tag LD_DW_IMM relocations and `map_lookup_elem` returns:

```rust
struct MapRegion {
    /// Relocated pointer value (matches the value loaded by LD_DW_IMM src=1).
    relocated_ptr: u64,
    /// Size of each value in this map.
    value_size: u32,
    /// Inclusive start of the map's backing storage.
    data_start: u64,
    /// Exclusive end of the map's backing storage.
    data_end: u64,
}
```

The `data_start` / `data_end` fields define the bounds of the map's allocated memory.  They are used to validate helper return pointers (§5.2) before tagging — a returned pointer that falls outside `[data_start, data_end)` is rejected as a fatal error.  This closes the trust boundary: helpers do not need to be in the trusted computing base for memory safety.

The `maps` slice is indexed by `map_index` — the same index used in the LD_DW_IMM instruction's `imm` field and stored in the `MapDescriptor { map_index }` tag.  The mapping is:

- LD_DW_IMM src=1, imm=*i* → `maps[i].relocated_ptr` is loaded into the register value, tagged as `MapDescriptor { map_index: i }`.
- Helper return resolution (§5.2) → `maps[reg[map_arg].region.tag.map_index].value_size` gives the value size.
- Out-of-bounds `map_index` (≥ `maps.len()`) is a fatal `InvalidMapIndex` error at LD_DW_IMM time.

### 10.2  Helper registration

Helpers change from `(u32, Helper)` pairs to `HelperDescriptor` structs that include return-type metadata (§5.1).

### 10.3  Interpreter trait

If the interpreter is abstracted behind a trait, the `load()` method will additionally need map value sizes — either by extending map pointer parameters to carry sizes, or by adding a parallel `map_value_sizes: &[u32]` parameter.

### 10.4  Existing tests

The implementation should keep all existing interpreter tests passing.  Tests that exercise pointer arithmetic, map access, and stack spills will gain additional coverage from the tag enforcement.  Some tests may need updates to supply the new `maps` parameter or to expect new error variants (e.g., `NonDereferenceableAccess` instead of `MemoryAccessViolation` for scalar dereferences).

---

## 11  Future Extensions

### 11.1  Read-only map enforcement

Some program classes may require read-only map access.  With tagged regions, this is trivial: use a `MapValueReadOnly` tag variant.  `mem_store` rejects writes to read-only map regions, just as it rejects writes to Context.

### 11.2  Instruction metering

The tagged interpreter loop is a natural place to add an instruction counter for execution budgets.  Each iteration decrements a counter; exhaustion returns a new `InstructionBudgetExceeded` error.

### 11.3  Context field access tracking

The `Context` region could be refined to track individual field accesses within the context struct, enabling the interpreter to report which context fields a program actually reads — useful for diagnostics.

### 11.4  Taint tracking

The tag infrastructure could be extended to track whether a register's value has been influenced by external input (e.g., data received from helpers).  This is not planned for the initial implementation but the machinery is in place.

---

## 12  Design Decisions and Alternatives

### 12.1  Why not just improve `check_mem`?

Adding map regions to the existing `check_mem` scan would work but doesn't solve the fundamental problem: every load/store site is a separate `unsafe` block that must independently get the bounds check + dereference sequence right.  The tagged approach structurally prevents "forgot to call check_mem" bugs.

### 12.2  Why not a safe abstraction over slices?

An alternative is to replace raw pointer dereferences with Rust slice indexing (e.g., convert the address back to an offset into `mem` or `stack` and index the slice).  This eliminates `unsafe` entirely but requires an address-to-region-and-offset lookup on every access — which is effectively what the tagged register provides, but less ergonomically.  The tagged approach is a superset: it enables slice-based access as an implementation strategy within `mem_load` if desired.

### 12.3  Why a bitmap + table for spill tracking?

A full shadow array (one `Option<Region>` per 8-byte stack slot) would cost 512 × 32 = 16,384 bytes.  The bitmap + table approach costs ~1,100 bytes for the same capability, at the expense of a linear scan of up to 32 entries on spill restore.  Given that BPF programs rarely have more than a handful of live pointer spills, the linear scan is negligible.

### 12.4  What if a valid program is rejected?

The tagged interpreter is strictly more conservative than the raw interpreter: it may reject programs that the raw interpreter would execute successfully (e.g., if the spill table overflows, or if pointer arithmetic doesn't match the expected patterns).  However:

1. A BPF static verifier already enforces the same rules at load time.  Any program that passes verification should also pass the dynamic tag checks.
2. If a discrepancy is found, it indicates either a verifier bug or an interpreter bug — both worth investigating.
3. The fallback behavior (fault the program) is always safe — it can never cause an unsound memory access.

### 12.5  Compact `TaggedReg` representation

The default Rust layout for `TaggedReg` is 40 bytes due to alignment padding in `Option<Region>`.  For constrained environments, a manual layout can reduce this to 32 bytes:

```rust
struct TaggedReg {
    value: u64,          // 8 bytes
    base: u64,           // 8 bytes (0 when scalar)
    end: u64,            // 8 bytes (0 when scalar)
    tag: u8,             // 1 byte: 0=Scalar, 1=Stack, 2=Context, 3=MapValue, 4=MapDescriptor
    _pad: [u8; 3],       // 3 bytes padding
    tag_data: u32,       // 4 bytes: value_size or map_index (tag-dependent)
}
// Total: 32 bytes, no Option overhead
```

This trades the ergonomic `Option<Region>` for a manually discriminated struct. A `tag` value of 0 (Scalar) means `base`, `end`, and `tag_data` are unused — equivalent to `region: None`.  This representation brings registers to 11 × 32 = 352 bytes and call frames to 8 × 80 = 640 bytes, saving ~800 bytes of interpreter state.
