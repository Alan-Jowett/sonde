// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BIP-39 wordlist fingerprint for recovery public key verification (GW-2011).
//!
//! Computes a 6-word fingerprint from an X25519 public key:
//!
//! 1. `hash = SHA-256(public_key_bytes)`
//! 2. Extract the first 66 bits of `hash` (bits 0–65).
//! 3. Split into six 11-bit unsigned integers.
//! 4. Map each to a word from the BIP-39 English wordlist (2048 entries).
//!
//! The 66-bit fingerprint provides a ~2^66 collision resistance target
//! for anti-MITM verification.
//!
//! This module is feature-gated behind `fingerprint` to avoid pulling the
//! 2048-word BIP-39 list into constrained firmware builds.

mod bip39_wordlist;

pub use bip39_wordlist::BIP39_ENGLISH;

use crate::traits::Sha256Provider;

/// Compute a 6-word BIP-39 fingerprint from an X25519 public key.
///
/// Both the gateway (for modem display) and the admin CLI/SPA must
/// produce identical fingerprints for the same public key.
pub fn compute_fingerprint(
    public_key: &[u8; 32],
    sha: &(impl Sha256Provider + ?Sized),
) -> [&'static str; 6] {
    let hash = sha.hash(public_key);

    // Pack the first 9 bytes (72 bits) into a u128 for bit extraction.
    // We only use 66 bits (6 × 11-bit indices).
    let bits = u128::from_be_bytes([
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7], hash[8], 0, 0, 0,
        0, 0, 0, 0,
    ]);

    let mut words = [""; 6];
    for i in 0..6u32 {
        let index = ((bits >> (128 - 11 - 11 * i)) & 0x7FF) as usize;
        words[i as usize] = BIP39_ENGLISH[index];
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSha256;
    impl Sha256Provider for StubSha256 {
        fn hash(&self, data: &[u8]) -> [u8; 32] {
            // Deterministic non-cryptographic hash for testing
            let mut out = [0u8; 32];
            for (i, &b) in data.iter().enumerate() {
                out[i % 32] = out[i % 32].wrapping_add(b);
            }
            // Mix to spread bits
            for i in 0..32 {
                out[i] = out[i].wrapping_mul(37).wrapping_add(out[(i + 1) % 32]);
            }
            out
        }
    }

    // T-2011: Fingerprint computation determinism
    #[test]
    fn test_fingerprint_determinism() {
        let pubkey = [0x42u8; 32];
        let sha = StubSha256;

        let fp1 = compute_fingerprint(&pubkey, &sha);
        let fp2 = compute_fingerprint(&pubkey, &sha);

        // Same key always produces same words
        assert_eq!(fp1, fp2);

        // All 6 words are non-empty
        for word in &fp1 {
            assert!(!word.is_empty());
        }
    }

    // T-2011: Different keys produce different fingerprints
    #[test]
    fn test_fingerprint_different_keys() {
        let sha = StubSha256;

        let fp1 = compute_fingerprint(&[0x42u8; 32], &sha);
        let fp2 = compute_fingerprint(&[0x43u8; 32], &sha);

        assert_ne!(
            fp1, fp2,
            "different keys should produce different fingerprints"
        );
    }

    // T-2011: All words are from the BIP-39 wordlist
    #[test]
    fn test_fingerprint_words_in_wordlist() {
        let sha = StubSha256;
        let fp = compute_fingerprint(&[0x99u8; 32], &sha);

        for word in &fp {
            assert!(
                BIP39_ENGLISH.contains(word),
                "word '{word}' not in BIP-39 wordlist"
            );
        }
    }

    // Verify BIP-39 wordlist has exactly 2048 entries
    #[test]
    fn test_bip39_wordlist_size() {
        assert_eq!(BIP39_ENGLISH.len(), 2048);
    }

    // Verify index extraction produces valid indices (0–2047)
    #[test]
    fn test_fingerprint_index_range() {
        let sha = StubSha256;
        // Test with many different keys
        for seed in 0u8..=255 {
            let mut key = [0u8; 32];
            key[0] = seed;
            let fp = compute_fingerprint(&key, &sha);
            for word in &fp {
                let idx = BIP39_ENGLISH.iter().position(|w| w == word);
                assert!(idx.is_some(), "word '{word}' not found in wordlist");
                assert!(idx.unwrap() < 2048);
            }
        }
    }
}
