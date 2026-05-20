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

use crate::admin::{system_time_to_millis, validate_program_hash_bytes};
use crate::engine::PendingCommand;
use crate::program::{ProgramLibrary, VerificationProfile};
use crate::storage::Storage;

/// Inbound escrow messages received from the control plane.
#[derive(Debug)]
pub enum EscrowInboundMessage {
    /// KEY_ESCROW_RESPONSE (msg_type 0x12) — raw CBOR bytes.
    /// RETIRED by evolve-962 — replaced by `recovered_psks` in gateway DESIRED_STATE.
    KeyEscrowResponse(Vec<u8>),
    /// MASTER_KEY_INSTALL (msg_type 0x13) — raw CBOR bytes.
    /// RETIRED by evolve-962 — replaced by `rotation_payload` in gateway DESIRED_STATE.
    MasterKeyInstall(Vec<u8>),
}

/// Parsed gateway DESIRED_STATE fields from evolve-962 §2.5.
#[derive(Clone, Debug, Default)]
pub struct GatewayDesiredState {
    /// Gateway entity_id (hex-encoded gateway_id).
    pub entity_id: String,
    /// Desired ESP-NOW channel (CBOR key 15).
    pub channel: Option<u32>,
    /// KDF salt from cloud (CBOR key 21, 16 bytes).
    pub salt: Option<Vec<u8>>,
    /// KDF parameters from cloud (CBOR key 22).
    pub kdf_params: Option<KdfParams>,
    /// X25519-encrypted rotation payload (CBOR key 28).
    pub rotation_payload: Option<Vec<u8>>,
    /// Recovered PSK records for missing nodes (CBOR key 29).
    pub recovered_psks: Option<Vec<RecoveredPskRecord>>,
}

/// A single recovered PSK record from DESIRED_STATE (evolve-962 §2.8).
#[derive(Clone, Debug)]
pub struct RecoveredPskRecord {
    /// Node identifier.
    pub node_id: String,
    /// Key hint (u16).
    pub key_hint: u16,
    /// Encrypted PSK blob (60 bytes).
    pub encrypted_psk: Vec<u8>,
    /// Opaque master key ID (16 bytes).
    pub master_key_id: Vec<u8>,
}

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
        schedule_interval_s: Option<u32>,
        battery_mv: Option<u32>,
        firmware_abi_version: Option<u32>,
        firmware_version: Option<String>,
        timestamp_ms: u64,
        /// Encrypted PSK blob (60 bytes: nonce + ciphertext + tag), CBOR key 12.
        encrypted_psk_escrow: Option<Vec<u8>>,
        /// Key hint (u16) for Azure recovery lookup, CBOR key 13.
        escrow_key_hint: Option<u16>,
        /// Opaque master key ID (16 bytes) that encrypted this PSK, CBOR key 14.
        master_key_id: Option<Vec<u8>>,
        /// Gateway-scoped status_details extension (escrow state + salt).
        status_details: Option<StatusDetails>,
    },
    AppData {
        node_id: String,
        program_hash: Vec<u8>,
        payload: Vec<u8>,
        timestamp_ms: u64,
        payload_origin: ConnectorPayloadOrigin,
        /// Decoded sensor readings from decoder BPF execution (GW-1903).
        readings: Option<std::collections::BTreeMap<String, i64>>,
    },
    Health {
        health_state: ConnectorHealthState,
        timestamp_ms: u64,
        failure_mode: String,
        stale_scope: Vec<String>,
        remediation: String,
    },
    /// Recovery public key publication (GW-2001).
    /// RETIRED by evolve-962 — replaced by gateway ACTUAL_STATE field
    /// `x25519_public_key` (key 18). Kept temporarily for compilation
    /// until all callers are migrated.
    KeyEscrowPubkey {
        public_key: [u8; 32],
        key_epoch: u64,
        created_at: u64,
        fingerprint_words: [String; 6],
    },
    /// Request escrowed PSK(s) for an unknown key_hint (GW-2009).
    /// RETIRED by evolve-962 — replaced by gateway ACTUAL_STATE field
    /// `missing_key_hints` (key 20). Kept temporarily for compilation
    /// until all callers are migrated.
    KeyEscrowRequest { key_hint: u16, request_id: [u8; 16] },
    /// Gateway-scoped ACTUAL_STATE with all evolve-962 fields (keys 15–27).
    GatewayActualState {
        entity_id: String,
        timestamp_ms: u64,
        channel: u32,
        master_key_id: [u8; 16],
        master_key_epoch: u64,
        x25519_public_key: [u8; 32],
        fingerprint_words: [String; 6],
        missing_key_hints: Vec<u16>,
        salt: Option<Vec<u8>>,
        kdf_params: Option<KdfParams>,
        gateway_version: String,
        gateway_commit: String,
        modem_firmware_version: Option<String>,
        modem_firmware_commit: Option<String>,
        rotation_in_progress: bool,
    },
}

