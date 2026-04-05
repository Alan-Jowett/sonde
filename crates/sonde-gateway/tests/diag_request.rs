// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for the DIAG_REQUEST processing pipeline (GW-1700–1706).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use zeroize::Zeroizing;

use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;

use sonde_gateway::GatewayAead;
use sonde_protocol::{
    decode_frame, encode_frame, open_frame, FrameHeader, GatewayMessage, NodeMessage,
    Sha256Provider, MSG_DIAG_REPLY, MSG_DIAG_REQUEST,
};

const TEST_PHONE_PSK: [u8; 32] = [0x55u8; 32];

struct TestEnv {
    gateway: Gateway,
}

impl TestEnv {
    async fn new() -> Self {
        Self::with_thresholds(-60, -75).await
    }

    async fn with_thresholds(good: i8, bad: i8) -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let crypto_sha = RustCryptoSha256;
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
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let mut gateway = Gateway::new_with_pending(
            storage.clone(),
            pending_commands,
            session_manager,
            Arc::new(RwLock::new(sonde_gateway::handler::HandlerRouter::new(
                Vec::new(),
            ))),
        );
        gateway.set_rssi_thresholds(good, bad);
        Self { gateway }
    }
}

fn compute_key_hint(psk: &[u8; 32]) -> u16 {
    let hash = RustCryptoSha256.hash(psk);
    u16::from_be_bytes([hash[30], hash[31]])
}

fn build_diag_request(phone_psk: &[u8; 32]) -> Vec<u8> {
    let msg = NodeMessage::DiagRequest {
        diagnostic_type: sonde_protocol::DIAG_TYPE_RSSI,
    };
    let cbor = msg.encode().unwrap();
    let header = FrameHeader {
        key_hint: compute_key_hint(phone_psk),
        msg_type: MSG_DIAG_REQUEST,
        nonce: 0xDEADBEEF,
    };
    encode_frame(&header, &cbor, phone_psk, &GatewayAead, &RustCryptoSha256).unwrap()
}

fn peer() -> PeerAddress {
    b"test-peer".to_vec()
}

fn decode_diag_reply(raw: &[u8], phone_psk: &[u8; 32]) -> GatewayMessage {
    let decoded = decode_frame(raw).unwrap();
    assert_eq!(decoded.header.msg_type, MSG_DIAG_REPLY);
    let plaintext = open_frame(&decoded, phone_psk, &GatewayAead, &RustCryptoSha256).unwrap();
    GatewayMessage::decode(MSG_DIAG_REPLY, &plaintext).unwrap()
}

#[tokio::test]
async fn diag_request_good_rssi() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-50))
        .await;
    assert!(response.is_some());
    match decode_diag_reply(&response.unwrap(), &TEST_PHONE_PSK) {
        GatewayMessage::DiagReply {
            signal_quality,
            rssi_dbm,
            ..
        } => {
            assert_eq!(signal_quality, 0);
            assert_eq!(rssi_dbm, -50);
        }
        other => panic!("expected DiagReply, got {:?}", other),
    }
}

#[tokio::test]
async fn diag_request_marginal_rssi() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-65))
        .await
        .unwrap();
    match decode_diag_reply(&response, &TEST_PHONE_PSK) {
        GatewayMessage::DiagReply {
            signal_quality,
            rssi_dbm,
            ..
        } => {
            assert_eq!(signal_quality, 1);
            assert_eq!(rssi_dbm, -65);
        }
        other => panic!("expected DiagReply, got {:?}", other),
    }
}

#[tokio::test]
async fn diag_request_bad_rssi() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-80))
        .await
        .unwrap();
    match decode_diag_reply(&response, &TEST_PHONE_PSK) {
        GatewayMessage::DiagReply {
            signal_quality,
            rssi_dbm,
            ..
        } => {
            assert_eq!(signal_quality, 2);
            assert_eq!(rssi_dbm, -80);
        }
        other => panic!("expected DiagReply, got {:?}", other),
    }
}

#[tokio::test]
async fn diag_request_at_good_boundary() {
    let env = TestEnv::with_thresholds(-60, -75).await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-60))
        .await
        .unwrap();
    match decode_diag_reply(&response, &TEST_PHONE_PSK) {
        GatewayMessage::DiagReply { signal_quality, .. } => assert_eq!(signal_quality, 0),
        other => panic!("expected DiagReply, got {:?}", other),
    }
}

#[tokio::test]
async fn diag_request_below_bad_boundary() {
    let env = TestEnv::with_thresholds(-60, -75).await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-76))
        .await
        .unwrap();
    match decode_diag_reply(&response, &TEST_PHONE_PSK) {
        GatewayMessage::DiagReply { signal_quality, .. } => assert_eq!(signal_quality, 2),
        other => panic!("expected DiagReply, got {:?}", other),
    }
}

#[tokio::test]
async fn diag_request_no_rssi_sentinel() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), None)
        .await
        .unwrap();
    match decode_diag_reply(&response, &TEST_PHONE_PSK) {
        GatewayMessage::DiagReply {
            signal_quality,
            rssi_dbm,
            ..
        } => {
            assert_eq!(rssi_dbm, 0);
            assert_eq!(signal_quality, 2);
        }
        other => panic!("expected DiagReply, got {:?}", other),
    }
}

#[tokio::test]
async fn diag_request_wrong_psk_discarded() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&[0xBBu8; 32]);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-50))
        .await;
    assert!(response.is_none());
}

#[tokio::test]
async fn diag_request_revoked_psk_discarded() {
    let storage = Arc::new(InMemoryStorage::new());
    let hash = RustCryptoSha256.hash(&TEST_PHONE_PSK);
    let hint = u16::from_be_bytes([hash[30], hash[31]]);
    let record = PhonePskRecord {
        phone_id: 0,
        phone_key_hint: hint,
        psk: Zeroizing::new(TEST_PHONE_PSK),
        label: "test".into(),
        issued_at: std::time::SystemTime::now(),
        status: PhonePskStatus::Active,
    };
    let id = storage.store_phone_psk(&record).await.unwrap();
    storage.revoke_phone_psk(id).await.unwrap();

    let sm = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let pc: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let gw = Gateway::new_with_pending(
        storage,
        pc,
        sm,
        Arc::new(RwLock::new(sonde_gateway::handler::HandlerRouter::new(
            Vec::new(),
        ))),
    );
    let frame = build_diag_request(&TEST_PHONE_PSK);
    assert!(gw
        .process_frame_with_rssi(&frame, peer(), Some(-50))
        .await
        .is_none());
}

#[tokio::test]
async fn diag_reply_echoes_nonce() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-50))
        .await
        .unwrap();
    let decoded = decode_frame(&response).unwrap();
    assert_eq!(decoded.header.nonce, 0xDEADBEEF);
}

#[tokio::test]
async fn diag_reply_uses_phone_key_hint() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    let response = env
        .gateway
        .process_frame_with_rssi(&frame, peer(), Some(-50))
        .await
        .unwrap();
    let decoded = decode_frame(&response).unwrap();
    assert_eq!(decoded.header.key_hint, compute_key_hint(&TEST_PHONE_PSK));
    assert!(open_frame(&decoded, &TEST_PHONE_PSK, &GatewayAead, &RustCryptoSha256).is_ok());
}

#[tokio::test]
async fn diag_request_via_process_frame() {
    let env = TestEnv::new().await;
    let frame = build_diag_request(&TEST_PHONE_PSK);
    assert!(env.gateway.process_frame(&frame, peer()).await.is_some());
}
