// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 2B integration tests: protocol engine, command dispatch, chunked
//! transfer, and authentication.

use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MAX_FRAME_SIZE, MSG_APP_DATA, MSG_CHUNK, MSG_COMMAND, MSG_GET_CHUNK,
    MSG_PROGRAM_ACK, MSG_WAKE,
};

use sonde_gateway::crypto::RustCryptoHmac;
use tracing_test::traced_test;

// ─── Test helpers ──────────────────────────────────────────────────────

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

    /// Build a valid WAKE frame.
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

    /// Build a GET_CHUNK frame with the given sequence number.
    fn build_get_chunk(&self, seq: u64, chunk_index: u32) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_GET_CHUNK,
            nonce: seq,
        };
        let msg = NodeMessage::GetChunk { chunk_index };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
    }

    /// Build a PROGRAM_ACK frame with the given sequence number.
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
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
    }

    /// Build an APP_DATA frame with the given sequence number.
    #[allow(dead_code)]
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
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
    }
}

/// Create a Gateway with in-memory storage and a 30s session timeout.
fn make_gateway(storage: Arc<InMemoryStorage>) -> Gateway {
    Gateway::new(storage, Duration::from_secs(30))
}

/// Create a small program image, ingest it, store it, and return its hash.
async fn store_test_program(storage: &InMemoryStorage, bytecode: &[u8]) -> Vec<u8> {
    store_test_program_with_profile(storage, bytecode, VerificationProfile::Resident).await
}

/// Create a program image with a specific verification profile, ingest, store, and return its hash.
async fn store_test_program_with_profile(
    storage: &InMemoryStorage,
    bytecode: &[u8],
    profile: VerificationProfile,
) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let record = lib.ingest_unverified(cbor, profile).unwrap();
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

/// Create a program image with a specific ABI version, ingest, store, and return its hash.
async fn store_test_program_with_abi(
    storage: &InMemoryStorage,
    bytecode: &[u8],
    abi_version: u32,
) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let mut record = lib
        .ingest_unverified(cbor, VerificationProfile::Resident)
        .unwrap();
    record.abi_version = Some(abi_version);
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

/// Decode a gateway response frame and return the GatewayMessage.
fn decode_response(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = decode_frame(raw).unwrap();
    assert!(verify_frame(&decoded, psk, &RustCryptoHmac));
    let msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
    (decoded.header, msg)
}

/// Send a WAKE with a specific firmware ABI version and return
/// the (starting_seq, timestamp_ms, CommandPayload) from the COMMAND response.
async fn do_wake_with_abi(
    gw: &Gateway,
    node: &TestNode,
    nonce: u64,
    firmware_abi_version: u32,
    program_hash: &[u8],
) -> (u64, u64, CommandPayload) {
    let frame = node.build_wake(nonce, firmware_abi_version, program_hash, 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");
    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => (starting_seq, timestamp_ms, payload),
        other => panic!("expected Command, got {:?}", other),
    }
}

/// Send a WAKE and return the (starting_seq, timestamp_ms, CommandPayload)
/// from the COMMAND response.
async fn do_wake(
    gw: &Gateway,
    node: &TestNode,
    nonce: u64,
    program_hash: &[u8],
) -> (u64, u64, CommandPayload) {
    do_wake_with_abi(gw, node, nonce, 1, program_hash).await
}

// ═══════════════════════════════════════════════════════════════════════
//  T-01xx: Protocol and Communication Tests
// ═══════════════════════════════════════════════════════════════════════

/// T-0101: Valid CBOR encoding in response.
#[tokio::test]
async fn t0101_valid_cbor_encoding() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-01", 0x0001, [0xAA; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let frame = node.build_wake(42, 1, &[0u8; 32], 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected response");

    let decoded = decode_frame(&resp).unwrap();
    // Payload must be valid CBOR decodable as a GatewayMessage
    let msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload);
    assert!(msg.is_ok(), "response payload must be valid CBOR");
}

/// T-0102: Malformed CBOR tolerance (valid HMAC, garbage payload).
#[tokio::test]
async fn t0102_malformed_cbor_tolerance() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-02", 0x0002, [0xBB; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // Build a frame with valid header + HMAC but garbage CBOR payload
    let header = FrameHeader {
        key_hint: 0x0002,
        msg_type: MSG_WAKE,
        nonce: 99,
    };
    let garbage = &[0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
    let frame = encode_frame(&header, garbage, &node.psk, &RustCryptoHmac).unwrap();

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_none(), "garbage CBOR must be silently discarded");
}

