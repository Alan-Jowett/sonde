// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use crate::program::{ProgramRecord, VerificationProfile};
use crate::registry::NodeRecord;
use crate::storage::{Storage, StorageError};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nodes (
    node_id TEXT PRIMARY KEY,
    key_hint INTEGER NOT NULL,
    psk BLOB NOT NULL,
    assigned_program_hash BLOB,
    current_program_hash BLOB,
    schedule_interval_s INTEGER NOT NULL DEFAULT 60,
    firmware_abi_version INTEGER,
    last_battery_mv INTEGER,
    last_seen_epoch_s INTEGER
);
CREATE INDEX IF NOT EXISTS idx_nodes_key_hint ON nodes(key_hint);

CREATE TABLE IF NOT EXISTS programs (
    hash BLOB PRIMARY KEY,
    image BLOB NOT NULL,
    size INTEGER NOT NULL,
    verification_profile TEXT NOT NULL
);
";

/// SQLite-backed persistent storage for the gateway.
pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    /// Open (or create) a SQLite database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn =
            Connection::open(path).map_err(|e| StorageError::Internal(format!("open: {e}")))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| StorageError::Internal(format!("pragma: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| StorageError::Internal(format!("schema: {e}")))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory SQLite database (for testing).
    pub fn in_memory() -> Result<Self, StorageError> {
        Self::open(":memory:")
    }
}

/// Convert a `SystemTime` to seconds since the Unix epoch.
fn system_time_to_epoch_s(t: &SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Convert seconds since the Unix epoch to a `SystemTime`.
fn epoch_s_to_system_time(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs((-secs) as u64)
    }
}

/// Parse a `VerificationProfile` from its stored text representation.
fn parse_profile(s: &str) -> Result<VerificationProfile, StorageError> {
    match s {
        "Resident" => Ok(VerificationProfile::Resident),
        "Ephemeral" => Ok(VerificationProfile::Ephemeral),
        other => Err(StorageError::Internal(format!(
            "unknown verification profile: {other}"
        ))),
    }
}

/// Serialize a `VerificationProfile` to its text representation.
fn profile_to_str(p: &VerificationProfile) -> &'static str {
    match p {
        VerificationProfile::Resident => "Resident",
        VerificationProfile::Ephemeral => "Ephemeral",
    }
}

/// Read a `NodeRecord` from the current row of a rusqlite statement.
fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRecord> {
    let psk_vec: Vec<u8> = row.get(2)?;
    let psk: [u8; 32] = psk_vec.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Blob,
            "psk is not 32 bytes".into(),
        )
    })?;
    let last_seen_epoch: Option<i64> = row.get(8)?;
    Ok(NodeRecord {
        node_id: row.get(0)?,
        key_hint: row.get::<_, u32>(1)? as u16,
        psk,
        assigned_program_hash: row.get(3)?,
        current_program_hash: row.get(4)?,
        schedule_interval_s: row.get(5)?,
        firmware_abi_version: row.get(6)?,
        last_battery_mv: row.get(7)?,
        last_seen: last_seen_epoch.map(epoch_s_to_system_time),
    })
}

