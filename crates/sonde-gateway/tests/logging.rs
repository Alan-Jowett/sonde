// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Operational logging tests (GW-1300, GW-1302).
//!
//! Validates that the gateway emits the structured tracing events specified
//! by the operational logging requirements.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use sonde_gateway::crypto::RustCryptoHmac;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::{PeerAddress, Transport};

use sonde_protocol::modem::{encode_modem_frame, FrameDecoder, ModemMessage, RecvFrame};
use sonde_protocol::{
    encode_frame, FrameHeader, HmacProvider, NodeMessage, Sha256Provider, MSG_PEER_REQUEST,
    MSG_WAKE, PEER_REQ_KEY_PAYLOAD,
};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;

use tracing_test::traced_test;

// ── Test helpers ───────────────────────────────────────────────────────

const TEST_NODE_PSK: [u8; 32] = [0x42u8; 32];
const TEST_PHONE_PSK: [u8; 32] = [0x55u8; 32];

fn compute_key_hint(psk: &[u8; 32]) -> u16 {
    let crypto_sha = sonde_gateway::crypto::RustCryptoSha256;
    let h = crypto_sha.hash(psk);
    u16::from_be_bytes([h[30], h[31]])
}

fn make_gateway(storage: Arc<InMemoryStorage>) -> Gateway {
    Gateway::new(storage, Duration::from_secs(30))
}

async fn store_test_program(storage: &InMemoryStorage, bytecode: &[u8]) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let record = lib
        .ingest_unverified(cbor, VerificationProfile::Resident)
        .unwrap();
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

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
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
    }
}

// ── PEER_REQUEST helpers (adapted from peer_request.rs) ────────────────

fn ecdh_encrypt(identity: &GatewayIdentity, plaintext: &[u8]) -> Vec<u8> {
    let mut eph_scalar = [0u8; 32];
    getrandom::fill(&mut eph_scalar).unwrap();
    let eph_secret = x25519_dalek::StaticSecret::from(eph_scalar);
    let eph_public = X25519PublicKey::from(&eph_secret);

    let (_, gw_x25519_public) = identity.to_x25519().unwrap();
    let shared_secret = eph_secret.diffie_hellman(&gw_x25519_public);

    let gateway_id = identity.gateway_id();
    let hkdf = Hkdf::<Sha256>::new(Some(gateway_id), shared_secret.as_bytes());
    let mut aes_key = [0u8; 32];
    hkdf.expand(b"sonde-node-pair-v1", &mut aes_key).unwrap();

    let cipher = Aes256Gcm::new_from_slice(&aes_key).unwrap();
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).unwrap();
    let nonce = GcmNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            nonce,
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad: gateway_id,
            },
        )
        .unwrap();

    let mut out = Vec::new();
    out.extend_from_slice(eph_public.as_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    out
}

fn build_peer_request(
    identity: &GatewayIdentity,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    phone_psk: &[u8; 32],
) -> Vec<u8> {
    let node_key_hint = compute_key_hint(node_psk);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Inner pairing CBOR (keys match build_pairing_cbor in peer_request.rs).
    let cbor = ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(1.into()),
            ciborium::Value::Text(node_id.to_string()),
        ),
        (
            ciborium::Value::Integer(2.into()),
            ciborium::Value::Integer(node_key_hint.into()),
        ),
        (
            ciborium::Value::Integer(3.into()),
            ciborium::Value::Bytes(node_psk.to_vec()),
        ),
        (
            ciborium::Value::Integer(4.into()),
            ciborium::Value::Integer(rf_channel.into()),
        ),
        (
            ciborium::Value::Integer(6.into()),
            ciborium::Value::Integer(ts.into()),
        ),
    ]);
    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(&cbor, &mut cbor_bytes).unwrap();

    // Authenticated request: phone_key_hint(2) + inner_cbor + HMAC(phone_psk, inner_cbor).
    let crypto_sha = sonde_gateway::crypto::RustCryptoSha256;
    let phone_hash = crypto_sha.hash(phone_psk);
    let phone_key_hint_bytes = u16::from_be_bytes([phone_hash[30], phone_hash[31]]);

    let hmac = RustCryptoHmac;
    let phone_hmac = hmac.compute(phone_psk, &cbor_bytes);

    let mut authenticated = Vec::new();
    authenticated.extend_from_slice(&phone_key_hint_bytes.to_be_bytes());
    authenticated.extend_from_slice(&cbor_bytes);
    authenticated.extend_from_slice(&phone_hmac);

    let encrypted_payload = ecdh_encrypt(identity, &authenticated);

    // Outer CBOR: { 1: encrypted_payload }
    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    let header = FrameHeader {
        key_hint: node_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0x1234567890ABCDEF,
    };

    encode_frame(&header, &outer_buf, node_psk, &RustCryptoHmac).unwrap()
}