/// T-0103: WAKE reception and field extraction.
#[tokio::test]
async fn t0103_wake_reception_and_field_extraction() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-03", 0x0003, [0xCC; 32]);
    let mut record = node.to_record();
    let program_hash = store_test_program(&storage, b"test-bytecode").await;
    record.assigned_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    let frame = node.build_wake(100, 1, &program_hash, 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");

    let (_, msg) = decode_response(&resp, &node.psk);
    assert!(
        matches!(msg, GatewayMessage::Command { .. }),
        "must respond with COMMAND"
    );

    // Verify the registry was updated
    let updated = storage.get_node("node-03").await.unwrap().unwrap();
    assert_eq!(updated.firmware_abi_version, Some(1));
    assert_eq!(updated.last_battery_mv, Some(3300));
}

/// T-0104: WAKE with missing fields rejected.
#[tokio::test]
async fn t0104_wake_missing_fields_rejected() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-04", 0x0004, [0xDD; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // Build a WAKE with missing battery_mv by encoding raw CBOR manually
    // We'll send a CBOR map with only firmware_abi_version and program_hash
    use sonde_protocol::{KEY_FIRMWARE_ABI_VERSION, KEY_PROGRAM_HASH};
    let pairs: Vec<(ciborium::Value, ciborium::Value)> = vec![
        (
            ciborium::Value::Integer(KEY_FIRMWARE_ABI_VERSION.into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Integer(KEY_PROGRAM_HASH.into()),
            ciborium::Value::Bytes(vec![0u8; 32]),
        ),
    ];
    let value = ciborium::Value::Map(pairs);
    let mut cbor_buf = Vec::new();
    ciborium::into_writer(&value, &mut cbor_buf).unwrap();

    let header = FrameHeader {
        key_hint: 0x0004,
        msg_type: MSG_WAKE,
        nonce: 200,
    };
    let frame = encode_frame(&header, &cbor_buf, &node.psk, &RustCryptoHmac).unwrap();

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_none(), "WAKE with missing fields must be discarded");
}

/// T-0105: COMMAND response structure (echoed nonce, starting_seq, timestamp_ms).
#[tokio::test]
async fn t0105_command_response_structure() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-05", 0x0005, [0xEE; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let wake_nonce = 12345u64;
    let frame = node.build_wake(wake_nonce, 1, &[0u8; 32], 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");

    let (hdr, msg) = decode_response(&resp, &node.psk);

    // Response nonce must echo the WAKE nonce
    assert_eq!(hdr.nonce, wake_nonce, "nonce must echo WAKE nonce");
    assert_eq!(hdr.msg_type, MSG_COMMAND);

    match msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => {
            // starting_seq is a valid random u64 — just verify we decoded successfully
            let _ = starting_seq;
            // timestamp_ms should be within 5 seconds of now
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            assert!(
                timestamp_ms <= now_ms && now_ms - timestamp_ms < 5000,
                "timestamp_ms must be within 5s of now"
            );
            assert_eq!(payload.command_type(), sonde_protocol::CMD_NOP);
        }
        other => panic!("expected Command, got {:?}", other),
    }
}

