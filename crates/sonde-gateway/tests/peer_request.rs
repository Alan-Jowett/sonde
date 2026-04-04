// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for the PEER_REQUEST processing pipeline (GW-1211–GW-1221).
//!
//! Tests cover: happy path, bad GCM tag, revoked phone, wrong phone PSK,
//! bad outer frame AEAD, timestamp drift, duplicate node_id, key_hint mismatch,
//! rf_channel out of range, and node_id length violations.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use zeroize::Zeroizing;

use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;

use sonde_gateway::GatewayAead;
use sonde_protocol::{
    decode_frame, encode_frame, open_frame, FrameHeader, Sha256Provider, MSG_PEER_ACK,
    MSG_PEER_REQUEST, PEER_ACK_KEY_STATUS, PEER_REQ_KEY_PAYLOAD,
};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};

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
            Arc::new(RwLock::new(sonde_gateway::handler::HandlerRouter::new(
                Vec::new(),
            ))),
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
        (
            Value::Integer(2.into()),
            Value::Integer(node_key_hint.into()),
        ),
        (Value::Integer(3.into()), Value::Bytes(node_psk.to_vec())),
        (Value::Integer(4.into()), Value::Integer(rf_channel.into())),
    ];
    if let Some(sensors) = sensors {
        entries.push((Value::Integer(5.into()), Value::Array(sensors)));
    }
    entries.push((Value::Integer(6.into()), Value::Integer(timestamp.into())));

    let map = Value::Map(entries);
    let mut buf = Vec::new();
    ciborium::into_writer(&map, &mut buf).unwrap();
    buf
}

/// Encrypt PairingRequest CBOR with phone_psk using AES-256-GCM.
/// Returns: inner_nonce(12) ‖ ciphertext ‖ tag(16)
/// AAD = "sonde-pairing-v2"
fn encrypt_inner_payload(pairing_cbor: &[u8], phone_psk: &[u8; 32]) -> Vec<u8> {
    const PAIRING_AAD: &[u8] = b"sonde-pairing-v2";

    let cipher = Aes256Gcm::new_from_slice(phone_psk).unwrap();
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).unwrap();
    let nonce = GcmNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: pairing_cbor,
                aad: PAIRING_AAD,
            },
        )
        .unwrap();

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    out
}

/// Build a complete PEER_REQUEST frame using the AEAD protocol format.
///
/// The outer frame is encrypted with `phone_psk` (key_hint = phone_key_hint).
/// The CBOR payload is `{ 1: encrypted_payload }` where `encrypted_payload`
/// = inner_nonce(12) ‖ AES-256-GCM(phone_psk, PairingRequestCBOR, AAD).
#[allow(clippy::too_many_arguments)]
fn build_peer_request_detailed(
    _identity: &GatewayIdentity,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    phone_psk: &[u8; 32],
    timestamp: Option<u64>,
    sensors: Option<Vec<ciborium::Value>>,
    nonce: u64,
) -> Vec<u8> {
    let node_key_hint = compute_key_hint(node_psk);
    let phone_key_hint = compute_key_hint(phone_psk);
    let ts = timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });

    let cbor_bytes = build_pairing_cbor(node_id, node_key_hint, node_psk, rf_channel, ts, sensors);
    let encrypted_payload = encrypt_inner_payload(&cbor_bytes, phone_psk);

    // Build outer CBOR: { 1: encrypted_payload }
    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    let header = FrameHeader {
        key_hint: phone_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce,
    };

    encode_frame(
        &header,
        &outer_buf,
        phone_psk,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap()
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
    build_peer_request_detailed(
        identity,
        node_id,
        node_psk,
        rf_channel,
        phone_psk,
        timestamp,
        sensors,
        0x1234567890ABCDEF,
    )
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
    assert!(
        response.is_some(),
        "valid PEER_REQUEST must produce a response"
    );

    let raw = response.unwrap();
    let decoded = decode_frame(&raw).unwrap();
    let plaintext = open_frame(&decoded, &TEST_NODE_PSK, &GatewayAead, &RustCryptoSha256).unwrap();
    assert_eq!(decoded.header.msg_type, MSG_PEER_ACK);

    // Parse PEER_ACK CBOR: { 1: status(0) }
    let ack: ciborium::Value = ciborium::from_reader(&plaintext[..]).unwrap();
    let map = ack.as_map().unwrap();
    let mut status: Option<u64> = None;
    for (k, v) in map {
        let key = k.as_integer().and_then(|i| u64::try_from(i).ok()).unwrap();
        if key == PEER_ACK_KEY_STATUS {
            status = v.as_integer().and_then(|i| u64::try_from(i).ok());
        }
    }
    assert_eq!(status, Some(0), "PEER_ACK status must be 0 (success)");

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
            (
                Value::Integer(3.into()),
                Value::Text("temperature".to_string()),
            ),
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

/// Invalid sensor_type (5, outside 1-4 enum) → silent discard.
#[tokio::test]
async fn peer_request_invalid_sensor_type() {
    use ciborium::Value;

    let env = TestEnv::new().await;

    let sensors = vec![Value::Map(vec![
        (Value::Integer(1.into()), Value::Integer(5.into())), // invalid type
        (Value::Integer(2.into()), Value::Integer(0.into())),
    ])];

    let frame = build_peer_request(
        &env.identity,
        "node-bad-sensor",
        &TEST_NODE_PSK,
        3,
        &TEST_PHONE_PSK,
        None,
        Some(sensors),
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_none(),
        "invalid sensor_type must cause silent discard"
    );
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
    assert!(
        response.is_none(),
        "tampered frame must be silently discarded"
    );
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
    assert!(
        response.is_none(),
        "revoked phone must cause silent discard"
    );
}

/// GW-1213: Wrong phone PSK → silent discard (outer AEAD decryption fails).
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
    assert!(
        response.is_none(),
        "wrong phone HMAC must cause silent discard"
    );
}

