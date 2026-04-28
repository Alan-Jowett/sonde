// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

mod common;

use std::collections::HashMap;
#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ciborium::Value;
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::sync::RwLock;
use tonic::Request;

use sonde_gateway::admin::pb::gateway_admin_client::GatewayAdminClient;
use sonde_gateway::admin::pb::Empty;

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
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
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
            pending_commands.clone(),
            event_hub.clone(),
            DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE,
        );
        Self {
            storage,
            pending_commands,
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

async fn store_program_with_profile(
    storage: &Arc<InMemoryStorage>,
    hash_byte: u8,
    verification_profile: VerificationProfile,
) -> Vec<u8> {
    let hash = vec![hash_byte; 32];
    storage
        .store_program(&ProgramRecord {
            hash: hash.clone(),
            image: make_cbor_image(MINIMAL_BPF),
            size: MINIMAL_BPF.len() as u32,
            verification_profile,
            abi_version: None,
            source_filename: None,
        })
        .await
        .unwrap();
    hash
}

async fn store_program(storage: &Arc<InMemoryStorage>, hash_byte: u8) -> Vec<u8> {
    store_program_with_profile(storage, hash_byte, VerificationProfile::Resident).await
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

async fn spawn_connection_with_capacity(
    service: ConnectorService,
    capacity: usize,
) -> (DuplexStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(capacity);
    let handle = tokio::spawn(async move {
        service
            .handle_connection(server)
            .await
            .expect("connector session must complete cleanly");
    });
    tokio::task::yield_now().await;
    (client, handle)
}

async fn spawn_connection(
    service: ConnectorService,
) -> (DuplexStream, tokio::task::JoinHandle<()>) {
    spawn_connection_with_capacity(service, 16 * 1024).await
}

async fn write_framed<T>(stream: &mut T, payload: &[u8])
where
    T: AsyncWrite + Unpin,
{
    let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

async fn read_framed<T>(stream: &mut T) -> Vec<u8>
where
    T: AsyncRead + Unpin,
{
    let mut len = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut len))
        .await
        .expect("timed out waiting for connector frame length")
        .unwrap();
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap();
    let mut payload = vec![0u8; len];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut payload))
        .await
        .expect("timed out waiting for connector frame payload")
        .unwrap();
    payload
}

#[cfg(windows)]
static NEXT_CONNECTOR_TEST_PIPE_ID: AtomicU64 = AtomicU64::new(1);

async fn expect_session_closed<T>(stream: &mut T)
where
    T: AsyncRead + Unpin,
{
    let mut byte = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte))
        .await
        .expect("connector session should close promptly")
        .unwrap();
    assert_eq!(n, 0, "connector session should close cleanly");
}