/// Gateway-scoped ACTUAL_STATE status_details for escrow.
/// RETIRED by evolve-962 — replaced by GatewayActualState variant.
/// Kept temporarily for compilation until all callers are migrated.
#[derive(Clone, Debug, Default)]
pub struct StatusDetails {
    pub escrow_state: Option<String>,
    pub escrow_key_version: Option<u64>,
    pub escrow_salt: Option<Vec<u8>>,
    pub escrow_kdf_params: Option<KdfParams>,
}

/// KDF parameters for Argon2id.
#[derive(Clone, Debug)]
pub struct KdfParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub kdf_version: u32,
}

impl ConnectorOutboundMessage {
    fn encode(&self) -> Result<Vec<u8>, String> {
        let message = match self {
            Self::ActualState {
                entity_kind,
                entity_id,
                current_program_hash,
                assigned_program_hash,
                schedule_interval_s,
                battery_mv,
                firmware_abi_version,
                firmware_version,
                timestamp_ms,
                encrypted_psk_escrow,
                escrow_key_hint,
                master_key_id,
                status_details,
            } => {
                let mut pairs = vec![
                    map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
                    map_entry(2, Value::Text((*entity_kind).to_string())),
                    map_entry(3, Value::Text(entity_id.clone())),
                    map_entry(4, opt_bytes_value(current_program_hash.as_deref())),
                    map_entry(5, opt_bytes_value(assigned_program_hash.as_deref())),
                    map_entry(6, opt_u32_value(*battery_mv)),
                    map_entry(7, opt_u32_value(*firmware_abi_version)),
                    map_entry(8, opt_text_value(firmware_version.as_deref())),
                    map_entry(9, Value::Integer((*timestamp_ms).into())),
                ];

                // Build status_details map (key 10)
                let mut sd_pairs = Vec::new();
                if let Some(ref sd) = status_details {
                    if let Some(ref state_str) = sd.escrow_state {
                        sd_pairs.push(map_entry(1, Value::Text(state_str.clone())));
                    }
                    if let Some(kv) = sd.escrow_key_version {
                        sd_pairs.push(map_entry(2, Value::Integer(kv.into())));
                    }
                    if let Some(ref salt) = sd.escrow_salt {
                        sd_pairs.push(map_entry(3, Value::Bytes(salt.clone())));
                    }
                    if let Some(ref kdf) = sd.escrow_kdf_params {
                        let kdf_map = Value::Map(vec![
                            map_entry(1, Value::Integer((kdf.m_cost as u64).into())),
                            map_entry(2, Value::Integer((kdf.t_cost as u64).into())),
                            map_entry(3, Value::Integer((kdf.p_cost as u64).into())),
                            map_entry(4, Value::Integer((kdf.kdf_version as u64).into())),
                        ]);
                        sd_pairs.push(map_entry(4, kdf_map));
                    }
                }
                pairs.push(map_entry(10, Value::Map(sd_pairs)));

                pairs.push(map_entry(11, opt_u32_value(*schedule_interval_s)));

                // Escrow fields (keys 12, 13, 14) — GW-2003
                pairs.push(map_entry(
                    12,
                    match encrypted_psk_escrow {
                        Some(ref blob) => Value::Bytes(blob.clone()),
                        None => Value::Null,
                    },
                ));
                pairs.push(map_entry(
                    13,
                    match escrow_key_hint {
                        Some(kh) => Value::Integer((*kh as u64).into()),
                        None => Value::Null,
                    },
                ));
                pairs.push(map_entry(
                    14,
                    match master_key_id {
                        Some(ref id) => Value::Bytes(id.clone()),
                        None => Value::Null,
                    },
                ));

                Value::Map(pairs)
            }
            Self::AppData {
                node_id,
                program_hash,
                payload,
                timestamp_ms,
                payload_origin,
                readings,
            } => {
                let mut pairs = vec![
                    map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
                    map_entry(2, Value::Text(node_id.clone())),
                    map_entry(3, Value::Bytes(program_hash.clone())),
                    map_entry(4, Value::Bytes(payload.clone())),
                    map_entry(5, Value::Integer((*timestamp_ms).into())),
                    map_entry(6, Value::Text(payload_origin.as_str().to_string())),
                ];
                // GW-1903: append readings at CBOR key 16 when present.
                if let Some(ref readings_map) = readings {
                    let readings_cbor = Value::Map(
                        readings_map
                            .iter()
                            .map(|(k, v)| (Value::Text(k.clone()), Value::Integer((*v).into())))
                            .collect(),
                    );
                    pairs.push(map_entry(16, readings_cbor));
                }
                Value::Map(pairs)
            }
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
            Self::KeyEscrowPubkey {
                public_key,
                key_epoch,
                created_at,
                fingerprint_words,
            } => Value::Map(vec![
                map_entry(
                    1,
                    Value::Integer(sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_PUBKEY.into()),
                ),
                map_entry(2, Value::Bytes(public_key.to_vec())),
                map_entry(3, Value::Integer((*key_epoch).into())),
                map_entry(4, Value::Integer((*created_at).into())),
                map_entry(
                    5,
                    Value::Array(fingerprint_words.iter().cloned().map(Value::Text).collect()),
                ),
            ]),
            Self::KeyEscrowRequest {
                key_hint,
                request_id,
            } => Value::Map(vec![
                map_entry(
                    1,
                    Value::Integer(sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_REQUEST.into()),
                ),
                map_entry(2, Value::Integer((*key_hint as u64).into())),
                map_entry(3, Value::Bytes(request_id.to_vec())),
            ]),
            Self::GatewayActualState {
                entity_id,
                timestamp_ms,
                channel,
                master_key_id,
                master_key_epoch,
                x25519_public_key,
                fingerprint_words,
                missing_key_hints,
                salt,
                kdf_params,
                gateway_version,
                gateway_commit,
                modem_firmware_version,
                modem_firmware_commit,
                rotation_in_progress,
            } => {
                let mut pairs = vec![
                    map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
                    map_entry(2, Value::Text("gateway".to_string())),
                    map_entry(3, Value::Text(entity_id.clone())),
                    map_entry(9, Value::Integer((*timestamp_ms).into())),
                    // Keys 15–27: gateway-specific fields
                    map_entry(15, Value::Integer((*channel as u64).into())),
                    map_entry(16, Value::Bytes(master_key_id.to_vec())),
                    map_entry(17, Value::Integer((*master_key_epoch).into())),
                    map_entry(18, Value::Bytes(x25519_public_key.to_vec())),
                    map_entry(
                        19,
                        Value::Array(fingerprint_words.iter().cloned().map(Value::Text).collect()),
                    ),
                    map_entry(
                        20,
                        Value::Array(
                            missing_key_hints
                                .iter()
                                .map(|h| Value::Integer((*h as u64).into()))
                                .collect(),
                        ),
                    ),
                ];
                // Salt (key 21): null if absent
                pairs.push(map_entry(
                    21,
                    match salt {
                        Some(ref s) => Value::Bytes(s.clone()),
                        None => Value::Null,
                    },
                ));
                // KDF params (key 22): null if absent
                pairs.push(map_entry(
                    22,
                    match kdf_params {
                        Some(ref kdf) => Value::Map(vec![
                            map_entry(1, Value::Integer((kdf.m_cost as u64).into())),
                            map_entry(2, Value::Integer((kdf.t_cost as u64).into())),
                            map_entry(3, Value::Integer((kdf.p_cost as u64).into())),
                            map_entry(4, Value::Integer((kdf.kdf_version as u64).into())),
                        ]),
                        None => Value::Null,
                    },
                ));
                pairs.push(map_entry(23, Value::Text(gateway_version.clone())));
                pairs.push(map_entry(24, Value::Text(gateway_commit.clone())));
                pairs.push(map_entry(
                    25,
                    opt_text_value(modem_firmware_version.as_deref()),
                ));
                pairs.push(map_entry(
                    26,
                    opt_text_value(modem_firmware_commit.as_deref()),
                ));
                pairs.push(map_entry(27, Value::Bool(*rotation_in_progress)));
                Value::Map(pairs)
            }
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

    /// Number of active connector subscribers (companion processes).
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_actual_state_for_node(
        &self,
        node_id: String,
        current_program_hash: Vec<u8>,
        assigned_program_hash: Option<Vec<u8>>,
        schedule_interval_s: u32,
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
            schedule_interval_s: Some(schedule_interval_s),
            battery_mv: Some(battery_mv),
            firmware_abi_version: Some(firmware_abi_version),
            firmware_version: Some(firmware_version),
            timestamp_ms,
            encrypted_psk_escrow: None,
            escrow_key_hint: None,
            master_key_id: None,
            status_details: None,
        });
    }

