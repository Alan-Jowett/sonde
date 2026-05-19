// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use sonde_protocol::normalize_display_filename;
use tokio::sync::RwLock;

use crate::gateway_identity::GatewayIdentity;
use crate::phone_trust::PhonePskRecord;
use crate::program::ProgramRecord;
use crate::registry::NodeRecord;

/// Errors returned by storage operations.
#[derive(Debug, Clone)]
pub enum StorageError {
    /// The requested item was not found.
    NotFound(String),
    /// A generic internal error.
    Internal(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NotFound(msg) => write!(f, "not found: {}", msg),
            StorageError::Internal(msg) => write!(f, "storage error: {}", msg),
        }
    }
}

impl std::error::Error for StorageError {}

/// A handler routing record for persistent storage (GW-1401).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerRecord {
    /// `"*"` for catch-all or a 64-char lowercase hex SHA-256 hash.
    pub program_hash: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub reply_timeout_ms: Option<u64>,
}

/// Lightweight program metadata for human-facing displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramDisplayRecord {
    pub hash: Vec<u8>,
    pub source_filename: Option<String>,
}

/// Program metadata for admin/program listings.
///
/// Backends can override `list_program_summary_records()` to avoid loading image
/// blobs when serving metadata-only listings.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramSummaryRecord {
    pub hash: Vec<u8>,
    pub size: u32,
    pub verification_profile: crate::program::VerificationProfile,
    pub abi_version: Option<u32>,
    pub source_filename: Option<String>,
    pub has_decoder: bool,
}

/// Escrow keypair record for PSK key escrow (GW-2000).
#[derive(Debug, Clone)]
pub struct EscrowKeypairRecord {
    /// X25519 private key, AES-256-GCM encrypted with the master key.
    pub secret_enc: Vec<u8>,
    /// X25519 public key (32 bytes).
    pub public_key: [u8; 32],
    /// Monotonic key epoch, incremented on each regeneration.
    pub epoch: u64,
    /// Creation timestamp (Unix milliseconds).
    pub created_at: u64,
}

/// Pending key rotation record for crash-safe migration (GW-2007).
#[derive(Debug, Clone)]
pub struct PendingRotationRecord {
    /// New master key, AES-256-GCM encrypted with the OLD master key.
    pub new_master_key_enc: Vec<u8>,
    /// Target key version for this rotation.
    pub new_key_version: u64,
    /// Unique operation ID for idempotency.
    pub operation_id: [u8; 16],
    /// Whether the recovery private key has been rewrapped under the new key.
    pub privkey_rewrapped: bool,
    /// Rotation start timestamp (Unix milliseconds).
    pub started_at: u64,
}

