// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::Stream;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tonic::Request;

#[cfg(unix)]
use sonde_gateway::admin::pb::gateway_admin_client::GatewayAdminClient;
#[cfg(unix)]
use sonde_gateway::admin::pb::Empty as AdminEmpty;
use sonde_gateway::companion::pb::companion_event::Event;
#[cfg(unix)]
use sonde_gateway::companion::pb::gateway_companion_client::GatewayCompanionClient;
use sonde_gateway::companion::pb::gateway_companion_server::GatewayCompanion;
use sonde_gateway::companion::pb::*;
use sonde_gateway::companion::{
    CompanionEventHub, CompanionService, DEFAULT_COMPANION_EVENT_BUFFER,
};
use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::handler::HandlerRouter;
use sonde_gateway::program::{ProgramRecord, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transient_display::DisplayStateHandle;
use sonde_gateway::transport::PeerAddress;
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, ProgramImage, MSG_APP_DATA, MSG_WAKE,
};

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
        blob: Option<Vec<u8>>,
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
            firmware_version: "0.5.0".into(),
            blob,
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

struct CompanionHarness {
    storage: Arc<InMemoryStorage>,
    gateway: Gateway,
    event_hub: Arc<CompanionEventHub>,
    companion: CompanionService,
}

impl CompanionHarness {
    fn new() -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let gateway = Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
            Arc::new(RwLock::new(HandlerRouter::new(Vec::new()))),
        );
        let event_hub = gateway.companion_event_hub();
        let companion = CompanionService::new(
            storage.clone(),
            pending_commands.clone(),
            session_manager.clone(),
            event_hub.clone(),
            DisplayStateHandle::new(),
        );
        Self {
            storage,
            gateway,
            event_hub,
            companion,
        }
    }
}

#[tokio::test]
async fn companion_event_hub_zero_capacity_clamps_to_one() {
    let hub = CompanionEventHub::new(0);
    let mut events = hub.subscribe();

    hub.emit_node_checkin(
        "node-0".into(),
        vec![0x42; 32],
        None,
        3200,
        1,
        "0.5.0".into(),
        1234,
    );

    let event = events.recv().await.expect("event should be delivered");
    match event.event {
        Some(Event::NodeCheckin(checkin)) => {
            assert_eq!(checkin.node_id, "node-0");
            assert_eq!(checkin.current_program_hash, vec![0x42; 32]);
            assert_eq!(checkin.timestamp_ms, 1234);
        }
        other => panic!("expected node_checkin, got {other:?}"),
    }
}

fn make_cbor_image(bytecode: &[u8]) -> Vec<u8> {
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    image.encode_deterministic().unwrap()
}

const MINIMAL_BPF: &[u8] = &[
    0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
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
    blob: Option<Vec<u8>>,
) -> (u64, u64, CommandPayload) {
    let frame = node.build_wake(nonce, 1, program_hash, 3300, blob);
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
            blob: _,
        } => (starting_seq, timestamp_ms, payload),
        other => panic!("expected Command, got {other:?}"),
    }
}

async fn next_stream_item(
    stream: &mut (impl Stream<Item = Result<CompanionEvent, tonic::Status>> + Unpin),
) -> Result<CompanionEvent, tonic::Status> {
    tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out waiting for companion event")
        .expect("companion stream ended unexpectedly")
}

async fn assert_no_stream_item(
    stream: &mut (impl Stream<Item = Result<CompanionEvent, tonic::Status>> + Unpin),
) {
    let result = tokio::time::timeout(Duration::from_millis(150), stream.next()).await;
    assert!(result.is_err(), "expected no replayed companion event");
}

async fn register_node(storage: &Arc<InMemoryStorage>, node: &TestNode) {
    storage
        .upsert_node(&NodeRecord::new(
            node.node_id.clone(),
            node.key_hint,
            node.psk,
        ))
        .await
        .unwrap();
}

async fn store_program(
    storage: &Arc<InMemoryStorage>,
    hash_byte: u8,
    profile: VerificationProfile,
) -> Vec<u8> {
    let hash = vec![hash_byte; 32];
    storage
        .store_program(&ProgramRecord {
            hash: hash.clone(),
            image: make_cbor_image(MINIMAL_BPF),
            size: MINIMAL_BPF.len() as u32,
            verification_profile: profile,
            abi_version: None,
            source_filename: None,
        })
        .await
        .unwrap();
    hash
}

