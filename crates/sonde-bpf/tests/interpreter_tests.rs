// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use sonde_bpf::ebpf;
use sonde_bpf::interpreter::{execute_program, BpfError};

/// Helper: build an instruction from parts.
fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let regs = (src << 4) | (dst & 0x0f);
    let off_bytes = off.to_le_bytes();
    let imm_bytes = imm.to_le_bytes();
    [
        opc,
        regs,
        off_bytes[0],
        off_bytes[1],
        imm_bytes[0],
        imm_bytes[1],
        imm_bytes[2],
        imm_bytes[3],
    ]
}

fn prog_from(insns: &[[u8; 8]]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.iter().copied()).collect()
}

// ── Basic tests ─────────────────────────────────────────────────────

#[test]
fn test_mov64_imm_exit() {
    // r0 = 42; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_add64_imm() {
    // r0 = 10; r0 += 32; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 10),
        insn(ebpf::ADD64_IMM, 0, 0, 0, 32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_add64_reg() {
    // r0 = 10; r1 = 32; r0 += r1; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 10),
        insn(ebpf::MOV64_IMM, 1, 0, 0, 32),
        insn(ebpf::ADD64_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_sub64() {
    // r0 = 100; r0 -= 58; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 100),
        insn(ebpf::SUB64_IMM, 0, 0, 0, 58),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_mul64() {
    // r0 = 6; r0 *= 7; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 6),
        insn(ebpf::MUL64_IMM, 0, 0, 0, 7),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_div64() {
    // r0 = 84; r0 /= 2; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 84),
        insn(ebpf::DIV64_IMM, 0, 0, 0, 2),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_div64_by_zero() {
    // r0 = 42; r0 /= 0; exit  -> result should be 0
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::DIV64_IMM, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 0);
}

#[test]
fn test_mod64() {
    // r0 = 47; r0 %= 5; exit  -> 47 % 5 = 2
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 47),
        insn(ebpf::MOD64_IMM, 0, 0, 0, 5),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 2);
}

#[test]
fn test_mod64_by_zero() {
    // r0 = 42; r0 %= 0; exit  -> result should be 42 (unchanged)
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::MOD64_IMM, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_bitwise_ops() {
    // r0 = 0xff; r0 &= 0x0f; r0 |= 0x30; r0 ^= 0x06; exit
    // 0xff & 0x0f = 0x0f
    // 0x0f | 0x30 = 0x3f
    // 0x3f ^ 0x06 = 0x39 = 57
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0xff),
        insn(ebpf::AND64_IMM, 0, 0, 0, 0x0f),
        insn(ebpf::OR64_IMM, 0, 0, 0, 0x30),
        insn(ebpf::XOR64_IMM, 0, 0, 0, 0x06),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 0x39);
}

#[test]
fn test_shifts64() {
    // r0 = 1; r0 <<= 5; r0 >>= 2; exit  -> 1 << 5 = 32; 32 >> 2 = 8
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),
        insn(ebpf::LSH64_IMM, 0, 0, 0, 5),
        insn(ebpf::RSH64_IMM, 0, 0, 0, 2),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 8);
}

#[test]
fn test_neg64() {
    // r0 = 1; r0 = -r0; exit  -> -1 as u64
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),
        insn(ebpf::NEG64, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        (-1i64) as u64
    );
}

#[test]
fn test_arsh64() {
    // r0 = -16 (as i64); r0 >>= 2 (arithmetic); exit  -> -4
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, -16),
        insn(ebpf::ARSH64_IMM, 0, 0, 0, 2),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        (-4i64) as u64
    );
}

// ── ALU32 tests ─────────────────────────────────────────────────────

#[test]
fn test_add32() {
    // r0 = 0x1_0000_000a; r0 += 32 (32-bit); exit -> upper bits zeroed, 0xa + 32 = 42
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 10),
        insn(ebpf::ADD32_IMM, 0, 0, 0, 32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_mov32_zeroes_upper() {
    // mov64 r0, -1 (all 1s); mov32 r0, 42; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, -1),
        insn(ebpf::MOV32_IMM, 0, 0, 0, 42),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

// ── Jump tests ──────────────────────────────────────────────────────

#[test]
fn test_ja() {
    // r0 = 1; ja +1; r0 = 2; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),
        insn(ebpf::JA, 0, 0, 1, 0),        // skip next insn
        insn(ebpf::MOV64_IMM, 0, 0, 0, 2), // skipped
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 1);
}

#[test]
fn test_jeq_imm_taken() {
    // r0 = 42; r1 = 42; jeq r1, 42, +1; r0 = 0; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::MOV64_IMM, 1, 0, 0, 42),
        insn(ebpf::JEQ_IMM, 1, 0, 1, 42),  // taken
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0), // skipped
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

#[test]
fn test_jeq_imm_not_taken() {
    // r0 = 42; r1 = 99; jeq r1, 42, +1; r0 = 0; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::MOV64_IMM, 1, 0, 0, 99),
        insn(ebpf::JEQ_IMM, 1, 0, 1, 42),  // not taken
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0), // executed
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 0);
}

