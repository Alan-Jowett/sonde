// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Unit tests for protocol helpers and CBOR encoding details.
//!
//! These tests verify internal implementation correctness (key-hint derivation,
//! CBOR key ordering) and are intentionally separate from the numbered T-P00x
//! validation suite in `validation.rs`.

use sonde_protocol::*;

use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Software providers (shared with validation.rs, duplicated to keep files
// self-contained — these are tiny and test-only).
// ---------------------------------------------------------------------------

struct SoftwareSha256;

impl Sha256Provider for SoftwareSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}

// ---------------------------------------------------------------------------
// key_hint_from_psk tests
// ---------------------------------------------------------------------------

#[test]
fn test_key_hint_from_psk() {
    let psk = [0x42u8; 32];
    let hint = key_hint_from_psk(&psk, &SoftwareSha256);

    // Verify independently using the sha2 crate directly (not via SoftwareSha256).
    let mut hasher = Sha256::new();
    hasher.update(psk);
    let hash = hasher.finalize();
    let expected = u16::from_be_bytes([hash[30], hash[31]]);
    assert_eq!(hint, expected);
}

#[test]
fn test_key_hint_from_psk_different_keys() {
    let psk_a = [0x42u8; 32];
    let psk_b = [0xAAu8; 32];
    let hint_a = key_hint_from_psk(&psk_a, &SoftwareSha256);
    let hint_b = key_hint_from_psk(&psk_b, &SoftwareSha256);
    // Verify each PSK maps to the expected hint derived from SHA-256,
    // without assuming different PSKs must produce different 16-bit hints.
    let expected_a = {
        let mut hasher = Sha256::new();
        hasher.update(psk_a);
        let hash = hasher.finalize();
        u16::from_be_bytes([hash[30], hash[31]])
    };
    let expected_b = {
        let mut hasher = Sha256::new();
        hasher.update(psk_b);
        let hash = hasher.finalize();
        u16::from_be_bytes([hash[30], hash[31]])
    };

    assert_eq!(hint_a, expected_a);
    assert_eq!(hint_b, expected_b);
}

// ---------------------------------------------------------------------------
// COMMAND CBOR key ordering test
// ---------------------------------------------------------------------------

#[test]
fn test_command_cbor_key_order() {
    let cmd = GatewayMessage::Command {
        starting_seq: 100,
        timestamp_ms: 999,
        payload: CommandPayload::UpdateProgram {
            program_hash: vec![0xABu8; 32],
            program_size: 512,
            chunk_size: 190,
            chunk_count: 3,
        },
    };
    let cbor = cmd.encode().unwrap();

    // Decode raw CBOR and verify integer keys are in ascending order.
    let value: ciborium::Value = ciborium::from_reader(cbor.as_slice()).expect("valid CBOR");
    if let ciborium::Value::Map(pairs) = value {
        let keys: Vec<u64> = pairs
            .iter()
            .map(|(k, _)| {
                u64::try_from(k.as_integer().expect("integer key")).expect("non-negative")
            })
            .collect();
        // Must be strictly ascending: {4, 5, 13, 14}
        for w in keys.windows(2) {
            assert!(w[0] < w[1], "CBOR keys not in ascending order: {:?}", keys);
        }
        assert_eq!(keys, vec![4, 5, 13, 14]);
    } else {
        panic!("Expected CBOR map");
    }
}
