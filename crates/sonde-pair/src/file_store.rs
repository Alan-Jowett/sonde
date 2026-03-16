// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Persistent [`PairingStore`] backed by a JSON file.
//!
//! Enabled by the `file-store` cargo feature. Stores pairing artifacts in a
//! platform-appropriate location:
//!
//! - **Windows:** `%APPDATA%\sonde\pairing.json`
//! - **Linux / macOS:** `~/.config/sonde/pairing.json`
//!
//! Writes are atomic (write-to-temp, then rename) to prevent corruption on
//! crash. On Unix the file is restricted to user-only access (mode `0o600`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::PairingError;
use crate::store::PairingStore;
use crate::types::{GatewayIdentity, PairingArtifacts};

// ---------------------------------------------------------------------------
// Serialisation types — kept private so the JSON schema is an internal detail.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoredData {
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_identity: Option<StoredGatewayIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<StoredArtifacts>,
}

#[derive(Serialize, Deserialize)]
struct StoredGatewayIdentity {
    public_key: String,
    gateway_id: String,
}

#[derive(Serialize, Deserialize)]
struct StoredArtifacts {
    gateway_public_key: String,
    gateway_id: String,
    phone_psk: String,
    phone_key_hint: u16,
    rf_channel: u8,
    phone_label: String,
}

// ---------------------------------------------------------------------------
// FilePairingStore
// ---------------------------------------------------------------------------

/// Persistent [`PairingStore`] backed by a JSON file.
///
/// See [module-level documentation](self) for platform paths and atomicity
/// guarantees.
pub struct FilePairingStore {
    path: PathBuf,
}

impl FilePairingStore {
    /// Create a store at the platform default location.
    pub fn new() -> Result<Self, PairingError> {
        Ok(Self {
            path: default_path()?,
        })
    }

    /// Create a store at a caller-chosen path (useful for tests).
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the file path backing this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // -- internal helpers ---------------------------------------------------

    fn read_stored(&self) -> Result<Option<StoredData>, PairingError> {
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PairingError::StoreLoadFailed(e.to_string())),
        };

        let data: StoredData = serde_json::from_slice(&bytes).map_err(|e| {
            PairingError::StoreCorrupted(format!("{e}: delete or fix {}", self.path.display()))
        })?;

        Ok(Some(data))
    }

    fn write_stored(&self, data: &StoredData) -> Result<(), PairingError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        }

        let json = serde_json::to_string_pretty(data)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;

        // Atomic write: temp file → fsync → rename.
        let temp_path = self.path.with_extension("json.tmp");
        let mut file = fs::File::create(&temp_path)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        file.write_all(json.as_bytes())
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        file.sync_all()
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        drop(file);

        // Set restrictive permissions *before* rename so the file is never
        // world-readable, even briefly.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))
                .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        }

        fs::rename(&temp_path, &self.path)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;

        Ok(())
    }
}

impl PairingStore for FilePairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        let mut data = self.read_stored()?.unwrap_or(StoredData {
            gateway_identity: None,
            artifacts: None,
        });
        data.artifacts = Some(artifacts_to_stored(artifacts));
        self.write_stored(&data)
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        match self.read_stored()? {
            Some(StoredData {
                artifacts: Some(ref s),
                ..
            }) => Ok(Some(artifacts_from_stored(s)?)),
            _ => Ok(None),
        }
    }

    fn clear(&mut self) -> Result<(), PairingError> {
        // Also clean up any leftover temp file from an interrupted write.
        let _ = fs::remove_file(self.path.with_extension("json.tmp"));
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PairingError::StoreSaveFailed(e.to_string())),
        }
    }

    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError> {
        let data = match self.read_stored()? {
            Some(d) => d,
            None => return Ok(None),
        };
        // Standalone identity takes precedence (mirrors MemoryPairingStore).
        if let Some(ref id) = data.gateway_identity {
            return Ok(Some(identity_from_stored(id)?));
        }
        if let Some(ref arts) = data.artifacts {
            let pk = from_hex(&arts.gateway_public_key, 32)?;
            let gid = from_hex(&arts.gateway_id, 16)?;
            return Ok(Some(GatewayIdentity {
                public_key: pk.try_into().unwrap(),
                gateway_id: gid.try_into().unwrap(),
            }));
        }
        Ok(None)
    }

    fn save_gateway_identity(&mut self, identity: &GatewayIdentity) -> Result<(), PairingError> {
        let mut data = self.read_stored()?.unwrap_or(StoredData {
            gateway_identity: None,
            artifacts: None,
        });
        data.gateway_identity = Some(identity_to_stored(identity));
        self.write_stored(&data)
    }
}

