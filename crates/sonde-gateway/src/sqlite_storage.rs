// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use zeroize::Zeroizing;

use crate::gateway_identity::GatewayIdentity;
use crate::phone_trust::{PhonePskRecord, PhonePskStatus};
use crate::program::{ProgramRecord, VerificationProfile};
use crate::registry::{NodeRecord, SensorDescriptor};
use crate::storage::{Storage, StorageError};

/// Encrypted PSK blob length: 12-byte nonce + 32-byte ciphertext + 16-byte GCM tag = 60 bytes.
const ENCRYPTED_PSK_LEN: usize = 12 + 32 + 16;

/// Encrypt a 32-byte PSK using AES-256-GCM with a random nonce.
///
/// The `node_id` is used as Additional Authenticated Data (AAD) to cryptographically
/// bind the encrypted PSK to its owning node. This prevents an attacker from copying
/// a ciphertext blob between node rows to cause PSK confusion.
///
/// Returns a blob of the form `nonce (12 B) || ciphertext+tag (48 B)`.
fn encrypt_psk(
    master_key: &[u8; 32],
    node_id: &str,
    psk: &[u8; 32],
) -> Result<Vec<u8>, StorageError> {
    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);

    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| StorageError::Internal(format!("nonce rng: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let payload = Payload {
        msg: psk.as_slice(),
        aad: node_id.as_bytes(),
    };

    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|e| StorageError::Internal(format!("psk encrypt: {e}")))?;

    let mut out = Vec::with_capacity(ENCRYPTED_PSK_LEN);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an encrypted PSK blob produced by [`encrypt_psk`].
///
/// Returns `StorageError::Internal` if the blob length is wrong or the
/// GCM authentication tag check fails (which indicates data corruption or
/// an incorrect master key).
///
/// Note: legacy 32-byte plaintext PSK blobs are handled exclusively by
/// [`migrate_legacy_psks`] during [`SqliteStorage::open`]. This function
/// does not accept plaintext blobs — after migration, all stored PSKs
/// must be [`ENCRYPTED_PSK_LEN`] bytes.
fn decrypt_psk(
    master_key: &[u8; 32],
    node_id: &str,
    blob: &[u8],
) -> Result<[u8; 32], StorageError> {
    if blob.len() != ENCRYPTED_PSK_LEN {
        return Err(StorageError::Internal(format!(
            "encrypted psk blob has wrong length: expected {ENCRYPTED_PSK_LEN}, got {}",
            blob.len()
        )));
    }

    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&blob[..12]);

    let payload = Payload {
        msg: &blob[12..],
        aad: node_id.as_bytes(),
    };

    let plaintext = Zeroizing::new(cipher.decrypt(nonce, payload).map_err(|_| {
        StorageError::Internal("psk decryption failed — wrong master key or data corruption".into())
    })?);

    plaintext
        .as_slice()
        .try_into()
        .map_err(|_| StorageError::Internal("decrypted psk is not 32 bytes".into()))
}

/// Encrypt a 32-byte Ed25519 seed using AES-256-GCM.
///
/// The `gateway_id` is included in the AAD to cryptographically bind the
/// seed to its associated gateway identity. This prevents a tampered
/// `gateway_id` from being accepted alongside a valid seed.
/// Returns `nonce (12 B) || ciphertext+tag (48 B)`.
fn encrypt_seed(
    master_key: &[u8; 32],
    seed: &[u8; 32],
    gateway_id: &[u8; 16],
) -> Result<Vec<u8>, StorageError> {
    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);

    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| StorageError::Internal(format!("nonce rng: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let mut aad = Vec::with_capacity(22 + 16);
    aad.extend_from_slice(b"sonde-gateway-identity");
    aad.extend_from_slice(gateway_id);

    let payload = Payload {
        msg: seed.as_slice(),
        aad: &aad,
    };

    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|e| StorageError::Internal(format!("seed encrypt: {e}")))?;

    let mut out = Vec::with_capacity(ENCRYPTED_PSK_LEN);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an encrypted Ed25519 seed blob.
///
/// The `gateway_id` is used as part of the AAD (matching [`encrypt_seed`]),
/// ensuring that a tampered `gateway_id` is detected as an authentication failure.
fn decrypt_seed(
    master_key: &[u8; 32],
    blob: &[u8],
    gateway_id: &[u8; 16],
) -> Result<[u8; 32], StorageError> {
    if blob.len() != ENCRYPTED_PSK_LEN {
        return Err(StorageError::Internal(format!(
            "encrypted seed blob has wrong length: expected {ENCRYPTED_PSK_LEN}, got {}",
            blob.len()
        )));
    }

    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&blob[..12]);

    let mut aad = Vec::with_capacity(22 + 16);
    aad.extend_from_slice(b"sonde-gateway-identity");
    aad.extend_from_slice(gateway_id);

    let payload = Payload {
        msg: &blob[12..],
        aad: &aad,
    };

    let plaintext = Zeroizing::new(cipher.decrypt(nonce, payload).map_err(|_| {
        StorageError::Internal(
            "seed decryption failed — wrong master key or data corruption".into(),
        )
    })?);

    plaintext
        .as_slice()
        .try_into()
        .map_err(|_| StorageError::Internal("decrypted seed is not 32 bytes".into()))
}

/// Encrypt a 32-byte phone PSK using AES-256-GCM.
///
/// The `phone_id` (as string) is used as AAD.
fn encrypt_phone_psk(
    master_key: &[u8; 32],
    phone_id: u32,
    psk: &[u8; 32],
) -> Result<Vec<u8>, StorageError> {
    let aad = phone_id.to_string();
    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);

    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| StorageError::Internal(format!("nonce rng: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let payload = Payload {
        msg: psk.as_slice(),
        aad: aad.as_bytes(),
    };

    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|e| StorageError::Internal(format!("phone psk encrypt: {e}")))?;

    let mut out = Vec::with_capacity(ENCRYPTED_PSK_LEN);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an encrypted phone PSK blob.