#[cfg(unix)]
#[tokio::test]
async fn t0818_companion_grpc_uds_transport_and_distinct_contract() {
    use hyper_util::rt::TokioIo;
    use tonic::transport::{Endpoint, Uri};
    use tower::service_fn;

    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let socket_path = tmp_dir.path().join("companion.sock");
    let socket_path_str = socket_path.to_str().unwrap().to_owned();
    let h = CompanionHarness::new();

    let socket_path_server = socket_path_str.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) =
            sonde_gateway::companion::serve_companion(h.companion, &socket_path_server).await
        {
            eprintln!("companion server task ended: {e}");
        }
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
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
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("failed to connect to UDS companion socket: {e}"),
        }
    };

    let mut companion_client = GatewayCompanionClient::new(channel.clone());
    let mut admin_client = GatewayAdminClient::new(channel);

    let list = companion_client
        .list_nodes(Request::new(CompanionListNodesRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert!(list.nodes.is_empty());

    let err = admin_client
        .list_nodes(Request::new(AdminEmpty {}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented);

    server_handle.abort();
}

#[tokio::test]
async fn t0819_node_checkin_event_on_wake() {
    let h = CompanionHarness::new();
    let node = TestNode::new("alpha", 0x1010, [0x11; 32]);
    let program_hash = vec![0x44; 32];

    register_node(&h.storage, &node).await;

    let mut stream = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();

    let _ = do_wake(&h.gateway, &node, 100, &program_hash, None).await;

    let event = next_stream_item(&mut stream).await.unwrap();
    match event.event.unwrap() {
        Event::NodeCheckin(checkin) => {
            assert_eq!(checkin.node_id, node.node_id);
            assert_eq!(checkin.current_program_hash, program_hash);
            assert_eq!(checkin.battery_mv, 3300);
            assert_eq!(checkin.firmware_abi_version, 1);
            assert_eq!(checkin.firmware_version, "0.5.0");
            assert!(checkin.timestamp_ms > 0);
        }
        other => panic!("expected node_checkin, got {other:?}"),
    }

    let mut fresh_stream = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_no_stream_item(&mut fresh_stream).await;
}

#[tokio::test]
async fn t0820_node_payload_event_on_app_data() {
    let h = CompanionHarness::new();
    let node = TestNode::new("beta", 0x2020, [0x22; 32]);
    let program_hash = vec![0x55; 32];

    register_node(&h.storage, &node).await;
    let mut stored = h.storage.get_node(&node.node_id).await.unwrap().unwrap();
    stored.current_program_hash = Some(program_hash.clone());
    h.storage.upsert_node(&stored).await.unwrap();

    let (starting_seq, _, _) = do_wake(&h.gateway, &node, 200, &program_hash, None).await;
    let mut stream = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();

    let app_payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let frame = node.build_app_data(starting_seq, &app_payload);
    let response = h.gateway.process_frame(&frame, node.peer_address()).await;
    assert!(
        response.is_none(),
        "no handler configured, so no app-data reply is expected"
    );

    let event = next_stream_item(&mut stream).await.unwrap();
    match event.event.unwrap() {
        Event::NodePayload(payload) => {
            assert_eq!(payload.node_id, node.node_id);
            assert_eq!(payload.program_hash, program_hash);
            assert_eq!(payload.payload, app_payload);
            assert_eq!(payload.payload_origin(), CompanionPayloadOrigin::AppData);
            assert!(payload.timestamp_ms > 0);
        }
        other => panic!("expected node_payload, got {other:?}"),
    }
}

#[tokio::test]
async fn t0821_wake_blob_orders_checkin_before_payload() {
    let h = CompanionHarness::new();
    let node = TestNode::new("gamma", 0x3030, [0x33; 32]);
    let program_hash = vec![0x66; 32];
    let wake_blob = vec![0x01, 0x02, 0x03];

    register_node(&h.storage, &node).await;

    let mut stream = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();

    let _ = do_wake(
        &h.gateway,
        &node,
        300,
        &program_hash,
        Some(wake_blob.clone()),
    )
    .await;

    let first = next_stream_item(&mut stream).await.unwrap();
    let second = next_stream_item(&mut stream).await.unwrap();

    match first.event.unwrap() {
        Event::NodeCheckin(checkin) => assert_eq!(checkin.node_id, node.node_id),
        other => panic!("expected first event to be node_checkin, got {other:?}"),
    }
    match second.event.unwrap() {
        Event::NodePayload(payload) => {
            assert_eq!(payload.node_id, node.node_id);
            assert_eq!(payload.program_hash, program_hash);
            assert_eq!(payload.payload, wake_blob);
            assert_eq!(payload.payload_origin(), CompanionPayloadOrigin::WakeBlob);
        }
        other => panic!("expected second event to be node_payload, got {other:?}"),
    }
}

#[tokio::test]
async fn t0822_lagging_subscriber_is_terminated_without_blocking_active_one() {
    let h = CompanionHarness::new();

    let mut stalled = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();
    let mut active = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();

    let lag_count = DEFAULT_COMPANION_EVENT_BUFFER + 24;
    let (active_started_tx, active_started_rx) = tokio::sync::oneshot::channel();
    let active_reader = tokio::spawn(async move {
        let mut seen = Vec::new();

        let first_item = tokio::time::timeout(Duration::from_secs(2), active.next())
            .await
            .expect("timed out waiting for active subscriber event")
            .expect("active stream ended unexpectedly")
            .expect("active subscriber should keep receiving events");
        seen.push(first_item);
        active_started_tx
            .send(())
            .expect("active reader should signal startup exactly once");

        for _ in 1..lag_count {
            let item = tokio::time::timeout(Duration::from_secs(2), active.next())
                .await
                .expect("timed out waiting for active subscriber event")
                .expect("active stream ended unexpectedly")
                .expect("active subscriber should keep receiving events");
            if seen.len() < 8 {
                seen.push(item);
            }
        }
        seen
    });

    h.event_hub.emit_node_checkin(
        "node-0".into(),
        vec![0; 32],
        None,
        3200,
        1,
        "0.5.0".into(),
        0,
    );
    active_started_rx
        .await
        .expect("active reader should observe the first event before the burst");

    for i in 1..lag_count {
        let payload_byte = u8::try_from(i).expect("test event index should fit in u8");
        let battery_mv_offset = u32::try_from(i).expect("test event index should fit in u32");
        let timestamp = u64::try_from(i).expect("test event index should fit in u64");
        h.event_hub.emit_node_checkin(
            format!("node-{i}"),
            vec![payload_byte; 32],
            None,
            3200 + battery_mv_offset,
            1,
            "0.5.0".into(),
            timestamp,
        );
        tokio::task::yield_now().await;
    }

    let mut stalled_err = None;
    for _ in 0..lag_count {
        match next_stream_item(&mut stalled).await {
            Ok(_) => continue,
            Err(status) => {
                stalled_err = Some(status);
                break;
            }
        }
    }
    let stalled_err = stalled_err.expect("stalled subscriber should receive a terminal error");
    assert_eq!(stalled_err.code(), tonic::Code::ResourceExhausted);

    let active_events = active_reader.await.unwrap();
    assert_eq!(active_events.len(), 8);

    let mut fresh = h
        .companion
        .stream_events(Request::new(CompanionStreamEventsRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_no_stream_item(&mut fresh).await;

    h.event_hub.emit_node_checkin(
        "fresh-node".into(),
        vec![0xAA; 32],
        None,
        3300,
        1,
        "0.5.0".into(),
        999,
    );
    let fresh_event = next_stream_item(&mut fresh).await.unwrap();
    match fresh_event.event.unwrap() {
        Event::NodeCheckin(checkin) => assert_eq!(checkin.node_id, "fresh-node"),
        other => panic!("expected fresh node_checkin, got {other:?}"),
    }
}

#[tokio::test]
async fn t0823_companion_commands_share_gateway_state() {
    let h = CompanionHarness::new();
    let resident_hash = store_program(&h.storage, 0x77, VerificationProfile::Resident).await;
    let ephemeral_hash = store_program(&h.storage, 0x88, VerificationProfile::Ephemeral).await;
    let assign_node = TestNode::new("delta-assign", 0x4040, [0x44; 32]);
    let schedule_node = TestNode::new("delta-schedule", 0x4041, [0x45; 32]);
    let reboot_node = TestNode::new("delta-reboot", 0x4042, [0x46; 32]);
    let ephemeral_node = TestNode::new("delta-ephemeral", 0x4043, [0x47; 32]);

    register_node(&h.storage, &assign_node).await;
    register_node(&h.storage, &schedule_node).await;
    register_node(&h.storage, &reboot_node).await;
    register_node(&h.storage, &ephemeral_node).await;

    h.companion
        .assign_program(Request::new(CompanionAssignProgramRequest {
            node_id: assign_node.node_id.clone(),
            program_hash: resident_hash.clone(),
        }))
        .await
        .unwrap();
    let stored = h
        .storage
        .get_node(&assign_node.node_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.assigned_program_hash, Some(resident_hash.clone()));
    let (_, _, assign_payload) = do_wake(&h.gateway, &assign_node, 600, &[0u8; 32], None).await;
    assert!(matches!(
        assign_payload,
        CommandPayload::UpdateProgram { program_hash, .. } if program_hash == resident_hash
    ));

    h.companion
        .set_schedule(Request::new(CompanionSetScheduleRequest {
            node_id: schedule_node.node_id.clone(),
            interval_s: 900,
        }))
        .await
        .unwrap();
    let (_, _, schedule_payload) = do_wake(&h.gateway, &schedule_node, 601, &[0u8; 32], None).await;
    assert!(matches!(
        schedule_payload,
        CommandPayload::UpdateSchedule { interval_s: 900 }
    ));

    h.companion
        .queue_reboot(Request::new(CompanionQueueRebootRequest {
            node_id: reboot_node.node_id.clone(),
        }))
        .await
        .unwrap();
    let (_, _, reboot_payload) = do_wake(&h.gateway, &reboot_node, 602, &[0u8; 32], None).await;
    assert!(matches!(reboot_payload, CommandPayload::Reboot));

    h.companion
        .queue_ephemeral(Request::new(CompanionQueueEphemeralRequest {
            node_id: ephemeral_node.node_id.clone(),
            program_hash: ephemeral_hash.clone(),
        }))
        .await
        .unwrap();
    let (_, _, ephemeral_payload) =
        do_wake(&h.gateway, &ephemeral_node, 603, &[0u8; 32], None).await;
    assert!(matches!(
        ephemeral_payload,
        CommandPayload::RunEphemeral { program_hash, .. } if program_hash == ephemeral_hash
    ));
}

#[tokio::test]
async fn t0824_companion_queries_report_node_state() {
    let h = CompanionHarness::new();
    let node = TestNode::new("epsilon", 0x5050, [0x55; 32]);
    let program_hash = vec![0x99; 32];

    register_node(&h.storage, &node).await;
    let _ = do_wake(&h.gateway, &node, 500, &program_hash, None).await;

    let list = h
        .companion
        .list_nodes(Request::new(CompanionListNodesRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.nodes.len(), 1);
    assert_eq!(list.nodes[0].node_id, node.node_id);

    let info = h
        .companion
        .get_node(Request::new(CompanionGetNodeRequest {
            node_id: node.node_id.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.node_id, node.node_id);
    assert_eq!(info.current_program_hash, None);
    assert_eq!(info.last_battery_mv, Some(3300));
    assert_eq!(info.last_firmware_abi_version, Some(1));
    assert_eq!(info.schedule_interval_s, Some(60));

    let status = h
        .companion
        .get_node_status(Request::new(CompanionGetNodeStatusRequest {
            node_id: node.node_id.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.node_id, node.node_id);
    assert!(status.current_program_hash.is_empty());
    assert_eq!(status.battery_mv, Some(3300));
    assert_eq!(status.firmware_abi_version, Some(1));
    assert!(status.last_seen_ms.is_some());
    assert!(status.has_active_session);
}

#[test]
fn t0825_companion_contract_excludes_operator_only_workflows() {
    let proto = include_str!("../proto/companion.proto");

    for rpc in [
        "rpc StreamEvents",
        "rpc ListNodes",
        "rpc GetNode",
        "rpc AssignProgram",
        "rpc SetSchedule",
        "rpc QueueReboot",
        "rpc QueueEphemeral",
        "rpc GetNodeStatus",
        "rpc ShowModemDisplayMessage",
    ] {
        assert!(
            proto.contains(rpc),
            "expected companion contract to include `{rpc}`"
        );
    }

    for rpc in [
        "RegisterNode",
        "RemoveNode",
        "IngestProgram",
        "RemoveProgram",
        "ExportState",
        "ImportState",
        "SetModemChannel",
        "BeginBlePairing",
        "EndBlePairing",
        "AddHandler",
        "RemoveHandler",
    ] {
        assert!(
            !proto.contains(rpc),
            "operator-only workflow `{rpc}` must not appear in companion contract"
        );
    }
}
