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
        blob: None,
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

// ---------------------------------------------------------------------------
// Diagnostic message tests (T-P110 through T-P113)
// ---------------------------------------------------------------------------

// T-P110: DIAG_REQUEST round-trip encode/decode
#[test]
fn test_diag_request_round_trip() {
    let msg = NodeMessage::DiagRequest {
        diagnostic_type: DIAG_TYPE_RSSI,
    };
    assert_eq!(msg.msg_type(), MSG_DIAG_REQUEST);

    let cbor = msg.encode().expect("encode");
    let decoded = NodeMessage::decode(MSG_DIAG_REQUEST, &cbor).expect("decode");
    assert_eq!(decoded, msg);
}

// T-P111: DIAG_REPLY round-trip encode/decode
#[test]
fn test_diag_reply_round_trip() {
    let msg = GatewayMessage::DiagReply {
        diagnostic_type: DIAG_TYPE_RSSI,
        rssi_dbm: -55,
        signal_quality: SIGNAL_QUALITY_GOOD,
    };
    assert_eq!(msg.msg_type(), MSG_DIAG_REPLY);

    let cbor = msg.encode().expect("encode");
    let decoded = GatewayMessage::decode(MSG_DIAG_REPLY, &cbor).expect("decode");
    assert_eq!(decoded, msg);
}

// T-P111 additional: negative RSSI values round-trip correctly
#[test]
fn test_diag_reply_negative_rssi() {
    for rssi in [-90i8, -75, -60, -30, 0] {
        let msg = GatewayMessage::DiagReply {
            diagnostic_type: DIAG_TYPE_RSSI,
            rssi_dbm: rssi,
            signal_quality: SIGNAL_QUALITY_BAD,
        };
        let cbor = msg.encode().expect("encode");
        let decoded = GatewayMessage::decode(MSG_DIAG_REPLY, &cbor).expect("decode");
        assert_eq!(decoded, msg, "failed for rssi={}", rssi);
    }
}

// T-P112: DIAG_REQUEST unknown CBOR keys ignored
#[test]
fn test_diag_request_unknown_keys_ignored() {
    // Manually build CBOR: {1: 0x01, 99: "extra"}
    let value = ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(1.into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Integer(99.into()),
            ciborium::Value::Text("extra".into()),
        ),
    ]);
    let mut cbor = Vec::new();
    ciborium::into_writer(&value, &mut cbor).unwrap();

    let decoded = NodeMessage::decode(MSG_DIAG_REQUEST, &cbor).expect("decode");
    match decoded {
        NodeMessage::DiagRequest { diagnostic_type } => {
            assert_eq!(diagnostic_type, DIAG_TYPE_RSSI);
        }
        _ => panic!("expected DiagRequest"),
    }
}

// T-P113: DIAG_REPLY deterministic CBOR encoding
#[test]
fn test_diag_reply_deterministic_encoding() {
    let msg = GatewayMessage::DiagReply {
        diagnostic_type: DIAG_TYPE_RSSI,
        rssi_dbm: -70,
        signal_quality: SIGNAL_QUALITY_MARGINAL,
    };
    let cbor1 = msg.encode().expect("encode 1");
    let cbor2 = msg.encode().expect("encode 2");
    assert_eq!(cbor1, cbor2, "encoding must be deterministic");

    // Verify CBOR keys are in ascending order (1, 2, 3)
    let value: ciborium::Value = ciborium::from_reader(cbor1.as_slice()).expect("valid CBOR");
    if let ciborium::Value::Map(pairs) = value {
        let keys: Vec<u64> = pairs
            .iter()
            .map(|(k, _)| {
                u64::try_from(k.as_integer().expect("integer key")).expect("non-negative")
            })
            .collect();
        assert_eq!(keys, vec![1, 2, 3], "CBOR keys must be 1, 2, 3 in order");
    } else {
        panic!("Expected CBOR map");
    }
}
