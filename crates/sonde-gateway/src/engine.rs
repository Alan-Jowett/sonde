// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng;
use tokio::sync::RwLock;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK, MSG_COMMAND, MSG_GET_CHUNK,
    MSG_PROGRAM_ACK, MSG_WAKE,
};

use crate::crypto::{RustCryptoHmac, RustCryptoSha256};
use crate::handler::HandlerRouter;
use crate::program::ProgramLibrary;
use crate::registry::NodeRecord;
use crate::session::{SessionManager, SessionState};
use crate::storage::Storage;
use crate::transport::PeerAddress;

/// Default chunk size for program transfers (bytes).
const DEFAULT_CHUNK_SIZE: u32 = 128;

/// A pending command queued for a specific node.
#[derive(Debug, Clone)]
pub enum PendingCommand {
    RunEphemeral { program_hash: Vec<u8> },
    UpdateSchedule { interval_s: u32 },
    Reboot,
}

/// The core protocol engine. Ties together authentication, session management,
/// program library, command dispatch, and handler routing.
pub struct Gateway {
    storage: Arc<dyn Storage>,
    session_manager: Arc<SessionManager>,
    program_library: ProgramLibrary,
    crypto_hmac: RustCryptoHmac,
    #[allow(dead_code)]
    crypto_sha: RustCryptoSha256,
    /// Pending commands per node (ephemeral programs, schedule changes, reboots).
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    /// Optional handler router for APP_DATA dispatch (Phase 2C).
    handler_router: Option<Arc<HandlerRouter>>,
}

impl Gateway {
    /// Create a new gateway with the given storage backend and session timeout.
    /// No handler router is configured; APP_DATA is silently accepted.
    pub fn new(storage: Arc<dyn Storage>, session_timeout: Duration) -> Self {
        Self {
            storage,
            session_manager: Arc::new(SessionManager::new(session_timeout)),
            program_library: ProgramLibrary::new(),
            crypto_hmac: RustCryptoHmac,
            crypto_sha: RustCryptoSha256,
            pending_commands: Arc::new(RwLock::new(HashMap::new())),
            handler_router: None,
        }
    }

    /// Create a new gateway with a handler router for APP_DATA dispatch.
    pub fn new_with_handler(
        storage: Arc<dyn Storage>,
        session_timeout: Duration,
        handler_router: Arc<HandlerRouter>,
    ) -> Self {
        Self {
            storage,
            session_manager: Arc::new(SessionManager::new(session_timeout)),
            program_library: ProgramLibrary::new(),
            crypto_hmac: RustCryptoHmac,
            crypto_sha: RustCryptoSha256,
            pending_commands: Arc::new(RwLock::new(HashMap::new())),
            handler_router: Some(handler_router),
        }
    }

    /// Create a gateway that shares state with an `AdminService`.
    pub fn new_with_pending(
        storage: Arc<dyn Storage>,
        pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        Self {
            storage,
            session_manager,
            program_library: ProgramLibrary::new(),
            crypto_hmac: RustCryptoHmac,
            crypto_sha: RustCryptoSha256,
            pending_commands,
            handler_router: None,
        }
    }

    /// Queue a pending command for a node.
    pub async fn queue_command(&self, node_id: &str, cmd: PendingCommand) {
        self.pending_commands
            .write()
            .await
            .entry(node_id.to_string())
            .or_default()
            .push(cmd);
    }

    /// Expose the session manager for test inspection.
    pub fn session_manager(&self) -> &SessionManager {
        self.session_manager.as_ref()
    }

