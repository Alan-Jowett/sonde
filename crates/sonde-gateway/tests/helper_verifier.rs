// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Prevail verifier coverage tests for all 17 sonde BPF helper prototypes.
//!
//! Each test constructs minimal BPF bytecode that calls a single helper with
//! correctly-typed arguments, wraps it in a minimal ELF, and verifies that
//! `ProgramLibrary::ingest_elf()` (which runs the Prevail verifier with
//! `SondePlatform`) accepts the program.
//!
//! Map helpers (10, 11) are skipped because the Prevail verifier requires a
//! map section in the ELF for `PtrToMap` / `PtrToMapKey` / `PtrToMapValue`
//! argument types, and constructing a valid map ELF is out of scope here.

use sonde_gateway::program::{ProgramLibrary, VerificationProfile};

// ---------------------------------------------------------------------------
// BPF instruction encoding helpers
// ---------------------------------------------------------------------------

fn bpf_insn(opcode: u8, dst_src: u8, offset: i16, imm: i32) -> [u8; 8] {
    let mut insn = [0u8; 8];
    insn[0] = opcode;
    insn[1] = dst_src;
    insn[2..4].copy_from_slice(&offset.to_le_bytes());
    insn[4..8].copy_from_slice(&imm.to_le_bytes());
    insn
}

/// `mov rN, imm` (BPF_MOV64_IMM)
fn mov_imm(dst: u8, imm: i32) -> [u8; 8] {
    bpf_insn(0xb7, dst, 0, imm)
}

/// `mov rD, rS` (BPF_MOV64_REG)
fn mov_reg(dst: u8, src: u8) -> [u8; 8] {
    bpf_insn(0xbf, dst | (src << 4), 0, 0)
}

/// `add rN, imm` (BPF_ALU64_IMM + BPF_ADD)
fn add_imm(dst: u8, imm: i32) -> [u8; 8] {
    bpf_insn(0x07, dst, 0, imm)
}

/// `*(u32 *)(rD + off) = imm` (BPF_ST_MEM_W) — initialise stack memory.
fn st_mem_w(dst: u8, offset: i16, imm: i32) -> [u8; 8] {
    bpf_insn(0x62, dst, offset, imm)
}

/// `call helper_id` (BPF_CALL)
fn call_helper(id: i32) -> [u8; 8] {
    bpf_insn(0x85, 0, 0, id)
}

/// `exit` (BPF_EXIT)
fn exit_insn() -> [u8; 8] {
    bpf_insn(0x95, 0, 0, 0)
}

// ---------------------------------------------------------------------------
// Minimal ELF builder (duplicated from program.rs tests since it is private)
// ---------------------------------------------------------------------------