    /// Emit ACTUAL_STATE for a node with escrow blob (GW-2003).
    #[allow(clippy::too_many_arguments)]
    pub fn emit_actual_state_for_node_with_escrow(
        &self,
        node_id: String,
        current_program_hash: Vec<u8>,
        assigned_program_hash: Option<Vec<u8>>,
        schedule_interval_s: u32,
        battery_mv: u32,
        firmware_abi_version: u32,
        firmware_version: String,
        timestamp_ms: u64,
        encrypted_psk_escrow: Option<Vec<u8>>,
        escrow_key_hint: Option<u16>,
        master_key_id: Option<Vec<u8>>,
    ) {
        let escrow_fields = match encrypted_psk_escrow {
            Some(blob) => {
                if blob.len() != 60 {
                    error!(
                        node_id = %node_id,
                        len = blob.len(),
                        "dropping escrow: encrypted_psk must be 60 bytes"
                    );
                    (None, None, None)
                } else if let (Some(key_hint), Some(key_id)) = (escrow_key_hint, master_key_id) {
                    if key_id.len() != 16 {
                        error!(
                            node_id = %node_id,
                            len = key_id.len(),
                            "dropping escrow: master_key_id must be 16 bytes"
                        );
                        (None, None, None)
                    } else {
                        (Some(blob), Some(key_hint), Some(key_id))
                    }
                } else {
                    error!(
                        node_id = %node_id,
                        "dropping inconsistent escrow fields from node ACTUAL_STATE"
                    );
                    (None, None, None)
                }
            }
            None => (None, None, None),
        };
        let _ = self.tx.send(ConnectorOutboundMessage::ActualState {
            entity_kind: "node",
            entity_id: node_id,
            current_program_hash: Some(current_program_hash),
            assigned_program_hash,
            schedule_interval_s: Some(schedule_interval_s),
            battery_mv: Some(battery_mv),
            firmware_abi_version: Some(firmware_abi_version),
            firmware_version: Some(firmware_version),
            timestamp_ms,
            encrypted_psk_escrow: escrow_fields.0,
            escrow_key_hint: escrow_fields.1,
            master_key_id: escrow_fields.2,
            status_details: None,
        });
    }

