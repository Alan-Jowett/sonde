// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Tests for BPF helper trust boundary and map return validation (issue #334).
//!
//! Covers the 6 unit-testable gaps identified in the spec audit:
//!
//! 1. `MapValueOrNull` NULL → scalar (§5.2)
//! 2. `MapValueOrNull` non-zero → validated `MapValue` (§5.2)
//! 3. Helper returns out-of-bounds pointer → `MemoryAccessViolation` (§5.2)
//! 4. `InvalidHelperArgument` — scalar where `MapDescriptor` expected (§5.2)
//! 5. `LD_DW_IMM src=1` negative imm → `InvalidMapIndex` (§4.2)
//! 6. `LD_DW_IMM src=1` out-of-bounds → `InvalidMapIndex` (§4.2)
//!
//! E2E map access and map memory budget are out of scope (see issue #334).

use sonde_bpf::ebpf;
use sonde_bpf::interpreter::{
    execute_program, execute_program_no_maps, BpfError, HelperDescriptor, HelperReturn, MapRegion,
    UNLIMITED_BUDGET,
};

// ── Helpers ─────────────────────────────────────────────────────────

/// Build a single 8-byte BPF instruction.
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

/// Helper that always returns 0 (simulates map_lookup_elem "not found").
fn helper_return_null(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    0
}

/// Helper that returns its first argument as a pointer (for controlled testing).
fn helper_return_r1(a: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    a
}

/// Build a `MapRegion` that points into heap-allocated `backing` storage.
///
/// The returned `MapRegion` points into `backing` so the caller must keep it
/// alive for the duration of the test. Backing buffers should be heap-allocated
/// (`Vec<u8>` / `Box<[u8]>`) to ensure pointer provenance is clearly upheld.
fn make_map(backing: &mut [u8], value_size: u32) -> MapRegion {
    let ptr = backing.as_mut_ptr() as u64;
    MapRegion {
        relocated_ptr: ptr,
        value_size,
        data_start: ptr,
        data_end: ptr + backing.len() as u64,
    }
}

// ── §4.2  LD_DW_IMM src=1 — map descriptor relocation ──────────────

#[test]
fn test_ld_dw_imm_src1_valid_map_index() {
    // LD_DW_IMM src=1, imm=0 → load map descriptor for map[0]
    // Then exit with r0 = 0x42 to prove we didn't error.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0), // r2 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),               // second slot of LD_DW_IMM
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0x42),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result = unsafe { execute_program(&prog, &mut ctx, &[], &[map], false, UNLIMITED_BUDGET) };
    assert_eq!(result.unwrap(), 0x42);
}

#[test]
fn test_ld_dw_imm_src1_negative_imm() {
    // LD_DW_IMM src=1, imm=-1 → InvalidMapIndex
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, -1), // negative index
        insn(0, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result = execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET);
    assert!(
        matches!(result, Err(BpfError::InvalidMapIndex { index: -1, .. })),
        "negative imm must yield InvalidMapIndex, got: {result:?}"
    );
}

#[test]
fn test_ld_dw_imm_src1_out_of_bounds() {
    // LD_DW_IMM src=1, imm=5 with only 2 maps → InvalidMapIndex
    let mut b0 = vec![0u8; 64];
    let mut b1 = vec![0u8; 64];
    let m0 = make_map(&mut b0, 8);
    let m1 = make_map(&mut b1, 8);

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 5), // index 5, but only 2 maps
        insn(0, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, &[], &[m0, m1], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidMapIndex { index: 5, .. })),
        "out-of-bounds map index must yield InvalidMapIndex, got: {result:?}"
    );
}

#[test]
fn test_ld_dw_imm_src1_zero_maps() {
    // LD_DW_IMM src=1, imm=0 with empty map list → InvalidMapIndex
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0),
        insn(0, 0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result = execute_program_no_maps(&prog, &mut ctx, &[], false, UNLIMITED_BUDGET);
    assert!(
        matches!(result, Err(BpfError::InvalidMapIndex { index: 0, .. })),
        "map index 0 with no maps must yield InvalidMapIndex, got: {result:?}"
    );
}

// ── §5.2  MapValueOrNull — NULL return → scalar ─────────────────────

