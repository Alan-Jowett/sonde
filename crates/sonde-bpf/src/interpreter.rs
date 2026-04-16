// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BPF interpreter — zero heap allocation during execution.
//!
//! Implements the BPF instruction set per RFC 9669, covering:
//! - ALU32 / ALU64 arithmetic (including SDIV, SMOD, MOVSX)
//! - Byte swap instructions (LE, BE, BSWAP)
//! - JMP / JMP32 conditional and unconditional branches
//! - CALL (helper functions + BPF-to-BPF local calls) and EXIT
//! - Load/Store MEM, sign-extension loads (MEMSX)
//! - 64-bit immediate load (LD_DW_IMM)
//! - Atomic operations (ADD, OR, AND, XOR, XCHG, CMPXCHG, +FETCH)
//!
//! All memory accesses are validated through tagged registers.  Each pointer
//! register carries a [`Region`] descriptor that bounds-checks every load and
//! store at a small number of choke-point functions, eliminating scattered
//! unsafe blocks throughout the interpreter loop.

use crate::ebpf::{self, Helper, INSN_SIZE, MAX_CALL_DEPTH, STACK_SIZE, STACK_SIZE_PER_FRAME};

/// Errors returned by the interpreter.
#[derive(Debug)]
pub enum BpfError {
    /// Program counter went out of bounds.
    OutOfBounds { pc: usize },
    /// Unknown / unsupported opcode.
    UnknownOpcode { pc: usize, opc: u8 },
    /// Unknown helper function id.
    UnknownHelper { pc: usize, id: u32 },
    /// Call stack overflow.
    CallDepthExceeded { pc: usize },
    /// Memory access out of the provided regions.
    MemoryAccessViolation { pc: usize, addr: u64, len: usize },
    /// Attempted to dereference a register with no valid region.
    NonDereferenceableAccess { pc: usize },
    /// Invalid argument passed to a helper function.
    InvalidHelperArgument { pc: usize, arg: u8 },
    /// Attempted to write to a read-only region (e.g. context).
    ReadOnlyWrite { pc: usize },
    /// Pointer arithmetic that would produce an invalid result.
    InvalidPointerArithmetic { pc: usize },
    /// Invalid map index in a LD_DW_IMM relocation.
    InvalidMapIndex { pc: usize, index: i32 },
    /// The program exceeded the instruction budget.
    InstructionBudgetExceeded { pc: usize },
}

impl core::fmt::Display for BpfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds { pc } => write!(f, "program counter out of bounds at insn #{pc}"),
            Self::UnknownOpcode { pc, opc } => write!(f, "unknown opcode {opc:#04x} at insn #{pc}"),
            Self::UnknownHelper { pc, id } => {
                write!(f, "unknown helper function {id:#x} at insn #{pc}")
            }
            Self::CallDepthExceeded { pc } => write!(f, "call depth exceeded at insn #{pc}"),
            Self::MemoryAccessViolation { pc, addr, len } => write!(
                f,
                "memory access violation at insn #{pc}: addr={addr:#x} len={len}"
            ),
            Self::NonDereferenceableAccess { pc } => {
                write!(f, "non-dereferenceable access at insn #{pc}")
            }
            Self::InvalidHelperArgument { pc, arg } => {
                write!(f, "invalid helper argument r{arg} at insn #{pc}")
            }
            Self::ReadOnlyWrite { pc } => {
                write!(f, "write to read-only region at insn #{pc}")
            }
            Self::InvalidPointerArithmetic { pc } => {
                write!(f, "invalid pointer arithmetic at insn #{pc}")
            }
            Self::InvalidMapIndex { pc, index } => {
                write!(f, "invalid map index {index} at insn #{pc}")
            }
            Self::InstructionBudgetExceeded { pc } => {
                write!(f, "instruction budget exceeded at insn #{pc}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BpfError {}

// ── Tagged register types ───────────────────────────────────────────

/// Describes the kind of memory region a pointer register refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionTag {
    Stack,
    /// Read-only input memory (writes silently ignored per ND-0505 AC6).
    Context,
    /// Writable input memory (same as Context but allows stores).
    Memory,
    MapValue {
        value_size: u32,
    },
    MapDescriptor {
        map_index: u32,
    },
}

/// A validated memory region that a pointer register is allowed to access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub tag: RegionTag,
    pub base: u64,
    pub end: u64,
}

/// A register value paired with an optional region descriptor.
#[derive(Clone, Copy, Debug)]
struct TaggedReg {
    value: u64,
    region: Option<Region>,
}

impl TaggedReg {
    const fn scalar(value: u64) -> Self {
        Self {
            value,
            region: None,
        }
    }
    const fn zeroed() -> Self {
        Self {
            value: 0,
            region: None,
        }
    }
}

// ── Helper descriptor types ─────────────────────────────────────────

/// Describes what a helper function returns for tag propagation.
#[derive(Clone, Copy, Debug)]
pub enum HelperReturn {
    Scalar,
    MapValueOrNull { map_arg: u8 },
}

/// A helper function with its metadata for tag-aware dispatch.
#[derive(Clone, Copy)]
pub struct HelperDescriptor {
    pub id: u32,
    pub func: Helper,
    pub ret: HelperReturn,
}

/// Describes a map region for bounds validation.
#[derive(Clone, Copy, Debug)]
pub struct MapRegion {
    pub relocated_ptr: u64,
    pub value_size: u32,
    pub data_start: u64,
    pub data_end: u64,
}

// ── Spill tracker ───────────────────────────────────────────────────

const MAX_SPILL_SLOTS: usize = 32;

#[derive(Clone, Copy)]
struct SpillEntry {
    stack_offset: u16,
    region: Region,
}

struct SpillTracker {
    bitmap: [u8; STACK_SIZE / 64],
    entries: [SpillEntry; MAX_SPILL_SLOTS],
    count: u8,
}

impl SpillTracker {
    fn new() -> Self {
        Self {
            bitmap: [0u8; STACK_SIZE / 64],
            entries: [SpillEntry {
                stack_offset: 0,
                region: Region {
                    tag: RegionTag::Stack,
                    base: 0,
                    end: 0,
                },
            }; MAX_SPILL_SLOTS],
            count: 0,
        }
    }

