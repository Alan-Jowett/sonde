// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use ciborium::Value;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, RwLock, Semaphore};
use tokio_util::bytes::Bytes;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{error, info, warn};

use crate::admin::{
    assign_program_impl, queue_ephemeral_impl, set_schedule_impl, system_time_to_millis,
};
use crate::engine::PendingCommand;
use crate::storage::Storage;

pub const MSG_TYPE_DESIRED_STATE: u64 = 0x01;
pub const MSG_TYPE_ACTUAL_STATE: u64 = 0x02;
pub const MSG_TYPE_APP_DATA: u64 = 0x03;
pub const MSG_TYPE_CONNECTOR_HEALTH: u64 = 0x04;
pub const DEFAULT_CONNECTOR_EVENT_BUFFER: usize = 64;
pub const DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE: usize = 1_048_576;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorPayloadOrigin {
    AppData,
    WakeBlob,
}

impl ConnectorPayloadOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppData => "app_data",
            Self::WakeBlob => "wake_blob",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorHealthState {
    Ok,
    Degraded,
    Desynchronized,
}

impl ConnectorHealthState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Desynchronized => "desynchronized",
        }
    }
}

#[derive(Clone, Debug)]
enum ConnectorOutboundMessage {
    ActualState {
        entity_kind: &'static str,
        entity_id: String,
        current_program_hash: Option<Vec<u8>>,
        assigned_program_hash: Option<Vec<u8>>,
        battery_mv: Option<u32>,
        firmware_abi_version: Option<u32>,
        firmware_version: Option<String>,
        timestamp_ms: u64,
    },
    AppData {
        node_id: String,
        program_hash: Vec<u8>,
        payload: Vec<u8>,
        timestamp_ms: u64,
        payload_origin: ConnectorPayloadOrigin,
    },
    Health {
        health_state: ConnectorHealthState,
        timestamp_ms: u64,
        failure_mode: String,
        stale_scope: Vec<String>,
        remediation: String,
    },
}

impl ConnectorOutboundMessage {
    fn encode(&self) -> Result<Vec<u8>, String> {
        let message = match self {
            Self::ActualState {
                entity_kind,
                entity_id,
                current_program_hash,
                assigned_program_hash,
                battery_mv,
                firmware_abi_version,
                firmware_version,
                timestamp_ms,
            } => Value::Map(vec![
                map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
                map_entry(2, Value::Text((*entity_kind).to_string())),
                map_entry(3, Value::Text(entity_id.clone())),
                map_entry(4, opt_bytes_value(current_program_hash.as_deref())),
                map_entry(5, opt_bytes_value(assigned_program_hash.as_deref())),
                map_entry(6, opt_u32_value(*battery_mv)),
                map_entry(7, opt_u32_value(*firmware_abi_version)),
                map_entry(8, opt_text_value(firmware_version.as_deref())),
                map_entry(9, Value::Integer((*timestamp_ms).into())),
                map_entry(10, Value::Map(Vec::new())),
            ]),
            Self::AppData {
                node_id,
                program_hash,
                payload,
                timestamp_ms,
                payload_origin,
            } => Value::Map(vec![
                map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
                map_entry(2, Value::Text(node_id.clone())),
                map_entry(3, Value::Bytes(program_hash.clone())),
                map_entry(4, Value::Bytes(payload.clone())),
                map_entry(5, Value::Integer((*timestamp_ms).into())),
                map_entry(6, Value::Text(payload_origin.as_str().to_string())),
            ]),
            Self::Health {
                health_state,
                timestamp_ms,
                failure_mode,
                stale_scope,
                remediation,
            } => Value::Map(vec![
                map_entry(1, Value::Integer(MSG_TYPE_CONNECTOR_HEALTH.into())),
                map_entry(2, Value::Text(health_state.as_str().to_string())),
                map_entry(3, Value::Integer((*timestamp_ms).into())),
                map_entry(
                    4,
                    Value::Map(vec![
                        map_entry(1, Value::Text(failure_mode.clone())),
                        map_entry(
                            2,
                            Value::Array(
                                stale_scope
                                    .iter()
                                    .cloned()
                                    .map(Value::Text)
                                    .collect::<Vec<_>>(),
                            ),
                        ),
                        map_entry(3, Value::Text(remediation.clone())),
                    ]),
                ),
            ]),
        };

        let mut bytes = Vec::new();
        ciborium::into_writer(&message, &mut bytes)
            .map_err(|e| format!("failed to encode connector message: {e}"))?;
        Ok(bytes)
    }
}