#[test]
fn test_jgt_unsigned() {
    // Test unsigned comparison: 0xffffffff_ffffffff > 1
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),  // r0 = 1 (success marker)
        insn(ebpf::MOV64_IMM, 1, 0, 0, -1), // r1 = 0xffffffffffffffff (unsigned max)
        insn(ebpf::JGT_IMM, 1, 0, 1, 1),    // if r1 > 1 goto +1 (taken, since unsigned)
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0),  // r0 = 0 (failure)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 1);
}

#[test]
fn test_jslt_signed() {
    // r1 = -5; if (signed) r1 < 0 goto +1; r0 = 0; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),
        insn(ebpf::MOV64_IMM, 1, 0, 0, -5),
        insn(ebpf::JSLT_IMM, 1, 0, 1, 0), // taken
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 1);
}

// ── JMP32 tests ─────────────────────────────────────────────────────

#[test]
fn test_jeq32() {
    // r1 = 0x1_0000_002a; jeq32 r1, 42, +1; r0 = 0; exit
    // 32-bit: lower 32 bits == 42 -> taken
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),  // r0 = 1
        insn(ebpf::MOV64_IMM, 1, 0, 0, 42), // r1 = 42
        insn(ebpf::JEQ_IMM32, 1, 0, 1, 42), // taken
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 1);
}

// ── Memory tests ────────────────────────────────────────────────────

#[test]
fn test_load_store_byte() {
    // Store byte 0xAB at mem[0], load it back.
    // r1 = mem; st8 [r1+0], 0xab; ld8 r0, [r1+0]; exit
    let prog = prog_from(&[
        insn(ebpf::ST_B_IMM, 1, 0, 0, 0xABu8 as i32),
        insn(ebpf::LD_B_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 0xAB);
}

#[test]
fn test_load_store_word() {
    // Store word 0x12345678 at mem[0], load it back.
    let prog = prog_from(&[
        insn(ebpf::ST_W_IMM, 1, 0, 0, 0x12345678u32 as i32),
        insn(ebpf::LD_W_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 0x12345678);
}

#[test]
fn test_load_store_dw() {
    // stdw [r1+0], 0xdeadbeef_cafebabe; ldxdw r0, [r1+0]; exit
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 0, 0, 0xcafebabeu32 as i32),
        insn(0x00, 0, 0, 0, 0xdeadbeefu32 as i32),
        insn(ebpf::ST_DW_REG, 1, 2, 0, 0),
        insn(ebpf::LD_DW_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        0xdeadbeef_cafebabe
    );
}

#[test]
fn test_store_reg() {
    // r2 = 0xBEEF; stxh [r1+0], r2; ldxh r0, [r1+0]; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 2, 0, 0, 0xBEEFu16 as i32),
        insn(ebpf::ST_H_REG, 1, 2, 0, 0),
        insn(ebpf::LD_H_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 0xBEEF);
}

#[test]
fn test_ld_dw_imm() {
    // LD_DW_IMM r0, 0x1_0000_0002 (split across two insns)
    // imm = 2 (lower), next_imm = 1 (upper)
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 0, 0, 0, 2),
        insn(0x00, 0, 0, 0, 1), // pseudo-insn with next_imm = 1
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        0x1_0000_0002
    );
}

// ── Helper function test ────────────────────────────────────────────

#[test]
fn test_helper_call() {
    fn helper_add(a: u64, b: u64, _: u64, _: u64, _: u64) -> u64 {
        a + b
    }

    // r1 = 20; r2 = 22; call helper 1; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 1, 0, 0, 20),
        insn(ebpf::MOV64_IMM, 2, 0, 0, 22),
        insn(ebpf::CALL, 0, 0, 0, 1),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    let helpers: &[(u32, ebpf::Helper)] = &[(1, helper_add)];
    assert_eq!(execute_program(&prog, &mut mem, helpers).unwrap(), 42);
}

#[test]
fn test_unknown_helper() {
    let prog = prog_from(&[insn(ebpf::CALL, 0, 0, 0, 99), insn(ebpf::EXIT, 0, 0, 0, 0)]);
    let mut mem = [];
    assert!(matches!(
        execute_program(&prog, &mut mem, &[]),
        Err(BpfError::UnknownHelper { id: 99, .. })
    ));
}

// ── BPF-to-BPF call test ───────────────────────────────────────────

#[test]
fn test_local_call() {
    // Main:
    //   r1 = 20; r2 = 22
    //   call +2 (local func at pc=5, insn index 5)
    //   exit
    // Local func (at insn 4, which is pc after call=4, +2 = 6... let me think more carefully):
    //   r0 = r1 + r2; exit
    //
    // The call instruction is at insn index 2. After fetch, pc = 3.
    // call imm=2 with src=1 means: pc = 3 + 2 = 5
    // So insn 5 is the start of the local function.
    //
    // Insn 0: mov r1, 20
    // Insn 1: mov r2, 22
    // Insn 2: call +2 (local, src=1) -> jumps to insn 5
    // Insn 3: exit (main return)
    // Insn 4: (filler) never reached
    // Insn 5: r0 = r1
    // Insn 6: r0 += r2
    // Insn 7: exit (returns to insn 3)

    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 1, 0, 0, 20), // 0
        insn(ebpf::MOV64_IMM, 2, 0, 0, 22), // 1
        insn(ebpf::CALL, 0, 1, 0, 2),       // 2: local call, src=1, imm=2 -> pc goes to 3+2=5
        insn(ebpf::EXIT, 0, 0, 0, 0),       // 3: main exit
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0),  // 4: filler (never reached)
        insn(ebpf::MOV64_REG, 0, 1, 0, 0),  // 5: r0 = r1
        insn(ebpf::ADD64_REG, 0, 2, 0, 0),  // 6: r0 += r2
        insn(ebpf::EXIT, 0, 0, 0, 0),       // 7: local return
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

