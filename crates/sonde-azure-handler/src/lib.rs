// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_core_legacy::auth::TokenCredential as LegacyTokenCredential;
use azure_core_legacy::error::ErrorKind as LegacyAzureErrorKind;
use azure_core_legacy::Error as LegacyAzureError;
use azure_core_legacy::StatusCode as LegacyStatusCode;
use azure_data_tables::prelude::TableServiceClient;
use azure_identity::ManagedIdentityCredential;
use azure_identity_legacy::{AppServiceManagedIdentityCredential, TokenCredentialOptions};
use base64::Engine as _;
use ciborium::value::Integer;
use ciborium::Value;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sonde_gateway::connector::{MSG_TYPE_ACTUAL_STATE, MSG_TYPE_APP_DATA, MSG_TYPE_DESIRED_STATE};
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};
use sonde_protocol::normalize_display_filename;
use thiserror::Error;
use tracing::warn;

/// Maximum uploaded ELF size (1 MB) — defense-in-depth before verification.
const MAX_ELF_UPLOAD_SIZE: usize = 1_048_576;

static HISTORY_ROW_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static HISTORY_ROW_PROCESS_NONCE: OnceLock<u64> = OnceLock::new();

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Decode(String),
    #[error("{0}")]
    Store(String),
    #[error("{0}")]
    Publish(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    AzureCore(#[from] azure_core::Error),
    #[error("{0}")]
    Http(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub storage_queue_endpoint: String,
    pub upstream_queue: String,
    pub downstream_queue: String,
    pub storage_account: String,
    pub actual_state_table: String,
    pub desired_state_table: String,
    pub programs_table: String,
    pub sensor_data_table: String,
    /// Azure Table for gateway escrow metadata (pubkey, salt, state).
    /// Defaults to `"gatewayescrow"` if not set.
    pub escrow_table: String,
}

impl RuntimeConfig {
    pub fn from_env() -> Result<Self, HandlerError> {
        let storage_queue_endpoint = required_env("SONDE_AZURE_HANDLER_STORAGE_QUEUE_ENDPOINT")
            .or_else(|_| required_env("QueueConnection__queueServiceUri"))?;
        Ok(Self {
            storage_queue_endpoint,
            upstream_queue: required_env("SONDE_AZURE_HANDLER_UPSTREAM_QUEUE")?,
            downstream_queue: required_env("SONDE_AZURE_HANDLER_DOWNSTREAM_QUEUE")?,
            storage_account: required_env("SONDE_AZURE_HANDLER_STORAGE_ACCOUNT")?,
            actual_state_table: required_env("SONDE_AZURE_HANDLER_ACTUAL_STATE_TABLE")?,
            desired_state_table: required_env("SONDE_AZURE_HANDLER_DESIRED_STATE_TABLE")?,
            programs_table: required_env("SONDE_AZURE_HANDLER_PROGRAMS_TABLE")?,
            sensor_data_table: required_env("SONDE_AZURE_HANDLER_SENSOR_DATA_TABLE")?,
            escrow_table: std::env::var("SONDE_AZURE_HANDLER_ESCROW_TABLE")
                .unwrap_or_else(|_| "gatewayescrow".to_string()),
        })
    }
}

fn required_env(name: &str) -> Result<String, HandlerError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| HandlerError::Config(format!("environment variable `{name}` must be set")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualStateRow {
    pub row_key: String,
    /// Entity kind: `"node"` or `"phone"`.
    pub entity_kind: String,
    pub node_id: String,
    pub observed_current_program_hash: Option<Vec<u8>>,
    pub observed_assigned_program_hash: Option<Vec<u8>>,
    pub observed_schedule_interval_s: Option<u32>,
    pub battery_mv: Option<u32>,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    pub timestamp_ms: u64,
    /// Encrypted PSK escrow blob (AZH-0600).
    pub encrypted_psk_escrow: Option<Vec<u8>>,
    /// Key hint for escrow recovery lookup (AZH-0601).
    pub escrow_key_hint: Option<u16>,
    /// Master key version for escrow blob versioning.
    pub escrow_key_version: Option<u64>,
}

impl ActualStateRow {
    fn from_message(message: &ActualStateMessage) -> Result<Self, HandlerError> {
        if message.encrypted_psk_escrow.is_some()
            && (message.escrow_key_hint.is_none() || message.escrow_key_version.is_none())
        {
            return Err(HandlerError::Decode(
                "ACTUAL_STATE with encrypted_psk_escrow must include escrow_key_hint and escrow_key_version"
                    .into(),
            ));
        }
        Ok(Self {
            row_key: next_history_row_key(message.timestamp_ms)?,
            entity_kind: message.entity_kind.clone(),
            node_id: message.entity_id.clone(),
            observed_current_program_hash: message.current_program_hash.clone(),
            observed_assigned_program_hash: message.assigned_program_hash.clone(),
            observed_schedule_interval_s: message.schedule_interval_s,
            battery_mv: message.battery_mv,
            firmware_abi_version: message.firmware_abi_version,
            firmware_version: message.firmware_version.clone(),
            timestamp_ms: message.timestamp_ms,
            encrypted_psk_escrow: message.encrypted_psk_escrow.clone(),
            escrow_key_hint: message.escrow_key_hint,
            escrow_key_version: message.escrow_key_version,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredStateRow {
    pub row_key: String,
    pub node_id: String,
    pub desired_assigned_program_hash: Option<Vec<u8>>,
    pub desired_schedule_interval_s: Option<u32>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramImageRow {
    pub program_hash: Vec<u8>,
    pub cbor_image: Vec<u8>,
    pub elf_image: Vec<u8>,
    pub source_filename: Option<String>,
    pub abi_version: Option<u32>,
    pub size_bytes: u32,
    pub verification_profile: String,
    pub created_at: String,
}

/// One row in the `SensorData` Azure Table (AZH-0500).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorDataRow {
    pub row_key: String,
    pub node_id: String,
    pub timestamp_ms: u64,
    pub program_hash: Vec<u8>,
    /// Raw APP_DATA blob bytes (base64-encoded when written to Azure Table).
    pub raw_payload: Vec<u8>,
    /// JSON string of decoded readings, or `""` if no decoder or failure.
    pub decoded_readings: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualStateMessage {
    pub entity_kind: String,
    pub entity_id: String,
    pub current_program_hash: Option<Vec<u8>>,
    pub assigned_program_hash: Option<Vec<u8>>,
    pub battery_mv: Option<u32>,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    pub timestamp_ms: u64,
    pub schedule_interval_s: Option<u32>,
    /// Encrypted PSK escrow blob (CBOR key 12, GW-2003).
    pub encrypted_psk_escrow: Option<Vec<u8>>,
    /// Key hint from escrow blob metadata (CBOR key 13, GW-2003).
    pub escrow_key_hint: Option<u16>,
    /// Master key version from escrow blob metadata (CBOR key 14, GW-2003).
    pub escrow_key_version: Option<u64>,
    /// Gateway-scoped status_details (CBOR key 10, AZH-0605).
    pub status_details: Option<GatewayStatusDetails>,
}

/// Parsed gateway-scoped status_details from ACTUAL_STATE (CBOR key 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayStatusDetails {
    /// Escrow lifecycle state (disabled, bootstrapping, ready, etc.).
    pub escrow_state: Option<String>,
    /// Current master key version.
    pub escrow_key_version: Option<u64>,
    /// KDF salt bytes.
    pub escrow_salt: Option<Vec<u8>>,
    /// KDF parameters (Argon2id).
    pub escrow_kdf_params: Option<KdfParams>,
}

/// KDF parameters for Argon2id (mirroring the gateway-side definition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdfParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub kdf_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDataMessage {
    pub node_id: String,
    pub program_hash: Vec<u8>,
    pub payload: Vec<u8>,
    pub timestamp_ms: u64,
    /// Decoded sensor readings from decoder BPF execution (GW-1903),
    /// extracted from CBOR key 16 of the upstream connector message.
    pub readings: Option<BTreeMap<String, i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorMessage {
    ActualState(ActualStateMessage),
    AppData(AppDataMessage),
    /// KEY_ESCROW_PUBKEY (msg_type 0x10, GW-2001).
    KeyEscrowPubkey(KeyEscrowPubkeyMessage),
    /// KEY_ESCROW_REQUEST (msg_type 0x11, GW-2009).
    KeyEscrowRequest(KeyEscrowRequestMessage),
    /// MASTER_KEY_INSTALL (msg_type 0x13, AZH-0604) — opaque relay.
    MasterKeyInstall(Vec<u8>),
    Unsupported(u64),
}

/// Gateway recovery public key publication (AZH-0602).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEscrowPubkeyMessage {
    pub public_key: Vec<u8>,
    pub key_epoch: u64,
    pub created_at: u64,
    pub fingerprint_words: Vec<String>,
}

/// Request for escrowed PSK(s) matching a key_hint (AZH-0601).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEscrowRequestMessage {
    pub key_hint: u16,
    pub request_id: Vec<u8>,
}

#[async_trait]
pub trait HandlerStore: Send + Sync {
    async fn append_actual_state(&self, row: &ActualStateRow) -> Result<(), HandlerError>;
    async fn load_latest_actual_state(
        &self,
        node_id: &str,
    ) -> Result<Option<ActualStateRow>, HandlerError>;
    async fn load_latest_desired_state(
        &self,
        node_id: &str,
    ) -> Result<Option<DesiredStateRow>, HandlerError>;
    async fn load_program_image(
        &self,
        program_hash: &[u8],
    ) -> Result<Option<ProgramImageRow>, HandlerError>;
    async fn store_program_image(&self, row: &ProgramImageRow) -> Result<(), HandlerError>;
    /// Append a row to the `SensorData` table for a `GW-0813` message (AZH-0500).
    async fn append_sensor_data(&self, row: &SensorDataRow) -> Result<(), HandlerError>;

    // ── PSK key escrow (AZH-0600–AZH-0605) ────────────────────

    /// Store or update the gateway's recovery public key (AZH-0602).
    /// Only updates if `key_epoch` >= stored epoch (monotonic guard).
    async fn store_gateway_escrow_pubkey(
        &self,
        public_key: &[u8],
        key_epoch: u64,
        created_at: u64,
    ) -> Result<(), HandlerError> {
        let _ = (public_key, key_epoch, created_at);
        Err(HandlerError::Store(
            "store_gateway_escrow_pubkey not implemented for this store backend".into(),
        ))
    }

    /// Load escrowed PSK blobs matching a key_hint (AZH-0601).
    /// Returns at most `max_candidates` blobs.
    async fn load_escrow_blobs_by_key_hint(
        &self,
        key_hint: u16,
        max_candidates: usize,
    ) -> Result<Vec<Vec<u8>>, HandlerError> {
        let _ = (key_hint, max_candidates);
        Err(HandlerError::Store(
            "load_escrow_blobs_by_key_hint not implemented for this store backend".into(),
        ))
    }

    /// Store the gateway's escrow lifecycle state (AZH-0605).
    async fn store_gateway_escrow_state(
        &self,
        details: &GatewayStatusDetails,
    ) -> Result<(), HandlerError> {
        let _ = details;
        Err(HandlerError::Store(
            "store_gateway_escrow_state not implemented for this store backend".into(),
        ))
    }

    /// Store the KDF salt with first-writer-wins semantics (AZH-0603).
    ///
    /// If no salt has been stored yet, persists the given salt and returns
    /// `Ok(true)`. If a salt already exists, leaves it untouched and returns
    /// `Ok(false)`.
    async fn store_escrow_salt_if_absent(
        &self,
        salt: &[u8],
        kdf_params: Option<&KdfParams>,
        created_at: u64,
    ) -> Result<bool, HandlerError> {
        let _ = (salt, kdf_params, created_at);
        Err(HandlerError::Store(
            "store_escrow_salt_if_absent not implemented for this store backend".into(),
        ))
    }
}

#[async_trait]
pub trait QueuePublisher: Send + Sync {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError>;
}

pub struct AzureHandler<S, P> {
    store: Arc<S>,
    publisher: Arc<P>,
    downstream_queue: String,
}

impl<S, P> AzureHandler<S, P>
where
    S: HandlerStore,
    P: QueuePublisher,
{
    pub fn new(store: Arc<S>, publisher: Arc<P>, downstream_queue: impl Into<String>) -> Self {
        Self {
            store,
            publisher,
            downstream_queue: downstream_queue.into(),
        }
    }

    pub async fn handle_payload(&self, payload: &[u8]) -> Result<(), HandlerError> {
        match decode_connector_message(payload)? {
            ConnectorMessage::ActualState(actual_state) => {
                match actual_state.entity_kind.as_str() {
                    "node" => self.handle_actual_state(actual_state).await?,
                    "phone" => {
                        // Phone escrow ACTUAL_STATE (AZH-0600): store escrow blob
                        // with phone-scoped partition key ("p:" prefix).
                        if actual_state.encrypted_psk_escrow.is_some() {
                            let row = ActualStateRow::from_message(&actual_state)?;
                            self.store.append_actual_state(&row).await?;
                        }
                    }
                    "gateway" => {
                        // Gateway-scoped ACTUAL_STATE (AZH-0605): persist escrow state.
                        if let Some(ref details) = actual_state.status_details {
                            self.store.store_gateway_escrow_state(details).await?;

                            // AZH-0603: first-writer-wins salt storage.
                            if let Some(ref salt) = details.escrow_salt {
                                let stored = self
                                    .store
                                    .store_escrow_salt_if_absent(
                                        salt,
                                        details.escrow_kdf_params.as_ref(),
                                        actual_state.timestamp_ms,
                                    )
                                    .await?;
                                if !stored {
                                    tracing::debug!(
                                        "escrow salt already exists, ignoring incoming salt (first-writer-wins)"
                                    );
                                }
                            }
                        }
                    }
                    other => {
                        warn!(
                            entity_kind = %other,
                            entity_id = %actual_state.entity_id,
                            "ignoring ACTUAL_STATE for unknown entity_kind"
                        );
                    }
                }
                Ok(())
            }
            ConnectorMessage::AppData(app_data) => self.handle_app_data(app_data).await,
            ConnectorMessage::KeyEscrowPubkey(msg) => {
                self.store
                    .store_gateway_escrow_pubkey(&msg.public_key, msg.key_epoch, msg.created_at)
                    .await
            }
            ConnectorMessage::KeyEscrowRequest(msg) => self.handle_key_escrow_request(msg).await,
            ConnectorMessage::MasterKeyInstall(raw_bytes) => {
                self.publisher
                    .publish(&self.downstream_queue, raw_bytes)
                    .await
            }
            ConnectorMessage::Unsupported(msg_type) => {
                warn!(msg_type, "ignoring unsupported connector message");
                Ok(())
            }
        }
    }

    async fn handle_actual_state(
        &self,
        actual_state: ActualStateMessage,
    ) -> Result<(), HandlerError> {
        if actual_state.entity_id.is_empty() {
            return Err(HandlerError::Decode(
                "node-scoped ACTUAL_STATE requires a non-empty entity_id".to_string(),
            ));
        }

        let appended_row = ActualStateRow::from_message(&actual_state)?;
        self.store.append_actual_state(&appended_row).await?;

        let latest_actual = self
            .store
            .load_latest_actual_state(&actual_state.entity_id)
            .await?
            .ok_or_else(|| {
                HandlerError::Store(format!(
                    "actual-state row `{}` disappeared after append",
                    actual_state.entity_id
                ))
            })?;
        if latest_actual.timestamp_ms > appended_row.timestamp_ms {
            return Ok(());
        }
        let actual_state_for_evaluation = appended_row;

        let Some(desired_row) = self
            .store
            .load_latest_desired_state(&actual_state.entity_id)
            .await?
        else {
            return Ok(());
        };
        if desired_row.node_id != actual_state.entity_id {
            return Err(HandlerError::Store(format!(
                "desired-state row node_id `{}` did not match requested node `{}`",
                desired_row.node_id, actual_state.entity_id
            )));
        }

        let program_diverged = desired_row
            .desired_assigned_program_hash
            .as_ref()
            .is_some_and(|desired| {
                actual_state_for_evaluation
                    .observed_current_program_hash
                    .as_ref()
                    != Some(desired)
            });
        let schedule_diverged = desired_row
            .desired_schedule_interval_s
            .is_some_and(|desired| {
                actual_state_for_evaluation.observed_schedule_interval_s != Some(desired)
            });
        if !program_diverged && !schedule_diverged {
            return Ok(());
        }

        let program_row = if program_diverged {
            match desired_row.desired_assigned_program_hash.as_deref() {
                Some(desired_hash) => {
                    let row = self.store.load_program_image(desired_hash).await?;
                    if let Some(ref r) = row {
                        if r.elf_image.is_empty() {
                            warn!(
                                program_hash = hex::encode(desired_hash),
                                node_id = %actual_state.entity_id,
                                "program row has no elf_image (legacy row); \
                                 DESIRED_STATE will omit inline ELF — \
                                 re-ingest the program to populate elf_image"
                            );
                        }
                    } else {
                        warn!(
                            program_hash = hex::encode(desired_hash),
                            node_id = %actual_state.entity_id,
                            "program not found in programs table; \
                             DESIRED_STATE will omit inline ELF"
                        );
                    }
                    row
                }
                None => None,
            }
        } else {
            None
        };
        let desired = encode_desired_state(&desired_row, program_row.as_ref())?;
        self.publisher
            .publish(&self.downstream_queue, desired)
            .await
    }

    /// Handle KEY_ESCROW_REQUEST: look up escrow blobs by key_hint and
    /// respond with KEY_ESCROW_RESPONSE (AZH-0601).
    async fn handle_key_escrow_request(
        &self,
        msg: KeyEscrowRequestMessage,
    ) -> Result<(), HandlerError> {
        let mut candidates = self
            .store
            .load_escrow_blobs_by_key_hint(msg.key_hint, 16)
            .await?;
        candidates.truncate(16);

        let response = encode_key_escrow_response(&msg.request_id, msg.key_hint, &candidates)?;
        self.publisher
            .publish(&self.downstream_queue, response)
            .await
    }

    async fn handle_app_data(&self, app_data: AppDataMessage) -> Result<(), HandlerError> {
        let decoded_readings = readings_to_json(&app_data.readings);
        let sensor_row = SensorDataRow {
            row_key: next_history_row_key(app_data.timestamp_ms)?,
            node_id: app_data.node_id,
            timestamp_ms: app_data.timestamp_ms,
            program_hash: app_data.program_hash,
            raw_payload: app_data.payload,
            decoded_readings,
        };
        self.store.append_sensor_data(&sensor_row).await
    }

    /// Handle a ProgramIngest HTTP trigger invocation (WEB-0300).
    ///
    /// Accepts JSON: `{"elf": "base64...", "source_filename": "...",
    /// "abi_version": N, "verification_profile": "resident"|"ephemeral"}`
    ///
    /// Returns a JSON response with the program hash and metadata, or an error.
    pub async fn handle_program_ingest(
        &self,
        body: &serde_json::Value,
    ) -> Result<IngestResponse, IngestError> {
        let elf_b64 = body
            .get("elf")
            .and_then(|v| v.as_str())
            .ok_or_else(|| IngestError::bad_request("missing or non-string `elf` field"))?;

        // Pre-decode length check: base64 encodes 3 bytes → 4 chars, so
        // MAX_ELF_UPLOAD_SIZE decoded bytes ≈ ceil(n/3)*4 encoded chars.
        let max_encoded_len = (MAX_ELF_UPLOAD_SIZE / 3 + 1) * 4;
        if elf_b64.len() > max_encoded_len {
            return Err(IngestError::payload_too_large(format!(
                "base64 `elf` field length {} exceeds maximum; \
                 decoded ELF must not exceed {} bytes",
                elf_b64.len(),
                MAX_ELF_UPLOAD_SIZE
            )));
        }

        let elf_bytes = base64::engine::general_purpose::STANDARD
            .decode(elf_b64)
            .map_err(|e| {
                IngestError::bad_request(format!("`elf` field is not valid base64: {e}"))
            })?;

        if elf_bytes.is_empty() {
            return Err(IngestError::bad_request("`elf` field must not be empty"));
        }
        if elf_bytes.len() > MAX_ELF_UPLOAD_SIZE {
            return Err(IngestError::payload_too_large(format!(
                "ELF size {} bytes exceeds limit of {} bytes",
                elf_bytes.len(),
                MAX_ELF_UPLOAD_SIZE
            )));
        }

        let profile_str = match body.get("verification_profile") {
            Some(serde_json::Value::Null) | None => "resident",
            Some(serde_json::Value::String(s)) => s.as_str(),
            Some(_) => {
                return Err(IngestError::bad_request(
                    "`verification_profile` must be a string (`resident` or `ephemeral`)",
                ));
            }
        };
        let profile = match profile_str {
            "resident" => VerificationProfile::Resident,
            "ephemeral" => VerificationProfile::Ephemeral,
            other => {
                return Err(IngestError::bad_request(format!(
                    "unknown `verification_profile`: `{other}`; \
                     expected `resident` or `ephemeral`"
                )));
            }
        };

        let source_filename_raw = match body.get("source_filename") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Null) | None => None,
            Some(_) => {
                return Err(IngestError::bad_request(
                    "`source_filename` must be a string",
                ));
            }
        };
        let source_filename = normalize_display_filename(&source_filename_raw);

        let abi_version = match body.get("abi_version") {
            Some(serde_json::Value::Null) | None => None,
            Some(v) => {
                let raw = v.as_u64().ok_or_else(|| {
                    IngestError::bad_request("`abi_version` must be a non-negative integer")
                })?;
                // Azure Tables stores this as Edm.Int32, so cap at i32::MAX.
                let max_abi = i32::MAX as u64;
                if raw > max_abi {
                    return Err(IngestError::bad_request(format!(
                        "`abi_version` value {raw} exceeds Edm.Int32 maximum ({max_abi})"
                    )));
                }
                Some(raw as u32)
            }
        };

        let lib = ProgramLibrary::new();
        let mut record = lib.ingest_elf(&elf_bytes, profile).map_err(|e| {
            use sonde_gateway::program::ProgramError;
            match &e {
                ProgramError::Internal(_) => {
                    IngestError::internal(format!("program ingestion internal error: {e}"))
                }
                _ => IngestError::unprocessable(format!(
                    "program verification/ingestion failed: {e}"
                )),
            }
        })?;
        record.abi_version = abi_version;
        record.source_filename = source_filename.clone();

        let now = chrono_iso8601_utc_now();
        let profile_name = match record.verification_profile {
            VerificationProfile::Resident => "resident",
            VerificationProfile::Ephemeral => "ephemeral",
        };
        let row = ProgramImageRow {
            program_hash: record.hash.clone(),
            cbor_image: record.image,
            elf_image: elf_bytes.to_vec(),
            source_filename: record.source_filename.clone(),
            abi_version: record.abi_version,
            size_bytes: record.size,
            verification_profile: profile_name.to_string(),
            created_at: now,
        };
        self.store
            .store_program_image(&row)
            .await
            .map_err(|e| IngestError::internal(format!("store program failed: {e}")))?;

        Ok(IngestResponse {
            program_hash: hex::encode(&record.hash),
            size: record.size,
            abi_version: record.abi_version,
            source_filename: record.source_filename,
        })
    }
}

/// Successful response from program ingestion.
#[derive(Debug, Clone, Serialize)]
pub struct IngestResponse {
    pub program_hash: String,
    pub size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abi_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_filename: Option<String>,
}

/// Error from program ingestion with HTTP status code.
#[derive(Debug)]
pub struct IngestError {
    pub status_code: u16,
    pub message: String,
}

impl IngestError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status_code: 400,
            message: msg.into(),
        }
    }

    fn payload_too_large(msg: impl Into<String>) -> Self {
        Self {
            status_code: 413,
            message: msg.into(),
        }
    }

    fn unprocessable(msg: impl Into<String>) -> Self {
        Self {
            status_code: 422,
            message: msg.into(),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status_code: 500,
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status_code, self.message)
    }
}