/// T-0106: Frame size constraint — all responses ≤ 250 bytes.
#[tokio::test]
async fn t0106_frame_size_constraint() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-06", 0x0006, [0x11; 32]);
    let mut record = node.to_record();

    // Store a program that will produce chunk responses near the limit
    let bytecode = vec![0xABu8; 512];
    let program_hash = store_test_program(&storage, &bytecode).await;
    record.assigned_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // Send WAKE → get UPDATE_PROGRAM
    let (starting_seq, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    let chunk_count = match &payload {
        CommandPayload::UpdateProgram { chunk_count, .. } => *chunk_count,
        other => panic!("expected UpdateProgram, got {:?}", other),
    };

    // Request all chunks and verify each response ≤ 250 bytes
    for i in 0..chunk_count {
        let seq = starting_seq.wrapping_add(i as u64);
        let chunk_frame = node.build_get_chunk(seq, i);
        let resp = gw
            .process_frame(&chunk_frame, node.peer_address())
            .await
            .expect("expected CHUNK response");
        assert!(
            resp.len() <= MAX_FRAME_SIZE,
            "chunk {} response {} bytes exceeds {} byte limit",
            i,
            resp.len(),
            MAX_FRAME_SIZE
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  T-02xx: Command Set Tests
// ═══════════════════════════════════════════════════════════════════════

/// T-0200: NOP when program_hash matches.
#[tokio::test]
async fn t0200_nop_when_hash_matches() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-200", 0x0200, [0x20; 32]);
    let program_hash = store_test_program(&storage, b"resident-prog").await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    let (_, _, payload) = do_wake(&gw, &node, 1, &program_hash).await;
    assert_eq!(payload.command_type(), sonde_protocol::CMD_NOP);
    assert!(matches!(payload, CommandPayload::Nop));
}

/// T-0201: UPDATE_PROGRAM when hash differs.
#[tokio::test]
async fn t0201_update_program_when_hash_differs() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-201", 0x0201, [0x21; 32]);
    let assigned_hash = store_test_program(&storage, b"program-A").await;
    let node_reports = vec![0u8; 32]; // different hash

    let mut record = node.to_record();
    record.assigned_program_hash = Some(assigned_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    let (_, _, payload) = do_wake(&gw, &node, 1, &node_reports).await;
    assert_eq!(payload.command_type(), sonde_protocol::CMD_UPDATE_PROGRAM);
    match &payload {
        CommandPayload::UpdateProgram {
            program_hash,
            program_size,
            chunk_size,
            chunk_count,
        } => {
            assert_eq!(program_hash, &assigned_hash);
            assert!(*program_size > 0);
            assert!(*chunk_size > 0);
            assert!(*chunk_count > 0);
        }
        other => panic!("expected UpdateProgram, got {:?}", other),
    }
}

/// T-0202: RUN_EPHEMERAL via pending command.
#[tokio::test]
async fn t0202_run_ephemeral() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-202", 0x0202, [0x22; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let ephemeral_hash = store_test_program_with_profile(
        &storage,
        b"ephemeral-diag",
        VerificationProfile::Ephemeral,
    )
    .await;

    gw.queue_command(
        "node-202",
        PendingCommand::RunEphemeral {
            program_hash: ephemeral_hash.clone(),
        },
    )
    .await;

    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(payload.command_type(), sonde_protocol::CMD_RUN_EPHEMERAL);
    match &payload {
        CommandPayload::RunEphemeral {
            program_hash,
            program_size,
            chunk_size,
            chunk_count,
        } => {
            assert_eq!(program_hash, &ephemeral_hash);
            assert!(*program_size > 0);
            assert!(*chunk_size > 0);
            assert!(*chunk_count > 0);
        }
        other => panic!("expected RunEphemeral, got {:?}", other),
    }
}

/// T-0203: UPDATE_SCHEDULE via pending command.
#[tokio::test]
async fn t0203_update_schedule() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-203", 0x0203, [0x23; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    gw.queue_command(
        "node-203",
        PendingCommand::UpdateSchedule { interval_s: 120 },
    )
    .await;

    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(payload.command_type(), sonde_protocol::CMD_UPDATE_SCHEDULE);
    match &payload {
        CommandPayload::UpdateSchedule { interval_s } => {
            assert_eq!(*interval_s, 120);
        }
        other => panic!("expected UpdateSchedule, got {:?}", other),
    }
}

/// T-0204: REBOOT via pending command.
#[tokio::test]
async fn t0204_reboot() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-204", 0x0204, [0x24; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    gw.queue_command("node-204", PendingCommand::Reboot).await;

    let (_, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(payload.command_type(), sonde_protocol::CMD_REBOOT);
    assert!(matches!(payload, CommandPayload::Reboot));
}