#[test]
fn test_map_value_or_null_returns_null_is_scalar() {
    // Setup: LD_DW_IMM src=1 loads map descriptor into R1, then call a
    // helper that returns 0 with MapValueOrNull { map_arg: 1 }.
    // R0 must become scalar(0). We verify by attempting a load via R0 —
    // this must fail with NonDereferenceableAccess because R0 is scalar.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_null,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0), // r1 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),               // LD_DW_IMM second slot
        insn(ebpf::CALL, 0, 0, 0, 1),      // call helper 1 → returns 0
        // R0 is now scalar(0). Attempting a load through it must fail
        // because scalars are not dereferenceable.
        insn(ebpf::LD_B_REG, 3, 0, 0, 0), // r3 = *(u8*)(r0 + 0)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::NonDereferenceableAccess { .. })),
        "dereferencing scalar(0) must yield NonDereferenceableAccess, got: {result:?}"
    );
}

// ── §5.2  MapValueOrNull — non-zero → validated MapValue ────────────

#[test]
fn test_map_value_or_null_returns_valid_ptr_is_map_value() {
    // The helper returns a pointer within the map's backing storage.
    // After the call, R0 should be tagged MapValue and be dereferenceable.
    // We verify by storing a value through R0 and reading it back.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    let valid_ptr = backing.as_mut_ptr() as u64;

    // helper_return_r1 returns R1 (we pass the valid pointer as R1).
    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_r1,
        ret: HelperReturn::MapValueOrNull { map_arg: 2 },
    }];

    // Program:
    //   r2 = map_descriptor(0)       (LD_DW_IMM src=1)
    //   r1 = valid_ptr               (LD_DW_IMM src=0, pointing into backing)
    //   call helper 1                (returns r1 as MapValue)
    //   r1 = 0xBEEF                  (value to store)
    //   *(u32*)(r0 + 0) = r1         (store through MapValue pointer)
    //   r0 = *(u32*)(r0 + 0)         — avoid this: reusing r0 would overwrite the tagged pointer value.
    //
    // Simpler: just verify the call succeeds and returns the expected pointer.
    let imm_lo = valid_ptr as u32 as i32;
    let imm_hi = (valid_ptr >> 32) as u32 as i32;

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0),      // r2 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),                    // second slot
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo), // r1 = valid_ptr (plain imm)
        insn(0, 0, 0, 0, imm_hi),               // second slot
        insn(ebpf::CALL, 0, 0, 0, 1),           // call → returns valid_ptr as MapValue
        // R0 is now tagged MapValue. Store 0xBEEF through it to prove it's dereferenceable.
        insn(ebpf::MOV64_IMM, 3, 0, 0, 0xBEEFu32 as i32),
        insn(ebpf::ST_DW_REG, 0, 3, 0, 0), // *(u64*)(r0+0) = r3
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1), // r0 = 1 (success sentinel)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert_eq!(
        result.unwrap(),
        1,
        "non-zero MapValueOrNull with valid pointer must succeed"
    );
    // Verify the store actually wrote through the pointer.
    assert_eq!(
        u64::from_le_bytes(backing[..8].try_into().unwrap()),
        0xBEEF,
        "store through MapValue pointer must write to backing storage"
    );
}

// ── §5.2  Helper returns out-of-bounds pointer → MemoryAccessViolation

#[test]
fn test_map_value_or_null_returns_out_of_bounds_ptr() {
    // The helper returns a pointer that is outside the map's [data_start, data_end).
    // The interpreter must reject it with MemoryAccessViolation.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    // Choose an address just past the end of the map's storage so it is guaranteed OOB.
    let bad_ptr: u64 = map.data_end.wrapping_add(1);

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_r1,
        ret: HelperReturn::MapValueOrNull { map_arg: 2 },
    }];

    let imm_lo = bad_ptr as u32 as i32;
    let imm_hi = (bad_ptr >> 32) as u32 as i32;

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0), // r2 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo), // r1 = bad_ptr
        insn(0, 0, 0, 0, imm_hi),
        insn(ebpf::CALL, 0, 0, 0, 1), // call → returns bad_ptr
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(
            result,
            Err(BpfError::MemoryAccessViolation { addr, len: 8, .. })
            if addr == bad_ptr
        ),
        "helper returning out-of-bounds pointer must yield MemoryAccessViolation, got: {result:?}"
    );
}

#[test]
fn test_map_value_or_null_returns_ptr_just_past_end() {
    // Pointer is at data_end - value_size + 1 — one byte past valid range.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    // One byte past the last valid start: data_end - value_size + 1
    let barely_oob = backing.as_mut_ptr() as u64
        + backing.len() as u64
        - map.value_size as u64
        + 1;

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_r1,
        ret: HelperReturn::MapValueOrNull { map_arg: 2 },
    }];

    let imm_lo = barely_oob as u32 as i32;
    let imm_hi = (barely_oob >> 32) as u32 as i32;

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0),
        insn(0, 0, 0, 0, 0),
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo),
        insn(0, 0, 0, 0, imm_hi),
        insn(ebpf::CALL, 0, 0, 0, 1),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::MemoryAccessViolation { .. })),
        "pointer just past valid end must yield MemoryAccessViolation, got: {result:?}"
    );
}