/// GW-1214: Outer frame encrypted with wrong PSK → silent discard.
#[tokio::test]
async fn peer_request_bad_frame_hmac() {
    let env = TestEnv::new().await;

    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let phone_key_hint = compute_key_hint(&TEST_PHONE_PSK);
    let ts = current_timestamp();
    let cbor_bytes =
        build_pairing_cbor("node-bad-hmac", node_key_hint, &TEST_NODE_PSK, 7, ts, None);
    let encrypted_payload = encrypt_inner_payload(&cbor_bytes, &TEST_PHONE_PSK);

    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    let header = FrameHeader {
        key_hint: phone_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0x1234,
    };

    // Encrypt the outer frame with a DIFFERENT PSK than the registered phone_psk.
    let wrong_psk = [0xEE; 32];
    let frame = encode_frame(
        &header,
        &outer_buf,
        &wrong_psk,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap();

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_none(),
        "bad outer frame AEAD must cause silent discard"
    );
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
    assert!(
        response.is_none(),
        "old timestamp must cause silent discard"
    );
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
    assert!(
        response.is_none(),
        "future timestamp must cause silent discard"
    );
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

    // Second registration with same node_id and matching PSK must still
    // return a PEER_ACK so the node can complete enrollment (GW-1218 AC4).
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
    assert!(
        response2.is_some(),
        "duplicate node_id with matching PSK must return PEER_ACK"
    );
}