    /// Main entry point: process a raw inbound frame and optionally return a
    /// response frame. Returns `None` for silent discard.
    pub async fn process_frame(&self, raw: &[u8], peer: PeerAddress) -> Option<Vec<u8>> {
        // 1. Decode the frame (reject TooShort / TooLong)
        let decoded = decode_frame(raw).ok()?;

        // 2. Extract key_hint from header
        let key_hint = decoded.header.key_hint;

        // 3. Lookup candidate nodes by key_hint
        let candidates = self.storage.get_nodes_by_key_hint(key_hint).await.ok()?;
        if candidates.is_empty() {
            return None;
        }

        // 4. Try HMAC verify with each candidate PSK — identify the node
        let mut matched_node: Option<NodeRecord> = None;
        for candidate in &candidates {
            if verify_frame(&decoded, &candidate.psk, &self.crypto_hmac) {
                matched_node = Some(candidate.clone());
                break;
            }
        }
        let node = matched_node?;

        // 5. Dispatch by msg_type
        match decoded.header.msg_type {
            MSG_WAKE => {
                self.handle_wake(&node, &decoded.header, &decoded.payload, peer)
                    .await
            }
            MSG_GET_CHUNK | MSG_PROGRAM_ACK | MSG_APP_DATA => {
                self.handle_post_wake(&node, &decoded.header, &decoded.payload)
                    .await
            }
            _ => None,
        }
    }

    /// Handle a WAKE message: create session, determine command, respond.
    async fn handle_wake(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
        peer: PeerAddress,
    ) -> Option<Vec<u8>> {
        // 1. Decode NodeMessage::Wake from payload
        let (firmware_abi_version, program_hash, battery_mv) =
            match NodeMessage::decode(MSG_WAKE, payload) {
                Ok(NodeMessage::Wake {
                    firmware_abi_version,
                    program_hash,
                    battery_mv,
                }) => (firmware_abi_version, program_hash, battery_mv),
                _ => return None,
            };

        // 2. Create/replace session (random starting_seq, current timestamp_ms)
        let starting_seq: u64 = rand::thread_rng().gen();
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let _session = self
            .session_manager
            .create_session(node.node_id.clone(), peer, header.nonce, starting_seq)
            .await;

        // 3. Determine command (check pending commands, then program_hash match, then NOP)
        let command_payload = self.select_command(node, &program_hash).await?;

        // If the command involves a chunked transfer, update session state
        match &command_payload {
            CommandPayload::UpdateProgram {
                program_hash: ph,
                program_size,
                chunk_size,
                chunk_count,
            } => {
                let _ = self
                    .session_manager
                    .set_state(
                        &node.node_id,
                        SessionState::ChunkedTransfer {
                            program_hash: ph.clone(),
                            program_size: *program_size,
                            chunk_size: *chunk_size,
                            chunk_count: *chunk_count,
                            is_ephemeral: false,
                        },
                    )
                    .await;
            }
            CommandPayload::RunEphemeral {
                program_hash: ph,
                program_size,
                chunk_size,
                chunk_count,
            } => {
                let _ = self
                    .session_manager
                    .set_state(
                        &node.node_id,
                        SessionState::ChunkedTransfer {
                            program_hash: ph.clone(),
                            program_size: *program_size,
                            chunk_size: *chunk_size,
                            chunk_count: *chunk_count,
                            is_ephemeral: true,
                        },
                    )
                    .await;
            }
            _ => {}
        }

        // 4. Update registry (battery_mv, firmware_abi_version, last_seen)
        let mut updated_node = node.clone();
        updated_node.update_telemetry(battery_mv, firmware_abi_version);
        let _ = self.storage.upsert_node(&updated_node).await;

        // 5. Encode GatewayMessage::Command response
        let response_msg = GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload: command_payload,
        };
        let response_cbor = response_msg.encode().ok()?;

        // 6. Encode response frame (echoing wake nonce, using node's PSK)
        let response_header = FrameHeader {
            key_hint: node.key_hint,
            msg_type: MSG_COMMAND,
            nonce: header.nonce,
        };
        let frame = encode_frame(
            &response_header,
            &response_cbor,
            &node.psk,
            &self.crypto_hmac,
        )
        .ok()?;

