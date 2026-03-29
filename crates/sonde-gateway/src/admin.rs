// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use tracing::warn;
use zeroize::Zeroizing;

use crate::ble_pairing::BlePairingController;
use crate::engine::PendingCommand;
use crate::handler::HandlerConfig;
use crate::handler::ProgramMatcher;
use crate::modem::UsbEspNowTransport;
use crate::program::{ProgramLibrary, VerificationProfile};
use crate::registry::NodeRecord;
use crate::session::SessionManager;
use crate::storage::{HandlerRecord, Storage};

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
    ble_controller: Option<Arc<BlePairingController>>,
    transport: Option<Arc<UsbEspNowTransport>>,
    handler_configs: RwLock<Vec<HandlerConfig>>,
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
            ble_controller: None,
            transport: None,
            handler_configs: RwLock::new(Vec::new()),
        }
    }

    /// Set the BLE pairing controller and transport for admin BLE RPCs.
    pub fn with_ble(
        mut self,
        controller: Arc<BlePairingController>,
        transport: Arc<UsbEspNowTransport>,
    ) -> Self {
        self.ble_controller = Some(controller);
        self.transport = Some(transport);
        self
    }

    /// Set the handler routing configurations for full state export.
    pub fn with_handler_configs(mut self, configs: Vec<HandlerConfig>) -> Self {
        self.handler_configs = RwLock::new(configs);
        self
    }

    /// Reload handler configs from storage into the in-memory cache.
    async fn reload_handler_configs(&self) {
        match self.storage.list_handlers().await {
            Ok(records) => {
                let configs: Vec<HandlerConfig> =
                    records.into_iter().map(handler_record_to_config).collect();
                *self.handler_configs.write().await = configs;
            }
            Err(e) => {
                warn!("failed to reload handler configs: {e}");
            }
        }
    }
}

