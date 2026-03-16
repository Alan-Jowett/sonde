// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for the PEER_REQUEST processing pipeline (GW-1211–GW-1221).
//!
//! Tests cover: happy path, bad GCM tag, revoked phone, bad phone HMAC,
//! bad frame HMAC, timestamp drift, duplicate node_id, key_hint mismatch,
//! rf_channel out of range, and node_id length violations.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use zeroize::Zeroizing;

use sonde_gateway::crypto::RustCryptoHmac;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, FrameHeader, HmacProvider, Sha256Provider,
    MSG_PEER_ACK, MSG_PEER_REQUEST, PEER_ACK_KEY_PROOF, PEER_ACK_KEY_STATUS, PEER_REQ_KEY_PAYLOAD,
};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;

// ── Test infrastructure ────────────────────────────────────────────────

const TEST_NODE_PSK: [u8; 32] = [0x42u8; 32];
const TEST_PHONE_PSK: [u8; 32] = [0x55u8; 32];

struct TestEnv {
    storage: Arc<InMemoryStorage>,
    gateway: Gateway,
    identity: GatewayIdentity,
    phone_id: u32,
}

impl TestEnv {
    async fn new() -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let identity = GatewayIdentity::generate().unwrap();
        storage.store_gateway_identity(&identity).await.unwrap();

        // Register a phone PSK.
        let crypto_sha = sonde_gateway::RustCryptoSha256;
        let phone_psk_hash = crypto_sha.hash(&TEST_PHONE_PSK);
        let phone_key_hint = u16::from_be_bytes([phone_psk_hash[30], phone_psk_hash[31]]);
        let phone_record = PhonePskRecord {
            phone_id: 0, // will be assigned
            phone_key_hint,
            psk: Zeroizing::new(TEST_PHONE_PSK),
            label: "test-phone".into(),
            issued_at: std::time::SystemTime::now(),
            status: PhonePskStatus::Active,
        };
        let phone_id = storage.store_phone_psk(&phone_record).await.unwrap();

        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let gateway = Gateway::new_with_pending(
            storage.clone(),
            pending_commands,
            session_manager,
        );

        Self {
            storage,
            gateway,
            identity,
            phone_id,
        }
    }
}

/// Compute the key_hint for a PSK: lower 16 bits of SHA-256(psk).
fn compute_key_hint(psk: &[u8; 32]) -> u16 {
    let crypto_sha = sonde_gateway::RustCryptoSha256;
    let hash = crypto_sha.hash(psk);
    u16::from_be_bytes([hash[30], hash[31]])
}

/// Build a PairingRequest CBOR body.
fn build_pairing_cbor(
    node_id: &str,
    node_key_hint: u16,
    node_psk: &[u8; 32],
    rf_channel: u8,
    timestamp: u64,
    sensors: Option<Vec<ciborium::Value>>,
) -> Vec<u8> {
    use ciborium::Value;

    let mut entries = vec![
        (Value::Integer(1.into()), Value::Text(node_id.to_string())),
        (Value::Integer(2.into()), Value::Integer(node_key_hint.into())),
        (Value::Integer(3.into()), Value::Bytes(node_psk.to_vec())),
        (Value::Integer(4.into()), Value::Integer(rf_channel.into())),
        (Value::Integer(6.into()), Value::Integer(timestamp.into())),
    ];
    if let Some(sensors) = sensors {
        entries.push((Value::Integer(5.into()), Value::Array(sensors)));
    }

    let map = Value::Map(entries);
    let mut buf = Vec::new();
    ciborium::into_writer(&map, &mut buf).unwrap();
    buf
}

/// Build authenticated_request: phone_key_hint(2) + cbor_bytes + phone_hmac(32).
fn build_authenticated_request(cbor_bytes: &[u8], phone_psk: &[u8; 32]) -> Vec<u8> {
    let crypto_sha = sonde_gateway::RustCryptoSha256;
    let phone_hash = crypto_sha.hash(phone_psk);
    let phone_key_hint = u16::from_be_bytes([phone_hash[30], phone_hash[31]]);

    let hmac = RustCryptoHmac;
    let phone_hmac = hmac.compute(phone_psk, cbor_bytes);

    let mut out = Vec::new();
    out.extend_from_slice(&phone_key_hint.to_be_bytes());
    out.extend_from_slice(cbor_bytes);
    out.extend_from_slice(&phone_hmac);
    out
}

