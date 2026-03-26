// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 3 validation tests:
//! - Prevail verification (T-0402 through T-0404)
//! - Frame overhead budget (T-0608)
//! - Stale program detection (T-0701)
//! - Operational tests (T-1000 through T-1003)

use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::crypto::RustCryptoHmac;
use sonde_gateway::engine::Gateway;
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::state_bundle;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MSG_COMMAND, MSG_WAKE,
};

// ── Helpers ─────────────────────────────────────────────────────────────────

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

    fn build_wake(&self, nonce: u64, abi: u32, program_hash: &[u8], battery_mv: u32) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_WAKE,
            nonce,
        };
        let msg = NodeMessage::Wake {
            firmware_abi_version: abi,
            program_hash: program_hash.to_vec(),
            battery_mv,
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
    }
}

fn make_gateway(storage: Arc<InMemoryStorage>) -> Gateway {
    Gateway::new(storage, Duration::from_secs(30))
}

fn decode_response(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = decode_frame(raw).unwrap();
    assert!(verify_frame(&decoded, psk, &RustCryptoHmac));
    let msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
    (decoded.header, msg)
}

async fn store_test_program(storage: &InMemoryStorage, bytecode: &[u8]) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let record = lib
        .ingest_unverified(cbor, VerificationProfile::Resident)
        .unwrap();
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

