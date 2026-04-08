// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for the AES-256-GCM `process_frame` engine path.
//!
//! These tests mirror the HMAC engine tests (phase2b) but exercise the AEAD
//! codec.

use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::Gateway;
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, FrameHeader, GatewayMessage, NodeMessage, MSG_APP_DATA,
    MSG_GET_CHUNK, MSG_PEER_ACK, MSG_PEER_REQUEST, MSG_PROGRAM_ACK, MSG_WAKE, PEER_ACK_KEY_STATUS,
    PEER_REQ_KEY_PAYLOAD,
};

// ── Helpers ─────────────────────────────────────────────────────────

struct TestNode {
    node_id: String,
    key_hint: u16,
    psk: [u8; 32],
}

impl TestNode {
    fn new(node_id: &str, key_hint: u16, psk: [u8; 32]) -> Self {
        Self {
            node_id: node_id.to_string(),
            key_hint,
            psk,
        }
    }

    fn to_record(&self) -> NodeRecord {
        NodeRecord::new(self.node_id.clone(), self.key_hint, self.psk)
    }

    fn peer_address(&self) -> PeerAddress {
        self.node_id.as_bytes().to_vec()
    }

    fn build_wake(
        &self,
        nonce: u64,
        firmware_abi_version: u32,
        program_hash: &[u8],
        battery_mv: u32,
    ) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_WAKE,
            nonce,
        };
        let msg = NodeMessage::Wake {
            firmware_abi_version,
            program_hash: program_hash.to_vec(),
            battery_mv,
            firmware_version: "0.4.0".into(),
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }

    #[allow(dead_code)]
    fn build_get_chunk(&self, seq: u64, chunk_index: u32) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_GET_CHUNK,
            nonce: seq,
        };
        let msg = NodeMessage::GetChunk { chunk_index };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }

    #[allow(dead_code)]
    fn build_program_ack(&self, seq: u64, program_hash: &[u8]) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_PROGRAM_ACK,
            nonce: seq,
        };
        let msg = NodeMessage::ProgramAck {
            program_hash: program_hash.to_vec(),
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }

    fn build_app_data(&self, seq: u64, blob: &[u8]) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_APP_DATA,
            nonce: seq,
        };
        let msg = NodeMessage::AppData {
            blob: blob.to_vec(),
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }
}

fn make_gateway(storage: Arc<InMemoryStorage>) -> Gateway {
    Gateway::new(storage, Duration::from_secs(30))
}

// ── Tests ───────────────────────────────────────────────────────────

/// AEAD WAKE → COMMAND round-trip: a valid AEAD-encoded WAKE produces a
/// COMMAND response that can be decoded and decrypted with the same PSK.
#[tokio::test]
async fn aead_wake_round_trip() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-aead-01", 0x0001, [0xBBu8; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let frame = node.build_wake(42, 1, &[0u8; 32], 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("AEAD WAKE must produce a COMMAND response");

    // The response must be a valid AEAD frame decodable with the node's PSK.
    let decoded = decode_frame(&resp).expect("response must decode as AEAD frame");
    let plaintext = open_frame(&decoded, &node.psk, &GatewayAead, &RustCryptoSha256)
        .expect("open must succeed with correct PSK");

    let msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext);
    assert!(msg.is_ok(), "response payload must be valid CBOR");
}

/// Wrong PSK: an AEAD frame authenticated with a different PSK must be
/// silently discarded (no response).
#[tokio::test]
async fn aead_wrong_psk_discarded() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let real_psk = [0xCCu8; 32];
    let wrong_psk = [0xDDu8; 32];

    let node_record = NodeRecord::new("node-aead-wrong".into(), 0x0002, real_psk);
    storage.upsert_node(&node_record).await.unwrap();

    // Build a WAKE frame using the wrong PSK.
    let imposter = TestNode::new("node-aead-wrong", 0x0002, wrong_psk);
    let frame = imposter.build_wake(10, 1, &[0u8; 32], 3000);

    let resp = gw.process_frame(&frame, b"imposter".to_vec()).await;
    assert!(resp.is_none(), "wrong-PSK frame must be silently discarded");
}

/// Tampered frame: flipping a ciphertext bit must cause authentication
/// failure and silent discard.
#[tokio::test]
async fn aead_tampered_frame_discarded() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-aead-tamper", 0x0003, [0xEEu8; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let mut frame = node.build_wake(99, 1, &[0u8; 32], 3300);
    // Flip a bit in the ciphertext region (past the 11-byte header).
    if frame.len() > 12 {
        frame[12] ^= 0x01;
    }

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_none(), "tampered frame must be silently discarded");
}

