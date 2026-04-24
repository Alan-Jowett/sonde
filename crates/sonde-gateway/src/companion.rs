// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use futures::Stream;
use tokio::sync::{broadcast, RwLock};
use tonic::{Request, Response, Status};

use crate::admin::{
    assign_program_impl, get_node_impl, get_node_status_impl, list_nodes_impl,
    queue_ephemeral_impl, queue_reboot_impl, set_schedule_impl, NodeStatusSnapshot,
};
use crate::engine::PendingCommand;
use crate::registry::NodeRecord;
use crate::session::SessionManager;
use crate::storage::Storage;

pub mod pb {
    tonic::include_proto!("sonde.companion");
}

use pb::companion_event::Event;
use pb::gateway_companion_server::GatewayCompanion;
use pb::*;

pub const DEFAULT_COMPANION_EVENT_BUFFER: usize = 64;

#[derive(Clone)]
pub struct CompanionEventHub {
    tx: broadcast::Sender<CompanionEvent>,
}

impl Default for CompanionEventHub {
    fn default() -> Self {
        Self::new(DEFAULT_COMPANION_EVENT_BUFFER)
    }
}

impl CompanionEventHub {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CompanionEvent> {
        self.tx.subscribe()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_node_checkin(
        &self,
        node_id: String,
        current_program_hash: Vec<u8>,
        assigned_program_hash: Option<Vec<u8>>,
        battery_mv: u32,
        firmware_abi_version: u32,
        firmware_version: String,
        timestamp_ms: u64,
    ) {
        let _ = self.tx.send(CompanionEvent {
            event: Some(Event::NodeCheckin(CompanionNodeCheckIn {
                node_id,
                current_program_hash,
                assigned_program_hash,
                battery_mv,
                firmware_abi_version,
                firmware_version,
                timestamp_ms,
            })),
        });
    }

    pub fn emit_node_payload(
        &self,
        node_id: String,
        program_hash: Vec<u8>,
        payload: Vec<u8>,
        timestamp_ms: u64,
        payload_origin: CompanionPayloadOrigin,
    ) {
        let _ = self.tx.send(CompanionEvent {
            event: Some(Event::NodePayload(CompanionNodePayload {
                node_id,
                program_hash,
                payload,
                timestamp_ms,
                payload_origin: payload_origin as i32,
            })),
        });
    }
}

pub struct CompanionService {
    storage: Arc<dyn Storage>,
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    session_manager: Arc<SessionManager>,
    event_hub: Arc<CompanionEventHub>,
}

impl CompanionService {
    pub fn new(
        storage: Arc<dyn Storage>,
        pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
        session_manager: Arc<SessionManager>,
        event_hub: Arc<CompanionEventHub>,
    ) -> Self {
        Self {
            storage,
            pending_commands,
            session_manager,
            event_hub,
        }
    }
}

fn system_time_to_millis(t: SystemTime) -> Option<u64> {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d: std::time::Duration| d.as_millis() as u64)
}

fn node_to_info(node: &NodeRecord, last_seen: Option<SystemTime>) -> CompanionNodeInfo {
    let last_seen_ms = last_seen.and_then(system_time_to_millis);
    CompanionNodeInfo {
        node_id: node.node_id.clone(),
        key_hint: node.key_hint as u32,
        assigned_program_hash: node.assigned_program_hash.clone(),
        current_program_hash: node.current_program_hash.clone(),
        last_battery_mv: node.last_battery_mv,
        last_firmware_abi_version: node.firmware_abi_version,
        last_seen_ms,
        schedule_interval_s: Some(node.schedule_interval_s),
    }
}

fn node_status_to_proto(status: NodeStatusSnapshot) -> CompanionNodeStatus {
    CompanionNodeStatus {
        node_id: status.node_id,
        current_program_hash: status.current_program_hash,
        battery_mv: status.battery_mv,
        firmware_abi_version: status.firmware_abi_version,
        last_seen_ms: status.last_seen_ms,
        has_active_session: status.has_active_session,
    }
}

#[tonic::async_trait]
impl GatewayCompanion for CompanionService {
    type StreamEventsStream =
        Pin<Box<dyn Stream<Item = Result<CompanionEvent, Status>> + Send + 'static>>;

    async fn stream_events(
        &self,
        _request: Request<CompanionStreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        enum StreamState {
            Active(broadcast::Receiver<CompanionEvent>),
            Done,
        }

        let stream = futures::stream::unfold(
            StreamState::Active(self.event_hub.subscribe()),
            |state| async move {
                match state {
                    StreamState::Active(mut event_rx) => match event_rx.recv().await {
                        Ok(event) => Some((Ok(event), StreamState::Active(event_rx))),
                        Err(broadcast::error::RecvError::Lagged(_)) => Some((
                            Err(Status::resource_exhausted(
                                "companion subscriber fell behind the live event stream",
                            )),
                            StreamState::Done,
                        )),
                        Err(broadcast::error::RecvError::Closed) => None,
                    },
                    StreamState::Done => None,
                }
            },
        );

        Ok(Response::new(Box::pin(stream)))
    }

