// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::fmt;
use std::io::Write;

use crate::crypto::RustCryptoSha256;
use prevail::crab::ebpf_domain::DomainContext;
use prevail::crab::var_registry::VariableRegistry;
use prevail::elf_loader::ElfObject;
use prevail::fwd_analyzer;
use prevail::ir::program::Program as PrevailProgram;
use prevail::ir::unmarshal;
use prevail::linux::linux_platform::LinuxPlatform;
use prevail::spec::config::EbpfVerifierOptions;
use sonde_protocol::{MapDef, ProgramImage, Sha256Provider};

/// Program verification profile.
#[derive(Debug, Clone, PartialEq)]
pub enum VerificationProfile {
    /// Resident programs are stored persistently on the node.
    Resident,
    /// Ephemeral programs are run once and discarded.
    Ephemeral,
}

/// A stored program record: the CBOR-encoded image plus metadata.
#[derive(Debug, Clone)]
pub struct ProgramRecord {
    /// SHA-256 of the CBOR-encoded program image.
    pub hash: Vec<u8>,
    /// CBOR-encoded program image (bytecode + map definitions).
    pub image: Vec<u8>,
    /// Byte length of the CBOR image.
    pub size: u32,
    /// Verification profile used at ingestion time.
    pub verification_profile: VerificationProfile,
    /// ABI version this program was compiled for (`None` = any ABI).
    pub abi_version: Option<u32>,
}

/// Errors from program library operations.
#[derive(Debug, Clone)]
pub enum ProgramError {
    /// Image is empty or invalid.
    InvalidImage,
    /// Image exceeds the size limit for its profile.
    ImageTooLarge { size: u32, limit: u32 },
    /// Program not found by hash.
    NotFound,
    /// ELF parsing failed.
    ElfParseError(String),
    /// Prevail verification failed.
    VerificationFailed(String),
    /// Generic error.
    Internal(String),
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::InvalidImage => write!(f, "image is empty or invalid"),
            ProgramError::ImageTooLarge { size, limit } => {
                write!(f, "image size {} exceeds limit {}", size, limit)
            }
            ProgramError::NotFound => write!(f, "program not found"),
            ProgramError::ElfParseError(msg) => write!(f, "ELF parse error: {}", msg),
            ProgramError::VerificationFailed(msg) => {
                write!(f, "verification failed: {}", msg)
            }
            ProgramError::Internal(msg) => write!(f, "program error: {}", msg),
        }
    }
}

impl std::error::Error for ProgramError {}

/// Maximum CBOR image sizes per profile (GW-0403).
const MAX_RESIDENT_SIZE: u32 = 4096;
/// Maximum CBOR image size for ephemeral programs (GW-0202 AC3).
pub const MAX_EPHEMERAL_SIZE: u32 = 2048;

/// Program library: stores verified programs and serves chunks.
pub struct ProgramLibrary {
    sha256: RustCryptoSha256,
}

impl ProgramLibrary {
    pub fn new() -> Self {
        Self {
            sha256: RustCryptoSha256,
        }
    }

    /// Ingest a CBOR-encoded program image **without** BPF verification.
    ///
    /// **Warning:** This bypasses Prevail verification. Use `ingest_elf()`
    /// for production ingestion of BPF programs. This method exists for
    /// testing and for internal use by `ingest_elf()` after verification.
    pub fn ingest_unverified(
        &self,
        image: Vec<u8>,
        profile: VerificationProfile,
    ) -> Result<ProgramRecord, ProgramError> {
        if image.is_empty() {
            return Err(ProgramError::InvalidImage);
        }

        let size = image.len() as u32;
        let limit = match profile {
            VerificationProfile::Resident => MAX_RESIDENT_SIZE,
            VerificationProfile::Ephemeral => MAX_EPHEMERAL_SIZE,
        };
        if size > limit {
            return Err(ProgramError::ImageTooLarge { size, limit });
        }

        let hash = self.sha256.hash(&image).to_vec();

        Ok(ProgramRecord {
            hash,
            image,
            size,
            verification_profile: profile,
            abi_version: None,
        })
    }