// ── T-1300  WAKE lifecycle logging ─────────────────────────────────────

/// T-1300: Validates GW-1300 AC3 (WAKE received), AC5 (session created),
/// and AC4 (COMMAND selected).
#[tokio::test]
#[traced_test]
async fn t1300_wake_lifecycle_logging() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-log-1300", 0x1300, [0x13u8; 32]);
    let program_hash = store_test_program(&storage, b"test-bytecode").await;
    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    let frame = node.build_wake(100, 1, &program_hash, 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_some(), "expected COMMAND response");

    // GW-1300 AC3: WAKE received with node_id, seq, battery_mv.
    assert!(logs_contain("WAKE received"));
    assert!(logs_contain("node-log-1300"));
    assert!(logs_contain("battery_mv"));

    // GW-1300 AC5: session created with node_id.
    assert!(logs_contain("session created"));

    // GW-1300 AC4: COMMAND selected with node_id and command_type.
    assert!(logs_contain("COMMAND selected"));
}

// ── T-1301  Session expiry logging ─────────────────────────────────────

/// T-1301: Validates GW-1300 AC6 (session expired).
#[tokio::test]
#[traced_test]
async fn t1301_session_expiry_logging() {
    let session_manager = Arc::new(SessionManager::new(Duration::from_millis(1)));

    // Create a session.
    session_manager
        .create_session("node-log-1301".to_string(), b"peer".to_vec(), 1, 100)
        .await;

    // Wait for the session to expire.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let expired = session_manager.reap_expired().await;
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0], "node-log-1301");

    // GW-1300 AC6: session expired with node_id.
    assert!(logs_contain("session expired"));
    assert!(logs_contain("node-log-1301"));
}

// ── T-1302  PEER_REQUEST logging ───────────────────────────────────────

/// T-1302: Validates GW-1300 AC1 (PEER_REQUEST processed) and AC2 (PEER_ACK
/// frame encoded).
#[tokio::test]
#[traced_test]
async fn t1302_peer_request_logging() {
    let storage = Arc::new(InMemoryStorage::new());
    let identity = GatewayIdentity::generate().unwrap();
    storage.store_gateway_identity(&identity).await.unwrap();

    let crypto_sha = sonde_gateway::crypto::RustCryptoSha256;
    let phone_psk_hash = crypto_sha.hash(&TEST_PHONE_PSK);
    let phone_key_hint = u16::from_be_bytes([phone_psk_hash[30], phone_psk_hash[31]]);
    let phone_record = PhonePskRecord {
        phone_id: 0,
        phone_key_hint,
        psk: Zeroizing::new(TEST_PHONE_PSK),
        label: "test-phone".into(),
        issued_at: std::time::SystemTime::now(),
        status: PhonePskStatus::Active,
    };
    storage.store_phone_psk(&phone_record).await.unwrap();

    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let gw = Gateway::new_with_pending(storage.clone(), pending, session_manager);

    let frame = build_peer_request(
        &identity,
        "node-peer-log",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
    );
    let peer: PeerAddress = b"peer-addr".to_vec();

    let resp = gw.process_frame(&frame, peer).await;
    assert!(resp.is_some(), "expected PEER_ACK response");

    // GW-1300 AC1: PEER_REQUEST processed with result "registered".
    assert!(logs_contain("PEER_REQUEST processed"));
    assert!(logs_contain("registered"));

    // GW-1300 AC2: PEER_ACK frame encoded with node_id.
    assert!(logs_contain("PEER_ACK frame encoded"));
}

// ── T-1303  Modem frame debug logging ──────────────────────────────────

/// T-1303: Validates GW-1302 AC1 (recv frame debug log) and AC2 (send
/// frame debug log).
#[tokio::test]
#[traced_test]
async fn t1303_modem_frame_debug_logging() {
    let (transport, mut server) = common::create_transport_and_server(6).await;

    // AC1: Inject RECV_FRAME and assert debug log.
    let frame_data = vec![
        0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let peer_mac = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac,
        rssi: -50,
        frame_data: frame_data.clone(),
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    let (_data, _peer) = transport.recv().await.unwrap();
    assert!(
        logs_contain("frame received from modem"),
        "expected RECV debug log"
    );

    // AC2: Send a frame and assert debug log.
    let send_frame = vec![
        0x00, 0x01, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let send_peer = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    transport.send(&send_frame, &send_peer).await.unwrap();

    // Read the sent message from the mock server side to avoid blocking.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let _msg = common::read_modem_msg(&mut server, &mut decoder, &mut buf).await;

    assert!(
        logs_contain("frame sent to modem"),
        "expected SEND debug log"
    );
}