fn decrypt_phone_psk(
    master_key: &[u8; 32],
    phone_id: u32,
    blob: &[u8],
) -> Result<[u8; 32], StorageError> {
    if blob.len() != ENCRYPTED_PSK_LEN {
        return Err(StorageError::Internal(format!(
            "encrypted phone psk blob has wrong length: expected {ENCRYPTED_PSK_LEN}, got {}",
            blob.len()
        )));
    }

    let aad = phone_id.to_string();
    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&blob[..12]);

    let payload = Payload {
        msg: &blob[12..],
        aad: aad.as_bytes(),
    };

    let plaintext = Zeroizing::new(cipher.decrypt(nonce, payload).map_err(|_| {
        StorageError::Internal(
            "phone psk decryption failed — wrong master key or data corruption".into(),
        )
    })?);

    plaintext
        .as_slice()
        .try_into()
        .map_err(|_| StorageError::Internal("decrypted phone psk is not 32 bytes".into()))
}

/// Re-encrypt any legacy plaintext PSK blobs left over from pre-GW-0601a
/// databases.  Called once during [`SqliteStorage::open`].
///
/// Any row whose `psk` column is exactly 32 bytes is treated as a plaintext
/// PSK and transparently re-encrypted with the current master key.  After this
/// function returns all PSK blobs in the database are [`ENCRYPTED_PSK_LEN`]
/// bytes long and decryption can unconditionally use the AES-256-GCM path.
fn migrate_legacy_psks(conn: &mut Connection, master_key: &[u8; 32]) -> Result<(), StorageError> {
    use zeroize::Zeroize;

    let tx = conn
        .transaction()
        .map_err(|e| StorageError::Internal(format!("begin migration tx: {e}")))?;

    // Collect only node_ids so plaintext PSK material is never buffered for
    // more than one row at a time.
    let legacy_ids: Vec<String> = tx
        .prepare("SELECT node_id FROM nodes WHERE LENGTH(psk) = 32")
        .map_err(map_err)
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get(0))
                .map_err(map_err)?
                .collect::<Result<_, _>>()
                .map_err(map_err)
        })?;

    if legacy_ids.is_empty() {
        return Ok(());
    }

    tracing::warn!(
        count = legacy_ids.len(),
        "migrating {} legacy plaintext PSK(s) to AES-256-GCM encryption — \
         this is irreversible; verify the master key is correct and ensure \
         a database backup exists before proceeding",
        legacy_ids.len(),
    );

    for node_id in &legacy_ids {
        let mut psk_blob: Vec<u8> = tx
            .query_row(
                "SELECT psk FROM nodes WHERE node_id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .map_err(map_err)?;
        if psk_blob.len() != 32 {
            let len = psk_blob.len();
            psk_blob.zeroize();
            return Err(StorageError::Internal(format!(
                "legacy PSK migration: node `{node_id}` has a {len}-byte psk \
                 blob (expected 32); database may be corrupt",
            )));
        }
        let mut psk = [0u8; 32];
        psk.copy_from_slice(&psk_blob);
        psk_blob.zeroize();
        let encrypted = encrypt_psk(master_key, node_id, &psk);
        psk.zeroize();
        let encrypted = encrypted?;
        tx.execute(
            "UPDATE nodes SET psk = ?1 WHERE node_id = ?2",
            params![encrypted, node_id],
        )
        .map_err(map_err)?;
    }

    tx.commit()
        .map_err(|e| StorageError::Internal(format!("commit migration tx: {e}")))?;
    Ok(())
}

