// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Deterministic UUID generator for KiCad elements.
//!
//! UUIDs are derived from a seed (project name + IR content hash) plus a
//! unique path string for each element. This ensures identical IR files
//! always produce identical output.

use sha2::{Digest, Sha256};

/// Generates deterministic UUID v4 strings from a seed and element paths.
pub struct UuidGenerator {
    seed: [u8; 32],
    counter: u64,
}

impl UuidGenerator {
    /// Create a new generator seeded from a project name and IR content hash.
    pub fn new(project: &str, ir_content_hash: &[u8; 32]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(project.as_bytes());
        hasher.update(ir_content_hash);
        Self {
            seed: hasher.finalize().into(),
            counter: 0,
        }
    }

    /// Generate the next deterministic UUID for the given element path.
    ///
    /// The `path` argument provides uniqueness within a run (e.g.,
    /// `"symbol:R1"`, `"pin:R1:1"`, `"wire:SDA:0"`).
    pub fn next(&mut self, path: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.seed);
        hasher.update(path.as_bytes());
        hasher.update(self.counter.to_le_bytes());
        self.counter += 1;
        let hash = hasher.finalize();
        format_uuid_v4(&hash[..16])
    }
}

/// Format 16 bytes as a UUID v4 string, setting the version and variant bits.
fn format_uuid_v4(bytes: &[u8]) -> String {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&bytes[..16]);
    // Set version to 4 (0100 in high nibble of byte 6)
    buf[6] = (buf[6] & 0x0f) | 0x40;
    // Set variant to RFC 4122 (10xx in high bits of byte 8)
    buf[8] = (buf[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        u16::from_be_bytes([buf[4], buf[5]]),
        u16::from_be_bytes([buf[6], buf[7]]),
        u16::from_be_bytes([buf[8], buf[9]]),
        // Last 6 bytes as a 48-bit value
        (buf[10] as u64) << 40
            | (buf[11] as u64) << 32
            | (buf[12] as u64) << 24
            | (buf[13] as u64) << 16
            | (buf[14] as u64) << 8
            | (buf[15] as u64),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_output() {
        let hash = [0x42u8; 32];
        let mut gen1 = UuidGenerator::new("test-project", &hash);
        let mut gen2 = UuidGenerator::new("test-project", &hash);
        let uuid1 = gen1.next("symbol:R1");
        let uuid2 = gen2.next("symbol:R1");
        assert_eq!(uuid1, uuid2, "same seed + path must produce same UUID");
    }

    #[test]
    fn different_paths_differ() {
        let hash = [0x42u8; 32];
        let mut gen = UuidGenerator::new("test-project", &hash);
        let uuid_r1 = gen.next("symbol:R1");
        let mut gen2 = UuidGenerator::new("test-project", &hash);
        let uuid_r2 = gen2.next("symbol:R2");
        assert_ne!(uuid_r1, uuid_r2);
    }

    #[test]
    fn uuid_v4_format() {
        let hash = [0x42u8; 32];
        let mut gen = UuidGenerator::new("test-project", &hash);
        let uuid = gen.next("test");
        // UUID v4 format: 8-4-4-4-12 hex chars
        let re = regex_lite_check(&uuid);
        assert!(re, "UUID {uuid} does not match v4 format");
        // Version nibble must be 4
        assert_eq!(&uuid[14..15], "4", "version nibble must be 4");
        // Variant nibble must be 8, 9, a, or b
        let variant = &uuid[19..20];
        assert!(
            matches!(variant, "8" | "9" | "a" | "b"),
            "variant nibble must be 8/9/a/b, got {variant}"
        );
    }

    fn regex_lite_check(uuid: &str) -> bool {
        if uuid.len() != 36 {
            return false;
        }
        let parts: Vec<&str> = uuid.split('-').collect();
        if parts.len() != 5 {
            return false;
        }
        let expected_lens = [8, 4, 4, 4, 12];
        for (part, &expected) in parts.iter().zip(&expected_lens) {
            if part.len() != expected || !part.chars().all(|c| c.is_ascii_hexdigit()) {
                return false;
            }
        }
        true
    }
}
