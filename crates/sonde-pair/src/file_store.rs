// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Persistent [`PairingStore`] backed by a JSON file on the local filesystem.
//!
//! Enabled via the `file-store` Cargo feature.
//!
//! # Platform paths
//!
//! [`FilePairingStore::default_location`] resolves to:
//! - **Windows**: `%APPDATA%\sonde\pairing.json`
//! - **Linux/macOS**: `$XDG_CONFIG_HOME/sonde/pairing.json` (defaults to
//!   `~/.config/sonde/pairing.json`)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::PairingError;
use crate::store::PairingStore;
use crate::types::{GatewayIdentity, PairingArtifacts};

// ---------------------------------------------------------------------------
// JSON schema
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoredGatewayIdentity {
    public_key: String,
    gateway_id: String,
}

#[derive(Serialize, Deserialize)]
struct StoredPhoneCredentials {
    phone_psk: String,
    phone_key_hint: u16,
    rf_channel: u8,
    phone_label: String,
}

#[derive(Serialize, Deserialize)]
struct StoredData {
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_identity: Option<StoredGatewayIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone_credentials: Option<StoredPhoneCredentials>,
}

// ---------------------------------------------------------------------------
// FilePairingStore
// ---------------------------------------------------------------------------

/// Persistent [`PairingStore`] backed by a JSON file.
///
/// Writes are atomic (temp file + rename) to prevent corruption on crash.
/// On Unix, file permissions are set to `0o600` (owner read/write only).
pub struct FilePairingStore {
    path: PathBuf,
}

impl FilePairingStore {
    /// Create a store at the given file path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Create a store at the platform default location.
    ///
    /// - **Windows**: `%APPDATA%\sonde\pairing.json`
    /// - **Linux/macOS**: `$XDG_CONFIG_HOME/sonde/pairing.json`
    ///   (defaults to `~/.config/sonde/pairing.json`)
    pub fn default_location() -> Result<Self, PairingError> {
        let dir = default_config_dir().ok_or_else(|| {
            PairingError::StoreLoadFailed(
                "cannot determine config directory: \
                 set APPDATA (Windows) or HOME (Unix)"
                    .into(),
            )
        })?;
        Ok(Self {
            path: dir.join("sonde").join("pairing.json"),
        })
    }

    /// Returns the file path used by this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read and deserialize the JSON file, returning `None` if it does not
    /// exist.
    fn read_stored(&self) -> Result<Option<StoredData>, PairingError> {
        let mut contents = match fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(PairingError::StoreLoadFailed(format!(
                    "{}: {e}",
                    self.path.display()
                )))
            }
        };

        let result = serde_json::from_str::<StoredData>(&contents);
        contents.zeroize();

        match result {
            Ok(data) => Ok(Some(data)),
            Err(e) => Err(PairingError::StoreCorrupted(format!(
                "invalid JSON in {}: {e}",
                self.path.display()
            ))),
        }
    }

    /// Serialize and atomically write the JSON file.
    fn write_stored(&self, data: &StoredData) -> Result<(), PairingError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                PairingError::StoreSaveFailed(format!(
                    "cannot create directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let mut json = serde_json::to_string_pretty(data)
            .map_err(|e| PairingError::StoreSaveFailed(format!("JSON encoding failed: {e}")))?;

        let tmp_path = self.path.with_extension("json.tmp");
        let write_result = fs::write(&tmp_path, json.as_bytes());
        json.zeroize();
        write_result.map_err(|e| {
            PairingError::StoreSaveFailed(format!("write to {}: {e}", tmp_path.display()))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                PairingError::StoreSaveFailed(format!(
                    "set permissions on {}: {e}",
                    tmp_path.display()
                ))
            })?;
        }

        fs::rename(&tmp_path, &self.path).map_err(|e| {
            PairingError::StoreSaveFailed(format!(
                "rename {} → {}: {e}",
                tmp_path.display(),
                self.path.display()
            ))
        })?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn decode_gateway_identity(
    stored: &StoredGatewayIdentity,
) -> Result<GatewayIdentity, PairingError> {
    let pk_bytes = hex::decode(&stored.public_key)
        .map_err(|e| PairingError::StoreCorrupted(format!("invalid public_key hex: {e}")))?;
    if pk_bytes.len() != 32 {
        return Err(PairingError::StoreCorrupted(format!(
            "public_key must be 32 bytes, got {}",
            pk_bytes.len()
        )));
    }

    let gw_bytes = hex::decode(&stored.gateway_id)
        .map_err(|e| PairingError::StoreCorrupted(format!("invalid gateway_id hex: {e}")))?;
    if gw_bytes.len() != 16 {
        return Err(PairingError::StoreCorrupted(format!(
            "gateway_id must be 16 bytes, got {}",
            gw_bytes.len()
        )));
    }

    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&pk_bytes);

    let mut gateway_id = [0u8; 16];
    gateway_id.copy_from_slice(&gw_bytes);

    Ok(GatewayIdentity {
        public_key,
        gateway_id,
    })
}

