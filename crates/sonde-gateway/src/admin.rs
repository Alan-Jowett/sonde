// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

use crate::engine::PendingCommand;
use crate::program::{ProgramLibrary, VerificationProfile};
use crate::registry::NodeRecord;
use crate::session::SessionManager;
use crate::storage::Storage;

pub mod pb {
    tonic::include_proto!("sonde.admin");
}

use pb::gateway_admin_server::GatewayAdmin;
use pb::*;

/// gRPC admin service implementation backed by the gateway's shared state.
pub struct AdminService {
    storage: Arc<dyn Storage>,
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    program_library: ProgramLibrary,
    session_manager: Arc<SessionManager>,
}

impl AdminService {
    pub fn new(
        storage: Arc<dyn Storage>,
        pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        Self {
            storage,
            pending_commands,
            program_library: ProgramLibrary::new(),
            session_manager,
        }
    }
}

fn node_to_info(n: &NodeRecord) -> NodeInfo {
    let last_seen_ms = n.last_seen.and_then(|t| {
        t.duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
    });
    NodeInfo {
        node_id: n.node_id.clone(),
        key_hint: n.key_hint as u32,
        assigned_program_hash: n.assigned_program_hash.clone().unwrap_or_default(),
        current_program_hash: n.current_program_hash.clone().unwrap_or_default(),
        last_battery_mv: n.last_battery_mv,
        last_firmware_abi_version: n.firmware_abi_version,
        last_seen_ms,
        schedule_interval_s: Some(n.schedule_interval_s),
    }
}

#[allow(clippy::result_large_err)]
fn parse_profile(value: i32) -> Result<VerificationProfile, Status> {
    match value {
        1 => Ok(VerificationProfile::Resident),
        2 => Ok(VerificationProfile::Ephemeral),
        _ => Err(Status::invalid_argument(format!(
            "unknown `verification_profile`: {value}; expected RESIDENT (1) or EPHEMERAL (2)"
        ))),
    }
}

fn profile_to_proto(p: &VerificationProfile) -> i32 {
    match p {
        VerificationProfile::Resident => 1,
        VerificationProfile::Ephemeral => 2,
    }
}

