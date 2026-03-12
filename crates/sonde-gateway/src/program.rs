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
const MAX_EPHEMERAL_SIZE: u32 = 2048;

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

    /// Ingest a CBOR-encoded program image.
    ///
    /// Steps:
    ///   1. Enforce size limits per profile (GW-0403).
    ///   2. Compute the SHA-256 hash (GW-0402).
    ///   3. Return a `ProgramRecord` ready for storage.
    ///
    /// TODO: Integrate prevail-rust for BPF verification (GW-0401).
    /// For Phase 2A we store programs without verification.
    pub fn ingest(
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
        let tmp_path = tmp.path().to_string_lossy().to_string();

        // Configure prevail verifier options.
        let mut opts = EbpfVerifierOptions::default();
        opts.cfg_opts.check_for_termination = true;

        // Parse the ELF.
        let mut elf = ElfObject::new(&tmp_path, opts)
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
                return Err(ProgramError::VerificationFailed(format!(
                    "program `{}` failed verification",
                    raw_prog.function_name
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
        self.ingest(cbor, profile)
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
}