/// T-0205: Command priority ordering (ephemeral > update > schedule > reboot > NOP).
#[tokio::test]
async fn t0205_command_priority_ordering() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-205", 0x0205, [0x25; 32]);
    let assigned_hash = store_test_program(&storage, b"assigned-prog").await;
    let ephemeral_hash = store_test_program_with_profile(
        &storage,
        b"ephemeral-prog",
        VerificationProfile::Ephemeral,
    )
    .await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(assigned_hash.clone());
    // Keep the stored current_program_hash out of sync with the assigned hash to
    // mimic a partially-synchronized registry. UPDATE_PROGRAM is actually triggered
    // by a mismatch between the hash reported in WAKE and assigned_program_hash
    // (see the do_wake() calls below), not by this field.
    record.current_program_hash = Some(vec![0u8; 32]);
    storage.upsert_node(&record).await.unwrap();

    // Queue ephemeral + schedule + reboot ALL before the first WAKE
    gw.queue_command(
        "node-205",
        PendingCommand::RunEphemeral {
            program_hash: ephemeral_hash.clone(),
        },
    )
    .await;
    gw.queue_command(
        "node-205",
        PendingCommand::UpdateSchedule { interval_s: 60 },
    )
    .await;
    gw.queue_command("node-205", PendingCommand::Reboot).await;

    // WAKE 1: must be RUN_EPHEMERAL (highest priority)
    let (_, _, p1) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(
        p1.command_type(),
        sonde_protocol::CMD_RUN_EPHEMERAL,
        "first WAKE must yield RUN_EPHEMERAL"
    );

    // WAKE 2: must be UPDATE_PROGRAM (hash mismatch has priority 2)
    let (_, _, p2) = do_wake(&gw, &node, 2, &[0u8; 32]).await;
    assert_eq!(
        p2.command_type(),
        sonde_protocol::CMD_UPDATE_PROGRAM,
        "second WAKE must yield UPDATE_PROGRAM"
    );

    // Node now sends matching hash — update is no longer triggered
    // WAKE 3: must be UPDATE_SCHEDULE (priority 3)
    let (_, _, p3) = do_wake(&gw, &node, 3, &assigned_hash).await;
    assert_eq!(
        p3.command_type(),
        sonde_protocol::CMD_UPDATE_SCHEDULE,
        "third WAKE must yield UPDATE_SCHEDULE"
    );

    // WAKE 4: must be REBOOT (priority 4)
    let (_, _, p4) = do_wake(&gw, &node, 4, &assigned_hash).await;
    assert_eq!(
        p4.command_type(),
        sonde_protocol::CMD_REBOOT,
        "fourth WAKE must yield REBOOT"
    );

    // WAKE 5: must be NOP (nothing pending, hash matches)
    let (_, _, p5) = do_wake(&gw, &node, 5, &assigned_hash).await;
    assert_eq!(
        p5.command_type(),
        sonde_protocol::CMD_NOP,
        "fifth WAKE must yield NOP"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  T-03xx: Chunked Transfer Tests
// ═══════════════════════════════════════════════════════════════════════

/// T-0300: Complete chunked transfer (GET_CHUNK → CHUNK for all chunks).
#[tokio::test]
async fn t0300_complete_chunked_transfer() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-300", 0x0300, [0x30; 32]);

    // Create a multi-chunk program (> 128 bytes of bytecode → multiple chunks)
    let bytecode = vec![0xAB; 400];
    let assigned_hash = store_test_program(&storage, &bytecode).await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(assigned_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // WAKE → UPDATE_PROGRAM
    let (starting_seq, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    let (chunk_count, chunk_size) = match &payload {
        CommandPayload::UpdateProgram {
            chunk_count,
            chunk_size,
            ..
        } => (*chunk_count, *chunk_size),
        other => panic!("expected UpdateProgram, got {:?}", other),
    };
    assert!(chunk_count > 1, "need multiple chunks for this test");

    // Retrieve the original image for comparison
    let program = storage.get_program(&assigned_hash).await.unwrap().unwrap();
    let original_image = &program.image;

    // Request all chunks and reassemble
    let mut reassembled = Vec::new();
    for i in 0..chunk_count {
        let seq = starting_seq.wrapping_add(i as u64);
        let chunk_frame = node.build_get_chunk(seq, i);
        let resp = gw
            .process_frame(&chunk_frame, node.peer_address())
            .await
            .expect("expected CHUNK response");

        let (resp_hdr, msg) = decode_response(&resp, &node.psk);
        // Verify the response echoes the sequence number
        assert_eq!(resp_hdr.nonce, seq, "CHUNK must echo the GET_CHUNK nonce");
        assert_eq!(resp_hdr.msg_type, MSG_CHUNK);

        match msg {
            GatewayMessage::Chunk {
                chunk_index,
                chunk_data,
            } => {
                assert_eq!(chunk_index, i);
                reassembled.extend_from_slice(&chunk_data);
            }
            other => panic!("expected Chunk, got {:?}", other),
        }
    }

    // Reassembled data must match the original image
    let expected_len = original_image.len();
    assert_eq!(
        reassembled.len(),
        expected_len,
        "reassembled size must match"
    );
    assert_eq!(
        &reassembled[..],
        &original_image[..],
        "reassembled data must match original CBOR image"
    );

    // Verify chunk_size and chunk_count are consistent
    let expected_count =
        sonde_protocol::chunk_count(original_image.len(), chunk_size as usize).unwrap();
    assert_eq!(chunk_count, expected_count);
}

/// T-0301: Transfer resumption from chunk 0.
#[tokio::test]
async fn t0301_transfer_resumption() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-301", 0x0301, [0x31; 32]);
    let bytecode = vec![0xCD; 400];
    let assigned_hash = store_test_program(&storage, &bytecode).await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(assigned_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // First WAKE → UPDATE_PROGRAM
    let (seq1, _, _) = do_wake(&gw, &node, 1, &[0u8; 32]).await;

    // Request chunks 0 and 1
    let chunk0_frame = node.build_get_chunk(seq1, 0);
    let resp0 = gw
        .process_frame(&chunk0_frame, node.peer_address())
        .await
        .expect("chunk 0");
    let (_, c0) = decode_response(&resp0, &node.psk);
    let first_chunk0_data = match c0 {
        GatewayMessage::Chunk { chunk_data, .. } => chunk_data,
        _ => panic!("expected Chunk"),
    };

    // Simulate sleep — send a new WAKE (replaces session)
    let (seq2, _, payload2) = do_wake(&gw, &node, 2, &[0u8; 32]).await;
    assert_eq!(
        payload2.command_type(),
        sonde_protocol::CMD_UPDATE_PROGRAM,
        "second WAKE must still yield UPDATE_PROGRAM"
    );

    // Request chunk 0 again from the new session
    let chunk0_frame2 = node.build_get_chunk(seq2, 0);
    let resp0_2 = gw
        .process_frame(&chunk0_frame2, node.peer_address())
        .await
        .expect("chunk 0 re-request");
    let (_, c0_2) = decode_response(&resp0_2, &node.psk);
    let second_chunk0_data = match c0_2 {
        GatewayMessage::Chunk { chunk_data, .. } => chunk_data,
        _ => panic!("expected Chunk"),
    };

    // Data must be identical
    assert_eq!(first_chunk0_data, second_chunk0_data);
}

/// T-0302: PROGRAM_ACK updates registry.
#[tokio::test]
async fn t0302_program_ack_updates_registry() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-302", 0x0302, [0x32; 32]);
    let bytecode = vec![0xEF; 100];
    let assigned_hash = store_test_program(&storage, &bytecode).await;

    let mut record = node.to_record();
    record.assigned_program_hash = Some(assigned_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // WAKE → UPDATE_PROGRAM
    let (starting_seq, _, payload) = do_wake(&gw, &node, 1, &[0u8; 32]).await;
    assert_eq!(payload.command_type(), sonde_protocol::CMD_UPDATE_PROGRAM);
    let chunk_count = match &payload {
        CommandPayload::UpdateProgram { chunk_count, .. } => *chunk_count,
        _ => panic!("expected UpdateProgram"),
    };

    // Fetch all chunks
    for i in 0..chunk_count {
        let seq = starting_seq.wrapping_add(i as u64);
        let f = node.build_get_chunk(seq, i);
        let _ = gw.process_frame(&f, node.peer_address()).await;
    }

    // Send PROGRAM_ACK
    let ack_seq = starting_seq.wrapping_add(chunk_count as u64);
    let ack_frame = node.build_program_ack(ack_seq, &assigned_hash);
    let ack_resp = gw.process_frame(&ack_frame, node.peer_address()).await;
    assert!(
        ack_resp.is_none(),
        "PROGRAM_ACK should not produce a response frame"
    );

    // Verify the registry was updated
    let updated = storage.get_node("node-302").await.unwrap().unwrap();
    assert_eq!(
        updated.current_program_hash.as_deref(),
        Some(assigned_hash.as_slice()),
        "current_program_hash must be updated after PROGRAM_ACK"
    );

    // Next WAKE with matching hash → NOP
    let (_, _, p2) = do_wake(&gw, &node, 2, &assigned_hash).await;
    assert_eq!(
        p2.command_type(),
        sonde_protocol::CMD_NOP,
        "after PROGRAM_ACK, WAKE with matching hash should yield NOP"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  T-06xx: Authentication and Security Tests
// ═══════════════════════════════════════════════════════════════════════

/// T-0600: Valid HMAC accepted.
#[tokio::test]
async fn t0600_valid_hmac_accepted() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-600", 0x0600, [0x60; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let frame = node.build_wake(1, 1, &[0u8; 32], 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_some(), "valid HMAC must be accepted");
}

/// T-0601: Invalid HMAC rejected (flipped bit).
#[tokio::test]
async fn t0601_invalid_hmac_rejected() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-601", 0x0601, [0x61; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    let mut frame = node.build_wake(1, 1, &[0u8; 32], 3300);
    // Flip a bit in the HMAC (last 32 bytes)
    let last = frame.len() - 1;
    frame[last] ^= 0x01;

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_none(), "flipped HMAC bit must be rejected");
}

/// T-0602: Wrong key rejected.
#[tokio::test]
async fn t0602_wrong_key_rejected() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-602", 0x0602, [0x62; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // Build a WAKE using a different PSK but with node's key_hint
    let wrong_psk = [0xFF; 32];
    let header = FrameHeader {
        key_hint: 0x0602,
        msg_type: MSG_WAKE,
        nonce: 1,
    };
    let msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0u8; 32],
        battery_mv: 3300,
    };
    let cbor = msg.encode().unwrap();
    let frame = encode_frame(&header, &cbor, &wrong_psk, &RustCryptoHmac).unwrap();

    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_none(), "wrong PSK must be rejected");
}