    /// Ingest a raw ELF binary with prevail verification (GW-0401).
    ///
    /// Steps:
    ///   1. Write ELF bytes to a temp file for prevail.
    ///   2. Parse the ELF with `ElfObject`.
    ///   3. Extract programs and run prevail verification on each extracted program.
    ///   4. Reject ELF files containing multiple programs (ambiguous selection).
    ///   5. Serialize bytecode and map definitions into a `ProgramImage`.
    ///   6. Delegate size-limit enforcement and hashing to `ingest()`.
    pub fn ingest_elf(
        &self,
        elf_bytes: &[u8],
        profile: VerificationProfile,
    ) -> Result<ProgramRecord, ProgramError> {
        if elf_bytes.is_empty() {
            return Err(ProgramError::InvalidImage);
        }

        // Write ELF bytes to a temp file for prevail's file-based parser.
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProgramError::Internal(format!("failed to create temp file: {e}")))?;
        tmp.write_all(elf_bytes)
            .map_err(|e| ProgramError::Internal(format!("failed to write temp file: {e}")))?;
        let tmp_path = tmp.path().to_str().ok_or_else(|| {
            ProgramError::Internal("temporary file path is not valid UTF-8".into())
        })?;

        // Configure prevail verifier options based on profile.
        let mut opts = EbpfVerifierOptions::default();
        match profile {
            VerificationProfile::Resident => {
                opts.cfg_opts.check_for_termination = false;
            }
            VerificationProfile::Ephemeral => {
                opts.cfg_opts.check_for_termination = true;
            }
        }

        // Parse the ELF.
        let mut elf = ElfObject::new(tmp_path, opts)
            .map_err(|e| ProgramError::ElfParseError(e.to_string()))?;

        // Extract programs using the Linux platform.
        let mut platform = LinuxPlatform::new();
        let raw_programs = elf
            .get_programs("", "", &mut platform)
            .map_err(|e| ProgramError::ElfParseError(e.to_string()))?;

        if raw_programs.is_empty() {
            return Err(ProgramError::ElfParseError(
                "no programs found in ELF".into(),
            ));
        }
        if raw_programs.len() > 1 {
            return Err(ProgramError::ElfParseError(format!(
                "ELF contains {} programs; expected exactly one",
                raw_programs.len()
            )));
        }

        // Verify each program with prevail.
        for raw_prog in &raw_programs {
            let mut notes: Vec<Vec<String>> = Vec::new();
            let inst_seq =
                unmarshal::unmarshal(&raw_prog.prog, &mut notes, &raw_prog.info, &platform, &opts)
                    .map_err(|e| ProgramError::VerificationFailed(format!("unmarshal: {e}")))?;

            let program =
                PrevailProgram::from_sequence(&inst_seq, &raw_prog.info, &platform, &opts)
                    .map_err(|e| {
                        ProgramError::VerificationFailed(format!("invalid control flow: {e}"))
                    })?;

            let ctx = DomainContext {
                program_info: &raw_prog.info,
                options: &opts,
                platform: &platform,
            };
            let mut registry = VariableRegistry::new();
            let result = fwd_analyzer::analyze(&program, &ctx, &mut registry);

            if result.failed {
                // Collect verifier notes for diagnostics.
                let diag: Vec<String> = notes.into_iter().flatten().collect();
                let diag_str = if diag.is_empty() {
                    String::new()
                } else {
                    format!(": {}", diag.join("; "))
                };
                return Err(ProgramError::VerificationFailed(format!(
                    "program `{}` failed verification{}",
                    raw_prog.function_name, diag_str
                )));
            }
        }

        // Build the ProgramImage from the first program's bytecode and maps.
        let first = &raw_programs[0];

        let mut bytecode = Vec::with_capacity(first.prog.len() * 8);
        for inst in &first.prog {
            bytecode.push(inst.opcode);
            bytecode.push(inst.dst_src);
            bytecode.extend_from_slice(&inst.offset.to_le_bytes());
            bytecode.extend_from_slice(&inst.imm.to_le_bytes());
        }

        let maps: Vec<MapDef> = first
            .info
            .map_descriptors
            .iter()
            .map(|md| MapDef {
                map_type: md.map_type,
                key_size: md.key_size,
                value_size: md.value_size,
                max_entries: md.max_entries,
            })
            .collect();

        let image = ProgramImage { bytecode, maps };
        let cbor = image
            .encode_deterministic()
            .map_err(|e| ProgramError::Internal(format!("CBOR encoding failed: {e}")))?;

        // Delegate size-limit and hashing logic to the canonical ingest path.
        self.ingest_unverified(cbor, profile)
    }

    /// Look up a program by its hash in the given storage snapshot.
    pub fn get_by_hash<'a>(
        &self,
        records: &'a [ProgramRecord],
        hash: &[u8],
    ) -> Option<&'a ProgramRecord> {
        records.iter().find(|r| r.hash == hash)
    }

    /// Serve a chunk from a program image using `sonde_protocol::get_chunk()`.
    pub fn get_chunk<'a>(
        &self,
        image: &'a [u8],
        chunk_index: u32,
        chunk_size: u32,
    ) -> Option<&'a [u8]> {
        sonde_protocol::get_chunk(image, chunk_index, chunk_size)
    }

    /// Compute the chunk count for a given image size and chunk size.
    pub fn chunk_count(&self, image_size: usize, chunk_size: usize) -> Option<u32> {
        sonde_protocol::chunk_count(image_size, chunk_size)
    }
}