// ---------------------------------------------------------------------------
// Hex helpers — validate ASCII before byte-indexing (see code-quality guide).
// ---------------------------------------------------------------------------

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(hex: &str, expected_len: usize) -> Result<Vec<u8>, PairingError> {
    if hex.len() != expected_len * 2 {
        return Err(PairingError::StoreCorrupted(format!(
            "expected {} hex chars, got {}",
            expected_len * 2,
            hex.len()
        )));
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(PairingError::StoreCorrupted(
            "invalid hex character in stored data".into(),
        ));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| PairingError::StoreCorrupted(e.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Domain ↔ stored conversions
// ---------------------------------------------------------------------------

fn identity_to_stored(id: &GatewayIdentity) -> StoredGatewayIdentity {
    StoredGatewayIdentity {
        public_key: to_hex(&id.public_key),
        gateway_id: to_hex(&id.gateway_id),
    }
}

fn identity_from_stored(s: &StoredGatewayIdentity) -> Result<GatewayIdentity, PairingError> {
    let pk = from_hex(&s.public_key, 32)?;
    let gid = from_hex(&s.gateway_id, 16)?;
    Ok(GatewayIdentity {
        public_key: pk.try_into().unwrap(),
        gateway_id: gid.try_into().unwrap(),
    })
}

fn artifacts_to_stored(a: &PairingArtifacts) -> StoredArtifacts {
    StoredArtifacts {
        gateway_public_key: to_hex(&a.gateway_identity.public_key),
        gateway_id: to_hex(&a.gateway_identity.gateway_id),
        phone_psk: to_hex(a.phone_psk.as_ref()),
        phone_key_hint: a.phone_key_hint,
        rf_channel: a.rf_channel,
        phone_label: a.phone_label.clone(),
    }
}

fn artifacts_from_stored(s: &StoredArtifacts) -> Result<PairingArtifacts, PairingError> {
    let pk = from_hex(&s.gateway_public_key, 32)?;
    let gid = from_hex(&s.gateway_id, 16)?;

    let mut psk_vec = from_hex(&s.phone_psk, 32)?;
    let mut psk = Zeroizing::new([0u8; 32]);
    psk.copy_from_slice(&psk_vec);
    psk_vec.zeroize();

    Ok(PairingArtifacts {
        gateway_identity: GatewayIdentity {
            public_key: pk.try_into().unwrap(),
            gateway_id: gid.try_into().unwrap(),
        },
        phone_psk: psk,
        phone_key_hint: s.phone_key_hint,
        rf_channel: s.rf_channel,
        phone_label: s.phone_label.clone(),
    })
}

// ---------------------------------------------------------------------------
// Default path
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn default_path() -> Result<PathBuf, PairingError> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| PairingError::StoreCorrupted("%APPDATA% not set".into()))?;
    Ok(PathBuf::from(appdata).join("sonde").join("pairing.json"))
}

#[cfg(not(target_os = "windows"))]
fn default_path() -> Result<PathBuf, PairingError> {
    let home =
        std::env::var("HOME").map_err(|_| PairingError::StoreCorrupted("$HOME not set".into()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("sonde")
        .join("pairing.json"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_artifacts() -> PairingArtifacts {
        PairingArtifacts {
            gateway_identity: GatewayIdentity {
                public_key: [0x42u8; 32],
                gateway_id: [0x01u8; 16],
            },
            phone_psk: Zeroizing::new([0x42u8; 32]),
            phone_key_hint: 0x1234,
            rf_channel: 6,
            phone_label: "test-phone".into(),
        }
    }

    fn temp_store() -> (FilePairingStore, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let store = FilePairingStore::with_path(dir.path().join("pairing.json"));
        (store, dir)
    }

    #[test]
    fn save_and_load_round_trip() {
        let (mut store, _dir) = temp_store();
        let artifacts = test_artifacts();
        store.save_artifacts(&artifacts).unwrap();

        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should have artifacts");
        assert_eq!(loaded.gateway_identity, artifacts.gateway_identity);
        assert_eq!(*loaded.phone_psk, *artifacts.phone_psk);
        assert_eq!(loaded.phone_key_hint, artifacts.phone_key_hint);
        assert_eq!(loaded.rf_channel, artifacts.rf_channel);
        assert_eq!(loaded.phone_label, artifacts.phone_label);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let (store, _dir) = temp_store();
        assert!(store.load_artifacts().unwrap().is_none());
        assert!(store.load_gateway_identity().unwrap().is_none());
    }

    #[test]
    fn corrupted_json_returns_store_corrupted() {
        let (store, _dir) = temp_store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"not valid json{{{").unwrap();

        let err = store.load_artifacts().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("corrupted"),
            "error should mention corruption: {msg}"
        );
        assert!(
            msg.contains("pairing.json"),
            "error should name the file: {msg}"
        );
    }

    #[test]
    fn missing_field_returns_store_corrupted() {
        let (store, _dir) = temp_store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        // Valid JSON but missing required fields in artifacts.
        fs::write(store.path(), r#"{"artifacts": {"phone_key_hint": 1}}"#).unwrap();

        let err = store.load_artifacts().unwrap_err();
        assert!(
            err.to_string().contains("corrupted"),
            "should report corruption: {err}"
        );
    }

    #[test]
    fn clear_removes_file() {
        let (mut store, _dir) = temp_store();
        store.save_artifacts(&test_artifacts()).unwrap();
        assert!(store.path().exists());

        store.clear().unwrap();
        assert!(!store.path().exists());
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn clear_missing_file_is_ok() {
        let (mut store, _dir) = temp_store();
        // Should not error when the file doesn't exist.
        store.clear().unwrap();
    }

    #[test]
    fn gateway_identity_standalone() {
        let (mut store, _dir) = temp_store();
        let identity = GatewayIdentity {
            public_key: [0x42u8; 32],
            gateway_id: [0x01u8; 16],
        };
        store.save_gateway_identity(&identity).unwrap();

        let loaded = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity");
        assert_eq!(loaded, identity);
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn gateway_identity_from_artifacts() {
        let (mut store, _dir) = temp_store();
        store.save_artifacts(&test_artifacts()).unwrap();

        let loaded = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity");
        assert_eq!(loaded.public_key, [0x42u8; 32]);
        assert_eq!(loaded.gateway_id, [0x01u8; 16]);
    }

    #[test]
    fn standalone_identity_takes_precedence() {
        let (mut store, _dir) = temp_store();
        store.save_artifacts(&test_artifacts()).unwrap();

        let new_identity = GatewayIdentity {
            public_key: [0x99u8; 32],
            gateway_id: [0x02u8; 16],
        };
        store.save_gateway_identity(&new_identity).unwrap();

        let loaded = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity");
        assert_eq!(loaded, new_identity);
    }

    #[test]
    fn creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let nested = dir
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("pairing.json");
        let mut store = FilePairingStore::with_path(&nested);
        store.save_artifacts(&test_artifacts()).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn no_node_psk_in_json() {
        let (mut store, _dir) = temp_store();
        store.save_artifacts(&test_artifacts()).unwrap();

        let json = fs::read_to_string(store.path()).unwrap();
        assert!(
            !json.contains("node_psk"),
            "node_psk must never appear in persisted JSON"
        );
    }

    #[test]
    fn persists_across_instances() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");

        {
            let mut store = FilePairingStore::with_path(&path);
            store.save_artifacts(&test_artifacts()).unwrap();
        }

        // New instance at the same path should see persisted data.
        let store = FilePairingStore::with_path(&path);
        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should survive across instances");
        assert_eq!(loaded.phone_key_hint, 0x1234);
    }

    #[test]
    fn hex_round_trip() {
        let input = [0xDE, 0xAD, 0xBE, 0xEF];
        let hex = to_hex(&input);
        assert_eq!(hex, "deadbeef");
        let out = from_hex(&hex, 4).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn hex_rejects_non_ascii() {
        let err = from_hex("café0000", 4).unwrap_err();
        assert!(err.to_string().contains("corrupted"));
    }

    #[test]
    fn hex_rejects_wrong_length() {
        let err = from_hex("abcd", 4).unwrap_err();
        assert!(err.to_string().contains("corrupted"));
    }
}