/// Abstract storage backend for node registry and program library.
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Node registry ──────────────────────────────────────────
    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StorageError>;
    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StorageError>;
    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>, StorageError>;
    async fn upsert_node(&self, record: &NodeRecord) -> Result<(), StorageError>;
    /// Persist only the durable metadata reported in a WAKE.
    ///
    /// Implementations must not rewrite unrelated fields such as the encrypted
    /// PSK when only firmware metadata is being refreshed.
    async fn update_node_wake_metadata(
        &self,
        node_id: &str,
        firmware_abi_version: u32,
        firmware_version: &str,
    ) -> Result<(), StorageError>;
    /// Atomically set `current_program_hash` for a node, but only if the
    /// node's `assigned_program_hash` still equals the given `program_hash`.
    ///
    /// This is used for lost-PROGRAM_ACK recovery during WAKE processing: the
    /// node reports a program hash in its WAKE that matches the assigned hash,
    /// proving it has already installed the program even though the gateway
    /// never received the PROGRAM_ACK.
    ///
    /// The conditional check on `assigned_program_hash` prevents a stale WAKE
    /// snapshot from overwriting a concurrent reassignment.
    ///
    /// Returns `Ok(true)` if the update was applied, `Ok(false)` if no update
    /// was needed (assigned hash no longer matches, or `current_program_hash`
    /// already equals `program_hash`), or an error.
    async fn reconcile_current_program_hash(
        &self,
        node_id: &str,
        program_hash: &[u8],
    ) -> Result<bool, StorageError>;
    /// Insert a node only if no node with the same `node_id` exists.
    ///
    /// Returns `true` if the node was inserted, `false` if it already existed.
    async fn insert_node_if_not_exists(&self, record: &NodeRecord) -> Result<bool, StorageError>;
    async fn delete_node(&self, node_id: &str) -> Result<(), StorageError>;

    // ── Program library ────────────────────────────────────────
    async fn get_program(&self, hash: &[u8]) -> Result<Option<ProgramRecord>, StorageError>;
    async fn store_program(&self, record: &ProgramRecord) -> Result<(), StorageError>;
    async fn delete_program(&self, hash: &[u8]) -> Result<(), StorageError>;
    async fn list_programs(&self) -> Result<Vec<ProgramRecord>, StorageError>;
    async fn list_program_summary_records(
        &self,
    ) -> Result<Vec<ProgramSummaryRecord>, StorageError> {
        Ok(self
            .list_programs()
            .await?
            .into_iter()
            .map(|program| ProgramSummaryRecord {
                hash: program.hash,
                size: program.size,
                verification_profile: program.verification_profile,
                abi_version: program.abi_version,
                source_filename: program.source_filename,
                has_decoder: program.decoder_image.is_some(),
            })
            .collect())
    }
    async fn list_program_display_records(
        &self,
    ) -> Result<Vec<ProgramDisplayRecord>, StorageError> {
        Ok(self
            .list_programs()
            .await?
            .into_iter()
            .map(|program| ProgramDisplayRecord {
                hash: program.hash,
                source_filename: program.source_filename,
            })
            .collect())
    }

    /// Atomically replace all nodes and programs with the given sets.
    ///
    /// Implementations should perform the replacement in a single transaction
    /// where possible. The default implementation is non-atomic (delete-then-insert).
    async fn replace_state(
        &self,
        nodes: &[NodeRecord],
        programs: &[ProgramRecord],
    ) -> Result<(), StorageError> {
        // Default: non-atomic fallback for backends that don't support transactions.
        let existing_nodes = self.list_nodes().await?;
        for n in existing_nodes {
            self.delete_node(&n.node_id).await?;
        }
        let existing_programs = self.list_programs().await?;
        for p in existing_programs {
            self.delete_program(&p.hash).await?;
        }
        for program in programs {
            self.store_program(program).await?;
        }
        for node in nodes {
            self.upsert_node(node).await?;
        }
        Ok(())
    }

    // ── Gateway identity (GW-1200, GW-1201) ───────────────────
    async fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, StorageError>;
    async fn store_gateway_identity(&self, identity: &GatewayIdentity) -> Result<(), StorageError>;

    // ── Phone trust store (GW-1210) ────────────────────────────
    async fn list_phone_psks(&self) -> Result<Vec<PhonePskRecord>, StorageError>;
    async fn get_phone_psks_by_key_hint(
        &self,
        key_hint: u16,
    ) -> Result<Vec<PhonePskRecord>, StorageError>;
    async fn store_phone_psk(&self, record: &PhonePskRecord) -> Result<u32, StorageError>;
    async fn revoke_phone_psk(&self, phone_id: u32) -> Result<(), StorageError>;
    async fn delete_phone_psk(&self, phone_id: u32) -> Result<(), StorageError>;

    // ── Gateway config (GW-0808) ───────────────────────────────
    /// Retrieve a gateway configuration value by key.
    async fn get_config(&self, key: &str) -> Result<Option<String>, StorageError>;
    /// Set a gateway configuration value (insert or update).
    async fn set_config(&self, key: &str, value: &str) -> Result<(), StorageError>;

    /// Atomically replace all phone PSK registrations with the given set.
    ///
    /// `phone_id` values on the incoming records are ignored — each
    /// implementation assigns fresh IDs (auto-increment for SQLite,
    /// sequential counter for in-memory).
    ///
    /// Implementations should perform the replacement in a single transaction
    /// where possible. The default implementation is non-atomic (delete-then-insert).
    async fn replace_phone_psks(&self, records: &[PhonePskRecord]) -> Result<(), StorageError> {
        // Default: non-atomic fallback for backends that don't support transactions.
        let existing = self.list_phone_psks().await?;
        for p in existing {
            self.delete_phone_psk(p.phone_id).await?;
        }
        for p in records {
            self.store_phone_psk(p).await?;
        }
        Ok(())
    }

    // ── Handler routing (GW-1401) ──────────────────────────────

    /// List all handler records, ordered by `program_hash`.
    async fn list_handlers(&self) -> Result<Vec<HandlerRecord>, StorageError>;

    /// Add a handler record. Returns `true` if inserted, `false` if a
    /// handler with the same `program_hash` already exists (no-op).
    async fn add_handler(&self, record: &HandlerRecord) -> Result<bool, StorageError>;

    /// Remove a handler by `program_hash`. Returns `true` if a handler was
    /// removed, `false` if none matched.
    async fn remove_handler(&self, program_hash: &str) -> Result<bool, StorageError>;

    /// Replace all handler records with the given set.
    ///
    /// Implementations should perform the replacement in a single transaction
    /// where possible. The default implementation is non-atomic (delete-then-insert).
    async fn replace_handlers(&self, records: &[HandlerRecord]) -> Result<(), StorageError> {
        let existing = self.list_handlers().await?;
        for h in &existing {
            self.remove_handler(&h.program_hash).await?;
        }
        for h in records {
            self.add_handler(h).await?;
        }
        Ok(())
    }

    // ── PSK key escrow (GW-2000–GW-2007) ──────────────────────

    /// Retrieve the persisted escrow keypair, if any.
    async fn get_escrow_keypair(&self) -> Result<Option<EscrowKeypairRecord>, StorageError>;

    /// Persist an escrow keypair (insert or replace).
    async fn store_escrow_keypair(&self, record: &EscrowKeypairRecord) -> Result<(), StorageError>;

    /// Atomically check and record a key-management operation.
    /// Returns `true` if this is the first time the operation was seen (newly recorded),
    /// `false` if it was already processed (duplicate).
    async fn try_record_operation(&self, operation_id: &[u8; 16]) -> Result<bool, StorageError>;

    /// Retrieve a pending key rotation record, if any.
    async fn get_pending_rotation(&self) -> Result<Option<PendingRotationRecord>, StorageError>;

    /// Persist a pending key rotation record.
    async fn store_pending_rotation(
        &self,
        record: &PendingRotationRecord,
    ) -> Result<(), StorageError>;

    /// Delete the pending rotation record after successful completion.
    async fn delete_pending_rotation(&self) -> Result<(), StorageError>;
}

