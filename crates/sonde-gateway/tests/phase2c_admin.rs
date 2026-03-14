// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 2C-ii integration tests: gRPC admin API (T-0800 to T-0810).
//!
//! Tests call `AdminService` methods directly via tonic `Request`/`Response`
//! (no gRPC transport needed). Combined admin+protocol tests also create a
//! `Gateway` sharing the same storage and pending-commands map.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tonic::Request;

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use sonde_gateway::admin::AdminService;
use sonde_gateway::crypto::RustCryptoHmac;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::program::ProgramLibrary;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::InMemoryStorage;
use sonde_gateway::transport::PeerAddress;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
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
        encode_frame(&header, &cbor, &self.psk, &RustCryptoHmac).unwrap()
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
        )
    }
}

fn make_cbor_image(bytecode: &[u8]) -> Vec<u8> {
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
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
    assert!(verify_frame(&decoded, psk, &RustCryptoHmac));
    let msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
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

// ═══════════════════════════════════════════════════════════════════════
//  T-0802: IngestProgram + ListPrograms
// ═══════════════════════════════════════════════════════════════════════

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
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// T-0802d: IngestProgram with abi_version — round-trip through ListPrograms.
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
            verification_profile: 1, // Resident
            abi_version: None,
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
    assert!(!data.is_empty());
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
            verification_profile: 1,
            abi_version: None,
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