/// Verify that the provided master key can decrypt an existing PSK row and
/// that every PSK blob in the database has an expected length.
///
/// This is called during [`SqliteStorage::open`] after legacy migration to catch
/// a wrong master key as early as possible — at startup — rather than silently
/// accepting the database and producing decryption errors on every node read.
///
/// The function also rejects databases that contain PSK blobs with unexpected
/// lengths (neither 32-byte legacy plaintext nor [`ENCRYPTED_PSK_LEN`]-byte
/// encrypted), which would indicate corruption or tampering.
///
/// If no PSK rows exist yet (new or empty database) the function returns
/// `Ok(())` since there is nothing to validate against.
fn validate_master_key(conn: &Connection, master_key: &[u8; 32]) -> Result<(), StorageError> {
    // Reject any PSK blobs with unexpected lengths.
    let bad_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE LENGTH(psk) != ?1 AND LENGTH(psk) != 32",
            params![ENCRYPTED_PSK_LEN as i64],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    if bad_count > 0 {
        return Err(StorageError::Internal(format!(
            "master key validation failed — {bad_count} node(s) have PSK blobs with \
             unexpected lengths (expected 32 or {ENCRYPTED_PSK_LEN} bytes); \
             database may be corrupt or tampered with",
        )));
    }

    // Try to decrypt one encrypted row to verify the master key.
    let psk_row: Option<(String, Vec<u8>)> = conn
        .query_row(
            "SELECT node_id, psk FROM nodes WHERE LENGTH(psk) = ?1 LIMIT 1",
            params![ENCRYPTED_PSK_LEN as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(map_err)?;

    if let Some((node_id, psk_blob)) = psk_row {
        let _decrypted =
            Zeroizing::new(decrypt_psk(master_key, &node_id, &psk_blob).map_err(|e| {
                StorageError::Internal(format!(
                    "master key validation failed — cannot decrypt PSK for node \
                     `{node_id}`: {e}; this may indicate a wrong master key or \
                     corrupt/tampered database data",
                ))
            })?);
    }
    Ok(())
}

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
    verification_profile TEXT NOT NULL,
    abi_version INTEGER
);

CREATE TABLE IF NOT EXISTS gateway_identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    encrypted_seed BLOB NOT NULL,
    gateway_id BLOB NOT NULL,
    public_key BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS phone_psks (
    phone_id INTEGER PRIMARY KEY AUTOINCREMENT,
    phone_key_hint INTEGER NOT NULL,
    psk BLOB NOT NULL,
    label TEXT NOT NULL,
    issued_at_epoch_s INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
);
CREATE INDEX IF NOT EXISTS idx_phone_psks_key_hint ON phone_psks(phone_key_hint);
";

/// SQLite-backed persistent storage for the gateway.
///
/// Uses `Arc<Mutex<Connection>>` so storage operations can be offloaded
/// to `spawn_blocking` to avoid holding a sync lock on async threads.
///
/// PSKs are encrypted at rest using AES-256-GCM with the provided master key
/// (GW-0601a). The master key is never written to the database.
pub struct SqliteStorage {
    conn: Arc<Mutex<Connection>>,
    master_key: Arc<Zeroizing<[u8; 32]>>,
}

impl SqliteStorage {
    /// Open (or create) a SQLite database at the given path.
    ///
    /// `master_key` is a 32-byte AES-256 key used to encrypt PSK material at
    /// rest. All PSKs are transparently encrypted on write and decrypted on
    /// read; the key is never persisted. See [`decrypt_psk`] / [`encrypt_psk`].
    ///
    /// On Unix, callers should ensure the database file and its WAL/SHM
    /// sidecars have restrictive permissions (e.g., 0600) as an additional
    /// layer of protection. This can be done by setting `umask(0o077)` before
    /// calling `open()`, or by adjusting permissions after creation.
    pub fn open(
        path: impl AsRef<Path>,
        master_key: Zeroizing<[u8; 32]>,
    ) -> Result<Self, StorageError> {
        let mut conn =
            Connection::open(path).map_err(|e| StorageError::Internal(format!("open: {e}")))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| StorageError::Internal(format!("pragma: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| StorageError::Internal(format!("schema: {e}")))?;
        // Migration: add abi_version column to programs table if it does not exist
        // (for databases created before this field was introduced).
        let has_abi = conn
            .prepare("PRAGMA table_info(programs)")
            .and_then(|mut stmt| {
                let names: rusqlite::Result<Vec<String>> =
                    stmt.query_map([], |row| row.get::<_, String>(1))?.collect();
                names.map(|ns| ns.iter().any(|n| n == "abi_version"))
            })
            .map_err(|e| StorageError::Internal(format!("migration check: {e}")))?;
        if !has_abi {
            conn.execute_batch("ALTER TABLE programs ADD COLUMN abi_version INTEGER")
                .map_err(|e| StorageError::Internal(format!("migration: {e}")))?;
        }
        // Migration: add BLE pairing columns to nodes table if they don't exist.
        {
            let node_cols: Vec<String> = conn
                .prepare("PRAGMA table_info(nodes)")
                .and_then(|mut stmt| stmt.query_map([], |row| row.get::<_, String>(1))?.collect())
                .map_err(|e| StorageError::Internal(format!("migration check: {e}")))?;
            if !node_cols.iter().any(|n| n == "rf_channel") {
                conn.execute_batch(
                    "ALTER TABLE nodes ADD COLUMN rf_channel INTEGER;\
                     ALTER TABLE nodes ADD COLUMN sensors_json TEXT;\
                     ALTER TABLE nodes ADD COLUMN registered_by_phone_id INTEGER;",
                )
                .map_err(|e| StorageError::Internal(format!("migration: {e}")))?;
            }
        }
        // Migrate any legacy plaintext 32-byte PSK blobs to AES-256-GCM encrypted
        // form. This must run before `validate_master_key` since validation only
        // checks encrypted blobs.
        migrate_legacy_psks(&mut conn, &master_key)?;
        // Verify that the master key can actually decrypt existing PSK data.
        // Catches a wrong key at startup rather than at first node read.
        validate_master_key(&conn, &master_key)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            master_key: Arc::new(master_key),
        })
    }

    /// Create an in-memory SQLite database (for testing).
    pub fn in_memory(master_key: Zeroizing<[u8; 32]>) -> Result<Self, StorageError> {
        Self::open(":memory:", master_key)
    }

    /// Run a synchronous closure on the connection via `spawn_blocking`.
    async fn with_conn<F, T>(&self, f: F) -> Result<T, StorageError>
    where
        F: FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn = conn
                .lock()
                .map_err(|_| StorageError::Internal("storage mutex poisoned".into()))?;
            f(&conn)
        })
        .await
        .map_err(|e| StorageError::Internal(format!("spawn_blocking: {e}")))?
    }
}

/// Convert a `rusqlite::Error` into a `StorageError`.
fn map_err(e: rusqlite::Error) -> StorageError {
    StorageError::Internal(e.to_string())
}

/// Convert a `SystemTime` to seconds since the Unix epoch.
///
/// Pre-epoch times are stored as negative values. Sub-second
/// precision is lost (rounded toward negative infinity for
/// pre-epoch times, truncated for post-epoch times).
fn system_time_to_epoch_s(t: &SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => {
            let dur = e.duration();
            let secs = dur.as_secs();
            let extra: u64 = if dur.subsec_nanos() > 0 { 1 } else { 0 };
            let total = secs.saturating_add(extra);
            i64::try_from(total).map(|v| -v).unwrap_or(i64::MIN + 1)
        }
    }
}

/// Convert seconds since the Unix epoch to a `SystemTime`.
///
/// Returns `UNIX_EPOCH` if the value cannot be represented.
fn epoch_s_to_system_time(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(secs as u64))
            .unwrap_or(UNIX_EPOCH)
    } else {
        let abs = (secs as i128).unsigned_abs() as u64;
        UNIX_EPOCH
            .checked_sub(Duration::from_secs(abs))
            .unwrap_or(UNIX_EPOCH)
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

/// Serialize sensor descriptors to a compact JSON array for SQLite storage.
fn sensors_to_json(sensors: &[SensorDescriptor]) -> Option<String> {
    if sensors.is_empty() {
        return None;
    }
    // Manual JSON to avoid adding serde_json dependency.
    let mut out = String::from("[");
    for (i, s) in sensors.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match &s.label {
            Some(label) => {
                // Escape JSON string: backslash and double-quote.
                let escaped: String = label
                    .chars()
                    .flat_map(|c| match c {
                        '"' => vec!['\\', '"'],
                        '\\' => vec!['\\', '\\'],
                        c => vec![c],
                    })
                    .collect();
                out.push_str(&format!(
                    "{{\"t\":{},\"i\":{},\"l\":\"{}\"}}",
                    s.sensor_type, s.sensor_id, escaped
                ));
            }
            None => {
                out.push_str(&format!(
                    "{{\"t\":{},\"i\":{}}}",
                    s.sensor_type, s.sensor_id
                ));
            }
        }
    }
    out.push(']');
    Some(out)
}