#[async_trait]
impl Storage for SqliteStorage {
    // ── Node registry ──────────────────────────────────────────

    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, key_hint, psk, assigned_program_hash, \
                 current_program_hash, schedule_interval_s, firmware_abi_version, \
                 last_battery_mv, last_seen_epoch_s FROM nodes",
            )
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let rows = stmt
            .query_map([], row_to_node)
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row.map_err(|e| StorageError::Internal(e.to_string()))?);
        }
        Ok(nodes)
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        conn.query_row(
            "SELECT node_id, key_hint, psk, assigned_program_hash, \
             current_program_hash, schedule_interval_s, firmware_abi_version, \
             last_battery_mv, last_seen_epoch_s FROM nodes WHERE node_id = ?1",
            params![node_id],
            row_to_node,
        )
        .optional()
        .map_err(|e| StorageError::Internal(e.to_string()))
    }

    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT node_id, key_hint, psk, assigned_program_hash, \
                 current_program_hash, schedule_interval_s, firmware_abi_version, \
                 last_battery_mv, last_seen_epoch_s FROM nodes WHERE key_hint = ?1",
            )
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let rows = stmt
            .query_map(params![key_hint as u32], row_to_node)
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row.map_err(|e| StorageError::Internal(e.to_string()))?);
        }
        Ok(nodes)
    }

    async fn upsert_node(&self, record: &NodeRecord) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let last_seen_epoch = record.last_seen.as_ref().map(system_time_to_epoch_s);
        conn.execute(
            "INSERT INTO nodes (node_id, key_hint, psk, assigned_program_hash, \
             current_program_hash, schedule_interval_s, firmware_abi_version, \
             last_battery_mv, last_seen_epoch_s) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(node_id) DO UPDATE SET \
             key_hint = excluded.key_hint, \
             psk = excluded.psk, \
             assigned_program_hash = excluded.assigned_program_hash, \
             current_program_hash = excluded.current_program_hash, \
             schedule_interval_s = excluded.schedule_interval_s, \
             firmware_abi_version = excluded.firmware_abi_version, \
             last_battery_mv = excluded.last_battery_mv, \
             last_seen_epoch_s = excluded.last_seen_epoch_s",
            params![
                record.node_id,
                record.key_hint as u32,
                record.psk.as_slice(),
                record.assigned_program_hash,
                record.current_program_hash,
                record.schedule_interval_s,
                record.firmware_abi_version,
                record.last_battery_mv,
                last_seen_epoch,
            ],
        )
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        conn.execute("DELETE FROM nodes WHERE node_id = ?1", params![node_id])
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    // ── Program library ────────────────────────────────────────

    async fn get_program(&self, hash: &[u8]) -> Result<Option<ProgramRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        conn.query_row(
            "SELECT hash, image, size, verification_profile FROM programs WHERE hash = ?1",
            params![hash],
            |row| {
                let profile_str: String = row.get(3)?;
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, profile_str))
            },
        )
        .optional()
        .map_err(|e| StorageError::Internal(e.to_string()))?
        .map(
            |(hash, image, size, profile_str): (Vec<u8>, Vec<u8>, u32, String)| {
                Ok(ProgramRecord {
                    hash,
                    image,
                    size,
                    verification_profile: parse_profile(&profile_str)?,
                })
            },
        )
        .transpose()
    }

    async fn store_program(&self, record: &ProgramRecord) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO programs (hash, image, size, verification_profile) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                record.hash,
                record.image,
                record.size,
                profile_to_str(&record.verification_profile),
            ],
        )
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn delete_program(&self, hash: &[u8]) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        conn.execute("DELETE FROM programs WHERE hash = ?1", params![hash])
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn list_programs(&self) -> Result<Vec<ProgramRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT hash, image, size, verification_profile FROM programs")
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let profile_str: String = row.get(3)?;
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, profile_str))
            })
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut programs = Vec::new();
        for row in rows {
            let (hash, image, size, profile_str): (Vec<u8>, Vec<u8>, u32, String) =
                row.map_err(|e| StorageError::Internal(e.to_string()))?;
            programs.push(ProgramRecord {
                hash,
                image,
                size,
                verification_profile: parse_profile(&profile_str)?,
            });
        }
        Ok(programs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, key_hint: u16) -> NodeRecord {
        NodeRecord {
            node_id: id.to_string(),
            key_hint,
            psk: [0xAB; 32],
            assigned_program_hash: None,
            current_program_hash: None,
            schedule_interval_s: 60,
            firmware_abi_version: None,
            last_battery_mv: None,
            last_seen: None,
        }
    }

    fn make_program(tag: u8) -> ProgramRecord {
        let hash = vec![tag; 32];
        let image = vec![0x01, 0x02, 0x03, tag];
        ProgramRecord {
            hash,
            image: image.clone(),
            size: image.len() as u32,
            verification_profile: VerificationProfile::Resident,
        }
    }

    #[tokio::test]
    async fn test_node_crud() {
        let store = SqliteStorage::in_memory().unwrap();

        // Initially empty.
        assert!(store.list_nodes().await.unwrap().is_empty());
        assert!(store.get_node("n1").await.unwrap().is_none());

        // Create.
        let mut node = make_node("n1", 42);
        node.assigned_program_hash = Some(vec![0xFF; 32]);
        node.last_seen = Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        store.upsert_node(&node).await.unwrap();

        // Read.
        let fetched = store.get_node("n1").await.unwrap().unwrap();
        assert_eq!(fetched.node_id, "n1");
        assert_eq!(fetched.key_hint, 42);
        assert_eq!(fetched.psk, [0xAB; 32]);
        assert_eq!(fetched.assigned_program_hash, Some(vec![0xFF; 32]));
        assert_eq!(fetched.schedule_interval_s, 60);
        assert_eq!(
            fetched.last_seen,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );

        // List.
        assert_eq!(store.list_nodes().await.unwrap().len(), 1);

        // Delete.
        store.delete_node("n1").await.unwrap();
        assert!(store.get_node("n1").await.unwrap().is_none());
        assert!(store.list_nodes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_program_crud() {
        let store = SqliteStorage::in_memory().unwrap();

        // Initially empty.
        assert!(store.list_programs().await.unwrap().is_empty());

        let prog = make_program(0x01);
        store.store_program(&prog).await.unwrap();

        // Get by hash.
        let fetched = store.get_program(&prog.hash).await.unwrap().unwrap();
        assert_eq!(fetched.hash, prog.hash);
        assert_eq!(fetched.image, prog.image);
        assert_eq!(fetched.size, prog.size);
        assert_eq!(fetched.verification_profile, VerificationProfile::Resident);

        // List.
        assert_eq!(store.list_programs().await.unwrap().len(), 1);

        // Delete.
        store.delete_program(&prog.hash).await.unwrap();
        assert!(store.get_program(&prog.hash).await.unwrap().is_none());
        assert!(store.list_programs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_nodes_by_key_hint() {
        let store = SqliteStorage::in_memory().unwrap();

        store.upsert_node(&make_node("a", 10)).await.unwrap();
        store.upsert_node(&make_node("b", 10)).await.unwrap();
        store.upsert_node(&make_node("c", 20)).await.unwrap();

        let hint10 = store.get_nodes_by_key_hint(10).await.unwrap();
        assert_eq!(hint10.len(), 2);
        let ids: Vec<&str> = hint10.iter().map(|n| n.node_id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));

        let hint20 = store.get_nodes_by_key_hint(20).await.unwrap();
        assert_eq!(hint20.len(), 1);
        assert_eq!(hint20[0].node_id, "c");

        let hint99 = store.get_nodes_by_key_hint(99).await.unwrap();
        assert!(hint99.is_empty());
    }

    #[tokio::test]
    async fn test_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // First open: write data.
        {
            let store = SqliteStorage::open(&db_path).unwrap();
            store.upsert_node(&make_node("p1", 5)).await.unwrap();
            store.store_program(&make_program(0xAA)).await.unwrap();
        }

        // Second open: data survives.
        {
            let store = SqliteStorage::open(&db_path).unwrap();
            assert!(store.get_node("p1").await.unwrap().is_some());
            assert!(store.get_program(&vec![0xAA; 32]).await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn test_upsert_overwrites() {
        let store = SqliteStorage::in_memory().unwrap();

        let mut node = make_node("u1", 1);
        node.schedule_interval_s = 30;
        store.upsert_node(&node).await.unwrap();

        // Upsert with different values.
        node.schedule_interval_s = 120;
        node.key_hint = 2;
        node.last_battery_mv = Some(3300);
        store.upsert_node(&node).await.unwrap();

        // Only one record, with updated values.
        let nodes = store.list_nodes().await.unwrap();
        assert_eq!(nodes.len(), 1);
        let fetched = &nodes[0];
        assert_eq!(fetched.schedule_interval_s, 120);
        assert_eq!(fetched.key_hint, 2);
        assert_eq!(fetched.last_battery_mv, Some(3300));
    }
}