#[derive(Clone)]
pub struct ConnectorEventHub {
    tx: broadcast::Sender<ConnectorOutboundMessage>,
}

impl Default for ConnectorEventHub {
    fn default() -> Self {
        Self::new(DEFAULT_CONNECTOR_EVENT_BUFFER)
    }
}

impl ConnectorEventHub {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    fn subscribe(&self) -> broadcast::Receiver<ConnectorOutboundMessage> {
        self.tx.subscribe()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_actual_state_for_node(
        &self,
        node_id: String,
        current_program_hash: Vec<u8>,
        assigned_program_hash: Option<Vec<u8>>,
        battery_mv: u32,
        firmware_abi_version: u32,
        firmware_version: String,
        timestamp_ms: u64,
    ) {
        let _ = self.tx.send(ConnectorOutboundMessage::ActualState {
            entity_kind: "node",
            entity_id: node_id,
            current_program_hash: Some(current_program_hash),
            assigned_program_hash,
            battery_mv: Some(battery_mv),
            firmware_abi_version: Some(firmware_abi_version),
            firmware_version: Some(firmware_version),
            timestamp_ms,
        });
    }

    pub fn emit_app_data(
        &self,
        node_id: String,
        program_hash: Vec<u8>,
        payload: Vec<u8>,
        timestamp_ms: u64,
        payload_origin: ConnectorPayloadOrigin,
    ) {
        let _ = self.tx.send(ConnectorOutboundMessage::AppData {
            node_id,
            program_hash,
            payload,
            timestamp_ms,
            payload_origin,
        });
    }

    pub fn emit_health(
        &self,
        health_state: ConnectorHealthState,
        failure_mode: impl Into<String>,
        stale_scope: Vec<String>,
        remediation: impl Into<String>,
    ) {
        let timestamp_ms = current_time_ms();
        let _ = self.tx.send(ConnectorOutboundMessage::Health {
            health_state,
            timestamp_ms,
            failure_mode: failure_mode.into(),
            stale_scope,
            remediation: remediation.into(),
        });
    }
}

#[derive(Clone)]
pub struct ConnectorService {
    storage: Arc<dyn Storage>,
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    event_hub: Arc<ConnectorEventHub>,
    max_message_size: usize,
}

impl ConnectorService {
    pub fn new(
        storage: Arc<dyn Storage>,
        pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
        event_hub: Arc<ConnectorEventHub>,
        max_message_size: usize,
    ) -> Self {
        Self {
            storage,
            pending_commands,
            event_hub,
            max_message_size: max_message_size.max(1),
        }
    }

    pub async fn handle_connection<T>(&self, stream: T) -> Result<(), String>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut framed = Framed::new(stream, connector_codec(self.max_message_size));
        let mut outbound = self.event_hub.subscribe();

