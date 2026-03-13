// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Opcode constants and instruction decoding for BPF (RFC 9669).
//!
//! This module defines the complete BPF instruction set encoding.

#![cfg_attr(rustfmt, rustfmt_skip)]

// ── Instruction size ────────────────────────────────────────────────
/// Size of a single BPF instruction in bytes.
pub const INSN_SIZE: usize = 8;

/// Maximum call depth for BPF-to-BPF calls.
pub const MAX_CALL_DEPTH: usize = 8;

/// BPF stack size per call frame in bytes.
pub const STACK_SIZE_PER_FRAME: usize = 512;

/// Total BPF stack size in bytes (one frame per call depth level).
pub const STACK_SIZE: usize = STACK_SIZE_PER_FRAME * MAX_CALL_DEPTH;

// ── Instruction classes (3 LSBs of opcode) ──────────────────────────
pub const BPF_LD:    u8 = 0x00;
pub const BPF_LDX:   u8 = 0x01;
pub const BPF_ST:    u8 = 0x02;
pub const BPF_STX:   u8 = 0x03;
pub const BPF_ALU:   u8 = 0x04;
pub const BPF_JMP:   u8 = 0x05;
pub const BPF_JMP32: u8 = 0x06;
pub const BPF_ALU64: u8 = 0x07;

// ── Size modifiers (load/store) ─────────────────────────────────────
pub const BPF_W:  u8 = 0x00; // word        (4 bytes)
pub const BPF_H:  u8 = 0x08; // half-word   (2 bytes)
pub const BPF_B:  u8 = 0x10; // byte        (1 byte)
pub const BPF_DW: u8 = 0x18; // double-word (8 bytes)

// ── Mode modifiers (load/store) ─────────────────────────────────────
pub const BPF_IMM:    u8 = 0x00;
pub const BPF_ABS:    u8 = 0x20;
pub const BPF_IND:    u8 = 0x40;
pub const BPF_MEM:    u8 = 0x60;
pub const BPF_MEMSX:  u8 = 0x80;
pub const BPF_ATOMIC: u8 = 0xc0;

// ── Source modifiers (ALU/JMP) ──────────────────────────────────────
pub const BPF_K: u8 = 0x00; // immediate
pub const BPF_X: u8 = 0x08; // register

// ── ALU operation codes ─────────────────────────────────────────────
pub const BPF_ADD:  u8 = 0x00;
pub const BPF_SUB:  u8 = 0x10;
pub const BPF_MUL:  u8 = 0x20;
pub const BPF_DIV:  u8 = 0x30;
pub const BPF_OR:   u8 = 0x40;
pub const BPF_AND:  u8 = 0x50;
pub const BPF_LSH:  u8 = 0x60;
pub const BPF_RSH:  u8 = 0x70;
pub const BPF_NEG:  u8 = 0x80;
pub const BPF_MOD:  u8 = 0x90;
pub const BPF_XOR:  u8 = 0xa0;
pub const BPF_MOV:  u8 = 0xb0;
pub const BPF_ARSH: u8 = 0xc0;
pub const BPF_END:  u8 = 0xd0;

// ── JMP operation codes ─────────────────────────────────────────────
pub const BPF_JA:   u8 = 0x00;
pub const BPF_JEQ:  u8 = 0x10;
pub const BPF_JGT:  u8 = 0x20;
pub const BPF_JGE:  u8 = 0x30;
pub const BPF_JSET: u8 = 0x40;
pub const BPF_JNE:  u8 = 0x50;
pub const BPF_JSGT: u8 = 0x60;
pub const BPF_JSGE: u8 = 0x70;
pub const BPF_CALL: u8 = 0x80;
pub const BPF_EXIT: u8 = 0x90;
pub const BPF_JLT:  u8 = 0xa0;
pub const BPF_JLE:  u8 = 0xb0;
pub const BPF_JSLT: u8 = 0xc0;
pub const BPF_JSLE: u8 = 0xd0;

// ── Atomic operation imm field values ───────────────────────────────
pub const BPF_ATOMIC_ADD:     u32 = 0x00;
pub const BPF_ATOMIC_OR:      u32 = 0x40;
pub const BPF_ATOMIC_AND:     u32 = 0x50;
pub const BPF_ATOMIC_XOR:     u32 = 0xa0;
pub const BPF_ATOMIC_FETCH:   u32 = 0x01;
pub const BPF_ATOMIC_XCHG:    u32 = 0xe0 | 0x01;
pub const BPF_ATOMIC_CMPXCHG: u32 = 0xf0 | 0x01;