impl Default for ProgramLibrary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_elf_empty_bytes_rejected() {
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&[], VerificationProfile::Resident)
            .unwrap_err();
        assert!(matches!(err, ProgramError::InvalidImage));
    }

    #[test]
    fn ingest_elf_invalid_bytes_rejected() {
        let lib = ProgramLibrary::new();
        let err = lib
            .ingest_elf(&[0xDE, 0xAD, 0xBE, 0xEF], VerificationProfile::Resident)
            .unwrap_err();
        assert!(matches!(err, ProgramError::ElfParseError(_)));
    }

    #[test]
    fn ingest_elf_truncated_elf_header_rejected() {
        let lib = ProgramLibrary::new();
        // Valid ELF magic but truncated
        let err = lib
            .ingest_elf(&[0x7f, b'E', b'L', b'F'], VerificationProfile::Resident)
            .unwrap_err();
        assert!(matches!(err, ProgramError::ElfParseError(_)));
    }

    /// Build a minimal valid BPF ELF containing a single `.text` section
    /// with `mov r0, 0; exit` — the simplest passing eBPF program.
    fn make_minimal_bpf_elf() -> Vec<u8> {
        // Construct a minimal 64-bit little-endian ELF relocatable object
        // with a single .text section containing two BPF instructions.
        //
        // Layout: ELF header (64B) | .text (16B) | .shstrtab (17B) | section headers (3*64B)
        let bpf_code: [u8; 16] = [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ];
        let shstrtab: &[u8] = b"\0.text\0.shstrtab\0"; // 17 bytes

        let text_offset: u64 = 64; // right after ELF header
        let shstrtab_offset: u64 = text_offset + bpf_code.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();

        // ── ELF header (64 bytes) ──
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']); // e_ident magic
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
        elf.extend_from_slice(&3u16.to_le_bytes()); // e_shnum (null + .text + .shstrtab)
        elf.extend_from_slice(&2u16.to_le_bytes()); // e_shstrndx = 2
        assert_eq!(elf.len(), 64);

        // ── .text section data ──
        elf.extend_from_slice(&bpf_code);

        // ── .shstrtab section data ──
        elf.extend_from_slice(shstrtab);

        // ── Section headers (3 entries × 64 bytes each) ──

        // [0] Null section header
        elf.extend_from_slice(&[0u8; 64]);

        // [1] .text section header
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes()); // sh_name = offset of ".text" in shstrtab
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // sh_type = SHT_PROGBITS
        let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
        sh[8..16].copy_from_slice(&flags.to_le_bytes()); // sh_flags
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes()); // sh_offset
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes()); // sh_size
        sh[48..56].copy_from_slice(&8u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh);

        // [2] .shstrtab section header
        let mut sh2 = [0u8; 64];
        sh2[0..4].copy_from_slice(&7u32.to_le_bytes()); // sh_name = offset of ".shstrtab"
        sh2[4..8].copy_from_slice(&3u32.to_le_bytes()); // sh_type = SHT_STRTAB
        sh2[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes()); // sh_offset
        sh2[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes()); // sh_size
        sh2[48..56].copy_from_slice(&1u64.to_le_bytes()); // sh_addralign
        elf.extend_from_slice(&sh2);

        elf
    }

    #[test]
    fn ingest_elf_valid_minimal_program() {
        let elf = make_minimal_bpf_elf();
        let lib = ProgramLibrary::new();
        let record = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();

        assert!(!record.hash.is_empty());
        assert!(record.size > 0);
        assert_eq!(record.verification_profile, VerificationProfile::Resident);

        let image = ProgramImage::decode(&record.image).unwrap();
        assert_eq!(image.bytecode.len(), 16);
        assert!(image.maps.is_empty());
    }

    #[test]
    fn ingest_elf_content_hash_is_deterministic() {
        let elf = make_minimal_bpf_elf();
        let lib = ProgramLibrary::new();
        let r1 = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();
        let r2 = lib.ingest_elf(&elf, VerificationProfile::Resident).unwrap();
        assert_eq!(r1.hash, r2.hash);
    }
}