// Pre-compiled BPF ELF for nop.c (clang -target bpf -O2).
// This is a simple `return 0` program. Embedding avoids CI dependency on clang.
#[rustfmt::skip]
const NOP_ELF: &[u8] = &[
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0xF7, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xD8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x06, 0x00, 0x01, 0x00,
    0xB7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x23, 0x00, 0x00, 0x00, 0x04, 0x00, 0xF1, 0xFF,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x07, 0x00, 0x00, 0x00, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2E, 0x74, 0x65, 0x78, 0x74, 0x00, 0x70,
    0x72, 0x6F, 0x67, 0x72, 0x61, 0x6D, 0x00, 0x2E, 0x6C, 0x6C, 0x76, 0x6D, 0x5F, 0x61, 0x64, 0x64,
    0x72, 0x73, 0x69, 0x67, 0x00, 0x73, 0x6F, 0x6E, 0x64, 0x65, 0x00, 0x6E, 0x6F, 0x70, 0x2E, 0x63,
    0x00, 0x2E, 0x73, 0x74, 0x72, 0x74, 0x61, 0x62, 0x00, 0x2E, 0x73, 0x79, 0x6D, 0x74, 0x61, 0x62,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x29, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x39, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1D, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00, 0x00, 0x03, 0x4C, 0xFF, 0x6F,
    0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x31, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

// ── T-0402: Prevail verification — resident pass ────────────────────────────

/// T-0402  Prevail verification — resident pass.
///
/// Submit a valid resident BPF program (nop.o). Assert: verification passes.
#[tokio::test]
async fn t0402_prevail_verification_resident_pass() {
    let elf_bytes = NOP_ELF;
    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(elf_bytes, VerificationProfile::Resident);
    assert!(
        result.is_ok(),
        "valid resident BPF program should pass verification: {:?}",
        result.err()
    );
    let record = result.unwrap();
    assert!(!record.image.is_empty());
    assert!(!record.hash.is_empty());
}

// ── T-0403: Prevail verification — resident fail ────────────────────────────

/// T-0403  Prevail verification — resident fail.
///
/// Submit a BPF ELF with invalid instructions. Assert: verification fails.
#[tokio::test]
async fn t0403_prevail_verification_resident_fail() {
    let lib = ProgramLibrary::new();
    // Construct a minimal ELF header with garbage BPF instructions that
    // Prevail cannot verify. A completely random blob should fail at ELF
    // parse or verification.
    let garbage_elf = vec![
        0x7f, b'E', b'L', b'F', // ELF magic
        0x02, 0x01, 0x01, 0x00, // 64-bit, little-endian, ELF v1
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // padding
        0x01, 0x00, // ET_REL
        0xF7, 0x00, // EM_BPF (247)
        // ... rest is garbage, will fail parsing
        0x00, 0x00, 0x00, 0x00,
    ];
    let result = lib.ingest_elf(&garbage_elf, VerificationProfile::Resident);
    assert!(
        result.is_err(),
        "invalid ELF should fail verification/parsing"
    );
}

// ── T-0404: Prevail verification — ephemeral profile ────────────────────────

/// T-0404  Prevail verification — ephemeral profile.
///
/// The nop program is simple enough that it should pass verification
/// under the ephemeral profile as well (termination check enabled).
#[tokio::test]
async fn t0404_prevail_verification_ephemeral_profile() {
    let elf_bytes = NOP_ELF;
    let lib = ProgramLibrary::new();
    let result = lib.ingest_elf(elf_bytes, VerificationProfile::Ephemeral);
    assert!(
        result.is_ok(),
        "valid ephemeral BPF program should pass verification: {:?}",
        result.err()
    );
}

// ── T-0608: Frame overhead budget ───────────────────────────────────────────

/// T-0608  Frame overhead budget.
///
/// Capture any outbound frame from the gateway. Assert:
///   - First 11 bytes = header (key_hint 2B + msg_type 1B + nonce 8B)
///   - Last 32 bytes = HMAC
///   - Total = 11 + payload_len + 32
#[tokio::test]
async fn t0608_frame_overhead_budget() {
    let storage = Arc::new(InMemoryStorage::new());
    let node = TestNode::new("node-overhead", 0x0001, [0xAA; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let gateway = make_gateway(storage);

    let wake_frame = node.build_wake(42, 1, &[0u8; 32], 3300);
    let response = gateway
        .process_frame(&wake_frame, node.peer_address())
        .await;
    assert!(response.is_some(), "gateway must respond to WAKE");

    let raw = response.unwrap();
    // Minimum frame: 11 header + at least 1 byte payload + 32 HMAC = 44
    assert!(raw.len() >= 44, "frame too short: {} bytes", raw.len());

    // Decode and verify structure.
    let decoded = decode_frame(&raw).unwrap();
    assert_eq!(decoded.header.msg_type, MSG_COMMAND);
    assert!(verify_frame(&decoded, &node.psk, &RustCryptoHmac));

    // Verify total = 11 (header) + payload_len + 32 (HMAC)
    let expected_total = 11 + decoded.payload.len() + 32;
    assert_eq!(
        raw.len(),
        expected_total,
        "frame structure: 11 + {} + 32 = {} but got {}",
        decoded.payload.len(),
        expected_total,
        raw.len()
    );
}

// ── T-0701: Stale program detection ─────────────────────────────────────────

/// T-0701  Stale program detection.
///
/// Assign program A, WAKE reports hash_A → NOP.
/// Reassign to program B, WAKE reports hash_A → UPDATE_PROGRAM for B.
#[tokio::test]
async fn t0701_stale_program_detection() {
    let storage = Arc::new(InMemoryStorage::new());
    let node = TestNode::new("node-stale", 0x0010, [0xCC; 32]);

    // Ingest two programs with distinct bytecode.
    let hash_a =
        store_test_program(&storage, &[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]).await;
    let hash_b =
        store_test_program(&storage, &[0xB7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]).await;

    // Register node with program A assigned.
    let mut node_record = node.to_record();
    node_record.assigned_program_hash = Some(hash_a.clone());
    storage.upsert_node(&node_record).await.unwrap();

    let gateway = make_gateway(storage.clone());

    // WAKE with hash_A → should be NOP (program matches).
    let wake = node.build_wake(1, 1, &hash_a, 3300);
    let resp = gateway
        .process_frame(&wake, node.peer_address())
        .await
        .unwrap();
    let (_, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::Command { payload, .. } => {
            assert!(
                matches!(payload, CommandPayload::Nop),
                "NOP expected when hash matches, got: {:?}",
                payload,
            );
        }
        other => panic!("expected Command, got: {:?}", other),
    }

    // Reassign to program B.
    node_record.assigned_program_hash = Some(hash_b.clone());
    storage.upsert_node(&node_record).await.unwrap();

    // WAKE again with hash_A → should get UPDATE_PROGRAM for B.
    let wake = node.build_wake(2, 1, &hash_a, 3300);
    let resp = gateway
        .process_frame(&wake, node.peer_address())
        .await
        .unwrap();
    let (_, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::Command { payload, .. } => match payload {
            CommandPayload::UpdateProgram { program_hash, .. } => {
                assert_eq!(
                    program_hash, hash_b,
                    "UPDATE_PROGRAM must reference program B"
                );
            }
            other => panic!("expected UpdateProgram, got: {:?}", other),
        },
        other => panic!("expected Command, got: {:?}", other),
    }
}

// ── T-1000: Gateway failover ────────────────────────────────────────────────

/// T-1000  Gateway failover (export from A, import to B, B serves node).
#[tokio::test]
async fn t1000_gateway_failover() {
    let storage_a = Arc::new(InMemoryStorage::new());
    let node = TestNode::new("node-failover", 0x0020, [0xDD; 32]);
    storage_a.upsert_node(&node.to_record()).await.unwrap();

    let gateway_a = make_gateway(storage_a.clone());

    // Complete a WAKE on gateway A.
    let wake = node.build_wake(1, 1, &[0u8; 32], 3300);
    let resp_a = gateway_a.process_frame(&wake, node.peer_address()).await;
    assert!(resp_a.is_some(), "gateway A must respond");

    // Export state from A.
    let passphrase = "test-passphrase";
    let nodes_a = storage_a.list_nodes().await.unwrap();
    let programs_a = storage_a.list_programs().await.unwrap();
    let exported = state_bundle::encrypt_state(&nodes_a, &programs_a, passphrase).unwrap();

    // Start gateway B, import state.
    let storage_b = Arc::new(InMemoryStorage::new());
    let (imported_nodes, imported_programs) =
        state_bundle::decrypt_state(&exported, passphrase).unwrap();
    for n in &imported_nodes {
        storage_b.upsert_node(n).await.unwrap();
    }
    for p in &imported_programs {
        storage_b.store_program(p).await.unwrap();
    }

    let gateway_b = make_gateway(storage_b);

    // WAKE from the same node on B.
    let wake = node.build_wake(10, 1, &[0u8; 32], 3300);
    let resp_b = gateway_b.process_frame(&wake, node.peer_address()).await;
    assert!(resp_b.is_some(), "gateway B must respond after import");

    // B recognizes the node.
    let (_, msg) = decode_response(&resp_b.unwrap(), &node.psk);
    match msg {
        GatewayMessage::Command { .. } => {} // success
        other => panic!("expected Command, got: {:?}", other),
    }
}

// ── T-1001: Program hash consistency ────────────────────────────────────────

/// T-1001  Program hash consistency — same image on two instances produces
/// identical hashes and chunk data.
#[tokio::test]
async fn t1001_program_hash_consistency() {
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        maps: vec![],
        map_initial_data: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();

    let rec1 = lib
        .ingest_unverified(cbor.clone(), VerificationProfile::Resident)
        .unwrap();
    let rec2 = lib
        .ingest_unverified(cbor, VerificationProfile::Resident)
        .unwrap();

    assert_eq!(rec1.hash, rec2.hash, "same image must produce same hash");
    assert_eq!(rec1.image, rec2.image, "same image must produce same data");
}

// ── T-1002: Export/import round-trip ────────────────────────────────────────

/// T-1002  Export/import round-trip — nodes and programs survive transfer.
#[tokio::test]
async fn t1002_export_import_round_trip() {
    let storage_a = Arc::new(InMemoryStorage::new());

    // Register nodes and programs.
    let node1 = NodeRecord::new("n1".into(), 0x1234, [0xAA; 32]);
    let node2 = NodeRecord::new("n2".into(), 0x5678, [0xBB; 32]);
    storage_a.upsert_node(&node1).await.unwrap();
    storage_a.upsert_node(&node2).await.unwrap();

    let hash = store_test_program(
        &storage_a,
        &[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    )
    .await;

    let passphrase = "round-trip-test";
    let nodes_a = storage_a.list_nodes().await.unwrap();
    let programs_a = storage_a.list_programs().await.unwrap();
    let exported = state_bundle::encrypt_state(&nodes_a, &programs_a, passphrase).unwrap();

    let storage_b = Arc::new(InMemoryStorage::new());
    let (imported_nodes, imported_programs) =
        state_bundle::decrypt_state(&exported, passphrase).unwrap();
    for n in &imported_nodes {
        storage_b.upsert_node(n).await.unwrap();
    }
    for p in &imported_programs {
        storage_b.store_program(p).await.unwrap();
    }

    // Verify all data survived.
    let nodes = storage_b.list_nodes().await.unwrap();
    assert_eq!(nodes.len(), 2);
    let n1 = storage_b.get_node("n1").await.unwrap().unwrap();
    assert_eq!(n1.key_hint, 0x1234);
    assert_eq!(n1.psk, [0xAA; 32]);

    let prog = storage_b.get_program(&hash).await.unwrap().unwrap();
    assert_eq!(prog.hash, hash);
}

// ── T-1003: Concurrent node handling ────────────────────────────────────────

/// T-1003  Concurrent node handling — 10 nodes WAKE simultaneously, all get
/// responses with no cross-contamination.
#[tokio::test]
async fn t1003_concurrent_node_handling() {
    let storage = Arc::new(InMemoryStorage::new());

    // Register 10 nodes with distinct PSKs.
    let mut nodes = Vec::new();
    for i in 0u8..10 {
        let mut psk = [0u8; 32];
        psk[0] = i + 1;
        let tn = TestNode::new(&format!("concurrent-{i}"), 0x0100 + i as u16, psk);
        storage.upsert_node(&tn.to_record()).await.unwrap();
        nodes.push(tn);
    }

    let gateway = Arc::new(make_gateway(storage));

    // Send WAKE from all 10 simultaneously.
    let mut handles = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        let gw = Arc::clone(&gateway);
        let wake = node.build_wake(100 + i as u64, 1, &[0u8; 32], 3300 + i as u32);
        let psk = node.psk;
        let peer = node.peer_address();
        handles.push(tokio::spawn(async move {
            let resp = gw.process_frame(&wake, peer).await;
            (i, resp, psk)
        }));
    }

    // All 10 should get responses.
    for handle in handles {
        let (i, resp, psk) = handle.await.unwrap();
        assert!(resp.is_some(), "node {i} must receive a response");
        let raw = resp.unwrap();
        let decoded = decode_frame(&raw).unwrap();
        assert!(
            verify_frame(&decoded, &psk, &RustCryptoHmac),
            "node {i} response must be authenticated with its own PSK"
        );
    }
}