async fn wait_for_desired_state(
    harness: &ConnectorHarness,
    node_id: &str,
    expected_program_hash: Option<&[u8]>,
    expected_schedule_interval_s: u32,
) -> sonde_gateway::registry::NodeRecord {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let stored = harness.storage.get_node(node_id).await.unwrap().unwrap();
            if stored.assigned_program_hash.as_deref() == expected_program_hash
                && stored.schedule_interval_s == expected_schedule_interval_s
            {
                break stored;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for desired state to be applied")
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

#[cfg(unix)]
async fn connect_connector_socket(
    socket_path: &str,
) -> Result<tokio::net::UnixStream, Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(err) if tokio::time::Instant::now() < deadline => {
                let _ = err;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

#[cfg(windows)]
async fn connect_connector_socket(
    pipe_name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, Box<dyn std::error::Error>> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return Ok(client),
            Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {}
            Err(e) => return Err(e.into()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connector client timed out waiting for named pipe",
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(unix)]
async fn connect_admin_client(
    socket_path: &str,
) -> Result<GatewayAdminClient<tonic::transport::Channel>, Box<dyn std::error::Error>> {
    use hyper_util::rt::TokioIo;
    use tonic::transport::{Endpoint, Uri};
    use tower::service_fn;

    let socket_path = socket_path.to_owned();
    let channel = Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                let stream = loop {
                    match tokio::net::UnixStream::connect(&path).await {
                        Ok(stream) => break stream,
                        Err(err) if tokio::time::Instant::now() < deadline => {
                            let _ = err;
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(err) => return Err(err),
                    }
                };
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(GatewayAdminClient::new(channel))
}

#[cfg(windows)]
async fn connect_admin_client(
    pipe_name: &str,
) -> Result<GatewayAdminClient<tonic::transport::Channel>, Box<dyn std::error::Error>> {
    use hyper_util::rt::TokioIo;
    use tokio::net::windows::named_pipe::ClientOptions;
    use tonic::transport::{Endpoint, Uri};

    let pipe_name = pipe_name.to_owned();
    let channel = Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let name = pipe_name.clone();
            async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                let client = loop {
                    match ClientOptions::new().open(&name) {
                        Ok(client) => break client,
                        Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {}
                        Err(e) => return Err(e),
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "admin client timed out waiting for named pipe",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                };
                Ok::<_, std::io::Error>(TokioIo::new(client))
            }
        }))
        .await?;
    Ok(GatewayAdminClient::new(channel))
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

    expect_session_closed(&mut client).await;

    handle.await.unwrap();
}

#[tokio::test]
async fn connector_transport_uses_real_ipc_and_rejects_second_client() {
    let harness = ConnectorHarness::new();
    let _tmp_dir = TempDir::new().expect("failed to create temp dir");
    #[cfg(unix)]
    let socket_path = _tmp_dir.path().join("connector.sock");
    #[cfg(windows)]
    let socket_path = format!(
        r"\\.\pipe\sonde-connector-test-{}-{}",
        std::process::id(),
        NEXT_CONNECTOR_TEST_PIPE_ID.fetch_add(1, Ordering::Relaxed)
    );
    #[cfg(unix)]
    let socket_path_str = socket_path.to_string_lossy().to_string();
    #[cfg(windows)]
    let socket_path_str = socket_path.clone();

    let service = harness.service.clone();
    let server_socket_path = socket_path_str.clone();
    let server_handle = tokio::spawn(async move {
        sonde_gateway::connector::serve_connector(service, &server_socket_path)
            .await
            .expect("connector server should run");
    });

    let mut admin_client = connect_admin_client(&socket_path_str)
        .await
        .expect("admin client transport should connect");
    assert!(
        admin_client
            .list_nodes(Request::new(Empty {}))
            .await
            .is_err(),
        "connector socket must not accept admin gRPC calls"
    );

    let mut first = connect_connector_socket(&socket_path_str)
        .await
        .expect("first connector client should connect");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut second = connect_connector_socket(&socket_path_str)
        .await
        .expect("second connector client should reach IPC endpoint");
    expect_session_closed(&mut second).await;

    write_framed(
        &mut first,
        &encode_value(&Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
            map_entry(2, Value::Text("gateway".to_string())),
            map_entry(3, Value::Text(String::new())),
            map_entry(4, Value::Map(Vec::new())),
        ])),
    )
    .await;

    drop(first);
    server_handle.abort();
}

#[tokio::test]
async fn connector_rejects_oversized_declared_frame() {
    let harness = ConnectorHarness::new();
    let service = ConnectorService::new(
        harness.storage.clone(),
        Arc::new(RwLock::new(HashMap::new())),
        harness.event_hub.clone(),
        8,
    );
    let (mut client, handle) = spawn_connection(service).await;

    client.write_all(&9u32.to_be_bytes()).await.unwrap();
    client.write_all(&[0u8; 9]).await.unwrap();
    client.flush().await.unwrap();

    expect_session_closed(&mut client).await;

    handle.await.unwrap();
}

#[tokio::test]
async fn connector_rejects_truncated_declared_frame() {
    let harness = ConnectorHarness::new();
    let (mut client, handle) = spawn_connection(harness.service.clone()).await;

    client.write_all(&8u32.to_be_bytes()).await.unwrap();
    client.write_all(&[0u8; 3]).await.unwrap();
    client.shutdown().await.unwrap();

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

    let stored = wait_for_desired_state(&harness, &node.node_id, Some(&program_hash), 900).await;
    assert_eq!(stored.assigned_program_hash, Some(program_hash.clone()));
    assert_eq!(stored.desired_schedule_interval_s, Some(900));
    assert_eq!(stored.schedule_interval_s, 900);

    let other_stored = wait_for_desired_state(&harness, &other.node_id, None, 60).await;
    assert_eq!(other_stored.assigned_program_hash, None);
    assert_eq!(other_stored.desired_schedule_interval_s, Some(60));
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
async fn connector_null_schedule_clears_desired_schedule_target() {
    let harness = ConnectorHarness::new();
    let node = TestNode::new("alpha-clear", 0x1111, [0x31; 32]);
    register_node(&harness.storage, &node).await;

    let mut stored = harness
        .storage
        .get_node(&node.node_id)
        .await
        .unwrap()
        .unwrap();
    stored.desired_schedule_interval_s = Some(900);
    stored.schedule_interval_s = 900;
    harness.storage.upsert_node(&stored).await.unwrap();

    let (mut client, handle) = spawn_connection(harness.service.clone()).await;
    let desired = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("node".to_string())),
        map_entry(3, Value::Text(node.node_id.clone())),
        map_entry(4, Value::Map(vec![map_entry(2, Value::Null)])),
    ]);
    write_framed(&mut client, &encode_value(&desired)).await;

    let stored = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let stored = harness
                .storage
                .get_node(&node.node_id)
                .await
                .unwrap()
                .unwrap();
            if stored.desired_schedule_interval_s.is_none() {
                break stored;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for desired schedule clear");
    assert_eq!(stored.schedule_interval_s, 900);

    let payload = do_wake(&harness.gateway, &node, 101, &[0x42; 32], None).await;
    assert!(matches!(payload, CommandPayload::Nop));

    drop(client);
    handle.await.unwrap();
}

