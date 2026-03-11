// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use tokio::sync::RwLock;

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

/// Abstract storage backend for node registry and program library.
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Node registry ──────────────────────────────────────────
    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StorageError>;
    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StorageError>;
    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>, StorageError>;
    async fn upsert_node(&self, record: &NodeRecord) -> Result<(), StorageError>;
    async fn delete_node(&self, node_id: &str) -> Result<(), StorageError>;

    // ── Program library ────────────────────────────────────────
    async fn get_program(&self, hash: &[u8]) -> Result<Option<ProgramRecord>, StorageError>;
    async fn store_program(&self, record: &ProgramRecord) -> Result<(), StorageError>;
    async fn delete_program(&self, hash: &[u8]) -> Result<(), StorageError>;
    async fn list_programs(&self) -> Result<Vec<ProgramRecord>, StorageError>;
}

/// In-memory storage backend for testing.
pub struct InMemoryStorage {
    nodes: RwLock<HashMap<String, NodeRecord>>,
    programs: RwLock<HashMap<Vec<u8>, ProgramRecord>>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            programs: RwLock::new(HashMap::new()),
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
}