fn chrono_iso8601_utc_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Compute year/month/day from days since epoch (1970-01-01).
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m + 1,
        remaining + 1,
        hours,
        minutes,
        seconds
    )
}

/// Extract the JSON body from an Azure Functions HTTP trigger invocation
/// envelope. The envelope is the outer JSON sent by the Functions host when
/// `enableForwardingHttpRequest` is `false`.
///
/// Returns the parsed body as a `serde_json::Value`.
pub fn extract_http_trigger_body(request_body: &[u8]) -> Result<serde_json::Value, HandlerError> {
    let envelope: serde_json::Value = serde_json::from_slice(request_body)?;
    let body_str = envelope
        .get("Data")
        .or_else(|| envelope.get("data"))
        .and_then(|data| data.get("req").or_else(|| data.get("Req")))
        .and_then(|req| req.get("Body").or_else(|| req.get("body")))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            HandlerError::Decode("HTTP trigger envelope missing `Data.req.Body`".to_string())
        })?;
    serde_json::from_str(body_str)
        .map_err(|e| HandlerError::Decode(format!("HTTP trigger body is not valid JSON: {e}")))
}

/// Format an `IngestResponse` as an Azure Functions HTTP output binding
/// response envelope. The binding name `res` matches
/// `ProgramIngest/function.json`.
pub fn format_ingest_response(status_code: u16, body: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "Outputs": {
            "res": {
                "statusCode": status_code,
                "headers": {"Content-Type": "application/json"},
                "body": serde_json::to_string(body).unwrap_or_default()
            }
        }
    })
}

pub struct AzureTablesStore {
    actual_state_table: azure_data_tables::clients::TableClient,
    desired_state_table: azure_data_tables::clients::TableClient,
    programs_table: azure_data_tables::clients::TableClient,
    sensor_data_table: azure_data_tables::clients::TableClient,
    escrow_table: azure_data_tables::clients::TableClient,
}

impl AzureTablesStore {
    pub fn new(config: &RuntimeConfig) -> Result<Self, HandlerError> {
        let credential: Arc<dyn LegacyTokenCredential> = Arc::new(
            AppServiceManagedIdentityCredential::create(TokenCredentialOptions::default())
                .map_err(|e| {
                    HandlerError::Config(format!("create Azure Table credential failed: {e}"))
                })?,
        );
        let service = TableServiceClient::new(config.storage_account.clone(), credential);
        Ok(Self {
            actual_state_table: service.table_client(config.actual_state_table.clone()),
            desired_state_table: service.table_client(config.desired_state_table.clone()),
            programs_table: service.table_client(config.programs_table.clone()),
            sensor_data_table: service.table_client(config.sensor_data_table.clone()),
            escrow_table: service.table_client(config.escrow_table.clone()),
        })
    }
}

fn is_legacy_not_found(e: &LegacyAzureError) -> bool {
    matches!(
        e.kind(),
        LegacyAzureErrorKind::HttpResponse {
            status: LegacyStatusCode::NotFound,
            ..
        }
    )
}

fn is_legacy_conflict(e: &LegacyAzureError) -> bool {
    matches!(
        e.kind(),
        LegacyAzureErrorKind::HttpResponse {
            status: LegacyStatusCode::Conflict,
            ..
        }
    )
}

#[async_trait]
impl HandlerStore for AzureTablesStore {
    async fn append_actual_state(&self, row: &ActualStateRow) -> Result<(), HandlerError> {
        let entity = ActualStateEntity::try_from(row.clone())?;
        self.actual_state_table
            .insert::<_, ActualStateEntity>(&entity)
            .map_err(|e| HandlerError::Store(format!("prepare actual-state insert failed: {e}")))?
            .await
            .map_err(|e| HandlerError::Store(format!("append actual-state row failed: {e}")))?;
        Ok(())
    }

    async fn load_latest_actual_state(
        &self,
        node_id: &str,
    ) -> Result<Option<ActualStateRow>, HandlerError> {
        let partition_key = encode_node_partition_key(node_id);
        let mut stream = self
            .actual_state_table
            .query()
            .filter(partition_filter(&partition_key))
            .top(1)
            .into_stream::<ActualStateEntity>();
        match stream.next().await {
            Some(Ok(response)) => response
                .entities
                .into_iter()
                .next()
                .map(ActualStateRow::try_from)
                .transpose(),
            Some(Err(e)) => Err(HandlerError::Store(format!(
                "query latest actual-state row failed: {e}"
            ))),
            None => Ok(None),
        }
    }

    async fn load_latest_desired_state(
        &self,
        node_id: &str,
    ) -> Result<Option<DesiredStateRow>, HandlerError> {
        let partition_key = encode_node_partition_key(node_id);
        let mut stream = self
            .desired_state_table
            .query()
            .filter(partition_filter(&partition_key))
            .top(1)
            .into_stream::<DesiredStateEntity>();
        match stream.next().await {
            Some(Ok(response)) => response
                .entities
                .into_iter()
                .next()
                .map(DesiredStateRow::try_from)
                .transpose(),
            Some(Err(e)) => Err(HandlerError::Store(format!(
                "query latest desired-state row failed: {e}"
            ))),
            None => Ok(None),
        }
    }