    /// Emit gateway-scoped ACTUAL_STATE with escrow status details (GW-2004).
    pub fn emit_gateway_escrow_state(&self, details: StatusDetails) {
        let _ = self.tx.send(ConnectorOutboundMessage::ActualState {
            entity_kind: "gateway",
            entity_id: String::new(),
            current_program_hash: None,
            assigned_program_hash: None,
            schedule_interval_s: None,
            battery_mv: None,
            firmware_abi_version: None,
            firmware_version: None,
            timestamp_ms: current_time_ms(),
            encrypted_psk_escrow: None,
            escrow_key_hint: None,
            master_key_id: None,
            status_details: Some(details),
        });
    }

    /// Emit KEY_ESCROW_PUBKEY (GW-2001).
    pub fn emit_key_escrow_pubkey(
        &self,
        public_key: &[u8; 32],
        key_epoch: u64,
        created_at: u64,
        fingerprint_words: &[&str; 6],
    ) {
        let _ = self.tx.send(ConnectorOutboundMessage::KeyEscrowPubkey {
            public_key: *public_key,
            key_epoch,
            created_at,
            fingerprint_words: fingerprint_words.map(str::to_string),
        });
    }

    /// Emit KEY_ESCROW_REQUEST (GW-2009).
    pub fn emit_key_escrow_request(&self, key_hint: u16, request_id: [u8; 16]) {
        let _ = self.tx.send(ConnectorOutboundMessage::KeyEscrowRequest {
            key_hint,
            request_id,
        });
    }

    /// Emit gateway ACTUAL_STATE with all evolve-962 fields (GW-2003).
    ///
    /// Emitted on startup, connector reconnection, and whenever gateway state
    /// changes (channel change, rotation complete, etc.).
    #[allow(clippy::too_many_arguments)]
    pub fn emit_gateway_actual_state(
        &self,
        entity_id: String,
        channel: u32,
        master_key_id: [u8; 16],
        master_key_epoch: u64,
        x25519_public_key: [u8; 32],
        fingerprint_words: [String; 6],
        missing_key_hints: Vec<u16>,
        salt: Option<Vec<u8>>,
        kdf_params: Option<KdfParams>,
        gateway_version: String,
        gateway_commit: String,
        modem_firmware_version: Option<String>,
        modem_firmware_commit: Option<String>,
        rotation_in_progress: bool,
    ) {
        let _ = self.tx.send(ConnectorOutboundMessage::GatewayActualState {
            entity_id,
            timestamp_ms: current_time_ms(),
            channel,
            master_key_id,
            master_key_epoch,
            x25519_public_key,
            fingerprint_words,
            missing_key_hints,
            salt,
            kdf_params,
            gateway_version,
            gateway_commit,
            modem_firmware_version,
            modem_firmware_commit,
            rotation_in_progress,
        });
    }