/// In-memory storage backend for testing.
pub struct InMemoryStorage {
    nodes: RwLock<HashMap<String, NodeRecord>>,
    programs: RwLock<HashMap<Vec<u8>, ProgramRecord>>,
    identity: RwLock<Option<GatewayIdentity>>,
    phone_psks: RwLock<Vec<PhonePskRecord>>,
    next_phone_id: RwLock<u32>,
    config: RwLock<HashMap<String, String>>,
    handlers: RwLock<Vec<HandlerRecord>>,
    escrow_keypair: RwLock<Option<EscrowKeypairRecord>>,
    escrow_operations: RwLock<std::collections::HashSet<Vec<u8>>>,
    pending_rotation: RwLock<Option<PendingRotationRecord>>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            programs: RwLock::new(HashMap::new()),
            identity: RwLock::new(None),
            phone_psks: RwLock::new(Vec::new()),
            next_phone_id: RwLock::new(1),
            config: RwLock::new(HashMap::new()),
            handlers: RwLock::new(Vec::new()),
            escrow_keypair: RwLock::new(None),
            escrow_operations: RwLock::new(std::collections::HashSet::new()),
            pending_rotation: RwLock::new(None),
        }
    }

    fn stored_node_record(record: &NodeRecord) -> NodeRecord {
        let mut stored = record.clone();
        stored.last_battery_mv = None;
        stored.last_seen = None;
        stored.battery_history.clear();
        stored
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Storage for InMemoryStorage {
    // ── Node registry ──────────────────────────────────────────

    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StorageError> {
        let nodes = self.nodes.read().await;
        Ok(nodes.values().cloned().collect())
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StorageError> {
        let nodes = self.nodes.read().await;
        Ok(nodes.get(node_id).cloned())
    }

    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>, StorageError> {
        let nodes = self.nodes.read().await;
        Ok(nodes
            .values()
            .filter(|n| n.key_hint == key_hint)
            .cloned()
            .collect())
    }

    async fn upsert_node(&self, record: &NodeRecord) -> Result<(), StorageError> {
        let mut nodes = self.nodes.write().await;
        nodes.insert(record.node_id.clone(), Self::stored_node_record(record));
        Ok(())
    }

    async fn update_node_wake_metadata(
        &self,
        node_id: &str,
        firmware_abi_version: u32,
        firmware_version: &str,
    ) -> Result<(), StorageError> {
        let mut nodes = self.nodes.write().await;
        let node = nodes
            .get_mut(node_id)
            .ok_or_else(|| StorageError::NotFound(format!("node `{node_id}`")))?;
        if node.firmware_abi_version == Some(firmware_abi_version)
            && node.firmware_version.as_deref() == Some(firmware_version)
        {
            return Ok(());
        }
        node.firmware_abi_version = Some(firmware_abi_version);
        node.firmware_version = Some(firmware_version.to_string());
        Ok(())
    }

    async fn insert_node_if_not_exists(&self, record: &NodeRecord) -> Result<bool, StorageError> {
        let mut nodes = self.nodes.write().await;
        use std::collections::hash_map::Entry;
        match nodes.entry(record.node_id.clone()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(e) => {
                e.insert(Self::stored_node_record(record));
                Ok(true)
            }
        }
    }

    async fn reconcile_current_program_hash(
        &self,
        node_id: &str,
        program_hash: &[u8],
    ) -> Result<bool, StorageError> {
        let mut nodes = self.nodes.write().await;
        let node = nodes
            .get_mut(node_id)
            .ok_or_else(|| StorageError::NotFound(format!("node `{node_id}`")))?;
        if node.assigned_program_hash.as_deref() != Some(program_hash) {
            return Ok(false);
        }
        if node.current_program_hash.as_deref() == Some(program_hash) {
            return Ok(false);
        }
        node.current_program_hash = Some(program_hash.to_vec());
        Ok(true)
    }

    async fn delete_node(&self, node_id: &str) -> Result<(), StorageError> {
        let mut nodes = self.nodes.write().await;
        nodes.remove(node_id);
        Ok(())
    }

    // ── Program library ────────────────────────────────────────

    async fn get_program(&self, hash: &[u8]) -> Result<Option<ProgramRecord>, StorageError> {
        let programs = self.programs.read().await;
        Ok(programs.get(hash).cloned())
    }

    async fn store_program(&self, record: &ProgramRecord) -> Result<(), StorageError> {
        let mut programs = self.programs.write().await;
        let mut stored = record.clone();
        stored.source_filename = normalize_display_filename(&stored.source_filename);
        programs.insert(stored.hash.clone(), stored);
        Ok(())
    }

    async fn delete_program(&self, hash: &[u8]) -> Result<(), StorageError> {
        let mut programs = self.programs.write().await;
        programs.remove(hash);
        Ok(())
    }

    async fn list_programs(&self) -> Result<Vec<ProgramRecord>, StorageError> {
        let programs = self.programs.read().await;
        Ok(programs.values().cloned().collect())
    }

    async fn list_program_summary_records(
        &self,
    ) -> Result<Vec<ProgramSummaryRecord>, StorageError> {
        let programs = self.programs.read().await;
        Ok(programs
            .values()
            .map(|program| ProgramSummaryRecord {
                hash: program.hash.clone(),
                size: program.size,
                verification_profile: program.verification_profile.clone(),
                abi_version: program.abi_version,
                source_filename: program.source_filename.clone(),
                has_decoder: program.decoder_image.is_some(),
            })
            .collect())
    }

    async fn list_program_display_records(
        &self,
    ) -> Result<Vec<ProgramDisplayRecord>, StorageError> {
        let programs = self.programs.read().await;
        Ok(programs
            .values()
            .map(|program| ProgramDisplayRecord {
                hash: program.hash.clone(),
                source_filename: program.source_filename.clone(),
            })
            .collect())
    }

    // ── Gateway identity ───────────────────────────────────────

    async fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, StorageError> {
        let identity = self.identity.read().await;
        Ok(identity.clone())
    }

    async fn store_gateway_identity(&self, identity: &GatewayIdentity) -> Result<(), StorageError> {
        let mut stored = self.identity.write().await;
        *stored = Some(identity.clone());
        Ok(())
    }

    // ── Phone trust store ──────────────────────────────────────

    async fn list_phone_psks(&self) -> Result<Vec<PhonePskRecord>, StorageError> {
        let psks = self.phone_psks.read().await;
        Ok(psks.clone())
    }

    async fn get_phone_psks_by_key_hint(
        &self,
        key_hint: u16,
    ) -> Result<Vec<PhonePskRecord>, StorageError> {
        let psks = self.phone_psks.read().await;
        Ok(psks
            .iter()
            .filter(|p| p.phone_key_hint == key_hint)
            .cloned()
            .collect())
    }

    async fn store_phone_psk(&self, record: &PhonePskRecord) -> Result<u32, StorageError> {
        use crate::phone_trust::PHONE_LABEL_MAX_BYTES;

        if record.label.len() > PHONE_LABEL_MAX_BYTES {
            return Err(StorageError::Internal(format!(
                "phone label exceeds {PHONE_LABEL_MAX_BYTES}-byte limit: {} bytes",
                record.label.len()
            )));
        }

        let mut psks = self.phone_psks.write().await;
        let mut next_id = self.next_phone_id.write().await;
        let id = *next_id;
        let mut stored = record.clone();
        stored.phone_id = id;
        *next_id = id
            .checked_add(1)
            .ok_or_else(|| StorageError::Internal("phone_id overflow".into()))?;
        psks.push(stored);
        Ok(id)
    }

    async fn revoke_phone_psk(&self, phone_id: u32) -> Result<(), StorageError> {
        let mut psks = self.phone_psks.write().await;
        let psk = psks
            .iter_mut()
            .find(|p| p.phone_id == phone_id)
            .ok_or_else(|| StorageError::NotFound(format!("phone_id {phone_id}")))?;
        psk.status = crate::phone_trust::PhonePskStatus::Revoked;
        Ok(())
    }

    async fn delete_phone_psk(&self, phone_id: u32) -> Result<(), StorageError> {
        let mut psks = self.phone_psks.write().await;
        psks.retain(|p| p.phone_id != phone_id);
        Ok(())
    }

    async fn replace_phone_psks(&self, records: &[PhonePskRecord]) -> Result<(), StorageError> {
        use crate::phone_trust::PHONE_LABEL_MAX_BYTES;

        for r in records {
            if r.label.len() > PHONE_LABEL_MAX_BYTES {
                return Err(StorageError::Internal(format!(
                    "phone label exceeds {PHONE_LABEL_MAX_BYTES}-byte limit: {} bytes",
                    r.label.len()
                )));
            }
        }

        let mut psks = self.phone_psks.write().await;
        let mut next_id = self.next_phone_id.write().await;
        psks.clear();
        for r in records {
            let id = *next_id;
            let mut stored = r.clone();
            stored.phone_id = id;
            *next_id = id
                .checked_add(1)
                .ok_or_else(|| StorageError::Internal("phone_id overflow".into()))?;
            psks.push(stored);
        }
        Ok(())
    }

    // ── Gateway config ─────────────────────────────────────────

    async fn get_config(&self, key: &str) -> Result<Option<String>, StorageError> {
        let config = self.config.read().await;
        Ok(config.get(key).cloned())
    }

    async fn set_config(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let mut config = self.config.write().await;
        config.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    // ── Handler routing ────────────────────────────────────────

    async fn list_handlers(&self) -> Result<Vec<HandlerRecord>, StorageError> {
        let handlers = self.handlers.read().await;
        let mut result = handlers.clone();
        result.sort_by(|a, b| a.program_hash.cmp(&b.program_hash));
        Ok(result)
    }

    async fn add_handler(&self, record: &HandlerRecord) -> Result<bool, StorageError> {
        let mut handlers = self.handlers.write().await;
        let key = record.program_hash.to_ascii_lowercase();
        if handlers.iter().any(|h| h.program_hash == key) {
            return Ok(false);
        }
        let mut stored = record.clone();
        stored.program_hash = key;
        handlers.push(stored);
        Ok(true)
    }

    async fn remove_handler(&self, program_hash: &str) -> Result<bool, StorageError> {
        let mut handlers = self.handlers.write().await;
        let key = program_hash.to_ascii_lowercase();
        let before = handlers.len();
        handlers.retain(|h| h.program_hash != key);
        Ok(handlers.len() < before)
    }

    async fn replace_handlers(&self, records: &[HandlerRecord]) -> Result<(), StorageError> {
        let mut handlers = self.handlers.write().await;
        handlers.clear();
        for r in records {
            let mut stored = r.clone();
            stored.program_hash = stored.program_hash.to_ascii_lowercase();
            handlers.push(stored);
        }
        Ok(())
    }

    // ── PSK key escrow ─────────────────────────────────────────

    async fn get_escrow_keypair(&self) -> Result<Option<EscrowKeypairRecord>, StorageError> {
        Ok(self.escrow_keypair.read().await.clone())
    }

    async fn store_escrow_keypair(&self, record: &EscrowKeypairRecord) -> Result<(), StorageError> {
        *self.escrow_keypair.write().await = Some(record.clone());
        Ok(())
    }

    async fn try_record_operation(&self, operation_id: &[u8; 16]) -> Result<bool, StorageError> {
        Ok(self
            .escrow_operations
            .write()
            .await
            .insert(operation_id.to_vec()))
    }

    async fn get_pending_rotation(&self) -> Result<Option<PendingRotationRecord>, StorageError> {
        Ok(self.pending_rotation.read().await.clone())
    }

    async fn store_pending_rotation(
        &self,
        record: &PendingRotationRecord,
    ) -> Result<(), StorageError> {
        *self.pending_rotation.write().await = Some(record.clone());
        Ok(())
    }

    async fn delete_pending_rotation(&self) -> Result<(), StorageError> {
        *self.pending_rotation.write().await = None;
        Ok(())
    }
}