    async fn load_program_image(
        &self,
        program_hash: &[u8],
    ) -> Result<Option<ProgramImageRow>, HandlerError> {
        let row_key = hex::encode(program_hash);
        let entity_client = self
            .programs_table
            .partition_key_client("program")
            .entity_client(row_key);
        match entity_client.get::<ProgramImageEntity>().await {
            Ok(response) => {
                let e = response.entity;
                let cbor_image = decode_base64_field(e.cbor_image, "programs.cbor_image")?;
                let elf_image = match e.elf_image {
                    Some(b64) => decode_base64_field(b64, "programs.elf_image")?,
                    None => Vec::new(),
                };
                Ok(Some(ProgramImageRow {
                    program_hash: program_hash.to_vec(),
                    cbor_image: cbor_image.clone(),
                    elf_image,
                    source_filename: e.source_filename,
                    abi_version: e.abi_version,
                    size_bytes: e.size_bytes.unwrap_or(cbor_image.len() as u32),
                    verification_profile: e
                        .verification_profile
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "resident".to_string()),
                    created_at: e.created_at.unwrap_or_default(),
                }))
            }
            Err(e) if is_legacy_not_found(&e) => Ok(None),
            Err(e) => Err(HandlerError::Store(format!(
                "query program image failed: {e}"
            ))),
        }
    }

    async fn store_program_image(&self, row: &ProgramImageRow) -> Result<(), HandlerError> {
        let row_key = hex::encode(&row.program_hash);
        let entity = ProgramImageEntity {
            partition_key: "program".to_string(),
            row_key: row_key.clone(),
            cbor_image: base64::engine::general_purpose::STANDARD.encode(&row.cbor_image),
            elf_image: Some(base64::engine::general_purpose::STANDARD.encode(&row.elf_image)),
            source_filename: row.source_filename.clone(),
            abi_version: row.abi_version,
            size_bytes: Some(row.size_bytes),
            verification_profile: Some(row.verification_profile.clone()),
            created_at: Some(row.created_at.clone()),
        };
        let entity_client = self
            .programs_table
            .partition_key_client("program")
            .entity_client(row_key);
        entity_client
            .insert_or_replace(entity)
            .map_err(|e| HandlerError::Store(format!("prepare program-image upsert failed: {e}")))?
            .await
            .map_err(|e| HandlerError::Store(format!("upsert program-image row failed: {e}")))?;
        Ok(())
    }

    async fn append_sensor_data(&self, row: &SensorDataRow) -> Result<(), HandlerError> {
        let entity = SensorDataEntity::try_from(row.clone())?;
        self.sensor_data_table
            .insert::<_, SensorDataEntity>(&entity)
            .map_err(|e| HandlerError::Store(format!("prepare sensor-data insert failed: {e}")))?
            .await
            .map_err(|e| HandlerError::Store(format!("append sensor-data row failed: {e}")))?;
        Ok(())
    }

    async fn store_gateway_escrow_pubkey(
        &self,
        public_key: &[u8],
        key_epoch: u64,
        created_at: u64,
    ) -> Result<(), HandlerError> {
        let key_epoch_i64 = i64::try_from(key_epoch)
            .map_err(|_| HandlerError::Store(format!("key_epoch {key_epoch} exceeds i64::MAX")))?;
        let created_at_i64 = i64::try_from(created_at).map_err(|_| {
            HandlerError::Store(format!("created_at {created_at} exceeds i64::MAX"))
        })?;

        // Monotonic guard: only update if incoming epoch >= stored.
        let existing = self
            .escrow_table
            .partition_key_client(ESCROW_PARTITION_KEY)
            .entity_client("pubkey")
            .get::<GatewayEscrowPubkeyEntity>()
            .await;

        match existing {
            Ok(response) => {
                if response.entity.key_epoch > key_epoch_i64 {
                    // Stale message — stored epoch is higher, ignore.
                    return Ok(());
                }
            }
            Err(e) if is_legacy_not_found(&e) => {
                // No existing pubkey — proceed with upsert.
            }
            Err(e) => {
                return Err(HandlerError::Store(format!(
                    "query existing escrow pubkey failed: {e}"
                )));
            }
        }

        let entity = GatewayEscrowPubkeyEntity {
            partition_key: ESCROW_PARTITION_KEY.to_string(),
            row_key: "pubkey".to_string(),
            public_key: base64::prelude::BASE64_STANDARD.encode(public_key),
            key_epoch: key_epoch_i64,
            created_at: created_at_i64,
        };
        self.escrow_table
            .partition_key_client(ESCROW_PARTITION_KEY)
            .entity_client("pubkey")
            .insert_or_replace(entity)
            .map_err(|e| HandlerError::Store(format!("prepare escrow pubkey upsert failed: {e}")))?
            .await
            .map_err(|e| HandlerError::Store(format!("upsert escrow pubkey failed: {e}")))?;
        Ok(())
    }

    async fn load_escrow_blobs_by_key_hint(
        &self,
        key_hint: u16,
        max_candidates: usize,
    ) -> Result<Vec<Vec<u8>>, HandlerError> {
        // Query actual_state table for rows with matching escrow_key_hint.
        // Azure Tables stores key_hint as u32, so filter on that.
        let filter = format!("escrow_key_hint eq {}", key_hint as u32);
        // Fetch more than needed to deduplicate by partition (latest per subject).
        let fetch_limit = (max_candidates + 1) * 3;
        let mut stream = self
            .actual_state_table
            .query()
            .filter(filter)
            .top(fetch_limit as u32)
            .into_stream::<ActualStateEntity>();

        // Collect results, dedup by partition_key (keep first = latest per reverse-tick row_key).
        let mut seen_partitions = std::collections::HashSet::new();
        let mut blobs = Vec::new();
        while let Some(result) = stream.next().await {
            let response = result.map_err(|e| {
                HandlerError::Store(format!("query escrow blobs by key_hint failed: {e}"))
            })?;
            for entity in response.entities {
                if seen_partitions.contains(&entity.partition_key) {
                    continue;
                }
                if let Some(b64_blob) = &entity.encrypted_psk_escrow {
                    let blob = base64::prelude::BASE64_STANDARD
                        .decode(b64_blob)
                        .map_err(|e| {
                            HandlerError::Store(format!(
                                "invalid base64 in encrypted_psk_escrow: {e}"
                            ))
                        })?;
                    seen_partitions.insert(entity.partition_key.clone());
                    blobs.push(blob);
                }
            }
        }

        if blobs.len() > max_candidates {
            warn!(
                key_hint,
                total = blobs.len(),
                max_candidates,
                "escrow recovery candidates exceed max, truncating"
            );
            blobs.truncate(max_candidates);
        }
        Ok(blobs)
    }

    async fn store_gateway_escrow_state(
        &self,
        details: &GatewayStatusDetails,
    ) -> Result<(), HandlerError> {
        let escrow_key_version = details
            .escrow_key_version
            .map(|v| {
                i64::try_from(v).map_err(|_| {
                    HandlerError::Store(format!("escrow_key_version {v} exceeds i64::MAX"))
                })
            })
            .transpose()?;

        let kdf_params_json = details.escrow_kdf_params.as_ref().map(|kdf| {
            serde_json::json!({
                "m_cost": kdf.m_cost,
                "t_cost": kdf.t_cost,
                "p_cost": kdf.p_cost,
                "version": kdf.kdf_version,
            })
            .to_string()
        });

        let entity = GatewayEscrowStateEntity {
            partition_key: ESCROW_PARTITION_KEY.to_string(),
            row_key: "state".to_string(),
            escrow_state: details.escrow_state.clone(),
            escrow_key_version,
            escrow_salt: details
                .escrow_salt
                .as_ref()
                .map(|s| base64::prelude::BASE64_STANDARD.encode(s)),
            kdf_params_json,
        };
        self.escrow_table
            .partition_key_client(ESCROW_PARTITION_KEY)
            .entity_client("state")
            .insert_or_replace(entity)
            .map_err(|e| HandlerError::Store(format!("prepare escrow state upsert failed: {e}")))?
            .await
            .map_err(|e| HandlerError::Store(format!("upsert escrow state failed: {e}")))?;
        Ok(())
    }

    async fn store_escrow_salt_if_absent(
        &self,
        salt: &[u8],
        kdf_params: Option<&KdfParams>,
        created_at: u64,
    ) -> Result<bool, HandlerError> {
        let created_at_i64 = i64::try_from(created_at).map_err(|_| {
            HandlerError::Store(format!("salt created_at {created_at} exceeds i64::MAX"))
        })?;
        let kdf_params_json = kdf_params.map(|kdf| {
            serde_json::json!({
                "m_cost": kdf.m_cost,
                "t_cost": kdf.t_cost,
                "p_cost": kdf.p_cost,
                "version": kdf.kdf_version,
            })
            .to_string()
        });
        let entity = GatewayEscrowSaltEntity {
            partition_key: ESCROW_PARTITION_KEY.to_string(),
            row_key: "salt".to_string(),
            salt: base64::prelude::BASE64_STANDARD.encode(salt),
            kdf_params_json,
            created_at: created_at_i64,
        };
        // First-writer-wins: use insert (not upsert). 409 Conflict means
        // a salt row already exists, which is the expected steady-state.
        match self
            .escrow_table
            .insert::<_, GatewayEscrowSaltEntity>(&entity)
            .map_err(|e| HandlerError::Store(format!("prepare escrow salt insert failed: {e}")))?
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if is_legacy_conflict(&e) => Ok(false),
            Err(e) => Err(HandlerError::Store(format!(
                "insert escrow salt failed: {e}"
            ))),
        }
    }
}

const STORAGE_QUEUE_API_VERSION: &str = "2024-11-04";
const STORAGE_TOKEN_SCOPE: &str = "https://storage.azure.com/.default";

pub struct StorageQueuePublisher {
    queue_endpoint: String,
    credential: Arc<dyn TokenCredential>,
    http_client: reqwest::Client,
}

impl StorageQueuePublisher {
    pub fn new(queue_endpoint: impl Into<String>) -> Result<Self, HandlerError> {
        let credential: Arc<dyn TokenCredential> =
            ManagedIdentityCredential::new(None).map_err(|e| {
                HandlerError::Config(format!("create Storage Queue credential failed: {e}"))
            })?;
        let http_client = reqwest::Client::new();
        let mut endpoint = queue_endpoint.into();
        if endpoint.ends_with('/') {
            endpoint.pop();
        }
        Ok(Self {
            queue_endpoint: endpoint,
            credential,
            http_client,
        })
    }

    async fn get_bearer_token(&self) -> Result<String, HandlerError> {
        let token = self
            .credential
            .get_token(&[STORAGE_TOKEN_SCOPE], None)
            .await?;
        Ok(token.token.secret().to_string())
    }
}

#[async_trait]
impl QueuePublisher for StorageQueuePublisher {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError> {
        let token = self.get_bearer_token().await?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
        let body = format!("<QueueMessage><MessageText>{encoded}</MessageText></QueueMessage>");
        let url = format!("{}/{queue}/messages", self.queue_endpoint);
        let date = httpdate::fmt_http_date(std::time::SystemTime::now());
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-ms-version", STORAGE_QUEUE_API_VERSION)
            .header("x-ms-date", &date)
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| HandlerError::Http(format!("send Storage Queue message failed: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read response>".to_string());
            return Err(HandlerError::Http(format!(
                "Storage Queue POST message returned {status}: {body}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ActualStateEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    node_id: String,
    observed_current_program_hash: Option<String>,
    observed_assigned_program_hash: Option<String>,
    observed_schedule_interval_s: Option<u32>,
    battery_mv: Option<u32>,
    firmware_abi_version: Option<u32>,
    firmware_version: Option<String>,
    #[serde(deserialize_with = "deserialize_u64_flexible")]
    timestamp_ms: u64,
    /// Base64-encoded encrypted PSK escrow blob (AZH-0600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encrypted_psk_escrow: Option<String>,
    /// Key hint for escrow recovery lookup (AZH-0601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escrow_key_hint: Option<u32>,
    /// Master key version for escrow blob versioning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escrow_key_version: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct DesiredStateEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    node_id: String,
    desired_assigned_program_hash: Option<String>,
    desired_schedule_interval_s: Option<u32>,
    #[serde(deserialize_with = "deserialize_u64_flexible")]
    timestamp_ms: u64,
}

fn deserialize_u64_flexible<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::{self, Visitor};

    struct U64Visitor;

    impl<'de> Visitor<'de> for U64Visitor {
        type Value = u64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a u64, i64, f64, or numeric string")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom(format!("timestamp_ms {v} is not a valid u64")))
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<u64, E> {
            if !v.is_finite() || v < 0.0 || v >= 2.0_f64.powi(64) || v.fract() != 0.0 {
                return Err(E::custom(format!("timestamp_ms {v} is not a valid u64")));
            }
            Ok(v as u64)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            v.parse::<u64>()
                .map_err(|_| E::custom(format!("cannot parse \"{v}\" as u64")))
        }
    }

    d.deserialize_any(U64Visitor)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ProgramImageEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    cbor_image: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    elf_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    source_filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    abi_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    size_bytes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    verification_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    created_at: Option<String>,
}

impl TryFrom<ActualStateEntity> for ActualStateRow {
    type Error = HandlerError;

    fn try_from(value: ActualStateEntity) -> Result<Self, Self::Error> {
        let entity_kind = if value.partition_key.starts_with("p:") {
            "phone".to_string()
        } else {
            "node".to_string()
        };
        Ok(Self {
            row_key: value.row_key,
            entity_kind,
            node_id: value.node_id,
            observed_current_program_hash: decode_optional_program_hash(
                value.observed_current_program_hash,
                "observed_current_program_hash",
            )?,
            observed_assigned_program_hash: decode_optional_program_hash(
                value.observed_assigned_program_hash,
                "observed_assigned_program_hash",
            )?,
            observed_schedule_interval_s: value.observed_schedule_interval_s,
            battery_mv: value.battery_mv,
            firmware_abi_version: value.firmware_abi_version,
            firmware_version: value.firmware_version,
            timestamp_ms: value.timestamp_ms,
            encrypted_psk_escrow: value
                .encrypted_psk_escrow
                .map(|b64| {
                    base64::prelude::BASE64_STANDARD.decode(&b64).map_err(|e| {
                        HandlerError::Decode(format!("invalid base64 in encrypted_psk_escrow: {e}"))
                    })
                })
                .transpose()?,
            escrow_key_hint: match value.escrow_key_hint {
                Some(h) => Some(u16::try_from(h).map_err(|_| {
                    HandlerError::Decode(format!("escrow_key_hint {} out of u16 range", h))
                })?),
                None => None,
            },
            escrow_key_version: value.escrow_key_version,
        })
    }
}

impl TryFrom<ActualStateRow> for ActualStateEntity {
    type Error = HandlerError;

    fn try_from(value: ActualStateRow) -> Result<Self, Self::Error> {
        let partition_key = match value.entity_kind.as_str() {
            "phone" => encode_phone_partition_key(&value.node_id),
            _ => encode_node_partition_key(&value.node_id),
        };
        Ok(Self {
            partition_key,
            row_key: value.row_key,
            node_id: value.node_id,
            observed_current_program_hash: encode_optional_hex(value.observed_current_program_hash),
            observed_assigned_program_hash: encode_optional_hex(
                value.observed_assigned_program_hash,
            ),
            observed_schedule_interval_s: value.observed_schedule_interval_s,
            battery_mv: value.battery_mv,
            firmware_abi_version: value.firmware_abi_version,
            firmware_version: value.firmware_version,
            timestamp_ms: value.timestamp_ms,
            encrypted_psk_escrow: value
                .encrypted_psk_escrow
                .map(|b| base64::prelude::BASE64_STANDARD.encode(&b)),
            escrow_key_hint: value.escrow_key_hint.map(|h| h as u32),
            escrow_key_version: value.escrow_key_version,
        })
    }
}

impl TryFrom<DesiredStateEntity> for DesiredStateRow {
    type Error = HandlerError;

    fn try_from(value: DesiredStateEntity) -> Result<Self, Self::Error> {
        Ok(Self {
            row_key: value.row_key,
            node_id: value.node_id,
            desired_assigned_program_hash: decode_optional_program_hash(
                value.desired_assigned_program_hash,
                "desired_assigned_program_hash",
            )?,
            desired_schedule_interval_s: value.desired_schedule_interval_s,
            timestamp_ms: value.timestamp_ms,
        })
    }
}

impl TryFrom<DesiredStateRow> for DesiredStateEntity {
    type Error = HandlerError;

    fn try_from(value: DesiredStateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            partition_key: encode_node_partition_key(&value.node_id),
            row_key: value.row_key,
            node_id: value.node_id,
            desired_assigned_program_hash: encode_optional_hex(value.desired_assigned_program_hash),
            desired_schedule_interval_s: value.desired_schedule_interval_s,
            timestamp_ms: value.timestamp_ms,
        })
    }
}

pub fn extract_trigger_payload(request_body: &[u8]) -> Result<Vec<u8>, HandlerError> {
    let envelope: serde_json::Value = serde_json::from_slice(request_body)?;
    if let Some(value) = envelope
        .get("data")
        .and_then(|data| data.get("message"))
        .or_else(|| envelope.get("Data").and_then(|data| data.get("message")))
        .or_else(|| envelope.get("Data"))
        .or_else(|| envelope.get("Body"))
        .or_else(|| envelope.get("body"))
    {
        return extract_json_payload(value);
    }
    if let Some(object) = envelope.get("data").and_then(|value| value.as_object()) {
        if object.len() == 1 {
            if let Some((_, value)) = object.iter().next() {
                return extract_json_payload(value);
            }
        } else if !object.is_empty() {
            return Err(HandlerError::Decode(
                "custom handler request `data` object must contain exactly one binding payload"
                    .to_string(),
            ));
        }
    }
    Err(HandlerError::Decode(
        "custom handler request did not contain a trigger payload".to_string(),
    ))
}

fn extract_json_payload(value: &serde_json::Value) -> Result<Vec<u8>, HandlerError> {
    match value {
        serde_json::Value::String(text) => {
            // The Functions runtime may double-quote string values from queue
            // triggers (e.g. `"\"base64...\""`). Strip surrounding quotes.
            let stripped = text
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(text);
            match base64::engine::general_purpose::STANDARD.decode(stripped) {
                Ok(bytes) => Ok(bytes),
                Err(_) => Ok(stripped.as_bytes().to_vec()),
            }
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| {
                        HandlerError::Decode(
                            "payload byte array must contain uint8 values".to_string(),
                        )
                    })
            })
            .collect(),
        serde_json::Value::Object(_) => serde_json::to_vec(value).map_err(HandlerError::from),
        other => Err(HandlerError::Decode(format!(
            "unsupported trigger payload shape `{}`",
            other
        ))),
    }
}

