// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tagged register safety invariant tests — 30 invariants (T-BPF-001 through
//! T-BPF-030) covered by 32 test functions.
//!
//! These tests validate the tagged-register safety model described in
//! `docs/safe-bpf-interpreter.md` §2–§8, following the test procedures in
//! `docs/safe-bpf-interpreter-validation.md`.

use std::sync::atomic::{AtomicU64, Ordering};

use sonde_bpf::ebpf;
use sonde_bpf::interpreter::{
    execute_program, execute_program_no_maps, BpfError, HelperDescriptor, HelperReturn, MapRegion,
    UNLIMITED_BUDGET,
};

// ── Helpers ─────────────────────────────────────────────────────────

fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let regs = ((src & 0x0f) << 4) | (dst & 0x0f);
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

// ═══════════════════════════════════════════════════════════════════
// §2  Pointer dereference tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-001: Load via scalar register → `NonDereferenceableAccess`
#[test]
fn t_bpf_001_load_via_scalar() {
    // R3 is scalar (default zero-initialized), attempt LDX_DW r0, [r3+0]
    let prog = prog_from(&[
        insn(ebpf::LD_DW_REG, 0, 3, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

/// T-BPF-002: Store via MapDescriptor register → `NonDereferenceableAccess`
#[test]
fn t_bpf_002_store_via_map_descriptor() {
    // LD_DW_IMM r1, src=1, imm=0 → R1 = MapDescriptor for map 0
    // STX_DW [r1+0], r0 → store via MapDescriptor
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0), // src=1 → map relocation
        insn(0x00, 0, 0, 0, 0),            // second slot of LD_DW_IMM
        insn(ebpf::ST_DW_REG, 1, 0, 0, 0), // STX_DW [r1+0], r0
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let map_buf = Box::new([0u8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    assert!(matches!(
        unsafe { execute_program(&prog, &mut ctx, &[], &maps, false, UNLIMITED_BUDGET) },
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
    drop(map_buf);
}

/// T-BPF-003: Load with addr+offset wrapping past u64::MAX → `MemoryAccessViolation`
#[test]
fn t_bpf_003_address_overflow() {
    let mut ctx = [0x42u8; 256];
    let ctx_base = ctx.as_ptr() as u64;
    // delta = u64::MAX - ctx_base so that r1 + delta == u64::MAX exactly.
    // A 1-byte load at [r1+0] then needs range u64::MAX..u64::MAX+1 → overflow.
    // This subtraction can never underflow because ctx_base fits in u64.
    let delta = u64::MAX - ctx_base;
    let lo = delta as u32 as i32;
    let hi = (delta >> 32) as i32;

    let prog = prog_from(&[
        // Load delta into r2 via LD_DW_IMM (64-bit immediate, src=0)
        insn(ebpf::LD_DW_IMM, 2, 0, 0, lo),
        insn(0x00, 0, 0, 0, hi),
        // r1 = r1 + r2 (pointer + scalar → pointer at u64::MAX)
        insn(ebpf::ADD64_REG, 1, 2, 0, 0),
        // LDX_B r0, [r1+0] → address u64::MAX, size 1 → checked_add overflows
        insn(ebpf::LD_B_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::MemoryAccessViolation { .. })
    ));
}

/// T-BPF-004: Atomic op on Context (read-only) — write silently ignored
/// (ND-0505 AC6), memory unchanged, execution continues.
#[test]
fn t_bpf_004_atomic_on_readonly_ctx() {
    let prog = prog_from(&[
        insn(ebpf::ST_DW_ATOMIC, 1, 0, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let result = execute_program_no_maps(&prog, &mut ctx, &[], true, UNLIMITED_BUDGET);
    assert!(
        result.is_ok(),
        "atomic on read-only ctx must succeed: {result:?}"
    );
    // Context memory must remain unchanged (write suppressed).
    assert!(ctx.iter().all(|&b| b == 0x42), "context must be unchanged");
}

/// T-BPF-005: Atomic op via scalar register → `NonDereferenceableAccess`
#[test]
fn t_bpf_005_atomic_on_scalar() {
    // R3 is scalar (never assigned a pointer).
    let prog = prog_from(&[
        insn(ebpf::ST_DW_ATOMIC, 3, 0, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

// ═══════════════════════════════════════════════════════════════════
// §3  Pointer arithmetic tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-006: pointer + pointer → `InvalidPointerArithmetic`
#[test]
fn t_bpf_006_pointer_plus_pointer() {
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 1, 0, 0), // r2 = r1 (Context pointer)
        insn(ebpf::ADD64_REG, 1, 2, 0, 0), // r1 += r2 → pointer + pointer
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
}

/// T-BPF-007: scalar − pointer → `InvalidPointerArithmetic`
#[test]
fn t_bpf_007_scalar_minus_pointer() {
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 3, 0, 0, 42), // r3 = 42 (scalar)
        insn(ebpf::MOV64_REG, 4, 1, 0, 0),  // r4 = r1 (Context pointer)
        insn(ebpf::SUB64_REG, 3, 4, 0, 0),  // r3 -= r4 → scalar - pointer
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
}

/// T-BPF-008: AND/OR/XOR on pointer → `InvalidPointerArithmetic`
#[test]
fn t_bpf_008_bitwise_on_pointer_and() {
    let prog = prog_from(&[
        insn(ebpf::AND64_IMM, 1, 0, 0, 0xff), // r1 &= 0xff (r1 is Context pointer)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
}

#[test]
fn t_bpf_008_bitwise_on_pointer_or() {
    let prog = prog_from(&[
        insn(ebpf::OR64_IMM, 1, 0, 0, 0xff), // r1 |= 0xff (r1 is Context pointer)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
}

#[test]
fn t_bpf_008_bitwise_on_pointer_xor() {
    let prog = prog_from(&[
        insn(ebpf::XOR64_IMM, 1, 0, 0, 0xff), // r1 ^= 0xff (r1 is Context pointer)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
}

/// T-BPF-009: MapDescriptor in non-MOV arithmetic → `InvalidPointerArithmetic`
#[test]
fn t_bpf_009_map_descriptor_add() {
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0), // r2 = MapDescriptor(0)
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::ADD64_IMM, 2, 0, 0, 1), // r2 += 1 → error
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let map_buf = Box::new([0u8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    assert!(matches!(
        unsafe { execute_program(&prog, &mut ctx, &[], &maps, false, UNLIMITED_BUDGET) },
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
    drop(map_buf);
}

/// T-BPF-010: MUL on pointer → result is scalar (tag cleared)
#[test]
fn t_bpf_010_mul_clears_pointer_tag() {
    // MOV r2, r1 (Context pointer), MUL r2, 1, then attempt to dereference r2
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 1, 0, 0), // r2 = r1 (Context pointer)
        insn(ebpf::MUL64_IMM, 2, 0, 0, 1), // r2 *= 1 → clears tag to scalar
        insn(ebpf::LD_DW_REG, 0, 2, 0, 0), // LDX_DW r0, [r2+0] → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

/// T-BPF-011: NEG on pointer → result is scalar
#[test]
fn t_bpf_011_neg_clears_pointer_tag() {
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 1, 0, 0), // r2 = r1 (Context pointer)
        insn(ebpf::NEG64, 2, 0, 0, 0),     // r2 = -r2 → clears tag
        insn(ebpf::LD_DW_REG, 0, 2, 0, 0), // LDX_DW r0, [r2+0] → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

/// T-BPF-012: ALU32 with pointer input → always scalar (truncation)
#[test]
fn t_bpf_012_alu32_clears_pointer_tag() {
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 1, 0, 0), // r2 = r1 (Context pointer)
        insn(ebpf::ADD32_IMM, 2, 0, 0, 0), // ADD32 r2, 0 → clears tag
        insn(ebpf::LD_DW_REG, 0, 2, 0, 0), // LDX_DW r0, [r2+0] → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

/// T-BPF-013: pointer − pointer (same region) → scalar result
#[test]
fn t_bpf_013_ptr_sub_same_region() {
    // r2 = r1, r2 += 4, r2 -= r1 → r2 = 4 (scalar)
    // Store r2 to stack, load it back to r0 to verify.
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 1, 0, 0),   // r2 = r1 (Context pointer)
        insn(ebpf::ADD64_IMM, 2, 0, 0, 4),   // r2 += 4
        insn(ebpf::SUB64_REG, 2, 1, 0, 0),   // r2 -= r1 → scalar 4
        insn(ebpf::ST_DW_REG, 10, 2, -8, 0), // store r2 to stack
        insn(ebpf::LD_DW_REG, 0, 10, -8, 0), // load back to r0
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        4
    );
}

/// T-BPF-014: pointer − pointer (different regions) → `InvalidPointerArithmetic`
#[test]
fn t_bpf_014_ptr_sub_different_regions() {
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 10, 0, 0), // r2 = r10 (Stack pointer)
        insn(ebpf::SUB64_REG, 2, 1, 0, 0),  // r2 -= r1 (Context pointer)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::InvalidPointerArithmetic { .. })
    ));
}

// ═══════════════════════════════════════════════════════════════════
// §4  Tag propagation tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-015: MOV reg-to-reg inherits source pointer tag
#[test]
fn t_bpf_015_mov_inherits_pointer_tag() {
    // r2 = r1 (Context pointer), then load via r2 → should succeed
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 2, 1, 0, 0), // r2 = r1 (inherits Context tag)
        insn(ebpf::LD_B_REG, 0, 2, 0, 0),  // LDX_B r0, [r2+0] → succeeds
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        0x42
    );
}

/// T-BPF-016: Helper call clobbers R1–R5 tags to scalar
#[test]
fn t_bpf_016_helper_clobbers_r1_r5_tags() {
    fn noop_helper(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
        0
    }

    // Save Context pointer in R3, call helper, then try to dereference R3.
    let prog = prog_from(&[
        insn(ebpf::MOV64_REG, 3, 1, 0, 0), // r3 = r1 (Context pointer)
        insn(ebpf::CALL, 0, 0, 0, 1),      // call helper_id=1
        insn(ebpf::LD_B_REG, 0, 3, 0, 0),  // LDX_B r0, [r3+0] → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let helpers = [HelperDescriptor {
        id: 1,
        func: noop_helper,
        ret: HelperReturn::Scalar,
    }];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &helpers, false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

/// T-BPF-017: Initial register state — R1=Context, R10=Stack, R3=scalar
#[test]
fn t_bpf_017_initial_register_tags() {
    // Load from R1 (Context) → succeeds
    // Store to R10 (Stack) → succeeds
    // Load from R3 (scalar) → fails
    let prog = prog_from(&[
        insn(ebpf::LD_B_REG, 0, 1, 0, 0),   // LDX_B r0, [r1+0] → OK
        insn(ebpf::ST_B_REG, 10, 0, -1, 0), // STX_B [r10-1], r0 → OK
        insn(ebpf::LD_B_REG, 0, 3, 0, 0),   // LDX_B r0, [r3+0] → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

// ═══════════════════════════════════════════════════════════════════
// §5  Spill tracking tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-018: STX_DW pointer to stack sets spill bitmap, LDX_DW restores tag
#[test]
fn t_bpf_018_spill_and_restore() {
    // Spill Context pointer to stack, reload, dereference.
    let prog = prog_from(&[
        insn(ebpf::ST_DW_REG, 10, 1, -8, 0), // STX_DW [r10-8], r1 (spill)
        insn(ebpf::LD_DW_REG, 2, 10, -8, 0), // LDX_DW r2, [r10-8] (restore)
        insn(ebpf::LD_B_REG, 0, 2, 0, 0),    // LDX_B r0, [r2+0] → succeeds
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        0x42
    );
}

/// T-BPF-019: LDX_DW from spill slot restores pointer tag after clobber
#[test]
fn t_bpf_019_spill_restore_after_clobber() {
    // Spill r1, clobber r1 to scalar, reload from spill, dereference.
    let prog = prog_from(&[
        insn(ebpf::ST_DW_REG, 10, 1, -8, 0), // spill r1
        insn(ebpf::MOV64_IMM, 1, 0, 0, 0),   // clobber r1 to scalar
        insn(ebpf::LD_DW_REG, 1, 10, -8, 0), // reload r1 from spill
        insn(ebpf::LD_B_REG, 0, 1, 0, 0),    // dereference r1 → succeeds
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        0x42
    );
}

/// T-BPF-020: Partial overwrite (STX_B) clears spill bitmap bit
#[test]
fn t_bpf_020_partial_overwrite_invalidates_spill() {
    // Spill Context pointer, partial overwrite with STX_B, reload, attempt deref.
    let prog = prog_from(&[
        insn(ebpf::ST_DW_REG, 10, 1, -8, 0), // spill r1 (Context pointer)
        insn(ebpf::ST_B_REG, 10, 0, -8, 0),  // partial overwrite (1 byte)
        insn(ebpf::LD_DW_REG, 2, 10, -8, 0), // reload → scalar (spill cleared)
        insn(ebpf::LD_B_REG, 0, 2, 0, 0),    // attempt deref → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

/// T-BPF-021: Spill table overflow (>32 slots) falls back to scalar on reload
#[test]
fn t_bpf_021_spill_table_overflow() {
    // Spill Context pointer to 33 distinct 8-byte-aligned stack slots (0..32).
    // The 33rd spill exceeds MAX_SPILL_SLOTS. Reload slot 32 and attempt deref.
    let mut insns: Vec<[u8; 8]> = Vec::new();

    // Spill to slots 0..=32 (33 spills, offsets: -8, -16, ..., -264)
    for i in 0..33u16 {
        let off = -((i as i16 + 1) * 8);
        insns.push(insn(ebpf::ST_DW_REG, 10, 1, off, 0));
    }
    // Reload the last slot (slot 32, offset -264)
    insns.push(insn(ebpf::LD_DW_REG, 2, 10, -264, 0));
    // Attempt to dereference
    insns.push(insn(ebpf::LD_B_REG, 0, 2, 0, 0));
    insns.push(insn(ebpf::EXIT, 0, 0, 0, 0));

    let prog = prog_from(&insns);
    let mut ctx = [0x42u8; 16];
    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
}

// ═══════════════════════════════════════════════════════════════════
// §6  Call frame tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-022: R6–R9 pointer tags saved/restored across BPF-to-BPF calls
#[test]
fn t_bpf_022_callee_saved_tags_across_calls() {
    // Caller: save Context pointer in R6, call callee, dereference R6.
    // Callee: clobber R6 to scalar, exit.
    let prog = prog_from(&[
        // Caller
        insn(ebpf::MOV64_REG, 6, 1, 0, 0), // 0: r6 = r1 (Context pointer)
        insn(ebpf::CALL, 0, 1, 0, 2),      // 1: local call → insn 4 (pc=2, target=2+2=4)
        insn(ebpf::LD_B_REG, 0, 6, 0, 0),  // 2: LDX_B r0, [r6+0] → should succeed
        insn(ebpf::EXIT, 0, 0, 0, 0),      // 3: main exit
        // Callee (starts at insn 4)
        insn(ebpf::MOV64_IMM, 6, 0, 0, 0), // 4: r6 = 0 (clobber to scalar)
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0), // 5: r0 = 0
        insn(ebpf::EXIT, 0, 0, 0, 0),      // 6: return
    ]);
    let mut ctx = [0x42u8; 16];
    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        0x42
    );
}

/// T-BPF-023: R10 Stack tag invariant maintained across call frames
#[test]
fn t_bpf_023_r10_stack_tag_across_frames() {
    // Both caller and callee can store to stack via R10.
    let prog = prog_from(&[
        // Caller
        insn(ebpf::ST_DW_REG, 10, 0, -8, 0), // 0: store to caller stack
        insn(ebpf::CALL, 0, 1, 0, 2),        // 1: local call → insn 4
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),   // 2: r0 = 1 (success marker)
        insn(ebpf::EXIT, 0, 0, 0, 0),        // 3: main exit
        // Callee
        insn(ebpf::ST_DW_REG, 10, 0, -8, 0), // 4: store to callee stack
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0),   // 5: r0 = 0
        insn(ebpf::EXIT, 0, 0, 0, 0),        // 6: return
    ]);
    let mut ctx = [0x42u8; 16];
    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        1
    );
}

// ═══════════════════════════════════════════════════════════════════
// §7  Helper return validation tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-024: `MapValueOrNull` helper returns 0 → R0 tagged scalar
#[test]
fn t_bpf_024_map_value_or_null_returns_null() {
    fn null_helper(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
        0 // NULL / not found
    }

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0), // r1 = MapDescriptor(0)
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::CALL, 0, 0, 0, 1),     // call helper_id=1
        insn(ebpf::LD_B_REG, 0, 0, 0, 0), // LDX_B r0, [r0+0] → fails
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let map_buf = Box::new([0xAAu8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    let helpers = [HelperDescriptor {
        id: 1,
        func: null_helper,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];
    assert!(matches!(
        unsafe { execute_program(&prog, &mut ctx, &helpers, &maps, false, UNLIMITED_BUDGET) },
        Err(BpfError::NonDereferenceableAccess { .. })
    ));
    drop(map_buf);
}

/// T-BPF-025: `MapValueOrNull` helper returns valid pointer → R0 tagged MapValue
#[test]
fn t_bpf_025_map_value_or_null_returns_valid() {
    let mut map_buf = Box::new([0xBBu8; 64]);
    let map_ptr = map_buf.as_mut_ptr() as u64;

    // Helper returns pointer to start of map data.
    let helper_func: fn(u64, u64, u64, u64, u64) -> u64 = {
        static MAP_DATA_PTR: AtomicU64 = AtomicU64::new(0);
        MAP_DATA_PTR.store(map_ptr, Ordering::SeqCst);
        |_: u64, _: u64, _: u64, _: u64, _: u64| -> u64 { MAP_DATA_PTR.load(Ordering::SeqCst) }
    };

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0), // r1 = MapDescriptor(0)
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::CALL, 0, 0, 0, 1),     // call helper_id=1
        insn(ebpf::LD_B_REG, 0, 0, 0, 0), // LDX_B r0, [r0+0] → succeeds
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    let helpers = [HelperDescriptor {
        id: 1,
        func: helper_func,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];
    let result =
        unsafe { execute_program(&prog, &mut ctx, &helpers, &maps, false, UNLIMITED_BUDGET) };
    assert_eq!(result.unwrap(), 0xBB);
    drop(map_buf);
}

/// T-BPF-026: Helper returns out-of-bounds pointer → `MemoryAccessViolation`
#[test]
fn t_bpf_026_helper_returns_oob_pointer() {
    let map_buf = Box::new([0xCCu8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;

    // Helper returns pointer before map data region.
    let helper_func: fn(u64, u64, u64, u64, u64) -> u64 = {
        static OOB_PTR: AtomicU64 = AtomicU64::new(0);
        OOB_PTR.store(map_ptr.wrapping_sub(1), Ordering::SeqCst);
        |_: u64, _: u64, _: u64, _: u64, _: u64| -> u64 { OOB_PTR.load(Ordering::SeqCst) }
    };

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0),
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::CALL, 0, 0, 0, 1),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    let helpers = [HelperDescriptor {
        id: 1,
        func: helper_func,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];
    assert!(matches!(
        unsafe { execute_program(&prog, &mut ctx, &helpers, &maps, false, UNLIMITED_BUDGET) },
        Err(BpfError::MemoryAccessViolation { .. })
    ));
    drop(map_buf);
}

// ═══════════════════════════════════════════════════════════════════
// §8  Helper argument validation tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-027: Helper expects MapDescriptor but receives scalar → `InvalidHelperArgument`
#[test]
fn t_bpf_027_helper_expects_map_descriptor_gets_scalar() {
    fn dummy_helper(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
        42 // non-zero so MapValueOrNull path validates the descriptor
    }

    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 1, 0, 0, 42), // r1 = scalar (not MapDescriptor)
        insn(ebpf::CALL, 0, 0, 0, 1),       // call helper_id=1
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let map_buf = Box::new([0u8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    let helpers = [HelperDescriptor {
        id: 1,
        func: dummy_helper,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];
    assert!(matches!(
        unsafe { execute_program(&prog, &mut ctx, &helpers, &maps, false, UNLIMITED_BUDGET) },
        Err(BpfError::InvalidHelperArgument { .. })
    ));
    drop(map_buf);
}

// ═══════════════════════════════════════════════════════════════════
// §4.2  LD_DW_IMM map relocation tests
// ═══════════════════════════════════════════════════════════════════

/// T-BPF-028: LD_DW_IMM src=1 with negative imm → `InvalidMapIndex`
#[test]
fn t_bpf_028_ld_dw_imm_negative_map_index() {
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, -1), // src=1, imm=-1
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let map_buf = Box::new([0u8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    let result = unsafe { execute_program(&prog, &mut ctx, &[], &maps, false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidMapIndex { index: -1, .. })),
        "expected InvalidMapIndex with index=-1, got {:?}",
        result
    );
    drop(map_buf);
}

/// T-BPF-029: LD_DW_IMM src=1 with imm ≥ maps.len() → `InvalidMapIndex`
#[test]
fn t_bpf_029_ld_dw_imm_out_of_bounds_map_index() {
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 5), // src=1, imm=5 (only 2 maps)
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let map_buf_0 = Box::new([0u8; 64]);
    let map_buf_1 = Box::new([0u8; 64]);
    let map_ptr_0 = map_buf_0.as_ptr() as u64;
    let map_ptr_1 = map_buf_1.as_ptr() as u64;
    let maps = [
        MapRegion {
            relocated_ptr: map_ptr_0,
            value_size: 64,
            data_start: map_ptr_0,
            data_end: map_ptr_0 + 64,
        },
        MapRegion {
            relocated_ptr: map_ptr_1,
            value_size: 64,
            data_start: map_ptr_1,
            data_end: map_ptr_1 + 64,
        },
    ];
    let result = unsafe { execute_program(&prog, &mut ctx, &[], &maps, false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidMapIndex { index: 5, .. })),
        "expected InvalidMapIndex with index=5, got {:?}",
        result
    );
    drop((map_buf_0, map_buf_1));
}

/// T-BPF-030: LD_DW_IMM src=1 happy path — R1 tagged MapDescriptor
#[test]
fn t_bpf_030_ld_dw_imm_map_descriptor_happy_path() {
    let map_buf = Box::new([0u8; 64]);
    let map_ptr = map_buf.as_ptr() as u64;

    // Helper returns a non-zero in-bounds pointer so the interpreter
    // is forced to validate the MapDescriptor tag on R1 (the NULL path
    // skips that check).
    let helper_func: fn(u64, u64, u64, u64, u64) -> u64 = {
        static MAP_DATA_PTR: AtomicU64 = AtomicU64::new(0);
        MAP_DATA_PTR.store(map_ptr, Ordering::SeqCst);
        |_: u64, _: u64, _: u64, _: u64, _: u64| -> u64 { MAP_DATA_PTR.load(Ordering::SeqCst) }
    };

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0), // r1 = MapDescriptor(0)
        insn(0x00, 0, 0, 0, 0),
        insn(ebpf::CALL, 0, 0, 0, 1), // call helper_id=1 (expects MapDescriptor in r1)
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1), // r0 = 1 (success marker)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0x42u8; 16];
    let maps = [MapRegion {
        relocated_ptr: map_ptr,
        value_size: 64,
        data_start: map_ptr,
        data_end: map_ptr + 64,
    }];
    let helpers = [HelperDescriptor {
        id: 1,
        func: helper_func,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];
    // The helper returns an in-bounds pointer, forcing the interpreter to
    // verify the MapDescriptor tag on R1. If the tag were missing, the call
    // would fail with InvalidHelperArgument.
    assert_eq!(
        unsafe { execute_program(&prog, &mut ctx, &helpers, &maps, false, UNLIMITED_BUDGET) }
            .unwrap(),
        1
    );
    drop(map_buf);
}