// ── Byte swap test ──────────────────────────────────────────────────

#[test]
fn test_be16() {
    // r0 = 0x0102; be16 r0; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0x0102),
        insn(ebpf::BE, 0, 0, 0, 16),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    let result = execute_program(&prog, &mut mem, &[]).unwrap();
    // On a little-endian host, to_be swaps: 0x0102 -> 0x0201
    assert_eq!(result, 0x0102u16.to_be() as u64);
}

// ── Loop test ───────────────────────────────────────────────────────

#[test]
fn test_loop_sum() {
    // Sum 1..=10 using a loop.
    // r0 = 0 (accumulator); r1 = 10 (counter)
    // loop: r0 += r1; r1 -= 1; jne r1, 0, -3; exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0),  // 0: r0 = 0
        insn(ebpf::MOV64_IMM, 1, 0, 0, 10), // 1: r1 = 10
        insn(ebpf::ADD64_REG, 0, 1, 0, 0),  // 2: r0 += r1
        insn(ebpf::SUB64_IMM, 1, 0, 0, 1),  // 3: r1 -= 1
        insn(ebpf::JNE_IMM, 1, 0, -3, 0),   // 4: if r1 != 0 goto 2 (4+1-3=2)
        insn(ebpf::EXIT, 0, 0, 0, 0),       // 5: exit
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 55);
}

// ── Sign-extension load test ────────────────────────────────────────

#[test]
fn test_ldsx_byte() {
    // Store -5 (0xFB) as a byte, load with sign extension.
    let prog = prog_from(&[
        insn(ebpf::ST_B_IMM, 1, 0, 0, 0xFBu8 as i8 as i32), // -5 as byte
        insn(ebpf::LDSX_B_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    let result = execute_program(&prog, &mut mem, &[]).unwrap();
    assert_eq!(result as i64, -5);
}

// ── Atomic add test ─────────────────────────────────────────────────

#[test]
fn test_atomic_add_w() {
    // mem[0] = 10 (as u32); r2 = 32; atomic_add32 [r1+0], r2; load back; exit
    let prog = prog_from(&[
        insn(ebpf::ST_W_IMM, 1, 0, 0, 10),
        insn(ebpf::MOV64_IMM, 2, 0, 0, 32),
        insn(ebpf::ST_W_ATOMIC, 1, 2, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::LD_W_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

// ── SDIV test ───────────────────────────────────────────────────────

#[test]
fn test_sdiv64() {
    // r0 = -42; r0 sdiv -1; exit -> 42
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, -42),
        insn(ebpf::DIV64_IMM, 0, 0, 1, -1), // offset=1 for SDIV
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(execute_program(&prog, &mut mem, &[]).unwrap(), 42);
}

// ── SMOD test ───────────────────────────────────────────────────────

#[test]
fn test_smod64() {
    // r0 = -13; r0 smod 3; exit -> -1 (truncated division)
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, -13),
        insn(ebpf::MOD64_IMM, 0, 0, 1, 3), // offset=1 for SMOD
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        (-1i64) as u64
    );
}

// ── MOVSX tests ─────────────────────────────────────────────────────

#[test]
fn test_movsx64_8() {
    // r1 = 0x80 (128, which is -128 as i8); movsx64 r0, r1 (off=8); exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 1, 0, 0, 0x80),
        insn(ebpf::MOV64_REG, 0, 1, 8, 0), // MOVSX, off=8 -> sign-extend 8-bit
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        (-128i64) as u64
    );
}

#[test]
fn test_movsx32_8() {
    // r1 = 0x80; movsx32 r0, r1 (off=8); exit
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 1, 0, 0, 0x80),
        insn(ebpf::MOV32_REG, 0, 1, 8, 0), // MOVSX32, off=8 -> sign-extend 8-bit to 32, zero upper
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    // -128 as i32 = 0xffffff80, zero-extended to u64
    assert_eq!(
        execute_program(&prog, &mut mem, &[]).unwrap(),
        0xffffff80u64
    );
}

// ── Memory access violation test ────────────────────────────────────

#[test]
fn test_mem_oob() {
    // Try to load from beyond mem.
    let prog = prog_from(&[
        insn(ebpf::LD_W_REG, 0, 1, 100, 0), // offset 100 with mem of 16 bytes
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    assert!(matches!(
        execute_program(&prog, &mut mem, &[]),
        Err(BpfError::MemoryAccessViolation { .. })
    ));
}