fn encode_gateway_identity(identity: &GatewayIdentity) -> StoredGatewayIdentity {
    StoredGatewayIdentity {
        public_key: hex::encode(identity.public_key),
        gateway_id: hex::encode(identity.gateway_id),
    }
}

// ---------------------------------------------------------------------------
// PairingStore impl
// ---------------------------------------------------------------------------

impl PairingStore for FilePairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        let mut data = StoredData {
            gateway_identity: Some(encode_gateway_identity(&artifacts.gateway_identity)),
            phone_credentials: Some(StoredPhoneCredentials {
                phone_psk: hex::encode(*artifacts.phone_psk),
                phone_key_hint: artifacts.phone_key_hint,
                rf_channel: artifacts.rf_channel,
                phone_label: artifacts.phone_label.clone(),
            }),
        };

        let result = self.write_stored(&data);
        if let Some(ref mut creds) = data.phone_credentials {
            creds.phone_psk.zeroize();
        }
        result
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        let stored = match self.read_stored()? {
            Some(s) => s,
            None => return Ok(None),
        };

        // Both gateway_identity and phone_credentials must be present.
        let (gw_stored, mut creds) = match (stored.gateway_identity, stored.phone_credentials) {
            (Some(gw), Some(c)) => (gw, c),
            (_, Some(mut c)) => {
                c.phone_psk.zeroize();
                return Ok(None);
            }
            _ => return Ok(None),
        };

        let gateway_identity = decode_gateway_identity(&gw_stored)?;

        let mut psk_bytes = hex::decode(&creds.phone_psk)
            .map_err(|e| PairingError::StoreCorrupted(format!("invalid phone_psk hex: {e}")))?;
        creds.phone_psk.zeroize();

        if psk_bytes.len() != 32 {
            psk_bytes.zeroize();
            return Err(PairingError::StoreCorrupted(format!(
                "phone_psk must be 32 bytes, got {}",
                psk_bytes.len()
            )));
        }

        let mut phone_psk = Zeroizing::new([0u8; 32]);
        phone_psk.copy_from_slice(&psk_bytes);
        psk_bytes.zeroize();

        Ok(Some(PairingArtifacts {
            gateway_identity,
            phone_psk,
            phone_key_hint: creds.phone_key_hint,
            rf_channel: creds.rf_channel,
            phone_label: creds.phone_label,
        }))
    }

    fn clear(&mut self) -> Result<(), PairingError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PairingError::StoreSaveFailed(format!(
                "cannot remove {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError> {
        let mut stored = match self.read_stored()? {
            Some(s) => s,
            None => return Ok(None),
        };

        // Zeroize phone credentials if present (not needed for this call).
        if let Some(ref mut creds) = stored.phone_credentials {
            creds.phone_psk.zeroize();
        }

        match stored.gateway_identity {
            Some(ref gw) => Ok(Some(decode_gateway_identity(gw)?)),
            None => Ok(None),
        }
    }

    fn save_gateway_identity(&mut self, identity: &GatewayIdentity) -> Result<(), PairingError> {
        let mut existing = self.read_stored()?.unwrap_or(StoredData {
            gateway_identity: None,
            phone_credentials: None,
        });

        existing.gateway_identity = Some(encode_gateway_identity(identity));

        let result = self.write_stored(&existing);
        // Zeroize phone credentials that may have been loaded.
        if let Some(ref mut creds) = existing.phone_credentials {
            creds.phone_psk.zeroize();
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Platform default config directory
// ---------------------------------------------------------------------------

fn default_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }
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
            phone_psk: Zeroizing::new([0xABu8; 32]),
            phone_key_hint: 0x1234,
            rf_channel: 6,
            phone_label: "test-phone".into(),
        }
    }

    fn store_in(dir: &TempDir) -> FilePairingStore {
        FilePairingStore::new(dir.path().join("pairing.json"))
    }

    #[test]
    fn round_trip() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
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
    fn missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        assert!(store.load_artifacts().unwrap().is_none());
        assert!(store.load_gateway_identity().unwrap().is_none());
    }

    #[test]
    fn corrupted_json_returns_store_corrupted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        fs::write(&path, "not valid json!!!").unwrap();

        let store = FilePairingStore::new(path);
        let err = store.load_artifacts().unwrap_err();
        assert!(
            matches!(err, PairingError::StoreCorrupted(_)),
            "expected StoreCorrupted, got: {err}"
        );
    }

    #[test]
    fn empty_file_returns_store_corrupted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        fs::write(&path, "").unwrap();

        let store = FilePairingStore::new(path);
        let err = store.load_artifacts().unwrap_err();
        assert!(
            matches!(err, PairingError::StoreCorrupted(_)),
            "expected StoreCorrupted, got: {err}"
        );
    }

    #[test]
    fn corrupted_hex_returns_store_corrupted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        let bad = serde_json::json!({
            "gateway_identity": {
                "public_key": "not-valid-hex",
                "gateway_id": "also-not-hex"
            }
        });
        fs::write(&path, bad.to_string()).unwrap();

        let store = FilePairingStore::new(path);
        let err = store.load_gateway_identity().unwrap_err();
        assert!(
            matches!(err, PairingError::StoreCorrupted(_)),
            "expected StoreCorrupted, got: {err}"
        );
    }

    #[test]
    fn wrong_public_key_length_returns_store_corrupted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        let bad = serde_json::json!({
            "gateway_identity": {
                "public_key": "aabb",
                "gateway_id": hex::encode([0x01u8; 16])
            }
        });
        fs::write(&path, bad.to_string()).unwrap();

        let store = FilePairingStore::new(path);
        let err = store.load_gateway_identity().unwrap_err();
        assert!(
            matches!(err, PairingError::StoreCorrupted(_)),
            "expected StoreCorrupted, got: {err}"
        );
    }

    #[test]
    fn wrong_phone_psk_length_returns_store_corrupted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        let bad = serde_json::json!({
            "gateway_identity": {
                "public_key": hex::encode([0x42u8; 32]),
                "gateway_id": hex::encode([0x01u8; 16])
            },
            "phone_credentials": {
                "phone_psk": "aabb",
                "phone_key_hint": 0x1234,
                "rf_channel": 6,
                "phone_label": "test"
            }
        });
        fs::write(&path, bad.to_string()).unwrap();

        let store = FilePairingStore::new(path);
        let err = store.load_artifacts().unwrap_err();
        assert!(
            matches!(err, PairingError::StoreCorrupted(_)),
            "expected StoreCorrupted, got: {err}"
        );
    }

    #[test]
    fn gateway_identity_standalone() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
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
    fn save_gateway_identity_preserves_phone_credentials() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        store.save_artifacts(&test_artifacts()).unwrap();

        let new_identity = GatewayIdentity {
            public_key: [0x99u8; 32],
            gateway_id: [0x02u8; 16],
        };
        store.save_gateway_identity(&new_identity).unwrap();

        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should have artifacts");
        assert_eq!(loaded.gateway_identity, new_identity);
        assert_eq!(*loaded.phone_psk, [0xABu8; 32]);
    }

    #[test]
    fn clear_removes_file() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        store.save_artifacts(&test_artifacts()).unwrap();
        assert!(store.path().exists());

        store.clear().unwrap();
        assert!(!store.path().exists());
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn clear_missing_file_is_ok() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        store.clear().unwrap();
    }

    #[test]
    fn no_node_psk_in_stored_json() {
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        store.save_artifacts(&test_artifacts()).unwrap();

        let contents = fs::read_to_string(store.path()).unwrap();
        assert!(
            !contents.contains("node_psk"),
            "node_psk must never appear in stored JSON"
        );
    }

    #[test]
    fn gateway_identity_only_no_artifacts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        let gw_only = serde_json::json!({
            "gateway_identity": {
                "public_key": hex::encode([0x42u8; 32]),
                "gateway_id": hex::encode([0x01u8; 16])
            }
        });
        fs::write(&path, gw_only.to_string()).unwrap();

        let store = FilePairingStore::new(path);
        assert!(store.load_artifacts().unwrap().is_none());

        let identity = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity");
        assert_eq!(identity.public_key, [0x42u8; 32]);
    }

    #[test]
    fn creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dirs").join("pairing.json");
        let mut store = FilePairingStore::new(path);
        store.save_artifacts(&test_artifacts()).unwrap();

        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should have artifacts");
        assert_eq!(loaded.rf_channel, 6);
    }

    #[test]
    fn default_location_returns_valid_path() {
        // This test just verifies `default_location` succeeds (it reads
        // environment variables, which should be set on any desktop OS).
        let store = FilePairingStore::default_location().unwrap();
        let path = store.path().to_string_lossy();
        assert!(
            path.contains("sonde"),
            "default path should contain 'sonde': {path}"
        );
        assert!(
            path.ends_with("pairing.json"),
            "default path should end with 'pairing.json': {path}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let mut store = store_in(&dir);
        store.save_artifacts(&test_artifacts()).unwrap();

        let perms = fs::metadata(store.path()).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }
}