/// Convert a [`HandlerRecord`] into a [`HandlerConfig`].
fn handler_record_to_config(r: HandlerRecord) -> HandlerConfig {
    let matcher = if r.program_hash == "*" {
        ProgramMatcher::Any
    } else {
        let bytes = (0..r.program_hash.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&r.program_hash[i..i + 2], 16).ok())
            .collect();
        ProgramMatcher::Hash(bytes)
    };
    HandlerConfig {
        matchers: vec![matcher],
        command: r.command,
        args: r.args,
        reply_timeout: r
            .reply_timeout_ms
            .filter(|&ms| ms > 0)
            .map(std::time::Duration::from_millis),
        working_dir: r.working_dir,
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

/// Map `BundleError` to gRPC status: encode/RNG failures → INTERNAL (server
/// error), everything else (bad input) → INVALID_ARGUMENT.
fn bundle_err(e: crate::state_bundle::BundleError) -> Status {
    match e {
        crate::state_bundle::BundleError::Encode(_) => Status::internal(e.to_string()),
        crate::state_bundle::BundleError::Rng => Status::internal(e.to_string()),
        _ => Status::invalid_argument(e.to_string()),
    }
}

/// Parse a program hash string: `"*"` → `ProgramMatcher::Any`, otherwise
/// validate as 64-char lowercase hex → `ProgramMatcher::Hash`.
fn validate_program_hash(s: &str) -> Result<ProgramMatcher, Status> {
    if s == "*" {
        return Ok(ProgramMatcher::Any);
    }
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Status::invalid_argument(
            "`program_hash` must be \"*\" or a 64-char hex string",
        ));
    }
    let mut bytes = Vec::with_capacity(32);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|e| Status::invalid_argument(format!("invalid hex in `program_hash`: {e}")))?;
        bytes.push(byte);
    }
    Ok(ProgramMatcher::Hash(bytes))
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
        // Verify the node exists before attempting deletion.
        // We load the full record to check existence (the Storage trait has no
        // lightweight exists query), so zeroize the PSK copy immediately.
        {
            use zeroize::Zeroize;
            let mut node = self
                .storage
                .get_node(node_id)
                .await
                .map_err(storage_err)?
                .ok_or_else(|| Status::not_found(format!("node `{node_id}` not found")))?;
            node.psk.zeroize();
        }

        // TODO: GW-0705 / T-0706 require a node-side factory reset (erase
        // PSK, persistent maps, and resident program) before removing the
        // key from the registry. This needs a protocol-level reset command
        // sent while the node is still authenticated — not yet implemented.
        self.storage
            .delete_node(node_id)
            .await
            .map_err(storage_err)?;

        // Invalidate any active in-memory session so the node cannot
        // continue communicating after removal.
        self.session_manager.remove_session(node_id).await;

        Ok(Response::new(Empty {}))
    }

    /// Trigger a factory reset for a node (GW-0705).
    ///
    /// Removes the node from the registry and clears any pending commands.
    ///
    /// **Note:** Full factory reset requires a protocol-level command sent to
    /// the node while it is still authenticated (erasing PSK, maps, and
    /// resident program on the node side). That protocol extension is not yet
    /// implemented; this RPC currently performs the gateway-side cleanup only.
    async fn factory_reset(
        &self,
        request: Request<FactoryResetRequest>,
    ) -> Result<Response<Empty>, Status> {
        let node_id = &request.get_ref().node_id;
        // Verify the node exists before attempting deletion.
        // We load the full record to check existence (the Storage trait has no
        // lightweight exists query), so zeroize the PSK copy immediately.
        {
            use zeroize::Zeroize;
            let mut node = self
                .storage
                .get_node(node_id)
                .await
                .map_err(storage_err)?
                .ok_or_else(|| Status::not_found(format!("node `{node_id}` not found")))?;
            node.psk.zeroize();
        }

        // TODO: GW-0705 / T-0706 — send a protocol-level factory reset
        // command to the node during its next WAKE cycle before removing
        // the registry entry. Requires a new command type in sonde-protocol.
        self.storage
            .delete_node(node_id)
            .await
            .map_err(storage_err)?;

        // Invalidate any active in-memory session so the node is
        // immediately treated as unknown (GW-0705 AC1).
        self.session_manager.remove_session(node_id).await;

        // Clear any pending commands for the removed node.
        self.pending_commands.write().await.remove(node_id);

        Ok(Response::new(Empty {}))
    }

    /// Ingest a program image (GW-0400).
    ///
    /// Accepts either an ELF binary (auto-detected via the `\x7fELF` magic)
    /// or a pre-encoded CBOR program image. ELF binaries are parsed,
    /// verified with Prevail, and converted to CBOR automatically.
    async fn ingest_program(
        &self,
        request: Request<IngestProgramRequest>,
    ) -> Result<Response<IngestProgramResponse>, Status> {
        let req = request.into_inner();
        let profile = parse_profile(req.verification_profile)?;

        let is_elf = req.image_data.len() >= 4 && &req.image_data[..4] == b"\x7fELF";

        let mut record = if is_elf {
            self.program_library
                .ingest_elf(&req.image_data, profile)
                .map_err(|e| Status::invalid_argument(e.to_string()))?
        } else if cfg!(debug_assertions) {
            // In debug builds, allow ingestion of pre-encoded CBOR program images
            // without Prevail verification. This path is disabled in release builds
            // to enforce GW-0401 (all stored programs must be verified).
            self.program_library
                .ingest_unverified(req.image_data, profile)
                .map_err(|e| Status::invalid_argument(e.to_string()))?
        } else {
            // In release/production builds, reject non-ELF inputs to avoid
            // storing unverified programs via the admin API.
            return Err(Status::invalid_argument(
                "non-ELF program images are not accepted in this build; \
                submit an ELF binary so the gateway can verify it with Prevail",
            ));
        };

        record.abi_version = req.abi_version;
        record.source_filename = req.source_filename;
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
                source_filename: p.source_filename.clone(),
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

    /// Export full gateway state (nodes, programs, identity, phone PSKs,
    /// and handler configs) as an AES-256-GCM-encrypted CBOR bundle.
    ///
    /// The passphrase is used to derive the encryption key via
    /// PBKDF2-HMAC-SHA256.
    ///
    /// **Security note:** This RPC returns PSK material (encrypted with the
    /// operator passphrase).  The admin gRPC endpoint MUST be bound to a
    /// local-only transport (Unix socket / named pipe) or protected by
    /// authentication before deployment.  See GW-0800 and security.md §2.3.
    async fn export_state(
        &self,
        request: Request<ExportStateRequest>,
    ) -> Result<Response<ExportStateResponse>, Status> {
        let req = request.into_inner();
        // Validate passphrase early to avoid loading sensitive material
        // for a request that will fail anyway.
        if req.passphrase.is_empty() {
            return Err(Status::invalid_argument("passphrase must not be empty"));
        }
        let nodes = self.storage.list_nodes().await.map_err(storage_err)?;
        let programs = self.storage.list_programs().await.map_err(storage_err)?;
        let identity = self
            .storage
            .load_gateway_identity()
            .await
            .map_err(storage_err)?;
        let phone_psks = self.storage.list_phone_psks().await.map_err(storage_err)?;
        let handler_configs = self.handler_configs.read().await.clone();
        let passphrase = Zeroizing::new(req.passphrase);
        // Offload CPU-bound PBKDF2 + AES-GCM encryption to a blocking thread
        // so the Tokio runtime is not stalled.
        let data = tokio::task::spawn_blocking(move || {
            crate::state_bundle::encrypt_state_full(
                &nodes,
                &programs,
                identity.as_ref(),
                &phone_psks,
                &handler_configs,
                &passphrase,
            )
        })
        .await
        .map_err(|e| Status::internal(format!("encrypt task failed: {e}")))?
        .map_err(bundle_err)?;
        Ok(Response::new(ExportStateResponse { data }))
    }

    /// Import gateway state from a bundle previously produced by `export_state`.
    ///
    /// Replaces the current node registry, program library, and phone PSK
    /// registrations with the bundle contents. Updates the stored handler
    /// configuration ([`AdminService::handler_configs`], used for subsequent
    /// `export_state` calls) to match the bundle. This does **not** reconfigure
    /// the live handler router used by the running gateway engine; handler
    /// routing remains whatever was set at initialization time. Restores the
    /// gateway identity if present in the bundle; otherwise, the existing
    /// gateway identity is left unchanged. Rejects the request with
    /// `FAILED_PRECONDITION` if any node sessions are active (the gateway
    /// should be quiescent before import). Pending commands are cleared after
    /// a successful import to prevent stale commands from being delivered to
    /// nodes whose records were replaced.
    ///
    /// **Security note:** see [`export_state`] — this RPC accepts key material
    /// and should only be exposed on a local-only or authenticated transport.
    async fn import_state(
        &self,
        request: Request<ImportStateRequest>,
    ) -> Result<Response<Empty>, Status> {
        // Acquire the import lock to prevent new sessions from being
        // created between the active_count check and replace_state.
        let _import_guard = self.session_manager.acquire_import_lock().await;

        // Reap expired sessions before checking count so stale sessions
        // don't block imports indefinitely.
        self.session_manager.reap_expired().await;

        // Reject import while sessions are active to avoid mixed in-memory
        // and on-disk state.
        let active = self.session_manager.active_count().await;
        if active > 0 {
            return Err(Status::failed_precondition(format!(
                "cannot import state while {active} session(s) are active; \
                 wait for sessions to expire or restart the gateway"
            )));
        }

        let req = request.into_inner();
        let data = req.data;
        let passphrase = Zeroizing::new(req.passphrase);
        // Offload CPU-bound PBKDF2 + AES-GCM decryption to a blocking thread.
        let (nodes, programs, identity, phone_psks, handler_cfgs) =
            tokio::task::spawn_blocking(move || {
                crate::state_bundle::decrypt_state_full(&data, &passphrase)
            })
            .await
            .map_err(|e| Status::internal(format!("decrypt task failed: {e}")))?
            .map_err(bundle_err)?;

        // Replace all nodes and programs with the bundle contents.
        // SqliteStorage performs this in a single transaction; other backends
        // use the default non-atomic delete-then-insert fallback.
        self.storage
            .replace_state(&nodes, &programs)
            .await
            .map_err(storage_err)?;

        // Restore gateway identity if present in the bundle.
        if let Some(ref id) = identity {
            self.storage
                .store_gateway_identity(id)
                .await
                .map_err(storage_err)?;
        }

        // Replace phone PSK registrations atomically.
        // SqliteStorage performs this in a single transaction; other backends
        // use the default non-atomic delete-then-insert fallback.
        self.storage
            .replace_phone_psks(&phone_psks)
            .await
            .map_err(storage_err)?;

        // Restore handler routing configuration.
        // Convert HandlerConfig → HandlerRecord for storage, then update
        // in-memory cache.
        if !handler_cfgs.is_empty() {
            let records: Vec<HandlerRecord> = handler_cfgs
                .iter()
                .map(|cfg| {
                    use std::fmt::Write;
                    let program_hash = cfg
                        .matchers
                        .first()
                        .map(|m| match m {
                            ProgramMatcher::Any => "*".to_string(),
                            ProgramMatcher::Hash(bytes) => {
                                let mut s = String::with_capacity(bytes.len() * 2);
                                for b in bytes {
                                    let _ = write!(s, "{b:02x}");
                                }
                                s
                            }
                        })
                        .unwrap_or_default();
                    HandlerRecord {
                        program_hash,
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        working_dir: cfg.working_dir.clone(),
                        reply_timeout_ms: cfg.reply_timeout.map(|d| d.as_millis() as u64),
                    }
                })
                .collect();
            self.storage
                .replace_handlers(&records)
                .await
                .map_err(storage_err)?;
        }
        *self.handler_configs.write().await = handler_cfgs;

        // Clear any pending commands queued for the old node set.
        self.pending_commands.write().await.clear();

        Ok(Response::new(Empty {}))
    }

    /// Add a handler configuration (GW-1402).
    async fn add_handler(
        &self,
        request: Request<AddHandlerRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        if req.command.is_empty() {
            return Err(Status::invalid_argument("`command` must not be empty"));
        }
        // Validate program_hash format (also normalises to lowercase).
        let _ = validate_program_hash(&req.program_hash)?;

        let program_hash = if req.program_hash == "*" {
            "*".to_string()
        } else {
            req.program_hash.to_ascii_lowercase()
        };

        let working_dir = if req.working_dir.is_empty() {
            None
        } else {
            Some(req.working_dir)
        };

        let record = HandlerRecord {
            program_hash,
            command: req.command,
            args: req.args,
            working_dir,
            reply_timeout_ms: req.reply_timeout_ms.filter(|&ms| ms > 0),
        };

        let inserted = self
            .storage
            .add_handler(&record)
            .await
            .map_err(storage_err)?;
        if !inserted {
            return Err(Status::already_exists(format!(
                "handler for `{}` already exists",
                record.program_hash
            )));
        }

        // Update in-memory cache.
        self.reload_handler_configs().await;
        Ok(Response::new(Empty {}))
    }

    /// Remove a handler configuration by program hash (GW-1402).
    async fn remove_handler(
        &self,
        request: Request<RemoveHandlerRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let _ = validate_program_hash(&req.program_hash)?;
        let program_hash = if req.program_hash == "*" {
            "*".to_string()
        } else {
            req.program_hash.to_ascii_lowercase()
        };

        let removed = self
            .storage
            .remove_handler(&program_hash)
            .await
            .map_err(storage_err)?;
        if !removed {
            return Err(Status::not_found(format!(
                "no handler found for `{}`",
                program_hash
            )));
        }

        // Update in-memory cache.
        self.reload_handler_configs().await;
        Ok(Response::new(Empty {}))
    }

    /// List all handler configurations (GW-1402).
    async fn list_handlers(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListHandlersResponse>, Status> {
        let records = self.storage.list_handlers().await.map_err(storage_err)?;
        let handlers = records
            .iter()
            .map(|r| HandlerInfo {
                program_hash: r.program_hash.clone(),
                command: r.command.clone(),
                args: r.args.clone(),
                working_dir: r.working_dir.clone().unwrap_or_default(),
                reply_timeout_ms: r.reply_timeout_ms,
            })
            .collect();
        Ok(Response::new(ListHandlersResponse { handlers }))
    }

    /// Get modem status (channel, counters, uptime).
    ///
    /// Requires a modem transport to be configured. The gateway forwards
    /// a `GET_STATUS` command to the modem over the serial protocol.
    async fn get_modem_status(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ModemStatus>, Status> {
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("no modem transport configured"))?;

        let status = transport
            .poll_status()
            .await
            .map_err(|e| Status::internal(format!("modem status poll failed: {e}")))?;

        Ok(Response::new(ModemStatus {
            channel: status.channel as u32,
            tx_count: status.tx_count,
            rx_count: status.rx_count,
            tx_fail_count: status.tx_fail_count,
            uptime_s: status.uptime_s,
        }))
    }

    /// Set the modem's ESP-NOW radio channel.
    ///
    /// Persists the new channel in the database (GW-0808) after a successful
    /// modem channel change so that reconnects and BLE pairing use the
    /// updated value. If persistence fails, the modem is rolled back to the
    /// previous channel to avoid inconsistency.
    async fn set_modem_channel(
        &self,
        request: Request<SetModemChannelRequest>,
    ) -> Result<Response<Empty>, Status> {
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("no modem transport configured"))?;

        let channel = request.into_inner().channel;
        if !(1..=14).contains(&channel) {
            return Err(Status::invalid_argument("channel must be between 1 and 14"));
        }

        // Read the current persisted channel before changing the modem so we
        // can roll back if the DB write fails.
        let previous_channel = match self.storage.get_config("espnow_channel").await {
            Ok(val) => val,
            Err(e) => {
                return Err(Status::internal(format!(
                    "failed to read current modem channel from storage: {e}"
                )));
            }
        };

        transport
            .change_channel(channel as u8)
            .await
            .map_err(|e| Status::internal(format!("set modem channel failed: {e}")))?;

        // GW-0808: persist the new channel so reconnects and BLE pairing use it.
        if let Err(e) = self
            .storage
            .set_config("espnow_channel", &channel.to_string())
            .await
        {
            // Roll back the modem to the previous channel to avoid
            // inconsistency between modem and database.
            if let Some(prev) = &previous_channel {
                if let Ok(prev_ch) = prev.parse::<u8>() {
                    let _ = transport.change_channel(prev_ch).await;
                }
            }
            return Err(Status::internal(format!("persist channel failed: {e}")));
        }

        Ok(Response::new(Empty {}))
    }

    /// Scan all WiFi channels and report per-channel AP activity.
    async fn scan_modem_channels(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ScanModemChannelsResponse>, Status> {
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("no modem transport configured"))?;

        let result = transport
            .scan_channels()
            .await
            .map_err(|e| Status::internal(format!("modem channel scan failed: {e}")))?;

        Ok(Response::new(ScanModemChannelsResponse {
            entries: result
                .entries
                .into_iter()
                .map(|e| ChannelSurveyEntry {
                    channel: e.channel as u32,
                    ap_count: e.ap_count as u32,
                    strongest_rssi: e.strongest_rssi as i32,
                })
                .collect(),
        }))
    }

    // -- BLE phone pairing (GW-1222) ----------------------------------------

    type OpenBlePairingStream =
        tokio_stream::wrappers::ReceiverStream<Result<BlePairingEvent, Status>>;

    /// Open a BLE phone registration window.
    ///
    /// Returns a server-streaming response that pushes BLE pairing events
    /// (passkey requests, phone connections, registrations) to the CLI.
    /// The stream ends when the window closes (auto-timeout or explicit).
    async fn open_ble_pairing(
        &self,
        request: Request<OpenBlePairingRequest>,
    ) -> Result<Response<Self::OpenBlePairingStream>, Status> {
        let controller = self.ble_controller.as_ref().ok_or_else(|| {
            Status::unavailable("BLE pairing not configured (no modem transport)")
        })?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("modem transport not configured"))?;

        let duration_s = request.into_inner().duration_s;
        let duration_s = if duration_s == 0 { 120 } else { duration_s };

        // Enable BLE advertising first — if this fails, don't open the window.
        transport
            .send_ble_enable()
            .await
            .map_err(|e| Status::internal(format!("failed to enable BLE: {e}")))?;

        // Open the registration window only after BLE is enabled.
        controller.open_window(duration_s).await;

        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let _ = tx
            .send(Ok(BlePairingEvent {
                event: Some(ble_pairing_event::Event::WindowOpened(
                    BlePairingWindowOpened { duration_s },
                )),
            }))
            .await;

        // Subscribe to BLE events broadcast from the BLE loop.
        let mut event_rx = controller.subscribe_events();

        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();
        let close_controller = Arc::clone(controller);
        let close_transport = Arc::clone(transport);

        controller.set_timeout_cancel(cancel.clone()).await;

        // Spawn a task that forwards BLE events to the stream, handles
        // timeout, cancellation, and client disconnect.
        tokio::spawn(async move {
            use crate::ble_pairing::BlePairingEventKind;

            let timeout = tokio::time::sleep(std::time::Duration::from_secs(duration_s as u64));
            tokio::pin!(timeout);

            loop {
                tokio::select! {
                    _ = &mut timeout => {
                        close_controller.close_window().await;
                        let _ = close_transport.send_ble_disable().await;
                        let _ = tx.send(Ok(BlePairingEvent {
                            event: Some(ble_pairing_event::Event::WindowClosed(
                                BlePairingWindowClosed {},
                            )),
                        })).await;
                        break;
                    }
                    _ = cancel_clone.cancelled() => {
                        // Explicit close — send WindowClosed then exit.
                        let _ = tx.send(Ok(BlePairingEvent {
                            event: Some(ble_pairing_event::Event::WindowClosed(
                                BlePairingWindowClosed {},
                            )),
                        })).await;
                        break;
                    }
                    _ = tx.closed() => {
                        // Client disconnected — close window and disable BLE.
                        close_controller.close_window().await;
                        let _ = close_transport.send_ble_disable().await;
                        break;
                    }
                    result = event_rx.recv() => {
                        match result {
                            Ok(evt) => {
                                let proto_event = match evt {
                                    BlePairingEventKind::PhoneConnected { peer_addr, mtu } => {
                                        ble_pairing_event::Event::PhoneConnected(
                                            BlePairingPhoneConnected {
                                                peer_addr: peer_addr.to_vec(),
                                                mtu: mtu as u32,
                                            },
                                        )
                                    }
                                    BlePairingEventKind::PhoneDisconnected { peer_addr } => {
                                        ble_pairing_event::Event::PhoneDisconnected(
                                            BlePairingPhoneDisconnected {
                                                peer_addr: peer_addr.to_vec(),
                                            },
                                        )
                                    }
                                    BlePairingEventKind::PasskeyRequest { passkey } => {
                                        ble_pairing_event::Event::Passkey(
                                            BlePairingPasskey { passkey },
                                        )
                                    }
                                    BlePairingEventKind::PhoneRegistered {
                                        label,
                                        phone_key_hint,
                                    } => {
                                        ble_pairing_event::Event::PhoneRegistered(
                                            BlePairingPhoneRegistered {
                                                label,
                                                phone_key_hint: phone_key_hint as u32,
                                            },
                                        )
                                    }
                                };
                                if tx.send(Ok(BlePairingEvent {
                                    event: Some(proto_event),
                                })).await.is_err() {
                                    break; // Client disconnected
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                // Missed some events due to slow consumer — keep going.
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                // Broadcast channel closed — exit loop.
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }

    /// Close the BLE phone registration window.
    async fn close_ble_pairing(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        let controller = self
            .ble_controller
            .as_ref()
            .ok_or_else(|| Status::unavailable("BLE pairing not configured"))?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Status::unavailable("modem transport not configured"))?;

        // Cancel the timeout task from OpenBlePairing (if running).
        controller.cancel_timeout().await;
        controller.close_window().await;
        transport
            .send_ble_disable()
            .await
            .map_err(|e| Status::internal(format!("failed to disable BLE: {e}")))?;

        Ok(Response::new(Empty {}))
    }

    /// Confirm or reject a BLE Numeric Comparison passkey.
    async fn confirm_ble_pairing(
        &self,
        request: Request<ConfirmBlePairingRequest>,
    ) -> Result<Response<Empty>, Status> {
        let controller = self
            .ble_controller
            .as_ref()
            .ok_or_else(|| Status::unavailable("BLE pairing not configured"))?;

        let accept = request.into_inner().accept;
        if !controller.confirm_passkey(accept).await {
            return Err(Status::failed_precondition(
                "no pending passkey confirmation request",
            ));
        }

        Ok(Response::new(Empty {}))
    }

    /// List all registered phones with their PSK metadata.
    async fn list_phones(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListPhonesResponse>, Status> {
        let records = self.storage.list_phone_psks().await.map_err(storage_err)?;

        let phones = records
            .iter()
            .map(|r| {
                let issued_at_ms = r
                    .issued_at
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                PhoneInfo {
                    phone_id: r.phone_id,
                    phone_key_hint: r.phone_key_hint as u32,
                    label: r.label.clone(),
                    issued_at_ms,
                    status: match r.status {
                        crate::phone_trust::PhonePskStatus::Active => "active".to_string(),
                        crate::phone_trust::PhonePskStatus::Revoked => "revoked".to_string(),
                    },
                }
            })
            .collect();

        Ok(Response::new(ListPhonesResponse { phones }))
    }

    /// Revoke a phone's PSK by phone_id.
    async fn revoke_phone(
        &self,
        request: Request<RevokePhoneRequest>,
    ) -> Result<Response<Empty>, Status> {
        let phone_id = request.into_inner().phone_id;
        self.storage
            .revoke_phone_psk(phone_id)
            .await
            .map_err(storage_err)?;
        Ok(Response::new(Empty {}))
    }
}

/// Bind and serve the admin gRPC server on a Unix domain socket (Linux/macOS)
/// or a Windows named pipe. This keeps the admin API off the network entirely.
///
/// On Unix the socket file is created at `socket_path`; any stale file from a
/// previous run is removed first. The parent directory is created if it does
/// not exist. On Windows `socket_path` is treated as a named-pipe name
/// (e.g. `\\.\pipe\sonde-admin`).
#[cfg(unix)]
pub async fn serve_admin(
    service: AdminService,
    socket_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;
    use tracing::info;

    // Create parent directory if it does not exist.
    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Remove a stale socket file left by a previous run.
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    info!(socket = %socket_path, "gRPC admin server listening on Unix socket");

    tonic::transport::Server::builder()
        .add_service(pb::gateway_admin_server::GatewayAdminServer::new(service))
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await?;
    Ok(())
}

/// Bind and serve the admin gRPC server on a Windows named pipe.
#[cfg(windows)]
pub async fn serve_admin(
    service: AdminService,
    pipe_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::ServerOptions;
    use tonic::transport::server::Connected;
    use tracing::info;

    /// Wraps a Windows named pipe server connection so it satisfies tonic's
    /// `Connected + AsyncRead + AsyncWrite + Unpin` bound.
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
    info!(pipe = %pipe_name, "gRPC admin server listening on named pipe");

    // Build a stream that accepts connections from the named pipe one at a time.
    // Each iteration creates a new server instance to wait for the next client.
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
        .add_service(pb::gateway_admin_server::GatewayAdminServer::new(service))
        .serve_with_incoming(incoming)
        .await?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub async fn serve_admin(
    _service: AdminService,
    _socket: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("sonde-gateway admin gRPC requires Unix (UDS) or Windows (named pipe)".into())
}