// ══════════════════════════════════════════════════════════════════════
// Composite opcodes — ALU32
// ══════════════════════════════════════════════════════════════════════
pub const ADD32_IMM:  u8 = BPF_ALU | BPF_K | BPF_ADD;
pub const ADD32_REG:  u8 = BPF_ALU | BPF_X | BPF_ADD;
pub const SUB32_IMM:  u8 = BPF_ALU | BPF_K | BPF_SUB;
pub const SUB32_REG:  u8 = BPF_ALU | BPF_X | BPF_SUB;
pub const MUL32_IMM:  u8 = BPF_ALU | BPF_K | BPF_MUL;
pub const MUL32_REG:  u8 = BPF_ALU | BPF_X | BPF_MUL;
pub const DIV32_IMM:  u8 = BPF_ALU | BPF_K | BPF_DIV;
pub const DIV32_REG:  u8 = BPF_ALU | BPF_X | BPF_DIV;
pub const OR32_IMM:   u8 = BPF_ALU | BPF_K | BPF_OR;
pub const OR32_REG:   u8 = BPF_ALU | BPF_X | BPF_OR;
pub const AND32_IMM:  u8 = BPF_ALU | BPF_K | BPF_AND;
pub const AND32_REG:  u8 = BPF_ALU | BPF_X | BPF_AND;
pub const LSH32_IMM:  u8 = BPF_ALU | BPF_K | BPF_LSH;
pub const LSH32_REG:  u8 = BPF_ALU | BPF_X | BPF_LSH;
pub const RSH32_IMM:  u8 = BPF_ALU | BPF_K | BPF_RSH;
pub const RSH32_REG:  u8 = BPF_ALU | BPF_X | BPF_RSH;
pub const NEG32:      u8 = BPF_ALU | BPF_K | BPF_NEG;
pub const MOD32_IMM:  u8 = BPF_ALU | BPF_K | BPF_MOD;
pub const MOD32_REG:  u8 = BPF_ALU | BPF_X | BPF_MOD;
pub const XOR32_IMM:  u8 = BPF_ALU | BPF_K | BPF_XOR;
pub const XOR32_REG:  u8 = BPF_ALU | BPF_X | BPF_XOR;
pub const MOV32_IMM:  u8 = BPF_ALU | BPF_K | BPF_MOV;
pub const MOV32_REG:  u8 = BPF_ALU | BPF_X | BPF_MOV;
pub const ARSH32_IMM: u8 = BPF_ALU | BPF_K | BPF_ARSH;
pub const ARSH32_REG: u8 = BPF_ALU | BPF_X | BPF_ARSH;

pub const LE:    u8 = BPF_ALU   | BPF_K | BPF_END;
pub const BE:    u8 = BPF_ALU   | BPF_X | BPF_END;
pub const BSWAP: u8 = BPF_ALU64 | BPF_K | BPF_END;

// ══════════════════════════════════════════════════════════════════════
// Composite opcodes — ALU64
// ══════════════════════════════════════════════════════════════════════
pub const ADD64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_ADD;
pub const ADD64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_ADD;
pub const SUB64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_SUB;
pub const SUB64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_SUB;
pub const MUL64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_MUL;
pub const MUL64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_MUL;
pub const DIV64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_DIV;
pub const DIV64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_DIV;
pub const OR64_IMM:   u8 = BPF_ALU64 | BPF_K | BPF_OR;
pub const OR64_REG:   u8 = BPF_ALU64 | BPF_X | BPF_OR;
pub const AND64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_AND;
pub const AND64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_AND;
pub const LSH64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_LSH;
pub const LSH64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_LSH;
pub const RSH64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_RSH;
pub const RSH64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_RSH;
pub const NEG64:      u8 = BPF_ALU64 | BPF_K | BPF_NEG;
pub const MOD64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_MOD;
pub const MOD64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_MOD;
pub const XOR64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_XOR;
pub const XOR64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_XOR;
pub const MOV64_IMM:  u8 = BPF_ALU64 | BPF_K | BPF_MOV;
pub const MOV64_REG:  u8 = BPF_ALU64 | BPF_X | BPF_MOV;
pub const ARSH64_IMM: u8 = BPF_ALU64 | BPF_K | BPF_ARSH;
pub const ARSH64_REG: u8 = BPF_ALU64 | BPF_X | BPF_ARSH;

// ══════════════════════════════════════════════════════════════════════
// Composite opcodes — Load / Store
// ══════════════════════════════════════════════════════════════════════
pub const LD_DW_IMM: u8 = BPF_LD  | BPF_IMM | BPF_DW;