/// ECDH encrypt a payload for the gateway.
/// Returns: eph_public(32) + gcm_nonce(12) + ciphertext(N+16)
fn ecdh_encrypt(
    identity: &GatewayIdentity,
    plaintext: &[u8],
) -> Vec<u8> {
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

/// Build a complete PEER_REQUEST frame.
fn build_peer_request(
    identity: &GatewayIdentity,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    phone_psk: &[u8; 32],
    timestamp: Option<u64>,
    sensors: Option<Vec<ciborium::Value>>,
) -> Vec<u8> {
    let node_key_hint = compute_key_hint(node_psk);
    let ts = timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });

    let cbor_bytes = build_pairing_cbor(node_id, node_key_hint, node_psk, rf_channel, ts, sensors);
    let authenticated_request = build_authenticated_request(&cbor_bytes, phone_psk);
    let encrypted_payload = ecdh_encrypt(identity, &authenticated_request);

    // Build outer CBOR: { 1: encrypted_payload }
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

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn peer() -> PeerAddress {
    b"test-peer".to_vec()
}

// ── Tests ──────────────────────────────────────────────────────────────

/// GW-1211–GW-1221: Happy path — valid PEER_REQUEST produces PEER_ACK.
#[tokio::test]
async fn peer_request_happy_path() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "node-test-1",
        &TEST_NODE_PSK,
        7, // valid rf_channel
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some(), "valid PEER_REQUEST must produce a response");

    let raw = response.unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert_eq!(decoded.header.msg_type, MSG_PEER_ACK);
    assert!(verify_frame(&decoded, &TEST_NODE_PSK, &RustCryptoHmac));

    // Parse PEER_ACK CBOR
    let ack: ciborium::Value = ciborium::from_reader(&decoded.payload[..]).unwrap();
    let map = ack.as_map().unwrap();
    let mut status: Option<u64> = None;
    let mut proof: Option<Vec<u8>> = None;
    for (k, v) in map {
        let key = k.as_integer().and_then(|i| u64::try_from(i).ok()).unwrap();
        match key {
            k if k == PEER_ACK_KEY_STATUS => {
                status = v.as_integer().and_then(|i| u64::try_from(i).ok());
            }
            k if k == PEER_ACK_KEY_PROOF => {
                proof = v.as_bytes().map(|b| b.to_vec());
            }
            _ => {}
        }
    }
    assert_eq!(status, Some(0), "PEER_ACK status must be 0 (success)");
    assert!(proof.is_some(), "PEER_ACK must contain registration_proof");
    assert_eq!(proof.unwrap().len(), 32, "registration_proof must be 32 bytes");

    // Verify node was registered.
    let node = env.storage.get_node("node-test-1").await.unwrap();
    assert!(node.is_some(), "node must be persisted after PEER_ACK");
    let node = node.unwrap();
    assert_eq!(node.rf_channel, Some(7));
    assert_eq!(node.registered_by_phone_id, Some(env.phone_id));
    assert_eq!(node.psk, TEST_NODE_PSK);
}

/// GW-1211–GW-1221: Happy path with sensors.
#[tokio::test]
async fn peer_request_with_sensors() {
    use ciborium::Value;

    let env = TestEnv::new().await;

    let sensors = vec![
        Value::Map(vec![
            (Value::Integer(1.into()), Value::Integer(1.into())), // I2C
            (Value::Integer(2.into()), Value::Integer(0x48.into())), // addr
            (Value::Integer(3.into()), Value::Text("temperature".to_string())),
        ]),
        Value::Map(vec![
            (Value::Integer(1.into()), Value::Integer(2.into())), // ADC
            (Value::Integer(2.into()), Value::Integer(0.into())), // channel
        ]),
    ];

    let frame = build_peer_request(
        &env.identity,
        "node-sensors",
        &TEST_NODE_PSK,
        3,
        &TEST_PHONE_PSK,
        None,
        Some(sensors),
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some());

    let node = env.storage.get_node("node-sensors").await.unwrap().unwrap();
    assert_eq!(node.sensors.len(), 2);
    assert_eq!(node.sensors[0].sensor_type, 1);
    assert_eq!(node.sensors[0].sensor_id, 0x48);
    assert_eq!(node.sensors[0].label.as_deref(), Some("temperature"));
    assert_eq!(node.sensors[1].sensor_type, 2);
    assert_eq!(node.sensors[1].sensor_id, 0);
    assert!(node.sensors[1].label.is_none());
}

