// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Validation tests for build metadata and verification diagnostics
//! (T-1304, T-1305a, T-1305b).

use sonde_gateway::program::{ProgramError, ProgramLibrary, VerificationProfile};

// ---------------------------------------------------------------------------
// T-1304: Build metadata in `--version` output
// ---------------------------------------------------------------------------

/// T-1304: Build metadata format validation.
///
/// The `SONDE_GIT_COMMIT` env var is set at build time by `build.rs`.
/// This test verifies the compile-time version string matches the
/// expected pattern: `<semver> (<7-char-hex-or-unknown>)`.
#[test]
fn t1304_version_string_format() {
    let version = env!("CARGO_PKG_VERSION");
    let major = env!("CARGO_PKG_VERSION_MAJOR");
    let minor = env!("CARGO_PKG_VERSION_MINOR");
    let patch = env!("CARGO_PKG_VERSION_PATCH");
    let commit = env!("SONDE_GIT_COMMIT");
    let full = format!("{version} ({commit})");

    // Semver core (major.minor.patch) must be numeric.
    assert!(
        major.chars().all(|c| c.is_ascii_digit()),
        "major must be numeric, got: {major}"
    );
    assert!(
        minor.chars().all(|c| c.is_ascii_digit()),
        "minor must be numeric, got: {minor}"
    );
    assert!(
        patch.chars().all(|c| c.is_ascii_digit()),
        "patch must be numeric, got: {patch}"
    );

    let core = format!("{major}.{minor}.{patch}");
    assert!(
        version.starts_with(&core),
        "version must start with semver core {core}, got: {version}"
    );

    // Commit hash must be 7 hex chars or "unknown".
    assert!(
        commit == "unknown" || (commit.len() == 7 && commit.chars().all(|c| c.is_ascii_hexdigit())),
        "commit must be 7 hex chars or 'unknown', got: {commit}"
    );

    // Full string matches the expected pattern.
    assert!(
        full.contains('(') && full.contains(')'),
        "version string must contain parenthesized commit: {full}"
    );
}

// ---------------------------------------------------------------------------
// T-1305a: Verification failure includes instruction-level diagnostics
// ---------------------------------------------------------------------------

/// Build a minimal BPF ELF with the given bytecode in a `sonde` section.
fn make_sonde_elf(bpf_code: &[u8]) -> Vec<u8> {
    let shstrtab: &[u8] = b"\0sonde\0.shstrtab\0";
    let text_offset: u64 = 64;
    let shstrtab_offset: u64 = text_offset + bpf_code.len() as u64;
    let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

    let mut elf = Vec::new();

    // ELF header (64 bytes)
    elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
    elf.push(2); // ELFCLASS64
    elf.push(1); // ELFDATA2LSB
    elf.push(1); // EI_VERSION
    elf.extend_from_slice(&[0; 9]);
    elf.extend_from_slice(&1u16.to_le_bytes()); // ET_REL
    elf.extend_from_slice(&247u16.to_le_bytes()); // EM_BPF
    elf.extend_from_slice(&1u32.to_le_bytes());
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
    elf.extend_from_slice(&shdr_offset.to_le_bytes());
    elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    elf.extend_from_slice(&3u16.to_le_bytes()); // e_shnum
    elf.extend_from_slice(&2u16.to_le_bytes()); // e_shstrndx

    // sonde section data
    elf.extend_from_slice(bpf_code);

    // .shstrtab section data
    elf.extend_from_slice(shstrtab);

    // Section headers (3 × 64 bytes)
    elf.extend_from_slice(&[0u8; 64]); // null

    // [1] sonde
    let mut sh = [0u8; 64];
    sh[0..4].copy_from_slice(&1u32.to_le_bytes());
    sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
    let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
    sh[8..16].copy_from_slice(&flags.to_le_bytes());
    sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
    sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
    sh[48..56].copy_from_slice(&8u64.to_le_bytes());
    elf.extend_from_slice(&sh);

    // [2] .shstrtab
    let mut sh2 = [0u8; 64];
    sh2[0..4].copy_from_slice(&7u32.to_le_bytes());
    sh2[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
    sh2[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
    sh2[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
    sh2[48..56].copy_from_slice(&1u64.to_le_bytes());
    elf.extend_from_slice(&sh2);

    elf
}

/// T-1305a: Verification failure includes instruction-level diagnostics.
///
/// Ingest a BPF program that fails Prevail forward analysis and assert
/// the error contains multi-line diagnostics with instruction labels.
#[test]
fn t1305a_verification_failure_includes_diagnostics() {
    // BPF: mov r1, 0; r0 = *(u64*)(r1+0); exit
    // Fails: dereferencing a scalar (r1 overwritten with 0).
    #[rustfmt::skip]
    let bpf_code: [u8; 24] = [
        0xb7, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r1, 0
        0x79, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // r0 = *(u64*)(r1+0)
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
    ];
    let elf = make_sonde_elf(&bpf_code);
    let lib = ProgramLibrary::new();
    let err = lib
        .ingest_elf(&elf, VerificationProfile::Resident)
        .unwrap_err();

    match &err {
        ProgramError::VerificationFailed(msg) => {
            assert!(
                msg.contains("failed verification"),
                "error should contain summary: {msg}"
            );
            // Multi-line diagnostics with at least one instruction label.
            let lines: Vec<&str> = msg.lines().collect();
            assert!(
                lines.len() >= 2,
                "expected multi-line diagnostics, got: {msg}"
            );
            let has_instruction_label = lines.iter().skip(1).any(|line| {
                let trimmed = line.trim_start();
                trimmed
                    .split_once(':')
                    .map(|(idx, _)| !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false)
            });
            assert!(
                has_instruction_label,
                "expected instruction-level diagnostic with label, got: {msg}"
            );
        }
        other => panic!("expected VerificationFailed, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// T-1305b: Successful verification produces no diagnostics
// ---------------------------------------------------------------------------

/// T-1305b: Successful verification produces no diagnostics.
///
/// Ingest a valid BPF program, assert success with no error messages,
/// and verify the returned program record has a valid hash and
/// decodable image.
#[test]
fn t1305b_successful_verification_no_diagnostics() {
    // BPF: mov r0, 0; exit — minimal valid program.
    #[rustfmt::skip]
    let bpf_code: [u8; 16] = [
        0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
    ];
    let elf = make_sonde_elf(&bpf_code);
    let lib = ProgramLibrary::new();
    let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

    // Program ingested successfully with valid hash and size.
    assert!(!record.hash.is_empty(), "hash must not be empty");
    assert!(record.size > 0, "program size must be positive");

    // Image is decodable.
    let image = sonde_protocol::ProgramImage::decode(&record.image).unwrap();
    assert_eq!(image.bytecode.len(), 16, "bytecode must be 16 bytes");
}
