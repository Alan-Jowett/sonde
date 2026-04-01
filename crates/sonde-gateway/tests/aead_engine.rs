// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for the AES-256-GCM `process_frame_aead` engine path.
//!
//! These tests mirror the HMAC engine tests (phase2b) but exercise the AEAD
//! codec. Only compiled when the `aes-gcm-codec` feature is enabled.

#![cfg(feature = "aes-gcm-codec")]

use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::Gateway;
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame_aead, encode_frame_aead, open_frame, FrameHeader, GatewayMessage, NodeMessage,
    MSG_GET_CHUNK, MSG_PEER_REQUEST, MSG_PROGRAM_ACK, MSG_WAKE,
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

    fn build_wake_aead(
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
        };
        let cbor = msg.encode().unwrap();
        encode_frame_aead(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }

    #[allow(dead_code)]
    fn build_get_chunk_aead(&self, seq: u64, chunk_index: u32) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_GET_CHUNK,
            nonce: seq,
        };
        let msg = NodeMessage::GetChunk { chunk_index };
        let cbor = msg.encode().unwrap();
        encode_frame_aead(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }

    #[allow(dead_code)]
    fn build_program_ack_aead(&self, seq: u64, program_hash: &[u8]) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_PROGRAM_ACK,
            nonce: seq,
        };
        let msg = NodeMessage::ProgramAck {
            program_hash: program_hash.to_vec(),
        };
        let cbor = msg.encode().unwrap();
        encode_frame_aead(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
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

    let frame = node.build_wake_aead(42, 1, &[0u8; 32], 3300);
    let resp = gw
        .process_frame_aead(&frame, node.peer_address())
        .await
        .expect("AEAD WAKE must produce a COMMAND response");

    // The response must be a valid AEAD frame decodable with the node's PSK.
    let decoded = decode_frame_aead(&resp).expect("response must decode as AEAD frame");
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
    let frame = imposter.build_wake_aead(10, 1, &[0u8; 32], 3000);

    let resp = gw.process_frame_aead(&frame, b"imposter".to_vec()).await;
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

    let mut frame = node.build_wake_aead(99, 1, &[0u8; 32], 3300);
    // Flip a bit in the ciphertext region (past the 11-byte header).
    if frame.len() > 12 {
        frame[12] ^= 0x01;
    }

    let resp = gw.process_frame_aead(&frame, node.peer_address()).await;
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
    let frame =
        encode_frame_aead(&header, &payload, &psk, &GatewayAead, &RustCryptoSha256).unwrap();

    let resp = gw.process_frame_aead(&frame, b"phone".to_vec()).await;
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
    let frame = node.build_wake_aead(1, 1, &[0u8; 32], 3000);

    let resp = gw.process_frame_aead(&frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "unknown key_hint must be silently discarded"
    );
}