    fn record_spill(&mut self, stack_base: u64, addr: u64, region: Region) {
        let offset = addr.wrapping_sub(stack_base) as usize;
        let slot = offset / 8;
        if slot >= STACK_SIZE / 8 {
            return;
        }
        let byte_idx = slot / 8;
        let bit_idx = slot % 8;
        self.bitmap[byte_idx] |= 1 << bit_idx;

        let offset_u16 = offset as u16;
        for i in 0..self.count as usize {
            if self.entries[i].stack_offset == offset_u16 {
                self.entries[i].region = region;
                return;
            }
        }
        if (self.count as usize) < MAX_SPILL_SLOTS {
            self.entries[self.count as usize] = SpillEntry {
                stack_offset: offset_u16,
                region,
            };
            self.count += 1;
        } else {
            // Table full — clear the bitmap bit so check_restore won't
            // see a hit without a corresponding entry.  The reloaded
            // value becomes scalar (safe fallback).
            self.bitmap[byte_idx] &= !(1 << bit_idx);
        }
    }

    fn check_restore(&self, stack_base: u64, addr: u64) -> Option<Region> {
        let offset = addr.wrapping_sub(stack_base) as usize;
        let slot = offset / 8;
        if slot >= STACK_SIZE / 8 {
            return None;
        }
        let byte_idx = slot / 8;
        let bit_idx = slot % 8;
        if self.bitmap[byte_idx] & (1 << bit_idx) == 0 {
            return None;
        }
        let offset_u16 = offset as u16;
        for i in 0..self.count as usize {
            if self.entries[i].stack_offset == offset_u16 {
                return Some(self.entries[i].region);
            }
        }
        None
    }

    fn invalidate(&mut self, stack_base: u64, addr: u64, len: usize) {
        let offset_start = addr.wrapping_sub(stack_base) as usize;
        let offset_end = offset_start.saturating_add(len);
        let first_slot = offset_start / 8;
        let last_slot = if offset_end == 0 {
            0
        } else {
            (offset_end - 1) / 8
        };
        for slot in first_slot..=last_slot {
            if slot >= STACK_SIZE / 8 {
                break;
            }
            let byte_idx = slot / 8;
            let bit_idx = slot % 8;
            self.bitmap[byte_idx] &= !(1 << bit_idx);
        }
    }
}

// ── Call frame ──────────────────────────────────────────────────────

/// Saved state for a BPF-to-BPF call frame (no heap allocation).
#[derive(Clone, Copy)]
struct CallFrame {
    /// Saved callee-saved registers r6-r9.
    saved_regs: [u64; 4],
    /// Saved region tags for r6-r9.
    saved_regions: [Option<Region>; 4],
    /// Return address (instruction index).
    return_pc: usize,
    /// Frame pointer adjustment applied.
    frame_size: u64,
}

impl CallFrame {
    const fn zeroed() -> Self {
        Self {
            saved_regs: [0; 4],
            saved_regions: [None; 4],
            return_pc: 0,
            frame_size: 0,
        }
    }
}

/// Compute and validate a jump target. Returns `Err` if the target is out of bounds.
#[inline]
fn check_jump(pc: usize, offset: isize, num_insns: usize) -> Result<usize, BpfError> {
    let target = pc as isize + offset;
    if target < 0 || target as usize >= num_insns {
        return Err(BpfError::OutOfBounds { pc: pc - 1 });
    }
    Ok(target as usize)
}

// ── Choke-point memory access functions ─────────────────────────────

#[inline]
fn mem_load<const N: usize>(base_reg: &TaggedReg, off: i16, pc: usize) -> Result<u64, BpfError> {
    let region = base_reg
        .region
        .ok_or(BpfError::NonDereferenceableAccess { pc })?;
    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }
    let addr = base_reg.value.wrapping_add_signed(off as i64);
    let end = addr
        .checked_add(N as u64)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: N })?;
    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: N });
    }
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

#[inline]
fn mem_load_sign_extend<const N: usize>(
    base_reg: &TaggedReg,
    off: i16,
    pc: usize,
) -> Result<u64, BpfError> {
    let region = base_reg
        .region
        .ok_or(BpfError::NonDereferenceableAccess { pc })?;
    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }
    let addr = base_reg.value.wrapping_add_signed(off as i64);
    let end = addr
        .checked_add(N as u64)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: N })?;
    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: N });
    }
    let val = unsafe {
        match N {
            1 => *(addr as *const i8) as i64 as u64,
            2 => (addr as *const i16).read_unaligned() as i64 as u64,
            4 => (addr as *const i32).read_unaligned() as i64 as u64,
            _ => unreachable!(),
        }
    };
    Ok(val)
}

#[inline]
fn mem_store<const N: usize>(
    base_reg: &TaggedReg,
    off: i16,
    val: u64,
    pc: usize,
) -> Result<(), BpfError> {
    let region = base_reg
        .region
        .ok_or(BpfError::NonDereferenceableAccess { pc })?;
    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }
    // ND-0505 AC6: writes to read-only context are silently ignored;
    // the program continues execution. Bounds validation is still
    // performed so that out-of-range stores are caught.
    let addr = base_reg.value.wrapping_add_signed(off as i64);
    let end = addr
        .checked_add(N as u64)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: N })?;
    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: N });
    }
    if matches!(region.tag, RegionTag::Context) {
        return Ok(());
    }
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

