// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, CommandPayload, DecodedFrame, FrameHeader,
    GatewayMessage, NodeMessage, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_CHUNK, MSG_COMMAND,
    MSG_DIAG_REPLY, MSG_DIAG_REQUEST, MSG_GET_CHUNK, MSG_PEER_ACK, MSG_PEER_REQUEST,
    MSG_PROGRAM_ACK, MSG_WAKE, PEER_ACK_KEY_STATUS, PEER_REQ_KEY_PAYLOAD,
};

use std::collections::BTreeMap;

use crate::connector::{ConnectorEventHub, ConnectorPayloadOrigin};
use crate::crypto::RustCryptoSha256;
use crate::gateway_identity::GatewayIdentity;
use crate::handler::HandlerRouter;
use crate::phone_trust::PhonePskStatus;
use crate::program::ProgramLibrary;
use crate::registry::NodeRecord;
use crate::session::{SessionManager, SessionState};
use crate::sqlite_storage::{decrypt_psk_with_master_key, SqliteStorage};
use crate::storage::Storage;
use crate::transport::PeerAddress;

// ── Missing key_hint tracker (GW-2009) ──────────────────────────────

/// Maximum number of unique key_hints tracked before LRU eviction.
const MISSING_HINT_MAX_ENTRIES: usize = 256;

/// Minimum interval between reports for the same key_hint (seconds).
const MISSING_HINT_RATE_LIMIT_SECS: u64 = 60;

/// Tracks unknown `key_hint` values for reporting in gateway ACTUAL_STATE.
///
/// Bounded dedup set (max 256 entries, LRU eviction). Each key_hint is
/// rate-limited to at most one report per 60 seconds. After draining,
/// reported hints are cleared — the node's wake cycle provides natural retry.
pub struct MissingKeyHintTracker {
    /// Map from key_hint → last_reported time (for rate limiting).
    entries: HashMap<u16, Instant>,
    /// Insertion-order queue for LRU eviction.
    order: Vec<u16>,
    /// Hints ready to be reported in the next ACTUAL_STATE emission.
    pending: Vec<u16>,
}

impl MissingKeyHintTracker {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Record an unknown key_hint. Returns `true` if the hint was accepted
    /// (not rate-limited), `false` if suppressed.
    pub fn report(&mut self, key_hint: u16) -> bool {
        let now = Instant::now();

        if let Some(last) = self.entries.get(&key_hint) {
            if now.duration_since(*last).as_secs() < MISSING_HINT_RATE_LIMIT_SECS {
                // Rate-limited — but still refresh LRU position so a
                // frequently-seen hint isn't evicted while suppressed.
                self.order.retain(|h| *h != key_hint);
                self.order.push(key_hint);
                return false;
            }
            // Rate limit expired — update timestamp and move to back of LRU.
            self.entries.insert(key_hint, now);
            self.order.retain(|h| *h != key_hint);
            self.order.push(key_hint);
            if !self.pending.contains(&key_hint) {
                self.pending.push(key_hint);
            }
            return true;
        }

        // Evict oldest if at capacity.
        if self.entries.len() >= MISSING_HINT_MAX_ENTRIES {
            if let Some(oldest) = self.order.first().copied() {
                self.entries.remove(&oldest);
                self.order.remove(0);
                self.pending.retain(|h| *h != oldest);
            }
        }

        self.entries.insert(key_hint, now);
        self.order.push(key_hint);
        self.pending.push(key_hint);
        true
    }

    /// Drain all pending hints for inclusion in the next ACTUAL_STATE.
    /// After this call, the pending set is empty. Timestamps are preserved
    /// for rate limiting.
    pub fn drain(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.pending)
    }

    /// Return the number of tracked key_hints (for diagnostics).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if no key_hints are being tracked.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for MissingKeyHintTracker {
    fn default() -> Self {
        Self::new()
    }
}

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