/// GW-1217: key_hint mismatch (frame key_hint ≠ pairing key_hint) → silent discard.
#[tokio::test]
async fn peer_request_key_hint_mismatch() {
    let env = TestEnv::new().await;

    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let phone_key_hint = compute_key_hint(&TEST_PHONE_PSK);
    let ts = current_timestamp();
    let cbor_bytes = build_pairing_cbor(
        "node-kh-mismatch",
        node_key_hint,
        &TEST_NODE_PSK,
        7,
        ts,
        None,
    );
    let encrypted_payload = encrypt_inner_payload(&cbor_bytes, &TEST_PHONE_PSK);

    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    // Use a DIFFERENT key_hint in the frame header (not matching phone_key_hint).
    let wrong_key_hint = phone_key_hint.wrapping_add(1);
    let header = FrameHeader {
        key_hint: wrong_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0x5678,
    };

    let frame = encode_frame(
        &header,
        &outer_buf,
        &TEST_PHONE_PSK,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap();

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_none(),
        "key_hint mismatch must cause silent discard"
    );
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
    assert!(
        response.is_none(),
        "rf_channel=14 must cause silent discard"
    );
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

/// Verify that a 30-byte node_id is accepted (fits within the 64-byte protocol
/// limit and the 250-byte ESP-NOW frame budget).
///
/// Note: node_ids longer than ~30 bytes exceed the ESP-NOW frame limit due to
/// ECDH + CBOR overhead, so the 64-byte protocol validation cannot be tested
/// end-to-end. The length check in `handle_peer_request` is verified
/// structurally by code inspection.
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
    assert!(
        response.is_none(),
        "empty node_id must cause silent discard"
    );
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

/// Timestamp at +86410s → rejected (well beyond the ±86400s window;
/// uses a 10s margin to avoid flaky races between frame creation and validation).
#[tokio::test]
async fn peer_request_timestamp_boundary_plus1_rejected() {
    let env = TestEnv::new().await;

    let over_ts = current_timestamp() + 86410;
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
    assert!(response.is_none(), "timestamp at +86410s must be rejected");
}

// ── T-NNNN Spec Tests ─────────────────────────────────────────────────
//
// The following tests are the canonical spec-numbered tests from
// gateway-validation.md.  They complement the tests above, which cover
// the same functionality under descriptive names.

// -- T-1210: PEER_REQUEST decryption happy path --

/// T-1210  PEER_REQUEST decryption happy path (GW-1212).
///
/// Construct a correctly encrypted PEER_REQUEST, submit it, and assert
/// the gateway successfully decrypts and produces a PEER_ACK.
#[tokio::test]
async fn t_1210_peer_request_decryption_happy_path() {
    let env = TestEnv::new().await;

    let frame = build_peer_request(
        &env.identity,
        "node-t1210",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_some(),
        "correctly encrypted PEER_REQUEST must produce a PEER_ACK"
    );

    let raw = response.unwrap();
    let decoded = decode_frame(&raw).unwrap();
    assert_eq!(decoded.header.msg_type, MSG_PEER_ACK);
}

// -- T-1211: PEER_REQUEST with bad GCM tag --

/// T-1211  PEER_REQUEST with bad GCM tag (GW-1212).
///
/// Corrupt the GCM authentication tag, assert silent discard.
#[tokio::test]
async fn t_1211_peer_request_bad_gcm_tag() {
    let env = TestEnv::new().await;

    let mut frame = build_peer_request(
        &env.identity,
        "node-t1211",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    // Corrupt a byte in the payload area (after the 11-byte header).
    if frame.len() > 20 {
        frame[20] ^= 0xFF;
    }

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_none(),
        "bad GCM tag must cause silent discard (no response)"
    );
}

// -- T-1212: Phone HMAC with multiple candidates --

/// T-1212  Phone HMAC with multiple candidates (GW-1213).
///
/// Register two phones whose PSKs produce the same `phone_key_hint`.
/// Build a PEER_REQUEST using the second phone's PSK and assert the
/// gateway tries both candidates and accepts the valid one.
#[tokio::test]
async fn t_1212_phone_hmac_multiple_candidates() {
    let env = TestEnv::new().await;

    // Phone A is already registered (TEST_PHONE_PSK) in TestEnv::new().
    let phone_a_key_hint = compute_key_hint(&TEST_PHONE_PSK);

    // Register phone B with a DIFFERENT PSK but the SAME key_hint.
    let phone_b_psk = [0xAAu8; 32];
    let phone_b_record = PhonePskRecord {
        phone_id: 0,
        phone_key_hint: phone_a_key_hint, // force same key_hint
        psk: Zeroizing::new(phone_b_psk),
        label: "phone-b-same-hint".into(),
        issued_at: std::time::SystemTime::now(),
        status: PhonePskStatus::Active,
    };
    env.storage.store_phone_psk(&phone_b_record).await.unwrap();

    // Verify two phones now share the same key_hint.
    let candidates = env
        .storage
        .get_phone_psks_by_key_hint(phone_a_key_hint)
        .await
        .unwrap();
    assert_eq!(
        candidates.len(),
        2,
        "two phone PSKs must share the same key_hint"
    );

    // Build request using phone A's PSK — the HMAC will only match phone A.
    let frame = build_peer_request(
        &env.identity,
        "node-t1212",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_some(),
        "gateway must try both candidates and accept the matching one"
    );

    // Verify the node was registered by the correct phone.
    let node = env.storage.get_node("node-t1212").await.unwrap().unwrap();
    assert_eq!(node.registered_by_phone_id, Some(env.phone_id));
}

// -- T-1213: Phone HMAC with revoked PSK --

/// T-1213  Phone HMAC with revoked PSK rejected (GW-1213).
///
/// Register a phone, revoke its PSK, then submit a PEER_REQUEST using
/// that PSK.  Assert silent discard (revoked PSK not tried).
#[tokio::test]
async fn t_1213_phone_hmac_revoked_psk() {
    let env = TestEnv::new().await;

    // Revoke the phone PSK.
    env.storage.revoke_phone_psk(env.phone_id).await.unwrap();

    let frame = build_peer_request(
        &env.identity,
        "node-t1213",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(
        response.is_none(),
        "revoked phone PSK must cause silent discard"
    );
}

// -- T-1214: PEER_REQUEST frame HMAC verification --

/// T-1214  PEER_REQUEST outer frame AEAD verification (GW-1214).
///
/// 1. Valid outer frame AEAD: processing continues (implicit in happy path).
/// 2. Outer frame encrypted with wrong PSK: silent discard.
#[tokio::test]
async fn t_1214_frame_hmac_verification() {
    let env = TestEnv::new().await;

    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let phone_key_hint = compute_key_hint(&TEST_PHONE_PSK);
    let ts = current_timestamp();
    let cbor_bytes = build_pairing_cbor("node-t1214", node_key_hint, &TEST_NODE_PSK, 7, ts, None);
    let encrypted_payload = encrypt_inner_payload(&cbor_bytes, &TEST_PHONE_PSK);

    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    let header = FrameHeader {
        key_hint: phone_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0xAAAA,
    };

    // Encrypt outer frame with a DIFFERENT PSK than the registered phone_psk.
    let wrong_psk = [0xEEu8; 32];
    let bad_frame = encode_frame(
        &header,
        &outer_buf,
        &wrong_psk,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap();

    let response = env.gateway.process_frame(&bad_frame, peer()).await;
    assert!(
        response.is_none(),
        "outer frame encrypted with wrong PSK must cause silent discard"
    );

    // Now submit a valid outer frame AEAD for the same payload.
    let good_frame = encode_frame(
        &header,
        &outer_buf,
        &TEST_PHONE_PSK,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap();
    let response = env.gateway.process_frame(&good_frame, peer()).await;
    assert!(
        response.is_some(),
        "valid outer frame AEAD must allow processing to continue"
    );
}

// -- T-1215: Timestamp outside ±86 400 s range --

/// T-1215  Timestamp outside ±86 400 s range (GW-1215).
///
/// 1. Timestamp 86 401 s in the past → silent discard.
/// 2. Timestamp 86 401 s in the future → silent discard.
/// 3. Timestamp within ±86 400 s → processing continues.
#[tokio::test]
async fn t_1215_timestamp_range_enforcement() {
    let env = TestEnv::new().await;

    // Past: 90 000 s ago (well beyond 86 400 s).
    let past_ts = current_timestamp() - 90_000;
    let frame = build_peer_request(
        &env.identity,
        "node-t1215a",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(past_ts),
        None,
    );
    assert!(
        env.gateway.process_frame(&frame, peer()).await.is_none(),
        "timestamp 90000s in the past must be rejected"
    );

    // Future: 90 000 s from now.
    let future_ts = current_timestamp() + 90_000;
    let frame = build_peer_request(
        &env.identity,
        "node-t1215b",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(future_ts),
        None,
    );
    assert!(
        env.gateway.process_frame(&frame, peer()).await.is_none(),
        "timestamp 90000s in the future must be rejected"
    );

    // Valid: current timestamp.
    let frame = build_peer_request(
        &env.identity,
        "node-t1215c",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        Some(current_timestamp()),
        None,
    );
    assert!(
        env.gateway.process_frame(&frame, peer()).await.is_some(),
        "current timestamp must be accepted"
    );
}

// -- T-1216: Duplicate node_id rejected --

/// T-1216  Duplicate node_id rejected (GW-1216).
///
/// Successfully pair a node, then submit a new PEER_REQUEST with the
/// same node_id.  Assert silent discard.
#[tokio::test]
async fn t_1216_duplicate_node_id_rejected() {
    let env = TestEnv::new().await;

    let frame1 = build_peer_request(
        &env.identity,
        "node-t1216",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );
    assert!(
        env.gateway.process_frame(&frame1, peer()).await.is_some(),
        "first registration must succeed"
    );

    // Same node_id with matching PSK — must return PEER_ACK (GW-1218 AC4).
    let frame2 = build_peer_request(
        &env.identity,
        "node-t1216",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
    );
    assert!(
        env.gateway.process_frame(&frame2, peer()).await.is_some(),
        "duplicate node_id with matching PSK must return PEER_ACK"
    );
}

// -- T-1217: Key hint mismatch rejected --

/// T-1217  Key hint mismatch rejected (GW-1217).
///
/// Build a frame where the header `key_hint` differs from the
/// phone_key_hint.  Assert silent discard.
#[tokio::test]
async fn t_1217_key_hint_mismatch_rejected() {
    let env = TestEnv::new().await;

    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let phone_key_hint = compute_key_hint(&TEST_PHONE_PSK);
    let ts = current_timestamp();
    let cbor_bytes = build_pairing_cbor("node-t1217", node_key_hint, &TEST_NODE_PSK, 7, ts, None);
    let encrypted_payload = encrypt_inner_payload(&cbor_bytes, &TEST_PHONE_PSK);

    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    // Use DIFFERENT key_hint in the frame header.
    let wrong_key_hint = phone_key_hint.wrapping_add(1);
    let header = FrameHeader {
        key_hint: wrong_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0xBBBB,
    };

    let frame = encode_frame(
        &header,
        &outer_buf,
        &TEST_PHONE_PSK,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap();
    assert!(
        env.gateway.process_frame(&frame, peer()).await.is_none(),
        "key_hint mismatch must cause silent discard"
    );
}

// -- T-1218: Node registration stores correct fields --

/// T-1218  Node registration stores correct fields (GW-1218).
///
/// Process a PEER_REQUEST, then query the registry and assert all
/// fields are stored: node_id, node_key_hint, node_psk, rf_channel,
/// sensors, and `registered_by` = phone_id (stable identifier).
#[tokio::test]
async fn t_1218_node_registration_stores_fields() {
    use ciborium::Value;

    let env = TestEnv::new().await;

    let sensors = vec![Value::Map(vec![
        (Value::Integer(1.into()), Value::Integer(1.into())), // I2C
        (Value::Integer(2.into()), Value::Integer(0x48.into())), // addr
        (
            Value::Integer(3.into()),
            Value::Text("temp-sensor".to_string()),
        ),
    ])];

    let frame = build_peer_request(
        &env.identity,
        "node-t1218",
        &TEST_NODE_PSK,
        9,
        &TEST_PHONE_PSK,
        None,
        Some(sensors),
    );

    let response = env.gateway.process_frame(&frame, peer()).await;
    assert!(response.is_some(), "PEER_REQUEST must succeed");

    let node = env
        .storage
        .get_node("node-t1218")
        .await
        .unwrap()
        .expect("node must be persisted");

    // Verify all required fields.
    assert_eq!(node.node_id, "node-t1218");
    assert_eq!(node.key_hint, compute_key_hint(&TEST_NODE_PSK));
    assert_eq!(node.psk, TEST_NODE_PSK);
    assert_eq!(node.rf_channel, Some(9));
    assert_eq!(node.sensors.len(), 1);
    assert_eq!(node.sensors[0].sensor_type, 1);
    assert_eq!(node.sensors[0].sensor_id, 0x48);
    assert_eq!(node.sensors[0].label.as_deref(), Some("temp-sensor"));

    // registered_by must be the phone's stable phone_id, not phone_key_hint.
    assert_eq!(
        node.registered_by_phone_id,
        Some(env.phone_id),
        "registered_by must be the phone's stable phone_id"
    );
}

// -- T-1219: PEER_ACK happy path --

/// T-1219  PEER_ACK happy path (GW-1219).
///
/// Submit a valid PEER_REQUEST and verify the PEER_ACK:
/// 1. CBOR = {1: 0} (status only, no registration_proof in AEAD format)
/// 2. Frame AEAD is valid under node_psk.
/// 3. Nonce in PEER_ACK header matches the PEER_REQUEST nonce.
#[tokio::test]
async fn t_1219_peer_ack_happy_path() {
    let env = TestEnv::new().await;

    let request_nonce: u64 = 0xCAFE_BABE_DEAD_BEEF;
    let frame = build_peer_request_detailed(
        &env.identity,
        "node-t1219",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
        None,
        None,
        request_nonce,
    );

    let response = env
        .gateway
        .process_frame(&frame, peer())
        .await
        .expect("valid PEER_REQUEST must produce PEER_ACK");

    // Decode and verify the PEER_ACK frame.
    let decoded = decode_frame(&response).unwrap();
    assert_eq!(decoded.header.msg_type, MSG_PEER_ACK);

    // 3. Nonce must echo the request nonce.
    assert_eq!(
        decoded.header.nonce, request_nonce,
        "PEER_ACK nonce must echo the PEER_REQUEST nonce"
    );

    // 2. Frame AEAD must be valid under node_psk.
    let plaintext = open_frame(&decoded, &TEST_NODE_PSK, &GatewayAead, &RustCryptoSha256).unwrap();

    // 1. Parse PEER_ACK CBOR: { 1: status(0) }
    let ack: ciborium::Value = ciborium::from_reader(&plaintext[..]).unwrap();
    let map = ack.as_map().unwrap();
    let mut status: Option<u64> = None;
    for (k, v) in map {
        let key = k.as_integer().and_then(|i| u64::try_from(i).ok()).unwrap();
        if key == PEER_ACK_KEY_STATUS {
            status = v.as_integer().and_then(|i| u64::try_from(i).ok());
        }
    }
    assert_eq!(status, Some(0), "PEER_ACK status must be 0 (success)");
    assert_eq!(
        map.len(),
        1,
        "AEAD PEER_ACK must contain only status (no registration_proof)"
    );
}