        Some(frame)
    }

    /// Handle post-WAKE messages (GET_CHUNK, PROGRAM_ACK, APP_DATA).
    ///
    /// Flow: pre-decode payload → atomically verify+advance seq → dispatch.
    /// Pre-decoding before the atomic seq advance ensures malformed CBOR
    /// does not consume a sequence number, while the atomic check+increment
    /// prevents TOCTOU races on concurrent frames.
    async fn handle_post_wake(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Option<Vec<u8>> {
        // 1. Pre-decode: validate the message payload before touching session state
        let msg = NodeMessage::decode(header.msg_type, payload).ok()?;

        // 2. Atomically verify session + sequence and advance counter
        self.session_manager
            .verify_and_advance_seq(&node.node_id, header.nonce)
            .await
            .ok()?;

        // 3. Dispatch with pre-decoded message (side effects applied only after
        //    both decode and seq check have passed)
        match msg {
            NodeMessage::GetChunk { chunk_index } => {
                self.handle_get_chunk(node, header, chunk_index).await
            }
            NodeMessage::ProgramAck { program_hash } => {
                self.handle_program_ack(node, program_hash).await;
                None
            }
            NodeMessage::AppData { blob } => self.handle_app_data(node, header, blob).await,
            _ => None,
        }
    }

    /// Serve a chunk from the program library.
    async fn handle_get_chunk(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        chunk_index: u32,
    ) -> Option<Vec<u8>> {
        // Look up program transfer state from session
        let session = self.session_manager.get_session(&node.node_id).await?;
        let (program_hash, chunk_size) = match &session.state {
            SessionState::ChunkedTransfer {
                program_hash,
                chunk_size,
                ..
            } => (program_hash.clone(), *chunk_size),
            _ => return None,
        };

        // Get the program from storage
        let program = self.storage.get_program(&program_hash).await.ok()??;

        // Serve the chunk
        let chunk_data = self
            .program_library
            .get_chunk(&program.image, chunk_index, chunk_size)?
            .to_vec();

        // Encode CHUNK response
        let response_msg = GatewayMessage::Chunk {
            chunk_index,
            chunk_data,
        };
        let response_cbor = response_msg.encode().ok()?;

        let response_header = FrameHeader {
            key_hint: node.key_hint,
            msg_type: MSG_CHUNK,
            nonce: header.nonce,
        };
        let frame = encode_frame(
            &response_header,
            &response_cbor,
            &node.psk,
            &self.crypto_hmac,
        )
        .ok()?;

        Some(frame)
    }

    /// Handle PROGRAM_ACK: validate against session state, update the node's
    /// `current_program_hash` in the registry, and transition the session out
    /// of `ChunkedTransfer`. Silently discards if the session is not in
    /// `ChunkedTransfer` or the ACK hash does not match the active transfer.
    async fn handle_program_ack(&self, node: &NodeRecord, program_hash: Vec<u8>) {
        // Require an active ChunkedTransfer session for this node
        let session = match self.session_manager.get_session(&node.node_id).await {
            Some(s) => s,
            None => return,
        };

        // Only accept the ACK if it matches the program_hash of the active transfer
        let matches = matches!(
            &session.state,
            SessionState::ChunkedTransfer { program_hash: expected, .. }
                if *expected == program_hash
        );
        if !matches {
            return;
        }

        // Update node record with the confirmed program
        let mut updated_node = node.clone();
        updated_node.confirm_program(program_hash);
        let _ = self.storage.upsert_node(&updated_node).await;

        // Transition session from ChunkedTransfer to BpfExecuting
        let _ = self
            .session_manager
            .set_state(&node.node_id, SessionState::BpfExecuting)
            .await;
    }

    /// Route APP_DATA to the handler router (Phase 2C). Looks up the node's
    /// `current_program_hash` from storage, calls the handler, and wraps any
    /// non-empty reply in a `GatewayMessage::AppDataReply` frame.
    async fn handle_app_data(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        blob: Vec<u8>,
    ) -> Option<Vec<u8>> {
        let router = self.handler_router.as_ref()?;

        // Use the node's `current_program_hash` (set via PROGRAM_ACK) for routing.
        // The node record was already loaded during frame authentication.
        let program_hash = match &node.current_program_hash {
            Some(hash) => hash.clone(),
            None => return None,
        };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let reply_data = router
            .route_app_data(&node.node_id, &program_hash, &blob, timestamp, header.nonce)
            .await?;

        // Encode APP_DATA_REPLY with the same nonce as the incoming APP_DATA
        let response_msg = GatewayMessage::AppDataReply { blob: reply_data };
        let response_cbor = response_msg.encode().ok()?;

        let response_header = FrameHeader {
            key_hint: node.key_hint,
            msg_type: MSG_APP_DATA_REPLY,
            nonce: header.nonce,
        };
        let frame = encode_frame(
            &response_header,
            &response_cbor,
            &node.psk,
            &self.crypto_hmac,
        )
        .ok()?;

        Some(frame)
    }

    /// Command selection logic (priority order per design doc 6.4).
    async fn select_command(
        &self,
        node: &NodeRecord,
        node_program_hash: &[u8],
    ) -> Option<CommandPayload> {
        // Priority 1: Pending ephemeral program
        // Peek with a read lock first; only remove after successful program load.
        let ephemeral_hash = {
            let pending = self.pending_commands.read().await;
            if let Some(cmds) = pending.get(&node.node_id) {
                cmds.iter().find_map(|c| {
                    if let PendingCommand::RunEphemeral { program_hash } = c {
                        Some(program_hash.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        };
        if let Some(program_hash) = ephemeral_hash {
            if let Ok(Some(program)) = self.storage.get_program(&program_hash).await {
                let chunk_size = DEFAULT_CHUNK_SIZE;
                if let Some(chunk_count) = self
                    .program_library
                    .chunk_count(program.image.len(), chunk_size as usize)
                {
                    // Program loaded successfully — now remove from queue
                    {
                        let mut pending = self.pending_commands.write().await;
                        if let Some(cmds) = pending.get_mut(&node.node_id) {
                            if let Some(pos) = cmds
                                .iter()
                                .position(|c| matches!(c, PendingCommand::RunEphemeral { .. }))
                            {
                                cmds.remove(pos);
                            }
                        }
                    }
                    return Some(CommandPayload::RunEphemeral {
                        program_hash: program.hash,
                        program_size: program.size,
                        chunk_size,
                        chunk_count,
                    });
                }
            }
            // Program load/chunking failed — fall through to lower-priority commands
        }

        // Priority 2: program_hash mismatch → UPDATE_PROGRAM
        // Treat missing/failed program lookup as non-fatal; fall through to NOP.
        if let Some(assigned_hash) = &node.assigned_program_hash {
            if assigned_hash.as_slice() != node_program_hash {
                if let Ok(Some(program)) = self.storage.get_program(assigned_hash).await {
                    let chunk_size = DEFAULT_CHUNK_SIZE;
                    if let Some(chunk_count) = self
                        .program_library
                        .chunk_count(program.image.len(), chunk_size as usize)
                    {
                        return Some(CommandPayload::UpdateProgram {
                            program_hash: program.hash,
                            program_size: program.size,
                            chunk_size,
                            chunk_count,
                        });
                    }
                }
                // Program not found or chunk_count failed — fall through
            }
        }

        // Priority 3: Pending schedule change
        let schedule_interval = {
            let mut pending = self.pending_commands.write().await;
            if let Some(cmds) = pending.get_mut(&node.node_id) {
                if let Some(pos) = cmds
                    .iter()
                    .position(|c| matches!(c, PendingCommand::UpdateSchedule { .. }))
                {
                    match cmds.remove(pos) {
                        PendingCommand::UpdateSchedule { interval_s } => Some(interval_s),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(interval_s) = schedule_interval {
            return Some(CommandPayload::UpdateSchedule { interval_s });
        }

        // Priority 4: Pending reboot
        let has_reboot = {
            let mut pending = self.pending_commands.write().await;
            if let Some(cmds) = pending.get_mut(&node.node_id) {
                if let Some(pos) = cmds
                    .iter()
                    .position(|c| matches!(c, PendingCommand::Reboot))
                {
                    cmds.remove(pos);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if has_reboot {
            return Some(CommandPayload::Reboot);
        }

        // Priority 5: NOP
        Some(CommandPayload::Nop)
    }
}