        loop {
            tokio::select! {
                inbound = framed.next() => {
                    match inbound {
                        Some(Ok(bytes)) => {
                            if let Err(e) = self.handle_inbound_message(bytes.as_ref()).await {
                                warn!(error = %e, "rejecting connector message");
                            }
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "closing connector session after framing error");
                            break;
                        }
                        None => break,
                    }
                }
                outbound_msg = outbound.recv() => {
                    match outbound_msg {
                        Ok(message) => {
                            let encoded = message.encode()?;
                            if let Err(e) = framed.send(Bytes::from(encoded)).await {
                                warn!(error = %e, "closing connector session after write failure");
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "connector subscriber lagged; marking connector state desynchronized");
                            let health = ConnectorOutboundMessage::Health {
                                health_state: ConnectorHealthState::Desynchronized,
                                timestamp_ms: current_time_ms(),
                                failure_mode: "subscriber_lag".to_string(),
                                stale_scope: vec![
                                    "desired_state".to_string(),
                                    "actual_state".to_string(),
                                    "app_data".to_string(),
                                    "reconciliation_progress".to_string(),
                                ],
                                remediation: "Reconnect the connector and rebuild the control-plane view from authoritative gateway state.".to_string(),
                            };
                            let encoded = health.encode()?;
                            if let Err(e) = framed.send(Bytes::from(encoded)).await {
                                warn!(error = %e, "closing connector session after lagged health write failure");
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_inbound_message(&self, bytes: &[u8]) -> Result<(), String> {
        let map = decode_map(bytes)?;
        let msg_type = required_u64(&map, 1, "msg_type")?;
        if msg_type != MSG_TYPE_DESIRED_STATE {
            return Err(format!(
                "unsupported inbound connector msg_type `{msg_type}`"
            ));
        }

        let entity_kind = required_text(&map, 2, "entity_kind")?;
        let entity_id = required_text(&map, 3, "entity_id")?;
        let desired_state = required_map(&map, 4, "desired_state")?;

        match entity_kind.as_str() {
            "gateway" => {
                // The current draft defines an empty gateway desired-state map.
                let _ = entity_id;
                let _ = desired_state;
                Ok(())
            }
            "node" => {
                self.apply_node_desired_state(&entity_id, &desired_state)
                    .await
            }
            other => Err(format!("unknown entity_kind `{other}`")),
        }
    }

    async fn apply_node_desired_state(
        &self,
        node_id: &str,
        desired_state: &[(Value, Value)],
    ) -> Result<(), String> {
        require_existing_node(&self.storage, node_id).await?;

        let assigned_program_hash =
            optional_bytes_field(desired_state, 1, "assigned_program_hash")?;
        let schedule_interval_s = optional_u32_field(desired_state, 2, "schedule_interval_s")?;
        let ephemeral_program_hash =
            optional_bytes_field(desired_state, 3, "ephemeral_program_hash")?;

        match assigned_program_hash {
            Some(program_hash) => {
                assign_program_impl(&self.storage, node_id, &program_hash)
                    .await
                    .map_err(|e| format!("assign connector desired program failed: {e}"))?;
            }
            None => {
                update_node_record(&self.storage, node_id, |node| {
                    node.assigned_program_hash = None;
                })
                .await
                .map_err(|e| format!("clear connector desired program failed: {e}"))?;
            }
        }

        match schedule_interval_s {
            Some(interval_s) => {
                set_schedule_impl(&self.storage, &self.pending_commands, node_id, interval_s)
                    .await
                    .map_err(|e| format!("set connector desired schedule failed: {e}"))?;
            }
            None => {
                update_node_record(&self.storage, node_id, |node| {
                    node.desired_schedule_interval_s = None;
                })
                .await
                .map_err(|e| format!("clear connector desired schedule failed: {e}"))?;
                let mut pending = self.pending_commands.write().await;
                if let Some(commands) = pending.get_mut(node_id) {
                    commands.retain(|cmd| !matches!(cmd, PendingCommand::UpdateSchedule { .. }));
                    if commands.is_empty() {
                        pending.remove(node_id);
                    }
                }
            }
        }

        match ephemeral_program_hash {
            Some(program_hash) => {
                clear_pending_commands(node_id, &self.pending_commands, |cmd| {
                    matches!(cmd, PendingCommand::RunEphemeral { .. })
                })
                .await;
                queue_ephemeral_impl(
                    &self.storage,
                    &self.pending_commands,
                    node_id,
                    &program_hash,
                )
                .await
                .map_err(|e| format!("queue connector desired ephemeral failed: {e}"))?;
            }
            None => {
                clear_pending_commands(node_id, &self.pending_commands, |cmd| {
                    matches!(cmd, PendingCommand::RunEphemeral { .. })
                })
                .await;
            }
        }

        Ok(())
    }
}

async fn clear_pending_commands<F>(
    node_id: &str,
    pending_commands: &Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    mut predicate: F,
) where
    F: FnMut(&PendingCommand) -> bool,
{
    let mut pending = pending_commands.write().await;
    if let Some(commands) = pending.get_mut(node_id) {
        commands.retain(|cmd| !predicate(cmd));
        if commands.is_empty() {
            pending.remove(node_id);
        }
    }
}

#[cfg(unix)]
pub async fn serve_connector(
    service: ConnectorService,
    socket_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::net::UnixListener;

    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    let active_session = Arc::new(Semaphore::new(1));
    info!(socket = %socket_path, "connector server listening on Unix socket");

    loop {
        let (stream, _) = listener.accept().await?;
        let permit = match Arc::clone(&active_session).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!("rejecting second connector session while another is active");
                drop(stream);
                continue;
            }
        };
        let service = service.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = service.handle_connection(stream).await {
                error!(error = %e, "connector session failed");
            }
        });
    }
}

#[cfg(windows)]
pub async fn serve_connector(
    service: ConnectorService,
    pipe_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::net::windows::named_pipe::ServerOptions;

    struct NamedPipeConn(tokio::net::windows::named_pipe::NamedPipeServer);

    impl AsyncRead for NamedPipeConn {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for NamedPipeConn {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    let active_session = Arc::new(Semaphore::new(1));
    let pipe_name = pipe_name.to_owned();
    info!(pipe = %pipe_name, "connector server listening on named pipe");

    loop {
        let server = ServerOptions::new().create(&pipe_name)?;
        server.connect().await?;
        let permit = match Arc::clone(&active_session).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!("rejecting second connector session while another is active");
                drop(server);
                continue;
            }
        };
        let service = service.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = service.handle_connection(NamedPipeConn(server)).await {
                error!(error = %e, "connector session failed");
            }
        });
    }
}