#[tokio::test]
async fn connector_invalid_ephemeral_preserves_existing_desired_state() {
    let harness = ConnectorHarness::new();
    let node = TestNode::new("alpha-ephemeral", 0x1212, [0x44; 32]);
    let old_assigned = store_program(&harness.storage, 0x51).await;
    let new_assigned = store_program(&harness.storage, 0x52).await;
    let old_ephemeral =
        store_program_with_profile(&harness.storage, 0x61, VerificationProfile::Ephemeral).await;
    let invalid_ephemeral = store_program(&harness.storage, 0x62).await;

    register_node(&harness.storage, &node).await;

    let mut stored = harness
        .storage
        .get_node(&node.node_id)
        .await
        .unwrap()
        .unwrap();
    stored.assigned_program_hash = Some(old_assigned.clone());
    stored.desired_schedule_interval_s = Some(120);
    stored.schedule_interval_s = 120;
    harness.storage.upsert_node(&stored).await.unwrap();

    harness.pending_commands.write().await.insert(
        node.node_id.clone(),
        vec![
            PendingCommand::UpdateSchedule { interval_s: 120 },
            PendingCommand::RunEphemeral {
                program_hash: old_ephemeral.clone(),
            },
        ],
    );

    let (mut client, handle) = spawn_connection(harness.service.clone()).await;
    let desired = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("node".to_string())),
        map_entry(3, Value::Text(node.node_id.clone())),
        map_entry(
            4,
            Value::Map(vec![
                map_entry(1, Value::Bytes(new_assigned.clone())),
                map_entry(2, Value::Integer(900u64.into())),
                map_entry(3, Value::Bytes(invalid_ephemeral)),
            ]),
        ),
    ]);
    write_framed(&mut client, &encode_value(&desired)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stored = harness
        .storage
        .get_node(&node.node_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.assigned_program_hash, Some(old_assigned));
    assert_eq!(stored.desired_schedule_interval_s, Some(120));
    assert_eq!(stored.schedule_interval_s, 120);

    let pending = harness.pending_commands.read().await;
    let commands = pending
        .get(&node.node_id)
        .expect("existing pending commands must be preserved");
    assert_eq!(commands.len(), 2);
    assert!(
        matches!(
            commands[0],
            PendingCommand::UpdateSchedule { interval_s: 120 }
        ),
        "expected preserved UpdateSchedule(120), got {:?}",
        commands[0]
    );
    match &commands[1] {
        PendingCommand::RunEphemeral { program_hash } => {
            assert_eq!(program_hash, &old_ephemeral);
        }
        other => panic!("expected preserved RunEphemeral, got {other:?}"),
    }

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

#[tokio::test]
async fn connector_emits_desynchronized_health_after_actual_subscriber_lag() {
    let storage = Arc::new(InMemoryStorage::new());
    let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let event_hub = Arc::new(ConnectorEventHub::new(1));
    let service = ConnectorService::new(
        storage,
        pending_commands,
        event_hub.clone(),
        DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE,
    );
    let (mut client, handle) = spawn_connection_with_capacity(service, 256).await;
    let remediation = "R".repeat(1024);

    for i in 0..32 {
        event_hub.emit_health(
            ConnectorHealthState::Ok,
            format!("steady_state_{i}"),
            vec!["actual_state".to_string()],
            remediation.clone(),
        );
    }

    let mut saw_desync = false;
    for _ in 0..8 {
        let message = decode_map(&read_framed(&mut client).await);
        if uint_field(&message, 1) != MSG_TYPE_CONNECTOR_HEALTH {
            continue;
        }
        if text_field(&message, 2) != "desynchronized" {
            continue;
        }
        let details = map_get(&message, 4).as_map().cloned().unwrap();
        assert_eq!(text_field(&details, 1), "subscriber_lag");
        let scopes = map_get(&details, 2).as_array().unwrap();
        assert_eq!(scopes.len(), 4);
        assert_eq!(scopes[3].as_text().unwrap(), "reconciliation_progress");
        saw_desync = true;
        break;
    }

    assert!(
        saw_desync,
        "expected a desynchronized health message after lag"
    );

    drop(client);
    handle.await.unwrap();
}
