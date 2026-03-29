// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
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

/// Abstract storage backend for node registry and program library.
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Node registry ──────────────────────────────────────────
    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StorageError>;
    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StorageError>;
    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>, StorageError>;
    async fn upsert_node(&self, record: &NodeRecord) -> Result<(), StorageError>;
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

    /// Atomically replace all handler records with the given set.
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
        }
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
        nodes.insert(record.node_id.clone(), record.clone());
        Ok(())
    }

    async fn insert_node_if_not_exists(&self, record: &NodeRecord) -> Result<bool, StorageError> {
        let mut nodes = self.nodes.write().await;
        use std::collections::hash_map::Entry;
        match nodes.entry(record.node_id.clone()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(e) => {
                e.insert(record.clone());
                Ok(true)
            }
        }
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
        programs.insert(record.hash.clone(), record.clone());
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
}
