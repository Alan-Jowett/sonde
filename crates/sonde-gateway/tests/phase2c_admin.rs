// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// Some imports and helpers are only used by debug-gated tests (raw CBOR
// ingestion is rejected in release builds).
#![allow(unused_imports, dead_code)]

//! Phase 2C-ii integration tests: gRPC admin API (T-0800 to T-0810, T-1005).
//!
//! Tests call `AdminService` methods directly via tonic `Request`/`Response`
//! (no gRPC transport needed). Combined admin+protocol tests also create a
//! `Gateway` sharing the same storage and pending-commands map.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use tokio::sync::RwLock;
use tonic::Request;
use zeroize::Zeroizing;

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use sonde_gateway::admin::AdminService;
use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::handler::{HandlerConfig, ProgramMatcher};
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::program::ProgramLibrary;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MSG_WAKE,
};

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
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }
}

struct TestHarness {
    storage: Arc<InMemoryStorage>,
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    session_manager: Arc<SessionManager>,
    admin: AdminService,
}

impl TestHarness {
    fn new() -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let admin = AdminService::new(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
        );
        Self {
            storage,
            pending_commands,
            session_manager,
            admin,
        }
    }

    fn make_gateway(&self) -> Gateway {
        Gateway::new_with_pending(
            self.storage.clone(),
            self.pending_commands.clone(),
            self.session_manager.clone(),
            Arc::new(RwLock::new(sonde_gateway::handler::HandlerRouter::new(Vec::new()))),
        )
    }
}

fn make_cbor_image(bytecode: &[u8]) -> Vec<u8> {
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    image.encode_deterministic().unwrap()
}

/// Minimal `mov r0, 0; exit` BPF program — two valid instructions.
const MINIMAL_BPF: &[u8] = &[
    0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
    0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
];

