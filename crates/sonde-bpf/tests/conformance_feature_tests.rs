// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! RFC 9669 conformance-group feature tests.

use sonde_bpf::ebpf;
use sonde_bpf::interpreter::{execute_program_no_maps, UNLIMITED_BUDGET};

fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let off = off.to_le_bytes();
    let imm = imm.to_le_bytes();
    [
        opc,
        (src << 4) | (dst & 0x0f),
        off[0],
        off[1],
        imm[0],
        imm[1],
        imm[2],
        imm[3],
    ]
}

fn program(insns: &[[u8; 8]]) -> Vec<u8> {
    insns.iter().flat_map(|insn| insn.iter().copied()).collect()
}

#[cfg(feature = "stack-512")]
fn helper_returns_7(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    7
}

#[test]
#[cfg(feature = "stack-512")]
fn stack_512_configures_one_frame_and_enforces_boundary() {
    assert_eq!(sonde_bpf::ebpf::STACK_SIZE_PER_FRAME, 512);
    assert_eq!(sonde_bpf::ebpf::MAX_CALL_DEPTH, 1);
    assert_eq!(sonde_bpf::ebpf::STACK_SIZE, 512);

    let boundary = program(&[
        insn(ebpf::MOV32_IMM, 0, 0, 0, 0x42),
        insn(ebpf::ST_B_IMM, 10, 0, -512, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let beyond = program(&[
        insn(ebpf::MOV32_IMM, 0, 0, 0, 0x42),
        insn(ebpf::ST_B_IMM, 10, 0, -513, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    execute_program_no_maps(&boundary, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap();
    assert!(matches!(
        execute_program_no_maps(&beyond, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::MemoryAccessViolation { .. })
    ));
}

#[test]
#[cfg(feature = "stack-512")]
fn stack_512_disables_local_calls_but_keeps_helpers() {
    let local_call = program(&[insn(ebpf::CALL, 0, 1, 0, 0), insn(ebpf::EXIT, 0, 0, 0, 0)]);
    let helper_call = program(&[insn(ebpf::CALL, 0, 0, 0, 7), insn(ebpf::EXIT, 0, 0, 0, 0)]);
    let helpers = [sonde_bpf::interpreter::HelperDescriptor {
        id: 7,
        func: helper_returns_7,
        ret: sonde_bpf::interpreter::HelperReturn::Scalar,
    }];
    let mut ctx = [];

    assert!(matches!(
        execute_program_no_maps(&local_call, &mut ctx, &helpers, false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::UnknownOpcode { opc, .. })
            if opc == ebpf::CALL
    ));
    assert_eq!(
        execute_program_no_maps(&helper_call, &mut ctx, &helpers, false, UNLIMITED_BUDGET).unwrap(),
        7
    );
}

#[test]
#[cfg(feature = "base64")]
fn base64_instruction_is_supported() {
    let prog = program(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        42
    );
}

#[test]
#[cfg(not(feature = "base64"))]
fn base64_instruction_is_rejected() {
    let prog = program(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::UnknownOpcode { opc, .. }) if opc == ebpf::MOV64_IMM
    ));
}

#[test]
#[cfg(feature = "atomic32")]
fn atomic32_instruction_is_supported() {
    let prog = program(&[
        insn(ebpf::MOV32_IMM, 2, 0, 0, 1),
        insn(ebpf::ST_W_ATOMIC, 1, 2, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0u8; 4];

    execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap();
    assert_eq!(u32::from_le_bytes(ctx), 1);
}

#[test]
#[cfg(not(feature = "atomic32"))]
fn atomic32_instruction_is_rejected() {
    let prog = program(&[
        insn(ebpf::ST_W_ATOMIC, 1, 2, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0u8; 4];

    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::UnknownOpcode { opc, .. }) if opc == ebpf::ST_W_ATOMIC
    ));
}

#[test]
#[cfg(feature = "atomic64")]
fn atomic64_instruction_is_supported() {
    let prog = program(&[
        insn(ebpf::MOV32_IMM, 2, 0, 0, 1),
        insn(ebpf::ST_DW_ATOMIC, 1, 2, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0u8; 8];

    execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap();
    assert_eq!(u64::from_le_bytes(ctx), 1);
}

#[test]
#[cfg(not(feature = "atomic64"))]
fn atomic64_instruction_is_rejected() {
    let prog = program(&[
        insn(ebpf::ST_DW_ATOMIC, 1, 2, 0, ebpf::BPF_ATOMIC_ADD as i32),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [0u8; 8];

    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::UnknownOpcode { opc, .. }) if opc == ebpf::ST_DW_ATOMIC
    ));
}

#[test]
#[cfg(feature = "divmul32")]
fn divmul32_instruction_is_supported() {
    let prog = program(&[
        insn(ebpf::MOV32_IMM, 0, 0, 0, 6),
        insn(ebpf::MOV32_IMM, 1, 0, 0, 2),
        insn(ebpf::DIV32_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        3
    );
}

#[test]
#[cfg(not(feature = "divmul32"))]
fn divmul32_instruction_is_rejected() {
    let prog = program(&[
        insn(ebpf::DIV32_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::UnknownOpcode { opc, .. }) if opc == ebpf::DIV32_REG
    ));
}

#[test]
#[cfg(feature = "divmul64")]
fn divmul64_instruction_is_supported() {
    let prog = program(&[
        insn(ebpf::MOV32_IMM, 0, 0, 0, 6),
        insn(ebpf::MOV32_IMM, 1, 0, 0, 2),
        insn(ebpf::DIV64_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    assert_eq!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET).unwrap(),
        3
    );
}

#[test]
#[cfg(not(feature = "divmul64"))]
fn divmul64_instruction_is_rejected() {
    let prog = program(&[
        insn(ebpf::DIV64_REG, 0, 1, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];

    assert!(matches!(
        execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET),
        Err(sonde_bpf::interpreter::BpfError::UnknownOpcode { opc, .. }) if opc == ebpf::DIV64_REG
    ));
}
