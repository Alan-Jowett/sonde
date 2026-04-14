// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tracing::{info, warn};

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK, MSG_COMMAND, MSG_DIAG_REPLY,
    MSG_DIAG_REQUEST, MSG_GET_CHUNK, MSG_PEER_ACK, MSG_PEER_REQUEST, MSG_PROGRAM_ACK, MSG_WAKE,
    PEER_ACK_KEY_STATUS, PEER_REQ_KEY_PAYLOAD,
};

use std::collections::BTreeMap;

use crate::crypto::RustCryptoSha256;
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

/// Parsed fields from a PairingRequest CBOR payload.
///
/// Used by both the HMAC and AEAD `handle_peer_request` paths to avoid
/// duplicating the CBOR parsing and validation logic.
struct PairingRequestFields {
    node_id: String,
    node_key_hint: u16,
    node_psk: [u8; 32],
    rf_channel: u8,
    timestamp: u64,
    sensors: Vec<crate::registry::SensorDescriptor>,
}

/// Parse and validate a PairingRequest CBOR payload.
///
/// Returns `None` on any parse/validation failure (silent discard per protocol).
fn parse_pairing_request(cbor_bytes: &[u8]) -> Option<PairingRequestFields> {
    use crate::registry::SensorDescriptor;

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
                                                return None;
                                            }
                                            label = Some(s.to_owned());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if let (Some(st), Some(si)) = (sensor_type, sensor_id) {
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
            _ => {}
        }
    }

    let node_id = node_id?;
    let node_key_hint = node_key_hint?;
    let node_psk = node_psk?;
    let rf_channel = rf_channel?;
    let timestamp = timestamp?;

    if node_id.is_empty() || node_id.len() > 64 {
        return None;
    }
    if !(1..=13).contains(&rf_channel) {
        return None;
    }

    Some(PairingRequestFields {
        node_id,
        node_key_hint,
        node_psk,
        rf_channel,
        timestamp,
        sensors,
    })
}

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
    #[allow(dead_code)]
    crypto_sha: RustCryptoSha256,
    /// Pending commands per node (ephemeral programs, schedule changes, reboots).
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    /// Shared handler router for APP_DATA dispatch and event routing (GW-1407).
    handler_router: Arc<tokio::sync::RwLock<HandlerRouter>>,
    /// Cached gateway identity metadata for pairing/peer-request handling (lazy-loaded from storage).
    #[allow(dead_code)]
    identity_cache: RwLock<Option<Arc<GatewayIdentity>>>,
    /// RSSI threshold (dBm) at or above which signal is "good".
    rssi_good_threshold: i8,
    /// RSSI threshold (dBm) below which signal is "bad".
    rssi_bad_threshold: i8,
    /// Deferred handler replies awaiting delivery on the next WAKE cycle.
    deferred_replies: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl Gateway {
    /// Create a new gateway with the given storage backend and session timeout.
    /// An empty `HandlerRouter` is created (GW-1407).
    pub fn new(storage: Arc<dyn Storage>, session_timeout: Duration) -> Self {
        Self {
            storage,
            session_manager: Arc::new(SessionManager::new(session_timeout)),
            program_library: ProgramLibrary::new(),
            crypto_sha: RustCryptoSha256,
            pending_commands: Arc::new(RwLock::new(HashMap::new())),
            handler_router: Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(Vec::new()))),
            identity_cache: RwLock::new(None),
            rssi_good_threshold: -60,
            rssi_bad_threshold: -75,
            deferred_replies: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new gateway with a handler router for APP_DATA dispatch.
    ///
    /// # Warning
    ///
    /// This constructor allocates its own `pending_commands` and
    /// `SessionManager`. It is **not** suitable for production use where
    /// the admin API must share those objects. Use [`new_with_pending`]
    /// instead, passing the shared `HandlerRouter`. This method exists
    /// for test convenience only (D-485).
    pub fn new_with_handler(
        storage: Arc<dyn Storage>,
        session_timeout: Duration,
        handler_router: Arc<tokio::sync::RwLock<HandlerRouter>>,
    ) -> Self {
        Self {
            storage,
            session_manager: Arc::new(SessionManager::new(session_timeout)),
            program_library: ProgramLibrary::new(),
            crypto_sha: RustCryptoSha256,
            pending_commands: Arc::new(RwLock::new(HashMap::new())),
            handler_router,
            identity_cache: RwLock::new(None),
            rssi_good_threshold: -60,
            rssi_bad_threshold: -75,
            deferred_replies: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a gateway that shares state with an `AdminService` (GW-1407, D-485).
    pub fn new_with_pending(
        storage: Arc<dyn Storage>,
        pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
        session_manager: Arc<SessionManager>,
        handler_router: Arc<tokio::sync::RwLock<HandlerRouter>>,
    ) -> Self {
        Self {
            storage,
            session_manager,
            program_library: ProgramLibrary::new(),
            crypto_sha: RustCryptoSha256,
            pending_commands,
            handler_router,
            identity_cache: RwLock::new(None),
            rssi_good_threshold: -60,
            rssi_bad_threshold: -75,
            deferred_replies: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set RSSI thresholds for diagnostic signal quality assessment (GW-1705).
    pub fn set_rssi_thresholds(&mut self, good: i8, bad: i8) {
        if good > bad {
            self.rssi_good_threshold = good;
            self.rssi_bad_threshold = bad;
        } else {
            tracing::error!(
                good,
                bad,
                "invalid RSSI thresholds (good must be > bad), keeping existing values (GW-1705)"
            );
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

    /// Return a clone of the shared handler router reference (GW-1407).
    pub fn handler_router(&self) -> Arc<tokio::sync::RwLock<HandlerRouter>> {
        Arc::clone(&self.handler_router)
    }

    /// Process a raw frame using AES-256-GCM authenticated encryption.
    ///
    /// Decodes the
    /// frame header, looks up candidate PSKs by `key_hint`, then tries
    /// each candidate with [`open_frame`] (AES-256-GCM decrypt + auth).
    ///
    /// For `PEER_REQUEST` frames, the `key_hint` identifies a phone PSK
    /// (not a node PSK).  The outer frame is decrypted with `phone_psk`,
    /// and the inner payload is also decrypted with `phone_psk`.
    pub async fn process_frame(&self, raw: &[u8], peer: PeerAddress) -> Option<Vec<u8>> {
        self.process_frame_with_rssi(raw, peer, None).await
    }

    /// Process a raw frame with optional RSSI metadata from the modem.
    ///
    /// RSSI is used for DIAG_REQUEST signal quality assessment (GW-1702).
    pub async fn process_frame_with_rssi(
        &self,
        raw: &[u8],
        peer: PeerAddress,
        rssi: Option<i8>,
    ) -> Option<Vec<u8>> {
        use crate::aead::GatewayAead;

        let decoded = decode_frame(raw).ok()?;

        // PEER_REQUEST: key_hint identifies a phone PSK, not a node.
        if decoded.header.msg_type == MSG_PEER_REQUEST {
            return self.handle_peer_request(&decoded).await;
        }

        // DIAG_REQUEST: key_hint identifies a phone PSK (GW-1700).
        if decoded.header.msg_type == MSG_DIAG_REQUEST {
            return self.handle_diag_request(&decoded, rssi, &peer).await;
        }

        let key_hint = decoded.header.key_hint;
        let candidates = self.storage.get_nodes_by_key_hint(key_hint).await.ok()?;
        if candidates.is_empty() {
            warn!(
                key_hint,
                "discarding AEAD frame from unknown node (no key_hint match)"
            );
            return None;
        }

        let aead = GatewayAead;
        let mut matched_node: Option<NodeRecord> = None;
        let mut plaintext_payload: Option<Vec<u8>> = None;
        for candidate in &candidates {
            if let Ok(pt) = open_frame(&decoded, &candidate.psk, &aead, &self.crypto_sha) {
                matched_node = Some(candidate.clone());
                plaintext_payload = Some(pt);
                break;
            }
        }
        let node = matched_node?;
        let payload = plaintext_payload?;

        match decoded.header.msg_type {
            MSG_WAKE => {
                self.handle_wake(&node, &decoded.header, &payload, peer)
                    .await
            }
            MSG_GET_CHUNK | MSG_PROGRAM_ACK | MSG_APP_DATA => {
                self.handle_post_wake(&node, &decoded.header, &payload)
                    .await
            }
            _ => None,
        }
    }

    /// Encode a response frame using AES-256-GCM.
    fn encode_response(
        &self,
        header: &FrameHeader,
        cbor: &[u8],
        psk: &[u8; 32],
    ) -> Option<Vec<u8>> {
        use crate::aead::GatewayAead;
        encode_frame(header, cbor, psk, &GatewayAead, &self.crypto_sha).ok()
    }
    #[allow(dead_code)]
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

    /// Handle a PEER_REQUEST frame over the AEAD path.
    ///
    /// The phone builds the complete ESP-NOW PEER_REQUEST frame encrypted
    /// with `phone_psk`.  The gateway:
    /// 1. Looks up phone PSK candidates by `key_hint`.
    /// 2. Decrypts the outer AEAD frame with `phone_psk`.
    /// 3. Extracts the inner `encrypted_payload` from CBOR `{1: bstr}`.
    /// 4. Decrypts the inner payload with `phone_psk` (AAD = `"sonde-pairing-v2"`).
    /// 5. Parses the PairingRequest CBOR and registers the node.
    /// 6. Sends PEER_ACK encrypted with `node_psk`.
    async fn handle_peer_request(
        &self,
        decoded: &sonde_protocol::DecodedFrame<'_>,
    ) -> Option<Vec<u8>> {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Nonce};

        use crate::aead::GatewayAead;

        const PAIRING_AAD: &[u8] = b"sonde-pairing-v2";
        const MAX_TIMESTAMP_DRIFT_S: u64 = 86400;

        let aead = GatewayAead;

        // Step 1: Look up phone PSK candidates by key_hint.
        let key_hint = decoded.header.key_hint;
        let phone_candidates = self
            .storage
            .get_phone_psks_by_key_hint(key_hint)
            .await
            .ok()?;

        // Step 2: Decrypt outer AEAD frame with each candidate phone_psk.
        // Keep a reference to the matched record to avoid copying PSK out
        // of its `Zeroizing` wrapper.
        let mut matched_phone: Option<&crate::phone_trust::PhonePskRecord> = None;
        let mut outer_payload: Option<Vec<u8>> = None;

        for phone in &phone_candidates {
            if matches!(phone.status, PhonePskStatus::Revoked) {
                continue;
            }
            if let Ok(pt) = open_frame(decoded, &phone.psk, &aead, &self.crypto_sha) {
                matched_phone = Some(phone);
                outer_payload = Some(pt);
                break;
            }
        }
        let matched_phone = matched_phone?;
        let phone_id = matched_phone.phone_id;
        let cbor_payload = outer_payload?;

        // Step 3: Parse CBOR, extract encrypted_payload (key 1).
        let cbor: ciborium::Value = ciborium::from_reader(&cbor_payload[..]).ok()?;
        let map = cbor.as_map()?;
        let mut encrypted_payload: Option<&[u8]> = None;
        for (k, v) in map {
            if let Some(key_val) = k.as_integer().and_then(|i| u64::try_from(i).ok()) {
                if key_val == PEER_REQ_KEY_PAYLOAD {
                    encrypted_payload = v.as_bytes().map(|b| b.as_slice());
                }
            }
        }
        let encrypted_payload = encrypted_payload?;

        // Step 4: Decrypt inner payload with phone_psk (via Zeroizing ref).
        // Layout: inner_nonce(12) ‖ ciphertext ‖ tag(16)
        if encrypted_payload.len() < 12 + 16 {
            return None;
        }
        let inner_nonce = Nonce::from_slice(&encrypted_payload[..12]);
        let inner_ciphertext = &encrypted_payload[12..];

        let cipher = Aes256Gcm::new_from_slice(&*matched_phone.psk).ok()?;
        let pairing_request_bytes = cipher
            .decrypt(
                inner_nonce,
                aes_gcm::aead::Payload {
                    msg: inner_ciphertext,
                    aad: PAIRING_AAD,
                },
            )
            .ok()?;

        // Step 5: Parse PairingRequest CBOR (shared with HMAC path).
        let pr = parse_pairing_request(&pairing_request_bytes)?;

        // Step 6: Verify timestamp within ±86400s (GW-1215).
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        if now.abs_diff(pr.timestamp) > MAX_TIMESTAMP_DRIFT_S {
            return None;
        }

        // Step 7: Validate key_hint consistency (GW-1217).
        let expected_hint = sonde_protocol::key_hint_from_psk(&pr.node_psk, &self.crypto_sha);
        if pr.node_key_hint != expected_hint {
            return None;
        }

        // Step 8: Register node (GW-1216, GW-1218).
        let mut record = NodeRecord::new(pr.node_id, pr.node_key_hint, pr.node_psk);
        record.rf_channel = Some(pr.rf_channel);
        record.sensors = pr.sensors;
        record.registered_by_phone_id = Some(phone_id);
        if !self.storage.insert_node_if_not_exists(&record).await.ok()? {
            let existing = self.storage.get_node(&record.node_id).await.ok()??;
            if existing.psk != record.psk {
                info!(
                    node_id = %record.node_id,
                    key_hint = record.key_hint,
                    result = "duplicate_psk_mismatch",
                    "PEER_REQUEST (AEAD) processed"
                );
                return None;
            }
            info!(
                node_id = %record.node_id,
                key_hint = record.key_hint,
                result = "duplicate_ack_resent",
                "PEER_REQUEST (AEAD) processed"
            );
        } else {
            info!(
                node_id = %record.node_id,
                key_hint = record.key_hint,
                result = "registered",
                "PEER_REQUEST (AEAD) processed"
            );
        }

        // Step 9: Send PEER_ACK(0) encrypted with node_psk via AEAD.
        let ack_cbor = ciborium::Value::Map(vec![(
            ciborium::Value::Integer(PEER_ACK_KEY_STATUS.into()),
            ciborium::Value::Integer(0.into()),
        )]);
        let mut ack_cbor_buf = Vec::new();
        ciborium::into_writer(&ack_cbor, &mut ack_cbor_buf).ok()?;

        let ack_header = FrameHeader {
            key_hint: record.key_hint,
            msg_type: MSG_PEER_ACK,
            nonce: decoded.header.nonce,
        };

        let frame = encode_frame(
            &ack_header,
            &ack_cbor_buf,
            &record.psk,
            &aead,
            &self.crypto_sha,
        )
        .ok()?;

        info!(node_id = %record.node_id, "PEER_ACK (AEAD) frame encoded");

        Some(frame)
    }

    /// Handle a DIAG_REQUEST frame (GW-1700 through GW-1706).
    ///
    /// Authenticates with phone PSK, measures RSSI, and returns a
    /// DIAG_REPLY with signal quality assessment. No session required.
    async fn handle_diag_request(
        &self,
        decoded: &sonde_protocol::DecodedFrame<'_>,
        rssi: Option<i8>,
        peer: &PeerAddress,
    ) -> Option<Vec<u8>> {
        use crate::aead::GatewayAead;

        let aead = GatewayAead;
        let key_hint = decoded.header.key_hint;

        // Step 1: Look up phone PSK candidates by key_hint (GW-1700).
        let phone_candidates = self
            .storage
            .get_phone_psks_by_key_hint(key_hint)
            .await
            .ok()?;

        // Step 2: Decrypt with each non-revoked candidate.
        let mut matched_phone: Option<&crate::phone_trust::PhonePskRecord> = None;
        let mut payload: Option<Vec<u8>> = None;

        for phone in &phone_candidates {
            if matches!(phone.status, PhonePskStatus::Revoked) {
                continue;
            }
            if let Ok(pt) = open_frame(decoded, &phone.psk, &aead, &self.crypto_sha) {
                matched_phone = Some(phone);
                payload = Some(pt);
                break;
            }
        }
        let matched_phone = matched_phone?;
        let cbor_payload = payload?;

        // Step 3: Decode DIAG_REQUEST CBOR (GW-1700).
        let msg = NodeMessage::decode(MSG_DIAG_REQUEST, &cbor_payload).ok()?;
        let diagnostic_type = match msg {
            NodeMessage::DiagRequest { diagnostic_type } => diagnostic_type,
            _ => return None,
        };

        if diagnostic_type != sonde_protocol::DIAG_TYPE_RSSI {
            warn!(diagnostic_type, peer = ?peer, "unknown diagnostic_type in DIAG_REQUEST, ignoring");
            return None;
        }

        info!(
            key_hint,
            rssi = rssi.unwrap_or(0),
            peer = ?peer,
            "DIAG_REQUEST received (GW-1706)"
        );

        // Step 4: Assess signal quality (GW-1703).
        let (rssi_dbm, signal_quality) = match rssi {
            Some(r) => {
                let sq = if r >= self.rssi_good_threshold {
                    sonde_protocol::SIGNAL_QUALITY_GOOD
                } else if r >= self.rssi_bad_threshold {
                    sonde_protocol::SIGNAL_QUALITY_MARGINAL
                } else {
                    sonde_protocol::SIGNAL_QUALITY_BAD
                };
                (r, sq)
            }
            None => {
                warn!("RSSI unavailable for DIAG_REQUEST, using sentinel (GW-1702)");
                (0i8, sonde_protocol::SIGNAL_QUALITY_BAD)
            }
        };

        // Step 5: Build DIAG_REPLY (GW-1704).
        let reply = GatewayMessage::DiagReply {
            diagnostic_type,
            rssi_dbm,
            signal_quality,
        };
        let reply_cbor = reply.encode().ok()?;

        // Echo the request nonce (GW-1704).
        let reply_header = FrameHeader {
            key_hint,
            msg_type: MSG_DIAG_REPLY,
            nonce: decoded.header.nonce,
        };

        let frame = self.encode_response(&reply_header, &reply_cbor, &matched_phone.psk)?;

        info!(rssi_dbm, signal_quality, peer = ?peer, "DIAG_REPLY sent (GW-1706)");

        Some(frame)
    }

    /// Shared WAKE business logic: decode, session management, command
    /// selection, telemetry update, and response CBOR encoding.
    ///
    /// Returns `(response_header, response_cbor, deferred_delivered)` so the
    /// caller can apply the appropriate frame codec and clean up deferred state.
    async fn handle_wake_core(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
        peer: PeerAddress,
    ) -> Option<(FrameHeader, Vec<u8>, bool)> {
        // 1. Decode NodeMessage::Wake from payload
        let (firmware_abi_version, program_hash, battery_mv, firmware_version, wake_blob) =
            match NodeMessage::decode(MSG_WAKE, payload) {
                Ok(NodeMessage::Wake {
                    firmware_abi_version,
                    program_hash,
                    battery_mv,
                    firmware_version,
                    blob,
                }) => (
                    firmware_abi_version,
                    program_hash,
                    battery_mv,
                    firmware_version,
                    blob,
                ),
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

        // 4. Retrieve previously stored deferred reply for THIS cycle's COMMAND
        // (checked after command selection below — only injected into NOP commands)

        // 5. Determine command
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

        // 4. Update registry (battery_mv, firmware_abi_version, firmware_version, last_seen)
        let mut updated_node = node.clone();
        updated_node.update_telemetry(battery_mv, firmware_abi_version, firmware_version);
        let _ = self.storage.upsert_node(&updated_node).await;

        // 4a. Emit node_online EVENT to handlers (GW-0507)
        {
            let process_refs = self.handler_router.read().await.clone_all_process_refs();
            // Lock released — broadcast events without holding router lock.
            let mut details = BTreeMap::new();
            details.insert(
                "battery_mv".to_string(),
                ciborium::Value::Integer(battery_mv.into()),
            );
            details.insert(
                "firmware_abi_version".to_string(),
                ciborium::Value::Integer(firmware_abi_version.into()),
            );
            if let Some(ref fv) = updated_node.firmware_version {
                details.insert(
                    "firmware_version".to_string(),
                    ciborium::Value::Text(fv.clone()),
                );
            }
            let msg = crate::handler::HandlerMessage::Event {
                node_id: node.node_id.clone(),
                event_type: "node_online".to_string(),
                details,
                timestamp: timestamp_ms / 1000,
            };
            for process_arc in &process_refs {
                let mut process = process_arc.lock().await;
                process.send_event(&msg).await;
            }
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

        // 6. Encode GatewayMessage::Command response
        // Peek at deferred reply for NOP commands; remove only after successful encode.
        let has_deferred = matches!(command_payload, CommandPayload::Nop)
            && self
                .deferred_replies
                .read()
                .await
                .contains_key(&node.node_id);
        let command_blob = if has_deferred {
            self.deferred_replies
                .read()
                .await
                .get(&node.node_id)
                .cloned()
        } else {
            None
        };
        let response_msg = GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload: command_payload,
            blob: command_blob,
        };
        let response_cbor = response_msg.encode().ok()?;
        // NOTE: Deferred reply removal happens in handle_wake() after AEAD
        // encoding succeeds, to prevent data loss if framing fails.

        // 6. Build response header (echoing wake nonce)
        let response_header = FrameHeader {
            key_hint: node.key_hint,
            msg_type: MSG_COMMAND,
            nonce: header.nonce,
        };

        // 3. Route WAKE blob to handler (store reply for NEXT cycle).
        // Spawned as a background task so it does not block COMMAND delivery.
        if let Some(wake_data) = wake_blob {
            if !wake_data.is_empty() && !program_hash.is_empty() {
                let handler_router = Arc::clone(&self.handler_router);
                let deferred_replies = Arc::clone(&self.deferred_replies);
                let node_id = node.node_id.clone();
                let program_hash = program_hash.clone();
                let nonce = header.nonce;
                tokio::spawn(async move {
                    let handler_result = {
                        let router = handler_router.read().await;
                        router.find_handler_cloned(&program_hash)
                    };
                    if let Some((config, process_arc)) = handler_result {
                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let msg = crate::handler::HandlerMessage::Data {
                            request_id: nonce,
                            node_id: node_id.clone(),
                            program_hash: program_hash.clone(),
                            data: wake_data,
                            timestamp,
                        };
                        info!(
                            node_id = %node_id,
                            command = %config.command,
                            "WAKE blob routed to handler"
                        );
                        let mut process = process_arc.lock().await;
                        if let Some(crate::handler::HandlerMessage::DataReply { data, .. }) =
                            process.send_data(&msg).await
                        {
                            if !data.is_empty()
                                && data.len() <= sonde_protocol::MAX_COMMAND_BLOB_SIZE
                            {
                                deferred_replies.write().await.insert(node_id.clone(), data);
                                info!(
                                    node_id = %node_id,
                                    "deferred reply stored from WAKE blob handler response"
                                );
                            } else if data.len() > sonde_protocol::MAX_COMMAND_BLOB_SIZE {
                                warn!(
                                    node_id = %node_id,
                                    len = data.len(),
                                    "WAKE blob handler reply too large for deferred delivery — dropping"
                                );
                            }
                        }
                    }
                });
            }
        }

        Some((response_header, response_cbor, has_deferred))
    }

    /// Handle a WAKE frame — business logic + AES-256-GCM response encoding.
    async fn handle_wake(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
        peer: PeerAddress,
    ) -> Option<Vec<u8>> {
        let (response_header, response_cbor, deferred_delivered) =
            self.handle_wake_core(node, header, payload, peer).await?;
        let frame = self.encode_response(&response_header, &response_cbor, &node.psk)?;
        // Only remove deferred reply if it was actually included in this NOP COMMAND.
        if deferred_delivered {
            self.deferred_replies.write().await.remove(&node.node_id);
            info!(
                node_id = %node.node_id,
                "deferred reply delivered in COMMAND"
            );
        }
        Some(frame)
    }

    /// Handle a post-WAKE message — dispatch + AES-256-GCM encoding.
    async fn handle_post_wake(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Option<Vec<u8>> {
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

        self.session_manager
            .verify_and_advance_seq(&node.node_id, header.nonce)
            .await
            .ok()?;

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

    /// Handle a GET_CHUNK request — AES-256-GCM response encoding.
    async fn handle_get_chunk(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        chunk_index: u32,
    ) -> Option<Vec<u8>> {
        let (response_header, response_cbor) = self
            .handle_get_chunk_core(node, header, chunk_index)
            .await?;
        self.encode_response(&response_header, &response_cbor, &node.psk)
    }

    /// Handle an APP_DATA message — AES-256-GCM response encoding.
    async fn handle_app_data(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        blob: Vec<u8>,
    ) -> Option<Vec<u8>> {
        let (response_header, response_cbor) =
            self.handle_app_data_core(node, header, blob).await?;
        self.encode_response(&response_header, &response_cbor, &node.psk)
    }

    /// Shared GET_CHUNK business logic: look up session/program, serve chunk.
    ///
    /// Returns `(response_header, response_cbor)` for the caller to encode.
    async fn handle_get_chunk_core(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        chunk_index: u32,
    ) -> Option<(FrameHeader, Vec<u8>)> {
        // Look up program transfer state from session
        let session = self.session_manager.get_session(&node.node_id).await?;
        let (program_hash, chunk_size) = match &session.state {
            SessionState::ChunkedTransfer {
                program_hash,
                chunk_size,
                ..
            } => (program_hash.clone(), *chunk_size),
            _ => {
                warn!(
                    node_id = %node.node_id,
                    chunk_index,
                    state = ?session.state,
                    "GET_CHUNK discarded — session not in ChunkedTransfer state"
                );
                return None;
            }
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

        Some((response_header, response_cbor))
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
        {
            let process_refs = self.handler_router.read().await.clone_all_process_refs();
            // Lock released — broadcast events without holding router lock.
            let mut details = BTreeMap::new();
            details.insert(
                "program_hash".to_string(),
                ciborium::Value::Bytes(program_hash),
            );
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let msg = crate::handler::HandlerMessage::Event {
                node_id: node.node_id.clone(),
                event_type: "program_updated".to_string(),
                details,
                timestamp,
            };
            for process_arc in &process_refs {
                let mut process = process_arc.lock().await;
                process.send_event(&msg).await;
            }
        }

        // Transition session from ChunkedTransfer to BpfExecuting
        let _ = self
            .session_manager
            .set_state(&node.node_id, SessionState::BpfExecuting)
            .await;
    }

    /// Shared APP_DATA business logic: route to handler, build reply.
    ///
    /// Returns `(response_header, response_cbor)` for the caller to encode.
    async fn handle_app_data_core(
        &self,
        node: &NodeRecord,
        header: &FrameHeader,
        blob: Vec<u8>,
    ) -> Option<(FrameHeader, Vec<u8>)> {
        // Use the node's `current_program_hash` (set via PROGRAM_ACK) for routing.
        // The node record was already loaded during frame authentication.
        let program_hash = match &node.current_program_hash {
            Some(hash) => hash.clone(),
            None => {
                warn!(
                    node_id = %node.node_id,
                    "APP_DATA dropped: node has no `current_program_hash` \
                     (PROGRAM_ACK never received for this node)"
                );
                return None;
            }
        };

        // Find the matching handler under the read lock, then release before I/O.
        let (config, process_arc) = {
            let router = self.handler_router.read().await;
            match router.find_handler_cloned(&program_hash) {
                Some(result) => result,
                None => {
                    let ph_hex: String = program_hash.iter().map(|b| format!("{b:02x}")).collect();
                    warn!(
                        node_id = %node.node_id,
                        program_hash = %ph_hex,
                        handler_count = router.handler_count(),
                        "APP_DATA dropped: no handler matched `program_hash`"
                    );
                    return None;
                }
            }
        }; // read lock released here

        // GW-1308 AC1: log APP_DATA received with node_id, program_hash, len.
        if tracing::enabled!(tracing::Level::INFO) {
            let ph_hex: String = program_hash.iter().map(|b| format!("{b:02x}")).collect();
            info!(
                node_id = %node.node_id,
                program_hash = %ph_hex,
                len = blob.len(),
                "APP_DATA received"
            );
        }

        // GW-1308 AC2: handler matched with program_hash and command.
        if tracing::enabled!(tracing::Level::INFO) {
            let ph_hex: String = program_hash.iter().map(|b| format!("{b:02x}")).collect();
            info!(
                program_hash = %ph_hex,
                command = %config.command,
                "handler matched"
            );
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // GW-1308 AC3: handler invoked with command.
        info!(command = %config.command, "handler invoked");

        let msg = crate::handler::HandlerMessage::Data {
            request_id: header.nonce,
            node_id: node.node_id.clone(),
            program_hash: program_hash.to_vec(),
            data: blob,
            timestamp,
        };

        let mut process = process_arc.lock().await;
        let reply = process.send_data(&msg).await?;
        match reply {
            crate::handler::HandlerMessage::DataReply { data, delivery, .. } => {
                if data.is_empty() {
                    None
                } else if delivery == 1 {
                    // Deferred delivery: store reply for next WAKE cycle.
                    // Validate that the data would fit in a NOP COMMAND payload.
                    if data.len() > sonde_protocol::MAX_COMMAND_BLOB_SIZE {
                        warn!(
                            node_id = %node.node_id,
                            len = data.len(),
                            max = sonde_protocol::MAX_COMMAND_BLOB_SIZE,
                            "deferred reply too large — dropping"
                        );
                    } else {
                        info!(
                            node_id = %node.node_id,
                            len = data.len(),
                            "handler replied with deferred delivery — storing for next WAKE"
                        );
                        self.deferred_replies
                            .write()
                            .await
                            .insert(node.node_id.clone(), data);
                    }
                    None
                } else {
                    // GW-1308 AC4: handler replied with len.
                    info!(len = data.len(), "handler replied");

                    let response_msg = GatewayMessage::AppDataReply { blob: data };
                    let response_cbor = response_msg.encode().ok()?;

                    let response_header = FrameHeader {
                        key_hint: node.key_hint,
                        msg_type: MSG_APP_DATA_REPLY,
                        nonce: header.nonce,
                    };

                    Some((response_header, response_cbor))
                }
            }
            _ => None,
        }
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
        // Clone handler process refs under the read lock, then release.
        let process_refs = self.handler_router.read().await.clone_all_process_refs();

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
                let msg = crate::handler::HandlerMessage::Event {
                    node_id: node.node_id.clone(),
                    event_type: "node_timeout".to_string(),
                    details,
                    timestamp: now.as_secs(),
                };
                for process_arc in &process_refs {
                    let mut process = process_arc.lock().await;
                    process.send_event(&msg).await;
                }
            }
        }
    }
}