/// T-0603: key_hint collision handling (two nodes, same key_hint).
#[tokio::test]
async fn t0603_key_hint_collision() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let shared_hint = 0x0603u16;
    let node_a = TestNode::new("node-603a", shared_hint, [0xA0; 32]);
    let node_b = TestNode::new("node-603b", shared_hint, [0xB0; 32]);

    storage.upsert_node(&node_a.to_record()).await.unwrap();
    storage.upsert_node(&node_b.to_record()).await.unwrap();

    // Send WAKE from node A — gateway must try both PSKs and find A's
    let frame_a = node_a.build_wake(1, 1, &[0u8; 32], 3300);
    let resp_a = gw
        .process_frame(&frame_a, node_a.peer_address())
        .await
        .expect("node A must be authenticated despite key_hint collision");

    // Verify the response can be decoded with node A's PSK
    let (_, msg_a) = decode_response(&resp_a, &node_a.psk);
    assert!(matches!(msg_a, GatewayMessage::Command { .. }));

    // Registry must show node A was updated, not node B
    let updated_a = storage.get_node("node-603a").await.unwrap().unwrap();
    assert!(updated_a.last_seen.is_some());

    // Send WAKE from node B — also must succeed
    let frame_b = node_b.build_wake(2, 1, &[0u8; 32], 3200);
    let resp_b = gw
        .process_frame(&frame_b, node_b.peer_address())
        .await
        .expect("node B must also be authenticated");
    let (_, msg_b) = decode_response(&resp_b, &node_b.psk);
    assert!(matches!(msg_b, GatewayMessage::Command { .. }));

    let updated_b = storage.get_node("node-603b").await.unwrap().unwrap();
    assert_eq!(updated_b.last_battery_mv, Some(3200));
}