fn decode_response(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = decode_frame(raw).unwrap();
    let plaintext = open_frame(&decoded, psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
    (decoded.header, msg)
}

async fn do_wake(
    gw: &Gateway,
    node: &TestNode,
    nonce: u64,
    program_hash: &[u8],
) -> (u64, u64, CommandPayload) {
    let frame = node.build_wake(nonce, 1, program_hash, 3300);
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

// ═══════════════════════════════════════════════════════════════════════
//  T-0800: RegisterNode + ListNodes + GetNode
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t0800_register_list_get_node() {
    let h = TestHarness::new();

    // Register two nodes
    let resp = h
        .admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "alpha".into(),
            key_hint: 0x0001,
            psk: vec![0xAA; 32],
        }))
        .await
        .unwrap();
    assert_eq!(resp.get_ref().node_id, "alpha");

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "beta".into(),
            key_hint: 0x0002,
            psk: vec![0xBB; 32],
        }))
        .await
        .unwrap();

    // ListNodes returns both
    let list = h
        .admin
        .list_nodes(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.nodes.len(), 2);

    // GetNode returns the correct node
    let info = h
        .admin
        .get_node(Request::new(GetNodeRequest {
            node_id: "alpha".into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.node_id, "alpha");
    assert_eq!(info.key_hint, 0x0001);
}

/// T-0800b: RegisterNode with invalid PSK length.
#[tokio::test]
async fn t0800b_register_invalid_psk() {
    let h = TestHarness::new();

    let err = h
        .admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "bad".into(),
            key_hint: 1,
            psk: vec![0xAA; 16],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// T-0800c: RegisterNode with empty `node_id`.
#[tokio::test]
async fn t0800c_register_empty_node_id() {
    let h = TestHarness::new();

    let err = h
        .admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "".into(),
            key_hint: 1,
            psk: vec![0xAA; 32],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0801: RemoveNode
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t0801_remove_node() {
    let h = TestHarness::new();

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "doomed".into(),
            key_hint: 1,
            psk: vec![0xCC; 32],
        }))
        .await
        .unwrap();

    h.admin
        .remove_node(Request::new(RemoveNodeRequest {
            node_id: "doomed".into(),
        }))
        .await
        .unwrap();

    // GetNode should fail
    let err = h
        .admin
        .get_node(Request::new(GetNodeRequest {
            node_id: "doomed".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// T-0801b: RemoveNode for non-existent node returns NotFound.
#[tokio::test]
async fn t0801b_remove_nonexistent() {
    let h = TestHarness::new();

    let err = h
        .admin
        .remove_node(Request::new(RemoveNodeRequest {
            node_id: "ghost".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// T-0705: Factory reset — RemoveNode deletes registry entry AND subsequent
/// WAKE from the removed node is silently discarded (GW-0705 AC3).
/// Full factory reset (node-side PSK/program erasure) requires protocol-level
/// support not yet implemented; this test validates the gateway-side invariant.
#[tokio::test]
async fn t0705_remove_node_wake_rejected() {
    let h = TestHarness::new();
    let psk = [0x42u8; 32];

    // Register the node.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "reset-node".into(),
            key_hint: 0x0705,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    // WAKE succeeds while node is registered.
    let gw = h.make_gateway();
    let node = TestNode::new("reset-node", 0x0705, psk);
    let frame = node.build_wake(1, 1, &[0u8; 32], 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_some(), "WAKE must succeed while node is registered");

    // Remove the node (simulates gateway side of factory reset).
    h.admin
        .remove_node(Request::new(RemoveNodeRequest {
            node_id: "reset-node".into(),
        }))
        .await
        .unwrap();

    // Subsequent WAKE must be silently discarded (unknown node).
    let frame2 = node.build_wake(2, 1, &[0u8; 32], 3300);
    let resp2 = gw.process_frame(&frame2, node.peer_address()).await;
    assert!(
        resp2.is_none(),
        "WAKE from removed node must be silently discarded"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0802: IngestProgram + ListPrograms
// ═══════════════════════════════════════════════════════════════════════

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0802_ingest_and_list_programs() {
    let h = TestHarness::new();

    let cbor = make_cbor_image(&[0x01, 0x02, 0x03]);
    let resp = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor.clone(),
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(!resp.program_hash.is_empty());
    assert_eq!(resp.program_size, cbor.len() as u32);

    // Verify via ProgramLibrary that the hash matches
    let lib = ProgramLibrary::new();
    let expected = lib
        .ingest_unverified(cbor, sonde_gateway::VerificationProfile::Resident)
        .unwrap();
    assert_eq!(resp.program_hash, expected.hash);

    // ListPrograms
    let list = h
        .admin
        .list_programs(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.programs.len(), 1);
    assert_eq!(list.programs[0].hash, resp.program_hash);
    assert_eq!(
        list.programs[0].verification_profile,
        VerificationProfile::Resident as i32
    );
}

/// T-0802b: IngestProgram with invalid profile.
#[tokio::test]
async fn t0802b_ingest_invalid_profile() {
    let h = TestHarness::new();

    let err = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: make_cbor_image(&[0x01]),
            verification_profile: 99,
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// T-0802c: IngestProgram with empty image.
#[tokio::test]
async fn t0802c_ingest_empty_image() {
    let h = TestHarness::new();

    let err = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: vec![],
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// T-0802d: IngestProgram with abi_version — round-trip through ListPrograms.
#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0802d_ingest_abi_version_round_trip() {
    let h = TestHarness::new();

    let cbor = make_cbor_image(&[0xAA, 0xBB]);
    let resp = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: Some(3),
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    // ListPrograms must return the same abi_version.
    let list = h
        .admin
        .list_programs(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    let prog = list
        .programs
        .iter()
        .find(|p| p.hash == resp.program_hash)
        .expect("ingested program not found in list");
    assert_eq!(
        prog.abi_version,
        Some(3),
        "abi_version must round-trip through IngestProgram / ListPrograms"
    );
}

/// T-0414: source_filename round-trip through IngestProgram → ListPrograms.
#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0414_ingest_source_filename_round_trip() {
    let h = TestHarness::new();

    // Ingest with a source filename.
    let cbor_a = make_cbor_image(&[0xCC, 0xDD]);
    let resp_a = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor_a,
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: Some("tmp102_sensor.o".into()),
        }))
        .await
        .unwrap()
        .into_inner();

    // Ingest without a source filename.
    let cbor_b = make_cbor_image(&[0xEE, 0xFF]);
    let resp_b = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor_b,
            verification_profile: VerificationProfile::Ephemeral.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let list = h
        .admin
        .list_programs(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();

    let prog_a = list
        .programs
        .iter()
        .find(|p| p.hash == resp_a.program_hash)
        .expect("program A not found");
    assert_eq!(
        prog_a.source_filename.as_deref(),
        Some("tmp102_sensor.o"),
        "source_filename must round-trip through IngestProgram / ListPrograms"
    );

    let prog_b = list
        .programs
        .iter()
        .find(|p| p.hash == resp_b.program_hash)
        .expect("program B not found");
    assert!(
        prog_b.source_filename.is_none(),
        "source_filename must be None when not provided"
    );
}

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0803_assign_program() {
    let h = TestHarness::new();

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "n1".into(),
            key_hint: 1,
            psk: vec![0xAA; 32],
        }))
        .await
        .unwrap();

    let cbor = make_cbor_image(&[0x10, 0x20]);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    h.admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "n1".into(),
            program_hash: ingest.program_hash.clone(),
        }))
        .await
        .unwrap();

    // Verify via GetNode
    let info = h
        .admin
        .get_node(Request::new(GetNodeRequest {
            node_id: "n1".into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.assigned_program_hash, ingest.program_hash);
}

/// T-0803b: AssignProgram to non-existent node.
#[tokio::test]
async fn t0803b_assign_program_no_node() {
    let h = TestHarness::new();

    let err = h
        .admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "ghost".into(),
            program_hash: vec![0; 32],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// T-0803c: AssignProgram with non-existent program.
#[tokio::test]
async fn t0803c_assign_program_no_program() {
    let h = TestHarness::new();

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "n1".into(),
            key_hint: 1,
            psk: vec![0xAA; 32],
        }))
        .await
        .unwrap();

    let err = h
        .admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "n1".into(),
            program_hash: vec![0xFF; 32],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0804: RemoveProgram
// ═══════════════════════════════════════════════════════════════════════

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0804_remove_program() {
    let h = TestHarness::new();

    let cbor = make_cbor_image(&[0xCA, 0xFE]);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Ephemeral.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    h.admin
        .remove_program(Request::new(RemoveProgramRequest {
            program_hash: ingest.program_hash.clone(),
        }))
        .await
        .unwrap();

    // ListPrograms should be empty
    let list = h
        .admin
        .list_programs(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert!(list.programs.is_empty());
}

/// T-0804b: RemoveProgram for non-existent program.
#[tokio::test]
async fn t0804b_remove_nonexistent_program() {
    let h = TestHarness::new();

    let err = h
        .admin
        .remove_program(Request::new(RemoveProgramRequest {
            program_hash: vec![0xFF; 32],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0805: SetSchedule queues command, verified via WAKE
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t0805_set_schedule_via_wake() {
    let h = TestHarness::new();
    let psk = [0xAA; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "sched-node".into(),
            key_hint: 1,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    h.admin
        .set_schedule(Request::new(SetScheduleRequest {
            node_id: "sched-node".into(),
            interval_s: 120,
        }))
        .await
        .unwrap();

    // Send WAKE and verify the command is UpdateSchedule
    let gw = h.make_gateway();
    let node = TestNode::new("sched-node", 1, psk);
    let (_seq, _ts, payload) = do_wake(&gw, &node, 42, &[0u8; 32]).await;
    assert!(
        matches!(payload, CommandPayload::UpdateSchedule { interval_s: 120 }),
        "expected UpdateSchedule(120), got {:?}",
        payload
    );
}

/// T-0805b: SetSchedule for non-existent node.
#[tokio::test]
async fn t0805b_set_schedule_no_node() {
    let h = TestHarness::new();

    let err = h
        .admin
        .set_schedule(Request::new(SetScheduleRequest {
            node_id: "ghost".into(),
            interval_s: 60,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0806: QueueReboot verified via WAKE
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t0806_queue_reboot_via_wake() {
    let h = TestHarness::new();
    let psk = [0xBB; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "reboot-node".into(),
            key_hint: 2,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    h.admin
        .queue_reboot(Request::new(QueueRebootRequest {
            node_id: "reboot-node".into(),
        }))
        .await
        .unwrap();

    let gw = h.make_gateway();
    let node = TestNode::new("reboot-node", 2, psk);
    let (_seq, _ts, payload) = do_wake(&gw, &node, 99, &[0u8; 32]).await;
    assert!(
        matches!(payload, CommandPayload::Reboot),
        "expected Reboot, got {:?}",
        payload
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0807: QueueEphemeral verified via WAKE
// ═══════════════════════════════════════════════════════════════════════

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0807_queue_ephemeral_via_wake() {
    let h = TestHarness::new();
    let psk = [0xCC; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "eph-node".into(),
            key_hint: 3,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    // Ingest an ephemeral program
    let cbor = make_cbor_image(&[0xDE, 0xAD]);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Ephemeral.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    h.admin
        .queue_ephemeral(Request::new(QueueEphemeralRequest {
            node_id: "eph-node".into(),
            program_hash: ingest.program_hash.clone(),
        }))
        .await
        .unwrap();

    let gw = h.make_gateway();
    let node = TestNode::new("eph-node", 3, psk);
    let (_seq, _ts, payload) = do_wake(&gw, &node, 77, &[0u8; 32]).await;
    match payload {
        CommandPayload::RunEphemeral { program_hash, .. } => {
            assert_eq!(program_hash, ingest.program_hash);
        }
        other => panic!("expected RunEphemeral, got {:?}", other),
    }
}

/// T-0807b: QueueEphemeral for non-existent program.
#[tokio::test]
async fn t0807b_queue_ephemeral_no_program() {
    let h = TestHarness::new();

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "eph-node".into(),
            key_hint: 3,
            psk: vec![0xCC; 32],
        }))
        .await
        .unwrap();

    let err = h
        .admin
        .queue_ephemeral(Request::new(QueueEphemeralRequest {
            node_id: "eph-node".into(),
            program_hash: vec![0xFF; 32],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0808: GetNodeStatus (with and without active session)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t0808_get_node_status() {
    let h = TestHarness::new();
    let psk = [0xDD; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "status-node".into(),
            key_hint: 4,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    // Before WAKE: no active session
    let status = h
        .admin
        .get_node_status(Request::new(GetNodeStatusRequest {
            node_id: "status-node".into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.node_id, "status-node");
    assert!(!status.has_active_session);

    // Send WAKE to create a session
    let gw = h.make_gateway();
    let node = TestNode::new("status-node", 4, psk);
    do_wake(&gw, &node, 55, &[0u8; 32]).await;

    // After WAKE: active session
    let status = h
        .admin
        .get_node_status(Request::new(GetNodeStatusRequest {
            node_id: "status-node".into(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(status.has_active_session);
    assert!(status.last_seen_ms.is_some());
    assert_eq!(status.battery_mv, Some(3300));
    assert_eq!(status.firmware_abi_version, Some(1));
}

/// T-0808b: GetNodeStatus for non-existent node.
#[tokio::test]
async fn t0808b_status_no_node() {
    let h = TestHarness::new();

    let err = h
        .admin
        .get_node_status(Request::new(GetNodeStatusRequest {
            node_id: "ghost".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0809: AssignProgram → WAKE delivers UPDATE_PROGRAM
// ═══════════════════════════════════════════════════════════════════════

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0809_assign_program_wake_delivers_update() {
    let h = TestHarness::new();
    let psk = [0xEE; 32];

    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "update-node".into(),
            key_hint: 5,
            psk: psk.to_vec(),
        }))
        .await
        .unwrap();

    let cbor = make_cbor_image(&[0xAB, 0xCD]);
    let ingest = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    h.admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "update-node".into(),
            program_hash: ingest.program_hash.clone(),
        }))
        .await
        .unwrap();

    // WAKE with a different program hash → gateway should send UPDATE_PROGRAM
    let gw = h.make_gateway();
    let node = TestNode::new("update-node", 5, psk);
    let (_seq, _ts, payload) = do_wake(&gw, &node, 101, &[0u8; 32]).await;
    match payload {
        CommandPayload::UpdateProgram { program_hash, .. } => {
            assert_eq!(program_hash, ingest.program_hash);
        }
        other => panic!("expected UpdateProgram, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0810: ExportState + ImportState (GW-0805)
// ═══════════════════════════════════════════════════════════════════════

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0810_export_state_returns_encrypted_bundle() {
    let h = TestHarness::new();

    // Register a node and ingest a program so there is something to export.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "exp-node".into(),
            key_hint: 0x0042,
            psk: vec![0xABu8; 32],
        }))
        .await
        .unwrap();
    let cbor = make_cbor_image(MINIMAL_BPF);
    h.admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap();

    let resp = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "test-passphrase".into(),
        }))
        .await
        .unwrap();

    let data = resp.into_inner().data;
    // The bundle must be non-empty and start with the expected magic bytes.
    assert!(
        data.len() >= 8,
        "bundle too short ({} bytes) to contain magic header",
        data.len()
    );
    assert_eq!(&data[..8], b"SNDESTAT");
}

#[tokio::test]
async fn t0810_export_empty_passphrase_rejected() {
    let h = TestHarness::new();

    let err = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[cfg(debug_assertions)] // raw CBOR ingestion only accepted in debug builds
#[tokio::test]
async fn t0810_import_state_restores_nodes_and_programs() {
    let h = TestHarness::new();

    // Populate the source gateway.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "import-node".into(),
            key_hint: 0x0007,
            psk: vec![0xCDu8; 32],
        }))
        .await
        .unwrap();
    let cbor = make_cbor_image(MINIMAL_BPF);
    let ingest_resp = h
        .admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: cbor,
            verification_profile: VerificationProfile::Resident.into(),
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap();
    let prog_hash = ingest_resp.into_inner().program_hash;

    // Export.
    let export_resp = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "failover-pass".into(),
        }))
        .await
        .unwrap();
    let bundle = export_resp.into_inner().data;

    // Import into a fresh gateway.
    let h2 = TestHarness::new();
    h2.admin
        .import_state(Request::new(ImportStateRequest {
            data: bundle,
            passphrase: "failover-pass".into(),
        }))
        .await
        .unwrap();

    // Verify node was restored.
    let nodes = h2
        .admin
        .list_nodes(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "import-node");
    assert_eq!(nodes[0].key_hint, 0x0007);

    // Verify program was restored.
    let programs = h2
        .admin
        .list_programs(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner()
        .programs;
    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0].hash, prog_hash);
}

#[tokio::test]
async fn t0810_import_wrong_passphrase_rejected() {
    let h = TestHarness::new();

    let export_resp = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "correct-pass".into(),
        }))
        .await
        .unwrap();
    let bundle = export_resp.into_inner().data;

    let err = h
        .admin
        .import_state(Request::new(ImportStateRequest {
            data: bundle,
            passphrase: "wrong-pass".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// Import must restore gateway identity, phone PSKs, and handler configs
/// — not just nodes and programs.
#[tokio::test]
async fn t0810_import_state_restores_identity_phones_and_handlers() {
    let storage = Arc::new(InMemoryStorage::new());
    let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));

    // Seed a gateway identity.
    let identity = GatewayIdentity::generate().unwrap();
    let orig_gateway_id = *identity.gateway_id();
    let orig_public_key = *identity.public_key();
    storage.store_gateway_identity(&identity).await.unwrap();

    // Seed a phone PSK.  Use a fixed timestamp so the second-granularity
    // CBOR encoding round-trips without sub-second precision loss.
    let fixed_issued_at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let phone_psk = PhonePskRecord {
        phone_id: 0, // auto-assigned by storage
        phone_key_hint: 0x1234,
        psk: Zeroizing::new([0x42u8; 32]),
        label: "test-phone".into(),
        issued_at: fixed_issued_at,
        status: PhonePskStatus::Active,
    };
    storage.store_phone_psk(&phone_psk).await.unwrap();

    // Build the admin service with handler configs.
    let handler_configs = vec![HandlerConfig {
        matchers: vec![ProgramMatcher::Any],
        command: "/usr/bin/handler".into(),
        args: vec!["--verbose".into()],
        reply_timeout: Some(Duration::from_secs(5)),
        working_dir: None,
    }];
    let admin = AdminService::new(
        storage.clone(),
        pending_commands.clone(),
        session_manager.clone(),
    )
    .with_handler_configs(handler_configs);

    // Export the full state.
    let bundle = admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "full-state-pass".into(),
        }))
        .await
        .unwrap()
        .into_inner()
        .data;

    // Import into a fresh gateway (no identity, no phone PSKs, no handler
    // configs).
    let storage2 = Arc::new(InMemoryStorage::new());
    let pending2: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let sm2 = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin2 = AdminService::new(storage2.clone(), pending2.clone(), sm2.clone());

    admin2
        .import_state(Request::new(ImportStateRequest {
            data: bundle,
            passphrase: "full-state-pass".into(),
        }))
        .await
        .unwrap();

    // Verify gateway identity was restored.
    let restored_id = storage2
        .load_gateway_identity()
        .await
        .unwrap()
        .expect("identity should be present after import");
    assert_eq!(*restored_id.gateway_id(), orig_gateway_id);
    assert_eq!(*restored_id.public_key(), orig_public_key);

    // Verify phone PSKs were restored.
    let restored_psks = storage2.list_phone_psks().await.unwrap();
    assert_eq!(restored_psks.len(), 1);
    assert_eq!(restored_psks[0].phone_key_hint, 0x1234);
    assert_eq!(*restored_psks[0].psk, [0x42u8; 32]);
    assert_eq!(restored_psks[0].label, "test-phone");
    assert_eq!(restored_psks[0].issued_at, fixed_issued_at);
    assert_eq!(restored_psks[0].status, PhonePskStatus::Active);

    // Verify handler configs were restored by exporting again and
    // round-tripping through decrypt_state_full.
    let re_exported = admin2
        .export_state(Request::new(ExportStateRequest {
            passphrase: "re-export".into(),
        }))
        .await
        .unwrap()
        .into_inner()
        .data;
    let (_, _, _, _, restored_handlers) =
        sonde_gateway::state_bundle::decrypt_state_full(&re_exported, "re-export").unwrap();
    assert_eq!(restored_handlers.len(), 1);
    assert_eq!(restored_handlers[0].command, "/usr/bin/handler");
    assert_eq!(restored_handlers[0].args, vec!["--verbose"]);
    assert_eq!(
        restored_handlers[0].reply_timeout,
        Some(Duration::from_secs(5))
    );
}

#[tokio::test]
async fn t0810_import_invalid_data_rejected() {
    let h = TestHarness::new();

    let err = h
        .admin
        .import_state(Request::new(ImportStateRequest {
            data: vec![0xFF; 50],
            passphrase: "some-pass".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn t0810_import_replaces_existing_state() {
    let h = TestHarness::new();

    // Populate with initial state.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "old-node".into(),
            key_hint: 0x0001,
            psk: vec![0x11u8; 32],
        }))
        .await
        .unwrap();

    // Export.
    let bundle = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "replace-pass".into(),
        }))
        .await
        .unwrap()
        .into_inner()
        .data;

    // Add a second node AFTER the export.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "new-node".into(),
            key_hint: 0x0002,
            psk: vec![0x22u8; 32],
        }))
        .await
        .unwrap();

    // Import the earlier bundle — should replace, leaving only "old-node".
    h.admin
        .import_state(Request::new(ImportStateRequest {
            data: bundle,
            passphrase: "replace-pass".into(),
        }))
        .await
        .unwrap();

    let nodes = h
        .admin
        .list_nodes(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "old-node");
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1005: Export plaintext key leakage (GW-1001)
// ═══════════════════════════════════════════════════════════════════════

/// T-1005 — Verify that exported state bundles do not leak key material
/// in plaintext.
///
/// Procedure (from gateway-validation.md):
/// 1. Register nodes with known PSKs.
/// 2. Call `ExportState` with a known export passphrase.
/// 3. Inspect the raw export bytes (encrypted bundle).
/// 4. Assert: no PSK value appears as a contiguous substring in the export payload.
/// 5. Attempt import with incorrect passphrase — assert rejected and state unchanged
///    (tested against both the original gateway and a fresh gateway to verify nodes
///    are not restored and WAKE is rejected).
/// 6. Import into a fresh gateway using the correct passphrase.
/// 7. Assert: nodes are restored and PSKs are functional (WAKE accepted).
#[tokio::test]
async fn t1005_export_plaintext_key_leakage() {
    // Use clearly non-zero, distinctive PSKs so substring scanning is meaningful.
    let psk_a = [0x42u8; 32];
    let psk_b = [0xDEu8; 32];

    let node_a = TestNode::new("leak-node-a", 0x00AA, psk_a);
    let node_b = TestNode::new("leak-node-b", 0x00BB, psk_b);

    let h = TestHarness::new();

    // Step 1: Register nodes with known PSKs.
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: node_a.node_id.clone(),
            key_hint: node_a.key_hint as u32,
            psk: psk_a.to_vec(),
        }))
        .await
        .unwrap();
    h.admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: node_b.node_id.clone(),
            key_hint: node_b.key_hint as u32,
            psk: psk_b.to_vec(),
        }))
        .await
        .unwrap();

    // Step 2: Export state with a known passphrase.
    let passphrase = "test-export-passphrase";
    let export_resp = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: passphrase.into(),
        }))
        .await
        .unwrap();
    let bundle = export_resp.into_inner().data;

    // Sanity: a vacuously-empty bundle would make the substring scan below
    // pass without providing any security signal.
    assert!(
        !bundle.is_empty(),
        "Exported state bundle is empty; ExportState may be returning an invalid payload"
    );

    // Step 3–4: Scan the raw export bytes for PSK byte sequences.
    // No PSK must appear as a contiguous substring anywhere in the bundle.
    assert!(
        !contains_subsequence(&bundle, &psk_a),
        "PSK A ({:#04X} repeated) found as plaintext in export bundle",
        psk_a[0]
    );
    assert!(
        !contains_subsequence(&bundle, &psk_b),
        "PSK B ({:#04X} repeated) found as plaintext in export bundle",
        psk_b[0]
    );

    // Step 5a: Import with incorrect passphrase against the original gateway —
    // must be rejected and existing state must remain intact.
    let err = h
        .admin
        .import_state(Request::new(ImportStateRequest {
            data: bundle.clone(),
            passphrase: "wrong-passphrase".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "import with wrong passphrase should fail"
    );

    // Verify state is unchanged after the failed import: check node count, IDs,
    // and key_hints, then validate WAKE for both nodes against the original gateway.
    let nodes_after_fail = h
        .admin
        .list_nodes(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(
        nodes_after_fail.len(),
        2,
        "state must be unchanged after failed import"
    );
    let mut fail_ids: Vec<&str> = nodes_after_fail
        .iter()
        .map(|n| n.node_id.as_str())
        .collect();
    fail_ids.sort();
    assert_eq!(fail_ids, vec!["leak-node-a", "leak-node-b"]);
    for n in &nodes_after_fail {
        match n.node_id.as_str() {
            "leak-node-a" => assert_eq!(n.key_hint, 0x00AA),
            "leak-node-b" => assert_eq!(n.key_hint, 0x00BB),
            other => panic!("unexpected node after failed import: {other}"),
        }
    }

    let gw = h.make_gateway();
    let _cmd_a = do_wake(&gw, &node_a, 1, &[0u8; 32]).await;
    let _cmd_b = do_wake(&gw, &node_b, 2, &[0u8; 32]).await;

    // Step 5b: Import with incorrect passphrase against a fresh gateway (no
    // pre-existing nodes). Validates that nodes are NOT restored and WAKE is
    // rejected, per T-1005 step 5 in gateway-validation.md.
    let h_fresh = TestHarness::new();
    let fresh_err = h_fresh
        .admin
        .import_state(Request::new(ImportStateRequest {
            data: bundle.clone(),
            passphrase: "wrong-passphrase".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        fresh_err.code(),
        tonic::Code::InvalidArgument,
        "import with wrong passphrase into fresh gateway should fail"
    );

    // Nodes must not be restored after failed import.
    let fresh_nodes = h_fresh
        .admin
        .list_nodes(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert!(
        fresh_nodes.is_empty(),
        "fresh gateway must have no nodes after failed import"
    );

    // WAKE from both nodes must be silently discarded.
    let gw_fresh = h_fresh.make_gateway();
    let frame_a = node_a.build_wake(5, 1, &[0u8; 32], 3300);
    let resp_a = gw_fresh
        .process_frame(&frame_a, node_a.peer_address())
        .await;
    assert!(
        resp_a.is_none(),
        "WAKE from node_a must be rejected on fresh gateway after failed import"
    );
    let frame_b = node_b.build_wake(6, 1, &[0u8; 32], 3300);
    let resp_b = gw_fresh
        .process_frame(&frame_b, node_b.peer_address())
        .await;
    assert!(
        resp_b.is_none(),
        "WAKE from node_b must be rejected on fresh gateway after failed import"
    );

    // Step 6: Import into a fresh gateway using the correct passphrase.
    let h2 = TestHarness::new();
    h2.admin
        .import_state(Request::new(ImportStateRequest {
            data: bundle,
            passphrase: passphrase.into(),
        }))
        .await
        .unwrap();

    // Step 7: Verify nodes are restored and PSKs are functional.
    let restored_nodes = h2
        .admin
        .list_nodes(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(restored_nodes.len(), 2, "both nodes must be restored");

    let mut restored_ids: Vec<&str> = restored_nodes.iter().map(|n| n.node_id.as_str()).collect();
    restored_ids.sort();
    assert_eq!(restored_ids, vec!["leak-node-a", "leak-node-b"]);

    // WAKE from both restored nodes must be accepted (PSKs functional).
    let gw2 = h2.make_gateway();
    let _cmd_a = do_wake(&gw2, &node_a, 3, &[0u8; 32]).await;
    let _cmd_b = do_wake(&gw2, &node_b, 4, &[0u8; 32]).await;
}

/// Returns `true` if `needle` appears as a contiguous subsequence in `haystack`.
fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0800 transport: gRPC API availability over Unix domain socket
//  Validates GW-0800 end-to-end (server binds UDS, client connects, RPC works).
// ═══════════════════════════════════════════════════════════════════════

/// T-0800 transport: Start the admin gRPC server on a Unix domain socket,
/// connect a tonic client, call ListNodes, and assert it succeeds.
#[cfg(unix)]
#[tokio::test]
async fn t0800_grpc_uds_transport() {
    use hyper_util::rt::TokioIo;
    use tonic::transport::{Endpoint, Uri};
    use tower::service_fn;

    use sonde_gateway::admin::pb::gateway_admin_client::GatewayAdminClient;
    use sonde_gateway::admin::pb::Empty;

    // Use a temporary directory that is automatically removed when dropped.
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let socket_path = tmp_dir.path().join("admin.sock");
    let socket_path_str = socket_path.to_str().unwrap().to_owned();

    // Build the admin service backed by an in-memory store.
    let h = TestHarness::new();

    // Start the gRPC server on the UDS socket in a background task.
    // Capture the handle so we can abort the task at the end of the test.
    let socket_path_server = socket_path_str.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = sonde_gateway::admin::serve_admin(h.admin, &socket_path_server).await {
            eprintln!("admin server task ended: {e}");
        }
    });

    // Retry connecting until the socket is ready or the deadline passes.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let channel = loop {
        let path = socket_path_str.clone();
        match Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(service_fn(move |_: Uri| {
                let p = path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(p).await?;
                    Ok::<_, std::io::Error>(TokioIo::new(stream))
                }
            }))
            .await
        {
            Ok(ch) => break ch,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(e) => panic!("failed to connect to UDS admin socket: {e}"),
        }
    };

    let mut client = GatewayAdminClient::new(channel);

    // T-0800 assertion: ListNodes succeeds over the UDS transport.
    let resp = client
        .list_nodes(tonic::Request::new(Empty {}))
        .await
        .expect("ListNodes over UDS should succeed");
    assert_eq!(resp.into_inner().nodes.len(), 0);
    // Abort the server task and let tmp_dir clean up the socket directory.
    server_handle.abort();
}