#[cfg(not(any(unix, windows)))]
pub async fn serve_connector(
    _service: ConnectorService,
    _socket_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("sonde-gateway connector server requires Unix (UDS) or Windows (named pipe)".into())
}

fn connector_codec(max_message_size: usize) -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_length(4)
        .big_endian()
        .max_frame_length(max_message_size)
        .new_codec()
}

async fn require_existing_node(storage: &Arc<dyn Storage>, node_id: &str) -> Result<(), String> {
    if node_id.is_empty() {
        return Err("node desired state requires a non-empty entity_id".to_string());
    }
    storage
        .get_node(node_id)
        .await
        .map_err(|e| format!("lookup node `{node_id}` failed: {e}"))?
        .ok_or_else(|| format!("node `{node_id}` not found"))?;
    Ok(())
}

async fn update_node_record<F>(
    storage: &Arc<dyn Storage>,
    node_id: &str,
    mut update: F,
) -> Result<(), String>
where
    F: FnMut(&mut crate::registry::NodeRecord),
{
    let mut node = storage
        .get_node(node_id)
        .await
        .map_err(|e| format!("lookup node `{node_id}` failed: {e}"))?
        .ok_or_else(|| format!("node `{node_id}` not found"))?;
    update(&mut node);
    storage
        .upsert_node(&node)
        .await
        .map_err(|e| format!("update node `{node_id}` failed: {e}"))
}

fn map_entry(key: u64, value: Value) -> (Value, Value) {
    (Value::Integer(key.into()), value)
}

fn opt_bytes_value(value: Option<&[u8]>) -> Value {
    match value {
        Some(bytes) => Value::Bytes(bytes.to_vec()),
        None => Value::Null,
    }
}

fn opt_u32_value(value: Option<u32>) -> Value {
    match value {
        Some(v) => Value::Integer(u64::from(v).into()),
        None => Value::Null,
    }
}

fn opt_text_value(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

fn current_time_ms() -> u64 {
    system_time_to_millis(SystemTime::now()).unwrap_or(0)
}

fn decode_map(bytes: &[u8]) -> Result<Vec<(Value, Value)>, String> {
    let value: Value = ciborium::from_reader(bytes)
        .map_err(|e| format!("failed to decode connector CBOR: {e}"))?;
    value
        .as_map()
        .cloned()
        .ok_or_else(|| "connector payload must be a CBOR map".to_string())
}

fn map_get(map: &[(Value, Value)], key: u64) -> Option<&Value> {
    map.iter().find_map(
        |(k, v)| match k.as_integer().and_then(|i| u64::try_from(i).ok()) {
            Some(found) if found == key => Some(v),
            _ => None,
        },
    )
}

fn required_u64(map: &[(Value, Value)], key: u64, field: &str) -> Result<u64, String> {
    map_get(map, key)
        .ok_or_else(|| format!("missing `{field}`"))
        .and_then(|value| {
            value
                .as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .ok_or_else(|| format!("`{field}` must be uint"))
        })
}

fn required_text(map: &[(Value, Value)], key: u64, field: &str) -> Result<String, String> {
    map_get(map, key)
        .ok_or_else(|| format!("missing `{field}`"))
        .and_then(|value| {
            value
                .as_text()
                .map(|text| text.to_string())
                .ok_or_else(|| format!("`{field}` must be text"))
        })
}

fn required_map(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Vec<(Value, Value)>, String> {
    map_get(map, key)
        .ok_or_else(|| format!("missing `{field}`"))
        .and_then(|value| {
            value
                .as_map()
                .cloned()
                .ok_or_else(|| format!("`{field}` must be a map"))
        })
}

fn optional_bytes_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<Vec<u8>>, String> {
    match map_get(map, key) {
        Some(Value::Bytes(bytes)) => Ok(Some(bytes.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(format!("`{field}` must be bstr or null")),
    }
}

fn optional_u32_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<u32>, String> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| format!("`{field}` must be uint or null")),
    }
}