fn make_sonde_elf(bpf_code: &[u8]) -> Vec<u8> {
    let shstrtab: &[u8] = b"\0sonde\0.shstrtab\0";

    let text_offset: u64 = 64;
    let shstrtab_offset: u64 = text_offset + bpf_code.len() as u64;
    let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

    let mut elf = Vec::new();

    // ELF header (64 bytes)
    elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
    elf.push(2); // EI_CLASS = ELFCLASS64
    elf.push(1); // EI_DATA = ELFDATA2LSB
    elf.push(1); // EI_VERSION
    elf.extend_from_slice(&[0; 9]); // padding
    elf.extend_from_slice(&1u16.to_le_bytes()); // e_type = ET_REL
    elf.extend_from_slice(&247u16.to_le_bytes()); // e_machine = EM_BPF
    elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
    elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
    elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    elf.extend_from_slice(&3u16.to_le_bytes()); // e_shnum
    elf.extend_from_slice(&2u16.to_le_bytes()); // e_shstrndx = 2
    assert_eq!(elf.len(), 64);

    // sonde section data
    elf.extend_from_slice(bpf_code);

    // .shstrtab section data
    elf.extend_from_slice(shstrtab);

    // Section headers (3 × 64 bytes)

    // [0] Null section header
    elf.extend_from_slice(&[0u8; 64]);

    // [1] sonde section header
    let mut sh = [0u8; 64];
    sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name
    sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
    let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
    sh[8..16].copy_from_slice(&flags.to_le_bytes());
    sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
    sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
    sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
    elf.extend_from_slice(&sh);

    // [2] .shstrtab section header
    let mut sh2 = [0u8; 64];
    sh2[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name
    sh2[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
    sh2[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
    sh2[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
    sh2[48..56].copy_from_slice(&1u64.to_le_bytes()); // sh_addralign
    elf.extend_from_slice(&sh2);

    elf
}

/// Concatenate BPF instructions into a flat bytecode slice.
fn assemble(insns: &[[u8; 8]]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.iter().copied()).collect()
}

/// Initialise N consecutive 32-bit stack slots starting at `r10 + base_off`.
/// This is required so the Prevail verifier sees the memory as initialised
/// before a readable-pointer argument is passed.
fn init_stack(base_off: i16, num_words: u16) -> Vec<[u8; 8]> {
    (0..num_words)
        .map(|i| st_mem_w(10, base_off + (i as i16) * 4, 0))
        .collect()
}

// ---------------------------------------------------------------------------
// Helper: run the verifier on assembled bytecode
// ---------------------------------------------------------------------------

fn verify_helper(helper_id: i32, helper_name: &str, insns: &[[u8; 8]]) {
    let bytecode = assemble(insns);
    let elf = make_sonde_elf(&bytecode);
    let lib = ProgramLibrary::new();
    match lib.ingest_elf(&elf, VerificationProfile::Resident) {
        Ok(_) => {}
        Err(err) => panic!("helper {helper_id} ({helper_name}) verification failed: {err:?}"),
    }
}

// ===========================================================================
// Tests — one per helper
// ===========================================================================

// ---- Simple helpers (scalar arguments only) ----

#[test]
fn verify_helper_05_gpio_read() {
    verify_helper(
        5,
        "gpio_read",
        &[
            mov_imm(1, 0), // r1 = pin
            call_helper(5),
            exit_insn(),
        ],
    );
}

#[test]
fn verify_helper_06_gpio_write() {
    verify_helper(
        6,
        "gpio_write",
        &[
            mov_imm(1, 0), // r1 = pin
            mov_imm(2, 1), // r2 = value
            call_helper(6),
            exit_insn(),
        ],
    );
}

#[test]
fn verify_helper_07_adc_read() {
    verify_helper(
        7,
        "adc_read",
        &[
            mov_imm(1, 0), // r1 = channel
            call_helper(7),
            exit_insn(),
        ],
    );
}

#[test]
fn verify_helper_12_get_time() {
    verify_helper(12, "get_time", &[call_helper(12), exit_insn()]);
}

#[test]
fn verify_helper_13_get_battery_mv() {
    verify_helper(13, "get_battery_mv", &[call_helper(13), exit_insn()]);
}

#[test]
fn verify_helper_14_delay_us() {
    verify_helper(
        14,
        "delay_us",
        &[
            mov_imm(1, 1000), // r1 = microseconds
            call_helper(14),
            exit_insn(),
        ],
    );
}

#[test]
fn verify_helper_15_set_next_wake() {
    verify_helper(
        15,
        "set_next_wake",
        &[
            mov_imm(1, 60), // r1 = seconds
            call_helper(15),
            exit_insn(),
        ],
    );
}

// ---- Pointer + size helpers (handle, *buf, len) ----

/// Helper 1: i2c_read(handle, *buf, buf_len)
/// Arg types: [Anything, PtrToWritableMem, ConstSize, DontCare, DontCare]
#[test]
fn verify_helper_01_i2c_read() {
    verify_helper(
        1,
        "i2c_read",
        &[
            mov_imm(1, 0x48), // r1 = handle
            mov_reg(2, 10),   // r2 = r10
            add_imm(2, -8),   // r2 = r10 - 8 (stack buffer, 8 bytes)
            mov_imm(3, 8),    // r3 = 8 (buf_len)
            call_helper(1),
            exit_insn(),
        ],
    );
}

/// Helper 2: i2c_write(handle, *data, data_len)
/// Arg types: [Anything, PtrToReadableMem, ConstSize, DontCare, DontCare]
#[test]
fn verify_helper_02_i2c_write() {
    // Initialise stack so the readable pointer passes verification.
    let mut insns = init_stack(-8, 2); // 8 bytes at r10-8
    insns.extend_from_slice(&[
        mov_imm(1, 0x48), // r1 = handle
        mov_reg(2, 10),   // r2 = r10
        add_imm(2, -8),   // r2 = r10 - 8
        mov_imm(3, 8),    // r3 = 8 (data_len)
        call_helper(2),
        exit_insn(),
    ]);
    verify_helper(2, "i2c_write", &insns);
}

/// Helper 3: i2c_write_read(handle, *write_ptr, write_len, *read_ptr, read_len)
/// Arg types: [Anything, PtrToReadableMem, ConstSize, PtrToWritableMem, ConstSize]
#[test]
fn verify_helper_03_i2c_write_read() {
    verify_helper(
        3,
        "i2c_write_read",
        &[
            st_mem_w(10, -8, 0), // init write buffer (4 bytes at r10-8)
            mov_imm(1, 0x48),    // r1 = handle
            mov_reg(2, 10),      // r2 = r10
            add_imm(2, -8),      // r2 = write_ptr (r10-8)
            mov_imm(3, 4),       // r3 = write_len
            mov_reg(4, 10),      // r4 = r10
            add_imm(4, -16),     // r4 = read_ptr (r10-16)
            mov_imm(5, 4),       // r5 = read_len
            call_helper(3),
            exit_insn(),
        ],
    );
}

/// Helper 4: spi_transfer(handle, *buf, len)
/// Arg types: [Anything, PtrToWritableMem, ConstSize, DontCare, DontCare]
#[test]
fn verify_helper_04_spi_transfer() {
    verify_helper(
        4,
        "spi_transfer",
        &[
            mov_imm(1, 0),  // r1 = handle
            mov_reg(2, 10), // r2 = r10
            add_imm(2, -8), // r2 = r10 - 8 (in-place buffer)
            mov_imm(3, 8),  // r3 = len
            call_helper(4),
            exit_insn(),
        ],
    );
}

/// Helper 8: send(*ptr, len)
/// Arg types: [PtrToReadableMem, ConstSize, DontCare, DontCare, DontCare]
#[test]
fn verify_helper_08_send() {
    verify_helper(
        8,
        "send",
        &[
            st_mem_w(10, -8, 0), // init stack
            mov_reg(1, 10),      // r1 = r10
            add_imm(1, -8),      // r1 = r10 - 8
            mov_imm(2, 4),       // r2 = 4
            call_helper(8),
            exit_insn(),
        ],
    );
}

/// Helper 9: send_recv(*ptr, len, *reply_buf, reply_len, timeout_ms)
/// Arg types: [PtrToReadableMem, ConstSize, PtrToWritableMem, ConstSize, Anything]
#[test]
fn verify_helper_09_send_recv() {
    verify_helper(
        9,
        "send_recv",
        &[
            st_mem_w(10, -8, 0), // init send buffer (4 bytes at r10-8)
            mov_reg(1, 10),      // r1 = r10
            add_imm(1, -8),      // r1 = send_ptr
            mov_imm(2, 4),       // r2 = send_len
            mov_reg(3, 10),      // r3 = r10
            add_imm(3, -16),     // r3 = reply_ptr (r10-16)
            mov_imm(4, 4),       // r4 = reply_len
            mov_imm(5, 1000),    // r5 = timeout_ms
            call_helper(9),
            exit_insn(),
        ],
    );
}

/// Helper 16: bpf_trace_printk(*fmt, fmt_len)
/// Arg types: [PtrToReadableMem, ConstSize, DontCare, DontCare, DontCare]
#[test]
fn verify_helper_16_bpf_trace_printk() {
    verify_helper(
        16,
        "bpf_trace_printk",
        &[
            st_mem_w(10, -8, 0), // init stack (4 bytes)
            mov_reg(1, 10),      // r1 = r10
            add_imm(1, -8),      // r1 = fmt_ptr
            mov_imm(2, 4),       // r2 = fmt_len
            call_helper(16),
            exit_insn(),
        ],
    );
}

/// Helper 17: send_async(*ptr, len)
/// Arg types: [PtrToReadableMem, ConstSize, DontCare, DontCare, DontCare]
#[test]
fn verify_helper_17_send_async() {
    verify_helper(
        17,
        "send_async",
        &[
            st_mem_w(10, -8, 0), // init stack
            mov_reg(1, 10),      // r1 = r10
            add_imm(1, -8),      // r1 = ptr
            mov_imm(2, 4),       // r2 = len
            call_helper(17),
            exit_insn(),
        ],
    );
}

// ---- Map helpers (require map section in ELF — skipped) ----

/// Helper 10: map_lookup_elem(*map, *key) -> *value or null
///
/// Skipped: the Prevail verifier requires `PtrToMap` arguments to reference a
/// map descriptor declared in the ELF's `.maps` section. Constructing a valid
/// map ELF is complex and orthogonal to helper prototype coverage. The
/// prototype correctness is verified by the unit tests in `sonde_platform.rs`.
#[test]
#[ignore = "requires ELF map section for PtrToMap argument"]
fn verify_helper_10_map_lookup_elem() {
    // Placeholder — would need a map-capable ELF builder.
    verify_helper(
        10,
        "map_lookup_elem",
        &[mov_imm(1, 0), mov_imm(2, 0), call_helper(10), exit_insn()],
    );
}

/// Helper 11: map_update_elem(*map, *key, *value)
///
/// Skipped: same as helper 10 — requires a map section in the ELF.
#[test]
#[ignore = "requires ELF map section for PtrToMap argument"]
fn verify_helper_11_map_update_elem() {
    verify_helper(
        11,
        "map_update_elem",
        &[
            mov_imm(1, 0),
            mov_imm(2, 0),
            mov_imm(3, 0),
            call_helper(11),
            exit_insn(),
        ],
    );
}
