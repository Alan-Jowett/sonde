// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use sha2::{Digest, Sha256};

/// Validate a node ID: must be 1-64 bytes of UTF-8, non-empty (after trimming).
pub fn validate_node_id(id: &str) -> Result<(), PairingError> {
    if id.len() > 64 {
        return Err(PairingError::InvalidNodeId(format!(
            "node ID must be at most 64 bytes, got {}",
            id.len()
        )));
    }
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(PairingError::InvalidNodeId(
            "node ID must not be empty".into(),
        ));
    }
    Ok(())
}

/// Validate an RF channel: must be 1-13.
pub fn validate_rf_channel(ch: u8) -> Result<(), PairingError> {
    if !(1..=13).contains(&ch) {
        return Err(PairingError::InvalidRfChannel(ch));
    }
    Ok(())
}

/// Compute `key_hint` from a PSK: `u16::from_be_bytes(SHA-256(psk)[30..32])`.
pub fn compute_key_hint(psk: &[u8; 32]) -> u16 {
    let hash = Sha256::digest(psk);
    u16::from_be_bytes([hash[30], hash[31]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_node_id() {
        validate_node_id("sensor-1").unwrap();
        validate_node_id("a").unwrap();
        // 64-byte ID is valid
        let max_id = "a".repeat(64);
        validate_node_id(&max_id).unwrap();
    }

    #[test]
    fn empty_node_id() {
        assert!(validate_node_id("").is_err());
        assert!(validate_node_id("   ").is_err());
    }

    #[test]
    fn node_id_too_long() {
        let long_id = "a".repeat(65);
        assert!(validate_node_id(&long_id).is_err());
    }

    #[test]
    fn valid_rf_channels() {
        for ch in 1..=13 {
            validate_rf_channel(ch).unwrap();
        }
    }

    #[test]
    fn invalid_rf_channels() {
        assert!(validate_rf_channel(0).is_err());
        assert!(validate_rf_channel(14).is_err());
        assert!(validate_rf_channel(255).is_err());
    }

    #[test]
    fn key_hint_deterministic() {
        let psk = [0x42u8; 32];
        let hint1 = compute_key_hint(&psk);
        let hint2 = compute_key_hint(&psk);
        assert_eq!(hint1, hint2);
    }

    #[test]
    fn key_hint_uses_last_two_bytes() {
        let psk = [0x42u8; 32];
        let hash = Sha256::digest(psk);
        let expected = u16::from_be_bytes([hash[30], hash[31]]);
        assert_eq!(compute_key_hint(&psk), expected);
    }

    #[test]
    fn key_hint_different_keys_differ() {
        let hint_a = compute_key_hint(&[0x42u8; 32]);
        let hint_b = compute_key_hint(&[0x43u8; 32]);
        // Different keys should (almost certainly) produce different hints
        assert_ne!(hint_a, hint_b);
    }
}
