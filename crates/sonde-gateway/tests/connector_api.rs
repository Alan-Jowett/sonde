// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ciborium::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::RwLock;

use sonde_gateway::connector::{
    ConnectorEventHub, ConnectorHealthState, ConnectorPayloadOrigin, ConnectorService,
    DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE, MSG_TYPE_ACTUAL_STATE, MSG_TYPE_APP_DATA,
    MSG_TYPE_CONNECTOR_HEALTH, MSG_TYPE_DESIRED_STATE,
};
use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::handler::HandlerRouter;
use sonde_gateway::program::{ProgramRecord, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
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

struct ConnectorHarness {
    storage: Arc<InMemoryStorage>,
    gateway: Gateway,
    event_hub: Arc<ConnectorEventHub>,
    service: ConnectorService,
}

impl ConnectorHarness {
    fn new() -> Self {
        let storage = Arc::new(InMemoryStorage::new());
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let gateway = Gateway::new_with_pending(
            storage.clone(),
            pending_commands.clone(),
            session_manager,
            Arc::new(RwLock::new(HandlerRouter::new(Vec::new()))),
        );
        let event_hub = gateway.connector_event_hub();
        let service = ConnectorService::new(
            storage.clone(),
            pending_commands,
            event_hub.clone(),
            DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE,
        );
        Self {
            storage,
            gateway,
            event_hub,
            service,
        }
    }
}

const MINIMAL_BPF: &[u8] = &[
    0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn make_cbor_image(bytecode: &[u8]) -> Vec<u8> {
    let image = ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    image.encode_deterministic().unwrap()
}

async fn store_program(storage: &Arc<InMemoryStorage>, hash_byte: u8) -> Vec<u8> {
    let hash = vec![hash_byte; 32];
    storage
        .store_program(&ProgramRecord {
            hash: hash.clone(),
            image: make_cbor_image(MINIMAL_BPF),
            size: MINIMAL_BPF.len() as u32,
            verification_profile: VerificationProfile::Resident,
            abi_version: None,
            source_filename: None,
        })
        .await
        .unwrap();
    hash
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

async fn do_wake(
    gateway: &Gateway,
    node: &TestNode,
    nonce: u64,
    program_hash: &[u8],
    blob: Option<Vec<u8>>,
) -> CommandPayload {
    let frame = node.build_wake(nonce, 1, program_hash, 3300, blob);
    let resp = gateway
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");
    let decoded = decode_frame(&resp).unwrap();
    let plaintext = open_frame(&decoded, &node.psk, &GatewayAead, &RustCryptoSha256).unwrap();
    match GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap() {
        GatewayMessage::Command { payload, .. } => payload,
        other => panic!("expected Command, got {other:?}"),
    }
}

async fn spawn_connection(
    service: ConnectorService,
) -> (DuplexStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(16 * 1024);
    let handle = tokio::spawn(async move {
        service
            .handle_connection(server)
            .await
            .expect("connector session must complete cleanly");
    });
    tokio::task::yield_now().await;
    (client, handle)
}

async fn write_framed(stream: &mut DuplexStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

async fn read_framed(stream: &mut DuplexStream) -> Vec<u8> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await.unwrap();
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap();
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.unwrap();
    payload
}

fn encode_value(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).unwrap();
    buf
}

fn map_entry(key: u64, value: Value) -> (Value, Value) {
    (Value::Integer(key.into()), value)
}

fn decode_map(bytes: &[u8]) -> Vec<(Value, Value)> {
    let value: Value = ciborium::from_reader(bytes).unwrap();
    value.as_map().cloned().unwrap()
}

fn map_get(map: &[(Value, Value)], key: u64) -> &Value {
    map.iter()
        .find_map(
            |(k, v)| match k.as_integer().and_then(|i| u64::try_from(i).ok()) {
                Some(found) if found == key => Some(v),
                _ => None,
            },
        )
        .unwrap_or_else(|| panic!("missing key {key}"))
}

fn text_field(map: &[(Value, Value)], key: u64) -> String {
    map_get(map, key).as_text().unwrap().to_string()
}

fn bytes_field(map: &[(Value, Value)], key: u64) -> Vec<u8> {
    map_get(map, key).as_bytes().unwrap().to_vec()
}

fn uint_field(map: &[(Value, Value)], key: u64) -> u64 {
    map_get(map, key)
        .as_integer()
        .and_then(|i| u64::try_from(i).ok())
        .unwrap()
}

#[tokio::test]
async fn connector_rejects_non_connector_protocol_bytes() {
    let harness = ConnectorHarness::new();
    let (mut client, handle) = spawn_connection(harness.service.clone()).await;

    client
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .unwrap();
    client.flush().await.unwrap();

    let mut byte = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(1), client.read(&mut byte))
        .await
        .expect("connector session should close promptly")
        .unwrap();
    assert_eq!(n, 0, "connector session should close on non-framed input");

    handle.await.unwrap();
}

#[tokio::test]
async fn connector_desired_state_updates_gateway_reconciliation_state() {
    let harness = ConnectorHarness::new();
    let node = TestNode::new("alpha", 0x1010, [0x11; 32]);
    let other = TestNode::new("beta", 0x2020, [0x22; 32]);
    let program_hash = store_program(&harness.storage, 0x55).await;

    register_node(&harness.storage, &node).await;
    register_node(&harness.storage, &other).await;

    let mut current = harness
        .storage
        .get_node(&node.node_id)
        .await
        .unwrap()
        .unwrap();
    current.current_program_hash = Some(program_hash.clone());
    harness.storage.upsert_node(&current).await.unwrap();

    let (mut client, handle) = spawn_connection(harness.service.clone()).await;

    let desired = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("node".to_string())),
        map_entry(3, Value::Text(node.node_id.clone())),
        map_entry(
            4,
            Value::Map(vec![
                map_entry(1, Value::Bytes(program_hash.clone())),
                map_entry(2, Value::Integer(900u64.into())),
            ]),
        ),
    ]);
    write_framed(&mut client, &encode_value(&desired)).await;

    let invalid = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("mystery".to_string())),
        map_entry(3, Value::Text(other.node_id.clone())),
        map_entry(4, Value::Map(Vec::new())),
    ]);
    write_framed(&mut client, &encode_value(&invalid)).await;

    let gateway_desired = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("gateway".to_string())),
        map_entry(3, Value::Text(String::new())),
        map_entry(4, Value::Map(Vec::new())),
    ]);
    write_framed(&mut client, &encode_value(&gateway_desired)).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let stored = harness
        .storage
        .get_node(&node.node_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.assigned_program_hash, Some(program_hash.clone()));
    assert_eq!(stored.schedule_interval_s, 900);

    let other_stored = harness
        .storage
        .get_node(&other.node_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other_stored.assigned_program_hash, None);
    assert_eq!(other_stored.schedule_interval_s, 60);

    let payload = do_wake(&harness.gateway, &node, 100, &program_hash, None).await;
    assert!(matches!(
        payload,
        CommandPayload::UpdateSchedule { interval_s: 900 }
    ));

    drop(client);
    handle.await.unwrap();
}

