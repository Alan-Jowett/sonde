// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::fmt;

use crate::crypto::RustCryptoSha256;
use sonde_protocol::Sha256Provider;

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