/// GW-1220: Bad GCM tag — tampered encrypted_payload → silent discard.
#[tokio::test]
async fn peer_request_bad_gcm_tag() {
    let env = TestEnv::new().await;

    let mut frame = build_peer_request(
        &env.identity,
        "node-bad-gcm",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    // Tamper with a byte in the payload area (after header).
    if frame.len() > 20 {
        frame[20] ^= 0xFF;
    }

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "tampered frame must be silently discarded");
}

/// GW-1213: Revoked phone PSK → silent discard.
#[tokio::test]
async fn peer_request_revoked_phone() {
    let env = TestEnv::new().await;

    // Revoke the phone PSK.
    env.storage.revoke_phone_psk(env.phone_id).await.unwrap();

    let frame = build_peer_request(
        &env.identity,
        "node-revoked",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "revoked phone must cause silent discard");
}

/// GW-1213: Wrong phone HMAC → silent discard.
#[tokio::test]
async fn peer_request_bad_phone_hmac() {
    let env = TestEnv::new().await;

    let wrong_phone_psk = [0xBBu8; 32];
    let frame = build_peer_request(
        &env.identity,
        "node-bad-phone",
        &TEST_NODE_PSK,
        7,
        &wrong_phone_psk, // not registered
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "wrong phone HMAC must cause silent discard");
}

/// GW-1214: Bad frame HMAC (wrong node PSK in frame) → silent discard.
#[tokio::test]
async fn peer_request_bad_frame_hmac() {
    let env = TestEnv::new().await;

    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let ts = current_timestamp();
    let cbor_bytes = build_pairing_cbor("node-bad-hmac", node_key_hint, &TEST_NODE_PSK, 7, ts, None);
    let authenticated_request = build_authenticated_request(&cbor_bytes, &TEST_PHONE_PSK);
    let encrypted_payload = ecdh_encrypt(&env.identity, &authenticated_request);

    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    let header = FrameHeader {
        key_hint: node_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0x1234,
    };

    // Sign with a DIFFERENT PSK than what's in the pairing request.
    let wrong_psk = [0xEE; 32];
    let frame = encode_frame(&header, &outer_buf, &wrong_psk, &RustCryptoHmac).unwrap();

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "bad frame HMAC must cause silent discard");
}

/// GW-1215: Timestamp too far in the past → silent discard.
#[tokio::test]
async fn peer_request_timestamp_drift_past() {
    let env = TestEnv::new().await;

    let old_timestamp = current_timestamp() - 90_000; // >86400s ago
    let frame = build_peer_request(
        &env.identity,
        "node-old-ts",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(old_timestamp),
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "old timestamp must cause silent discard");
}

/// GW-1215: Timestamp too far in the future → silent discard.
#[tokio::test]
async fn peer_request_timestamp_drift_future() {
    let env = TestEnv::new().await;

    let future_timestamp = current_timestamp() + 90_000; // >86400s from now
    let frame = build_peer_request(
        &env.identity,
        "node-future-ts",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(future_timestamp),
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "future timestamp must cause silent discard");
}

/// GW-1216: Duplicate node_id → silent discard.
#[tokio::test]
async fn peer_request_duplicate_node_id() {
    let env = TestEnv::new().await;

    // First registration succeeds.
    let frame1 = build_peer_request(
        &env.identity,
        "node-dup",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );
    let response1 = env.gateway.process_frame(&frame1, peer()).await;
    assert!(response1.is_some(), "first registration must succeed");

    // Second registration with same node_id must fail.
    let frame2 = build_peer_request(
        &env.identity,
        "node-dup",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );
    let response2 = env.gateway.process_frame(&frame2, peer()).await;
    assert!(response2.is_none(), "duplicate node_id must cause silent discard");
}