#[tokio::test]
async fn connector_emits_actual_state_after_wake() {
    let harness = ConnectorHarness::new();
    let node = TestNode::new("gamma", 0x3030, [0x33; 32]);
    let program_hash = vec![0x66; 32];

    register_node(&harness.storage, &node).await;

    let (mut client, handle) = spawn_connection(harness.service.clone()).await;
    let _ = do_wake(&harness.gateway, &node, 200, &program_hash, None).await;

    let message = decode_map(&read_framed(&mut client).await);
    assert_eq!(uint_field(&message, 1), MSG_TYPE_ACTUAL_STATE);
    assert_eq!(text_field(&message, 2), "node");
    assert_eq!(text_field(&message, 3), node.node_id);
    assert_eq!(bytes_field(&message, 4), program_hash);
    assert_eq!(uint_field(&message, 6), 3300);
    assert_eq!(uint_field(&message, 7), 1);
    assert_eq!(text_field(&message, 8), "0.5.0");
    assert!(uint_field(&message, 9) > 0);

    drop(client);
    handle.await.unwrap();
}

#[tokio::test]
async fn connector_emits_app_data_and_wake_blob_messages() {
    let harness = ConnectorHarness::new();
    let node = TestNode::new("delta", 0x4040, [0x44; 32]);
    let program_hash = vec![0x77; 32];

    register_node(&harness.storage, &node).await;
    let mut stored = harness
        .storage
        .get_node(&node.node_id)
        .await
        .unwrap()
        .unwrap();
    stored.current_program_hash = Some(program_hash.clone());
    harness.storage.upsert_node(&stored).await.unwrap();

    let (mut client, handle) = spawn_connection(harness.service.clone()).await;

    let _ = do_wake(
        &harness.gateway,
        &node,
        300,
        &program_hash,
        Some(vec![0xCC]),
    )
    .await;

    let first = decode_map(&read_framed(&mut client).await);
    let second = decode_map(&read_framed(&mut client).await);
    assert_eq!(uint_field(&first, 1), MSG_TYPE_ACTUAL_STATE);
    assert_eq!(uint_field(&second, 1), MSG_TYPE_APP_DATA);
    assert_eq!(
        text_field(&second, 6),
        ConnectorPayloadOrigin::WakeBlob.as_str()
    );
    assert_eq!(bytes_field(&second, 4), vec![0xCC]);

    let wake_frame = node.build_wake(301, 1, &program_hash, 3300, None);
    let response = harness
        .gateway
        .process_frame(&wake_frame, node.peer_address())
        .await
        .expect("expected wake response");
    let decoded = decode_frame(&response).unwrap();
    let plaintext = open_frame(&decoded, &node.psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let starting_seq = match GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap() {
        GatewayMessage::Command { starting_seq, .. } => starting_seq,
        other => panic!("expected command, got {other:?}"),
    };

    let app_payload = vec![0xAA, 0xBB];
    let app_frame = node.build_app_data(starting_seq, &app_payload);
    assert!(harness
        .gateway
        .process_frame(&app_frame, node.peer_address())
        .await
        .is_none());

    let node_status_message = decode_map(&read_framed(&mut client).await);
    assert_eq!(uint_field(&node_status_message, 1), MSG_TYPE_ACTUAL_STATE);

    let app_message = decode_map(&read_framed(&mut client).await);
    assert_eq!(uint_field(&app_message, 1), MSG_TYPE_APP_DATA);
    assert_eq!(text_field(&app_message, 2), node.node_id);
    assert_eq!(bytes_field(&app_message, 3), program_hash);
    assert_eq!(bytes_field(&app_message, 4), app_payload);
    assert_eq!(
        text_field(&app_message, 6),
        ConnectorPayloadOrigin::AppData.as_str()
    );

    drop(client);
    handle.await.unwrap();
}

#[tokio::test]
async fn connector_health_messages_are_delivered_as_framed_cbor() {
    let harness = ConnectorHarness::new();
    let (mut client, handle) = spawn_connection(harness.service.clone()).await;

    harness.event_hub.emit_health(
        ConnectorHealthState::Desynchronized,
        "test_fault",
        vec![
            "desired_state".to_string(),
            "actual_state".to_string(),
            "app_data".to_string(),
        ],
        "Rebuild state from the gateway.",
    );

    let message = decode_map(&read_framed(&mut client).await);
    assert_eq!(uint_field(&message, 1), MSG_TYPE_CONNECTOR_HEALTH);
    assert_eq!(text_field(&message, 2), "desynchronized");
    let details = map_get(&message, 4).as_map().cloned().unwrap();
    assert_eq!(text_field(&details, 1), "test_fault");
    let scopes = map_get(&details, 2).as_array().unwrap();
    assert_eq!(scopes.len(), 3);
    assert_eq!(scopes[2].as_text().unwrap(), "app_data");
    assert_eq!(text_field(&details, 3), "Rebuild state from the gateway.");

    drop(client);
    handle.await.unwrap();
}
