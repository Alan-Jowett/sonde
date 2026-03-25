// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tracing::{info, warn};

use sonde_protocol::{
    decode_frame, encode_frame, verify_frame, CommandPayload, FrameHeader, GatewayMessage,
    HmacProvider, NodeMessage, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK, MSG_COMMAND,
    MSG_GET_CHUNK, MSG_PEER_ACK, MSG_PEER_REQUEST, MSG_PROGRAM_ACK, MSG_WAKE, PEER_ACK_KEY_PROOF,
    PEER_ACK_KEY_STATUS, PEER_REQ_KEY_PAYLOAD,
};

use std::collections::BTreeMap;

use crate::crypto::{RustCryptoHmac, RustCryptoSha256};
use crate::gateway_identity::GatewayIdentity;
use crate::handler::HandlerRouter;
use crate::phone_trust::PhonePskStatus;
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
    FactoryReset,
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
    /// Cached gateway identity for ECDH decryption (lazy-loaded from storage).
    identity_cache: RwLock<Option<Arc<GatewayIdentity>>>,
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
            identity_cache: RwLock::new(None),
        }
    }

    /// Create a new gateway with a handler router for APP_DATA dispatch.
    ///
    /// # Warning
    ///
    /// This constructor allocates its own `pending_commands` and
    /// `SessionManager`. It is **not** suitable for production use where
    /// the admin API must share those objects. Use [`new_with_pending`]
    /// followed by [`set_handler_router`] instead. This method exists
    /// for test convenience only (D-485).
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
            identity_cache: RwLock::new(None),
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
            identity_cache: RwLock::new(None),
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

    /// Set the handler router for APP_DATA dispatch (GW-0504 AC4).
    ///
    /// Called after construction when `--handler-config` is provided,
    /// allowing the gateway to share `pending_commands` with the admin
    /// API while also routing APP_DATA to handler processes.
    pub fn set_handler_router(&mut self, router: Arc<HandlerRouter>) -> Result<(), &'static str> {
        if self.handler_router.is_some() {
            return Err(
                "handler_router is already set; set_handler_router must only be called once during initialization",
            );
        }
        self.handler_router = Some(router);
        Ok(())
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

        // GW-1211: For PEER_REQUEST (0x05), bypass the normal key-hint lookup
        // and HMAC verification. The node is not yet registered, so its PSK
        // is unknown. HMAC is verified later (step 8) using the node_psk
        // extracted from the decrypted payload.
        if decoded.header.msg_type == MSG_PEER_REQUEST {
            return self.handle_peer_request(raw, &decoded).await;
        }

        // 2. Extract key_hint from header
        let key_hint = decoded.header.key_hint;

        // 3. Lookup candidate nodes by key_hint
        let candidates = self.storage.get_nodes_by_key_hint(key_hint).await.ok()?;
        if candidates.is_empty() {
            // GW-1002: log discard of frames from unknown nodes.
            warn!(
                key_hint,
                "discarding frame from unknown node (no key_hint match)"
            );
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

    /// Lazy-load the gateway identity from storage (cached after first load).
    async fn get_identity(&self) -> Option<Arc<GatewayIdentity>> {
        // Fast path: return cached identity.
        {
            let cache = self.identity_cache.read().await;
            if let Some(ref id) = *cache {
                return Some(Arc::clone(id));
            }
        }
        // Slow path: load from storage.
        let id = self.storage.load_gateway_identity().await.ok()??;
        let arc = Arc::new(id);
        let mut cache = self.identity_cache.write().await;
        *cache = Some(Arc::clone(&arc));
        Some(arc)
    }

    /// Handle a PEER_REQUEST frame (GW-1211–GW-1221).
    ///
    /// Implements the 13-step pipeline from ble-pairing-protocol.md §7.3.
    /// All verification failures result in silent discard (no PEER_ACK).
    async fn handle_peer_request(
        &self,
        _raw: &[u8],
        decoded: &sonde_protocol::DecodedFrame,
    ) -> Option<Vec<u8>> {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Nonce};
        use hkdf::Hkdf;
        use sha2::Sha256;
        use x25519_dalek::PublicKey as X25519PublicKey;
        use zeroize::Zeroizing;

        use crate::registry::SensorDescriptor;

        const HKDF_INFO: &[u8] = b"sonde-node-pair-v1";
        const PROOF_DOMAIN: &[u8] = b"sonde-peer-ack-v1";
        const MAX_TIMESTAMP_DRIFT_S: u64 = 86400;

        // Step 1: msg_type already verified by caller (== MSG_PEER_REQUEST).

        // Step 2: Bypass key-hint lookup (handled by caller).

        // Step 3: Parse CBOR, extract encrypted_payload.
        let cbor: ciborium::Value = ciborium::from_reader(&decoded.payload[..]).ok()?;
        let map = cbor.as_map()?;
        let mut encrypted_payload: Option<&[u8]> = None;
        for (k, v) in map {
            let key = k.as_integer().and_then(|i| u64::try_from(i).ok())?;
            if key == PEER_REQ_KEY_PAYLOAD {
                encrypted_payload = v.as_bytes().map(|b| b.as_slice());
            }
        }
        let encrypted_payload = encrypted_payload?;

        // Step 4: Decrypt encrypted_payload (ECDH + HKDF + AES-256-GCM).
        // encrypted_payload layout: eph_public(32) + nonce(12) + ciphertext(N+16)
        if encrypted_payload.len() < 44 + 16 {
            return None; // Too short for eph_pub + nonce + tag
        }
        let eph_public_bytes: [u8; 32] = encrypted_payload[..32].try_into().ok()?;
        let gcm_nonce = &encrypted_payload[32..44];
        let ciphertext = &encrypted_payload[44..];

        let identity = self.get_identity().await?;
        let (x25519_secret, _) = identity.to_x25519().ok()?;
        let eph_public = X25519PublicKey::from(eph_public_bytes);
        let shared_secret = x25519_secret.diffie_hellman(&eph_public);

        // Reject zero shared secret (low-order point).
        if shared_secret.as_bytes() == &[0u8; 32] {
            return None;
        }

        let gateway_id = identity.gateway_id();
        let hkdf = Hkdf::<Sha256>::new(Some(gateway_id), shared_secret.as_bytes());
        let mut aes_key = Zeroizing::new([0u8; 32]);
        hkdf.expand(HKDF_INFO, &mut *aes_key).ok()?;

        let cipher = Aes256Gcm::new_from_slice(&*aes_key).ok()?;
        let nonce = Nonce::from_slice(gcm_nonce);
        let authenticated_request = cipher
            .decrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: ciphertext,
                    aad: gateway_id,
                },
            )
            .ok()?;

        // Step 5: Parse authenticated_request.
        // Layout: phone_key_hint(2) + cbor_bytes(N) + phone_hmac(32)
        if authenticated_request.len() < 2 + 32 {
            return None;
        }
        let phone_key_hint =
            u16::from_be_bytes([authenticated_request[0], authenticated_request[1]]);
        let cbor_bytes = &authenticated_request[2..authenticated_request.len() - 32];
        let phone_hmac = &authenticated_request[authenticated_request.len() - 32..];

        // Step 6: Verify phone HMAC (GW-1213).
        let phone_candidates = self
            .storage
            .get_phone_psks_by_key_hint(phone_key_hint)
            .await
            .ok()?;

        let mut matched_phone_id: Option<u32> = None;
        for phone in &phone_candidates {
            if matches!(phone.status, PhonePskStatus::Revoked) {
                continue;
            }
            if let Ok(hmac_arr) = <&[u8; 32]>::try_from(phone_hmac) {
                if self.crypto_hmac.verify(&*phone.psk, cbor_bytes, hmac_arr) {
                    matched_phone_id = Some(phone.phone_id);
                    break;
                }
            }
        }
        let phone_id = matched_phone_id?;

        // Step 7: Parse PairingRequest CBOR.
        let pairing_cbor: ciborium::Value = ciborium::from_reader(cbor_bytes).ok()?;
        let pairing_map = pairing_cbor.as_map()?;

        let mut node_id: Option<String> = None;
        let mut node_key_hint: Option<u16> = None;
        let mut node_psk: Option<[u8; 32]> = None;
        let mut rf_channel: Option<u8> = None;
        let mut timestamp: Option<u64> = None;
        let mut sensors: Vec<SensorDescriptor> = Vec::new();

        for (k, v) in pairing_map {
            let key = k.as_integer().and_then(|i| u64::try_from(i).ok())?;
            match key {
                1 => node_id = v.as_text().map(|s| s.to_owned()),
                2 => {
                    node_key_hint = v
                        .as_integer()
                        .and_then(|i| u64::try_from(i).ok())
                        .and_then(|v| u16::try_from(v).ok())
                }
                3 => {
                    if let Some(b) = v.as_bytes() {
                        if b.len() == 32 {
                            let mut psk = [0u8; 32];
                            psk.copy_from_slice(b);
                            node_psk = Some(psk);
                        }
                    }
                }
                4 => {
                    rf_channel = v
                        .as_integer()
                        .and_then(|i| u64::try_from(i).ok())
                        .and_then(|v| u8::try_from(v).ok())
                }
                5 => {
                    // Parse sensor descriptor array.
                    if let Some(arr) = v.as_array() {
                        for item in arr {
                            if let Some(sensor_map) = item.as_map() {
                                let mut sensor_type: Option<u8> = None;
                                let mut sensor_id: Option<u8> = None;
                                let mut label: Option<String> = None;
                                for (sk, sv) in sensor_map {
                                    let skey = sk.as_integer().and_then(|i| u64::try_from(i).ok());
                                    match skey {
                                        Some(1) => {
                                            sensor_type = sv
                                                .as_integer()
                                                .and_then(|i| u64::try_from(i).ok())
                                                .and_then(|v| u8::try_from(v).ok())
                                        }
                                        Some(2) => {
                                            sensor_id = sv
                                                .as_integer()
                                                .and_then(|i| u64::try_from(i).ok())
                                                .and_then(|v| u8::try_from(v).ok())
                                        }
                                        Some(3) => {
                                            if let Some(s) = sv.as_text() {
                                                if s.len() > 64 {
                                                    return None; // label exceeds 64-byte limit
                                                }
                                                label = Some(s.to_owned());
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                if let (Some(st), Some(si)) = (sensor_type, sensor_id) {
                                    // Validate sensor_type: 1=I2C, 2=ADC, 3=GPIO, 4=SPI.
                                    if !(1..=4).contains(&st) {
                                        return None;
                                    }
                                    sensors.push(SensorDescriptor {
                                        sensor_type: st,
                                        sensor_id: si,
                                        label,
                                    });
                                }
                            }
                        }
                    }
                }
                6 => timestamp = v.as_integer().and_then(|i| u64::try_from(i).ok()),
                _ => {} // ignore unknown keys for forward compatibility
            }
        }

        let node_id = node_id?;
        let node_key_hint = node_key_hint?;
        let node_psk = node_psk?;
        let rf_channel = rf_channel?;
        let timestamp = timestamp?;

        // Validate node_id length (1–64 bytes).
        if node_id.is_empty() || node_id.len() > 64 {
            return None;
        }

        // Validate rf_channel range (1–13).
        if !(1..=13).contains(&rf_channel) {
            return None;
        }

        // Step 8: Verify frame HMAC with extracted node_psk (GW-1214).
        if !verify_frame(decoded, &node_psk, &self.crypto_hmac) {
            return None;
        }

        // Step 9: Verify timestamp within ±86400s (GW-1215).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if now.abs_diff(timestamp) > MAX_TIMESTAMP_DRIFT_S {
            return None;
        }

        // Step 10 + 11: Verify key_hint consistency (GW-1217).
        if decoded.header.key_hint != node_key_hint {
            return None;
        }

        // Step 10 + 12: Atomically register node, rejecting if node_id exists (GW-1216, GW-1218).
        let mut record = NodeRecord::new(node_id, node_key_hint, node_psk);
        record.rf_channel = Some(rf_channel);
        record.sensors = sensors;
        record.registered_by_phone_id = Some(phone_id);
        if !self.storage.insert_node_if_not_exists(&record).await.ok()? {
            // Duplicate node_id. Check if PSK matches the existing record
            // (GW-1218 AC4). If so, still send PEER_ACK so the node can
            // complete enrollment after a lost ACK. If PSK differs, discard
            // silently (GW-1218 AC5).
            let existing = self.storage.get_node(&record.node_id).await.ok()??;
            if existing.psk != record.psk {
                info!(
                    node_id = %record.node_id,
                    key_hint = record.key_hint,
                    result = "duplicate_psk_mismatch",
                    "PEER_REQUEST processed"
                );
                return None;
            }
            info!(
                node_id = %record.node_id,
                key_hint = record.key_hint,
                result = "duplicate_ack_resent",
                "PEER_REQUEST processed"
            );
        } else {
            // GW-1300 AC1: log successful PEER_REQUEST registration.
            info!(
                node_id = %record.node_id,
                key_hint = record.key_hint,
                result = "registered",
                "PEER_REQUEST processed"
            );
        }

        // Step 13: Send PEER_ACK (GW-1219).
        // registration_proof = HMAC-SHA256(node_psk, "sonde-peer-ack-v1" || encrypted_payload)
        let mut proof_input = Vec::with_capacity(PROOF_DOMAIN.len() + encrypted_payload.len());
        proof_input.extend_from_slice(PROOF_DOMAIN);
        proof_input.extend_from_slice(encrypted_payload);
        let proof = self.crypto_hmac.compute(&node_psk, &proof_input);

        // Build CBOR: { 1: 0, 2: registration_proof }
        let ack_cbor = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(PEER_ACK_KEY_STATUS.into()),
                ciborium::Value::Integer(0.into()),
            ),
            (
                ciborium::Value::Integer(PEER_ACK_KEY_PROOF.into()),
                ciborium::Value::Bytes(proof.to_vec()),
            ),
        ]);
        let mut ack_cbor_buf = Vec::new();
        ciborium::into_writer(&ack_cbor, &mut ack_cbor_buf).ok()?;

        let ack_header = FrameHeader {
            key_hint: node_key_hint,
            msg_type: MSG_PEER_ACK,
            nonce: decoded.header.nonce, // echo nonce
        };

        let frame = encode_frame(&ack_header, &ack_cbor_buf, &node_psk, &self.crypto_hmac).ok()?;

        // GW-1300 AC2: log PEER_ACK frame encoded (transport send happens later).
        info!(node_id = %record.node_id, "PEER_ACK frame encoded");

        Some(frame)
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
                Ok(_) => return None,
                Err(e) => {
                    // GW-0101 AC3: log malformed inbound CBOR.
                    warn!(
                        node_id = %node.node_id,
                        error = %e,
                        "discarding WAKE with malformed CBOR payload"
                    );
                    return None;
                }
            };

        // 2. Create/replace session or reuse existing ChunkedTransfer session
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // GW-0602 AC5: If a ChunkedTransfer session is already active for
        // this node AND the WAKE nonce matches (i.e. this is a retry, not a
        // new wake cycle), reuse the existing session and its starting_seq —
        // otherwise the transfer state is lost and the node cannot complete
        // GET_CHUNK.
        let existing_session = self.session_manager.get_session(&node.node_id).await;
        let reuse_chunked = existing_session.as_ref().is_some_and(|s| {
            matches!(s.state, SessionState::ChunkedTransfer { .. }) && s.wake_nonce == header.nonce
        });

        let starting_seq: u64 = if reuse_chunked {
            // Reuse the session's current next_expected_seq so the COMMAND
            // response matches what the session is tracking.
            let seq = existing_session.as_ref().unwrap().next_expected_seq;
            info!(node_id = %node.node_id, seq, "WAKE retry — reusing existing ChunkedTransfer session");
            seq
        } else {
            let seq: u64 = {
                let mut buf = [0u8; 8];
                if let Err(err) = getrandom::fill(&mut buf) {
                    warn!(error = ?err, "CSPRNG failure while generating starting_seq; aborting WAKE handling");
                    return None;
                }
                u64::from_ne_bytes(buf)
            };
            let _session = self
                .session_manager
                .create_session(node.node_id.clone(), peer, header.nonce, seq)
                .await;
            info!(node_id = %node.node_id, seq, "session created");
            seq
        };

        // GW-1300 AC3: log WAKE received.
        info!(
            node_id = %node.node_id,
            seq = starting_seq,
            battery_mv,
            "WAKE received"
        );

        // 3. Determine command
        let command_payload = if reuse_chunked {
            // Re-send the same chunked transfer command from the existing session.
            let session = existing_session.unwrap();
            match &session.state {
                SessionState::ChunkedTransfer {
                    program_hash: ph,
                    program_size,
                    chunk_size,
                    chunk_count,
                    is_ephemeral,
                } => {
                    if *is_ephemeral {
                        CommandPayload::RunEphemeral {
                            program_hash: ph.clone(),
                            program_size: *program_size,
                            chunk_size: *chunk_size,
                            chunk_count: *chunk_count,
                        }
                    } else {
                        CommandPayload::UpdateProgram {
                            program_hash: ph.clone(),
                            program_size: *program_size,
                            chunk_size: *chunk_size,
                            chunk_count: *chunk_count,
                        }
                    }
                }
                _ => unreachable!(), // we checked reuse_chunked
            }
        } else {
            match self
                .select_command(node, &program_hash, firmware_abi_version)
                .await
            {
                Some(cmd) => cmd,
                None => return None,
            }
        };

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

        // 4a. Emit node_online EVENT to handlers (GW-0507)
        if let Some(router) = &self.handler_router {
            let mut details = BTreeMap::new();
            details.insert(
                "battery_mv".to_string(),
                ciborium::Value::Integer(battery_mv.into()),
            );
            details.insert(
                "firmware_abi_version".to_string(),
                ciborium::Value::Integer(firmware_abi_version.into()),
            );
            router
                .route_event(&node.node_id, "node_online", details, timestamp_ms / 1000)
                .await;
        }

        // GW-1300 AC4: log COMMAND selected (transport send happens later).
        let command_type = match &command_payload {
            CommandPayload::Nop => "Nop",
            CommandPayload::UpdateProgram { .. } => "UpdateProgram",
            CommandPayload::RunEphemeral { .. } => "RunEphemeral",
            CommandPayload::UpdateSchedule { .. } => "UpdateSchedule",
            CommandPayload::Reboot => "Reboot",
        };
        info!(
            node_id = %node.node_id,
            command_type,
            "COMMAND selected"
        );

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
        let msg = match NodeMessage::decode(header.msg_type, payload) {
            Ok(m) => m,
            Err(e) => {
                // GW-0101 AC3: log malformed inbound CBOR.
                warn!(
                    node_id = %node.node_id,
                    msg_type = header.msg_type,
                    error = %e,
                    "discarding post-WAKE message with malformed CBOR payload"
                );
                return None;
            }
        };

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
        updated_node.confirm_program(program_hash.clone());
        let _ = self.storage.upsert_node(&updated_node).await;

        // Emit program_updated EVENT to handlers (GW-0507)
        if let Some(router) = &self.handler_router {
            let mut details = BTreeMap::new();
            details.insert(
                "program_hash".to_string(),
                ciborium::Value::Bytes(program_hash),
            );
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            router
                .route_event(&node.node_id, "program_updated", details, timestamp)
                .await;
        }

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
        firmware_abi_version: u32,
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
                // GW-0703: ABI compatibility check — drop and warn if the program's ABI version
                // is set and does not match the node's reported firmware ABI version.
                // Since the node's ABI is permanent (firmware doesn't change between WAKEs),
                // an incompatible ephemeral can never be delivered and must be dropped.
                let abi_ok = match program.abi_version {
                    Some(prog_abi) if prog_abi != firmware_abi_version => {
                        warn!(
                            node_id = %node.node_id,
                            program_abi = prog_abi,
                            node_abi = firmware_abi_version,
                            "ABI mismatch: dropping RunEphemeral"
                        );
                        // Remove the incompatible command from the queue so subsequent
                        // compatible ephemeral commands can be delivered.
                        {
                            let mut pending = self.pending_commands.write().await;
                            if let Some(cmds) = pending.get_mut(&node.node_id) {
                                if let Some(pos) = cmds.iter().position(|c| {
                                    matches!(c, PendingCommand::RunEphemeral { program_hash: h } if h == &program.hash)
                                }) {
                                    cmds.remove(pos);
                                }
                            }
                        }
                        false
                    }
                    _ => true,
                };
                if abi_ok {
                    // GW-0202 AC3: reject ephemeral programs exceeding the
                    // ephemeral size budget. A program ingested as Resident
                    // (4 KB limit) may exceed the 2 KB ephemeral budget.
                    if program.size > crate::program::MAX_EPHEMERAL_SIZE {
                        warn!(
                            node_id = %node.node_id,
                            program_size = program.size,
                            limit = crate::program::MAX_EPHEMERAL_SIZE,
                            "ephemeral size budget exceeded — dropping RunEphemeral"
                        );
                        let mut pending = self.pending_commands.write().await;
                        if let Some(cmds) = pending.get_mut(&node.node_id) {
                            cmds.retain(|c| {
                                !matches!(c, PendingCommand::RunEphemeral { program_hash: h } if h == &program.hash)
                            });
                            if cmds.is_empty() {
                                pending.remove(&node.node_id);
                            }
                        }
                    } else {
                        let chunk_size = DEFAULT_CHUNK_SIZE;
                        if let Some(chunk_count) = self
                            .program_library
                            .chunk_count(program.image.len(), chunk_size as usize)
                        {
                            // Program loaded successfully — now remove from queue (match by hash).
                            let deliver_hash = program.hash.clone();
                            {
                                let mut pending = self.pending_commands.write().await;
                                if let Some(cmds) = pending.get_mut(&node.node_id) {
                                    if let Some(pos) = cmds.iter().position(|c| {
                                        matches!(c, PendingCommand::RunEphemeral { program_hash: h } if h == &deliver_hash)
                                    }) {
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
                }
            }
            // Program load/chunking failed or ABI mismatch — fall through to lower-priority commands
        }

        // Priority 2: program_hash mismatch → UPDATE_PROGRAM
        // Treat missing/failed program lookup as non-fatal; fall through to NOP.
        if let Some(assigned_hash) = &node.assigned_program_hash {
            if assigned_hash.as_slice() != node_program_hash {
                if let Ok(Some(program)) = self.storage.get_program(assigned_hash).await {
                    // GW-0703: ABI compatibility check — skip if the program's ABI version
                    // is set and does not match the node's reported firmware ABI version.
                    let abi_ok = match program.abi_version {
                        Some(prog_abi) if prog_abi != firmware_abi_version => {
                            warn!(
                                node_id = %node.node_id,
                                program_abi = prog_abi,
                                node_abi = firmware_abi_version,
                                "ABI mismatch: skipping UPDATE_PROGRAM"
                            );
                            false
                        }
                        _ => true,
                    };
                    if abi_ok {
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
                }
                // Program not found, ABI mismatch, or chunk_count failed — fall through
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

    /// Check for nodes that have missed their expected wake interval.
    ///
    /// Emits a `node_timeout` EVENT to handlers for each timed-out node.
    /// A node is considered timed-out when `multiplier × schedule_interval_s`
    /// has elapsed since its `last_seen` timestamp (default multiplier: 3,
    /// per gateway-design.md). Call this periodically from the gateway main
    /// loop.
    pub async fn check_node_timeouts(&self, multiplier: u64) {
        let router = match &self.handler_router {
            Some(r) => r,
            None => return,
        };

        let multiplier = if multiplier == 0 { 3 } else { multiplier };

        let nodes = self.storage.list_nodes().await.unwrap_or_default();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        for node in &nodes {
            let interval = node.schedule_interval_s as u64;
            if interval == 0 {
                continue;
            }

            let last_seen = match node.last_seen {
                Some(ts) => match ts.duration_since(UNIX_EPOCH) {
                    Ok(d) => d.as_secs(),
                    Err(_) => continue,
                },
                None => continue,
            };

            let deadline = last_seen.saturating_add(interval.saturating_mul(multiplier));
            if now.as_secs() > deadline {
                let mut details = BTreeMap::new();
                details.insert(
                    "last_seen".to_string(),
                    ciborium::Value::Integer(last_seen.into()),
                );
                details.insert(
                    "expected_interval_s".to_string(),
                    ciborium::Value::Integer(interval.into()),
                );
                router
                    .route_event(&node.node_id, "node_timeout", details, now.as_secs())
                    .await;
            }
        }
    }
}