/// Resolve the ESP-NOW channel from storage, seeding the database with the
/// CLI-supplied default if no value is persisted yet (GW-0808).
///
/// Returns the channel to use for the modem startup handshake.
///
/// # Errors
///
/// Returns an error if storage I/O fails, if the CLI-supplied default is
/// outside the valid WiFi channel range `1..=14`, if the persisted value is
/// not a valid `u8`, or if the persisted value is outside `1..=14`.
pub async fn resolve_espnow_channel(storage: &dyn Storage, cli_channel: u8) -> Result<u8, String> {
    if !(1..=14).contains(&cli_channel) {
        return Err(format!(
            "invalid CLI espnow_channel `{cli_channel}`: expected 1..=14"
        ));
    }

    match storage
        .get_config("espnow_channel")
        .await
        .map_err(|e| format!("failed to read espnow_channel: {e}"))?
    {
        Some(v) => {
            let channel = v
                .parse::<u8>()
                .map_err(|e| format!("invalid persisted espnow_channel `{v}`: {e}"))?;
            if !(1..=14).contains(&channel) {
                return Err(format!(
                    "persisted espnow_channel `{channel}` out of range: expected 1..=14"
                ));
            }
            Ok(channel)
        }
        None => {
            storage
                .set_config("espnow_channel", &cli_channel.to_string())
                .await
                .map_err(|e| format!("failed to seed espnow_channel: {e}"))?;
            Ok(cli_channel)
        }
    }
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
    /// Deferred handler replies awaiting delivery on the next WAKE cycle.
    deferred_replies: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Live event publication for connector processes.
    connector_event_hub: Arc<ConnectorEventHub>,
    /// Tracks unknown key_hints for reporting in ACTUAL_STATE (GW-2009).
    missing_hint_tracker: Arc<RwLock<MissingKeyHintTracker>>,
    /// Signalled when `MissingKeyHintTracker::report()` accepts a new hint,
    /// allowing the ACTUAL_STATE re-emission task to debounce and re-emit
    /// (GW-2003 AC-6, §23.14).
    missing_hints_notify: Arc<tokio::sync::Notify>,
    /// Typed storage reference for pending_recovery access (GW-2009).
    sqlite_storage: Option<Arc<SqliteStorage>>,
    /// Cached master_key_id for pending_recovery lookups (GW-2009).
    /// Loaded once at `set_sqlite_storage` time to avoid calling
    /// `init_master_key_id` on the per-frame path.
    cached_master_key_id: Option<[u8; 32]>,
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
            deferred_replies: Arc::new(RwLock::new(HashMap::new())),
            connector_event_hub: Arc::new(ConnectorEventHub::default()),
            missing_hint_tracker: Arc::new(RwLock::new(MissingKeyHintTracker::new())),
            missing_hints_notify: Arc::new(tokio::sync::Notify::new()),
            sqlite_storage: None,
            cached_master_key_id: None,
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
            deferred_replies: Arc::new(RwLock::new(HashMap::new())),
            connector_event_hub: Arc::new(ConnectorEventHub::default()),
            missing_hint_tracker: Arc::new(RwLock::new(MissingKeyHintTracker::new())),
            missing_hints_notify: Arc::new(tokio::sync::Notify::new()),
            sqlite_storage: None,
            cached_master_key_id: None,
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
            deferred_replies: Arc::new(RwLock::new(HashMap::new())),
            connector_event_hub: Arc::new(ConnectorEventHub::default()),
            missing_hint_tracker: Arc::new(RwLock::new(MissingKeyHintTracker::new())),
            missing_hints_notify: Arc::new(tokio::sync::Notify::new()),
            sqlite_storage: None,
            cached_master_key_id: None,
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

    /// Return a clone of the connector event hub.
    pub fn connector_event_hub(&self) -> Arc<ConnectorEventHub> {
        Arc::clone(&self.connector_event_hub)
    }

    /// Set the typed SQLite storage reference for pending_recovery access (GW-2009).
    ///
    /// Loads and caches the current `master_key_id` so `try_recovery_auth` does
    /// not need to call `init_master_key_id` on the per-frame path.
    ///
    /// This must be called before `process_frame` can perform trial authentication
    /// against `pending_recovery` candidates.
    pub async fn set_sqlite_storage(&mut self, storage: Arc<SqliteStorage>) {
        match storage.init_master_key_id().await {
            Ok((kid, _)) => {
                self.cached_master_key_id = Some(kid);
            }
            Err(e) => {
                warn!("failed to cache master_key_id during gateway init: {e}");
                // Recovery will be unavailable until master_key_id is initialized.
            }
        }
        self.sqlite_storage = Some(storage);
    }

    /// Drain accumulated unknown `key_hint` values for ACTUAL_STATE emission.
    ///
    /// Returns the hints collected since the last drain and clears the
    /// pending set. Rate-limit timestamps are preserved.
    pub async fn drain_missing_hints(&self) -> Vec<u16> {
        self.missing_hint_tracker.write().await.drain()
    }

    /// Return a clone of the `Notify` used to signal when the
    /// `MissingKeyHintTracker` accepts a new hint (GW-2003 AC-6, §23.14).
    pub fn missing_hints_notify(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.missing_hints_notify)
    }

    /// Record an unknown `key_hint` and notify the ACTUAL_STATE re-emission
    /// task if the hint was accepted (not rate-limited).
    async fn report_missing_hint(&self, key_hint: u16) {
        if self.missing_hint_tracker.write().await.report(key_hint) {
            self.missing_hints_notify.notify_one();
        }
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
            // No known node for this key_hint — attempt trial recovery (GW-2009).
            return self.try_recovery_auth(&decoded, key_hint, peer, rssi).await;
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
                self.handle_wake(&node, &decoded.header, &payload, peer, rssi)
                    .await
            }
            MSG_GET_CHUNK | MSG_PROGRAM_ACK | MSG_APP_DATA => {
                self.handle_post_wake(&node, &decoded.header, &payload)
                    .await
            }
            _ => None,
        }
    }

    /// Attempt trial authentication against `pending_recovery` candidates (GW-2009).
    ///
    /// Called when no known node matches the frame's `key_hint`. If a pending
    /// recovery PSK decrypts the frame, the node is promoted to the `nodes` table
    /// and the frame is processed normally. Otherwise the `key_hint` is recorded
    /// for reporting in the next gateway ACTUAL_STATE.
    async fn try_recovery_auth(
        &self,
        decoded: &DecodedFrame<'_>,
        key_hint: u16,
        peer: PeerAddress,
        rssi: Option<i8>,
    ) -> Option<Vec<u8>> {
        use crate::aead::GatewayAead;

        let sqlite = match &self.sqlite_storage {
            Some(s) => Arc::clone(s),
            None => {
                // No SQLite storage — can't do recovery. Record and discard.
                self.report_missing_hint(key_hint).await;
                warn!(
                    key_hint,
                    "discarding frame from unknown node (no recovery storage)"
                );
                return None;
            }
        };

        // Use cached master_key_id for pre-filtering candidates.
        let current_key_id = match self.cached_master_key_id {
            Some(kid) => kid,
            None => {
                warn!(key_hint, "no cached master_key_id — recovery unavailable");
                self.report_missing_hint(key_hint).await;
                return None;
            }
        };

        // Cap candidates per frame to bound AES-GCM work.
        const MAX_TRIAL_CANDIDATES: u32 = 8;

        let recovery_candidates = match sqlite
            .lookup_pending_recovery_filtered(key_hint, &current_key_id, MAX_TRIAL_CANDIDATES)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                warn!(key_hint, "pending_recovery lookup failed: {e}");
                self.report_missing_hint(key_hint).await;
                return None;
            }
        };

        if recovery_candidates.is_empty() {
            // No recovery candidates — record the hint and discard.
            self.report_missing_hint(key_hint).await;
            warn!(
                key_hint,
                "discarding frame from unknown node (no pending_recovery match)"
            );
            return None;
        }

        let master_key = sqlite.master_key();
        let aead = GatewayAead;
        let mut promoted_node: Option<NodeRecord> = None;
        let mut plaintext: Option<Vec<u8>> = None;
        let mut promoted_node_id: Option<String> = None;

        for candidate in &recovery_candidates {
            // Decrypt the escrowed PSK with the master key into zeroized memory.
            let psk = match decrypt_psk_with_master_key(
                &master_key,
                &candidate.node_id,
                &candidate.encrypted_psk,
            ) {
                Ok(psk) => psk,
                Err(e) => {
                    warn!(
                        node_id = %candidate.node_id,
                        key_hint,
                        "recovery PSK decryption failed: {e}"
                    );
                    continue;
                }
            };

            // Trial-decrypt the frame with the recovered PSK.
            if let Ok(pt) = open_frame(decoded, &psk, &aead, &self.crypto_sha) {
                info!(
                    node_id = %candidate.node_id,
                    key_hint,
                    "trial authentication succeeded — promoting node"
                );
                let node = NodeRecord::new(candidate.node_id.clone(), key_hint, *psk);
                promoted_node_id = Some(candidate.node_id.clone());
                promoted_node = Some(node);
                plaintext = Some(pt);
                break;
            }
        }

        let node = match promoted_node {
            Some(n) => n,
            None => {
                // All candidates failed — record hint for reporting.
                self.report_missing_hint(key_hint).await;
                debug!(
                    key_hint,
                    candidates = recovery_candidates.len(),
                    "trial authentication failed for all pending_recovery candidates"
                );
                return None;
            }
        };
        let payload = plaintext?;
        let promoted_id = promoted_node_id?;

        // Promote: upsert into nodes table, delete from pending_recovery.
        if let Err(e) = self.storage.upsert_node(&node).await {
            warn!(node_id = %promoted_id, "failed to promote recovered node: {e}");
            return None;
        }
        if let Err(e) = sqlite.delete_pending_recovery(key_hint, &promoted_id).await {
            warn!(node_id = %promoted_id, "failed to delete pending_recovery after promotion: {e}");
            // Continue processing — the node is already promoted.
        }

        // Process the frame normally.
        match decoded.header.msg_type {
            MSG_WAKE => {
                self.handle_wake(&node, &decoded.header, &payload, peer, rssi)
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

        // Step 4: Capture RSSI (GW-1702).
        let rssi_dbm = match rssi {
            Some(r) => r,
            None => {
                warn!("RSSI unavailable for DIAG_REQUEST, using sentinel (GW-1702)");
                0i8
            }
        };

        // Step 5: Build DIAG_REPLY (GW-1704).
        let reply = GatewayMessage::DiagReply {
            diagnostic_type,
            rssi_dbm,
        };
        let reply_cbor = reply.encode().ok()?;

        // Echo the request nonce (GW-1704).
        let reply_header = FrameHeader {
            key_hint,
            msg_type: MSG_DIAG_REPLY,
            nonce: decoded.header.nonce,
        };

        let frame = self.encode_response(&reply_header, &reply_cbor, &matched_phone.psk)?;

        info!(rssi_dbm, peer = ?peer, "DIAG_REPLY sent (GW-1706)");

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
        rssi: Option<i8>,
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
            wake_rssi_dbm = ?rssi,
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

        // 4. Update durable firmware metadata and runtime observations.
        let metadata_persisted = match self
            .storage
            .update_node_wake_metadata(&node.node_id, firmware_abi_version, &firmware_version)
            .await
        {
            Ok(()) => true,
            Err(crate::storage::StorageError::NotFound(_)) => {
                warn!(
                    node_id = %node.node_id,
                    "dropping WAKE runtime observation because node disappeared before metadata update",
                );
                self.session_manager.clear_last_seen(&node.node_id).await;
                self.session_manager.clear_battery_mv(&node.node_id).await;
                self.session_manager
                    .clear_wake_rssi_dbm(&node.node_id)
                    .await;
                false
            }
            Err(e) => {
                warn!(
                    node_id = %node.node_id,
                    error = %e,
                    "failed to persist WAKE firmware metadata",
                );
                false
            }
        };
        let mut updated_node = node.clone();
        updated_node.update_firmware_metadata(firmware_abi_version, firmware_version);
        if metadata_persisted {
            let observed_at = SystemTime::now();
            self.session_manager
                .record_last_seen(&node.node_id, observed_at)
                .await;
            self.session_manager
                .record_battery_mv(&node.node_id, battery_mv)
                .await;
            if let Some(rssi_val) = rssi {
                self.session_manager
                    .record_wake_rssi_dbm(&node.node_id, rssi_val)
                    .await;
            }
        }

        self.connector_event_hub.emit_actual_state_for_node(
            node.node_id.clone(),
            program_hash.clone(),
            updated_node.assigned_program_hash.clone(),
            updated_node.schedule_interval_s,
            battery_mv,
            firmware_abi_version,
            updated_node.firmware_version.clone().unwrap_or_default(),
            timestamp_ms,
            rssi,
        );

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

        // ── Lost-PROGRAM_ACK recovery (#961) ────────────────────────────
        //
        // If the node reports a program hash in its WAKE that matches the
        // assigned hash but differs from the stored `current_program_hash`,
        // the most likely explanation is a lost PROGRAM_ACK: the node
        // installed the program successfully but the ACK was dropped over
        // the radio.  Reconcile the stored hash so that downstream paths
        // (decoder enrichment, handler routing) use the correct program.
        //
        // The conditional storage update is atomic: it only writes
        // `current_program_hash` if `assigned_program_hash` still equals
        // the WAKE-reported hash, preventing clobber of concurrent
        // reassignments.
        if let Some(assigned) = &node.assigned_program_hash {
            if assigned.as_slice() == program_hash.as_slice()
                && node.current_program_hash.as_deref() != Some(program_hash.as_slice())
            {
                match self
                    .storage
                    .reconcile_current_program_hash(&node.node_id, &program_hash)
                    .await
                {
                    Ok(true) => {
                        let ph_hex: String =
                            program_hash.iter().map(|b| format!("{b:02x}")).collect();
                        info!(
                            node_id = %node.node_id,
                            program_hash = %ph_hex,
                            "WAKE reconciliation: node reports assigned program \
                             — updated `current_program_hash` (lost PROGRAM_ACK recovery)"
                        );
                    }
                    Ok(false) => {
                        debug!(
                            node_id = %node.node_id,
                            "WAKE reconciliation skipped: condition no longer holds \
                             (concurrent reassignment or already reconciled)"
                        );
                    }
                    Err(e) => {
                        warn!(
                            node_id = %node.node_id,
                            error = %e,
                            "WAKE reconciliation: failed to persist current_program_hash"
                        );
                    }
                }
            }
        }

        // 6. Encode GatewayMessage::Command response
        // Peek at deferred reply for NOP commands; remove only after successful AEAD.
        let command_blob = if matches!(command_payload, CommandPayload::Nop) {
            self.deferred_replies
                .read()
                .await
                .get(&node.node_id)
                .cloned()
        } else {
            None
        };
        let has_deferred = command_blob.is_some();
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
                // ── Decoder enrichment for WAKE blobs (GW-1903 parity) ──────
                //
                // Run the same decoder enrichment as the APP_DATA path so that
                // both the connector and handler receive decoded readings
                // regardless of whether the data arrived via WAKE blob or
                // APP_DATA.
                let readings = {
                    let decoder_image = match self.storage.get_program(&program_hash).await {
                        Ok(Some(record)) => record.decoder_image,
                        Ok(None) => None,
                        Err(e) => {
                            warn!(error = %e, "failed to look up program record for WAKE blob decoder — forwarding unenriched");
                            None
                        }
                    };
                    if let Some(ref decoder_cbor) = decoder_image {
                        let decoder_cbor = decoder_cbor.clone();
                        let blob_clone = wake_data.clone();
                        match tokio::task::spawn_blocking(move || {
                            // SAFETY: decoder_cbor was produced by Prevail-verified
                            // `extract_decoder` during ELF ingestion and stored in
                            // ProgramRecord. It has not been modified since verification.
                            unsafe { crate::decoder::execute_decoder(&decoder_cbor, &blob_clone) }
                        })
                        .await
                        {
                            Ok(Ok(r)) if !r.is_empty() => {
                                info!(
                                    node_id = %node.node_id,
                                    reading_count = r.len(),
                                    readings = ?r,
                                    "decoder enriched WAKE blob"
                                );
                                Some(r)
                            }
                            Ok(Ok(_)) => None,
                            Ok(Err(e)) => {
                                warn!(error = %e, "WAKE blob decoder execution failed — forwarding unenriched");
                                None
                            }
                            Err(e) => {
                                if e.is_panic() {
                                    warn!(error = %e, "WAKE blob decoder task panicked — forwarding unenriched");
                                } else {
                                    warn!(error = %e, "WAKE blob decoder task cancelled — forwarding unenriched");
                                }
                                None
                            }
                        }
                    } else {
                        None
                    }
                };

                let handler_result = {
                    let router = self.handler_router.read().await;
                    (
                        router.find_handler_cloned(&program_hash),
                        router.handler_count(),
                    )
                };
                let node_id = node.node_id.clone();
                let program_hash = program_hash.clone();
                match handler_result {
                    (Some((config, process_arc)), _) => {
                        self.connector_event_hub.emit_app_data(
                            node_id.clone(),
                            program_hash.clone(),
                            wake_data.clone(),
                            timestamp_ms,
                            ConnectorPayloadOrigin::WakeBlob,
                            readings.clone(),
                        );
                        let deferred_replies = Arc::clone(&self.deferred_replies);
                        let nonce = header.nonce;
                        tokio::spawn(async move {
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
                                readings,
                            };
                            info!(
                                node_id = %node_id,
                                command = %config.command,
                                "WAKE blob routed to handler"
                            );
                            let mut process = process_arc.lock().await;
                            if let Some(crate::handler::HandlerMessage::DataReply {
                                data, ..
                            }) = process.send_data(&msg).await
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
                        });
                    }
                    (None, handler_count) => {
                        self.connector_event_hub.emit_app_data(
                            node_id.clone(),
                            program_hash.clone(),
                            wake_data.clone(),
                            timestamp_ms,
                            ConnectorPayloadOrigin::WakeBlob,
                            readings,
                        );
                        let connector_subscribers = self.connector_event_hub.subscriber_count();
                        let ph_hex: String =
                            program_hash.iter().map(|b| format!("{b:02x}")).collect();
                        if connector_subscribers > 0 {
                            debug!(
                                node_id = %node_id,
                                program_hash = %ph_hex,
                                handler_count,
                                connector_subscribers,
                                "WAKE blob: no handler matched `program_hash` (forwarded to connector)"
                            );
                        } else {
                            warn!(
                                node_id = %node_id,
                                program_hash = %ph_hex,
                                handler_count,
                                "WAKE blob dropped: no handler matched `program_hash`"
                            );
                        }
                    }
                }
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
        rssi: Option<i8>,
    ) -> Option<Vec<u8>> {
        let (response_header, response_cbor, deferred_delivered) = self
            .handle_wake_core(node, header, payload, peer, rssi)
            .await?;
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

        let now_duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let timestamp = now_duration.as_secs();
        let timestamp_ms = now_duration.as_millis() as u64;

        // ── Decoder enrichment (GW-1903) ────────────────────────────────
        //
        // Run decoder before handler routing so that both the connector and
        // handler receive the same enriched readings (GW-1903 AC-6), even
        // when no handler is registered.
        let readings = {
            let decoder_image = match self.storage.get_program(&program_hash).await {
                Ok(Some(record)) => record.decoder_image,
                Ok(None) => None,
                Err(e) => {
                    warn!(error = %e, "failed to look up program record for decoder — forwarding unenriched");
                    None
                }
            };
            if let Some(ref decoder_cbor) = decoder_image {
                let decoder_cbor = decoder_cbor.clone();
                let blob_clone = blob.clone();
                match tokio::task::spawn_blocking(move || {
                    // SAFETY: decoder_cbor was produced by Prevail-verified
                    // `extract_decoder` during ELF ingestion and stored in
                    // ProgramRecord. It has not been modified since verification.
                    unsafe { crate::decoder::execute_decoder(&decoder_cbor, &blob_clone) }
                })
                .await
                {
                    Ok(Ok(r)) if !r.is_empty() => {
                        info!(
                            node_id = %node.node_id,
                            reading_count = r.len(),
                            readings = ?r,
                            "decoder enriched APP_DATA"
                        );
                        Some(r)
                    }
                    Ok(Ok(_)) => None,
                    Ok(Err(e)) => {
                        warn!(error = %e, "decoder execution failed — forwarding unenriched");
                        None
                    }
                    Err(e) => {
                        if e.is_panic() {
                            warn!(error = %e, "decoder task panicked — forwarding unenriched");
                        } else {
                            warn!(error = %e, "decoder task cancelled — forwarding unenriched");
                        }
                        None
                    }
                }
            } else {
                None
            }
        };

        // Find the matching handler under the read lock, then release before I/O.
        let handler_result = {
            let router = self.handler_router.read().await;
            match router.find_handler_cloned(&program_hash) {
                Some(result) => Ok(result),
                None => Err(router.handler_count()),
            }
        }; // read lock released here

        // Always emit to connector with enriched readings (GW-1903 AC-6).
        self.connector_event_hub.emit_app_data(
            node.node_id.clone(),
            program_hash.clone(),
            blob.clone(),
            timestamp_ms,
            ConnectorPayloadOrigin::AppData,
            readings.clone(),
        );

        let (config, process_arc) = match handler_result {
            Ok(result) => result,
            Err(handler_count) => {
                let connector_subscribers = self.connector_event_hub.subscriber_count();
                let ph_hex: String = program_hash.iter().map(|b| format!("{b:02x}")).collect();
                if connector_subscribers > 0 {
                    debug!(
                        node_id = %node.node_id,
                        program_hash = %ph_hex,
                        handler_count,
                        connector_subscribers,
                        "APP_DATA: no handler matched `program_hash` (forwarded to connector)"
                    );
                } else {
                    warn!(
                        node_id = %node.node_id,
                        program_hash = %ph_hex,
                        handler_count,
                        "APP_DATA dropped: no handler matched `program_hash`"
                    );
                }
                return None;
            }
        };

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

        // GW-1308 AC3: handler invoked with command.
        info!(command = %config.command, "handler invoked");

        let msg = crate::handler::HandlerMessage::Data {
            request_id: header.nonce,
            node_id: node.node_id.clone(),
            program_hash: program_hash.to_vec(),
            data: blob,
            timestamp,
            readings,
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
    /// has elapsed since its runtime `last_seen` timestamp (default multiplier: 3,
    /// per gateway-design.md). Call this periodically from the gateway main
    /// loop.
    pub async fn check_node_timeouts(&self, multiplier: u64) {
        // Clone handler process refs under the read lock, then release.
        let process_refs = self.handler_router.read().await.clone_all_process_refs();

        let multiplier = if multiplier == 0 { 3 } else { multiplier };

        let nodes = self.storage.list_nodes().await.unwrap_or_default();
        let last_seen_by_node = self.session_manager.snapshot_last_seen().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        for node in &nodes {
            let interval = node.schedule_interval_s as u64;
            if interval == 0 {
                continue;
            }

            let last_seen = match last_seen_by_node.get(&node.node_id).copied() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn report_missing_hint_fires_notify() {
        let storage: Arc<dyn Storage> = Arc::new(crate::storage::InMemoryStorage::new());
        let gw = Gateway::new(storage, Duration::from_secs(300));
        let notify = gw.missing_hints_notify();

        // Report a new hint — should fire notify.
        gw.report_missing_hint(0x1234).await;

        // Verify notification is pending (non-blocking check).
        tokio::time::timeout(Duration::from_millis(50), notify.notified())
            .await
            .expect("notify should have been signalled for new hint");
    }

    #[tokio::test]
    async fn report_missing_hint_rate_limited_does_not_fire() {
        let storage: Arc<dyn Storage> = Arc::new(crate::storage::InMemoryStorage::new());
        let gw = Gateway::new(storage, Duration::from_secs(300));
        let notify = gw.missing_hints_notify();

        // First report — accepted.
        gw.report_missing_hint(0xABCD).await;
        // Consume the notification.
        tokio::time::timeout(Duration::from_millis(50), notify.notified())
            .await
            .expect("first report should fire notify");

        // Second report of same hint — rate-limited.
        gw.report_missing_hint(0xABCD).await;
        // Should NOT fire.
        let result = tokio::time::timeout(Duration::from_millis(50), notify.notified()).await;
        assert!(result.is_err(), "rate-limited hint should not fire notify");
    }

    #[tokio::test]
    async fn drain_missing_hints_returns_reported() {
        let storage: Arc<dyn Storage> = Arc::new(crate::storage::InMemoryStorage::new());
        let gw = Gateway::new(storage, Duration::from_secs(300));

        gw.report_missing_hint(0x1111).await;
        gw.report_missing_hint(0x2222).await;

        let hints = gw.drain_missing_hints().await;
        assert_eq!(hints.len(), 2);
        assert!(hints.contains(&0x1111));
        assert!(hints.contains(&0x2222));

        // Second drain should be empty.
        let empty = gw.drain_missing_hints().await;
        assert!(empty.is_empty());
    }
}