/// T-0609: Unknown node — silent discard.
#[tokio::test]
async fn t0609_unknown_node_silent_discard() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    // No nodes registered — send a WAKE with an arbitrary key_hint
    let unknown = TestNode::new("unknown", 0xFFFF, [0x99; 32]);
    let frame = unknown.build_wake(1, 1, &[0u8; 32], 3000);

    let resp = gw.process_frame(&frame, unknown.peer_address()).await;
    assert!(resp.is_none(), "unknown node must be silently discarded");

    // Verify no state changed (no sessions created)
    assert_eq!(gw.session_manager().active_count().await, 0);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-07xx: Firmware ABI Version Tests
// ═══════════════════════════════════════════════════════════════════════

/// T-0704: ABI incompatibility — gateway must NOT issue UPDATE_PROGRAM when the
/// program's ABI version does not match the node's firmware ABI version,
/// and must log a warning.
#[tokio::test]
#[traced_test]
async fn t0704_abi_incompatibility_skips_update_program() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    // Store a program that targets ABI version 3.
    let prog_hash = store_test_program_with_abi(&storage, b"abi3-program", 3).await;

    let node = TestNode::new("node-704", 0x0704, [0x74; 32]);
    let mut record = node.to_record();
    record.assigned_program_hash = Some(prog_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // Node reports firmware ABI version 2 — incompatible with the assigned program (ABI 3).
    let (_, _, payload) = do_wake_with_abi(&gw, &node, 1, 2, &[0u8; 32]).await;
    assert_ne!(
        payload.command_type(),
        sonde_protocol::CMD_UPDATE_PROGRAM,
        "gateway must NOT issue UPDATE_PROGRAM for an ABI-incompatible program"
    );
    // Assert warning was logged.
    assert!(
        logs_contain("ABI mismatch"),
        "expected ABI mismatch warning to be logged"
    );

    // Compatible ABI: store a program for ABI 2 and assign it.
    let prog_hash_abi2 = store_test_program_with_abi(&storage, b"abi2-program", 2).await;
    let mut record2 = storage.get_node("node-704").await.unwrap().unwrap();
    record2.assigned_program_hash = Some(prog_hash_abi2.clone());
    storage.upsert_node(&record2).await.unwrap();

    // Same node reports ABI 2 — now the program is compatible.
    let (_, _, payload2) = do_wake_with_abi(&gw, &node, 2, 2, &[0u8; 32]).await;
    assert_eq!(
        payload2.command_type(),
        sonde_protocol::CMD_UPDATE_PROGRAM,
        "gateway MUST issue UPDATE_PROGRAM when ABI versions match"
    );
}

