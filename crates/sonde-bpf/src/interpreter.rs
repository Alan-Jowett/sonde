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
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BpfError {}

/// Saved state for a BPF-to-BPF call frame (no heap allocation).
#[derive(Clone, Copy)]
struct CallFrame {
    /// Saved callee-saved registers r6–r9.
    saved_regs: [u64; 4],
    /// Return address (instruction index).
    return_pc: usize,
    /// Frame pointer adjustment applied.
    frame_size: u64,
}

impl CallFrame {
    const fn zeroed() -> Self {
        Self {
            saved_regs: [0; 4],
            return_pc: 0,
            frame_size: 0,
        }
    }
}

/// Check that `[addr, addr+len)` falls within one of the allowed memory regions.
///
/// Checks the primary `mem` slice, the BPF `stack`, and any `extra_regions`
/// (e.g. map backing stores whose pointers are returned by helper functions).
#[inline]
fn check_mem(
    addr: u64,
    len: usize,
    pc: usize,
    mem: &[u8],
    stack: &[u8],
    extra_regions: &[(*const u8, usize)],
) -> Result<(), BpfError> {
    if let Some(end) = addr.checked_add(len as u64) {
        let mem_start = mem.as_ptr() as u64;
        if let Some(mem_end) = mem_start.checked_add(mem.len() as u64) {
            if addr >= mem_start && end <= mem_end {
                return Ok(());
            }
        }
        let stack_start = stack.as_ptr() as u64;
        if let Some(stack_end) = stack_start.checked_add(stack.len() as u64) {
            if addr >= stack_start && end <= stack_end {
                return Ok(());
            }
        }
        for &(region_ptr, region_len) in extra_regions {
            let region_start = region_ptr as u64;
            if let Some(region_end) = region_start.checked_add(region_len as u64) {
                if addr >= region_start && end <= region_end {
                    return Ok(());
                }
            }
        }
    }
    Err(BpfError::MemoryAccessViolation { pc, addr, len })
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

/// Execute a BPF program.
///
/// # Arguments
/// * `prog` — the BPF bytecode (must be a multiple of 8 bytes).
/// * `mem`  — memory region accessible to the program (r1 points here, length in r2).
/// * `helpers` — table of helper functions keyed by helper id.
///   The table is a slice of `(id, fn)` pairs; a linear scan is fine for
///   the small number of helpers typically registered.
///
/// # Returns
/// The value of `r0` when the program exits.
///
/// # Zero-allocation guarantee
/// All interpreter state (registers, call stack, BPF stack) lives on the
/// Rust call stack. No `Vec`, `Box`, or heap allocation occurs.
pub fn execute_program(
    prog: &[u8],
    mem: &mut [u8],
    helpers: &[(u32, Helper)],
) -> Result<u64, BpfError> {
    execute_program_inner(prog, mem, helpers, &[])
}

/// Execute a BPF program with additional allowed memory regions.
///
/// Identical to [`execute_program`] but also permits memory accesses into
/// `extra_regions`. Each entry is a `(*const u8, usize)` pair representing
/// the base pointer and byte length of an allowed region (e.g. map backing
/// stores whose pointers are returned by helper functions such as
/// `map_lookup_elem`). The caller is responsible for ensuring the pointed-to
/// memory remains valid for the duration of the call.
///
/// # Safety
/// Each `(*const u8, usize)` in `extra_regions` must point to a valid,
/// live memory region of the given length. The interpreter uses them only
/// for bounds-checking inside `check_mem`; actual reads and writes are
/// performed via the same raw-pointer arithmetic used throughout the
/// interpreter loop in `execute_program_inner`.
pub fn execute_program_with_extra_mem(
    prog: &[u8],
    mem: &mut [u8],
    helpers: &[(u32, Helper)],
    extra_regions: &[(*const u8, usize)],
) -> Result<u64, BpfError> {
    execute_program_inner(prog, mem, helpers, extra_regions)
}

fn execute_program_inner(
    prog: &[u8],
    mem: &mut [u8],
    helpers: &[(u32, Helper)],
    extra_regions: &[(*const u8, usize)],
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

    // Registers r0–r10.
    let mut reg: [u64; 11] = [0; 11];
    // r1 = pointer to memory, r2 = length of memory.
    reg[1] = mem.as_ptr() as u64;
    reg[2] = mem.len() as u64;
    // r10 = frame pointer (top of stack).
    reg[10] = stack.as_ptr() as u64 + STACK_SIZE as u64;

    let mut pc: usize = 0;

    while pc < num_insns {
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
                reg[dst] = (insn.imm as u32 as u64) | ((next.imm as u64) << 32);
            }

            // ── LDX MEM ─────────────────────────────────────────────
            ebpf::LD_B_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 1, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { *(addr as *const u8) } as u64;
            }
            ebpf::LD_H_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 2, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { (addr as *const u16).read_unaligned() } as u64;
            }
            ebpf::LD_W_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 4, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { (addr as *const u32).read_unaligned() } as u64;
            }
            ebpf::LD_DW_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 8, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { (addr as *const u64).read_unaligned() };
            }

            // ── LDXSX (sign-extension loads, RFC 9669 §5.2) ────────
            ebpf::LDSX_B_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 1, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { *(addr as *const i8) } as i64 as u64;
            }
            ebpf::LDSX_H_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 2, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { (addr as *const i16).read_unaligned() } as i64 as u64;
            }
            ebpf::LDSX_W_REG => {
                let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 4, pc - 1, mem, &stack, extra_regions)?;
                reg[dst] = unsafe { (addr as *const i32).read_unaligned() } as i64 as u64;
            }

            // ── ST IMM (store immediate to memory) ──────────────────
            ebpf::ST_B_IMM => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 1, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    *(addr as *mut u8) = insn.imm as u8;
                }
            }
            ebpf::ST_H_IMM => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 2, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    (addr as *mut u16).write_unaligned(insn.imm as u16);
                }
            }
            ebpf::ST_W_IMM => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 4, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    (addr as *mut u32).write_unaligned(insn.imm as u32);
                }
            }
            ebpf::ST_DW_IMM => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 8, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    (addr as *mut u64).write_unaligned(insn.imm as i64 as u64);
                }
            }

            // ── STX REG (store register to memory) ──────────────────
            ebpf::ST_B_REG => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 1, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    *(addr as *mut u8) = reg[src] as u8;
                }
            }
            ebpf::ST_H_REG => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 2, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    (addr as *mut u16).write_unaligned(reg[src] as u16);
                }
            }
            ebpf::ST_W_REG => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 4, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    (addr as *mut u32).write_unaligned(reg[src] as u32);
                }
            }
            ebpf::ST_DW_REG => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 8, pc - 1, mem, &stack, extra_regions)?;
                unsafe {
                    (addr as *mut u64).write_unaligned(reg[src]);
                }
            }

            // ── Atomic operations (RFC 9669 §5.3) ───────────────────
            ebpf::ST_W_ATOMIC => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 4, pc - 1, mem, &stack, extra_regions)?;
                execute_atomic32(addr, &mut reg, src, insn.imm as u32, pc - 1)?;
            }
            ebpf::ST_DW_ATOMIC => {
                let addr = (reg[dst] as i64).wrapping_add(insn.off as i64) as u64;
                check_mem(addr, 8, pc - 1, mem, &stack, extra_regions)?;
                execute_atomic64(addr, &mut reg, src, insn.imm as u32, pc - 1)?;
            }

            // ── ALU32 ───────────────────────────────────────────────
            ebpf::ADD32_IMM => reg[dst] = (reg[dst] as u32).wrapping_add(insn.imm as u32) as u64,
            ebpf::ADD32_REG => reg[dst] = (reg[dst] as u32).wrapping_add(reg[src] as u32) as u64,
            ebpf::SUB32_IMM => reg[dst] = (reg[dst] as u32).wrapping_sub(insn.imm as u32) as u64,
            ebpf::SUB32_REG => reg[dst] = (reg[dst] as u32).wrapping_sub(reg[src] as u32) as u64,
            ebpf::MUL32_IMM => reg[dst] = (reg[dst] as u32).wrapping_mul(insn.imm as u32) as u64,
            ebpf::MUL32_REG => reg[dst] = (reg[dst] as u32).wrapping_mul(reg[src] as u32) as u64,
            ebpf::DIV32_IMM => {
                let imm = insn.imm as u32;
                // offset=0 → unsigned, offset=1 → signed (SDIV)
                if insn.off == 0 {
                    reg[dst] = if imm == 0 {
                        0
                    } else {
                        (reg[dst] as u32 / imm) as u64
                    };
                } else {
                    let s = insn.imm; // already i32
                    reg[dst] = if s == 0 {
                        0
                    } else {
                        ((reg[dst] as i32).wrapping_div(s) as u32) as u64
                    };
                }
            }
            ebpf::DIV32_REG => {
                if insn.off == 0 {
                    let s = reg[src] as u32;
                    reg[dst] = if s == 0 {
                        0
                    } else {
                        (reg[dst] as u32 / s) as u64
                    };
                } else {
                    let s = reg[src] as i32;
                    reg[dst] = if s == 0 {
                        0
                    } else {
                        ((reg[dst] as i32).wrapping_div(s) as u32) as u64
                    };
                }
            }
            ebpf::OR32_IMM => reg[dst] = (reg[dst] as u32 | insn.imm as u32) as u64,
            ebpf::OR32_REG => reg[dst] = (reg[dst] as u32 | reg[src] as u32) as u64,
            ebpf::AND32_IMM => reg[dst] = (reg[dst] as u32 & insn.imm as u32) as u64,
            ebpf::AND32_REG => reg[dst] = (reg[dst] as u32 & reg[src] as u32) as u64,
            ebpf::LSH32_IMM => {
                reg[dst] = (reg[dst] as u32).wrapping_shl(insn.imm as u32 & 0x1f) as u64
            }
            ebpf::LSH32_REG => {
                reg[dst] = (reg[dst] as u32).wrapping_shl(reg[src] as u32 & 0x1f) as u64
            }
            ebpf::RSH32_IMM => {
                reg[dst] = (reg[dst] as u32).wrapping_shr(insn.imm as u32 & 0x1f) as u64
            }
            ebpf::RSH32_REG => {
                reg[dst] = (reg[dst] as u32).wrapping_shr(reg[src] as u32 & 0x1f) as u64
            }
            ebpf::NEG32 => reg[dst] = (reg[dst] as i32).wrapping_neg() as u32 as u64,
            ebpf::MOD32_IMM => {
                let imm = insn.imm as u32;
                if insn.off == 0 {
                    if imm != 0 {
                        reg[dst] = (reg[dst] as u32 % imm) as u64;
                    } else {
                        reg[dst] &= 0xffff_ffff;
                    }
                } else {
                    let s = insn.imm; // i32
                    if s != 0 {
                        reg[dst] = ((reg[dst] as i32).wrapping_rem(s) as u32) as u64;
                    } else {
                        reg[dst] &= 0xffff_ffff;
                    }
                }
            }
            ebpf::MOD32_REG => {
                if insn.off == 0 {
                    let s = reg[src] as u32;
                    if s != 0 {
                        reg[dst] = (reg[dst] as u32 % s) as u64;
                    } else {
                        reg[dst] &= 0xffff_ffff;
                    }
                } else {
                    let s = reg[src] as i32;
                    if s != 0 {
                        reg[dst] = ((reg[dst] as i32).wrapping_rem(s) as u32) as u64;
                    } else {
                        reg[dst] &= 0xffff_ffff;
                    }
                }
            }
            ebpf::XOR32_IMM => reg[dst] = (reg[dst] as u32 ^ insn.imm as u32) as u64,
            ebpf::XOR32_REG => reg[dst] = (reg[dst] as u32 ^ reg[src] as u32) as u64,
            ebpf::MOV32_IMM => {
                if insn.off == 0 {
                    reg[dst] = insn.imm as u32 as u64;
                } else {
                    return Err(BpfError::UnknownOpcode {
                        pc: pc - 1,
                        opc: insn.opc,
                    });
                }
            }
            ebpf::MOV32_REG => {
                if insn.off == 0 {
                    reg[dst] = reg[src] as u32 as u64;
                } else {
                    // MOVSX: sign extend 8/16-bit to 32, zero upper 32
                    reg[dst] = match insn.off {
                        8 => (reg[src] as i8 as i32 as u32) as u64,
                        16 => (reg[src] as i16 as i32 as u32) as u64,
                        _ => {
                            return Err(BpfError::UnknownOpcode {
                                pc: pc - 1,
                                opc: insn.opc,
                            });
                        }
                    };
                }
            }
            ebpf::ARSH32_IMM => {
                reg[dst] = ((reg[dst] as i32).wrapping_shr(insn.imm as u32 & 0x1f) as u32) as u64;
            }
            ebpf::ARSH32_REG => {
                reg[dst] = ((reg[dst] as i32).wrapping_shr(reg[src] as u32 & 0x1f) as u32) as u64;
            }

            // ── Byte swap (ALU class END) ───────────────────────────
            ebpf::LE => {
                reg[dst] = match insn.imm {
                    16 => (reg[dst] as u16).to_le() as u64,
                    32 => (reg[dst] as u32).to_le() as u64,
                    64 => reg[dst].to_le(),
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        })
                    }
                };
            }
            ebpf::BE => {
                reg[dst] = match insn.imm {
                    16 => (reg[dst] as u16).to_be() as u64,
                    32 => (reg[dst] as u32).to_be() as u64,
                    64 => reg[dst].to_be(),
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        })
                    }
                };
            }
            ebpf::BSWAP => {
                reg[dst] = match insn.imm {
                    16 => (reg[dst] as u16).swap_bytes() as u64,
                    32 => (reg[dst] as u32).swap_bytes() as u64,
                    64 => reg[dst].swap_bytes(),
                    _ => {
                        return Err(BpfError::UnknownOpcode {
                            pc: pc - 1,
                            opc: insn.opc,
                        })
                    }
                };
            }

            // ── ALU64 ───────────────────────────────────────────────
            ebpf::ADD64_IMM => reg[dst] = reg[dst].wrapping_add(insn.imm as i64 as u64),
            ebpf::ADD64_REG => reg[dst] = reg[dst].wrapping_add(reg[src]),
            ebpf::SUB64_IMM => reg[dst] = reg[dst].wrapping_sub(insn.imm as i64 as u64),
            ebpf::SUB64_REG => reg[dst] = reg[dst].wrapping_sub(reg[src]),
            ebpf::MUL64_IMM => reg[dst] = reg[dst].wrapping_mul(insn.imm as i64 as u64),
            ebpf::MUL64_REG => reg[dst] = reg[dst].wrapping_mul(reg[src]),
            ebpf::DIV64_IMM => {
                if insn.off == 0 {
                    // Unsigned: imm is sign-extended to 64-bit then treated as unsigned.
                    let imm = insn.imm as i64 as u64;
                    reg[dst] = if imm == 0 { 0 } else { reg[dst] / imm };
                } else {
                    // SDIV
                    let imm = insn.imm as i64;
                    reg[dst] = if imm == 0 {
                        0
                    } else {
                        (reg[dst] as i64).wrapping_div(imm) as u64
                    };
                }
            }
            ebpf::DIV64_REG => {
                if insn.off == 0 {
                    reg[dst] = if reg[src] == 0 {
                        0
                    } else {
                        reg[dst] / reg[src]
                    };
                } else {
                    let s = reg[src] as i64;
                    reg[dst] = if s == 0 {
                        0
                    } else {
                        (reg[dst] as i64).wrapping_div(s) as u64
                    };
                }
            }
            ebpf::OR64_IMM => reg[dst] |= insn.imm as i64 as u64,
            ebpf::OR64_REG => reg[dst] |= reg[src],
            ebpf::AND64_IMM => reg[dst] &= insn.imm as i64 as u64,
            ebpf::AND64_REG => reg[dst] &= reg[src],
            ebpf::LSH64_IMM => reg[dst] = reg[dst].wrapping_shl((insn.imm as u32) & 0x3f),
            ebpf::LSH64_REG => reg[dst] = reg[dst].wrapping_shl((reg[src] as u32) & 0x3f),
            ebpf::RSH64_IMM => reg[dst] = reg[dst].wrapping_shr((insn.imm as u32) & 0x3f),
            ebpf::RSH64_REG => reg[dst] = reg[dst].wrapping_shr((reg[src] as u32) & 0x3f),
            ebpf::NEG64 => reg[dst] = (reg[dst] as i64).wrapping_neg() as u64,
            ebpf::MOD64_IMM => {
                if insn.off == 0 {
                    let imm = insn.imm as i64 as u64;
                    if imm != 0 {
                        reg[dst] %= imm;
                    }
                } else {
                    let s = insn.imm as i64;
                    if s != 0 {
                        reg[dst] = (reg[dst] as i64).wrapping_rem(s) as u64;
                    }
                }
            }
            ebpf::MOD64_REG => {
                if insn.off == 0 {
                    if reg[src] != 0 {
                        reg[dst] %= reg[src];
                    }
                } else {
                    let s = reg[src] as i64;
                    if s != 0 {
                        reg[dst] = (reg[dst] as i64).wrapping_rem(s) as u64;
                    }
                }
            }
            ebpf::XOR64_IMM => reg[dst] ^= insn.imm as i64 as u64,
            ebpf::XOR64_REG => reg[dst] ^= reg[src],
            ebpf::MOV64_IMM => {
                reg[dst] = insn.imm as i64 as u64;
            }
            ebpf::MOV64_REG => {
                if insn.off == 0 {
                    reg[dst] = reg[src];
                } else {
                    // MOVSX: sign extend 8/16/32 to 64
                    reg[dst] = match insn.off {
                        8 => reg[src] as i8 as i64 as u64,
                        16 => reg[src] as i16 as i64 as u64,
                        32 => reg[src] as i32 as i64 as u64,
                        _ => {
                            return Err(BpfError::UnknownOpcode {
                                pc: pc - 1,
                                opc: insn.opc,
                            });
                        }
                    };
                }
            }
            ebpf::ARSH64_IMM => {
                reg[dst] = ((reg[dst] as i64).wrapping_shr((insn.imm as u32) & 0x3f)) as u64;
            }
            ebpf::ARSH64_REG => {
                reg[dst] = ((reg[dst] as i64).wrapping_shr((reg[src] as u32) & 0x3f)) as u64;
            }

            // ── JMP (64-bit operands) ───────────────────────────────
            ebpf::JA => {
                pc = check_jump(pc, insn.off as isize, num_insns)?;
            }
            ebpf::JEQ_IMM => {
                if reg[dst] == (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JEQ_REG => {
                if reg[dst] == reg[src] {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_IMM => {
                if reg[dst] > (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_REG => {
                if reg[dst] > reg[src] {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_IMM => {
                if reg[dst] >= (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_REG => {
                if reg[dst] >= reg[src] {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_IMM => {
                if reg[dst] < (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_REG => {
                if reg[dst] < reg[src] {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_IMM => {
                if reg[dst] <= (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_REG => {
                if reg[dst] <= reg[src] {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_IMM => {
                if reg[dst] & (insn.imm as i64 as u64) != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_REG => {
                if reg[dst] & reg[src] != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_IMM => {
                if reg[dst] != (insn.imm as i64 as u64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_REG => {
                if reg[dst] != reg[src] {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_IMM => {
                if (reg[dst] as i64) > (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_REG => {
                if (reg[dst] as i64) > (reg[src] as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_IMM => {
                if (reg[dst] as i64) >= (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_REG => {
                if (reg[dst] as i64) >= (reg[src] as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_IMM => {
                if (reg[dst] as i64) < (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_REG => {
                if (reg[dst] as i64) < (reg[src] as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_IMM => {
                if (reg[dst] as i64) <= (insn.imm as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_REG => {
                if (reg[dst] as i64) <= (reg[src] as i64) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }

            // ── JMP32 (32-bit operands) ─────────────────────────────
            ebpf::JA32 => {
                pc = check_jump(pc, insn.imm as isize, num_insns)?;
            }
            ebpf::JEQ_IMM32 => {
                if (reg[dst] as u32) == (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JEQ_REG32 => {
                if (reg[dst] as u32) == (reg[src] as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_IMM32 => {
                if (reg[dst] as u32) > (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGT_REG32 => {
                if (reg[dst] as u32) > (reg[src] as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_IMM32 => {
                if (reg[dst] as u32) >= (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JGE_REG32 => {
                if (reg[dst] as u32) >= (reg[src] as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_IMM32 => {
                if (reg[dst] as u32) < (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLT_REG32 => {
                if (reg[dst] as u32) < (reg[src] as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_IMM32 => {
                if (reg[dst] as u32) <= (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JLE_REG32 => {
                if (reg[dst] as u32) <= (reg[src] as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_IMM32 => {
                if (reg[dst] as u32) & (insn.imm as u32) != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSET_REG32 => {
                if (reg[dst] as u32) & (reg[src] as u32) != 0 {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_IMM32 => {
                if (reg[dst] as u32) != (insn.imm as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JNE_REG32 => {
                if (reg[dst] as u32) != (reg[src] as u32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_IMM32 => {
                if (reg[dst] as i32) > (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGT_REG32 => {
                if (reg[dst] as i32) > (reg[src] as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_IMM32 => {
                if (reg[dst] as i32) >= (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSGE_REG32 => {
                if (reg[dst] as i32) >= (reg[src] as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_IMM32 => {
                if (reg[dst] as i32) < (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLT_REG32 => {
                if (reg[dst] as i32) < (reg[src] as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_IMM32 => {
                if (reg[dst] as i32) <= (insn.imm) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }
            ebpf::JSLE_REG32 => {
                if (reg[dst] as i32) <= (reg[src] as i32) {
                    pc = check_jump(pc, insn.off as isize, num_insns)?;
                }
            }

            // ── CALL / EXIT ─────────────────────────────────────────
            ebpf::CALL => {
                match src as u8 {
                    // Helper function call
                    0 | 2 => {
                        let id = insn.imm as u32;
                        if let Some((_, func)) = helpers.iter().find(|(k, _)| *k == id) {
                            reg[0] = func(reg[1], reg[2], reg[3], reg[4], reg[5]);
                        } else {
                            return Err(BpfError::UnknownHelper { pc: pc - 1, id });
                        }
                    }
                    // BPF-to-BPF (local) call
                    1 => {
                        if frame_idx >= MAX_CALL_DEPTH {
                            return Err(BpfError::CallDepthExceeded { pc: pc - 1 });
                        }
                        call_frames[frame_idx]
                            .saved_regs
                            .copy_from_slice(&reg[6..10]);
                        call_frames[frame_idx].return_pc = pc;
                        // Each frame gets STACK_SIZE_PER_FRAME bytes.
                        let frame_size = STACK_SIZE_PER_FRAME as u64;
                        call_frames[frame_idx].frame_size = frame_size;
                        reg[10] -= frame_size;
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
                    reg[6..10].copy_from_slice(&call_frames[frame_idx].saved_regs);
                    pc = call_frames[frame_idx].return_pc;
                    reg[10] += call_frames[frame_idx].frame_size;
                } else {
                    return Ok(reg[0]);
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

/// Execute a 32-bit atomic operation at `addr`.
#[inline]
fn execute_atomic32(
    addr: u64,
    reg: &mut [u64; 11],
    src: usize,
    op: u32,
    pc: usize,
) -> Result<(), BpfError> {
    let ptr = addr as *mut u32;
    let fetch = (op & ebpf::BPF_ATOMIC_FETCH) != 0;
    let base_op = op & !ebpf::BPF_ATOMIC_FETCH;

    unsafe {
        let old = ptr.read_unaligned();
        match base_op {
            ebpf::BPF_ATOMIC_ADD => {
                ptr.write_unaligned(old.wrapping_add(reg[src] as u32));
                if fetch {
                    reg[src] = old as u64;
                }
            }
            ebpf::BPF_ATOMIC_OR => {
                ptr.write_unaligned(old | reg[src] as u32);
                if fetch {
                    reg[src] = old as u64;
                }
            }
            ebpf::BPF_ATOMIC_AND => {
                ptr.write_unaligned(old & reg[src] as u32);
                if fetch {
                    reg[src] = old as u64;
                }
            }
            ebpf::BPF_ATOMIC_XOR => {
                ptr.write_unaligned(old ^ reg[src] as u32);
                if fetch {
                    reg[src] = old as u64;
                }
            }
            0xe0 => {
                // XCHG (always has FETCH set)
                ptr.write_unaligned(reg[src] as u32);
                reg[src] = old as u64;
            }
            0xf0 => {
                // CMPXCHG (always has FETCH set)
                if old == reg[0] as u32 {
                    ptr.write_unaligned(reg[src] as u32);
                }
                reg[0] = old as u64;
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

/// Execute a 64-bit atomic operation at `addr`.
#[inline]
fn execute_atomic64(
    addr: u64,
    reg: &mut [u64; 11],
    src: usize,
    op: u32,
    pc: usize,
) -> Result<(), BpfError> {
    let ptr = addr as *mut u64;
    let fetch = (op & ebpf::BPF_ATOMIC_FETCH) != 0;
    let base_op = op & !ebpf::BPF_ATOMIC_FETCH;

    unsafe {
        let old = ptr.read_unaligned();
        match base_op {
            ebpf::BPF_ATOMIC_ADD => {
                ptr.write_unaligned(old.wrapping_add(reg[src]));
                if fetch {
                    reg[src] = old;
                }
            }
            ebpf::BPF_ATOMIC_OR => {
                ptr.write_unaligned(old | reg[src]);
                if fetch {
                    reg[src] = old;
                }
            }
            ebpf::BPF_ATOMIC_AND => {
                ptr.write_unaligned(old & reg[src]);
                if fetch {
                    reg[src] = old;
                }
            }
            ebpf::BPF_ATOMIC_XOR => {
                ptr.write_unaligned(old ^ reg[src]);
                if fetch {
                    reg[src] = old;
                }
            }
            0xe0 => {
                // XCHG
                ptr.write_unaligned(reg[src]);
                reg[src] = old;
            }
            0xf0 => {
                // CMPXCHG
                if old == reg[0] {
                    ptr.write_unaligned(reg[src]);
                }
                reg[0] = old;
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