/// Execute a 32-bit atomic operation at `[base_reg + off]`.
///
/// When the target region is read-only context, bounds validation and
/// FETCH/CMPXCHG register-result semantics are preserved but the actual
/// write to memory is suppressed (ND-0505 AC6).
#[inline]
fn mem_atomic32(
    base_reg: TaggedReg,
    off: i16,
    reg: &mut [TaggedReg; 11],
    src: usize,
    op: u32,
    pc: usize,
) -> Result<(), BpfError> {
    let region = base_reg
        .region
        .ok_or(BpfError::NonDereferenceableAccess { pc })?;
    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }
    let read_only = matches!(region.tag, RegionTag::Context);
    let addr = base_reg.value.wrapping_add_signed(off as i64);
    let end = addr
        .checked_add(4)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: 4 })?;
    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: 4 });
    }

    let ptr = addr as *mut u32;
    let fetch = (op & ebpf::BPF_ATOMIC_FETCH) != 0;
    let base_op = op & !ebpf::BPF_ATOMIC_FETCH;

    unsafe {
        let old = ptr.read_unaligned();
        match base_op {
            ebpf::BPF_ATOMIC_ADD => {
                if !read_only {
                    ptr.write_unaligned(old.wrapping_add(reg[src].value as u32));
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old as u64);
                }
            }
            ebpf::BPF_ATOMIC_OR => {
                if !read_only {
                    ptr.write_unaligned(old | reg[src].value as u32);
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old as u64);
                }
            }
            ebpf::BPF_ATOMIC_AND => {
                if !read_only {
                    ptr.write_unaligned(old & reg[src].value as u32);
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old as u64);
                }
            }
            ebpf::BPF_ATOMIC_XOR => {
                if !read_only {
                    ptr.write_unaligned(old ^ reg[src].value as u32);
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old as u64);
                }
            }
            0xe0 => {
                // XCHG
                if !read_only {
                    ptr.write_unaligned(reg[src].value as u32);
                }
                reg[src] = TaggedReg::scalar(old as u64);
            }
            0xf0 => {
                // CMPXCHG
                if !read_only && old == reg[0].value as u32 {
                    ptr.write_unaligned(reg[src].value as u32);
                }
                reg[0] = TaggedReg::scalar(old as u64);
            }
            _ => {
                return Err(BpfError::UnknownOpcode {
                    pc,
                    opc: ebpf::ST_W_ATOMIC,
                });
            }
        }
    }
    Ok(())
}

/// Execute a 64-bit atomic operation at `[base_reg + off]`.
///
/// When the target region is read-only context, bounds validation and
/// FETCH/CMPXCHG register-result semantics are preserved but the actual
/// write to memory is suppressed (ND-0505 AC6).
#[inline]
fn mem_atomic64(
    base_reg: TaggedReg,
    off: i16,
    reg: &mut [TaggedReg; 11],
    src: usize,
    op: u32,
    pc: usize,
) -> Result<(), BpfError> {
    let region = base_reg
        .region
        .ok_or(BpfError::NonDereferenceableAccess { pc })?;
    if matches!(region.tag, RegionTag::MapDescriptor { .. }) {
        return Err(BpfError::NonDereferenceableAccess { pc });
    }
    let read_only = matches!(region.tag, RegionTag::Context);
    let addr = base_reg.value.wrapping_add_signed(off as i64);
    let end = addr
        .checked_add(8)
        .ok_or(BpfError::MemoryAccessViolation { pc, addr, len: 8 })?;
    if addr < region.base || end > region.end {
        return Err(BpfError::MemoryAccessViolation { pc, addr, len: 8 });
    }

    let ptr = addr as *mut u64;
    let fetch = (op & ebpf::BPF_ATOMIC_FETCH) != 0;
    let base_op = op & !ebpf::BPF_ATOMIC_FETCH;

    unsafe {
        let old = ptr.read_unaligned();
        match base_op {
            ebpf::BPF_ATOMIC_ADD => {
                if !read_only {
                    ptr.write_unaligned(old.wrapping_add(reg[src].value));
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old);
                }
            }
            ebpf::BPF_ATOMIC_OR => {
                if !read_only {
                    ptr.write_unaligned(old | reg[src].value);
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old);
                }
            }
            ebpf::BPF_ATOMIC_AND => {
                if !read_only {
                    ptr.write_unaligned(old & reg[src].value);
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old);
                }
            }
            ebpf::BPF_ATOMIC_XOR => {
                if !read_only {
                    ptr.write_unaligned(old ^ reg[src].value);
                }
                if fetch {
                    reg[src] = TaggedReg::scalar(old);
                }
            }
            0xe0 => {
                // XCHG
                if !read_only {
                    ptr.write_unaligned(reg[src].value);
                }
                reg[src] = TaggedReg::scalar(old);
            }
            0xf0 => {
                // CMPXCHG
                if !read_only && old == reg[0].value {
                    ptr.write_unaligned(reg[src].value);
                }
                reg[0] = TaggedReg::scalar(old);
            }
            _ => {
                return Err(BpfError::UnknownOpcode {
                    pc,
                    opc: ebpf::ST_DW_ATOMIC,
                });
            }
        }
    }
    Ok(())
}

/// Sentinel value for `instruction_budget` that disables metering.
///
/// Pass this to [`execute_program`] or [`execute_program_no_maps`] when you
/// do not want to impose a limit on the number of instructions executed.
pub const UNLIMITED_BUDGET: u64 = u64::MAX;

/// Execute a BPF program without map access.
///
/// This is a safe wrapper around [`execute_program`] for programs that do
/// not use BPF maps.  Since `maps` is empty, the safety invariants on
/// [`MapRegion`] are trivially satisfied.
///
/// Pass [`UNLIMITED_BUDGET`] as `instruction_budget` to disable metering.
pub fn execute_program_no_maps(
    prog: &[u8],
    ctx: &mut [u8],
    helpers: &[HelperDescriptor],
    read_only_ctx: bool,
    instruction_budget: u64,
) -> Result<u64, BpfError> {
    // SAFETY: maps is empty — no raw pointer invariants to uphold.
    unsafe { execute_program(prog, ctx, helpers, &[], read_only_ctx, instruction_budget) }
}

