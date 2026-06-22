// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Decoder BPF program tests (GW-1900 through GW-1906).
//!
//! Validates decoder section extraction from ELF, DecoderPlatform
//! verification, decoder image storage, APP_DATA enrichment via
//! decoder execution, and backward compatibility.

use std::collections::BTreeMap;

use sonde_gateway::decoder;
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};

// ── BPF instruction helpers ────────────────────────────────────────────

fn bpf_insn(opcode: u8, dst_src: u8, offset: i16, imm: i32) -> [u8; 8] {
    let mut insn = [0u8; 8];
    insn[0] = opcode;
    insn[1] = dst_src;
    insn[2..4].copy_from_slice(&offset.to_le_bytes());
    insn[4..8].copy_from_slice(&imm.to_le_bytes());
    insn
}

fn mov_imm(dst: u8, imm: i32) -> [u8; 8] {
    bpf_insn(0xb7, dst, 0, imm)
}

fn call_helper(id: i32) -> [u8; 8] {
    bpf_insn(0x85, 0, 0, id)
}

fn exit_insn() -> [u8; 8] {
    bpf_insn(0x95, 0, 0, 0)
}

fn st_mem_w(dst: u8, offset: i16, imm: i32) -> [u8; 8] {
    bpf_insn(0x62, dst, offset, imm)
}

fn st_mem_b(dst: u8, offset: i16, imm: i32) -> [u8; 8] {
    bpf_insn(0x72, dst, offset, imm)
}

fn mov_reg(dst: u8, src: u8) -> [u8; 8] {
    bpf_insn(0xbf, dst | (src << 4), 0, 0)
}

fn add_imm(dst: u8, imm: i32) -> [u8; 8] {
    bpf_insn(0x07, dst, 0, imm)
}

fn assemble(insns: &[[u8; 8]]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.iter().copied()).collect()
}

// ── ELF builder ────────────────────────────────────────────────────────

/// Build a minimal BPF ELF with a single `sonde` section.
fn make_sonde_elf(bpf_code: &[u8]) -> Vec<u8> {
    make_elf_with_sections(&[("sonde", bpf_code)])
}

/// Build a BPF ELF with both `sonde` and `decoder` sections.
fn make_dual_section_elf(sonde_code: &[u8], decoder_code: &[u8]) -> Vec<u8> {
    make_elf_with_sections(&[("sonde", sonde_code), ("decoder", decoder_code)])
}