/// T-0705: ABI incompatibility on ephemeral path — gateway must NOT issue
/// RUN_EPHEMERAL when the program's ABI version does not match the node's
/// firmware ABI version, and must log a warning.
#[tokio::test]
#[traced_test]
async fn t0705_abi_incompatibility_skips_run_ephemeral() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-705", 0x0705, [0x75; 32]);
    storage.upsert_node(&node.to_record()).await.unwrap();

    // Store an ephemeral program targeting ABI 3.
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: b"eph-abi3".to_vec(),
        maps: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let mut eph_record = lib
        .ingest_unverified(cbor, VerificationProfile::Ephemeral)
        .unwrap();
    eph_record.abi_version = Some(3);
    let eph_hash = eph_record.hash.clone();
    storage.store_program(&eph_record).await.unwrap();

    // Queue the ephemeral program.
    gw.queue_command(
        "node-705",
        PendingCommand::RunEphemeral {
            program_hash: eph_hash.clone(),
        },
    )
    .await;

    // Node reports ABI 2 — incompatible with the ephemeral program (ABI 3).
    let (_, _, payload) = do_wake_with_abi(&gw, &node, 1, 2, &[0u8; 32]).await;
    assert_ne!(
        payload.command_type(),
        sonde_protocol::CMD_RUN_EPHEMERAL,
        "gateway must NOT issue RUN_EPHEMERAL for an ABI-incompatible program"
    );
    assert!(
        logs_contain("ABI mismatch"),
        "expected ABI mismatch warning to be logged"
    );

    // The incompatible ephemeral was dropped from the queue. Queue a compatible one (ABI 2).
    let cbor2 = sonde_protocol::ProgramImage {
        bytecode: b"eph-abi2".to_vec(),
        maps: vec![],
    }
    .encode_deterministic()
    .unwrap();
    let mut eph_record2 = lib
        .ingest_unverified(cbor2, VerificationProfile::Ephemeral)
        .unwrap();
    eph_record2.abi_version = Some(2);
    let eph_hash2 = eph_record2.hash.clone();
    storage.store_program(&eph_record2).await.unwrap();

    gw.queue_command(
        "node-705",
        PendingCommand::RunEphemeral {
            program_hash: eph_hash2.clone(),
        },
    )
    .await;

    // Node reports ABI 2 — now compatible.
    let (_, _, payload2) = do_wake_with_abi(&gw, &node, 2, 2, &[0u8; 32]).await;
    assert_eq!(
        payload2.command_type(),
        sonde_protocol::CMD_RUN_EPHEMERAL,
        "gateway MUST issue RUN_EPHEMERAL when ABI versions match"
    );
}