/// Parse sensor descriptors from JSON stored in SQLite.
fn sensors_from_json(json: &str) -> Vec<SensorDescriptor> {
    // Minimal JSON array parser for our known format.
    let mut sensors = Vec::new();
    let trimmed = json.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return sensors;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.trim().is_empty() {
        return sensors;
    }
    // Split on '},{' to separate objects.
    for obj_str in split_json_objects(inner) {
        let mut sensor_type: Option<u8> = None;
        let mut sensor_id: Option<u8> = None;
        let mut label: Option<String> = None;
        // Parse key-value pairs from the object string.
        let obj = obj_str.trim();
        let obj = obj.strip_prefix('{').unwrap_or(obj);
        let obj = obj.strip_suffix('}').unwrap_or(obj);
        for part in obj.split(',') {
            let part = part.trim();
            if let Some((key, val)) = part.split_once(':') {
                let key = key.trim().trim_matches('"');
                let val = val.trim();
                match key {
                    "t" => sensor_type = val.parse().ok(),
                    "i" => sensor_id = val.parse().ok(),
                    "l" => {
                        let s = val.trim_matches('"');
                        label = Some(s.replace("\\\"", "\"").replace("\\\\", "\\"));
                    }
                    _ => {}
                }
            }
        }
        if let (Some(st), Some(si)) = (sensor_type, sensor_id) {
            sensors.push(SensorDescriptor {
                sensor_type: st,
                sensor_id: si,
                label,
            });
        }
    }
    sensors
}

/// Split a JSON array interior into individual object strings.
fn split_json_objects(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    result.push(&s[start..=i]);
                    start = i + 1;
                    // Skip comma after '}'
                    // start will be adjusted on next '{' detection
                }
            }
            _ => {
                if depth == 0 && c == '{' {
                    // This shouldn't happen due to the '{' arm, but safety.
                }
            }
        }
        if depth == 0 && c != '}' && c != ',' && c != ' ' {
            // skip whitespace/commas between objects
        }
    }
    result
}

/// Read a `NodeRecord` from the current row of a rusqlite statement,
/// decrypting the stored PSK blob using `master_key`.
///
/// The `node_id` column (index 0) is used as AAD for the AES-GCM decryption
/// so that swapping PSK blobs between rows is detected as an authentication failure.
fn row_to_node(row: &rusqlite::Row<'_>, master_key: &[u8; 32]) -> rusqlite::Result<NodeRecord> {
    let node_id: String = row.get(0)?;
    let psk_blob: Vec<u8> = row.get(2)?;
    let psk = decrypt_psk(master_key, &node_id, &psk_blob).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Blob,
            format!("psk decryption failed for node '{node_id}': {e}").into(),
        )
    })?;
    let last_seen_epoch: Option<i64> = row.get(8)?;
    let rf_channel: Option<u8> = row
        .get(9)
        .ok()
        .and_then(|v: Option<u32>| v.and_then(|c| u8::try_from(c).ok()));
    let sensors_json: Option<String> = row.get(10)?;
    let registered_by_phone_id: Option<u32> = row.get(11)?;
    Ok(NodeRecord {
        node_id,
        key_hint: {
            let kh: u32 = row.get(1)?;
            u16::try_from(kh).map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Integer,
                    format!("key_hint {kh} out of u16 range").into(),
                )
            })?
        },
        psk,
        assigned_program_hash: row.get(3)?,
        current_program_hash: row.get(4)?,
        schedule_interval_s: row.get(5)?,
        firmware_abi_version: row.get(6)?,
        last_battery_mv: row.get(7)?,
        last_seen: last_seen_epoch.map(epoch_s_to_system_time),
        rf_channel,
        sensors: sensors_json
            .map(|j| sensors_from_json(&j))
            .unwrap_or_default(),
        registered_by_phone_id,
    })
}

#[async_trait]
impl Storage for SqliteStorage {
    // ── Node registry ──────────────────────────────────────────

