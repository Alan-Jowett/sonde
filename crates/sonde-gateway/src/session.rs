// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::info;

use crate::transport::PeerAddress;

/// Session-level errors.
#[derive(Debug, Clone)]
pub enum SessionError {
    /// No active session for the given node.
    NotFound(String),
    /// Session has expired (timed out).
    Expired(String),
    /// Sequence number mismatch — expected vs. received.
    SequenceMismatch { expected: u64, received: u64 },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::NotFound(id) => write!(f, "no active session for node {}", id),
            SessionError::Expired(id) => write!(f, "session expired for node {}", id),
            SessionError::SequenceMismatch { expected, received } => {
                write!(
                    f,
                    "sequence mismatch: expected {}, received {}",
                    expected, received
                )
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// State of a node session within a wake cycle.
#[derive(Debug, Clone)]
pub enum SessionState {
    /// Waiting for post-WAKE messages (GET_CHUNK, APP_DATA, etc.).
    AwaitingPostWake,
    /// Currently serving program chunks.
    ChunkedTransfer {
        program_hash: Vec<u8>,
        program_size: u32,
        chunk_size: u32,
        chunk_count: u32,
        is_ephemeral: bool,
    },
    /// BPF program executing on node; awaiting APP_DATA.
    BpfExecuting,
}

/// An active node session (exists only in memory, never persisted).
#[derive(Debug, Clone)]
pub struct Session {
    pub node_id: String,
    pub peer_address: PeerAddress,
    pub wake_nonce: u64,
    pub next_expected_seq: u64,
    pub created_at: Instant,
    pub state: SessionState,
}

/// Manages active sessions keyed by node ID.
pub struct SessionManager {
    sessions: RwLock<HashMap<String, Session>>,
    timeout: Duration,
    /// Held as a write-lock during state import to prevent new sessions
    /// from being created while the storage is being replaced.  Normal
    /// frame processing acquires a read-lock (zero contention).
    import_lock: RwLock<()>,
}

impl SessionManager {
    /// Create a new session manager with the given session timeout.
    pub fn new(timeout: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            timeout,
            import_lock: RwLock::new(()),
        }
    }

    /// Create (or replace) a session for the given node.
    /// Any existing session for this node is silently replaced (GW-0602).
    /// Blocks while a state import is in progress (import_lock).
    pub async fn create_session(
        &self,
        node_id: String,
        peer_address: PeerAddress,
        wake_nonce: u64,
        starting_seq: u64,
    ) -> Session {
        let _guard = self.import_lock.read().await;
        let session = Session {
            node_id: node_id.clone(),
            peer_address,
            wake_nonce,
            next_expected_seq: starting_seq,
            created_at: Instant::now(),
            state: SessionState::AwaitingPostWake,
        };
        let mut sessions = self.sessions.write().await;
        sessions.insert(node_id, session.clone());
        session
    }

    /// Get a clone of the session for the given node.
    pub async fn get_session(&self, node_id: &str) -> Option<Session> {
        let sessions = self.sessions.read().await;
        sessions.get(node_id).cloned()
    }

    /// Verify the sequence number for an inbound post-WAKE frame and
    /// advance `next_expected_seq`. Returns Ok(()) on success.
    pub async fn verify_and_advance_seq(
        &self,
        node_id: &str,
        received_seq: u64,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(node_id)
            .ok_or_else(|| SessionError::NotFound(node_id.to_string()))?;

        // Check if the session has expired
        if Instant::now().duration_since(session.created_at) > self.timeout {
            sessions.remove(node_id);
            return Err(SessionError::Expired(node_id.to_string()));
        }

        if received_seq != session.next_expected_seq {
            return Err(SessionError::SequenceMismatch {
                expected: session.next_expected_seq,
                received: received_seq,
            });
        }
        session.next_expected_seq += 1;
        Ok(())
    }

    /// Update the session state for a node.
    pub async fn set_state(&self, node_id: &str, state: SessionState) -> Result<(), SessionError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(node_id)
            .ok_or_else(|| SessionError::NotFound(node_id.to_string()))?;
        session.state = state;
        Ok(())
    }

    /// Remove and return the session for the given node.
    pub async fn remove_session(&self, node_id: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        sessions.remove(node_id)
    }

    /// Remove all sessions that have exceeded the configured timeout.
    /// Returns the node IDs of reaped sessions.
    pub async fn reap_expired(&self) -> Vec<String> {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| now.duration_since(s.created_at) > self.timeout)
            .map(|(id, _)| id.clone())
            .collect();

        for id in &expired {
            sessions.remove(id);
            // GW-1300 AC6: log session expiry.
            info!(node_id = %id, "session expired");
        }
        expired
    }

    /// Return the number of active sessions.
    pub async fn active_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Acquire the import lock (write-side), preventing any new sessions
    /// from being created while the guard is held.  Callers should check
    /// [`active_count`] after acquiring to verify the gateway is quiescent.
    pub async fn acquire_import_lock(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.import_lock.write().await
    }
}