/// Execute a BPF program.
///
/// # Arguments
/// * `prog` — the BPF bytecode (must be a multiple of 8 bytes).
/// * `ctx`  — memory region accessible to the program (r1 points here,
///   length in r2).
/// * `helpers` — table of helper function descriptors.
/// * `maps` — table of map region descriptors.
/// * `read_only_ctx` — when `true`, writes to the context region are
///   silently ignored (ND-0505 AC6).  When `false`, the region is writable.
/// * `instruction_budget` — maximum number of instruction slots that may be
///   executed before the program is terminated with
///   [`BpfError::InstructionBudgetExceeded`].  Pass [`UNLIMITED_BUDGET`] to
///   disable metering.
///
/// # Returns
/// The value of `r0` when the program exits.
///
/// # Safety
/// Each [`MapRegion`] in `maps` must satisfy:
/// - `relocated_ptr` is the address of a valid, live allocation.
/// - `data_start..data_end` covers the full backing storage of the map
///   and remains valid for the duration of this call.
/// - The map storage must not alias `ctx` or the interpreter's internal
///   BPF stack.
///
/// Violating these invariants may cause undefined behavior, because the
/// interpreter dereferences addresses within the declared map bounds via
/// raw pointers.
///
/// If `maps` is empty (no map access), these requirements are trivially
/// satisfied and the call is safe.
///
/// # Zero-allocation guarantee
/// All interpreter state (registers, call stack, BPF stack) lives on the
/// Rust call stack. No `Vec`, `Box`, or heap allocation occurs.
#[allow(clippy::manual_checked_ops)] // BPF division-by-zero returns 0 per RFC 9669 §5.2
pub unsafe fn execute_program(
    prog: &[u8],
    ctx: &mut [u8],
    helpers: &[HelperDescriptor],
    maps: &[MapRegion],
    read_only_ctx: bool,
    instruction_budget: u64,
) -> Result<u64, BpfError> {
    let num_insns = prog.len() / INSN_SIZE;
    if !prog.len().is_multiple_of(INSN_SIZE) {
        return Err(BpfError::OutOfBounds { pc: num_insns });
    }

    // BPF stack — lives on the Rust stack. Mutated via raw pointers in
    // store and atomic instructions.
    #[allow(unused_mut)]
    let mut stack = [0u8; STACK_SIZE];

    // Call frames for BPF-to-BPF calls.
    let mut call_frames: [CallFrame; MAX_CALL_DEPTH] = [CallFrame::zeroed(); MAX_CALL_DEPTH];
    let mut frame_idx: usize = 0;

    // Spill tracker for pointer register save/restore through the stack.
    let mut spill_tracker = SpillTracker::new();

    // Stack region bounds.
    let stack_base = stack.as_ptr() as u64;
    let stack_end = stack_base
        .checked_add(STACK_SIZE as u64)
        .expect("stack overflow");

    // Context region bounds.
    let ctx_base = ctx.as_ptr() as u64;
    let ctx_end = ctx_base
        .checked_add(ctx.len() as u64)
        .expect("ctx overflow");

    // Registers r0-r10.
    let mut reg: [TaggedReg; 11] = [TaggedReg::zeroed(); 11];
    // r1 = pointer to context, r2 = length of context.
    let ctx_tag = if read_only_ctx {
        RegionTag::Context
    } else {
        RegionTag::Memory
    };
    reg[1] = TaggedReg {
        value: ctx_base,
        region: if ctx.is_empty() {
            None
        } else {
            Some(Region {
                tag: ctx_tag,
                base: ctx_base,
                end: ctx_end,
            })
        },
    };
    reg[2] = TaggedReg::scalar(ctx.len() as u64);
    // r10 = frame pointer (top of stack).
    reg[10] = TaggedReg {
        value: stack_base + STACK_SIZE as u64,
        region: Some(Region {
            tag: RegionTag::Stack,
            base: stack_base,
            end: stack_end,
        }),
    };

    let mut pc: usize = 0;
    let mut insn_count: u64 = 0;

    while pc < num_insns {
        insn_count += 1;
        if insn_count > instruction_budget {
            return Err(BpfError::InstructionBudgetExceeded { pc });
        }
        let insn = ebpf::get_insn(prog, pc);
        pc += 1;

        let dst = insn.dst as usize;
        let src = insn.src as usize;

        if dst > 10 || src > 10 {
            return Err(BpfError::UnknownOpcode {
                pc: pc - 1,
                opc: insn.opc,
            });
        }

        match insn.opc {
            // ── LD_DW_IMM (128-bit wide instruction) ────────────────
            ebpf::LD_DW_IMM => {
                if pc >= num_insns {
                    return Err(BpfError::OutOfBounds { pc: pc - 1 });
                }
                let next = ebpf::get_insn(prog, pc);
                pc += 1;
                // LD_DW_IMM occupies two 8-byte slots; charge the second slot
                // so the budget accurately reflects the number of slots consumed.
                insn_count += 1;
                if insn_count > instruction_budget {
                    return Err(BpfError::InstructionBudgetExceeded { pc: pc - 1 });
                }

                match src as u8 {
                    0 => {
                        // Plain 64-bit immediate
                        let val = (insn.imm as u32 as u64) | ((next.imm as u64) << 32);
                        reg[dst] = TaggedReg::scalar(val);
                    }
                    1 => {
                        // Map descriptor relocation
                        if insn.imm < 0 {
                            return Err(BpfError::InvalidMapIndex {
                                pc: pc - 2,
                                index: insn.imm,
                            });
                        }
                        let idx = insn.imm as u32 as usize;
                        if idx >= maps.len() {
                            return Err(BpfError::InvalidMapIndex {
                                pc: pc - 2,
                                index: insn.imm,
                            });
                        }
                        reg[dst] = TaggedReg {
                            value: maps[idx].relocated_ptr,
                            region: Some(Region {
                                tag: RegionTag::MapDescriptor {
                                    map_index: idx as u32,
                                },
                                base: 0,
                                end: 0,
                            }),
                        };
                    }
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 2,
                            opc: insn.opc,
                        });
                    }
                }
            }

            // ── LDX MEM ─────────────────────────────────────────────
            ebpf::LD_B_REG => {
                reg[dst] = TaggedReg::scalar(mem_load::<1>(&reg[src], insn.off, pc - 1)?);
            }
            ebpf::LD_H_REG => {
                reg[dst] = TaggedReg::scalar(mem_load::<2>(&reg[src], insn.off, pc - 1)?);
            }
            ebpf::LD_W_REG => {
                reg[dst] = TaggedReg::scalar(mem_load::<4>(&reg[src], insn.off, pc - 1)?);
            }
            ebpf::LD_DW_REG => {
                let val = mem_load::<8>(&reg[src], insn.off, pc - 1)?;
                if matches!(
                    reg[src].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[src].value.wrapping_add_signed(insn.off as i64);
                    if let Some(spilled) = spill_tracker.check_restore(stack_base, addr) {
                        reg[dst] = TaggedReg {
                            value: val,
                            region: Some(spilled),
                        };
                    } else {
                        reg[dst] = TaggedReg::scalar(val);
                    }
                } else {
                    reg[dst] = TaggedReg::scalar(val);
                }
            }

            // ── LDXSX (sign-extension loads, RFC 9669 §5.2) ────────
            ebpf::LDSX_B_REG => {
                reg[dst] =
                    TaggedReg::scalar(mem_load_sign_extend::<1>(&reg[src], insn.off, pc - 1)?);
            }
            ebpf::LDSX_H_REG => {
                reg[dst] =
                    TaggedReg::scalar(mem_load_sign_extend::<2>(&reg[src], insn.off, pc - 1)?);
            }
            ebpf::LDSX_W_REG => {
                reg[dst] =
                    TaggedReg::scalar(mem_load_sign_extend::<4>(&reg[src], insn.off, pc - 1)?);
            }

            // ── ST IMM (store immediate to memory) ──────────────────
            ebpf::ST_B_IMM => {
                mem_store::<1>(&reg[dst], insn.off, insn.imm as u64, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 1);
                }
            }
            ebpf::ST_H_IMM => {
                mem_store::<2>(&reg[dst], insn.off, insn.imm as u64, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 2);
                }
            }
            ebpf::ST_W_IMM => {
                mem_store::<4>(&reg[dst], insn.off, insn.imm as u64, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 4);
                }
            }
            ebpf::ST_DW_IMM => {
                mem_store::<8>(&reg[dst], insn.off, insn.imm as i64 as u64, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 8);
                }
            }

            // ── STX REG (store register to memory) ──────────────────
            ebpf::ST_B_REG => {
                mem_store::<1>(&reg[dst], insn.off, reg[src].value, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 1);
                }
            }
            ebpf::ST_H_REG => {
                mem_store::<2>(&reg[dst], insn.off, reg[src].value, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 2);
                }
            }
            ebpf::ST_W_REG => {
                mem_store::<4>(&reg[dst], insn.off, reg[src].value, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 4);
                }
            }
            ebpf::ST_DW_REG => {
                mem_store::<8>(&reg[dst], insn.off, reg[src].value, pc - 1)?;
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = reg[dst].value.wrapping_add_signed(insn.off as i64);
                    if let Some(region) = reg[src].region {
                        if (addr.wrapping_sub(stack_base)).is_multiple_of(8) {
                            spill_tracker.record_spill(stack_base, addr, region);
                        } else {
                            // Unaligned pointer store — invalidate overlapping slots
                            // to prevent stale spill records.
                            spill_tracker.invalidate(stack_base, addr, 8);
                        }
                    } else {
                        spill_tracker.invalidate(stack_base, addr, 8);
                    }
                }
            }

            // ── Atomic operations (RFC 9669 §5.3) ───────────────────
            ebpf::ST_W_ATOMIC => {
                let base = reg[dst];
                mem_atomic32(base, insn.off, &mut reg, src, insn.imm as u32, pc - 1)?;
                if matches!(
                    base.region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = base.value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 4);
                }
            }
            ebpf::ST_DW_ATOMIC => {
                let base = reg[dst];
                mem_atomic64(base, insn.off, &mut reg, src, insn.imm as u32, pc - 1)?;
                if matches!(
                    base.region,
                    Some(Region {
                        tag: RegionTag::Stack,
                        ..
                    })
                ) {
                    let addr = base.value.wrapping_add_signed(insn.off as i64);
                    spill_tracker.invalidate(stack_base, addr, 8);
                }
            }

            // ── ALU32 ───────────────────────────────────────────────
            ebpf::ADD32_IMM => {
                reg[dst] =
                    TaggedReg::scalar((reg[dst].value as u32).wrapping_add(insn.imm as u32) as u64);
            }
            ebpf::ADD32_REG => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_add(reg[src].value as u32) as u64,
                );
            }
            ebpf::SUB32_IMM => {
                reg[dst] =
                    TaggedReg::scalar((reg[dst].value as u32).wrapping_sub(insn.imm as u32) as u64);
            }
            ebpf::SUB32_REG => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_sub(reg[src].value as u32) as u64,
                );
            }
            ebpf::MUL32_IMM => {
                reg[dst] =
                    TaggedReg::scalar((reg[dst].value as u32).wrapping_mul(insn.imm as u32) as u64);
            }
            ebpf::MUL32_REG => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_mul(reg[src].value as u32) as u64,
                );
            }
            ebpf::DIV32_IMM => {
                let imm = insn.imm as u32;
                if insn.off == 0 {
                    reg[dst] = TaggedReg::scalar(if imm == 0 {
                        0
                    } else {
                        (reg[dst].value as u32 / imm) as u64
                    });
                } else {
                    let s = insn.imm;
                    reg[dst] = TaggedReg::scalar(if s == 0 {
                        0
                    } else {
                        ((reg[dst].value as i32).wrapping_div(s) as u32) as u64
                    });
                }
            }
            ebpf::DIV32_REG => {
                if insn.off == 0 {
                    let s = reg[src].value as u32;
                    reg[dst] = TaggedReg::scalar(if s == 0 {
                        0
                    } else {
                        (reg[dst].value as u32 / s) as u64
                    });
                } else {
                    let s = reg[src].value as i32;
                    reg[dst] = TaggedReg::scalar(if s == 0 {
                        0
                    } else {
                        ((reg[dst].value as i32).wrapping_div(s) as u32) as u64
                    });
                }
            }
            ebpf::OR32_IMM => {
                reg[dst] = TaggedReg::scalar((reg[dst].value as u32 | insn.imm as u32) as u64);
            }
            ebpf::OR32_REG => {
                reg[dst] =
                    TaggedReg::scalar((reg[dst].value as u32 | reg[src].value as u32) as u64);
            }
            ebpf::AND32_IMM => {
                reg[dst] = TaggedReg::scalar((reg[dst].value as u32 & insn.imm as u32) as u64);
            }
            ebpf::AND32_REG => {
                reg[dst] =
                    TaggedReg::scalar((reg[dst].value as u32 & reg[src].value as u32) as u64);
            }
            ebpf::LSH32_IMM => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_shl(insn.imm as u32 & 0x1f) as u64,
                );
            }
            ebpf::LSH32_REG => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_shl(reg[src].value as u32 & 0x1f) as u64,
                );
            }
            ebpf::RSH32_IMM => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_shr(insn.imm as u32 & 0x1f) as u64,
                );
            }
            ebpf::RSH32_REG => {
                reg[dst] = TaggedReg::scalar(
                    (reg[dst].value as u32).wrapping_shr(reg[src].value as u32 & 0x1f) as u64,
                );
            }
            ebpf::NEG32 => {
                reg[dst] = TaggedReg::scalar((reg[dst].value as i32).wrapping_neg() as u32 as u64);
            }
            ebpf::MOD32_IMM => {
                let imm = insn.imm as u32;
                let new_val = if insn.off == 0 {
                    if imm != 0 {
                        (reg[dst].value as u32 % imm) as u64
                    } else {
                        reg[dst].value & 0xffff_ffff
                    }
                } else {
                    let s = insn.imm;
                    if s != 0 {
                        ((reg[dst].value as i32).wrapping_rem(s) as u32) as u64
                    } else {
                        reg[dst].value & 0xffff_ffff
                    }
                };
                reg[dst] = TaggedReg::scalar(new_val);
            }
            ebpf::MOD32_REG => {
                let new_val = if insn.off == 0 {
                    let s = reg[src].value as u32;
                    if s != 0 {
                        (reg[dst].value as u32 % s) as u64
                    } else {
                        reg[dst].value & 0xffff_ffff
                    }
                } else {
                    let s = reg[src].value as i32;
                    if s != 0 {
                        ((reg[dst].value as i32).wrapping_rem(s) as u32) as u64
                    } else {
                        reg[dst].value & 0xffff_ffff
                    }
                };
                reg[dst] = TaggedReg::scalar(new_val);
            }
            ebpf::XOR32_IMM => {
                reg[dst] = TaggedReg::scalar((reg[dst].value as u32 ^ insn.imm as u32) as u64);
            }
            ebpf::XOR32_REG => {
                reg[dst] =
                    TaggedReg::scalar((reg[dst].value as u32 ^ reg[src].value as u32) as u64);
            }
            ebpf::MOV32_IMM => {
                if insn.off == 0 {
                    reg[dst] = TaggedReg::scalar(insn.imm as u32 as u64);
                } else {
                    return Err(BpfError::UnknownOpcode {
                        pc: pc - 1,
                        opc: insn.opc,
                    });
                }
            }
            ebpf::MOV32_REG => {
                if insn.off == 0 {
                    reg[dst] = TaggedReg::scalar(reg[src].value as u32 as u64);
                } else {
                    // MOVSX: sign extend 8/16-bit to 32, zero upper 32
                    reg[dst] = TaggedReg::scalar(match insn.off {
                        8 => (reg[src].value as i8 as i32 as u32) as u64,
                        16 => (reg[src].value as i16 as i32 as u32) as u64,
                        _ => {
                            return Err(BpfError::UnknownOpcode {
                                pc: pc - 1,
                                opc: insn.opc,
                            });
                        }
                    });
                }
            }
            ebpf::ARSH32_IMM => {
                reg[dst] = TaggedReg::scalar(
                    ((reg[dst].value as i32).wrapping_shr(insn.imm as u32 & 0x1f) as u32) as u64,
                );
            }
            ebpf::ARSH32_REG => {
                reg[dst] = TaggedReg::scalar(
                    ((reg[dst].value as i32).wrapping_shr(reg[src].value as u32 & 0x1f) as u32)
                        as u64,
                );
            }

            // ── Byte swap (ALU class END) ───────────────────────────
            ebpf::LE => {
                reg[dst] = TaggedReg::scalar(match insn.imm {
                    16 => (reg[dst].value as u16).to_le() as u64,
                    32 => (reg[dst].value as u32).to_le() as u64,
                    64 => reg[dst].value.to_le(),
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        })
                    }
                });
            }
            ebpf::BE => {
                reg[dst] = TaggedReg::scalar(match insn.imm {
                    16 => (reg[dst].value as u16).to_be() as u64,
                    32 => (reg[dst].value as u32).to_be() as u64,
                    64 => reg[dst].value.to_be(),
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        })
                    }
                });
            }
            ebpf::BSWAP => {
                reg[dst] = TaggedReg::scalar(match insn.imm {
                    16 => (reg[dst].value as u16).swap_bytes() as u64,
                    32 => (reg[dst].value as u32).swap_bytes() as u64,
                    64 => reg[dst].value.swap_bytes(),
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        })
                    }
                });
            }

            // ── ALU64 ───────────────────────────────────────────────
            ebpf::ADD64_IMM => {
                let new_val = reg[dst].value.wrapping_add(insn.imm as i64 as u64);
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::MapDescriptor { .. },
                        ..
                    })
                ) {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value = new_val;
            }
            ebpf::ADD64_REG => {
                let new_val = reg[dst].value.wrapping_add(reg[src].value);
                let new_region = match (reg[dst].region, reg[src].region) {
                    (Some(r), None) | (None, Some(r)) => {
                        if matches!(r.tag, RegionTag::MapDescriptor { .. }) {
                            return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                        }
                        Some(r)
                    }
                    (None, None) => None,
                    (Some(_), Some(_)) => {
                        return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 })
                    }
                };
                reg[dst] = TaggedReg {
                    value: new_val,
                    region: new_region,
                };
            }
            ebpf::SUB64_IMM => {
                let new_val = reg[dst].value.wrapping_sub(insn.imm as i64 as u64);
                if matches!(
                    reg[dst].region,
                    Some(Region {
                        tag: RegionTag::MapDescriptor { .. },
                        ..
                    })
                ) {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value = new_val;
            }
            ebpf::SUB64_REG => {
                let new_val = reg[dst].value.wrapping_sub(reg[src].value);
                let new_region = match (&reg[dst].region, &reg[src].region) {
                    (Some(d), Some(s)) => {
                        if d == s {
                            None
                        } else {
                            return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                        }
                    }
                    (Some(r), None) => {
                        if matches!(r.tag, RegionTag::MapDescriptor { .. }) {
                            return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                        }
                        Some(*r)
                    }
                    (None, Some(_)) => {
                        return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 })
                    }
                    (None, None) => None,
                };
                reg[dst] = TaggedReg {
                    value: new_val,
                    region: new_region,
                };
            }
            ebpf::MUL64_IMM => {
                reg[dst] = TaggedReg::scalar(reg[dst].value.wrapping_mul(insn.imm as i64 as u64));
            }
            ebpf::MUL64_REG => {
                reg[dst] = TaggedReg::scalar(reg[dst].value.wrapping_mul(reg[src].value));
            }
            ebpf::DIV64_IMM => {
                if insn.off == 0 {
                    let imm = insn.imm as i64 as u64;
                    reg[dst] = TaggedReg::scalar(if imm == 0 { 0 } else { reg[dst].value / imm });
                } else {
                    let imm = insn.imm as i64;
                    reg[dst] = TaggedReg::scalar(if imm == 0 {
                        0
                    } else {
                        (reg[dst].value as i64).wrapping_div(imm) as u64
                    });
                }
            }
            ebpf::DIV64_REG => {
                if insn.off == 0 {
                    reg[dst] = TaggedReg::scalar(if reg[src].value == 0 {
                        0
                    } else {
                        reg[dst].value / reg[src].value
                    });
                } else {
                    let s = reg[src].value as i64;
                    reg[dst] = TaggedReg::scalar(if s == 0 {
                        0
                    } else {
                        (reg[dst].value as i64).wrapping_div(s) as u64
                    });
                }
            }
            ebpf::OR64_IMM => {
                if reg[dst].region.is_some() {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value |= insn.imm as i64 as u64;
            }
            ebpf::OR64_REG => {
                if reg[dst].region.is_some() || reg[src].region.is_some() {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value |= reg[src].value;
            }
            ebpf::AND64_IMM => {
                if reg[dst].region.is_some() {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value &= insn.imm as i64 as u64;
            }
            ebpf::AND64_REG => {
                if reg[dst].region.is_some() || reg[src].region.is_some() {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value &= reg[src].value;
            }
            ebpf::LSH64_IMM => {
                reg[dst] = TaggedReg::scalar(reg[dst].value.wrapping_shl((insn.imm as u32) & 0x3f));
            }
            ebpf::LSH64_REG => {
                reg[dst] =
                    TaggedReg::scalar(reg[dst].value.wrapping_shl((reg[src].value as u32) & 0x3f));
            }
            ebpf::RSH64_IMM => {
                reg[dst] = TaggedReg::scalar(reg[dst].value.wrapping_shr((insn.imm as u32) & 0x3f));
            }
            ebpf::RSH64_REG => {
                reg[dst] =
                    TaggedReg::scalar(reg[dst].value.wrapping_shr((reg[src].value as u32) & 0x3f));
            }
            ebpf::NEG64 => {
                reg[dst] = TaggedReg::scalar((reg[dst].value as i64).wrapping_neg() as u64);
            }
            ebpf::MOD64_IMM => {
                let new_val = if insn.off == 0 {
                    let imm = insn.imm as i64 as u64;
                    if imm != 0 {
                        reg[dst].value % imm
                    } else {
                        reg[dst].value
                    }
                } else {
                    let s = insn.imm as i64;
                    if s != 0 {
                        (reg[dst].value as i64).wrapping_rem(s) as u64
                    } else {
                        reg[dst].value
                    }
                };
                reg[dst] = TaggedReg::scalar(new_val);
            }
            ebpf::MOD64_REG => {
                let new_val = if insn.off == 0 {
                    if reg[src].value != 0 {
                        reg[dst].value % reg[src].value
                    } else {
                        reg[dst].value
                    }
                } else {
                    let s = reg[src].value as i64;
                    if s != 0 {
                        (reg[dst].value as i64).wrapping_rem(s) as u64
                    } else {
                        reg[dst].value
                    }
                };
                reg[dst] = TaggedReg::scalar(new_val);
            }
            ebpf::XOR64_IMM => {
                if reg[dst].region.is_some() {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value ^= insn.imm as i64 as u64;
            }
            ebpf::XOR64_REG => {
                if reg[dst].region.is_some() || reg[src].region.is_some() {
                    return Err(BpfError::InvalidPointerArithmetic { pc: pc - 1 });
                }
                reg[dst].value ^= reg[src].value;
            }
            ebpf::MOV64_IMM => {
                reg[dst] = TaggedReg::scalar(insn.imm as i64 as u64);
            }
            ebpf::MOV64_REG => {
                if insn.off == 0 {
                    reg[dst] = reg[src];
                } else {
                    // MOVSX: sign extend 8/16/32 to 64
                    reg[dst] = TaggedReg::scalar(match insn.off {
                        8 => reg[src].value as i8 as i64 as u64,
                        16 => reg[src].value as i16 as i64 as u64,
                        32 => reg[src].value as i32 as i64 as u64,
                        _ => {
                            return Err(BpfError::UnknownOpcode {
                                pc: pc - 1,
                                opc: insn.opc,
                            });
                        }
                    });
                }
            }
            ebpf::ARSH64_IMM => {
                reg[dst] = TaggedReg::scalar(
                    ((reg[dst].value as i64).wrapping_shr((insn.imm as u32) & 0x3f)) as u64,
                );
            }
            ebpf::ARSH64_REG => {
                reg[dst] = TaggedReg::scalar(
                    ((reg[dst].value as i64).wrapping_shr((reg[src].value as u32) & 0x3f)) as u64,
                );
            }

            // ── JMP (64-bit operands) ───────────────────────────────
            ebpf::JA => {
                pc = check_jump(pc, insn.off as isize, num_insns)?;
            }
            ebpf::JEQ_IMM => {
                if reg[dst].value == (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JEQ_REG => {
                if reg[dst].value == reg[src].value {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_IMM => {
                if reg[dst].value > (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_REG => {
                if reg[dst].value > reg[src].value {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_IMM => {
                if reg[dst].value >= (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_REG => {
                if reg[dst].value >= reg[src].value {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_IMM => {
                if reg[dst].value < (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_REG => {
                if reg[dst].value < reg[src].value {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_IMM => {
                if reg[dst].value <= (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_REG => {
                if reg[dst].value <= reg[src].value {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_IMM => {
                if reg[dst].value & (insn.imm as i64 as u64) != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_REG => {
                if reg[dst].value & reg[src].value != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_IMM => {
                if reg[dst].value != (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_REG => {
                if reg[dst].value != reg[src].value {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_IMM => {
                if (reg[dst].value as i64) > (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_REG => {
                if (reg[dst].value as i64) > (reg[src].value as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_IMM => {
                if (reg[dst].value as i64) >= (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_REG => {
                if (reg[dst].value as i64) >= (reg[src].value as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_IMM => {
                if (reg[dst].value as i64) < (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_REG => {
                if (reg[dst].value as i64) < (reg[src].value as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_IMM => {
                if (reg[dst].value as i64) <= (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_REG => {
                if (reg[dst].value as i64) <= (reg[src].value as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }

            // ── JMP32 (32-bit operands) ─────────────────────────────
            ebpf::JA32 => {
                pc = check_jump(pc, insn.imm as isize, num_insns)?;
            }
            ebpf::JEQ_IMM32 => {
                if (reg[dst].value as u32) == (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JEQ_REG32 => {
                if (reg[dst].value as u32) == (reg[src].value as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_IMM32 => {
                if (reg[dst].value as u32) > (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_REG32 => {
                if (reg[dst].value as u32) > (reg[src].value as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_IMM32 => {
                if (reg[dst].value as u32) >= (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_REG32 => {
                if (reg[dst].value as u32) >= (reg[src].value as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_IMM32 => {
                if (reg[dst].value as u32) < (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_REG32 => {
                if (reg[dst].value as u32) < (reg[src].value as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_IMM32 => {
                if (reg[dst].value as u32) <= (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_REG32 => {
                if (reg[dst].value as u32) <= (reg[src].value as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_IMM32 => {
                if (reg[dst].value as u32) & (insn.imm as u32) != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_REG32 => {
                if (reg[dst].value as u32) & (reg[src].value as u32) != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_IMM32 => {
                if (reg[dst].value as u32) != (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_REG32 => {
                if (reg[dst].value as u32) != (reg[src].value as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_IMM32 => {
                if (reg[dst].value as i32) > (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_REG32 => {
                if (reg[dst].value as i32) > (reg[src].value as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_IMM32 => {
                if (reg[dst].value as i32) >= (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_REG32 => {
                if (reg[dst].value as i32) >= (reg[src].value as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_IMM32 => {
                if (reg[dst].value as i32) < (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_REG32 => {
                if (reg[dst].value as i32) < (reg[src].value as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_IMM32 => {
                if (reg[dst].value as i32) <= (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_REG32 => {
                if (reg[dst].value as i32) <= (reg[src].value as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }

            // ── CALL / EXIT ─────────────────────────────────────────
            ebpf::CALL => {
                match src as u8 {
                    // Helper function call
                    0 | 2 => {
                        let id = insn.imm as u32;
                        if let Some(desc) = helpers.iter().find(|d| d.id == id) {
                            let result = (desc.func)(
                                reg[1].value,
                                reg[2].value,
                                reg[3].value,
                                reg[4].value,
                                reg[5].value,
                            );

                            match desc.ret {
                                HelperReturn::Scalar => {
                                    reg[0] = TaggedReg::scalar(result);
                                }
                                HelperReturn::MapValueOrNull { map_arg } => {
                                    if !(1..=5).contains(&map_arg) {
                                        return Err(BpfError::InvalidHelperArgument {
                                            pc: pc - 1,
                                            arg: map_arg,
                                        });
                                    }
                                    if result == 0 {
                                        reg[0] = TaggedReg::scalar(0);
                                    } else {
                                        let map_index = match reg[map_arg as usize].region {
                                            Some(Region {
                                                tag: RegionTag::MapDescriptor { map_index },
                                                ..
                                            }) => map_index,
                                            _ => {
                                                return Err(BpfError::InvalidHelperArgument {
                                                    pc: pc - 1,
                                                    arg: map_arg,
                                                })
                                            }
                                        };
                                        let map = &maps[map_index as usize];
                                        let vs = map.value_size as u64;
                                        let end = result.checked_add(vs).ok_or(
                                            BpfError::MemoryAccessViolation {
                                                pc: pc - 1,
                                                addr: result,
                                                len: vs as usize,
                                            },
                                        )?;
                                        if result < map.data_start || end > map.data_end {
                                            return Err(BpfError::MemoryAccessViolation {
                                                pc: pc - 1,
                                                addr: result,
                                                len: vs as usize,
                                            });
                                        }
                                        reg[0] = TaggedReg {
                                            value: result,
                                            region: Some(Region {
                                                tag: RegionTag::MapValue {
                                                    value_size: map.value_size,
                                                },
                                                base: result,
                                                end,
                                            }),
                                        };
                                    }
                                }
                            }

                            // Clobber R1-R5 tags (values left as-is).
                            for r in &mut reg[1..=5] {
                                r.region = None;
                            }
                        } else {
                            return Err(BpfError::UnknownHelper { pc: pc - 1, id });
                        }
                    }
                    // BPF-to-BPF (local) call
                    1 => {
                        if frame_idx >= MAX_CALL_DEPTH {
                            return Err(BpfError::CallDepthExceeded { pc: pc - 1 });
                        }
                        for i in 0..4 {
                            call_frames[frame_idx].saved_regs[i] = reg[6 + i].value;
                            call_frames[frame_idx].saved_regions[i] = reg[6 + i].region;
                        }
                        call_frames[frame_idx].return_pc = pc;
                        let frame_size = STACK_SIZE_PER_FRAME as u64;
                        call_frames[frame_idx].frame_size = frame_size;
                        reg[10].value -= frame_size;
                        frame_idx += 1;
                        pc = check_jump(pc, insn.imm as isize, num_insns)?;
                    }
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        });
                    }
                }
            }
            ebpf::EXIT => {
                if frame_idx > 0 {
                    frame_idx -= 1;
                    for i in 0..4 {
                        reg[6 + i] = TaggedReg {
                            value: call_frames[frame_idx].saved_regs[i],
                            region: call_frames[frame_idx].saved_regions[i],
                        };
                    }
                    pc = call_frames[frame_idx].return_pc;
                    reg[10].value += call_frames[frame_idx].frame_size;
                } else {
                    return Ok(reg[0].value);
                }
            }

            _ => {
                return Err(BpfError::UnknownOpcode {
                    pc: pc - 1,
                    opc: insn.opc,
                });
            }
        }
    }

    // If we fall off the end without an EXIT, that's an error.
    Err(BpfError::OutOfBounds { pc })
}