fn decode_connector_message(bytes: &[u8]) -> Result<ConnectorMessage, HandlerError> {
    let map = decode_map(bytes)?;
    let msg_type = required_u64(&map, 1, "msg_type")?;
    match msg_type {
        MSG_TYPE_ACTUAL_STATE => Ok(ConnectorMessage::ActualState(ActualStateMessage {
            entity_kind: required_text(&map, 2, "entity_kind")?,
            entity_id: required_text(&map, 3, "entity_id")?,
            current_program_hash: optional_program_hash_field(&map, 4, "current_program_hash")?,
            assigned_program_hash: optional_program_hash_field(&map, 5, "assigned_program_hash")?,
            battery_mv: optional_u32_field(&map, 6, "battery_mv")?,
            firmware_abi_version: optional_u32_field(&map, 7, "firmware_abi_version")?,
            firmware_version: optional_text_field(&map, 8, "firmware_version")?,
            timestamp_ms: required_u64(&map, 9, "timestamp_ms")?,
            schedule_interval_s: optional_u32_field(&map, 11, "schedule_interval_s")?,
            encrypted_psk_escrow: optional_bytes_field(&map, 12, "encrypted_psk_escrow")?,
            escrow_key_hint: optional_u16_field(&map, 13, "escrow_key_hint")?,
            escrow_key_version: optional_u64_field(&map, 14, "escrow_key_version")?,
            status_details: decode_optional_status_details(&map, 10)?,
        })),
        MSG_TYPE_APP_DATA => Ok(ConnectorMessage::AppData(AppDataMessage {
            node_id: required_text(&map, 2, "node_id")?,
            program_hash: required_program_hash(&map, 3, "program_hash")?,
            payload: required_bytes(&map, 4, "payload")?,
            timestamp_ms: required_u64(&map, 5, "timestamp_ms")?,
            readings: decode_optional_readings(&map, 16)?,
        })),
        sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_PUBKEY => {
            let public_key = required_bytes(&map, 2, "public_key")?;
            if public_key.len() != 32 {
                return Err(HandlerError::Decode(format!(
                    "public_key has invalid length: expected 32 bytes, got {}",
                    public_key.len()
                )));
            }
            Ok(ConnectorMessage::KeyEscrowPubkey(KeyEscrowPubkeyMessage {
                public_key,
                key_epoch: required_u64(&map, 3, "key_epoch")?,
                created_at: required_u64(&map, 4, "created_at")?,
                fingerprint_words: optional_text_array_field(&map, 5)?,
            }))
        }
        sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_REQUEST => {
            let key_hint_raw = required_u64(&map, 2, "key_hint")?;
            let key_hint = u16::try_from(key_hint_raw).map_err(|_| {
                HandlerError::Decode(format!("key_hint {} exceeds u16::MAX", key_hint_raw))
            })?;
            let request_id = required_bytes(&map, 3, "request_id")?;
            if request_id.len() != 16 {
                return Err(HandlerError::Decode(format!(
                    "request_id has invalid length: expected 16 bytes, got {}",
                    request_id.len()
                )));
            }
            Ok(ConnectorMessage::KeyEscrowRequest(
                KeyEscrowRequestMessage {
                    key_hint,
                    request_id,
                },
            ))
        }
        sonde_protocol::CONNECTOR_MSG_TYPE_MASTER_KEY_INSTALL => {
            Ok(ConnectorMessage::MasterKeyInstall(bytes.to_vec()))
        }
        other => Ok(ConnectorMessage::Unsupported(other)),
    }
}

pub(crate) fn encode_desired_state(
    row: &DesiredStateRow,
    program_row: Option<&ProgramImageRow>,
) -> Result<Vec<u8>, HandlerError> {
    let mut desired_state_entries = vec![
        map_entry(
            1,
            opt_bytes_value(row.desired_assigned_program_hash.as_deref()),
        ),
        map_entry(2, opt_u32_value(row.desired_schedule_interval_s)),
    ];
    if let Some(prog) = program_row {
        if !prog.elf_image.is_empty() {
            desired_state_entries.push(map_entry(5, Value::Bytes(prog.elf_image.clone())));
            desired_state_entries
                .push(map_entry(6, Value::Text(prog.verification_profile.clone())));
            if let Some(ref filename) = prog.source_filename {
                desired_state_entries.push(map_entry(7, Value::Text(filename.clone())));
            }
            if let Some(abi) = prog.abi_version {
                desired_state_entries.push(map_entry(8, Value::Integer(Integer::from(abi))));
            }
        }
    }
    let desired_state = Value::Map(desired_state_entries);
    let value = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("node".to_string())),
        map_entry(3, Value::Text(row.node_id.clone())),
        map_entry(4, desired_state),
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&value, &mut bytes)
        .map_err(|e| HandlerError::Decode(format!("encode desired state failed: {e}")))?;
    Ok(bytes)
}

fn encode_optional_hex(value: Option<Vec<u8>>) -> Option<String> {
    value.map(hex::encode)
}

/// Encode a KEY_ESCROW_RESPONSE message (msg_type 0x12, AZH-0601).
fn encode_key_escrow_response(
    request_id: &[u8],
    key_hint: u16,
    candidates: &[Vec<u8>],
) -> Result<Vec<u8>, HandlerError> {
    let candidate_values: Vec<Value> = candidates.iter().map(|c| Value::Bytes(c.clone())).collect();
    let value = Value::Map(vec![
        map_entry(
            1,
            Value::Integer(sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_RESPONSE.into()),
        ),
        map_entry(2, Value::Bytes(request_id.to_vec())),
        map_entry(3, Value::Array(candidate_values)),
        map_entry(4, Value::Integer((key_hint as u64).into())),
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&value, &mut bytes)
        .map_err(|e| HandlerError::Decode(format!("encode KEY_ESCROW_RESPONSE failed: {e}")))?;
    Ok(bytes)
}

/// JavaScript's `Number.MAX_SAFE_INTEGER` (2^53 - 1).
const JS_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Convert an optional readings map to a JSON string (AZH-0501).
///
/// Int64 values within JavaScript's `Number.MAX_SAFE_INTEGER` (±2^53 - 1) are
/// encoded as JSON numbers. Values exceeding that threshold are encoded as JSON
/// strings to preserve precision for SPA consumers that use `JSON.parse`.
/// Returns `""` when no readings are present.
fn readings_to_json(readings: &Option<BTreeMap<String, i64>>) -> String {
    let Some(map) = readings else {
        return String::new();
    };
    if map.is_empty() {
        return String::new();
    }
    let json_map: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, &v)| {
            let json_val = if (-JS_MAX_SAFE_INTEGER..=JS_MAX_SAFE_INTEGER).contains(&v) {
                serde_json::Value::Number(serde_json::Number::from(v))
            } else {
                serde_json::Value::String(v.to_string())
            };
            (k.clone(), json_val)
        })
        .collect();
    serde_json::to_string(&json_map).unwrap_or_default()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct SensorDataEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    node_id: String,
    #[serde(deserialize_with = "deserialize_u64_flexible")]
    timestamp_ms: u64,
    program_hash: String,
    raw_payload: String,
    decoded_readings: String,
}

impl TryFrom<SensorDataRow> for SensorDataEntity {
    type Error = HandlerError;

    fn try_from(value: SensorDataRow) -> Result<Self, Self::Error> {
        Ok(Self {
            partition_key: encode_node_partition_key(&value.node_id),
            row_key: value.row_key,
            node_id: value.node_id,
            timestamp_ms: value.timestamp_ms,
            program_hash: hex::encode(&value.program_hash),
            raw_payload: base64::engine::general_purpose::STANDARD.encode(&value.raw_payload),
            decoded_readings: value.decoded_readings,
        })
    }
}

fn encode_node_partition_key(node_id: &str) -> String {
    let digest = Sha256::digest(node_id.as_bytes());
    format!("n:{}", hex::encode(digest))
}

fn encode_phone_partition_key(phone_id: &str) -> String {
    let digest = Sha256::digest(phone_id.as_bytes());
    format!("p:{}", hex::encode(digest))
}

/// Azure Table entity for gateway escrow public key (AZH-0602).
#[derive(Debug, Deserialize, Serialize, Clone)]
struct GatewayEscrowPubkeyEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    /// Base64-encoded X25519 public key (32 bytes).
    public_key: String,
    /// Monotonic key epoch.
    key_epoch: i64,
    /// Creation timestamp (Unix milliseconds).
    created_at: i64,
}

/// Gateway escrow metadata partition key (fixed).
const ESCROW_PARTITION_KEY: &str = "gateway";

/// Azure Table entity for gateway escrow state (AZH-0605).
#[derive(Debug, Deserialize, Serialize, Clone)]
struct GatewayEscrowStateEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    /// Escrow lifecycle state (disabled, bootstrapping, ready, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escrow_state: Option<String>,
    /// Current master key version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escrow_key_version: Option<i64>,
    /// Base64-encoded KDF salt bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    escrow_salt: Option<String>,
    /// JSON-encoded KDF parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf_params_json: Option<String>,
}

/// Azure Table entity for KDF salt (AZH-0603).
#[derive(Debug, Deserialize, Serialize, Clone)]
struct GatewayEscrowSaltEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    /// Base64-encoded KDF salt bytes.
    salt: String,
    /// JSON-encoded KDF parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf_params_json: Option<String>,
    /// Creation timestamp (Unix milliseconds).
    created_at: i64,
}

// Equal-timestamp ordering is only guaranteed within one handler process
// lifetime; reconciliation correctness must not depend on cross-process suffix
// ordering.
fn next_history_row_key(timestamp_ms: u64) -> Result<String, HandlerError> {
    let sequence = HISTORY_ROW_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let process_nonce = history_row_process_nonce()?;
    Ok(format!(
        "{:016x}:{:016x}:{:016x}",
        u64::MAX - timestamp_ms,
        u64::MAX - sequence,
        process_nonce
    ))
}

fn history_row_process_nonce() -> Result<u64, HandlerError> {
    if let Some(nonce) = HISTORY_ROW_PROCESS_NONCE.get() {
        return Ok(*nonce);
    }

    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|e| HandlerError::Store(format!("generate row-key nonce failed: {e}")))?;
    let nonce = u64::from_be_bytes(bytes);
    if HISTORY_ROW_PROCESS_NONCE.set(nonce).is_ok() {
        return Ok(nonce);
    }
    HISTORY_ROW_PROCESS_NONCE
        .get()
        .copied()
        .ok_or_else(|| HandlerError::Store("row-key nonce initialization lost race".to_string()))
}

fn partition_filter(partition_key: &str) -> String {
    format!("PartitionKey eq '{partition_key}'")
}

fn decode_optional_program_hash(
    value: Option<String>,
    field: &str,
) -> Result<Option<Vec<u8>>, HandlerError> {
    value
        .map(|text| decode_hex_program_hash(text, field))
        .transpose()
}

fn decode_map(bytes: &[u8]) -> Result<Vec<(Value, Value)>, HandlerError> {
    let value: Value = ciborium::from_reader(bytes)
        .map_err(|e| HandlerError::Decode(format!("failed to decode connector CBOR: {e}")))?;
    value
        .as_map()
        .cloned()
        .ok_or_else(|| HandlerError::Decode("connector payload must be a CBOR map".to_string()))
}

fn map_entry(key: u64, value: Value) -> (Value, Value) {
    (Value::Integer(key.into()), value)
}

fn map_get(map: &[(Value, Value)], key: u64) -> Option<&Value> {
    map.iter().find_map(
        |(k, v)| match k.as_integer().and_then(|i| u64::try_from(i).ok()) {
            Some(found) if found == key => Some(v),
            _ => None,
        },
    )
}

fn required_u64(map: &[(Value, Value)], key: u64, field: &str) -> Result<u64, HandlerError> {
    map_get(map, key)
        .ok_or_else(|| HandlerError::Decode(format!("missing `{field}`")))
        .and_then(|value| {
            value
                .as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be uint")))
        })
}

fn required_text(map: &[(Value, Value)], key: u64, field: &str) -> Result<String, HandlerError> {
    map_get(map, key)
        .ok_or_else(|| HandlerError::Decode(format!("missing `{field}`")))
        .and_then(|value| {
            value
                .as_text()
                .map(|text| text.to_string())
                .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be text")))
        })
}

fn required_bytes(map: &[(Value, Value)], key: u64, field: &str) -> Result<Vec<u8>, HandlerError> {
    map_get(map, key)
        .ok_or_else(|| HandlerError::Decode(format!("missing `{field}`")))
        .and_then(|value| match value {
            Value::Bytes(bytes) => Ok(bytes.clone()),
            _ => Err(HandlerError::Decode(format!("`{field}` must be bstr"))),
        })
}

fn required_program_hash(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Vec<u8>, HandlerError> {
    let bytes = required_bytes(map, key, field)?;
    validate_program_hash_length(&bytes, field)?;
    Ok(bytes)
}

fn optional_program_hash_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<Vec<u8>>, HandlerError> {
    let value = optional_bytes_field(map, key, field)?;
    if let Some(bytes) = value.as_deref() {
        validate_program_hash_length(bytes, field)?;
    }
    Ok(value)
}

fn optional_bytes_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<Vec<u8>>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Bytes(bytes)) if bytes.is_empty() => Ok(None),
        Some(Value::Bytes(bytes)) => Ok(Some(bytes.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(HandlerError::Decode(format!(
            "`{field}` must be bstr or null"
        ))),
    }
}

fn optional_u32_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<u32>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be uint or null"))),
    }
}

fn optional_text_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<String>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_text()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be text or null"))),
    }
}

fn optional_u16_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<u16>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .and_then(|v| u16::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be uint or null"))),
    }
}

fn optional_u64_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<u64>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .map(Some)
            .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be uint or null"))),
    }
}

fn optional_text_array_field(
    map: &[(Value, Value)],
    key: u64,
) -> Result<Vec<String>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Array(arr)) => {
            let mut result = Vec::with_capacity(arr.len());
            for v in arr {
                match v {
                    Value::Text(s) => result.push(s.clone()),
                    _ => {
                        return Err(HandlerError::Decode(
                            "fingerprint_words array contains non-text entry".into(),
                        ));
                    }
                }
            }
            Ok(result)
        }
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(other) => Err(HandlerError::Decode(format!(
            "fingerprint_words must be an array, got {:?}",
            other
        ))),
    }
}

fn opt_bytes_value(value: Option<&[u8]>) -> Value {
    match value {
        Some(bytes) => Value::Bytes(bytes.to_vec()),
        None => Value::Null,
    }
}

fn decode_optional_status_details(
    map: &[(Value, Value)],
    key: u64,
) -> Result<Option<GatewayStatusDetails>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Map(sd_pairs)) => {
            let mut escrow_state = None;
            let mut escrow_key_version = None;
            let mut escrow_salt = None;
            let mut escrow_kdf_params = None;

            for (k, v) in sd_pairs {
                let sd_key = match k {
                    Value::Integer(i) => {
                        let val: i128 = (*i).into();
                        u64::try_from(val).ok()
                    }
                    _ => None,
                };
                match sd_key {
                    Some(1) => {
                        if let Value::Text(s) = v {
                            escrow_state = Some(s.clone());
                        }
                    }
                    Some(2) => {
                        if let Value::Integer(i) = v {
                            let val: i128 = (*i).into();
                            escrow_key_version = u64::try_from(val).ok();
                        }
                    }
                    Some(3) => {
                        if let Value::Bytes(b) = v {
                            escrow_salt = Some(b.clone());
                        }
                    }
                    Some(4) => {
                        if let Value::Map(kdf_pairs) = v {
                            let mut m_cost = 0u32;
                            let mut t_cost = 0u32;
                            let mut p_cost = 0u32;
                            let mut kdf_version = 0u32;
                            for (kk, kv) in kdf_pairs {
                                let kdf_key = match kk {
                                    Value::Integer(i) => {
                                        let val: i128 = (*i).into();
                                        u64::try_from(val).ok()
                                    }
                                    _ => None,
                                };
                                if let Value::Integer(iv) = kv {
                                    let val: i128 = (*iv).into();
                                    let val32 = u32::try_from(val).unwrap_or(0);
                                    match kdf_key {
                                        Some(1) => m_cost = val32,
                                        Some(2) => t_cost = val32,
                                        Some(3) => p_cost = val32,
                                        Some(4) => kdf_version = val32,
                                        _ => {}
                                    }
                                }
                            }
                            escrow_kdf_params = Some(KdfParams {
                                m_cost,
                                t_cost,
                                p_cost,
                                kdf_version,
                            });
                        }
                    }
                    _ => {}
                }
            }

            // Only return Some if at least one field was populated.
            if escrow_state.is_some()
                || escrow_key_version.is_some()
                || escrow_salt.is_some()
                || escrow_kdf_params.is_some()
            {
                Ok(Some(GatewayStatusDetails {
                    escrow_state,
                    escrow_key_version,
                    escrow_salt,
                    escrow_kdf_params,
                }))
            } else {
                Ok(None)
            }
        }
        Some(Value::Null) | None => Ok(None),
        _ => Ok(None),
    }
}

fn opt_u32_value(value: Option<u32>) -> Value {
    match value {
        Some(v) => Value::Integer(u64::from(v).into()),
        None => Value::Null,
    }
}

fn decode_hex_program_hash(text: String, field: &str) -> Result<Vec<u8>, HandlerError> {
    let bytes = hex::decode(&text)
        .map_err(|e| HandlerError::Store(format!("`{field}` must contain valid hex: {e}")))?;
    validate_program_hash_length(&bytes, field).map_err(|e| HandlerError::Store(e.to_string()))?;
    Ok(bytes)
}