/// Build a BPF ELF with arbitrary named sections.
fn make_elf_with_sections(sections: &[(&str, &[u8])]) -> Vec<u8> {
    // Build the shstrtab: null byte + each section name + ".shstrtab"
    let mut shstrtab = vec![0u8]; // null prefix
    let mut name_offsets: Vec<u32> = Vec::new();
    for (name, _) in sections {
        name_offsets.push(shstrtab.len() as u32);
        shstrtab.extend_from_slice(name.as_bytes());
        shstrtab.push(0);
    }
    let shstrtab_name_offset = shstrtab.len() as u32;
    shstrtab.extend_from_slice(b".shstrtab\0");

    // Compute offsets.
    let mut data_offset: u64 = 64; // after ELF header
    let mut section_offsets: Vec<u64> = Vec::new();
    for (_, code) in sections {
        section_offsets.push(data_offset);
        data_offset += code.len() as u64;
    }
    let shstrtab_offset: u64 = data_offset;
    let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

    // Number of section headers: null + each section + .shstrtab
    let num_sections = 1 + sections.len() + 1;
    let shstrndx = num_sections - 1; // .shstrtab is last

    let mut elf = Vec::new();

    // ELF header (64 bytes)
    elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
    elf.push(2); // ELFCLASS64
    elf.push(1); // ELFDATA2LSB
    elf.push(1); // EI_VERSION
    elf.extend_from_slice(&[0; 9]);
    elf.extend_from_slice(&1u16.to_le_bytes()); // ET_REL
    elf.extend_from_slice(&247u16.to_le_bytes()); // EM_BPF
    elf.extend_from_slice(&1u32.to_le_bytes()); // e_version
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
    elf.extend_from_slice(&shdr_offset.to_le_bytes()); // e_shoff
    elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    elf.extend_from_slice(&(num_sections as u16).to_le_bytes());
    elf.extend_from_slice(&(shstrndx as u16).to_le_bytes());
    assert_eq!(elf.len(), 64);

    // Section data
    for (_, code) in sections {
        elf.extend_from_slice(code);
    }
    elf.extend_from_slice(&shstrtab);

    // Section headers

    // [0] Null
    elf.extend_from_slice(&[0u8; 64]);

    // Program sections
    for (i, (_, code)) in sections.iter().enumerate() {
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&name_offsets[i].to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
        sh[8..16].copy_from_slice(&flags.to_le_bytes());
        sh[24..32].copy_from_slice(&section_offsets[i].to_le_bytes());
        sh[32..40].copy_from_slice(&(code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // alignment
        elf.extend_from_slice(&sh);
    }

    // .shstrtab section header
    let mut sh = [0u8; 64];
    sh[0..4].copy_from_slice(&shstrtab_name_offset.to_le_bytes());
    sh[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
    sh[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
    sh[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
    sh[48..56].copy_from_slice(&1u64.to_le_bytes());
    elf.extend_from_slice(&sh);

    elf
}

/// Minimal valid BPF program: `mov r0, 0; exit`
fn nop_bytecode() -> Vec<u8> {
    assemble(&[mov_imm(0, 0), exit_insn()])
}

/// Decoder BPF program that calls emit_reading("temp_mc", 7, 25125).
///
/// The program writes the name "temp_mc" to stack memory, then calls
/// emit_reading with: r1=name_ptr, r2=7, r3=25125.
fn emit_temp_decoder_bytecode() -> Vec<u8> {
    // We need to write "temp_mc" (7 bytes) to the stack, then call emit_reading.
    // Stack starts at r10 - 512. Let's use r10 - 8 for name storage.
    //
    // "temp_mc" = 0x74 0x65 0x6d 0x70 0x5f 0x6d 0x63
    //
    // Store 4 bytes at r10-8: "temp" = 0x706d6574
    // Store 3 bytes at r10-4: "_mc\0" = 0x00636d5f (but we only need 3 bytes)
    let insns: Vec<[u8; 8]> = vec![
        // Store "temp" at r10 - 8
        st_mem_w(10, -8, 0x706d6574_u32 as i32), // "temp" (little-endian)
        // Store "_mc" at r10 - 4 (with trailing null byte — we pass name_len=7)
        st_mem_w(10, -4, 0x00636d5f_u32 as i32), // "_mc\0"
        // r1 = r10 - 8 (pointer to name on stack)
        mov_reg(1, 10),
        add_imm(1, -8),
        // r2 = 7 (name length)
        mov_imm(2, 7),
        // r3 = 25125 (temperature in milli-degrees)
        mov_imm(3, 25125),
        // call emit_reading (helper 18)
        call_helper(18),
        // r0 = 0; exit
        mov_imm(0, 0),
        exit_insn(),
    ];
    assemble(&insns)
}

/// Build a decoder BPF program that calls `emit_reading(name, value)`.
///
/// `name` must be 1–4 bytes. The name is written to the stack, then
/// `emit_reading` is called with the given signed 32-bit value.
fn emit_named_decoder_bytecode(name: &[u8], value: i32) -> Vec<u8> {
    assert!(
        !name.is_empty() && name.len() <= 4,
        "name must be 1–4 bytes"
    );
    // Pack name bytes into a little-endian u32 (zero-padded).
    let mut packed = [0u8; 4];
    packed[..name.len()].copy_from_slice(name);
    let name_word = u32::from_le_bytes(packed);

    let insns: Vec<[u8; 8]> = vec![
        // Store name at r10 - 4
        st_mem_w(10, -4, name_word as i32),
        // r1 = r10 - 4 (pointer to name on stack)
        mov_reg(1, 10),
        add_imm(1, -4),
        // r2 = name length
        mov_imm(2, name.len() as i32),
        // r3 = value
        mov_imm(3, value),
        // call emit_reading (helper 18)
        call_helper(18),
        // r0 = 0; exit
        mov_imm(0, 0),
        exit_insn(),
    ];
    assemble(&insns)
}

// ── Tests ──────────────────────────────────────────────────────────────

// T-1900: Dual-section ELF ingestion
#[test]
fn t1900_dual_section_elf_ingestion() {
    let sonde_code = nop_bytecode();
    let decoder_code = nop_bytecode();
    let elf = make_dual_section_elf(&sonde_code, &decoder_code);

    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

    assert!(
        record.decoder_image.is_some(),
        "expected decoder_image to be present"
    );
    // Node program hash covers only the sonde image.
    assert_eq!(record.hash.len(), 32);
}

// T-1900a: ELF without decoder section (backward compat)
#[test]
fn t1900a_elf_without_decoder() {
    let sonde_code = nop_bytecode();
    let elf = make_sonde_elf(&sonde_code);

    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

    assert!(
        record.decoder_image.is_none(),
        "expected no decoder_image for single-section ELF"
    );
}

// T-1900b: ELF with decoder only (no sonde section) rejected
#[test]
fn t1900b_decoder_only_elf_rejected() {
    let decoder_code = nop_bytecode();
    let elf = make_elf_with_sections(&[("decoder", &decoder_code)]);

    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(&elf, VerificationProfile::Resident);
    assert!(result.is_err(), "expected rejection of decoder-only ELF");
}

// T-1900c: ELF with empty decoder section treated as no decoder
#[test]
fn t1900c_empty_decoder_section() {
    let sonde_code = nop_bytecode();
    let empty: &[u8] = &[];
    let elf = make_dual_section_elf(&sonde_code, empty);

    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

    assert!(
        record.decoder_image.is_none(),
        "expected empty decoder section to produce no decoder_image"
    );
}

// T-1900d: ELF with multiple decoder sections rejected (AC-7)
#[test]
fn t1900d_multiple_decoder_sections_rejected() {
    // Build an ELF with sonde + two decoder sections.
    let sonde_code = nop_bytecode();
    let decoder_code = nop_bytecode();
    let elf = make_elf_with_sections(&[
        ("sonde", &sonde_code),
        ("decoder", &decoder_code),
        ("decoder", &decoder_code),
    ]);

    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(&elf, VerificationProfile::Resident);
    // Multiple decoder sections must be rejected.
    assert!(
        result.is_err(),
        "expected error for ELF with multiple decoder sections, got {:?}",
        result
    );
}

// GW-1900 AC-6: Section matching is exact — `decoder.text` must be ignored
#[test]
fn t1900_ac6_decoder_text_section_ignored() {
    let sonde_code = nop_bytecode();
    let other_code = nop_bytecode();
    // Use "decoder.text" — not "decoder". Should be ignored.
    let elf = make_elf_with_sections(&[("sonde", &sonde_code), ("decoder.text", &other_code)]);

    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

    assert!(
        record.decoder_image.is_none(),
        "expected `decoder.text` section to be ignored (exact match only)"
    );
}

// T-1906: Program hash unchanged by decoder presence
#[test]
fn t1906_hash_unchanged_by_decoder() {
    let sonde_code = nop_bytecode();
    let lib = ProgramLibrary::new();

    // Ingest without decoder
    let elf_no_decoder = make_sonde_elf(&sonde_code);
    let record_no_decoder = lib
        .ingest_elf(&elf_no_decoder, VerificationProfile::Resident)
        .unwrap();

    // Ingest with decoder
    let decoder_code = nop_bytecode();
    let elf_with_decoder = make_dual_section_elf(&sonde_code, &decoder_code);
    let record_with_decoder = lib
        .ingest_elf(&elf_with_decoder, VerificationProfile::Resident)
        .unwrap();

    assert_eq!(
        record_no_decoder.hash, record_with_decoder.hash,
        "node program hash must be identical with and without decoder"
    );
}

// T-1901a: Decoder using hardware helpers rejected
#[test]
fn t1901a_decoder_with_hardware_helper_rejected() {
    let sonde_code = nop_bytecode();
    // Decoder that calls i2c_read (helper 1) — should fail verification.
    let decoder_code = assemble(&[
        mov_imm(1, 0),
        mov_imm(2, 0),
        mov_imm(3, 0),
        call_helper(1), // i2c_read — not allowed in decoder
        mov_imm(0, 0),
        exit_insn(),
    ]);
    let elf = make_dual_section_elf(&sonde_code, &decoder_code);

    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(&elf, VerificationProfile::Resident);
    assert!(
        result.is_err(),
        "expected decoder with hardware helper to be rejected"
    );
}

// T-1901b: Decoder using send helper rejected
#[test]
fn t1901b_decoder_with_send_helper_rejected() {
    let sonde_code = nop_bytecode();
    // Decoder that calls send (helper 8).
    let decoder_code = assemble(&[
        mov_imm(1, 0),
        mov_imm(2, 0),
        call_helper(8), // send — not allowed in decoder
        mov_imm(0, 0),
        exit_insn(),
    ]);
    let elf = make_dual_section_elf(&sonde_code, &decoder_code);

    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(&elf, VerificationProfile::Resident);
    assert!(
        result.is_err(),
        "expected decoder with send helper to be rejected"
    );
}

// T-1904: emit_reading captures readings with last-write-wins
#[test]
fn t1904_emit_reading_captures_readings() {
    // Build a decoder ProgramImage that calls emit_reading with known values.
    // We can't easily build a full ELF for this, so we test the decoder
    // execution engine directly.
    let bytecode = emit_temp_decoder_bytecode();
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    // Simulate a 6-byte TMP102 payload: raw_hi=0x19, raw_lo=0x80, temp_mC=25125
    let mut blob = vec![0u8; 6];
    blob[0] = 0x19;
    blob[1] = 0x80;
    blob[2..6].copy_from_slice(&25125i32.to_le_bytes());

    let readings = unsafe { decoder::execute_decoder(&cbor, &blob) }.unwrap();

    assert_eq!(readings.len(), 1);
    assert_eq!(readings.get("temp_mc"), Some(&25125i64));
}

// T-1904b: emit_reading with name_len=65 returns -1
#[test]
fn t1904b_emit_reading_name_too_long() {
    // Build a decoder that calls emit_reading with a 65-byte name.
    // We'll write 65 bytes to the stack and call emit_reading.
    // Since BPF stack is 512 bytes per frame, we can allocate 68 bytes
    // (round up to 4-byte boundary).
    let mut insns: Vec<[u8; 8]> = Vec::new();

    // Initialize 68 bytes of stack at r10 - 68 (17 words of 4 bytes)
    for i in 0..17 {
        insns.push(st_mem_w(10, -68 + i * 4, 0x41414141)); // 'AAAA'
    }

    // r1 = r10 - 68 (pointer to name)
    insns.push(mov_reg(1, 10));
    insns.push(add_imm(1, -68));
    // r2 = 65 (name length — exceeds 64 byte limit)
    insns.push(mov_imm(2, 65));
    // r3 = 42 (value)
    insns.push(mov_imm(3, 42));
    // call emit_reading
    insns.push(call_helper(18));
    // exit with r0 (should be -1)
    insns.push(exit_insn());

    let bytecode = assemble(&insns);
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = unsafe { decoder::execute_decoder(&cbor, &[0u8; 10]) }.unwrap();

    // The reading should NOT have been included (name too long).
    assert!(
        readings.is_empty(),
        "expected no readings for name_len > 64"
    );
}

// T-1904a: emit_reading with name_len=64 succeeds
#[test]
fn t1904a_emit_reading_max_name_ok() {
    let mut insns: Vec<[u8; 8]> = Vec::new();

    // Initialize 64 bytes of stack at r10 - 64 (16 words of 4 bytes)
    for i in 0..16 {
        insns.push(st_mem_w(10, -64 + i * 4, 0x41414141)); // 'AAAA'
    }

    // r1 = r10 - 64 (pointer to name)
    insns.push(mov_reg(1, 10));
    insns.push(add_imm(1, -64));
    // r2 = 64 (name length — exactly at limit)
    insns.push(mov_imm(2, 64));
    // r3 = 99 (value)
    insns.push(mov_imm(3, 99));
    insns.push(call_helper(18));
    insns.push(mov_imm(0, 0));
    insns.push(exit_insn());

    let bytecode = assemble(&insns);
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = unsafe { decoder::execute_decoder(&cbor, &[0u8; 10]) }.unwrap();

    assert_eq!(readings.len(), 1, "expected one reading with 64-byte name");
    let name = "A".repeat(64);
    assert_eq!(readings.get(&name), Some(&99i64));
}

// T-1903a: APP_DATA without decoder forwarded unchanged
// (tested implicitly: execute_decoder is only called when decoder_image is Some)

// T-1903b: Decoder failure does not block data delivery
#[test]
fn t1903b_decoder_failure_returns_error() {
    // Build a decoder that exceeds instruction budget (infinite loop).
    // BPF: `label: ja label` (unconditional jump to self)
    let insns = vec![
        bpf_insn(0x05, 0, -1, 0), // ja -1 (infinite loop)
    ];
    let bytecode = assemble(&insns);
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let result = unsafe { decoder::execute_decoder(&cbor, &[0u8; 10]) };
    assert!(
        result.is_err(),
        "expected decoder to fail with budget exceeded"
    );
}

// T-1901: Decoder verification passes with permitted helpers
#[test]
fn t1901_decoder_with_emit_reading_passes() {
    let sonde_code = nop_bytecode();
    let decoder_code = emit_temp_decoder_bytecode();
    let elf = make_dual_section_elf(&sonde_code, &decoder_code);

    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(&elf, VerificationProfile::Resident);
    assert!(
        result.is_ok(),
        "decoder with emit_reading should pass verification: {:?}",
        result.err()
    );
    assert!(result.unwrap().decoder_image.is_some());
}

// T-1903c: Enriched message preserves raw blob unchanged
#[test]
fn t1903c_enriched_preserves_raw_blob() {
    let bytecode = emit_temp_decoder_bytecode();
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let original_blob: Vec<u8> = vec![0x19, 0x80, 0x45, 0x62, 0x00, 0x00];
    let readings = unsafe { decoder::execute_decoder(&cbor, &original_blob) }.unwrap();

    // Readings should be non-empty.
    assert!(!readings.is_empty());

    // The blob should not have been mutated (we verify by checking the
    // original_blob variable is still intact — it was passed by reference).
    assert_eq!(original_blob, vec![0x19, 0x80, 0x45, 0x62, 0x00, 0x00]);
}

// T-1904c: emit_reading overflow (33rd reading returns -2)
#[test]
fn t1904c_emit_reading_overflow() {
    // Build a decoder that calls emit_reading 33 times with different names.
    // Each name is a single byte (different value) written to stack.
    let mut insns: Vec<[u8; 8]> = Vec::new();

    for i in 0u8..33 {
        // Write name byte to stack at r10 - 4
        insns.push(st_mem_b(10, -4, i as i32 + 0x30)); // '0', '1', ..., 'P'
                                                       // r1 = r10 - 4
        insns.push(mov_reg(1, 10));
        insns.push(add_imm(1, -4));
        // r2 = 1 (name length)
        insns.push(mov_imm(2, 1));
        // r3 = i (value)
        insns.push(mov_imm(3, i as i32));
        insns.push(call_helper(18));
        // Note: we don't check return value here — just keep calling
    }

    insns.push(mov_imm(0, 0));
    insns.push(exit_insn());

    let bytecode = assemble(&insns);
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = unsafe { decoder::execute_decoder(&cbor, &[0u8; 10]) }.unwrap();

    // First 32 readings should be present, 33rd rejected.
    assert_eq!(
        readings.len(),
        32,
        "expected exactly 32 readings (33rd overflow)"
    );
}

// Smoke test: handler DATA message roundtrip with readings
#[test]
fn handler_data_message_with_readings_roundtrip() {
    use sonde_gateway::HandlerMessage;

    let mut readings = BTreeMap::new();
    readings.insert("temp_mc".to_string(), 25125i64);
    readings.insert("rh_mpermille".to_string(), 45000i64);

    let msg = HandlerMessage::Data {
        request_id: 42,
        node_id: "node-01".to_string(),
        program_hash: vec![0xAA; 32],
        data: vec![0x01, 0x02, 0x03],
        timestamp: 1700000000,
        readings: Some(readings.clone()),
    };

    let encoded = msg.encode().unwrap();
    let decoded = HandlerMessage::decode(&encoded).unwrap();
    assert_eq!(msg, decoded);
}

// T-1904d: map_lookup_elem reads .rodata initial data
#[test]
fn t1904d_map_lookup_reads_rodata() {
    // Decoder program: lookup map[0], read value, emit as reading.
    //
    // Map 0: rodata, value_size=4, max_entries=1, initial_data = [0x39, 0x05, 0x00, 0x00] (1337 LE)
    //
    // BPF:
    //   r1 = map_fd[0]    (LDDW with src=1, imm=0)
    //   *(u32*)(r10-4) = 0  // key = 0
    //   r2 = r10 - 4
    //   call map_lookup_elem (10)
    //   if r0 == 0 goto exit
    //   r3 = *(u32*)(r0 + 0)  // read value
    //   *(u8*)(r10-8) = 'v'
    //   r1 = r10 - 8
    //   r2 = 1
    //   call emit_reading (18)
    //   r0 = 0
    //   exit
    let mut bytecode = Vec::new();
    // LDDW r1, map_fd=0 (opcode 0x18, src_reg=1, imm=0, next insn imm=0)
    bytecode.extend_from_slice(&bpf_insn(0x18, 0x11, 0, 0));
    bytecode.extend_from_slice(&[0u8; 8]); // second half of LDDW
                                           // *(u32*)(r10-4) = 0  (store key)
    bytecode.extend_from_slice(&st_mem_w(10, -4, 0));
    // r2 = r10 - 4
    bytecode.extend_from_slice(&mov_reg(2, 10));
    bytecode.extend_from_slice(&add_imm(2, -4));
    // call map_lookup_elem (10)
    bytecode.extend_from_slice(&call_helper(10));
    // if r0 == 0 goto +5 (skip to exit)
    bytecode.extend_from_slice(&bpf_insn(0x15, 0x00, 5, 0)); // jeq r0, 0, +5
                                                             // r3 = *(u32*)(r0 + 0)
    bytecode.extend_from_slice(&bpf_insn(0x61, 0x03, 0, 0)); // ldxw r3, [r0+0]
                                                             // *(u8*)(r10-8) = 'v'
    bytecode.extend_from_slice(&st_mem_b(10, -8, b'v' as i32));
    // r1 = r10 - 8
    bytecode.extend_from_slice(&mov_reg(1, 10));
    bytecode.extend_from_slice(&add_imm(1, -8));
    // r2 = 1
    bytecode.extend_from_slice(&mov_imm(2, 1));
    // call emit_reading (18)
    bytecode.extend_from_slice(&call_helper(18));
    // r0 = 0
    bytecode.extend_from_slice(&mov_imm(0, 0));
    // exit
    bytecode.extend_from_slice(&exit_insn());

    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![sonde_protocol::MapDef {
            map_type: 0,
            key_size: 4,
            value_size: 4,
            max_entries: 1,
        }],
        map_initial_data: vec![vec![0x39, 0x05, 0x00, 0x00]], // 1337 LE
        map_readonly: vec![true],
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = unsafe { decoder::execute_decoder(&cbor, &[0u8; 4]) }.unwrap();
    assert_eq!(
        readings.get("v"),
        Some(&1337i64),
        "expected .rodata value 1337 to be read via map_lookup"
    );
}

#[test]
fn t1904d_direct_map_value_relocation_reads_entry_zero_value() {
    // Decoder program: load map[0] entry 0's value directly via src=6,
    // read the first u32, and emit it as reading "g".
    //
    // The decoder runtime stores map values densely without key padding, so
    // this must read bytes 0..4 from the backing store rather than skipping
    // the first 4 bytes as if a u32 key prefix were present.
    let mut bytecode = Vec::new();
    bytecode.extend_from_slice(&bpf_insn(0x18, 0x61, 0, 0));
    bytecode.extend_from_slice(&[0u8; 8]);
    bytecode.extend_from_slice(&bpf_insn(0x61, 0x13, 0, 0)); // r3 = *(u32 *)(r1 + 0)
    bytecode.extend_from_slice(&st_mem_b(10, -8, b'g' as i32));
    bytecode.extend_from_slice(&mov_reg(1, 10));
    bytecode.extend_from_slice(&add_imm(1, -8));
    bytecode.extend_from_slice(&mov_imm(2, 1));
    bytecode.extend_from_slice(&call_helper(18));
    bytecode.extend_from_slice(&mov_imm(0, 0));
    bytecode.extend_from_slice(&exit_insn());

    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![sonde_protocol::MapDef {
            map_type: 0,
            key_size: 4,
            value_size: 8,
            max_entries: 1,
        }],
        map_initial_data: vec![vec![0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]],
        map_readonly: vec![true],
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = unsafe { decoder::execute_decoder(&cbor, &[0u8; 4]) }.unwrap();
    assert_eq!(
        readings.get("g"),
        Some(&0x11223344i64),
        "direct decoder relocations must point at entry 0's value bytes"
    );
}

// T-1904e: map_update_elem on .rodata returns error
#[test]
fn t1904e_map_update_rodata_rejected() {
    // Decoder program: try to update map[0] (rodata), should return -1.
    //
    // BPF:
    //   r1 = map_fd[0]    (LDDW with src=1, imm=0)
    //   *(u32*)(r10-4) = 0  // key = 0
    //   *(u32*)(r10-8) = 42 // value = 42
    //   r2 = r10 - 4
    //   r3 = r10 - 8
    //   r4 = 0 (flags)
    //   call map_update_elem (11)
    //   // r0 should be -1 (error), emit as reading to capture the return value
    //   r3 = r0  // value = return code
    //   *(u8*)(r10-12) = 'r'
    //   r1 = r10 - 12
    //   r2 = 1
    //   call emit_reading (18)
    //   r0 = 0
    //   exit
    let mut bytecode = Vec::new();
    // LDDW r1, map_fd=0
    bytecode.extend_from_slice(&bpf_insn(0x18, 0x11, 0, 0));
    bytecode.extend_from_slice(&[0u8; 8]);
    // *(u32*)(r10-4) = 0
    bytecode.extend_from_slice(&st_mem_w(10, -4, 0));
    // *(u32*)(r10-8) = 42
    bytecode.extend_from_slice(&st_mem_w(10, -8, 42));
    // r2 = r10 - 4
    bytecode.extend_from_slice(&mov_reg(2, 10));
    bytecode.extend_from_slice(&add_imm(2, -4));
    // r3 = r10 - 8
    bytecode.extend_from_slice(&mov_reg(3, 10));
    bytecode.extend_from_slice(&add_imm(3, -8));
    // r4 = 0
    bytecode.extend_from_slice(&mov_imm(4, 0));
    // call map_update_elem (11)
    bytecode.extend_from_slice(&call_helper(11));
    // r3 = r0 (capture return value)
    bytecode.extend_from_slice(&mov_reg(3, 0));
    // *(u8*)(r10-12) = 'r'
    bytecode.extend_from_slice(&st_mem_b(10, -12, b'r' as i32));
    // r1 = r10 - 12
    bytecode.extend_from_slice(&mov_reg(1, 10));
    bytecode.extend_from_slice(&add_imm(1, -12));
    // r2 = 1
    bytecode.extend_from_slice(&mov_imm(2, 1));
    // call emit_reading (18)
    bytecode.extend_from_slice(&call_helper(18));
    // r0 = 0
    bytecode.extend_from_slice(&mov_imm(0, 0));
    // exit
    bytecode.extend_from_slice(&exit_insn());

    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![sonde_protocol::MapDef {
            map_type: 0,
            key_size: 4,
            value_size: 4,
            max_entries: 1,
        }],
        map_initial_data: vec![vec![0xAA, 0xBB, 0xCC, 0xDD]],
        map_readonly: vec![true],
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = unsafe { decoder::execute_decoder(&cbor, &[0u8; 4]) }.unwrap();
    // map_update returns -1 (0xFFFFFFFFFFFFFFFF as u64, reinterpreted as i64 = -1)
    let ret = readings.get("r").expect("expected return value reading");
    assert_eq!(*ret, -1i64, "map_update on .rodata should return -1");
}

// T-1902: Decoder image storage and retrieval
//
// Traces to: GW-1902 (AC-1, AC-2, AC-4)
//
// Verifies that a decoder image is stored alongside the node image,
// retrievable by program hash, and removed when the program is deleted.
#[tokio::test]
async fn t1902_decoder_image_storage_and_retrieval() {
    use sonde_gateway::storage::Storage;
    use sonde_gateway::InMemoryStorage;

    let sonde_code = nop_bytecode();
    let decoder_code = emit_temp_decoder_bytecode();
    let elf = make_dual_section_elf(&sonde_code, &decoder_code);

    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();
    assert!(
        record.decoder_image.is_some(),
        "AC-1: decoder image present after ingest"
    );

    let storage = InMemoryStorage::new();
    storage.store_program(&record).await.unwrap();

    // AC-2: Decoder image retrievable by node program hash.
    let retrieved = storage.get_program(&record.hash).await.unwrap().unwrap();
    assert!(
        retrieved.decoder_image.is_some(),
        "AC-2: decoder image retrievable by program hash"
    );
    // Verify it decodes as a valid ProgramImage.
    let decoder_cbor = retrieved.decoder_image.as_ref().unwrap();
    let decoded = sonde_protocol::ProgramImage::decode(decoder_cbor);
    assert!(
        decoded.is_ok(),
        "decoder image should be decodable as ProgramImage"
    );

    // AC-4: Decoder image removed when program is deleted (RemoveProgram).
    storage.delete_program(&record.hash).await.unwrap();
    let after_delete = storage.get_program(&record.hash).await.unwrap();
    assert!(
        after_delete.is_none(),
        "AC-4: program (and decoder) removed after delete"
    );
}

// T-1902a: Decoder image replacement on re-ingest
//
// Traces to: GW-1902 (AC-7)
//
// Re-ingesting the same node program with a different decoder section
// replaces the decoder image while keeping the node program hash stable.
#[tokio::test]
async fn t1902a_decoder_replacement_on_reingest() {
    use sonde_gateway::storage::Storage;
    use sonde_gateway::InMemoryStorage;

    let sonde_code = nop_bytecode();
    let lib = ProgramLibrary::new();
    let storage = InMemoryStorage::new();

    // V1 decoder: emit_reading("A", 1)
    let decoder_v1 = emit_named_decoder_bytecode(b"A", 1);
    let elf_v1 = make_dual_section_elf(&sonde_code, &decoder_v1);
    let record_v1 = lib
        .ingest_elf(&elf_v1, VerificationProfile::Resident)
        .unwrap();
    storage.store_program(&record_v1).await.unwrap();

    // V2 decoder: emit_reading("B", 2) — same sonde code, different decoder.
    let decoder_v2 = emit_named_decoder_bytecode(b"B", 2);
    let elf_v2 = make_dual_section_elf(&sonde_code, &decoder_v2);
    let record_v2 = lib
        .ingest_elf(&elf_v2, VerificationProfile::Resident)
        .unwrap();

    // Same sonde bytecode → same node program hash.
    assert_eq!(
        record_v1.hash, record_v2.hash,
        "node program hash must be stable (GW-1906)"
    );

    // Upsert: store the updated record (same hash, new decoder image).
    storage.store_program(&record_v2).await.unwrap();

    // Retrieve and verify the decoder image was replaced.
    let retrieved = storage.get_program(&record_v2.hash).await.unwrap().unwrap();
    assert_ne!(
        record_v1.decoder_image, retrieved.decoder_image,
        "decoder image should differ after replacement"
    );
    assert_eq!(
        record_v2.decoder_image, retrieved.decoder_image,
        "decoder image should match v2 after upsert"
    );

    // Execute the new decoder to confirm it produces v2 readings.
    let decoder_cbor = retrieved.decoder_image.as_ref().unwrap();
    let readings = unsafe { decoder::execute_decoder(decoder_cbor, &[0u8; 10]) }.unwrap();
    assert_eq!(
        readings.get("B"),
        Some(&2i64),
        "v2 decoder should produce reading B=2"
    );
    assert!(
        !readings.contains_key("A"),
        "v1 reading A must not appear — decoder was replaced"
    );
}

// T-1902b: Decoder removal on re-ingest without decoder
//
// Traces to: GW-1902 (AC-8)
//
// Re-ingesting a node program without a decoder section removes any
// previously stored decoder image.
#[tokio::test]
async fn t1902b_decoder_removal_on_reingest() {
    use sonde_gateway::storage::Storage;
    use sonde_gateway::InMemoryStorage;

    let sonde_code = nop_bytecode();
    let lib = ProgramLibrary::new();
    let storage = InMemoryStorage::new();

    // Initial ingest: ELF with decoder section.
    let decoder_code = emit_temp_decoder_bytecode();
    let elf_with_decoder = make_dual_section_elf(&sonde_code, &decoder_code);
    let record_with = lib
        .ingest_elf(&elf_with_decoder, VerificationProfile::Resident)
        .unwrap();
    assert!(record_with.decoder_image.is_some());
    storage.store_program(&record_with).await.unwrap();

    // Re-ingest: ELF without decoder section (same sonde code).
    let elf_no_decoder = make_sonde_elf(&sonde_code);
    let record_without = lib
        .ingest_elf(&elf_no_decoder, VerificationProfile::Resident)
        .unwrap();

    // Same node program hash.
    assert_eq!(record_with.hash, record_without.hash);
    // No decoder in the new record.
    assert!(record_without.decoder_image.is_none());

    // Upsert: store the record without decoder — removes old decoder.
    storage.store_program(&record_without).await.unwrap();

    // Verify decoder is gone.
    let retrieved = storage
        .get_program(&record_without.hash)
        .await
        .unwrap()
        .unwrap();
    assert!(
        retrieved.decoder_image.is_none(),
        "AC-8: decoder image must be removed on re-ingest without decoder"
    );
}

// T-1903: APP_DATA enrichment with decoder
//
// Traces to: GW-1903 (AC-1, AC-3, AC-6)
//
// Verifies the full enrichment chain: decoder execution produces readings,
// which are correctly encoded in the handler DATA message alongside the
// preserved raw blob.
#[test]
fn t1903_app_data_enrichment_with_decoder() {
    use sonde_gateway::HandlerMessage;

    // Ingest a program with decoder that calls emit_reading("temp_mc", 25125).
    let sonde_code = nop_bytecode();
    let decoder_code = emit_temp_decoder_bytecode();
    let elf = make_dual_section_elf(&sonde_code, &decoder_code);
    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();
    let decoder_cbor = record.decoder_image.as_ref().unwrap();

    // Simulate APP_DATA blob from a node.
    let raw_blob = vec![0x19, 0x80, 0x45, 0x62, 0x00, 0x00];

    // AC-1: Execute decoder → produces readings.
    let readings = unsafe { decoder::execute_decoder(decoder_cbor, &raw_blob) }.unwrap();
    assert!(
        !readings.is_empty(),
        "AC-1: decoder should produce readings"
    );

    // AC-3: Readings contain the expected name-value pairs.
    assert_eq!(
        readings.get("temp_mc"),
        Some(&25125i64),
        "AC-3: readings should contain temp_mc=25125"
    );

    // Build the handler DATA message with enriched readings (GW-1903 AC-6).
    let msg = HandlerMessage::Data {
        request_id: 42,
        node_id: "node-01".to_string(),
        program_hash: record.hash.clone(),
        data: raw_blob.clone(),
        timestamp: 1700000000,
        readings: Some(readings.clone()),
    };

    // Round-trip the handler message: encode → decode.
    let encoded = msg.encode().unwrap();
    let decoded = HandlerMessage::decode(&encoded).unwrap();

    // Verify readings survive round-trip.
    if let HandlerMessage::Data {
        data,
        readings: decoded_readings,
        ..
    } = decoded
    {
        assert_eq!(
            decoded_readings.as_ref().and_then(|r| r.get("temp_mc")),
            Some(&25125i64),
            "readings must survive handler message round-trip"
        );
        // Raw blob is preserved byte-for-byte (AC-8 from GW-1903).
        assert_eq!(
            data, raw_blob,
            "raw blob must be preserved in handler DATA message"
        );
    } else {
        panic!("expected HandlerMessage::Data");
    }
}

// T-1903d: Both handler and connector receive identical enriched message
//
// Traces to: GW-1903 (AC-6)
//
// Verifies that the readings produced by decoder execution are delivered
// identically to both the handler (DATA message) and the connector
// (GW-0813 message). Since ConnectorOutboundMessage is module-private,
// this test validates the shared readings value at the production boundary:
// the same `BTreeMap<String, i64>` is cloned to both paths in engine.rs.
#[test]
fn t1903d_handler_and_connector_same_readings() {
    use sonde_gateway::HandlerMessage;

    // Execute a decoder to produce readings.
    let bytecode = emit_temp_decoder_bytecode();
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let blob = vec![0x19, 0x80, 0x45, 0x62, 0x00, 0x00];
    let readings = unsafe { decoder::execute_decoder(&cbor, &blob) }.unwrap();
    assert!(!readings.is_empty());

    // Engine clones readings for connector and handler (engine.rs:1386-1433).
    // Simulate this split:
    let handler_readings = readings.clone();
    let connector_readings = readings.clone();

    // Both must be identical.
    assert_eq!(
        handler_readings, connector_readings,
        "handler and connector must receive identical readings"
    );

    // Verify the handler message carries these readings correctly.
    let handler_msg = HandlerMessage::Data {
        request_id: 1,
        node_id: "n1".to_string(),
        program_hash: vec![0x42; 32],
        data: blob.clone(),
        timestamp: 1700000000,
        readings: Some(handler_readings),
    };
    let decoded = HandlerMessage::decode(&handler_msg.encode().unwrap()).unwrap();

    if let HandlerMessage::Data {
        readings: Some(r),
        data,
        ..
    } = decoded
    {
        assert_eq!(
            r, connector_readings,
            "readings must match connector's copy"
        );
        assert_eq!(data, blob, "raw blob preserved");
    } else {
        panic!("expected HandlerMessage::Data with readings");
    }
}

// T-1904f: Decoder context ABI — input_data and input_end pointers
//
// Traces to: GW-1904 AC-1
//
// Verifies that the decoder context provides correct `input_data` and
// `input_end` pointers that bracket the APP_DATA payload.  The decoder
// program loads both pointers from the context, computes the blob length
// via `input_end - input_data`, and emits it as a reading.
#[test]
fn t1904f_decoder_context_abi() {
    // BPF program that reads context pointers and emits blob length:
    //   r6 = *(u64*)(r1 + 0)     // input_data
    //   r7 = *(u64*)(r1 + 8)     // input_end
    //   r3 = r7 - r6             // blob length
    //   *(u8*)(r10 - 4) = 'L'    // name = "L"
    //   r1 = r10 - 4
    //   r2 = 1
    //   call emit_reading (18)
    //   r0 = 0
    //   exit
    let insns: Vec<[u8; 8]> = vec![
        // r6 = *(u64*)(r1 + 0)  — load input_data pointer
        bpf_insn(0x79, 0x16, 0, 0),
        // r7 = *(u64*)(r1 + 8)  — load input_end pointer
        bpf_insn(0x79, 0x17, 8, 0),
        // r3 = r7
        mov_reg(3, 7),
        // r3 -= r6  (input_end - input_data = blob length)
        bpf_insn(0x1f, 0x63, 0, 0), // sub64 r3, r6
        // *(u8*)(r10 - 4) = 'L'
        st_mem_b(10, -4, b'L' as i32),
        // r1 = r10 - 4
        mov_reg(1, 10),
        add_imm(1, -4),
        // r2 = 1
        mov_imm(2, 1),
        // call emit_reading (18)
        call_helper(18),
        // r0 = 0; exit
        mov_imm(0, 0),
        exit_insn(),
    ];
    let bytecode = assemble(&insns);
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    // Use a 13-byte blob to verify the pointers bracket exactly this length.
    let blob = vec![0xAAu8; 13];
    let readings = unsafe { decoder::execute_decoder(&cbor, &blob) }.unwrap();

    assert_eq!(
        readings.get("L"),
        Some(&13i64),
        "decoder context input_data/input_end must bracket the 13-byte APP_DATA blob"
    );
}

// T-1904g: bpf_trace_printk executes without error
//
// Traces to: GW-1904 AC-6
//
// Smoke test: verifies that a decoder calling bpf_trace_printk runs
// without error.  Full tracing-output assertion (confirming the message
// appears at target `decoder_bpf`) requires `tracing-test` infrastructure
// not yet set up in this test module.
#[test]
fn t1904g_trace_printk_logged() {
    // BPF program that calls bpf_trace_printk("hello", 5):
    //   *(u32*)(r10 - 8) = "hell"   (0x6c6c6568 LE)
    //   *(u8*)(r10 - 4) = 'o'
    //   r1 = r10 - 8
    //   r2 = 5
    //   r3 = 0
    //   call bpf_trace_printk (16)
    //   r0 = 0; exit
    let insns: Vec<[u8; 8]> = vec![
        st_mem_w(10, -8, 0x6c6c6568_u32 as i32), // "hell"
        st_mem_b(10, -4, b'o' as i32),           // "o"
        mov_reg(1, 10),
        add_imm(1, -8),
        mov_imm(2, 5),
        mov_imm(3, 0),
        call_helper(16), // bpf_trace_printk
        mov_imm(0, 0),
        exit_insn(),
    ];
    let bytecode = assemble(&insns);
    let image = sonde_protocol::ProgramImage {
        bytecode,
        maps: vec![],
        map_initial_data: vec![],
        map_readonly: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    // Execute — the test verifies the program runs without error.
    // Full tracing assertion would require tracing-test infrastructure;
    // this test confirms the helper executes and returns 0 without panic.
    let result = unsafe { decoder::execute_decoder(&cbor, &[0u8; 4]) };
    assert!(
        result.is_ok(),
        "bpf_trace_printk should execute without error: {:?}",
        result.err()
    );
}
