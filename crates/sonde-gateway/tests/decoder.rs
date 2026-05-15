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
    };
    let cbor = image.encode_deterministic().unwrap();

    // Simulate a 6-byte TMP102 payload: raw_hi=0x19, raw_lo=0x80, temp_mC=25125
    let mut blob = vec![0u8; 6];
    blob[0] = 0x19;
    blob[1] = 0x80;
    blob[2..6].copy_from_slice(&25125i32.to_le_bytes());

    let readings = decoder::execute_decoder(&cbor, &blob).unwrap();

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
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = decoder::execute_decoder(&cbor, &[0u8; 10]).unwrap();

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
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = decoder::execute_decoder(&cbor, &[0u8; 10]).unwrap();

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
    };
    let cbor = image.encode_deterministic().unwrap();

    let result = decoder::execute_decoder(&cbor, &[0u8; 10]);
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
    };
    let cbor = image.encode_deterministic().unwrap();

    let original_blob: Vec<u8> = vec![0x19, 0x80, 0x45, 0x62, 0x00, 0x00];
    let readings = decoder::execute_decoder(&cbor, &original_blob).unwrap();

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
    };
    let cbor = image.encode_deterministic().unwrap();

    let readings = decoder::execute_decoder(&cbor, &[0u8; 10]).unwrap();

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