/// PEER_REQUEST with no matching phone PSK is silently discarded.
/// With a matching phone PSK, it should be accepted and produce a PEER_ACK.
#[tokio::test]
async fn aead_peer_request_no_phone_psk_discarded() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let psk = [0x42u8; 32];
    let node_record = NodeRecord::new("node-aead-peer".into(), 0x0004, psk);
    storage.upsert_node(&node_record).await.unwrap();

    // Build a frame with MSG_PEER_REQUEST type using a PSK with no
    // matching phone PSK registered — the gateway should discard.
    let header = FrameHeader {
        key_hint: 0x0004,
        msg_type: MSG_PEER_REQUEST,
        nonce: 1,
    };
    let payload = vec![0xA0]; // minimal CBOR map
    let frame = encode_frame(&header, &payload, &psk, &GatewayAead, &RustCryptoSha256).unwrap();

    let resp = gw.process_frame(&frame, b"phone".to_vec()).await;
    assert!(
        resp.is_none(),
        "PEER_REQUEST with no matching phone PSK must be discarded"
    );
}

/// Unknown key_hint: frame from an unregistered node is silently discarded.
#[tokio::test]
async fn aead_unknown_key_hint_discarded() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());
    // Don't register any node.

    let node = TestNode::new("ghost", 0xFFFF, [0x11u8; 32]);
    let frame = node.build_wake(1, 1, &[0u8; 32], 3000);

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "unknown key_hint must be silently discarded"
    );
}

/// Happy-path AEAD PEER_REQUEST: register a phone PSK, build a properly
/// nested PEER_REQUEST (outer AEAD frame encrypted with `phone_psk`
/// containing inner `nonce(12) ‖ ciphertext ‖ tag`), and assert the
/// gateway returns a PEER_ACK that decrypts with `node_psk` and echoes
/// the nonce.
#[tokio::test]
async fn aead_peer_request_happy_path() {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use sonde_gateway::{PhonePskRecord, PhonePskStatus};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zeroize::Zeroizing;

    let sha = RustCryptoSha256;

    // --- Keys ---
    let phone_psk = [0x42u8; 32];
    let node_psk = [0xBBu8; 32];
    let phone_key_hint = sonde_protocol::key_hint_from_psk(&phone_psk, &sha);
    let node_key_hint = sonde_protocol::key_hint_from_psk(&node_psk, &sha);

    // --- Storage setup ---
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    // Register the phone PSK so the gateway can decrypt the outer frame.
    let phone_record = PhonePskRecord {
        phone_id: 0, // assigned by store
        phone_key_hint,
        psk: Zeroizing::new(phone_psk),
        label: "test-phone".into(),
        issued_at: SystemTime::now(),
        status: PhonePskStatus::Active,
    };
    storage.store_phone_psk(&phone_record).await.unwrap();

    // --- Build PairingRequest CBOR ---
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let pairing_cbor = ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(1.into()),
            ciborium::Value::Text("node-aead-pair-01".into()),
        ),
        (
            ciborium::Value::Integer(2.into()),
            ciborium::Value::Integer((node_key_hint as u64).into()),
        ),
        (
            ciborium::Value::Integer(3.into()),
            ciborium::Value::Bytes(node_psk.to_vec()),
        ),
        (
            ciborium::Value::Integer(4.into()),
            ciborium::Value::Integer(1.into()), // rf_channel = 1
        ),
        (
            ciborium::Value::Integer(6.into()),
            ciborium::Value::Integer(now.into()),
        ),
    ]);
    let mut pairing_bytes = Vec::new();
    ciborium::into_writer(&pairing_cbor, &mut pairing_bytes).unwrap();

    // --- Encrypt inner payload with phone_psk (AAD = "sonde-pairing-v2") ---
    let inner_nonce_bytes = [0x01u8; 12];
    let inner_nonce = Nonce::from_slice(&inner_nonce_bytes);
    let cipher = Aes256Gcm::new_from_slice(&phone_psk).unwrap();
    let inner_ciphertext = cipher
        .encrypt(
            inner_nonce,
            aes_gcm::aead::Payload {
                msg: &pairing_bytes,
                aad: b"sonde-pairing-v2",
            },
        )
        .unwrap();

    // encrypted_payload = inner_nonce(12) ‖ ciphertext+tag
    let mut encrypted_payload = Vec::with_capacity(12 + inner_ciphertext.len());
    encrypted_payload.extend_from_slice(&inner_nonce_bytes);
    encrypted_payload.extend_from_slice(&inner_ciphertext);

    // --- Wrap in outer CBOR: {1: bstr(encrypted_payload)} ---
    let outer_cbor = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_cbor_bytes = Vec::new();
    ciborium::into_writer(&outer_cbor, &mut outer_cbor_bytes).unwrap();

    // --- Encode outer AEAD frame (MSG_PEER_REQUEST, key_hint = phone_key_hint) ---
    let frame_nonce: u64 = 12345;
    let outer_header = FrameHeader {
        key_hint: phone_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: frame_nonce,
    };
    let frame = encode_frame(
        &outer_header,
        &outer_cbor_bytes,
        &phone_psk,
        &GatewayAead,
        &sha,
    )
    .unwrap();

    // --- Send to gateway ---
    let resp = gw
        .process_frame(&frame, b"phone-peer".to_vec())
        .await
        .expect("AEAD PEER_REQUEST must produce a PEER_ACK");

    // --- Verify response is a PEER_ACK decryptable with node_psk ---
    let decoded = decode_frame(&resp).expect("response must decode as AEAD frame");
    assert_eq!(
        decoded.header.msg_type, MSG_PEER_ACK,
        "response must be PEER_ACK"
    );
    assert_eq!(
        decoded.header.nonce, frame_nonce,
        "PEER_ACK must echo the request nonce"
    );

    let plaintext = open_frame(&decoded, &node_psk, &GatewayAead, &sha)
        .expect("PEER_ACK must decrypt with node_psk");

    // Verify CBOR contains status = 0 (registered).
    let ack_cbor: ciborium::Value = ciborium::from_reader(&plaintext[..]).unwrap();
    let ack_map = ack_cbor.as_map().expect("PEER_ACK payload must be a map");
    let mut status: Option<u64> = None;
    for (k, v) in ack_map {
        if let Some(key_val) = k.as_integer().and_then(|i| u64::try_from(i).ok()) {
            if key_val == PEER_ACK_KEY_STATUS {
                status = v.as_integer().and_then(|i| u64::try_from(i).ok());
            }
        }
    }
    assert_eq!(status, Some(0), "PEER_ACK status must be 0 (registered)");

    // Verify the node was registered in storage.
    let stored_node = storage
        .get_node("node-aead-pair-01")
        .await
        .unwrap()
        .expect("node must be registered after PEER_ACK");
    assert_eq!(stored_node.psk, node_psk);
    assert_eq!(stored_node.key_hint, node_key_hint);
}