fn decode_base64_field(text: String, field: &str) -> Result<Vec<u8>, HandlerError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|e| HandlerError::Store(format!("`{field}` must contain valid base64: {e}")))
}

fn validate_program_hash_length(bytes: &[u8], field: &str) -> Result<(), HandlerError> {
    if bytes.len() == 32 {
        return Ok(());
    }
    Err(HandlerError::Decode(format!(
        "`{field}` must be exactly 32 bytes"
    )))
}

/// Decode an optional CBOR readings map (key 16) as `BTreeMap<String, i64>`.
///
/// Returns `Ok(None)` when the key is absent or null. Returns an error when the
/// key is present but not a valid `{ text → signed integer }` map.
fn decode_optional_readings(
    map: &[(Value, Value)],
    key: u64,
) -> Result<Option<BTreeMap<String, i64>>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Map(entries)) => {
            let mut result = BTreeMap::new();
            for (k, v) in entries {
                let name = k.as_text().ok_or_else(|| {
                    HandlerError::Decode("readings map key must be text".to_string())
                })?;
                let val = v
                    .as_integer()
                    .and_then(|i| i64::try_from(i).ok())
                    .ok_or_else(|| {
                        HandlerError::Decode(format!(
                            "readings value for `{name}` must be a signed integer"
                        ))
                    })?;
                result.insert(name.to_string(), val);
            }
            Ok(Some(result))
        }
        Some(_) => Err(HandlerError::Decode(
            "`readings` (key 16) must be a CBOR map or null".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    #[test]
    fn storage_queue_xml_envelope_is_well_formed() {
        let payload = b"hello world";
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
        let xml = format!("<QueueMessage><MessageText>{encoded}</MessageText></QueueMessage>");
        assert!(xml.starts_with("<QueueMessage><MessageText>"));
        assert!(xml.ends_with("</MessageText></QueueMessage>"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap();
        assert_eq!(decoded, payload);
    }

    #[derive(Default)]
    struct MemoryStore {
        actual_rows: Mutex<HashMap<String, Vec<ActualStateRow>>>,
        desired_rows: Mutex<HashMap<String, Vec<DesiredStateRow>>>,
        program_images: Mutex<HashMap<String, ProgramImageRow>>,
        stored_program_rows: Mutex<Vec<ProgramImageRow>>,
        sensor_data_rows: Mutex<Vec<SensorDataRow>>,
        escrow_blobs_by_hint: Mutex<HashMap<u16, Vec<Vec<u8>>>>,
        stored_gateway_pubkeys: Mutex<Vec<(Vec<u8>, u64, u64)>>,
        stored_escrow_state: Mutex<Option<GatewayStatusDetails>>,
        stored_salt: Mutex<Option<Vec<u8>>>,
    }

    impl MemoryStore {
        async fn desired_writes(&self) -> usize {
            self.desired_rows
                .lock()
                .await
                .values()
                .map(Vec::len)
                .sum::<usize>()
        }

        async fn append_desired(&self, row: DesiredStateRow) {
            self.desired_rows
                .lock()
                .await
                .entry(row.node_id.clone())
                .or_default()
                .push(row);
        }

        async fn actual_rows_for(&self, node_id: &str) -> Vec<ActualStateRow> {
            self.actual_rows
                .lock()
                .await
                .get(node_id)
                .cloned()
                .unwrap_or_default()
        }

        async fn append_program_image(&self, program_hash: &[u8], program_image: &[u8]) {
            let row = ProgramImageRow {
                program_hash: program_hash.to_vec(),
                cbor_image: program_image.to_vec(),
                elf_image: program_image.to_vec(),
                source_filename: None,
                abi_version: None,
                size_bytes: program_image.len() as u32,
                verification_profile: "resident".to_string(),
                created_at: String::new(),
            };
            self.program_images
                .lock()
                .await
                .insert(hex::encode(program_hash), row);
        }

        async fn set_escrow_blobs(&self, key_hint: u16, blobs: Vec<Vec<u8>>) {
            self.escrow_blobs_by_hint
                .lock()
                .await
                .insert(key_hint, blobs);
        }
    }

    #[async_trait]
    impl HandlerStore for MemoryStore {
        async fn append_actual_state(&self, row: &ActualStateRow) -> Result<(), HandlerError> {
            self.actual_rows
                .lock()
                .await
                .entry(row.node_id.clone())
                .or_default()
                .push(row.clone());
            Ok(())
        }

        async fn load_latest_actual_state(
            &self,
            node_id: &str,
        ) -> Result<Option<ActualStateRow>, HandlerError> {
            Ok(self.actual_rows.lock().await.get(node_id).and_then(|rows| {
                rows.iter()
                    .min_by(|a, b| a.row_key.cmp(&b.row_key))
                    .cloned()
            }))
        }

        async fn load_latest_desired_state(
            &self,
            node_id: &str,
        ) -> Result<Option<DesiredStateRow>, HandlerError> {
            Ok(self
                .desired_rows
                .lock()
                .await
                .get(node_id)
                .and_then(|rows| {
                    rows.iter()
                        .min_by(|a, b| a.row_key.cmp(&b.row_key))
                        .cloned()
                }))
        }

        async fn load_program_image(
            &self,
            program_hash: &[u8],
        ) -> Result<Option<ProgramImageRow>, HandlerError> {
            Ok(self
                .program_images
                .lock()
                .await
                .get(&hex::encode(program_hash))
                .cloned())
        }

        async fn store_program_image(&self, row: &ProgramImageRow) -> Result<(), HandlerError> {
            self.program_images
                .lock()
                .await
                .insert(hex::encode(&row.program_hash), row.clone());
            self.stored_program_rows.lock().await.push(row.clone());
            Ok(())
        }

        async fn append_sensor_data(&self, row: &SensorDataRow) -> Result<(), HandlerError> {
            self.sensor_data_rows.lock().await.push(row.clone());
            Ok(())
        }

        async fn store_gateway_escrow_pubkey(
            &self,
            public_key: &[u8],
            key_epoch: u64,
            created_at: u64,
        ) -> Result<(), HandlerError> {
            self.stored_gateway_pubkeys.lock().await.push((
                public_key.to_vec(),
                key_epoch,
                created_at,
            ));
            Ok(())
        }

        async fn load_escrow_blobs_by_key_hint(
            &self,
            key_hint: u16,
            max_candidates: usize,
        ) -> Result<Vec<Vec<u8>>, HandlerError> {
            let blobs = self
                .escrow_blobs_by_hint
                .lock()
                .await
                .get(&key_hint)
                .cloned()
                .unwrap_or_default();
            Ok(blobs.into_iter().take(max_candidates).collect())
        }

        async fn store_gateway_escrow_state(
            &self,
            details: &GatewayStatusDetails,
        ) -> Result<(), HandlerError> {
            *self.stored_escrow_state.lock().await = Some(details.clone());
            Ok(())
        }

        async fn store_escrow_salt_if_absent(
            &self,
            salt: &[u8],
            _kdf_params: Option<&KdfParams>,
            _created_at: u64,
        ) -> Result<bool, HandlerError> {
            let mut stored = self.stored_salt.lock().await;
            if stored.is_some() {
                Ok(false)
            } else {
                *stored = Some(salt.to_vec());
                Ok(true)
            }
        }
    }

    #[derive(Default)]
    struct RecordingPublisher {
        sends: Mutex<Vec<(String, Vec<u8>)>>,
    }

    #[async_trait]
    impl QueuePublisher for RecordingPublisher {
        async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError> {
            self.sends.lock().await.push((queue.to_string(), payload));
            Ok(())
        }
    }

    struct FailOncePublisher {
        sends: Mutex<Vec<(String, Vec<u8>)>>,
        failed: Mutex<bool>,
    }

    #[async_trait]
    impl QueuePublisher for FailOncePublisher {
        async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError> {
            let mut failed = self.failed.lock().await;
            if !*failed {
                *failed = true;
                return Err(HandlerError::Publish(
                    "simulated first publish failure".to_string(),
                ));
            }
            drop(failed);
            self.sends.lock().await.push((queue.to_string(), payload));
            Ok(())
        }
    }

    struct FailingStore {
        append_actual_error: Option<String>,
        load_desired_error: Option<String>,
        load_program_image_error: Option<String>,
        latest_actual: Mutex<Option<ActualStateRow>>,
    }

    #[async_trait]
    impl HandlerStore for FailingStore {
        async fn append_actual_state(&self, row: &ActualStateRow) -> Result<(), HandlerError> {
            match &self.append_actual_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => {
                    *self.latest_actual.lock().await = Some(row.clone());
                    Ok(())
                }
            }
        }

        async fn load_latest_actual_state(
            &self,
            _node_id: &str,
        ) -> Result<Option<ActualStateRow>, HandlerError> {
            Ok(self.latest_actual.lock().await.clone())
        }

        async fn load_latest_desired_state(
            &self,
            node_id: &str,
        ) -> Result<Option<DesiredStateRow>, HandlerError> {
            match &self.load_desired_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(Some(desired_row(
                    node_id,
                    Some(vec![0xBB; 32]),
                    Some(60),
                    1234,
                ))),
            }
        }

        async fn load_program_image(
            &self,
            _program_hash: &[u8],
        ) -> Result<Option<ProgramImageRow>, HandlerError> {
            match &self.load_program_image_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(None),
            }
        }

        async fn store_program_image(&self, _row: &ProgramImageRow) -> Result<(), HandlerError> {
            Ok(())
        }

        async fn append_sensor_data(&self, _row: &SensorDataRow) -> Result<(), HandlerError> {
            Ok(())
        }
    }

    struct SameTimestampDifferentLatestStore {
        latest_actual: ActualStateRow,
        desired: DesiredStateRow,
        appended_rows: Mutex<Vec<ActualStateRow>>,
    }

    #[async_trait]
    impl HandlerStore for SameTimestampDifferentLatestStore {
        async fn append_actual_state(&self, row: &ActualStateRow) -> Result<(), HandlerError> {
            self.appended_rows.lock().await.push(row.clone());
            Ok(())
        }

        async fn load_latest_actual_state(
            &self,
            _node_id: &str,
        ) -> Result<Option<ActualStateRow>, HandlerError> {
            Ok(Some(self.latest_actual.clone()))
        }

        async fn load_latest_desired_state(
            &self,
            _node_id: &str,
        ) -> Result<Option<DesiredStateRow>, HandlerError> {
            Ok(Some(self.desired.clone()))
        }

        async fn load_program_image(
            &self,
            _program_hash: &[u8],
        ) -> Result<Option<ProgramImageRow>, HandlerError> {
            Ok(None)
        }

        async fn store_program_image(&self, _row: &ProgramImageRow) -> Result<(), HandlerError> {
            Ok(())
        }

        async fn append_sensor_data(&self, _row: &SensorDataRow) -> Result<(), HandlerError> {
            Ok(())
        }
    }

    struct MismatchedDesiredNodeStore {
        desired: DesiredStateRow,
        latest_actual: Mutex<Option<ActualStateRow>>,
        appended_rows: Mutex<Vec<ActualStateRow>>,
    }

    #[async_trait]
    impl HandlerStore for MismatchedDesiredNodeStore {
        async fn append_actual_state(&self, row: &ActualStateRow) -> Result<(), HandlerError> {
            self.appended_rows.lock().await.push(row.clone());
            *self.latest_actual.lock().await = Some(row.clone());
            Ok(())
        }

        async fn load_latest_actual_state(
            &self,
            _node_id: &str,
        ) -> Result<Option<ActualStateRow>, HandlerError> {
            Ok(self.latest_actual.lock().await.clone())
        }

        async fn load_latest_desired_state(
            &self,
            _node_id: &str,
        ) -> Result<Option<DesiredStateRow>, HandlerError> {
            Ok(Some(self.desired.clone()))
        }

        async fn load_program_image(
            &self,
            _program_hash: &[u8],
        ) -> Result<Option<ProgramImageRow>, HandlerError> {
            Ok(None)
        }

        async fn store_program_image(&self, _row: &ProgramImageRow) -> Result<(), HandlerError> {
            Ok(())
        }

        async fn append_sensor_data(&self, _row: &SensorDataRow) -> Result<(), HandlerError> {
            Ok(())
        }
    }

    fn desired_row(
        node_id: &str,
        desired_assigned_program_hash: Option<Vec<u8>>,
        desired_schedule_interval_s: Option<u32>,
        timestamp_ms: u64,
    ) -> DesiredStateRow {
        DesiredStateRow {
            row_key: next_history_row_key(timestamp_ms).unwrap(),
            node_id: node_id.to_string(),
            desired_assigned_program_hash,
            desired_schedule_interval_s,
            timestamp_ms,
        }
    }

    fn sample_actual_state_with_timestamp(
        node_id: &str,
        current_program_hash: Option<&[u8]>,
        assigned_program_hash: Option<&[u8]>,
        schedule_interval_s: Option<u32>,
        timestamp_ms: u64,
    ) -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("node".to_string())),
            map_entry(3, Value::Text(node_id.to_string())),
            map_entry(4, opt_bytes_value(current_program_hash)),
            map_entry(5, opt_bytes_value(assigned_program_hash)),
            map_entry(6, Value::Integer(3300u64.into())),
            map_entry(7, Value::Integer(1u64.into())),
            map_entry(8, Value::Text("1.2.3".to_string())),
            map_entry(9, Value::Integer(timestamp_ms.into())),
            map_entry(10, Value::Map(Vec::new())),
            map_entry(11, opt_u32_value(schedule_interval_s)),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_actual_state(
        node_id: &str,
        current_program_hash: Option<&[u8]>,
        assigned_program_hash: Option<&[u8]>,
        schedule_interval_s: Option<u32>,
    ) -> Vec<u8> {
        sample_actual_state_with_timestamp(
            node_id,
            current_program_hash,
            assigned_program_hash,
            schedule_interval_s,
            1234,
        )
    }

    fn sample_app_data(program_hash: &[u8], payload: &[u8]) -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
            map_entry(2, Value::Text("node-1".to_string())),
            map_entry(3, Value::Bytes(program_hash.to_vec())),
            map_entry(4, Value::Bytes(payload.to_vec())),
            map_entry(5, Value::Integer(321u64.into())),
            map_entry(6, Value::Text("app_data".to_string())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_app_data_with_readings(
        program_hash: &[u8],
        payload: &[u8],
        readings: &[(&str, i64)],
    ) -> Vec<u8> {
        let readings_cbor = Value::Map(
            readings
                .iter()
                .map(|(k, v)| (Value::Text(k.to_string()), Value::Integer((*v).into())))
                .collect(),
        );
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
            map_entry(2, Value::Text("node-1".to_string())),
            map_entry(3, Value::Bytes(program_hash.to_vec())),
            map_entry(4, Value::Bytes(payload.to_vec())),
            map_entry(5, Value::Integer(321u64.into())),
            map_entry(6, Value::Text("app_data".to_string())),
            map_entry(16, readings_cbor),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_unsupported() -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(0x99u64.into())),
            map_entry(2, Value::Text("ignored".to_string())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_key_escrow_request(key_hint: u16, request_id: [u8; 16]) -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(
                1,
                Value::Integer(sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_REQUEST.into()),
            ),
            map_entry(2, Value::Integer((key_hint as u64).into())),
            map_entry(3, Value::Bytes(request_id.to_vec())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_master_key_install() -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(
                1,
                Value::Integer(sonde_protocol::CONNECTOR_MSG_TYPE_MASTER_KEY_INSTALL.into()),
            ),
            map_entry(2, Value::Bytes(vec![0xAA; 32])),
            map_entry(3, Value::Bytes(vec![0xBB; 16])),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn first_actual_state_appends_row_without_publish_when_desired_is_absent() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                None,
                Some(60),
            ))
            .await
            .unwrap();

        let rows = store.actual_rows_for("node-1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].observed_current_program_hash, Some(vec![0xAA; 32]));
        assert_eq!(rows[0].observed_assigned_program_hash, None);
        assert_eq!(rows[0].observed_schedule_interval_s, Some(60));
        assert_eq!(rows[0].battery_mv, Some(3300));
        assert_eq!(rows[0].firmware_abi_version, Some(1));
        assert_eq!(rows[0].firmware_version.as_deref(), Some("1.2.3"));
        assert_eq!(rows[0].timestamp_ms, 1234);
        assert_eq!(store.desired_writes().await, 0);
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn divergence_publishes_full_desired_state_without_ephemeral() {
        let store = Arc::new(MemoryStore::default());
        store
            .append_desired(desired_row("node-1", Some(vec![0xBB; 32]), Some(120), 100))
            .await;
        store
            .append_program_image(&[0xBB; 32], b"program-image")
            .await;
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "desired-state");
        let desired = decode_map(&sends[0].1).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert_eq!(
            required_u64(&desired, 1, "msg_type").unwrap(),
            MSG_TYPE_DESIRED_STATE
        );
        assert_eq!(
            optional_bytes_field(desired_state, 1, "assigned_program_hash").unwrap(),
            Some(vec![0xBB; 32])
        );
        assert_eq!(
            optional_u32_field(desired_state, 2, "schedule_interval_s").unwrap(),
            Some(120)
        );
        assert_eq!(
            required_bytes(desired_state, 5, "assigned_program_elf").unwrap(),
            b"program-image"
        );
        assert_eq!(
            optional_text_field(desired_state, 6, "assigned_program_verification_profile").unwrap(),
            Some("resident".to_string())
        );
        assert!(map_get(desired_state, 3).is_none());
    }

    #[tokio::test]
    async fn aligned_actual_state_appends_history_without_publish() {
        let store = Arc::new(MemoryStore::default());
        store
            .append_desired(desired_row("node-1", Some(vec![0xAA; 32]), Some(60), 100))
            .await;
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xCC; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let rows = store.actual_rows_for("node-1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].observed_assigned_program_hash, Some(vec![0xCC; 32]));
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn repeated_actual_state_deliveries_are_all_recorded() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");
        let payload = sample_actual_state("node-1", Some(&[0xAA; 32]), Some(&[0xAA; 32]), Some(60));

        handler.handle_payload(&payload).await.unwrap();
        handler.handle_payload(&payload).await.unwrap();

        let rows = store.actual_rows_for("node-1").await;
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].row_key, rows[1].row_key);
    }

    #[tokio::test]
    async fn out_of_order_actual_state_is_retained_without_publish() {
        let store = Arc::new(MemoryStore::default());
        store
            .append_desired(desired_row("node-1", Some(vec![0xAA; 32]), Some(60), 100))
            .await;
        store
            .append_actual_state(&ActualStateRow {
                row_key: next_history_row_key(5000).unwrap(),
                entity_kind: "node".to_string(),
                node_id: "node-1".to_string(),
                observed_current_program_hash: Some(vec![0xAA; 32]),
                observed_assigned_program_hash: Some(vec![0xAA; 32]),
                observed_schedule_interval_s: Some(60),
                battery_mv: Some(3200),
                firmware_abi_version: Some(2),
                firmware_version: Some("2.0.0".to_string()),
                timestamp_ms: 5000,
                encrypted_psk_escrow: None,
                escrow_key_hint: None,
                escrow_key_version: None,
            })
            .await
            .unwrap();
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state_with_timestamp(
                "node-1",
                Some(&[0xBB; 32]),
                Some(&[0xBB; 32]),
                Some(120),
                1234,
            ))
            .await
            .unwrap();

        let rows = store.actual_rows_for("node-1").await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.timestamp_ms == 1234));
        let latest = store
            .load_latest_actual_state("node-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.timestamp_ms, 5000);
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn missing_desired_state_suppresses_publication() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn duplicate_actual_state_redelivery_retries_divergence_publish() {
        let store = Arc::new(MemoryStore::default());
        store
            .append_desired(desired_row("node-1", Some(vec![0xBB; 32]), Some(60), 10))
            .await;
        let publisher = Arc::new(FailOncePublisher {
            sends: Mutex::new(Vec::new()),
            failed: Mutex::new(false),
        });
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");
        let payload = sample_actual_state("node-1", Some(&[0xAA; 32]), Some(&[0xAA; 32]), Some(30));

        let err = handler.handle_payload(&payload).await.unwrap_err();
        assert!(err.to_string().contains("simulated first publish failure"));

        handler.handle_payload(&payload).await.unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        let desired = decode_map(&sends[0].1).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert_eq!(
            optional_bytes_field(desired_state, 1, "assigned_program_hash").unwrap(),
            Some(vec![0xBB; 32])
        );
        assert_eq!(
            optional_u32_field(desired_state, 2, "schedule_interval_s").unwrap(),
            Some(60)
        );
        drop(sends);

        let rows = store.actual_rows_for("node-1").await;
        assert_eq!(rows.len(), 2);
        let latest = store
            .load_latest_actual_state("node-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.timestamp_ms, 1234);
        assert_eq!(latest.observed_current_program_hash, Some(vec![0xAA; 32]));
    }

    #[tokio::test]
    async fn same_timestamp_row_key_mismatch_still_evaluates_current_delivery() {
        let store = Arc::new(SameTimestampDifferentLatestStore {
            latest_actual: ActualStateRow {
                row_key: next_history_row_key(1234).unwrap(),
                entity_kind: "node".to_string(),
                node_id: "node-1".to_string(),
                observed_current_program_hash: Some(vec![0xBB; 32]),
                observed_assigned_program_hash: Some(vec![0xBB; 32]),
                observed_schedule_interval_s: Some(60),
                battery_mv: Some(3200),
                firmware_abi_version: Some(1),
                firmware_version: Some("1.2.3".to_string()),
                timestamp_ms: 1234,
                encrypted_psk_escrow: None,
                escrow_key_hint: None,
                escrow_key_version: None,
            },
            desired: desired_row("node-1", Some(vec![0xCC; 32]), Some(60), 100),
            appended_rows: Mutex::new(Vec::new()),
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        assert_eq!(store.appended_rows.lock().await.len(), 1);
        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        let desired = decode_map(&sends[0].1).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert_eq!(
            optional_bytes_field(desired_state, 1, "assigned_program_hash").unwrap(),
            Some(vec![0xCC; 32])
        );
    }

    #[tokio::test]
    async fn mismatched_desired_row_node_id_is_rejected() {
        let store = Arc::new(MismatchedDesiredNodeStore {
            desired: desired_row("other-node", Some(vec![0xBB; 32]), Some(60), 100),
            latest_actual: Mutex::new(None),
            appended_rows: Mutex::new(Vec::new()),
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let err = handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(30),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains(
            "desired-state row node_id `other-node` did not match requested node `node-1`"
        ));
        assert_eq!(store.appended_rows.lock().await.len(), 1);
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn key_escrow_request_publishes_response_candidates() {
        let store = Arc::new(MemoryStore::default());
        store
            .set_escrow_blobs(0x1234, vec![vec![0xA1; 12], vec![0xB2; 24]])
            .await;
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "downstream");
        let request_id = [0xAB; 16];

        handler
            .handle_payload(&sample_key_escrow_request(0x1234, request_id))
            .await
            .unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "downstream");
        let response = decode_map(&sends[0].1).unwrap();
        assert_eq!(
            required_u64(&response, 1, "msg_type").unwrap(),
            sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_RESPONSE
        );
        assert_eq!(
            required_bytes(&response, 2, "request_id").unwrap(),
            request_id.to_vec()
        );
        assert_eq!(required_u64(&response, 4, "key_hint").unwrap(), 0x1234);
        let candidates = match map_get(&response, 3) {
            Some(Value::Array(values)) => values,
            other => panic!("expected candidates array, got {other:?}"),
        };
        assert_eq!(candidates.len(), 2);
        match &candidates[0] {
            Value::Bytes(bytes) => assert_eq!(bytes, &vec![0xA1; 12]),
            other => panic!("expected first candidate bytes, got {other:?}"),
        }
        match &candidates[1] {
            Value::Bytes(bytes) => assert_eq!(bytes, &vec![0xB2; 24]),
            other => panic!("expected second candidate bytes, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn master_key_install_is_relayed_verbatim() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "downstream");
        let payload = sample_master_key_install();

        handler.handle_payload(&payload).await.unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "downstream");
        assert_eq!(sends[0].1, payload);
    }

    #[tokio::test]
    async fn unsupported_messages_do_not_mutate_state() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler.handle_payload(&sample_unsupported()).await.unwrap();

        assert!(store.actual_rows.lock().await.is_empty());
        assert!(store.desired_rows.lock().await.is_empty());
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[test]
    fn extract_trigger_payload_decodes_base64_data_field() {
        let raw = vec![1u8, 2, 3, 4];
        let body = serde_json::json!({
            "Data": base64::engine::general_purpose::STANDARD.encode(&raw)
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_decodes_uppercase_data_message_field() {
        let raw = vec![9u8, 8, 7, 6];
        let body = serde_json::json!({
            "Data": {
                "message": base64::engine::general_purpose::STANDARD.encode(&raw)
            }
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_decodes_lowercase_data_message_field() {
        let raw = vec![5u8, 4, 3, 2];
        let body = serde_json::json!({
            "data": {
                "message": base64::engine::general_purpose::STANDARD.encode(&raw)
            }
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_decodes_body_field() {
        let raw = vec![7u8, 7, 7];
        let body = serde_json::json!({
            "Body": base64::engine::general_purpose::STANDARD.encode(&raw)
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_decodes_lowercase_body_field() {
        let raw = vec![6u8, 6, 6];
        let body = serde_json::json!({
            "body": base64::engine::general_purpose::STANDARD.encode(&raw)
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_decodes_single_lowercase_data_binding() {
        let raw = vec![0xAAu8, 0xBB, 0xCC];
        let body = serde_json::json!({
            "data": {
                "unexpectedBinding": base64::engine::general_purpose::STANDARD.encode(&raw)
            }
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_rejects_ambiguous_lowercase_data_bindings() {
        let body = serde_json::json!({
            "data": {
                "bindingA": "aGVsbG8=",
                "bindingB": "d29ybGQ="
            }
        });
        let err =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap_err();
        assert!(err
            .to_string()
            .contains("must contain exactly one binding payload"));
    }

    #[test]
    fn history_row_keys_sort_newer_timestamps_first() {
        let newer = next_history_row_key(200).unwrap();
        let older = next_history_row_key(100).unwrap();
        assert!(newer < older);
    }

    #[test]
    fn history_row_keys_sort_later_appends_first_for_equal_timestamps_within_process() {
        let first = next_history_row_key(1234).unwrap();
        let second = next_history_row_key(1234).unwrap();
        assert!(second < first);
    }

    #[test]
    fn actual_state_entity_round_trips_table_safe_partition_key() {
        let row = ActualStateRow {
            row_key: next_history_row_key(1234).unwrap(),
            entity_kind: "node".to_string(),
            node_id: "node/with?#unsafe\\chars".to_string(),
            observed_current_program_hash: Some(vec![0x22; 32]),
            observed_assigned_program_hash: Some(vec![0x33; 32]),
            observed_schedule_interval_s: Some(30),
            battery_mv: Some(3300),
            firmware_abi_version: Some(1),
            firmware_version: Some("1.2.3".to_string()),
            timestamp_ms: 1234,
            encrypted_psk_escrow: None,
            escrow_key_hint: None,
            escrow_key_version: None,
        };

        let entity = ActualStateEntity::try_from(row.clone()).unwrap();
        assert_eq!(
            entity.partition_key,
            "n:6a096929d88b3094bc781fbd0fb8dc060aa75ba06339ac3a706aa06f5ac9fedd"
        );
        assert_eq!(ActualStateRow::try_from(entity).unwrap(), row);
    }

    #[test]
    fn desired_state_entity_round_trips() {
        let row = desired_row("node-1", Some(vec![0x11; 32]), Some(60), 1234);
        let entity = DesiredStateEntity::try_from(row.clone()).unwrap();
        assert_eq!(DesiredStateRow::try_from(entity).unwrap(), row);
    }

    #[test]
    fn encode_desired_state_omits_program_image_when_absent() {
        let encoded = encode_desired_state(
            &desired_row("node-1", Some(vec![0x11; 32]), Some(60), 1234),
            None,
        )
        .unwrap();
        let desired = decode_map(&encoded).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert!(map_get(desired_state, 5).is_none());
    }

    #[test]
    fn long_node_ids_still_produce_bounded_partition_keys() {
        let node_id = "a".repeat(4096);
        let partition_key = encode_node_partition_key(&node_id);
        assert_eq!(partition_key.len(), 66);
    }

    #[test]
    fn actual_state_rejects_non_sha256_program_hash() {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("node".to_string())),
            map_entry(3, Value::Text("node-1".to_string())),
            map_entry(4, Value::Bytes(vec![0xAA; 31])),
            map_entry(5, Value::Null),
            map_entry(6, Value::Integer(3300u64.into())),
            map_entry(7, Value::Integer(1u64.into())),
            map_entry(8, Value::Text("1.2.3".to_string())),
            map_entry(9, Value::Integer(1234u64.into())),
            map_entry(10, Value::Map(Vec::new())),
            map_entry(11, Value::Integer(60u64.into())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();

        let err = decode_connector_message(&bytes).unwrap_err();
        assert!(err
            .to_string()
            .contains("`current_program_hash` must be exactly 32 bytes"));
    }

    #[test]
    fn app_data_rejects_non_sha256_program_hash() {
        let payload = sample_app_data(&[0x44; 31], &[1, 2, 3]);
        let err = decode_connector_message(&payload).unwrap_err();
        assert!(err
            .to_string()
            .contains("`program_hash` must be exactly 32 bytes"));
    }

    #[test]
    fn desired_state_row_decode_fails_closed_on_invalid_hex() {
        let err = DesiredStateRow::try_from(DesiredStateEntity {
            partition_key: encode_node_partition_key("node-1"),
            row_key: next_history_row_key(1234).unwrap(),
            node_id: "node-1".to_string(),
            desired_assigned_program_hash: Some("not-hex".to_string()),
            desired_schedule_interval_s: Some(60),
            timestamp_ms: 1234,
        })
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("`desired_assigned_program_hash` must contain valid hex"));
    }

    #[tokio::test]
    async fn actual_state_table_write_failures_are_surfaced() {
        let store = Arc::new(FailingStore {
            append_actual_error: Some("append failed".to_string()),
            load_desired_error: None,
            load_program_image_error: None,
            latest_actual: Mutex::new(None),
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let err = handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                None,
                Some(60),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("append failed"));
    }

    #[tokio::test]
    async fn desired_state_read_failures_are_surfaced() {
        let store = Arc::new(FailingStore {
            append_actual_error: None,
            load_desired_error: Some("desired read failed".to_string()),
            load_program_image_error: None,
            latest_actual: Mutex::new(None),
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let err = handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                None,
                Some(60),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("desired read failed"));
    }

    #[tokio::test]
    async fn program_image_read_failures_are_surfaced() {
        let store = Arc::new(FailingStore {
            append_actual_error: None,
            load_desired_error: None,
            load_program_image_error: Some("program image read failed".to_string()),
            latest_actual: Mutex::new(None),
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let err = handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("program image read failed"));
    }

    #[test]
    fn extract_trigger_payload_strips_double_quoted_base64() {
        let raw = vec![0xABu8, 1, 2];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let quoted = format!("\"{b64}\"");
        let body = serde_json::json!({
            "Data": {
                "message": quoted
            }
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn deserialize_actual_state_entity_f64_timestamp() {
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": 1778385831916.0_f64
        });
        let entity: ActualStateEntity = serde_json::from_value(json).unwrap();
        assert_eq!(entity.timestamp_ms, 1778385831916);
    }

    #[test]
    fn deserialize_desired_state_entity_f64_timestamp() {
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": 1000.0_f64
        });
        let entity: DesiredStateEntity = serde_json::from_value(json).unwrap();
        assert_eq!(entity.timestamp_ms, 1000);
    }

    #[test]
    fn deserialize_rejects_fractional_timestamp() {
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": 1234.9
        });
        let err = serde_json::from_value::<ActualStateEntity>(json).unwrap_err();
        assert!(err.to_string().contains("not a valid u64"));
    }

    #[test]
    fn deserialize_string_timestamp_from_azure_tables() {
        // Azure Tables returns Edm.Int64 values as JSON strings.
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": "1778458015159"
        });
        let entity: DesiredStateEntity = serde_json::from_value(json).unwrap();
        assert_eq!(entity.timestamp_ms, 1778458015159);
    }

    #[test]
    fn deserialize_actual_state_string_timestamp_from_azure_tables() {
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": "1778385831916"
        });
        let entity: ActualStateEntity = serde_json::from_value(json).unwrap();
        assert_eq!(entity.timestamp_ms, 1778385831916);
    }

    #[test]
    fn deserialize_integer_timestamp() {
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": 1778385831916_u64
        });
        let entity: ActualStateEntity = serde_json::from_value(json).unwrap();
        assert_eq!(entity.timestamp_ms, 1778385831916);
    }

    #[test]
    fn deserialize_rejects_negative_integer_timestamp() {
        let json = serde_json::json!({
            "PartitionKey": "pk",
            "RowKey": "rk",
            "node_id": "n1",
            "timestamp_ms": -1
        });
        let err = serde_json::from_value::<ActualStateEntity>(json).unwrap_err();
        assert!(err.to_string().contains("not a valid u64"));
    }

    // ── Program Ingest tests (T-WEB-0301 through T-WEB-0308) ──

    /// Build a minimal valid BPF ELF with a `sonde` section containing the
    /// given BPF bytecode. This mirrors `make_sonde_elf` from
    /// `crates/sonde-gateway/src/program.rs` (test-only).
    fn make_test_elf(bpf_code: &[u8]) -> Vec<u8> {
        let shstrtab: &[u8] = b"\0sonde\0.shstrtab\0";

        let text_offset: u64 = 64;
        let shstrtab_offset: u64 = text_offset + bpf_code.len() as u64;
        let shdr_offset: u64 = shstrtab_offset + shstrtab.len() as u64;

        let mut elf = Vec::new();
        elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf.push(2); // ELFCLASS64
        elf.push(1); // ELFDATA2LSB
        elf.push(1); // EI_VERSION
        elf.extend_from_slice(&[0; 9]);
        elf.extend_from_slice(&1u16.to_le_bytes()); // ET_REL
        elf.extend_from_slice(&247u16.to_le_bytes()); // EM_BPF
        elf.extend_from_slice(&1u32.to_le_bytes());
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        elf.extend_from_slice(&0u64.to_le_bytes()); // e_phoff
        elf.extend_from_slice(&shdr_offset.to_le_bytes());
        elf.extend_from_slice(&0u32.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&0u16.to_le_bytes());
        elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        elf.extend_from_slice(&3u16.to_le_bytes()); // e_shnum
        elf.extend_from_slice(&2u16.to_le_bytes()); // e_shstrndx
        assert_eq!(elf.len(), 64);
        elf.extend_from_slice(bpf_code);
        elf.extend_from_slice(shstrtab);

        // Null section header
        elf.extend_from_slice(&[0u8; 64]);

        // sonde section header
        let mut sh = [0u8; 64];
        sh[0..4].copy_from_slice(&1u32.to_le_bytes());
        sh[4..8].copy_from_slice(&1u32.to_le_bytes()); // SHT_PROGBITS
        let flags: u64 = 0x6; // SHF_ALLOC | SHF_EXECINSTR
        sh[8..16].copy_from_slice(&flags.to_le_bytes());
        sh[24..32].copy_from_slice(&text_offset.to_le_bytes());
        sh[32..40].copy_from_slice(&(bpf_code.len() as u64).to_le_bytes());
        sh[48..56].copy_from_slice(&8u64.to_le_bytes());
        elf.extend_from_slice(&sh);

        // .shstrtab section header
        let mut sh2 = [0u8; 64];
        sh2[0..4].copy_from_slice(&7u32.to_le_bytes());
        sh2[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
        sh2[24..32].copy_from_slice(&shstrtab_offset.to_le_bytes());
        sh2[32..40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());
        sh2[48..56].copy_from_slice(&1u64.to_le_bytes());
        elf.extend_from_slice(&sh2);

        elf
    }

    fn minimal_bpf_code() -> [u8; 16] {
        [
            0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
            0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
        ]
    }

    fn make_ingest_handler() -> (
        Arc<MemoryStore>,
        AzureHandler<MemoryStore, RecordingPublisher>,
    ) {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(store.clone(), publisher, "downstream-queue");
        (store, handler)
    }

    fn make_ingest_body(
        elf: &[u8],
        source_filename: Option<&str>,
        abi_version: Option<u32>,
        verification_profile: Option<&str>,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "elf": base64::engine::general_purpose::STANDARD.encode(elf),
        });
        if let Some(name) = source_filename {
            body["source_filename"] = serde_json::json!(name);
        }
        if let Some(abi) = abi_version {
            body["abi_version"] = serde_json::json!(abi);
        }
        if let Some(profile) = verification_profile {
            body["verification_profile"] = serde_json::json!(profile);
        }
        body
    }

    // T-WEB-0301: ProgramIngest accepts ELF + metadata via JSON POST
    // Also verifies all metadata fields are passed to the store (partial
    // coverage toward T-WEB-0304; full integration coverage is planned).
    #[tokio::test]
    async fn program_ingest_accepts_valid_elf() {
        let (store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, Some("sensor.o"), Some(1), Some("resident"));
        let resp = handler.handle_program_ingest(&body).await.unwrap();

        assert!(!resp.program_hash.is_empty());
        assert!(resp.size > 0);
        assert_eq!(resp.abi_version, Some(1));
        assert_eq!(resp.source_filename.as_deref(), Some("sensor.o"));

        // Verify the image was stored and can be loaded back.
        let hash_bytes = hex::decode(&resp.program_hash).unwrap();
        let stored = store.load_program_image(&hash_bytes).await.unwrap();
        assert!(stored.is_some());

        // Assert all metadata fields were passed to storage.
        let rows = store.stored_program_rows.lock().await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(hex::encode(&row.program_hash), resp.program_hash);
        assert_eq!(row.source_filename.as_deref(), Some("sensor.o"));
        assert_eq!(row.abi_version, Some(1));
        assert_eq!(row.size_bytes, resp.size);
        assert_eq!(row.verification_profile, "resident");
        assert!(!row.created_at.is_empty());
        assert!(!row.cbor_image.is_empty());
    }

    // T-WEB-0302: Prevail verification runs; invalid ELF rejected
    #[tokio::test]
    async fn program_ingest_rejects_invalid_elf() {
        let (_store, handler) = make_ingest_handler();
        let body = make_ingest_body(&[0xDE, 0xAD, 0xBE, 0xEF], None, None, None);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 422);
        assert!(err.message.contains("verification/ingestion failed"));
    }

    // T-WEB-0303: Program hash matches gateway computation
    #[tokio::test]
    async fn program_ingest_hash_matches_gateway() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, None, None, None);
        let resp = handler.handle_program_ingest(&body).await.unwrap();

        // Independently compute the hash via ProgramLibrary.
        let lib = sonde_gateway::program::ProgramLibrary::new();
        let record = lib
            .ingest_elf(&elf, sonde_gateway::program::VerificationProfile::Resident)
            .unwrap();
        assert_eq!(resp.program_hash, hex::encode(&record.hash));
    }

    // T-WEB-0305: Success returns hash+metadata; failure returns diagnostics
    #[tokio::test]
    async fn program_ingest_success_returns_metadata() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, Some("my-prog.o"), Some(42), Some("resident"));
        let resp = handler.handle_program_ingest(&body).await.unwrap();
        assert_eq!(resp.abi_version, Some(42));
        assert_eq!(resp.source_filename.as_deref(), Some("my-prog.o"));
        assert_eq!(resp.program_hash.len(), 64); // SHA-256 hex = 64 chars
    }

    #[tokio::test]
    async fn program_ingest_failure_returns_diagnostics() {
        let (_store, handler) = make_ingest_handler();
        let body = make_ingest_body(&[0x7f, b'E', b'L', b'F'], None, None, None);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 422);
        assert!(!err.message.is_empty());
    }

    // T-WEB-0306: Oversized programs rejected
    #[tokio::test]
    async fn program_ingest_rejects_oversized_elf() {
        let (_store, handler) = make_ingest_handler();
        let big_elf = vec![0u8; MAX_ELF_UPLOAD_SIZE + 1];
        let body = make_ingest_body(&big_elf, None, None, None);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 413);
        assert!(err.message.contains("exceeds limit"));
    }

    #[tokio::test]
    async fn program_ingest_exact_size_limit_not_rejected_as_too_large() {
        let (_store, handler) = make_ingest_handler();
        // Exactly at the limit — should NOT be rejected as 413.
        // It will fail later as invalid ELF (422), but that's expected.
        let at_limit_elf = vec![0u8; MAX_ELF_UPLOAD_SIZE];
        let body = make_ingest_body(&at_limit_elf, None, None, None);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_ne!(
            err.status_code, 413,
            "exact-boundary upload must not be rejected as too large"
        );
    }

    // T-WEB-0307: Empty ELF rejected
    #[tokio::test]
    async fn program_ingest_rejects_empty_elf() {
        let (_store, handler) = make_ingest_handler();
        let body = make_ingest_body(&[], None, None, None);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("must not be empty"));
    }

    // T-WEB-0308: source_filename normalized to basename
    #[tokio::test]
    async fn program_ingest_normalizes_source_filename() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, Some("/path/to/sensor.o"), None, None);
        let resp = handler.handle_program_ingest(&body).await.unwrap();
        assert_eq!(resp.source_filename.as_deref(), Some("sensor.o"));
    }

    #[tokio::test]
    async fn program_ingest_missing_elf_field() {
        let (_store, handler) = make_ingest_handler();
        let body = serde_json::json!({"source_filename": "test.o"});
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("`elf` field"));
    }

    #[tokio::test]
    async fn program_ingest_invalid_base64() {
        let (_store, handler) = make_ingest_handler();
        let body = serde_json::json!({"elf": "not-valid-base64!!!"});
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("base64"));
    }

    #[tokio::test]
    async fn program_ingest_invalid_verification_profile() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, None, None, Some("unknown-profile"));
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("verification_profile"));
    }

    #[tokio::test]
    async fn program_ingest_rejects_invalid_abi_version() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let mut body = make_ingest_body(&elf, None, None, None);
        body["abi_version"] = serde_json::json!("not-a-number");
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("abi_version"));
    }

    #[tokio::test]
    async fn program_ingest_defaults_to_resident_profile() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        // No verification_profile in body — should default to resident.
        let body = make_ingest_body(&elf, None, None, None);
        let resp = handler.handle_program_ingest(&body).await.unwrap();
        assert!(!resp.program_hash.is_empty());
    }

    #[tokio::test]
    async fn program_ingest_ephemeral_profile() {
        let (store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, None, None, Some("ephemeral"));
        let resp = handler.handle_program_ingest(&body).await.unwrap();
        assert!(!resp.program_hash.is_empty());

        let rows = store.stored_program_rows.lock().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verification_profile, "ephemeral");
    }

    #[tokio::test]
    async fn program_ingest_non_string_verification_profile_rejected() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let mut body = make_ingest_body(&elf, None, None, None);
        body["verification_profile"] = serde_json::json!(42);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("verification_profile"));
    }

    #[tokio::test]
    async fn program_ingest_is_idempotent() {
        let (store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, Some("v1.o"), Some(1), None);

        let resp1 = handler.handle_program_ingest(&body).await.unwrap();
        let body2 = make_ingest_body(&elf, Some("v2.o"), Some(2), None);
        let resp2 = handler.handle_program_ingest(&body2).await.unwrap();

        // Same hash (same ELF = same CBOR image).
        assert_eq!(resp1.program_hash, resp2.program_hash);

        // Stored image is still valid.
        let hash_bytes = hex::decode(&resp1.program_hash).unwrap();
        let stored = store.load_program_image(&hash_bytes).await.unwrap();
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn program_ingest_store_failure_returns_500() {
        struct FailingProgramStore;

        #[async_trait]
        impl HandlerStore for FailingProgramStore {
            async fn append_actual_state(&self, _: &ActualStateRow) -> Result<(), HandlerError> {
                Ok(())
            }
            async fn load_latest_actual_state(
                &self,
                _: &str,
            ) -> Result<Option<ActualStateRow>, HandlerError> {
                Ok(None)
            }
            async fn load_latest_desired_state(
                &self,
                _: &str,
            ) -> Result<Option<DesiredStateRow>, HandlerError> {
                Ok(None)
            }
            async fn load_program_image(
                &self,
                _: &[u8],
            ) -> Result<Option<ProgramImageRow>, HandlerError> {
                Ok(None)
            }
            async fn store_program_image(&self, _: &ProgramImageRow) -> Result<(), HandlerError> {
                Err(HandlerError::Store("simulated storage failure".to_string()))
            }
            async fn append_sensor_data(&self, _: &SensorDataRow) -> Result<(), HandlerError> {
                Ok(())
            }
        }

        let store = Arc::new(FailingProgramStore);
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(store, publisher, "downstream-queue");

        let elf = make_test_elf(&minimal_bpf_code());
        let body = make_ingest_body(&elf, None, None, None);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 500);
        assert!(err.message.contains("store program failed"));
    }

    #[tokio::test]
    async fn program_ingest_rejects_non_string_source_filename() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let mut body = make_ingest_body(&elf, None, None, None);
        body["source_filename"] = serde_json::json!(123);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("source_filename"));
    }

    #[tokio::test]
    async fn program_ingest_rejects_abi_version_above_i32_max() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let mut body = make_ingest_body(&elf, None, None, None);
        body["abi_version"] = serde_json::json!(i32::MAX as u64 + 1);
        let err = handler.handle_program_ingest(&body).await.unwrap_err();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("abi_version"));
    }

    #[tokio::test]
    async fn program_ingest_accepts_abi_version_at_i32_max() {
        let (_store, handler) = make_ingest_handler();
        let elf = make_test_elf(&minimal_bpf_code());
        let mut body = make_ingest_body(&elf, None, None, None);
        body["abi_version"] = serde_json::json!(i32::MAX);
        let resp = handler.handle_program_ingest(&body).await.unwrap();
        assert_eq!(resp.abi_version, Some(i32::MAX as u32));
    }

    #[test]
    fn chrono_iso8601_utc_now_has_valid_format() {
        let ts = chrono_iso8601_utc_now();
        // Must match YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "unexpected length: {ts}");
        assert!(ts.ends_with('Z'), "must end with Z: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        // Year must be >= 2025
        let year: u32 = ts[0..4].parse().unwrap();
        assert!(year >= 2025, "year {year} too low");
    }

    // Test the HTTP trigger envelope extraction.
    #[test]
    fn extract_http_trigger_body_parses_envelope() {
        let inner_json = serde_json::json!({"elf": "AAAA", "source_filename": "test.o"});
        let envelope = serde_json::json!({
            "Data": {
                "req": {
                    "Body": serde_json::to_string(&inner_json).unwrap(),
                    "Headers": {"Content-Type": "application/json"}
                }
            }
        });
        let body = extract_http_trigger_body(serde_json::to_string(&envelope).unwrap().as_bytes())
            .unwrap();
        assert_eq!(body.get("elf").unwrap().as_str().unwrap(), "AAAA");
        assert_eq!(
            body.get("source_filename").unwrap().as_str().unwrap(),
            "test.o"
        );
    }

    #[test]
    fn extract_http_trigger_body_rejects_missing_body() {
        let envelope = serde_json::json!({"Data": {"req": {}}});
        let err = extract_http_trigger_body(serde_json::to_string(&envelope).unwrap().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("Data.req.Body"));
    }

    #[test]
    fn format_ingest_response_has_correct_structure() {
        let body = serde_json::json!({"program_hash": "abcd", "size": 42});
        let resp = format_ingest_response(200, &body);
        let ret = resp.get("Outputs").unwrap().get("res").unwrap();
        assert_eq!(ret.get("statusCode").unwrap().as_u64().unwrap(), 200);
        assert_eq!(
            ret.get("headers")
                .unwrap()
                .get("Content-Type")
                .unwrap()
                .as_str()
                .unwrap(),
            "application/json"
        );
        let body_str = ret.get("body").unwrap().as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(body_str).unwrap();
        assert_eq!(
            parsed.get("program_hash").unwrap().as_str().unwrap(),
            "abcd"
        );
    }

    // ── SensorData table tests (AZH-0500, AZH-0501) ───────────────────

    /// T-AZH-0500: SensorData table row creation.
    /// Delivering a GW-0813 app-data message writes a SensorData row with
    /// correct node_id, program_hash, raw_payload, and row_key.
    #[tokio::test]
    async fn t_azh_0500_sensor_data_row_created_on_app_data() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];
        let payload = b"sensor-blob";

        let msg = sample_app_data(&program_hash, payload);
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        assert_eq!(rows.len(), 1, "exactly one SensorData row");
        assert_eq!(rows[0].node_id, "node-1");
        assert_eq!(rows[0].program_hash, program_hash.to_vec());
        assert_eq!(rows[0].raw_payload, payload.to_vec());
        assert_eq!(rows[0].timestamp_ms, 321);
        assert!(!rows[0].row_key.is_empty());
    }

    /// T-AZH-0500a: Two APP_DATA messages with the same timestamp_ms get
    /// distinct row keys.
    #[tokio::test]
    async fn t_azh_0500a_duplicate_timestamps_produce_distinct_row_keys() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];

        let msg = sample_app_data(&program_hash, b"blob-1");
        handler.handle_payload(&msg).await.unwrap();
        let msg = sample_app_data(&program_hash, b"blob-2");
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].row_key, rows[1].row_key);
    }

    /// T-AZH-0501: Decoded readings stored as JSON.
    /// A readings map with multiple entries is round-tripped to a JSON
    /// object in decoded_readings.
    #[tokio::test]
    async fn t_azh_0501_decoded_readings_stored_as_json() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];

        let msg = sample_app_data_with_readings(
            &program_hash,
            b"raw",
            &[("temperature_mc", 25125), ("humidity_pct", 4500)],
        );
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        assert_eq!(rows.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&rows[0].decoded_readings).unwrap();
        assert_eq!(parsed["temperature_mc"], 25125);
        assert_eq!(parsed["humidity_pct"], 4500);
    }

    /// T-AZH-0501a: Missing readings stored as empty string.
    /// When no readings key is present, decoded_readings is "".
    #[tokio::test]
    async fn t_azh_0501a_missing_readings_stored_as_empty_string() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];

        let msg = sample_app_data(&program_hash, b"no-decoder");
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decoded_readings, "");
    }

    /// T-AZH-0501b: Large int64 values encoded as JSON strings.
    /// Values above MAX_SAFE_INTEGER are JSON strings; values at or below
    /// are JSON numbers.
    #[tokio::test]
    async fn t_azh_0501b_large_int64_values_encoded_as_json_strings() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];

        // 9007199254740993 = MAX_SAFE_INTEGER + 2 (exceeds threshold)
        let above_safe = 9_007_199_254_740_993i64;
        let msg = sample_app_data_with_readings(
            &program_hash,
            b"raw",
            &[("big_value", above_safe), ("small_value", 42)],
        );
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        assert_eq!(rows.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&rows[0].decoded_readings).unwrap();
        // big_value must be a JSON string to preserve precision
        assert_eq!(
            parsed["big_value"],
            serde_json::Value::String("9007199254740993".to_string())
        );
        // small_value must be a JSON number
        assert_eq!(parsed["small_value"], serde_json::json!(42));
    }

    /// T-AZH-0501b (negative): Large negative int64 values also encoded as
    /// JSON strings.
    #[tokio::test]
    async fn t_azh_0501b_large_negative_int64_encoded_as_json_string() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];

        let below_safe = -9_007_199_254_740_993i64;
        let msg = sample_app_data_with_readings(&program_hash, b"raw", &[("neg_big", below_safe)]);
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        let parsed: serde_json::Value = serde_json::from_str(&rows[0].decoded_readings).unwrap();
        assert_eq!(
            parsed["neg_big"],
            serde_json::Value::String("-9007199254740993".to_string())
        );
    }

    /// APP_DATA messages write SensorData rows and succeed.
    #[tokio::test]
    async fn app_data_writes_sensor_data_and_succeeds() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let program_hash = [0x42u8; 32];
        let msg = sample_app_data(&program_hash, b"orphan-data");
        handler.handle_payload(&msg).await.unwrap();

        let rows = store.sensor_data_rows.lock().await;
        assert_eq!(rows.len(), 1, "SensorData row written");
        assert_eq!(rows[0].node_id, "node-1");
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[test]
    fn readings_to_json_returns_empty_for_none() {
        assert_eq!(readings_to_json(&None), "");
    }

    #[test]
    fn readings_to_json_returns_empty_for_empty_map() {
        assert_eq!(readings_to_json(&Some(BTreeMap::new())), "");
    }

    #[test]
    fn readings_to_json_encodes_safe_integers_as_numbers() {
        let mut map = BTreeMap::new();
        map.insert("temp".to_string(), 25125i64);
        map.insert("hum".to_string(), -100i64);
        let json = readings_to_json(&Some(map));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["temp"], 25125);
        assert_eq!(parsed["hum"], -100);
    }

    #[test]
    fn readings_to_json_boundary_max_safe_integer_is_number() {
        let mut map = BTreeMap::new();
        map.insert("at_limit".to_string(), 9_007_199_254_740_991i64);
        let json = readings_to_json(&Some(map));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["at_limit"].is_number());
    }

    #[test]
    fn readings_to_json_boundary_above_max_safe_integer_is_string() {
        let mut map = BTreeMap::new();
        map.insert("above_limit".to_string(), 9_007_199_254_740_992i64);
        let json = readings_to_json(&Some(map));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["above_limit"].is_string());
    }

    #[test]
    fn readings_to_json_i64_min_does_not_overflow() {
        let mut map = BTreeMap::new();
        map.insert("extreme".to_string(), i64::MIN);
        let json = readings_to_json(&Some(map));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["extreme"].is_string(), "i64::MIN exceeds safe range");
        assert_eq!(parsed["extreme"].as_str().unwrap(), i64::MIN.to_string());
    }

    #[test]
    fn sensor_data_entity_from_row_encodes_fields_correctly() {
        let row = SensorDataRow {
            row_key: "000000000000fcb6:fffffffffffffffe:abcdef".to_string(),
            node_id: "node-42".to_string(),
            timestamp_ms: 999,
            program_hash: vec![0x42u8; 32],
            raw_payload: b"hello sensor".to_vec(),
            decoded_readings: r#"{"temp":25}"#.to_string(),
        };
        let entity = SensorDataEntity::try_from(row.clone()).unwrap();
        assert_eq!(entity.row_key, row.row_key);
        assert_eq!(entity.node_id, "node-42");
        assert_eq!(entity.timestamp_ms, 999);
        assert_eq!(entity.program_hash, hex::encode(&row.program_hash));
        assert_eq!(entity.program_hash.len(), 64);
        assert_eq!(
            entity.raw_payload,
            base64::engine::general_purpose::STANDARD.encode(b"hello sensor")
        );
        assert_eq!(entity.decoded_readings, r#"{"temp":25}"#);
        // PartitionKey is "n:" + SHA-256 hex of node_id
        assert!(entity.partition_key.starts_with("n:"));
        assert_eq!(entity.partition_key.len(), 2 + 64);
    }

    // ── decode_optional_readings error path tests ──────────────────────

    #[test]
    fn decode_key_escrow_pubkey_rejects_non_text_fingerprint_words() {
        let value = Value::Map(vec![
            map_entry(
                1,
                Value::Integer(sonde_protocol::CONNECTOR_MSG_TYPE_KEY_ESCROW_PUBKEY.into()),
            ),
            map_entry(2, Value::Bytes(vec![0x42u8; 32])),
            map_entry(3, Value::Integer(1u64.into())),
            map_entry(4, Value::Integer(2u64.into())),
            map_entry(
                5,
                Value::Array(vec![
                    Value::Text("abandon".to_string()),
                    Value::Integer(7u64.into()),
                ]),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        let err = decode_connector_message(&bytes).unwrap_err();
        assert!(
            err.to_string()
                .contains("fingerprint_words array contains non-text entry"),
            "expected fingerprint_words error, got: {err}"
        );
    }

    #[test]
    fn decode_readings_rejects_non_map_value() {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
            map_entry(2, Value::Text("node-1".to_string())),
            map_entry(3, Value::Bytes(vec![0x42u8; 32])),
            map_entry(4, Value::Bytes(b"payload".to_vec())),
            map_entry(5, Value::Integer(100u64.into())),
            map_entry(16, Value::Text("not-a-map".to_string())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        let err = decode_connector_message(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("CBOR map or null"),
            "expected map-type error, got: {err}"
        );
    }

    #[test]
    fn decode_readings_rejects_non_text_key() {
        let readings = Value::Map(vec![(
            Value::Integer(42.into()),
            Value::Integer(100.into()),
        )]);
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
            map_entry(2, Value::Text("node-1".to_string())),
            map_entry(3, Value::Bytes(vec![0x42u8; 32])),
            map_entry(4, Value::Bytes(b"payload".to_vec())),
            map_entry(5, Value::Integer(100u64.into())),
            map_entry(16, readings),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        let err = decode_connector_message(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("key must be text"),
            "expected text-key error, got: {err}"
        );
    }

    #[test]
    fn decode_readings_rejects_non_integer_value() {
        let readings = Value::Map(vec![(
            Value::Text("temp".to_string()),
            Value::Text("not-a-number".to_string()),
        )]);
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
            map_entry(2, Value::Text("node-1".to_string())),
            map_entry(3, Value::Bytes(vec![0x42u8; 32])),
            map_entry(4, Value::Bytes(b"payload".to_vec())),
            map_entry(5, Value::Integer(100u64.into())),
            map_entry(16, readings),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        let err = decode_connector_message(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("signed integer"),
            "expected integer-value error, got: {err}"
        );
    }

    #[tokio::test]
    async fn default_escrow_store_methods_return_errors() {
        struct DefaultEscrowStore;

        #[async_trait]
        impl HandlerStore for DefaultEscrowStore {
            async fn append_actual_state(&self, _: &ActualStateRow) -> Result<(), HandlerError> {
                Ok(())
            }
            async fn load_latest_actual_state(
                &self,
                _: &str,
            ) -> Result<Option<ActualStateRow>, HandlerError> {
                Ok(None)
            }
            async fn load_latest_desired_state(
                &self,
                _: &str,
            ) -> Result<Option<DesiredStateRow>, HandlerError> {
                Ok(None)
            }
            async fn load_program_image(
                &self,
                _: &[u8],
            ) -> Result<Option<ProgramImageRow>, HandlerError> {
                Ok(None)
            }
            async fn store_program_image(&self, _: &ProgramImageRow) -> Result<(), HandlerError> {
                Ok(())
            }
            async fn append_sensor_data(&self, _: &SensorDataRow) -> Result<(), HandlerError> {
                Ok(())
            }
        }

        let store = DefaultEscrowStore;
        let err = store
            .store_gateway_escrow_pubkey(&[0x42u8; 32], 1, 2)
            .await
            .unwrap_err();
        assert!(
            matches!(err, HandlerError::Store(message) if message.contains("store_gateway_escrow_pubkey not implemented"))
        );

        let err = store
            .load_escrow_blobs_by_key_hint(0x1234, 4)
            .await
            .unwrap_err();
        assert!(
            matches!(err, HandlerError::Store(message) if message.contains("load_escrow_blobs_by_key_hint not implemented"))
        );
    }

    #[tokio::test]
    async fn gateway_actual_state_is_informational_only() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "downstream");

        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("gateway".to_string())),
            map_entry(3, Value::Text("gateway-1".to_string())),
            map_entry(9, Value::Integer(1234u64.into())),
        ]);
        let mut payload = Vec::new();
        ciborium::into_writer(&value, &mut payload).unwrap();

        handler.handle_payload(&payload).await.unwrap();
        assert!(store.actual_rows.lock().await.is_empty());
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn gateway_actual_state_persists_escrow_status_details() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "downstream");

        // Build status_details map: key 10 in the ACTUAL_STATE
        let status_details = Value::Map(vec![
            map_entry(1, Value::Text("ready".to_string())),
            map_entry(2, Value::Integer(3u64.into())),
            map_entry(3, Value::Bytes(vec![0xAA; 16])),
            map_entry(
                4,
                Value::Map(vec![
                    map_entry(1, Value::Integer(65536u64.into())),
                    map_entry(2, Value::Integer(3u64.into())),
                    map_entry(3, Value::Integer(1u64.into())),
                    map_entry(4, Value::Integer(1u64.into())),
                ]),
            ),
        ]);

        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("gateway".to_string())),
            map_entry(3, Value::Text("gateway-1".to_string())),
            map_entry(9, Value::Integer(1234u64.into())),
            map_entry(10, status_details),
        ]);
        let mut payload = Vec::new();
        ciborium::into_writer(&value, &mut payload).unwrap();

        handler.handle_payload(&payload).await.unwrap();
        let state = store.stored_escrow_state.lock().await;
        let details = state.as_ref().unwrap();
        assert_eq!(details.escrow_state, Some("ready".to_string()));
        assert_eq!(details.escrow_key_version, Some(3));
        assert_eq!(details.escrow_salt, Some(vec![0xAA; 16]));
        let kdf = details.escrow_kdf_params.as_ref().unwrap();
        assert_eq!(kdf.m_cost, 65536);
        assert_eq!(kdf.t_cost, 3);
        assert_eq!(kdf.p_cost, 1);
        assert_eq!(kdf.kdf_version, 1);
    }

    #[test]
    fn phone_actual_state_uses_phone_partition_key() {
        let row = ActualStateRow {
            row_key: next_history_row_key(1234).unwrap(),
            entity_kind: "phone".to_string(),
            node_id: "phone-42".to_string(),
            observed_current_program_hash: None,
            observed_assigned_program_hash: None,
            observed_schedule_interval_s: None,
            battery_mv: None,
            firmware_abi_version: None,
            firmware_version: None,
            timestamp_ms: 1234,
            encrypted_psk_escrow: Some(vec![0xBB; 48]),
            escrow_key_hint: Some(0x1234),
            escrow_key_version: Some(1),
        };
        let entity = ActualStateEntity::try_from(row).unwrap();
        assert!(
            entity.partition_key.starts_with("p:"),
            "phone entity should use 'p:' partition prefix, got: {}",
            entity.partition_key
        );
    }

    #[test]
    fn node_actual_state_uses_node_partition_key() {
        let row = ActualStateRow {
            row_key: next_history_row_key(1234).unwrap(),
            entity_kind: "node".to_string(),
            node_id: "node-1".to_string(),
            observed_current_program_hash: None,
            observed_assigned_program_hash: None,
            observed_schedule_interval_s: None,
            battery_mv: None,
            firmware_abi_version: None,
            firmware_version: None,
            timestamp_ms: 1234,
            encrypted_psk_escrow: None,
            escrow_key_hint: None,
            escrow_key_version: None,
        };
        let entity = ActualStateEntity::try_from(row).unwrap();
        assert!(
            entity.partition_key.starts_with("n:"),
            "node entity should use 'n:' partition prefix, got: {}",
            entity.partition_key
        );
    }

    #[tokio::test]
    async fn gateway_actual_state_stores_salt_first_writer_wins() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler = AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "downstream");

        let status_details = Value::Map(vec![
            map_entry(1, Value::Text("bootstrapping".to_string())),
            map_entry(3, Value::Bytes(vec![0xBB; 16])),
            map_entry(
                4,
                Value::Map(vec![
                    map_entry(1, Value::Integer(65536u64.into())),
                    map_entry(2, Value::Integer(3u64.into())),
                    map_entry(3, Value::Integer(1u64.into())),
                    map_entry(4, Value::Integer(1u64.into())),
                ]),
            ),
        ]);

        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("gateway".to_string())),
            map_entry(3, Value::Text("gateway-1".to_string())),
            map_entry(9, Value::Integer(1000u64.into())),
            map_entry(10, status_details),
        ]);
        let mut payload = Vec::new();
        ciborium::into_writer(&value, &mut payload).unwrap();

        // First write should succeed.
        handler.handle_payload(&payload).await.unwrap();
        assert_eq!(
            store.stored_salt.lock().await.as_deref(),
            Some([0xBBu8; 16].as_slice())
        );

        // Second write with different salt should be ignored (first-writer-wins).
        let status_details2 = Value::Map(vec![
            map_entry(1, Value::Text("ready".to_string())),
            map_entry(3, Value::Bytes(vec![0xCC; 16])),
        ]);
        let value2 = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("gateway".to_string())),
            map_entry(3, Value::Text("gateway-1".to_string())),
            map_entry(9, Value::Integer(2000u64.into())),
            map_entry(10, status_details2),
        ]);
        let mut payload2 = Vec::new();
        ciborium::into_writer(&value2, &mut payload2).unwrap();

        handler.handle_payload(&payload2).await.unwrap();
        // Salt should still be the original.
        assert_eq!(
            store.stored_salt.lock().await.as_deref(),
            Some([0xBBu8; 16].as_slice()),
            "salt should not be overwritten (first-writer-wins)"
        );
    }
}