    pub fn emit_app_data(
        &self,
        node_id: String,
        program_hash: Vec<u8>,
        payload: Vec<u8>,
        timestamp_ms: u64,
        payload_origin: ConnectorPayloadOrigin,
        readings: Option<std::collections::BTreeMap<String, i64>>,
    ) {
        let _ = self.tx.send(ConnectorOutboundMessage::AppData {
            node_id,
            program_hash,
            payload,
            timestamp_ms,
            payload_origin,
            readings,
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
    program_library: ProgramLibrary,
    pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>>,
    event_hub: Arc<ConnectorEventHub>,
    max_message_size: usize,
    /// Channel for forwarding escrow inbound messages to the gateway engine.
    escrow_inbound_tx: Option<tokio::sync::mpsc::UnboundedSender<EscrowInboundMessage>>,
    /// Channel for forwarding parsed gateway DESIRED_STATE to the gateway engine.
    gateway_desired_state_tx: Option<tokio::sync::mpsc::UnboundedSender<GatewayDesiredState>>,
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
            program_library: ProgramLibrary::new(),
            pending_commands,
            event_hub,
            max_message_size: max_message_size.max(1),
            escrow_inbound_tx: None,
            gateway_desired_state_tx: None,
        }
    }

    /// Set the channel for forwarding escrow inbound messages.
    pub fn set_escrow_inbound_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<EscrowInboundMessage>,
    ) {
        self.escrow_inbound_tx = Some(tx);
    }

