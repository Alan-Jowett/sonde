// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng;
use tokio::sync::RwLock;

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MSG_APP_DATA, MSG_CHUNK, MSG_COMMAND, MSG_GET_CHUNK, MSG_PROGRAM_ACK, MSG_WAKE,
};

use crate::crypto::{RustCryptoHmac, RustCryptoSha256};
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
/// program library, and command dispatch.
pub struct Gateway {
    storage: Arc<dyn Storage>,
    session_manager: SessionManager,
    program_library: ProgramLibrary,
    crypto_hmac: RustCryptoHmac,
    #[allow(dead_code)]
    crypto_sha: RustCryptoSha256,
    /// Pending commands per node (ephemeral programs, schedule changes, reboots).
    pending_commands: Arc<RwLock<HashMap<String, PendingCommand>>>,
}

impl Gateway {
    /// Create a new gateway with the given storage backend and session timeout.
    pub fn new(storage: Arc<dyn Storage>, session_timeout: Duration) -> Self {
        Self {
            storage,
            session_manager: SessionManager::new(session_timeout),
            program_library: ProgramLibrary::new(),
            crypto_hmac: RustCryptoHmac,
            crypto_sha: RustCryptoSha256,
            pending_commands: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Queue a pending command for a node.
    pub async fn queue_command(&self, node_id: &str, cmd: PendingCommand) {
        self.pending_commands
            .write()
            .await
            .insert(node_id.to_string(), cmd);
    }

    /// Expose the session manager for test inspection.
    pub fn session_manager(&self) -> &SessionManager {
        &self.session_manager
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
        let wake = match NodeMessage::decode(MSG_WAKE, payload) {
            Ok(NodeMessage::Wake { .. }) => NodeMessage::decode(MSG_WAKE, payload).ok()?,
            _ => return None,
        };

        let (firmware_abi_version, program_hash, battery_mv) = match wake {
            NodeMessage::Wake {
                firmware_abi_version,
                program_hash,
                battery_mv,
            } => (firmware_abi_version, program_hash, battery_mv),
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
    async fn handle_post_wake(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Option<Vec<u8>> {
        // 1. Verify session exists + sequence number matches
        self.session_manager
            .verify_and_advance_seq(&node.node_id, header.nonce)
            .await
            .ok()?;

        // 2. Dispatch by msg_type
        match header.msg_type {
            MSG_GET_CHUNK => self.handle_get_chunk(node, header, payload).await,
            MSG_PROGRAM_ACK => {
                self.handle_program_ack(node, payload).await;
                None
            }
            MSG_APP_DATA => {
                // Phase 2B: accept but no routing yet (Phase 2C)
                None
            }
            _ => None,
        }
    }

    /// Serve a chunk from the program library.
    async fn handle_get_chunk(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Option<Vec<u8>> {
        let get_chunk_msg = NodeMessage::decode(MSG_GET_CHUNK, payload).ok()?;
        let chunk_index = match get_chunk_msg {
            NodeMessage::GetChunk { chunk_index } => chunk_index,
            _ => return None,
        };

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

    /// Handle PROGRAM_ACK: update the node's current_program_hash in the registry.
    async fn handle_program_ack(&self, node: &NodeRecord, payload: &[u8]) {
        if let Ok(NodeMessage::ProgramAck { program_hash }) =
            NodeMessage::decode(MSG_PROGRAM_ACK, payload)
        {
            let mut updated_node = node.clone();
            updated_node.confirm_program(program_hash);
            let _ = self.storage.upsert_node(&updated_node).await;
        }
    }

    /// Command selection logic (priority order per design doc 6.4).
    async fn select_command(
        &self,
        node: &NodeRecord,
        node_program_hash: &[u8],
    ) -> Option<CommandPayload> {
        // Priority 1: Pending ephemeral program
        {
            let mut pending = self.pending_commands.write().await;
            if let Some(PendingCommand::RunEphemeral { program_hash }) =
                pending.get(&node.node_id).cloned()
            {
                pending.remove(&node.node_id);
                let program = self.storage.get_program(&program_hash).await.ok()??;
                let chunk_size = DEFAULT_CHUNK_SIZE;
                let chunk_count = self
                    .program_library
                    .chunk_count(program.image.len(), chunk_size as usize)?;
                return Some(CommandPayload::RunEphemeral {
                    program_hash: program.hash,
                    program_size: program.size,
                    chunk_size,
                    chunk_count,
                });
            }
        }

        // Priority 2: program_hash mismatch → UPDATE_PROGRAM
        if let Some(assigned_hash) = &node.assigned_program_hash {
            if assigned_hash.as_slice() != node_program_hash {
                let program = self.storage.get_program(assigned_hash).await.ok()??;
                let chunk_size = DEFAULT_CHUNK_SIZE;
                let chunk_count = self
                    .program_library
                    .chunk_count(program.image.len(), chunk_size as usize)?;
                return Some(CommandPayload::UpdateProgram {
                    program_hash: program.hash,
                    program_size: program.size,
                    chunk_size,
                    chunk_count,
                });
            }
        }

        // Priority 3: Pending schedule change
        {
            let mut pending = self.pending_commands.write().await;
            if let Some(PendingCommand::UpdateSchedule { interval_s }) =
                pending.get(&node.node_id).cloned()
            {
                pending.remove(&node.node_id);
                return Some(CommandPayload::UpdateSchedule { interval_s });
            }
        }

        // Priority 4: Pending reboot
        {
            let mut pending = self.pending_commands.write().await;
            if let Some(PendingCommand::Reboot) = pending.get(&node.node_id) {
                pending.remove(&node.node_id);
                return Some(CommandPayload::Reboot);
            }
        }

        // Priority 5: NOP
        Some(CommandPayload::Nop)
    }
}