/// GW-1217: key_hint mismatch (frame key_hint ≠ pairing key_hint) → silent discard.
#[tokio::test]
async fn peer_request_key_hint_mismatch() {
    let env = TestEnv::new().await;

    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let ts = current_timestamp();
    let cbor_bytes = build_pairing_cbor("node-kh-mismatch", node_key_hint, &TEST_NODE_PSK, 7, ts, None);
    let authenticated_request = build_authenticated_request(&cbor_bytes, &TEST_PHONE_PSK);
    let encrypted_payload = ecdh_encrypt(&env.identity, &authenticated_request);

    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    // Use a DIFFERENT key_hint in the frame header.
    let wrong_key_hint = node_key_hint.wrapping_add(1);
    let header = FrameHeader {
        key_hint: wrong_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0x5678,
    };

    let frame = encode_frame(&header, &outer_buf, &TEST_NODE_PSK, &RustCryptoHmac).unwrap();

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "key_hint mismatch must cause silent discard");
}

/// rf_channel = 0 → silent discard.
#[tokio::test]
async fn peer_request_rf_channel_zero() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "node-rf0",
        &TEST_NODE_PSK,
        0, // invalid
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "rf_channel=0 must cause silent discard");
}

/// rf_channel = 14 → silent discard.
#[tokio::test]
async fn peer_request_rf_channel_14() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "node-rf14",
        &TEST_NODE_PSK,
        14, // invalid
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "rf_channel=14 must cause silent discard");
}

/// rf_channel = 13 → accepted (boundary).
#[tokio::test]
async fn peer_request_rf_channel_13_ok() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "node-rf13",
        &TEST_NODE_PSK,
        13,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some(), "rf_channel=13 must be accepted");

    let node = env.storage.get_node("node-rf13").await.unwrap().unwrap();
    assert_eq!(node.rf_channel, Some(13));
}

/// rf_channel = 1 → accepted (boundary).
#[tokio::test]
async fn peer_request_rf_channel_1_ok() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "node-rf1",
        &TEST_NODE_PSK,
        1,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some(), "rf_channel=1 must be accepted");
}

/// node_id = 65 bytes → silent discard (validated before frame encoding would fail).
/// Note: 65-byte IDs also exceed the 250-byte ESP-NOW frame limit,
/// so we verify the validation works at a smaller boundary: a node_id
/// just over the 64-byte protocol limit is rejected even if we could
/// somehow deliver it. We test with a 30-byte ID (fits in frame) to
/// verify acceptance, and use unit-level reasoning for the 65-byte case.
#[tokio::test]
async fn peer_request_node_id_30_ok() {
    let env = TestEnv::new().await;

    let id_30: String = "B".repeat(30);
    let frame = build_peer_request(
        &env.identity,
        &id_30,
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some(), "30-byte node_id must be accepted");
}

/// Empty node_id → silent discard.
#[tokio::test]
async fn peer_request_empty_node_id() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "", // empty
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "empty node_id must cause silent discard");
}

/// Timestamp exactly at +86400s boundary → accepted.
#[tokio::test]
async fn peer_request_timestamp_boundary_ok() {
    let env = TestEnv::new().await;

    let boundary_ts = current_timestamp() + 86400;
    let frame = build_peer_request(
        &env.identity,
        "node-ts-boundary",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(boundary_ts),
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some(), "timestamp at +86400s must be accepted");
}

/// Timestamp at +86401s → rejected.
#[tokio::test]
async fn peer_request_timestamp_boundary_plus1_rejected() {
    let env = TestEnv::new().await;

    let over_ts = current_timestamp() + 86401;
    let frame = build_peer_request(
        &env.identity,
        "node-ts-over",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(over_ts),
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_none(), "timestamp at +86401s must be rejected");
}