    /// Set the channel for forwarding parsed gateway DESIRED_STATE.
    pub fn set_gateway_desired_state_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<GatewayDesiredState>,
    ) {
        self.gateway_desired_state_tx = Some(tx);
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

        match msg_type {
            MSG_TYPE_DESIRED_STATE => {
                let entity_kind = required_text(&map, 2, "entity_kind")?;
                let entity_id = required_text(&map, 3, "entity_id")?;
                let desired_state = required_map(&map, 4, "desired_state")?;

                match entity_kind.as_str() {
                    "gateway" => {
                        self.apply_gateway_desired_state(&entity_id, &desired_state)
                            .await
                    }
                    "node" => {
                        self.apply_node_desired_state(&entity_id, &desired_state)
                            .await
                    }
                    other => Err(format!("unknown entity_kind `{other}`")),
                }
            }
            sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_RESPONSE => {
                // Handled by the escrow subsystem via the inbound escrow channel.
                if let Some(ref tx) = self.escrow_inbound_tx {
                    tx.send(EscrowInboundMessage::KeyEscrowResponse(bytes.to_vec()))
                        .map_err(|_| "escrow channel closed".to_string())?;
                } else {
                    return Err(
                        "escrow inbound channel not configured; message cannot be processed"
                            .to_string(),
                    );
                }
                Ok(())
            }
            sonde_protocol::CONNECTOR_MSG_TYPE_MASTER_KEY_INSTALL => {
                // Handled by the escrow subsystem via the inbound escrow channel.
                if let Some(ref tx) = self.escrow_inbound_tx {
                    tx.send(EscrowInboundMessage::MasterKeyInstall(bytes.to_vec()))
                        .map_err(|_| "escrow channel closed".to_string())?;
                } else {
                    return Err(
                        "escrow inbound channel not configured; message cannot be processed"
                            .to_string(),
                    );
                }
                Ok(())
            }
            _ => Err(format!(
                "unsupported inbound connector msg_type `{msg_type:#x}`"
            )),
        }
    }

    /// Parse and forward gateway DESIRED_STATE (evolve-962 §2.5).
    async fn apply_gateway_desired_state(
        &self,
        entity_id: &str,
        desired_state: &[(Value, Value)],
    ) -> Result<(), String> {
        if entity_id.is_empty() {
            warn!("gateway DESIRED_STATE has empty entity_id; expected hex(gateway_id)");
        }
        let channel = optional_u32_field(desired_state, 15, "channel")?;
        let salt = optional_bytes_field(desired_state, 21, "salt")?;
        if let Some(ref s) = salt {
            if s.len() != 16 {
                return Err(format!(
                    "gateway DESIRED_STATE salt must be 16 bytes, got {}",
                    s.len()
                ));
            }
        }
        let kdf_params = parse_optional_kdf_params(desired_state, 22)?;
        let rotation_payload = optional_bytes_field(desired_state, 28, "rotation_payload")?;
        let recovered_psks = parse_optional_recovered_psks(desired_state, 29)?;

        let gw_ds = GatewayDesiredState {
            entity_id: entity_id.to_owned(),
            channel,
            salt,
            kdf_params,
            rotation_payload,
            recovered_psks,
        };

        if let Some(ref tx) = self.gateway_desired_state_tx {
            tx.send(gw_ds)
                .map_err(|_| "gateway desired state channel closed".to_string())?;
        } else {
            warn!("gateway DESIRED_STATE received but no handler configured; ignoring");
        }
        Ok(())
    }

    async fn apply_node_desired_state(
        &self,
        node_id: &str,
        desired_state: &[(Value, Value)],
    ) -> Result<(), String> {
        let mut node = load_existing_node(&self.storage, node_id).await?;
        let assigned_program_hash =
            optional_bytes_field(desired_state, 1, "assigned_program_hash")?;
        let schedule_interval_s = optional_u32_field(desired_state, 2, "schedule_interval_s")?;
        let ephemeral_program_hash =
            optional_bytes_field(desired_state, 3, "ephemeral_program_hash")?;

        // Ingest inline ELF (key 5) only when assigned_program_hash (key 1) is present.
        if assigned_program_hash.is_some() {
            let inline_elf = optional_bytes_field(desired_state, 5, "assigned_program_elf")?;
            if let Some(elf_bytes) = inline_elf.as_deref() {
                // Validate declared hash format before expensive Prevail verification.
                let declared_hash = assigned_program_hash.as_deref().unwrap();
                if declared_hash.len() != 32 {
                    return Err(format!(
                        "inline ELF declared hash must be exactly 32 bytes, got {}",
                        declared_hash.len()
                    ));
                }

                let profile_str =
                    optional_text_field(desired_state, 6, "assigned_program_verification_profile")?;
                let profile = match profile_str.as_deref() {
                    Some("resident") | None => VerificationProfile::Resident,
                    Some("ephemeral") => VerificationProfile::Ephemeral,
                    Some(other) => {
                        return Err(format!(
                            "`assigned_program_verification_profile` must be \
                             \"resident\" or \"ephemeral\", got \"{other}\""
                        ));
                    }
                };
                let source_filename =
                    optional_text_field(desired_state, 7, "assigned_program_source_filename")?;
                let abi_version =
                    optional_u32_field(desired_state, 8, "assigned_program_abi_version")?;

                let mut record = self
                    .program_library
                    .ingest_elf(elf_bytes, profile)
                    .map_err(|e| format!("inline ELF verification failed: {e}"))?;
                record.source_filename = source_filename;
                record.abi_version = abi_version;

                if record.hash != declared_hash {
                    return Err(format!(
                        "inline ELF hash mismatch: ingested `{}` but declared `{}`",
                        hex::encode(&record.hash),
                        hex::encode(declared_hash),
                    ));
                }

                self.storage
                    .store_program(&record)
                    .await
                    .map_err(|e| format!("store inline program failed: {e}"))?;
                info!(
                    program_hash = hex::encode(&record.hash),
                    size = record.size,
                    node_id = node_id,
                    "ingested inline ELF from DESIRED_STATE"
                );
            }
        }

        if let Some(program_hash) = assigned_program_hash.as_deref() {
            validate_program_exists(
                &self.storage,
                "assign connector desired program",
                "assign program",
                program_hash,
            )
            .await?;
        }
        if let Some(program_hash) = ephemeral_program_hash.as_deref() {
            validate_ephemeral_program(&self.storage, program_hash).await?;
        }

        node.assigned_program_hash = assigned_program_hash;
        match schedule_interval_s {
            Some(interval_s) => {
                node.desired_schedule_interval_s = Some(interval_s);
                node.schedule_interval_s = interval_s;
            }
            None => {
                node.desired_schedule_interval_s = None;
            }
        }
        self.storage
            .upsert_node(&node)
            .await
            .map_err(|e| format!("update node `{node_id}` failed: {e}"))?;

        let mut pending = self.pending_commands.write().await;
        if let Some(commands) = pending.get_mut(node_id) {
            commands.retain(|cmd| {
                !matches!(
                    cmd,
                    PendingCommand::UpdateSchedule { .. } | PendingCommand::RunEphemeral { .. }
                )
            });
            if commands.is_empty() {
                pending.remove(node_id);
            }
        }
        if schedule_interval_s.is_some() || ephemeral_program_hash.is_some() {
            let commands = pending.entry(node_id.to_string()).or_default();
            if let Some(interval_s) = schedule_interval_s {
                commands.push(PendingCommand::UpdateSchedule { interval_s });
            }
            if let Some(program_hash) = ephemeral_program_hash {
                commands.push(PendingCommand::RunEphemeral { program_hash });
            }
        }

        Ok(())
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

async fn load_existing_node(
    storage: &Arc<dyn Storage>,
    node_id: &str,
) -> Result<crate::registry::NodeRecord, String> {
    if node_id.is_empty() {
        return Err("node desired state requires a non-empty entity_id".to_string());
    }
    storage
        .get_node(node_id)
        .await
        .map_err(|e| format!("lookup node `{node_id}` failed: {e}"))?
        .ok_or_else(|| format!("node `{node_id}` not found"))
}

async fn validate_program_exists(
    storage: &Arc<dyn Storage>,
    error_context: &str,
    operation: &str,
    program_hash: &[u8],
) -> Result<(), String> {
    let program_hash_hex = validate_program_hash_bytes(operation, program_hash)
        .map_err(|e| format!("{error_context} failed: {e}"))?;
    storage
        .get_program(program_hash)
        .await
        .map_err(|e| format!("{error_context} failed: look up program `{program_hash_hex}`: {e}"))?
        .ok_or_else(|| format!("{error_context} failed: program `{program_hash_hex}` not found"))?;
    Ok(())
}

async fn validate_ephemeral_program(
    storage: &Arc<dyn Storage>,
    program_hash: &[u8],
) -> Result<(), String> {
    let program_hash_hex = validate_program_hash_bytes("queue ephemeral", program_hash)
        .map_err(|e| format!("queue connector desired ephemeral failed: {e}"))?;
    let program = storage
        .get_program(program_hash)
        .await
        .map_err(|e| {
            format!(
                "queue connector desired ephemeral failed: look up program `{program_hash_hex}`: {e}"
            )
        })?
        .ok_or_else(|| {
            format!(
                "queue connector desired ephemeral failed: program `{program_hash_hex}` not found"
            )
        })?;
    if program.verification_profile != VerificationProfile::Ephemeral {
        return Err(format!(
            "queue connector desired ephemeral failed: program `{program_hash_hex}` has {:?} verification profile, expected Ephemeral",
            program.verification_profile
        ));
    }
    Ok(())
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

fn required_u32(map: &[(Value, Value)], key: u64, field: &str) -> Result<u32, String> {
    let v = required_u64(map, key, field)?;
    u32::try_from(v).map_err(|_| format!("`{field}` exceeds u32::MAX ({v})"))
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

fn required_bytes(map: &[(Value, Value)], key: u64, field: &str) -> Result<Vec<u8>, String> {
    map_get(map, key)
        .ok_or_else(|| format!("missing `{field}`"))
        .and_then(|value| match value {
            Value::Bytes(bytes) => Ok(bytes.clone()),
            _ => Err(format!("`{field}` must be bstr")),
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

fn optional_text_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<String>, String> {
    match map_get(map, key) {
        Some(Value::Text(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(format!("`{field}` must be text or null")),
    }
}

/// Parse optional KDF params from a CBOR map field.
fn parse_optional_kdf_params(
    map: &[(Value, Value)],
    key: u64,
) -> Result<Option<KdfParams>, String> {
    match map_get(map, key) {
        Some(Value::Map(kdf_map)) => {
            let m_cost = required_u32(kdf_map, 1, "m_cost")?;
            let t_cost = required_u32(kdf_map, 2, "t_cost")?;
            let p_cost = required_u32(kdf_map, 3, "p_cost")?;
            let kdf_version = required_u32(kdf_map, 4, "kdf_version")?;
            Ok(Some(KdfParams {
                m_cost,
                t_cost,
                p_cost,
                kdf_version,
            }))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err("kdf_params must be a map or null".to_string()),
    }
}

/// Parse optional recovered PSK records from a CBOR array field.
fn parse_optional_recovered_psks(
    map: &[(Value, Value)],
    key: u64,
) -> Result<Option<Vec<RecoveredPskRecord>>, String> {
    match map_get(map, key) {
        Some(Value::Array(arr)) => {
            let mut records = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                match item {
                    Value::Map(rec_map) => {
                        let node_id =
                            required_text(rec_map, 1, &format!("recovered_psks[{i}].node_id"))?;
                        let key_hint_raw =
                            required_u64(rec_map, 2, &format!("recovered_psks[{i}].key_hint"))?;
                        let key_hint = u16::try_from(key_hint_raw).map_err(|_| {
                            format!(
                                "recovered_psks[{i}].key_hint exceeds u16::MAX ({key_hint_raw})"
                            )
                        })?;
                        let encrypted_psk = required_bytes(
                            rec_map,
                            3,
                            &format!("recovered_psks[{i}].encrypted_psk"),
                        )?;
                        if encrypted_psk.len() != 60 {
                            return Err(format!(
                                "recovered_psks[{i}].encrypted_psk must be 60 bytes, got {}",
                                encrypted_psk.len()
                            ));
                        }
                        let master_key_id = required_bytes(
                            rec_map,
                            4,
                            &format!("recovered_psks[{i}].master_key_id"),
                        )?;
                        if master_key_id.len() != 16 {
                            return Err(format!(
                                "recovered_psks[{i}].master_key_id must be 16 bytes, got {}",
                                master_key_id.len()
                            ));
                        }
                        records.push(RecoveredPskRecord {
                            node_id,
                            key_hint,
                            encrypted_psk,
                            master_key_id,
                        });
                    }
                    _ => {
                        return Err(format!("recovered_psks[{i}] must be a map, got {:?}", item));
                    }
                }
            }
            Ok(Some(records))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err("recovered_psks must be an array or null".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_state_encoding_includes_schedule_interval_at_key_11() {
        let message = ConnectorOutboundMessage::ActualState {
            entity_kind: "node",
            entity_id: "node-1".to_string(),
            current_program_hash: Some(vec![0x11; 32]),
            assigned_program_hash: Some(vec![0x22; 32]),
            schedule_interval_s: Some(60),
            battery_mv: Some(3300),
            firmware_abi_version: Some(1),
            firmware_version: Some("1.2.3".to_string()),
            timestamp_ms: 1234,
            encrypted_psk_escrow: None,
            escrow_key_hint: None,
            master_key_id: None,
            status_details: None,
        };

        let encoded = message.encode().unwrap();
        let decoded = decode_map(&encoded).unwrap();

        assert_eq!(
            required_u64(&decoded, 1, "msg_type").unwrap(),
            MSG_TYPE_ACTUAL_STATE
        );
        assert_eq!(
            optional_u32_field(&decoded, 11, "schedule_interval_s").unwrap(),
            Some(60)
        );
    }

    #[test]
    fn key_escrow_pubkey_encoding() {
        let message = ConnectorOutboundMessage::KeyEscrowPubkey {
            public_key: [0x42u8; 32],
            key_epoch: 3,
            created_at: 1_234_567_890,
            fingerprint_words: [
                "abandon".to_string(),
                "ability".to_string(),
                "able".to_string(),
                "about".to_string(),
                "above".to_string(),
                "absent".to_string(),
            ],
        };
        let encoded = message.encode().unwrap();
        let decoded = decode_map(&encoded).unwrap();
        assert_eq!(
            required_u64(&decoded, 1, "msg_type").unwrap(),
            sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_PUBKEY
        );
        let pk = required_bytes(&decoded, 2, "public_key").unwrap();
        assert_eq!(pk, vec![0x42u8; 32]);
        assert_eq!(required_u64(&decoded, 3, "key_epoch").unwrap(), 3);
        assert_eq!(
            required_u64(&decoded, 4, "created_at").unwrap(),
            1_234_567_890
        );
        let words = match map_get(&decoded, 5) {
            Some(Value::Array(words)) => words,
            other => panic!("expected fingerprint_words array, got {other:?}"),
        };
        assert_eq!(words.len(), 6);
        assert_eq!(words[0].as_text(), Some("abandon"));
        assert_eq!(words[5].as_text(), Some("absent"));
    }

    #[test]
    fn key_escrow_request_encoding() {
        let message = ConnectorOutboundMessage::KeyEscrowRequest {
            key_hint: 0x1234,
            request_id: [0xAB; 16],
        };
        let encoded = message.encode().unwrap();
        let decoded = decode_map(&encoded).unwrap();
        assert_eq!(
            required_u64(&decoded, 1, "msg_type").unwrap(),
            sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_REQUEST
        );
        assert_eq!(required_u64(&decoded, 2, "key_hint").unwrap(), 0x1234);
        let rid = required_bytes(&decoded, 3, "request_id").unwrap();
        assert_eq!(rid, vec![0xAB; 16]);
    }

    #[test]
    fn actual_state_encoding_drops_inconsistent_escrow_fields() {
        let hub = ConnectorEventHub::new(1);
        let mut rx = hub.subscribe();
        hub.emit_actual_state_for_node_with_escrow(
            "node-1".to_string(),
            vec![0x11; 32],
            Some(vec![0x22; 32]),
            60,
            3300,
            1,
            "1.2.3".to_string(),
            1234,
            Some(vec![0xAA; 8]),
            None,
            Some(vec![0x42; 16]),
        );

        let message = rx.try_recv().unwrap();
        let encoded = message.encode().unwrap();
        let decoded = decode_map(&encoded).unwrap();

        assert!(matches!(map_get(&decoded, 12), Some(Value::Null)));
        assert!(matches!(map_get(&decoded, 13), Some(Value::Null)));
        assert!(matches!(map_get(&decoded, 14), Some(Value::Null)));
    }

    fn encode_inbound_message(msg_type: u64) -> Vec<u8> {
        let value = ciborium::Value::Map(vec![(
            ciborium::Value::Integer(1u64.into()),
            ciborium::Value::Integer(msg_type.into()),
        )]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn escrow_messages_require_inbound_channel() {
        let service = ConnectorService::new(
            std::sync::Arc::new(crate::storage::InMemoryStorage::new()),
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            std::sync::Arc::new(ConnectorEventHub::new(1)),
            DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE,
        );

        for msg_type in [
            sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_RESPONSE,
            sonde_protocol::CONNECTOR_MSG_TYPE_MASTER_KEY_INSTALL,
        ] {
            let err = service
                .handle_inbound_message(&encode_inbound_message(msg_type))
                .await
                .unwrap_err();
            assert_eq!(
                err,
                "escrow inbound channel not configured; message cannot be processed"
            );
        }
    }
}