// ── APP_DATA AEAD tests (T-0503a, T-0503b, T-0503c) ────────────────

/// Helper: do WAKE handshake and extract starting_seq from COMMAND response.
async fn wake_and_get_seq(gw: &Gateway, node: &TestNode, nonce: u64, program_hash: &[u8]) -> u64 {
    let frame = node.build_wake(nonce, 1, program_hash, 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("WAKE must produce COMMAND");
    let decoded = decode_frame(&resp).unwrap();
    let plaintext = open_frame(&decoded, &node.psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
    match msg {
        GatewayMessage::Command { starting_seq, .. } => starting_seq,
        _ => panic!("expected Command"),
    }
}

/// T-0503a: APP_DATA with valid AEAD is accepted (auth + decode + seq advance).
///
/// Verifies that a correctly AEAD-authenticated APP_DATA frame passes
/// decryption, CBOR decode, and sequence validation. The session's
/// `next_expected_seq` advancing proves the frame was not silently
/// discarded. Handler routing is not exercised here (no HandlerRouter
/// configured) — see T-E2E-032 / T-E2E-051 for the full end-to-end
/// handler delivery path.
#[tokio::test]
async fn t0503a_app_data_valid_accepted() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let program_hash = vec![0x42u8; 32];
    let node = TestNode::new("node-appdata-aead", 0x0010, [0x42u8; 32]);
    let mut rec = node.to_record();
    rec.current_program_hash = Some(program_hash.clone());
    storage.upsert_node(&rec).await.unwrap();

    let seq = wake_and_get_seq(&gw, &node, 1000, &program_hash).await;

    // Verify session exists and check initial expected_seq.
    let session_before = gw.session_manager().get_session("node-appdata-aead").await;
    assert!(session_before.is_some(), "session must exist after WAKE");
    let expected_before = session_before.unwrap().next_expected_seq;

    // Send APP_DATA with valid AEAD.
    let frame = node.build_app_data(seq, &[0xDE, 0xAD]);
    let _resp = gw.process_frame(&frame, node.peer_address()).await;

    // Assert the session sequence advanced — proves the frame was authenticated
    // and processed (not silently discarded).
    let session_after = gw.session_manager().get_session("node-appdata-aead").await;
    assert!(session_after.is_some(), "session must still exist");
    assert_eq!(
        session_after.unwrap().next_expected_seq,
        expected_before + 1,
        "sequence must advance after valid APP_DATA"
    );
}

/// T-0503b: APP_DATA with invalid GCM tag is silently discarded.
#[tokio::test]
async fn t0503b_app_data_invalid_gcm_tag_rejected() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let program_hash = vec![0x43u8; 32];
    let node = TestNode::new("node-appdata-tamper", 0x0011, [0x43u8; 32]);
    let mut rec = node.to_record();
    rec.current_program_hash = Some(program_hash.clone());
    storage.upsert_node(&rec).await.unwrap();

    let seq = wake_and_get_seq(&gw, &node, 2000, &program_hash).await;

    // Build valid APP_DATA then corrupt GCM tag.
    let mut frame = node.build_app_data(seq, &[0xBE, 0xEF]);
    // Flip a bit in the GCM tag (last 16 bytes).
    let tag_offset = frame.len() - 1;
    frame[tag_offset] ^= 0x01;

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "APP_DATA with corrupted GCM tag must be silently discarded"
    );
}