fn storage_err(e: crate::storage::StorageError) -> Status {
    match e {
        crate::storage::StorageError::NotFound(_) => Status::not_found(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

#[tonic::async_trait]
impl GatewayAdmin for AdminService {
    async fn list_nodes(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        let nodes = self.storage.list_nodes().await.map_err(storage_err)?;
        let mut nodes: Vec<_> = nodes.iter().map(node_to_info).collect();
        nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(Response::new(ListNodesResponse { nodes }))
    }

    async fn get_node(
        &self,
        request: Request<GetNodeRequest>,
    ) -> Result<Response<NodeInfo>, Status> {
        let node_id = &request.get_ref().node_id;
        let node = self
            .storage
            .get_node(node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{node_id}` not found")))?;
        Ok(Response::new(node_to_info(&node)))
    }

    async fn register_node(
        &self,
        request: Request<RegisterNodeRequest>,
    ) -> Result<Response<RegisterNodeResponse>, Status> {
        let req = request.into_inner();
        if req.psk.len() != 32 {
            return Err(Status::invalid_argument(format!(
                "`psk` must be exactly 32 bytes, got {}",
                req.psk.len()
            )));
        }
        if req.node_id.is_empty() {
            return Err(Status::invalid_argument("`node_id` must not be empty"));
        }
        if req.key_hint > u16::MAX as u32 {
            return Err(Status::invalid_argument(format!(
                "`key_hint` must be <= 65535, got {}",
                req.key_hint
            )));
        }
        let key_hint = req.key_hint as u16;
        // Reject if node already exists (GW-0801: RegisterNode adds a new node).
        if self
            .storage
            .get_node(&req.node_id)
            .await
            .map_err(storage_err)?
            .is_some()
        {
            return Err(Status::already_exists(format!(
                "node `{}` is already registered",
                req.node_id
            )));
        }
        let mut psk = [0u8; 32];
        psk.copy_from_slice(&req.psk);
        let record = NodeRecord::new(req.node_id.clone(), key_hint, psk);
        self.storage
            .upsert_node(&record)
            .await
            .map_err(storage_err)?;
        Ok(Response::new(RegisterNodeResponse {
            node_id: req.node_id,
        }))
    }

    async fn remove_node(
        &self,
        request: Request<RemoveNodeRequest>,
    ) -> Result<Response<Empty>, Status> {
        let node_id = &request.get_ref().node_id;
        self.storage
            .get_node(node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{node_id}` not found")))?;
        self.storage
            .delete_node(node_id)
            .await
            .map_err(storage_err)?;
        Ok(Response::new(Empty {}))
    }

    /// Ingest a CBOR-encoded program image. ELF→CBOR extraction/verification
    /// will be added in a future phase; callers must supply pre-encoded CBOR for now.
    async fn ingest_program(
        &self,
        request: Request<IngestProgramRequest>,
    ) -> Result<Response<IngestProgramResponse>, Status> {
        let req = request.into_inner();
        let profile = parse_profile(req.verification_profile)?;
        let mut record = self
            .program_library
            .ingest_unverified(req.image_data, profile)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        record.abi_version = req.abi_version;
        let resp = IngestProgramResponse {
            program_hash: record.hash.clone(),
            program_size: record.size,
        };
        self.storage
            .store_program(&record)
            .await
            .map_err(storage_err)?;
        Ok(Response::new(resp))
    }

    async fn list_programs(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListProgramsResponse>, Status> {
        let mut programs = self.storage.list_programs().await.map_err(storage_err)?;
        programs.sort_by(|a, b| a.hash.cmp(&b.hash));
        let programs = programs
            .iter()
            .map(|p| ProgramInfo {
                hash: p.hash.clone(),
                size: p.size,
                verification_profile: profile_to_proto(&p.verification_profile),
                abi_version: p.abi_version,
            })
            .collect();
        Ok(Response::new(ListProgramsResponse { programs }))
    }

    async fn assign_program(
        &self,
        request: Request<AssignProgramRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.get_ref();
        let mut node = self
            .storage
            .get_node(&req.node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{}` not found", req.node_id)))?;
        self.storage
            .get_program(&req.program_hash)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found("program not found"))?;
        node.assigned_program_hash = Some(req.program_hash.clone());
        self.storage.upsert_node(&node).await.map_err(storage_err)?;
        Ok(Response::new(Empty {}))
    }

    async fn remove_program(
        &self,
        request: Request<RemoveProgramRequest>,
    ) -> Result<Response<Empty>, Status> {
        let hash = request.into_inner().program_hash;
        self.storage
            .get_program(&hash)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found("program not found"))?;

        // Prevent deletion while any node is still assigned to this program.
        let nodes = self.storage.list_nodes().await.map_err(storage_err)?;
        if nodes
            .iter()
            .any(|node| node.assigned_program_hash.as_deref() == Some(hash.as_slice()))
        {
            return Err(Status::failed_precondition(
                "cannot remove program: still assigned to one or more nodes",
            ));
        }

        // Prevent deletion while pending RunEphemeral commands reference it.
        {
            let guard = self.pending_commands.read().await;
            let has_ref = guard.values().any(|cmds| {
                cmds.iter().any(|cmd| {
                    matches!(cmd, PendingCommand::RunEphemeral { program_hash } if *program_hash == hash)
                })
            });
            if has_ref {
                return Err(Status::failed_precondition(
                    "cannot remove program: referenced by pending RunEphemeral commands",
                ));
            }
        }

        self.storage
            .delete_program(&hash)
            .await
            .map_err(storage_err)?;
        Ok(Response::new(Empty {}))
    }

    async fn set_schedule(
        &self,
        request: Request<SetScheduleRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.get_ref();
        let mut node = self
            .storage
            .get_node(&req.node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{}` not found", req.node_id)))?;

        // Persist the new schedule in the node record
        node.schedule_interval_s = req.interval_s;
        self.storage.upsert_node(&node).await.map_err(storage_err)?;

        // Queue the command for delivery on next WAKE, replacing any
        // previously-queued schedule update so the node always gets the latest.
        let mut guard = self.pending_commands.write().await;
        let commands = guard.entry(req.node_id.clone()).or_default();
        commands.retain(|cmd| !matches!(cmd, PendingCommand::UpdateSchedule { .. }));
        commands.push(PendingCommand::UpdateSchedule {
            interval_s: req.interval_s,
        });
        Ok(Response::new(Empty {}))
    }

    async fn queue_reboot(
        &self,
        request: Request<QueueRebootRequest>,
    ) -> Result<Response<Empty>, Status> {
        let node_id = &request.get_ref().node_id;
        self.storage
            .get_node(node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{node_id}` not found")))?;
        self.pending_commands
            .write()
            .await
            .entry(node_id.clone())
            .or_default()
            .push(PendingCommand::Reboot);
        Ok(Response::new(Empty {}))
    }

    async fn queue_ephemeral(
        &self,
        request: Request<QueueEphemeralRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.get_ref();
        self.storage
            .get_node(&req.node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{}` not found", req.node_id)))?;
        let program = self
            .storage
            .get_program(&req.program_hash)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found("program not found"))?;
        if program.verification_profile != VerificationProfile::Ephemeral {
            return Err(Status::failed_precondition(
                "program must have ephemeral verification profile for `QueueEphemeral`",
            ));
        }
        self.pending_commands
            .write()
            .await
            .entry(req.node_id.clone())
            .or_default()
            .push(PendingCommand::RunEphemeral {
                program_hash: req.program_hash.clone(),
            });
        Ok(Response::new(Empty {}))
    }

    async fn get_node_status(
        &self,
        request: Request<GetNodeStatusRequest>,
    ) -> Result<Response<NodeStatus>, Status> {
        let node_id = &request.get_ref().node_id;
        let node = self
            .storage
            .get_node(node_id)
            .await
            .map_err(storage_err)?
            .ok_or_else(|| Status::not_found(format!("node `{node_id}` not found")))?;
        let has_active_session = self.session_manager.get_session(node_id).await.is_some();
        let last_seen_ms = node.last_seen.and_then(|t| {
            t.duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as u64)
        });
        Ok(Response::new(NodeStatus {
            node_id: node.node_id,
            current_program_hash: node.current_program_hash.unwrap_or_default(),
            battery_mv: node.last_battery_mv,
            firmware_abi_version: node.firmware_abi_version,
            last_seen_ms,
            has_active_session,
        }))
    }

    /// Export gateway state (nodes + programs).
    ///
    /// Disabled until admin authentication/authorization is implemented.
    /// PSK material is now encrypted at rest (GW-0601a); however, exporting
    /// requires operator authorization to prevent unauthorized bulk key extraction.
    /// Handler routing configuration export is also deferred to Phase 2C-iii.
    async fn export_state(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ExportStateResponse>, Status> {
        Err(Status::unimplemented(
            "`export_state` is disabled until admin authentication/authorization is implemented",
        ))
    }

    /// Import gateway state (nodes + programs).
    ///
    /// Disabled until admin authentication/authorization is implemented.
    /// PSK material is now encrypted at rest (GW-0601a); however, importing
    /// requires operator authorization to prevent unauthorized key injection.
    /// Handler routing configuration import is also deferred to Phase 2C-iii.
    async fn import_state(
        &self,
        _request: Request<ImportStateRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented(
            "`import_state` is disabled until admin authentication/authorization is implemented",
        ))
    }

    /// Get modem status (channel, counters, uptime).
    ///
    /// Requires a modem transport to be configured. The gateway forwards
    /// a `GET_STATUS` command to the modem over the serial protocol.
    async fn get_modem_status(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ModemStatus>, Status> {
        Err(Status::unimplemented(
            "`get_modem_status` requires a modem transport — not yet wired",
        ))
    }

    /// Set the modem's ESP-NOW radio channel.
    async fn set_modem_channel(
        &self,
        _request: Request<SetModemChannelRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented(
            "`set_modem_channel` requires a modem transport — not yet wired",
        ))
    }

    /// Scan all WiFi channels and report per-channel AP activity.
    async fn scan_modem_channels(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ScanModemChannelsResponse>, Status> {
        Err(Status::unimplemented(
            "`scan_modem_channels` requires a modem transport — not yet wired",
        ))
    }
}