pub const LD_B_REG:  u8 = BPF_LDX | BPF_MEM | BPF_B;
pub const LD_H_REG:  u8 = BPF_LDX | BPF_MEM | BPF_H;
pub const LD_W_REG:  u8 = BPF_LDX | BPF_MEM | BPF_W;
pub const LD_DW_REG: u8 = BPF_LDX | BPF_MEM | BPF_DW;

pub const LDSX_B_REG: u8 = BPF_LDX | BPF_MEMSX | BPF_B;
pub const LDSX_H_REG: u8 = BPF_LDX | BPF_MEMSX | BPF_H;
pub const LDSX_W_REG: u8 = BPF_LDX | BPF_MEMSX | BPF_W;

pub const ST_B_IMM:  u8 = BPF_ST  | BPF_MEM | BPF_B;
pub const ST_H_IMM:  u8 = BPF_ST  | BPF_MEM | BPF_H;
pub const ST_W_IMM:  u8 = BPF_ST  | BPF_MEM | BPF_W;
pub const ST_DW_IMM: u8 = BPF_ST  | BPF_MEM | BPF_DW;

pub const ST_B_REG:  u8 = BPF_STX | BPF_MEM | BPF_B;
pub const ST_H_REG:  u8 = BPF_STX | BPF_MEM | BPF_H;
pub const ST_W_REG:  u8 = BPF_STX | BPF_MEM | BPF_W;
pub const ST_DW_REG: u8 = BPF_STX | BPF_MEM | BPF_DW;

pub const ST_W_ATOMIC:  u8 = BPF_STX | BPF_ATOMIC | BPF_W;
pub const ST_DW_ATOMIC: u8 = BPF_STX | BPF_ATOMIC | BPF_DW;

// ── Legacy packet access (deprecated, but included for completeness) ─
pub const LD_ABS_B:  u8 = BPF_LD | BPF_ABS | BPF_B;
pub const LD_ABS_H:  u8 = BPF_LD | BPF_ABS | BPF_H;
pub const LD_ABS_W:  u8 = BPF_LD | BPF_ABS | BPF_W;
pub const LD_ABS_DW: u8 = BPF_LD | BPF_ABS | BPF_DW;
pub const LD_IND_B:  u8 = BPF_LD | BPF_IND | BPF_B;
pub const LD_IND_H:  u8 = BPF_LD | BPF_IND | BPF_H;
pub const LD_IND_W:  u8 = BPF_LD | BPF_IND | BPF_W;
pub const LD_IND_DW: u8 = BPF_LD | BPF_IND | BPF_DW;

// ══════════════════════════════════════════════════════════════════════
// Composite opcodes — JMP (64-bit comparisons)
// ══════════════════════════════════════════════════════════════════════
pub const JA:        u8 = BPF_JMP | BPF_JA;
pub const JEQ_IMM:   u8 = BPF_JMP | BPF_K | BPF_JEQ;
pub const JEQ_REG:   u8 = BPF_JMP | BPF_X | BPF_JEQ;
pub const JGT_IMM:   u8 = BPF_JMP | BPF_K | BPF_JGT;
pub const JGT_REG:   u8 = BPF_JMP | BPF_X | BPF_JGT;
pub const JGE_IMM:   u8 = BPF_JMP | BPF_K | BPF_JGE;
pub const JGE_REG:   u8 = BPF_JMP | BPF_X | BPF_JGE;
pub const JLT_IMM:   u8 = BPF_JMP | BPF_K | BPF_JLT;
pub const JLT_REG:   u8 = BPF_JMP | BPF_X | BPF_JLT;
pub const JLE_IMM:   u8 = BPF_JMP | BPF_K | BPF_JLE;
pub const JLE_REG:   u8 = BPF_JMP | BPF_X | BPF_JLE;
pub const JSET_IMM:  u8 = BPF_JMP | BPF_K | BPF_JSET;
pub const JSET_REG:  u8 = BPF_JMP | BPF_X | BPF_JSET;
pub const JNE_IMM:   u8 = BPF_JMP | BPF_K | BPF_JNE;
pub const JNE_REG:   u8 = BPF_JMP | BPF_X | BPF_JNE;
pub const JSGT_IMM:  u8 = BPF_JMP | BPF_K | BPF_JSGT;
pub const JSGT_REG:  u8 = BPF_JMP | BPF_X | BPF_JSGT;
pub const JSGE_IMM:  u8 = BPF_JMP | BPF_K | BPF_JSGE;
pub const JSGE_REG:  u8 = BPF_JMP | BPF_X | BPF_JSGE;
pub const JSLT_IMM:  u8 = BPF_JMP | BPF_K | BPF_JSLT;
pub const JSLT_REG:  u8 = BPF_JMP | BPF_X | BPF_JSLT;
pub const JSLE_IMM:  u8 = BPF_JMP | BPF_K | BPF_JSLE;
pub const JSLE_REG:  u8 = BPF_JMP | BPF_X | BPF_JSLE;
pub const CALL:      u8 = BPF_JMP | BPF_K | BPF_CALL;
pub const EXIT:      u8 = BPF_JMP | BPF_K | BPF_EXIT;