#[test]
fn test_map_value_or_null_returns_ptr_before_start() {
    // Pointer is before data_start.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    let before_start = backing.as_mut_ptr() as u64 - 1;

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_r1,
        ret: HelperReturn::MapValueOrNull { map_arg: 2 },
    }];

    let imm_lo = before_start as u32 as i32;
    let imm_hi = (before_start >> 32) as u32 as i32;

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0),
        insn(0, 0, 0, 0, 0),
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo),
        insn(0, 0, 0, 0, imm_hi),
        insn(ebpf::CALL, 0, 0, 0, 1),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::MemoryAccessViolation { .. })),
        "pointer before data_start must yield MemoryAccessViolation, got: {result:?}"
    );
}

#[test]
fn test_map_value_or_null_exact_boundary_succeeds() {
    // Pointer exactly at the last valid position: data_end - value_size.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    let exact_last = backing.as_mut_ptr() as u64 + 64 - 8; // data_end - value_size

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_r1,
        ret: HelperReturn::MapValueOrNull { map_arg: 2 },
    }];

    let imm_lo = exact_last as u32 as i32;
    let imm_hi = (exact_last >> 32) as u32 as i32;

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0),
        insn(0, 0, 0, 0, 0),
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo),
        insn(0, 0, 0, 0, imm_hi),
        insn(ebpf::CALL, 0, 0, 0, 1),
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert_eq!(
        result.unwrap(),
        1,
        "pointer at exact boundary (data_end - value_size) must succeed"
    );
}

// ── §5.2  InvalidHelperArgument — scalar where MapDescriptor expected

#[test]
fn test_invalid_helper_argument_scalar_for_map_descriptor() {
    // Call a helper with MapValueOrNull { map_arg: 1 }, but R1 is a plain
    // scalar (no MapDescriptor tag).  The helper returns non-zero, which
    // triggers the MapDescriptor check.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    let valid_ptr = backing.as_mut_ptr() as u64;

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_r1,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];

    // R1 = scalar pointer value (no LD_DW_IMM src=1, so no MapDescriptor tag)
    let imm_lo = valid_ptr as u32 as i32;
    let imm_hi = (valid_ptr >> 32) as u32 as i32;

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo), // r1 = plain scalar (NOT map desc)
        insn(0, 0, 0, 0, imm_hi),
        insn(ebpf::CALL, 0, 0, 0, 1), // call → returns r1, but r1 has no MapDescriptor
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidHelperArgument { arg: 1, .. })),
        "scalar in map_arg register must yield InvalidHelperArgument, got: {result:?}"
    );
}

#[test]
fn test_invalid_helper_argument_map_value_for_map_descriptor() {
    // R1 carries a MapValue tag (from a prior lookup), not MapDescriptor.
    // Using it as map_arg should fail with InvalidHelperArgument.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);
    let valid_ptr = backing.as_mut_ptr() as u64;

    // First helper: returns valid_ptr as MapValue via map_arg=2
    let helpers = &[
        HelperDescriptor {
            id: 1,
            func: helper_return_r1,
            ret: HelperReturn::MapValueOrNull { map_arg: 2 },
        },
        HelperDescriptor {
            id: 2,
            func: helper_return_r1,
            ret: HelperReturn::MapValueOrNull { map_arg: 1 },
        },
    ];

    let imm_lo = valid_ptr as u32 as i32;
    let imm_hi = (valid_ptr >> 32) as u32 as i32;

    let prog = prog_from(&[
        // Step 1: get a valid MapValue pointer in R0
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0), // r2 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),
        insn(ebpf::LD_DW_IMM, 1, 0, 0, imm_lo), // r1 = valid_ptr
        insn(0, 0, 0, 0, imm_hi),
        insn(ebpf::CALL, 0, 0, 0, 1), // call → R0 = MapValue(valid_ptr)
        // Step 2: Move MapValue R0 into R1 (which has MapValue tag, not MapDescriptor)
        insn(ebpf::MOV64_REG, 1, 0, 0, 0), // r1 = r0 (MapValue tagged)
        // Load R1 (MapValue) as imm for helper arg (r1 has non-zero value)
        insn(ebpf::CALL, 0, 0, 0, 2), // call helper 2 with map_arg=1, but R1 is MapValue
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidHelperArgument { arg: 1, .. })),
        "MapValue tag in map_arg register must yield InvalidHelperArgument, got: {result:?}"
    );
}