    async fn list_nodes(
        &self,
        _request: Request<CompanionListNodesRequest>,
    ) -> Result<Response<CompanionListNodesResponse>, Status> {
        let nodes_records = list_nodes_impl(&self.storage).await?;
        let last_seen = self.session_manager.snapshot_last_seen().await;
        let mut nodes: Vec<_> = nodes_records
            .iter()
            .map(|node| node_to_info(node, last_seen.get(&node.node_id).copied()))
            .collect();
        nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(Response::new(CompanionListNodesResponse { nodes }))
    }

    async fn get_node(
        &self,
        request: Request<CompanionGetNodeRequest>,
    ) -> Result<Response<CompanionNodeInfo>, Status> {
        let node_id = &request.get_ref().node_id;
        let node = get_node_impl(&self.storage, node_id).await?;
        let last_seen = self.session_manager.get_last_seen(node_id).await;
        Ok(Response::new(node_to_info(&node, last_seen)))
    }

    async fn assign_program(
        &self,
        request: Request<CompanionAssignProgramRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        let req = request.into_inner();
        assign_program_impl(&self.storage, &req.node_id, &req.program_hash).await?;
        Ok(Response::new(CompanionEmpty {}))
    }

    async fn set_schedule(
        &self,
        request: Request<CompanionSetScheduleRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        let req = request.into_inner();
        set_schedule_impl(
            &self.storage,
            &self.pending_commands,
            &req.node_id,
            req.interval_s,
        )
        .await?;
        Ok(Response::new(CompanionEmpty {}))
    }

    async fn queue_reboot(
        &self,
        request: Request<CompanionQueueRebootRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        let req = request.into_inner();
        queue_reboot_impl(&self.storage, &self.pending_commands, &req.node_id).await?;
        Ok(Response::new(CompanionEmpty {}))
    }

    async fn queue_ephemeral(
        &self,
        request: Request<CompanionQueueEphemeralRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        let req = request.into_inner();
        queue_ephemeral_impl(
            &self.storage,
            &self.pending_commands,
            &req.node_id,
            &req.program_hash,
        )
        .await?;
        Ok(Response::new(CompanionEmpty {}))
    }

    async fn get_node_status(
        &self,
        request: Request<CompanionGetNodeStatusRequest>,
    ) -> Result<Response<CompanionNodeStatus>, Status> {
        let status = get_node_status_impl(
            &self.storage,
            &self.session_manager,
            &request.get_ref().node_id,
        )
        .await?;
        Ok(Response::new(node_status_to_proto(status)))
    }
}

#[cfg(unix)]
pub async fn serve_companion(
    service: CompanionService,
    socket_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;
    use tracing::info;

    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    info!(socket = %socket_path, "gRPC companion server listening on Unix socket");

    tonic::transport::Server::builder()
        .add_service(pb::gateway_companion_server::GatewayCompanionServer::new(
            service,
        ))
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await?;
    Ok(())
}

#[cfg(windows)]
pub async fn serve_companion(
    service: CompanionService,
    pipe_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::ServerOptions;
    use tonic::transport::server::Connected;
    use tracing::info;

    struct NamedPipeConn(tokio::net::windows::named_pipe::NamedPipeServer);

    impl Connected for NamedPipeConn {
        type ConnectInfo = ();
        fn connect_info(&self) -> Self::ConnectInfo {}
    }

    impl AsyncRead for NamedPipeConn {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
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

    let pipe_name = pipe_name.to_owned();
    info!(pipe = %pipe_name, "gRPC companion server listening on named pipe");

    let incoming = futures::stream::unfold((true, pipe_name), |(first, name)| async move {
        let server = match ServerOptions::new()
            .first_pipe_instance(first)
            .create(&name)
        {
            Ok(s) => s,
            Err(e) => return Some((Err::<NamedPipeConn, _>(e), (false, name))),
        };
        match server.connect().await {
            Ok(()) => Some((Ok(NamedPipeConn(server)), (false, name))),
            Err(e) => Some((Err(e), (false, name))),
        }
    });

    tonic::transport::Server::builder()
        .add_service(pb::gateway_companion_server::GatewayCompanionServer::new(
            service,
        ))
        .serve_with_incoming(incoming)
        .await?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub async fn serve_companion(
    _service: CompanionService,
    _socket: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("sonde-gateway companion gRPC requires Unix (UDS) or Windows (named pipe)".into())
}