// ══════════════════════════════════════════════════════════════════════
// Composite opcodes — JMP32 (32-bit comparisons)
// ══════════════════════════════════════════════════════════════════════
pub const JA32:        u8 = BPF_JMP32 | BPF_K | BPF_JA;
pub const JEQ_IMM32:   u8 = BPF_JMP32 | BPF_K | BPF_JEQ;
pub const JEQ_REG32:   u8 = BPF_JMP32 | BPF_X | BPF_JEQ;
pub const JGT_IMM32:   u8 = BPF_JMP32 | BPF_K | BPF_JGT;
pub const JGT_REG32:   u8 = BPF_JMP32 | BPF_X | BPF_JGT;
pub const JGE_IMM32:   u8 = BPF_JMP32 | BPF_K | BPF_JGE;
pub const JGE_REG32:   u8 = BPF_JMP32 | BPF_X | BPF_JGE;
pub const JLT_IMM32:   u8 = BPF_JMP32 | BPF_K | BPF_JLT;
pub const JLT_REG32:   u8 = BPF_JMP32 | BPF_X | BPF_JLT;
pub const JLE_IMM32:   u8 = BPF_JMP32 | BPF_K | BPF_JLE;
pub const JLE_REG32:   u8 = BPF_JMP32 | BPF_X | BPF_JLE;
pub const JSET_IMM32:  u8 = BPF_JMP32 | BPF_K | BPF_JSET;
pub const JSET_REG32:  u8 = BPF_JMP32 | BPF_X | BPF_JSET;
pub const JNE_IMM32:   u8 = BPF_JMP32 | BPF_K | BPF_JNE;
pub const JNE_REG32:   u8 = BPF_JMP32 | BPF_X | BPF_JNE;
pub const JSGT_IMM32:  u8 = BPF_JMP32 | BPF_K | BPF_JSGT;
pub const JSGT_REG32:  u8 = BPF_JMP32 | BPF_X | BPF_JSGT;
pub const JSGE_IMM32:  u8 = BPF_JMP32 | BPF_K | BPF_JSGE;
pub const JSGE_REG32:  u8 = BPF_JMP32 | BPF_X | BPF_JSGE;
pub const JSLT_IMM32:  u8 = BPF_JMP32 | BPF_K | BPF_JSLT;
pub const JSLT_REG32:  u8 = BPF_JMP32 | BPF_X | BPF_JSLT;
pub const JSLE_IMM32:  u8 = BPF_JMP32 | BPF_K | BPF_JSLE;
pub const JSLE_REG32:  u8 = BPF_JMP32 | BPF_X | BPF_JSLE;

// ── Masks ───────────────────────────────────────────────────────────
pub const BPF_CLS_MASK:    u8 = 0x07;
pub const BPF_ALU_OP_MASK: u8 = 0xf0;

// ══════════════════════════════════════════════════════════════════════
// Instruction decoding
// ══════════════════════════════════════════════════════════════════════

/// A decoded BPF instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    /// Opcode byte.
    pub opc: u8,
    /// Destination register (0-10).
    pub dst: u8,
    /// Source register (0-10).
    pub src: u8,
    /// Signed 16-bit offset.
    pub off: i16,
    /// Signed 32-bit immediate.
    pub imm: i32,
}

/// Decode instruction at index `idx` (instruction number, not byte offset).
///
/// # Panics
/// Panics if `prog` is too short to contain the instruction.
#[inline(always)]
pub fn get_insn(prog: &[u8], idx: usize) -> Insn {
    let off = idx * INSN_SIZE;
    let b = &prog[off..off + INSN_SIZE];
    Insn {
        opc: b[0],
        dst: b[1] & 0x0f,
        src: (b[1] >> 4) & 0x0f,
        off: i16::from_le_bytes([b[2], b[3]]),
        imm: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
    }
}

/// Helper function type: `fn(r1, r2, r3, r4, r5) -> r0`.
pub type Helper = fn(u64, u64, u64, u64, u64) -> u64;