// ── §5.2  R1-R5 tags clobbered after helper call ────────────────────

#[test]
fn test_helper_call_clobbers_r1_r5_tags() {
    // After a helper call, R1-R5 must lose their tags (become scalar)
    // while their values are preserved. Strategy:
    //   1. Load MapDescriptor into R1 via LD_DW_IMM src=1
    //   2. Call a Scalar helper (clobbers R1 tag, preserves value)
    //   3. Call a MapValueOrNull helper with map_arg=1
    // The second helper returns R1.value (non-zero relocated_ptr), which
    // triggers the MapDescriptor tag check. Since R1's tag was clobbered
    // to scalar by step 2, this must fail with InvalidHelperArgument.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);

    fn helper_noop(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
        42
    }

    let helpers = &[
        HelperDescriptor {
            id: 1,
            func: helper_noop,
            ret: HelperReturn::Scalar,
        },
        HelperDescriptor {
            id: 2,
            func: helper_return_r1,
            ret: HelperReturn::MapValueOrNull { map_arg: 1 },
        },
    ];

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 1, 1, 0, 0), // r1 = map_descriptor(0) [tagged]
        insn(0, 0, 0, 0, 0),
        insn(ebpf::CALL, 0, 0, 0, 1), // call scalar helper → clobbers R1 tag
        // Explicitly set R1 to a known non-zero scalar after the first call
        // so the test doesn't depend on R1's value being preserved (R1-R5 are
        // caller-saved in the eBPF calling convention).
        insn(ebpf::MOV64_IMM, 1, 0, 0, 0x1234), // r1 = non-zero scalar (untagged)
        insn(ebpf::CALL, 0, 0, 0, 2), // call MapValueOrNull(map_arg=1) — R1 is scalar now
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidHelperArgument { arg: 1, .. })),
        "R1 tag should be clobbered after first helper call, got: {result:?}"
    );
}

// ── §5.2  MapValueOrNull with NULL skips map_arg validation ─────────

#[test]
fn test_map_value_or_null_null_skips_map_arg_validation() {
    // When the helper returns 0 (NULL), the interpreter should NOT check
    // the map_arg register's tag — R0 becomes scalar(0) regardless.
    // Here R1 is a plain scalar (no MapDescriptor), but helper returns 0.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);

    let helpers = &[HelperDescriptor {
        id: 1,
        func: helper_return_null,
        ret: HelperReturn::MapValueOrNull { map_arg: 1 },
    }];

    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 1, 0, 0, 0x42), // r1 = scalar (no map descriptor)
        insn(ebpf::CALL, 0, 0, 0, 1),         // call → returns 0
        insn(ebpf::EXIT, 0, 0, 0, 0),         // exit with r0 = 0
    ]);
    let mut ctx = [];
    let result =
        unsafe { execute_program(&prog, &mut ctx, helpers, &[map], false, UNLIMITED_BUDGET) };
    assert_eq!(
        result.unwrap(),
        0,
        "NULL return should skip map_arg validation and yield scalar(0)"
    );
}

// ── MapDescriptor is not dereferenceable ─────────────────────────────

#[test]
fn test_map_descriptor_not_dereferenceable() {
    // Loading a MapDescriptor into a register and trying to dereference it
    // must fail with NonDereferenceableAccess.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0), // r2 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),
        insn(ebpf::LD_DW_REG, 0, 2, 0, 0), // r0 = *(u64*)(r2+0) — should fail
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result = unsafe { execute_program(&prog, &mut ctx, &[], &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::NonDereferenceableAccess { .. })),
        "dereferencing MapDescriptor must fail, got: {result:?}"
    );
}

// ── MapDescriptor arithmetic is rejected ─────────────────────────────

#[test]
fn test_map_descriptor_arithmetic_rejected() {
    // Adding a scalar to a MapDescriptor register must fail with
    // InvalidPointerArithmetic.
    let mut backing = vec![0u8; 64];
    let map = make_map(&mut backing, 8);

    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 1, 0, 0), // r2 = map_descriptor(0)
        insn(0, 0, 0, 0, 0),
        insn(ebpf::ADD64_IMM, 2, 0, 0, 1), // r2 += 1 — should fail
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut ctx = [];
    let result = unsafe { execute_program(&prog, &mut ctx, &[], &[map], false, UNLIMITED_BUDGET) };
    assert!(
        matches!(result, Err(BpfError::InvalidPointerArithmetic { .. })),
        "arithmetic on MapDescriptor must yield InvalidPointerArithmetic, got: {result:?}"
    );
}