    async fn list_nodes(&self) -> Result<Vec<NodeRecord>, StorageError> {
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT node_id, key_hint, psk, assigned_program_hash, \
                     current_program_hash, schedule_interval_s, firmware_abi_version, \
                     last_battery_mv, last_seen_epoch_s, rf_channel, sensors_json, \
                     registered_by_phone_id FROM nodes",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |row| row_to_node(row, &mk))
                .map_err(map_err)?;
            let mut nodes = Vec::new();
            for row in rows {
                nodes.push(row.map_err(map_err)?);
            }
            Ok(nodes)
        })
        .await
    }

    async fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, StorageError> {
        let node_id = node_id.to_string();
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT node_id, key_hint, psk, assigned_program_hash, \
                 current_program_hash, schedule_interval_s, firmware_abi_version, \
                 last_battery_mv, last_seen_epoch_s, rf_channel, sensors_json, \
                 registered_by_phone_id FROM nodes WHERE node_id = ?1",
                params![node_id],
                |row| row_to_node(row, &mk),
            )
            .optional()
            .map_err(map_err)
        })
        .await
    }

    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>, StorageError> {
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT node_id, key_hint, psk, assigned_program_hash, \
                     current_program_hash, schedule_interval_s, firmware_abi_version, \
                     last_battery_mv, last_seen_epoch_s, rf_channel, sensors_json, \
                     registered_by_phone_id FROM nodes WHERE key_hint = ?1",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![key_hint as u32], |row| row_to_node(row, &mk))
                .map_err(map_err)?;
            let mut nodes = Vec::new();
            for row in rows {
                nodes.push(row.map_err(map_err)?);
            }
            Ok(nodes)
        })
        .await
    }

    async fn upsert_node(&self, record: &NodeRecord) -> Result<(), StorageError> {
        let record = record.clone();
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let last_seen_epoch = record.last_seen.as_ref().map(system_time_to_epoch_s);
            let encrypted_psk = encrypt_psk(&mk, &record.node_id, &record.psk)?;
            let sensors_json = sensors_to_json(&record.sensors);
            conn.execute(
                "INSERT INTO nodes (node_id, key_hint, psk, assigned_program_hash, \
                 current_program_hash, schedule_interval_s, firmware_abi_version, \
                 last_battery_mv, last_seen_epoch_s, rf_channel, sensors_json, \
                 registered_by_phone_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(node_id) DO UPDATE SET \
                 key_hint = excluded.key_hint, \
                 psk = excluded.psk, \
                 assigned_program_hash = excluded.assigned_program_hash, \
                 current_program_hash = excluded.current_program_hash, \
                 schedule_interval_s = excluded.schedule_interval_s, \
                 firmware_abi_version = excluded.firmware_abi_version, \
                 last_battery_mv = excluded.last_battery_mv, \
                 last_seen_epoch_s = excluded.last_seen_epoch_s, \
                 rf_channel = excluded.rf_channel, \
                 sensors_json = excluded.sensors_json, \
                 registered_by_phone_id = excluded.registered_by_phone_id",
                params![
                    record.node_id,
                    record.key_hint as u32,
                    encrypted_psk,
                    record.assigned_program_hash,
                    record.current_program_hash,
                    record.schedule_interval_s,
                    record.firmware_abi_version,
                    record.last_battery_mv,
                    last_seen_epoch,
                    record.rf_channel.map(|c| c as u32),
                    sensors_json,
                    record.registered_by_phone_id,
                ],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
    }

    async fn insert_node_if_not_exists(&self, record: &NodeRecord) -> Result<bool, StorageError> {
        let record = record.clone();
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let last_seen_epoch = record.last_seen.as_ref().map(system_time_to_epoch_s);
            let encrypted_psk = encrypt_psk(&mk, &record.node_id, &record.psk)?;
            let sensors_json = sensors_to_json(&record.sensors);
            let rows = conn
                .execute(
                    "INSERT OR IGNORE INTO nodes (node_id, key_hint, psk, assigned_program_hash, \
                     current_program_hash, schedule_interval_s, firmware_abi_version, \
                     last_battery_mv, last_seen_epoch_s, rf_channel, sensors_json, \
                     registered_by_phone_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        record.node_id,
                        record.key_hint as u32,
                        encrypted_psk,
                        record.assigned_program_hash,
                        record.current_program_hash,
                        record.schedule_interval_s,
                        record.firmware_abi_version,
                        record.last_battery_mv,
                        last_seen_epoch,
                        record.rf_channel.map(|c| c as u32),
                        sensors_json,
                        record.registered_by_phone_id,
                    ],
                )
                .map_err(map_err)?;
            Ok(rows > 0)
        })
        .await
    }

    async fn delete_node(&self, node_id: &str) -> Result<(), StorageError> {
        let node_id = node_id.to_string();
        self.with_conn(move |conn| {
            conn.execute("DELETE FROM nodes WHERE node_id = ?1", params![node_id])
                .map_err(map_err)?;
            Ok(())
        })
        .await
    }

    // ── Program library ────────────────────────────────────────

    async fn get_program(&self, hash: &[u8]) -> Result<Option<ProgramRecord>, StorageError> {
        let hash = hash.to_vec();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT hash, image, size, verification_profile, abi_version FROM programs WHERE hash = ?1",
                params![hash],
                |row| {
                    let profile_str: String = row.get(3)?;
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, profile_str, row.get(4)?))
                },
            )
            .optional()
            .map_err(map_err)?
            .map(
                |(hash, image, size, profile_str, abi_version): (Vec<u8>, Vec<u8>, u32, String, Option<u32>)| {
                    Ok(ProgramRecord {
                        hash,
                        image,
                        size,
                        verification_profile: parse_profile(&profile_str)?,
                        abi_version,
                    })
                },
            )
            .transpose()
        })
        .await
    }

    async fn store_program(&self, record: &ProgramRecord) -> Result<(), StorageError> {
        let record = record.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO programs (hash, image, size, verification_profile, abi_version) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(hash) DO UPDATE SET \
                 image=excluded.image, size=excluded.size, \
                 verification_profile=excluded.verification_profile, \
                 abi_version=excluded.abi_version",
                params![
                    record.hash,
                    record.image,
                    record.size,
                    profile_to_str(&record.verification_profile),
                    record.abi_version,
                ],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
    }

    async fn delete_program(&self, hash: &[u8]) -> Result<(), StorageError> {
        let hash = hash.to_vec();
        self.with_conn(move |conn| {
            conn.execute("DELETE FROM programs WHERE hash = ?1", params![hash])
                .map_err(map_err)?;
            Ok(())
        })
        .await
    }

    async fn list_programs(&self) -> Result<Vec<ProgramRecord>, StorageError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT hash, image, size, verification_profile, abi_version FROM programs",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |row| {
                    let profile_str: String = row.get(3)?;
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        profile_str,
                        row.get(4)?,
                    ))
                })
                .map_err(map_err)?;
            let mut programs = Vec::new();
            for row in rows {
                let (hash, image, size, profile_str, abi_version): (
                    Vec<u8>,
                    Vec<u8>,
                    u32,
                    String,
                    Option<u32>,
                ) = row.map_err(map_err)?;
                programs.push(ProgramRecord {
                    hash,
                    image,
                    size,
                    verification_profile: parse_profile(&profile_str)?,
                    abi_version,
                });
            }
            Ok(programs)
        })
        .await
    }

    async fn replace_state(
        &self,
        nodes: &[NodeRecord],
        programs: &[ProgramRecord],
    ) -> Result<(), StorageError> {
        let nodes = nodes.to_vec();
        let programs = programs.to_vec();
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            conn.execute_batch("BEGIN IMMEDIATE").map_err(map_err)?;

            let result = (|| -> Result<(), StorageError> {
                conn.execute("DELETE FROM nodes", []).map_err(map_err)?;
                conn.execute("DELETE FROM programs", []).map_err(map_err)?;

                for record in &programs {
                    conn.execute(
                        "INSERT INTO programs (hash, image, size, verification_profile, abi_version) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            &record.hash,
                            &record.image,
                            record.size,
                            profile_to_str(&record.verification_profile),
                            record.abi_version,
                        ],
                    )
                    .map_err(map_err)?;
                }

                for record in &nodes {
                    let last_seen_epoch: Option<i64> =
                        record.last_seen.as_ref().map(system_time_to_epoch_s);
                    let encrypted_psk =
                        encrypt_psk(&mk, &record.node_id, &record.psk)?;
                    conn.execute(
                        "INSERT INTO nodes (node_id, key_hint, psk, assigned_program_hash, \
                         current_program_hash, schedule_interval_s, firmware_abi_version, \
                         last_battery_mv, last_seen_epoch_s) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            &record.node_id,
                            record.key_hint,
                            &encrypted_psk,
                            record.assigned_program_hash.as_deref(),
                            record.current_program_hash.as_deref(),
                            record.schedule_interval_s,
                            record.firmware_abi_version,
                            record.last_battery_mv,
                            last_seen_epoch,
                        ],
                    )
                    .map_err(map_err)?;
                }
                Ok(())
            })();

            match result {
                Ok(()) => {
                    conn.execute_batch("COMMIT").map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        map_err(e)
                    })?;
                    Ok(())
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })
        .await
    }

    // ── Gateway identity (GW-1200, GW-1201) ───────────────────

    async fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, StorageError> {
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let row: Option<(Vec<u8>, Vec<u8>, Vec<u8>)> = conn
                .query_row(
                    "SELECT encrypted_seed, gateway_id, public_key \
                     FROM gateway_identity WHERE id = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(map_err)?;

            match row {
                None => Ok(None),
                Some((encrypted_seed, gateway_id_vec, public_key_vec)) => {
                    let gateway_id: [u8; 16] = gateway_id_vec
                        .as_slice()
                        .try_into()
                        .map_err(|_| StorageError::Internal("gateway_id is not 16 bytes".into()))?;

                    let seed_bytes = decrypt_seed(&mk, &encrypted_seed, &gateway_id)?;
                    let seed = Zeroizing::new(seed_bytes);

                    let identity = GatewayIdentity::from_parts(seed, gateway_id);

                    // Validate stored public key matches derived value to detect
                    // corruption or tampering.
                    let stored_pk: [u8; 32] = public_key_vec
                        .as_slice()
                        .try_into()
                        .map_err(|_| StorageError::Internal("public_key is not 32 bytes".into()))?;
                    if stored_pk != *identity.public_key() {
                        return Err(StorageError::Internal(
                            "stored public key does not match derived value — \
                             database may be corrupt or tampered with"
                                .into(),
                        ));
                    }

                    Ok(Some(identity))
                }
            }
        })
        .await
    }

    async fn store_gateway_identity(&self, identity: &GatewayIdentity) -> Result<(), StorageError> {
        let mk = self.master_key.clone();
        let seed = Zeroizing::new(*identity.seed());
        let gateway_id_arr = *identity.gateway_id();
        let gateway_id = identity.gateway_id().to_vec();
        let public_key = identity.public_key().to_vec();
        self.with_conn(move |conn| {
            let encrypted_seed = encrypt_seed(&mk, &seed, &gateway_id_arr)?;
            conn.execute(
                "INSERT INTO gateway_identity (id, encrypted_seed, gateway_id, public_key) \
                 VALUES (1, ?1, ?2, ?3) \
                 ON CONFLICT(id) DO UPDATE SET \
                 encrypted_seed=excluded.encrypted_seed, \
                 gateway_id=excluded.gateway_id, \
                 public_key=excluded.public_key",
                params![encrypted_seed, gateway_id, public_key],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
    }

    // ── Phone trust store (GW-1210) ────────────────────────────

    async fn list_phone_psks(&self) -> Result<Vec<PhonePskRecord>, StorageError> {
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT phone_id, phone_key_hint, psk, label, issued_at_epoch_s, status \
                     FROM phone_psks",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(map_err)?;
            let mut records = Vec::new();
            for row in rows {
                let (phone_id, key_hint, psk_blob, label, issued_at, status_str) =
                    row.map_err(map_err)?;
                let psk = Zeroizing::new(decrypt_phone_psk(&mk, phone_id, &psk_blob)?);
                let status = PhonePskStatus::from_str_value(&status_str).ok_or_else(|| {
                    StorageError::Internal(format!("unknown phone psk status: {status_str}"))
                })?;
                records.push(PhonePskRecord {
                    phone_id,
                    phone_key_hint: u16::try_from(key_hint).map_err(|_| {
                        StorageError::Internal(format!(
                            "phone_key_hint {key_hint} out of u16 range for phone_id {phone_id}"
                        ))
                    })?,
                    psk,
                    label,
                    issued_at: epoch_s_to_system_time(issued_at),
                    status,
                });
            }
            Ok(records)
        })
        .await
    }

    async fn get_phone_psks_by_key_hint(
        &self,
        key_hint: u16,
    ) -> Result<Vec<PhonePskRecord>, StorageError> {
        let mk = self.master_key.clone();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT phone_id, phone_key_hint, psk, label, issued_at_epoch_s, status \
                     FROM phone_psks WHERE phone_key_hint = ?1",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![key_hint as u32], |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(map_err)?;
            let mut records = Vec::new();
            for row in rows {
                let (phone_id, kh, psk_blob, label, issued_at, status_str) =
                    row.map_err(map_err)?;
                let psk = Zeroizing::new(decrypt_phone_psk(&mk, phone_id, &psk_blob)?);
                let status = PhonePskStatus::from_str_value(&status_str).ok_or_else(|| {
                    StorageError::Internal(format!("unknown phone psk status: {status_str}"))
                })?;
                records.push(PhonePskRecord {
                    phone_id,
                    phone_key_hint: u16::try_from(kh).map_err(|_| {
                        StorageError::Internal(format!(
                            "phone_key_hint {kh} out of u16 range for phone_id {phone_id}"
                        ))
                    })?,
                    psk,
                    label,
                    issued_at: epoch_s_to_system_time(issued_at),
                    status,
                });
            }
            Ok(records)
        })
        .await
    }

    async fn store_phone_psk(&self, record: &PhonePskRecord) -> Result<u32, StorageError> {
        use crate::phone_trust::PHONE_LABEL_MAX_BYTES;

        if record.label.len() > PHONE_LABEL_MAX_BYTES {
            return Err(StorageError::Internal(format!(
                "phone label exceeds {PHONE_LABEL_MAX_BYTES}-byte limit: {} bytes",
                record.label.len()
            )));
        }

        let mk = self.master_key.clone();
        let record = record.clone();
        self.with_conn(move |conn| {
            let issued_at = system_time_to_epoch_s(&record.issued_at);

            // Wrap INSERT + UPDATE in a transaction so a crash between them
            // cannot leave a row with an invalid placeholder PSK.
            conn.execute_batch("BEGIN IMMEDIATE").map_err(map_err)?;

            let result = (|| -> Result<u32, StorageError> {
                // Insert with a placeholder PSK to get the auto-increment id.
                conn.execute(
                    "INSERT INTO phone_psks \
                     (phone_key_hint, psk, label, issued_at_epoch_s, status) \
                     VALUES (?1, X'00', ?2, ?3, ?4)",
                    params![
                        record.phone_key_hint as u32,
                        &record.label,
                        issued_at,
                        record.status.to_string(),
                    ],
                )
                .map_err(map_err)?;

                let rowid = conn.last_insert_rowid();
                let phone_id = u32::try_from(rowid).map_err(|_| {
                    StorageError::Internal(format!("phone_id rowid {rowid} exceeds u32::MAX"))
                })?;

                // Encrypt PSK with the actual phone_id as AAD and update.
                let encrypted_psk = encrypt_phone_psk(&mk, phone_id, &record.psk)?;
                conn.execute(
                    "UPDATE phone_psks SET psk = ?1 WHERE phone_id = ?2",
                    params![encrypted_psk, phone_id],
                )
                .map_err(map_err)?;

                Ok(phone_id)
            })();

            match result {
                Ok(id) => {
                    conn.execute_batch("COMMIT").map_err(|e| {
                        let _ = conn.execute_batch("ROLLBACK");
                        map_err(e)
                    })?;
                    Ok(id)
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })
        .await
    }

    async fn revoke_phone_psk(&self, phone_id: u32) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            let updated = conn
                .execute(
                    "UPDATE phone_psks SET status = 'revoked' WHERE phone_id = ?1",
                    params![phone_id],
                )
                .map_err(map_err)?;
            if updated == 0 {
                return Err(StorageError::NotFound(format!("phone_id {phone_id}")));
            }
            Ok(())
        })
        .await
    }

    async fn delete_phone_psk(&self, phone_id: u32) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM phone_psks WHERE phone_id = ?1",
                params![phone_id],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed test master key — must not be used outside of tests.
    const TEST_MASTER_KEY_RAW: [u8; 32] = [0x42u8; 32];

    fn test_key() -> Zeroizing<[u8; 32]> {
        Zeroizing::new(TEST_MASTER_KEY_RAW)
    }

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
            rf_channel: None,
            sensors: Vec::new(),
            registered_by_phone_id: None,
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
            abi_version: None,
        }
    }

    #[tokio::test]
    async fn test_node_crud() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

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
        let store = SqliteStorage::in_memory(test_key()).unwrap();

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
        let store = SqliteStorage::in_memory(test_key()).unwrap();

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
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            store.upsert_node(&make_node("p1", 5)).await.unwrap();
            store.store_program(&make_program(0xAA)).await.unwrap();
        }

        // Second open: data survives.
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            assert!(store.get_node("p1").await.unwrap().is_some());
            assert!(store.get_program(&[0xAA; 32]).await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn test_upsert_overwrites() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

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

    /// GW-0601a migration: existing databases with plaintext 32-byte PSK blobs
    /// must be transparently re-encrypted on the first `open()` with a master key.
    #[tokio::test]
    async fn test_legacy_psk_migration() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");
        let psk = [0xBEu8; 32];

        // Simulate a pre-GW-0601a database by writing a plaintext PSK blob directly.
        {
            use rusqlite::Connection;
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
            )
            .unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO nodes \
                 (node_id, key_hint, psk, schedule_interval_s) \
                 VALUES ('legacy-node', 7, ?1, 60)",
                params![psk.to_vec()],
            )
            .unwrap();
            // Verify the blob is 32 bytes (plaintext) before migration.
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT psk FROM nodes WHERE node_id = 'legacy-node'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(blob.len(), 32, "pre-migration blob must be 32 bytes");
        }

        // Open with the master key — migration runs automatically.
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            // The node must be readable and return the correct PSK.
            let node = store.get_node("legacy-node").await.unwrap().unwrap();
            assert_eq!(node.psk, psk, "migrated PSK must match original plaintext");
        }

        // After migration the on-disk blob must be encrypted (60 bytes).
        {
            use rusqlite::Connection;
            let conn = Connection::open(&db_path).unwrap();
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT psk FROM nodes WHERE node_id = 'legacy-node'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                blob.len(),
                ENCRYPTED_PSK_LEN,
                "post-migration blob must be encrypted"
            );
        }
    }

    /// Verify that opening an existing database that predates the `abi_version`
    /// column applies the migration and continues to work correctly.
    #[tokio::test]
    async fn test_abi_version_migration() {
        use rusqlite::Connection;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("old.db");

        // Create a "legacy" database without the abi_version column.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS programs (
                    hash BLOB PRIMARY KEY,
                    image BLOB NOT NULL,
                    size INTEGER NOT NULL,
                    verification_profile TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO programs (hash, image, size, verification_profile) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![vec![0x01u8; 32], vec![0x02u8; 4], 4i64, "Resident"],
            )
            .unwrap();
        }

        // Open with SqliteStorage — migration should add abi_version column.
        let store = SqliteStorage::open(&db_path, test_key()).unwrap();

        // The migrated row has abi_version = NULL (i.e., None).
        let prog = store
            .get_program(&[0x01u8; 32])
            .await
            .unwrap()
            .expect("program must survive migration");
        assert_eq!(
            prog.abi_version, None,
            "migrated rows must have abi_version = None"
        );

        // Writing and reading a new program with abi_version works.
        let mut new_prog = make_program(0x42);
        new_prog.abi_version = Some(2);
        store.store_program(&new_prog).await.unwrap();
        let fetched = store.get_program(&new_prog.hash).await.unwrap().unwrap();
        assert_eq!(fetched.abi_version, Some(2));
    }

    /// Verify that `open()` rejects a wrong master key when encrypted PSK rows
    /// already exist in the database.
    #[tokio::test]
    async fn test_wrong_master_key_rejected_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("keycheck.db");

        // Create a database with a node encrypted under the test key.
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            store.upsert_node(&make_node("node-a", 1)).await.unwrap();
        }

        // Re-opening with the same key must succeed.
        SqliteStorage::open(&db_path, test_key()).expect("correct key must succeed");

        // Re-opening with a different key must fail.
        let wrong_key = Zeroizing::new([0xFFu8; 32]);
        assert!(
            SqliteStorage::open(&db_path, wrong_key).is_err(),
            "wrong key must fail to open"
        );
    }

    #[tokio::test]
    async fn test_replace_state_encrypts_psks() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        // Seed with an existing node that will be replaced.
        store.upsert_node(&make_node("old", 1)).await.unwrap();

        let node_a = make_node("node-a", 10);
        let node_b = make_node("node-b", 20);
        let prog = make_program(0xCC);

        store
            .replace_state(
                &[node_a.clone(), node_b.clone()],
                std::slice::from_ref(&prog),
            )
            .await
            .unwrap();

        // Old node must be gone.
        assert!(store.get_node("old").await.unwrap().is_none());

        // New nodes must be readable (PSKs decryptable).
        let fetched_a = store.get_node("node-a").await.unwrap().unwrap();
        assert_eq!(fetched_a.psk, node_a.psk);
        assert_eq!(fetched_a.key_hint, 10);

        let fetched_b = store.get_node("node-b").await.unwrap().unwrap();
        assert_eq!(fetched_b.psk, node_b.psk);

        // list_nodes must also return both (verifies no plaintext blob
        // trips the 60-byte decrypt_psk check).
        let all_nodes = store.list_nodes().await.unwrap();
        assert_eq!(all_nodes.len(), 2);

        // Program must be present.
        let fetched_prog = store.get_program(&prog.hash).await.unwrap().unwrap();
        assert_eq!(fetched_prog.image, prog.image);
    }

    // ── Gateway identity tests (T-1200, T-1201) ───────────────

    #[tokio::test]
    async fn test_gateway_identity_store_and_load() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        // No identity initially.
        assert!(store.load_gateway_identity().await.unwrap().is_none());

        // Generate and store.
        let identity = GatewayIdentity::generate().unwrap();
        store.store_gateway_identity(&identity).await.unwrap();

        // Load and verify round-trip.
        let loaded = store.load_gateway_identity().await.unwrap().unwrap();
        assert_eq!(loaded.public_key(), identity.public_key());
        assert_eq!(loaded.gateway_id(), identity.gateway_id());
        assert_eq!(loaded.seed(), identity.seed());
    }

    #[tokio::test]
    async fn test_gateway_identity_persistence_across_reopen() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("identity.db");

        let identity = GatewayIdentity::generate().unwrap();

        // Store identity.
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            store.store_gateway_identity(&identity).await.unwrap();
        }

        // Reopen and verify persistence (T-1200 acceptance criteria #3).
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            let loaded = store.load_gateway_identity().await.unwrap().unwrap();
            assert_eq!(loaded.public_key(), identity.public_key());
            assert_eq!(loaded.gateway_id(), identity.gateway_id());
        }
    }

    #[tokio::test]
    async fn test_gateway_identity_seed_encrypted_at_rest() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("encrypted.db");

        let identity = GatewayIdentity::generate().unwrap();
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            store.store_gateway_identity(&identity).await.unwrap();
        }

        // Read the raw encrypted_seed from the database.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT encrypted_seed FROM gateway_identity WHERE id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            // Must be encrypted (60 bytes), not plaintext (32 bytes).
            assert_eq!(blob.len(), ENCRYPTED_PSK_LEN);
            // Encrypted blob must NOT match the plaintext seed.
            assert_ne!(&blob[12..44], identity.seed().as_slice());
        }
    }

    #[tokio::test]
    async fn test_gateway_identity_overwrite() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        let id1 = GatewayIdentity::generate().unwrap();
        store.store_gateway_identity(&id1).await.unwrap();

        let id2 = GatewayIdentity::generate().unwrap();
        store.store_gateway_identity(&id2).await.unwrap();

        // Should load the second identity.
        let loaded = store.load_gateway_identity().await.unwrap().unwrap();
        assert_eq!(loaded.public_key(), id2.public_key());
        assert_ne!(loaded.public_key(), id1.public_key());
    }

    // ── Phone PSK trust store tests (GW-1210) ─────────────────

    fn make_phone_psk(hint: u16, label: &str) -> PhonePskRecord {
        PhonePskRecord {
            phone_id: 0, // auto-assigned
            phone_key_hint: hint,
            psk: Zeroizing::new([0xBB; 32]),
            label: label.to_string(),
            issued_at: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1700000000),
            status: PhonePskStatus::Active,
        }
    }

    #[tokio::test]
    async fn test_phone_psk_store_and_list() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        // No PSKs initially.
        assert!(store.list_phone_psks().await.unwrap().is_empty());

        let phone_id = store
            .store_phone_psk(&make_phone_psk(42, "Alice's phone"))
            .await
            .unwrap();
        assert!(phone_id > 0);

        let all = store.list_phone_psks().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].phone_id, phone_id);
        assert_eq!(all[0].phone_key_hint, 42);
        assert_eq!(all[0].label, "Alice's phone");
        assert_eq!(*all[0].psk, [0xBB; 32]);
        assert_eq!(all[0].status, PhonePskStatus::Active);
    }

    #[tokio::test]
    async fn test_phone_psk_lookup_by_key_hint() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        // Store two PSKs with different hints and one with colliding hint.
        store
            .store_phone_psk(&make_phone_psk(100, "Phone A"))
            .await
            .unwrap();
        store
            .store_phone_psk(&make_phone_psk(200, "Phone B"))
            .await
            .unwrap();
        store
            .store_phone_psk(&make_phone_psk(100, "Phone C"))
            .await
            .unwrap();

        // Lookup hint=100 should return 2 records.
        let found = store.get_phone_psks_by_key_hint(100).await.unwrap();
        assert_eq!(found.len(), 2);

        // Lookup hint=200 should return 1.
        let found = store.get_phone_psks_by_key_hint(200).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Phone B");

        // Lookup non-existent hint returns empty.
        let found = store.get_phone_psks_by_key_hint(999).await.unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn test_phone_psk_revocation() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        let phone_id = store
            .store_phone_psk(&make_phone_psk(42, "Revokable"))
            .await
            .unwrap();

        // Revoke it.
        store.revoke_phone_psk(phone_id).await.unwrap();

        // Verify it's revoked.
        let all = store.list_phone_psks().await.unwrap();
        assert_eq!(all[0].status, PhonePskStatus::Revoked);

        // Revoking a non-existent phone_id should return NotFound.
        let result = store.revoke_phone_psk(99999).await;
        assert!(matches!(result, Err(StorageError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_phone_psk_delete() {
        let store = SqliteStorage::in_memory(test_key()).unwrap();

        let phone_id = store
            .store_phone_psk(&make_phone_psk(42, "Deletable"))
            .await
            .unwrap();
        assert_eq!(store.list_phone_psks().await.unwrap().len(), 1);

        store.delete_phone_psk(phone_id).await.unwrap();
        assert!(store.list_phone_psks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_phone_psk_encrypted_at_rest() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("phone.db");

        let psk_bytes = [0xCC; 32];
        let mut record = make_phone_psk(42, "Encrypted");
        record.psk = Zeroizing::new(psk_bytes);

        let phone_id;
        {
            let store = SqliteStorage::open(&db_path, test_key()).unwrap();
            phone_id = store.store_phone_psk(&record).await.unwrap();
        }

        // Read raw blob from database.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT psk FROM phone_psks WHERE phone_id = ?1",
                    params![phone_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(blob.len(), ENCRYPTED_PSK_LEN);
            assert_ne!(&blob[12..44], psk_bytes.as_slice());
        }
    }
}
