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
//!
//! # PSK encryption
//!
//! When a [`PskProtector`] is attached via [`FilePairingStore::with_protector`],
//! the `phone_psk` field is encrypted at rest using an OS-native backend:
//!
//! - **Windows:** DPAPI ([`crate::dpapi::DpapiPskProtector`], `dpapi` feature)
//! - **Linux:** Secret Service keyring
//!   ([`crate::secret_service_store::SecretServicePskProtector`],
//!   `secret-service-store` feature)
//!
//! Files written without a protector store `phone_psk` as plaintext hex and
//! are transparently read by stores *with* a protector (backward
//! compatibility).  On the next save the PSK will be re-encrypted.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::PairingError;
use crate::store::PairingStore;
use crate::types::{GatewayIdentity, PairingArtifacts};

// ---------------------------------------------------------------------------
// PskProtector trait
// ---------------------------------------------------------------------------

/// Encrypts and decrypts PSK material using OS-native key storage.
///
/// Platform-specific implementations:
/// - **Windows:** [`crate::dpapi::DpapiPskProtector`] — DPAPI
///   (enable the `dpapi` feature).
/// - **Linux:** [`crate::secret_service_store::SecretServicePskProtector`] —
///   D-Bus Secret Service keyring (enable the `secret-service-store` feature).
///
/// # Contract
///
/// [`protect`](Self::protect) returns opaque bytes that only the same backend
/// can [`unprotect`](Self::unprotect).  The bytes are stored as hex in the
/// JSON file.
pub trait PskProtector: Send + Sync {
    /// Encrypt a 32-byte PSK, returning opaque protected bytes.
    fn protect(&self, psk: &[u8; 32]) -> Result<Vec<u8>, PairingError>;

    /// Decrypt previously protected bytes back to a 32-byte PSK.
    fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<[u8; 32]>, PairingError>;

    /// Remove any externally stored protected material.
    ///
    /// Called by [`FilePairingStore::clear`] after deleting the JSON file.
    /// The default implementation is a no-op, suitable for backends that
    /// store protected data inline (e.g., DPAPI blobs in the JSON file).
    fn clear_protected(&self) -> Result<(), PairingError> {
        Ok(())
    }
}

/// Returns the platform-appropriate [`PskProtector`], if available.
///
/// - **Windows** (with `dpapi` feature): [`crate::dpapi::DpapiPskProtector`]
/// - **Linux** (with `secret-service-store` feature):
///   [`crate::secret_service_store::SecretServicePskProtector`]
/// - **Other**: `None`
pub fn default_protector() -> Option<Box<dyn PskProtector>> {
    #[cfg(all(windows, feature = "dpapi"))]
    {
        return Some(Box::new(crate::dpapi::DpapiPskProtector::new()));
    }
    #[cfg(all(target_os = "linux", feature = "secret-service-store"))]
    {
        return Some(Box::new(
            crate::secret_service_store::SecretServicePskProtector::default(),
        ));
    }
    #[allow(unreachable_code)]
    None
}

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
    /// Legacy plaintext hex — present only in pre-encryption files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phone_psk: Option<String>,
    /// Hex-encoded protected bytes — present when a [`PskProtector`] was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phone_psk_protected: Option<String>,
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
    protector: Option<Box<dyn PskProtector>>,
}

impl FilePairingStore {
    /// Create a store at the platform default location.
    pub fn new() -> Result<Self, PairingError> {
        Ok(Self {
            path: default_path()?,
            protector: None,
        })
    }

    /// Create a store at a caller-chosen path (useful for tests).
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            protector: None,
        }
    }

    /// Attach a [`PskProtector`] for encrypting `phone_psk` at rest.
    ///
    /// Use [`default_protector`] to obtain the platform-appropriate backend.
    pub fn with_protector(mut self, protector: Box<dyn PskProtector>) -> Self {
        self.protector = Some(protector);
        self
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
        // Clean up the temp file on any error so we never leave key material
        // (phone_psk) on disk in an orphaned file.
        let temp_path = self.path.with_extension("json.tmp");
        let result = self.write_temp_and_rename(&temp_path, json.as_bytes());
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    fn write_temp_and_rename(&self, temp_path: &Path, data: &[u8]) -> Result<(), PairingError> {
        // On Unix, create with restrictive permissions from the start so the
        // file is never world-readable, even briefly.
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(temp_path)
                .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?
        };
        #[cfg(not(unix))]
        let mut file = fs::File::create(temp_path)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;

        file.write_all(data)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        file.sync_all()
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        drop(file);

        fs::rename(temp_path, &self.path)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;

        Ok(())
    }

    fn artifacts_to_stored(&self, a: &PairingArtifacts) -> Result<StoredArtifacts, PairingError> {
        let (phone_psk, phone_psk_protected) = match &self.protector {
            Some(p) => {
                let protected = p.protect(&a.phone_psk)?;
                (None, Some(to_hex(&protected)))
            }
            None => (Some(to_hex(a.phone_psk.as_ref())), None),
        };
        Ok(StoredArtifacts {
            gateway_public_key: to_hex(&a.gateway_identity.public_key),
            gateway_id: to_hex(&a.gateway_identity.gateway_id),
            phone_psk,
            phone_psk_protected,
            phone_key_hint: a.phone_key_hint,
            rf_channel: a.rf_channel,
            phone_label: a.phone_label.clone(),
        })
    }

    fn artifacts_from_stored(&self, s: &StoredArtifacts) -> Result<PairingArtifacts, PairingError> {
        let pk = from_hex(&s.gateway_public_key, 32)?;
        let gid = from_hex(&s.gateway_id, 16)?;

        let psk = if let Some(ref protected_hex) = s.phone_psk_protected {
            let protector = self.protector.as_ref().ok_or_else(|| {
                PairingError::StoreLoadFailed(
                    "phone_psk is encrypted but no PskProtector is configured".into(),
                )
            })?;
            let protected = from_hex_var(protected_hex)?;
            protector.unprotect(&protected)?
        } else if let Some(ref psk_hex) = s.phone_psk {
            if self.protector.is_some() {
                tracing::warn!("phone_psk stored in plaintext — will be encrypted on next save");
            }
            let mut psk_vec = from_hex(psk_hex, 32)?;
            let mut psk = Zeroizing::new([0u8; 32]);
            psk.copy_from_slice(&psk_vec);
            psk_vec.zeroize();
            psk
        } else {
            return Err(PairingError::StoreCorrupted(
                "neither `phone_psk` nor `phone_psk_protected` is present".into(),
            ));
        };

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
}

impl PairingStore for FilePairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        let mut data = self.read_stored()?.unwrap_or(StoredData {
            gateway_identity: None,
            artifacts: None,
        });
        data.artifacts = Some(self.artifacts_to_stored(artifacts)?);
        self.write_stored(&data)
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        match self.read_stored()? {
            Some(StoredData {
                artifacts: Some(ref s),
                ..
            }) => Ok(Some(self.artifacts_from_stored(s)?)),
            _ => Ok(None),
        }
    }

    fn clear(&mut self) -> Result<(), PairingError> {
        if let Some(ref protector) = self.protector {
            if let Err(e) = protector.clear_protected() {
                tracing::warn!("failed to clear protected PSK: {e}");
            }
        }
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

/// Like [`from_hex`] but allows variable-length output (for protected blobs).
fn from_hex_var(hex: &str) -> Result<Vec<u8>, PairingError> {
    if !hex.len().is_multiple_of(2) {
        return Err(PairingError::StoreCorrupted(
            "protected data has odd hex length".into(),
        ));
    }
    if !hex.is_empty() && !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(PairingError::StoreCorrupted(
            "invalid hex character in protected data".into(),
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

// ---------------------------------------------------------------------------
// Default path
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn default_path() -> Result<PathBuf, PairingError> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| PairingError::StoreLoadFailed("%APPDATA% not set".into()))?;
    Ok(PathBuf::from(appdata).join("sonde").join("pairing.json"))
}

#[cfg(not(target_os = "windows"))]
fn default_path() -> Result<PathBuf, PairingError> {
    let home =
        std::env::var("HOME").map_err(|_| PairingError::StoreLoadFailed("$HOME not set".into()))?;
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

    // -- PskProtector tests ------------------------------------------------

    /// Deterministic mock: XOR with a fixed key.
    struct XorProtector([u8; 32]);

    impl PskProtector for XorProtector {
        fn protect(&self, psk: &[u8; 32]) -> Result<Vec<u8>, PairingError> {
            Ok(psk.iter().zip(self.0.iter()).map(|(a, b)| a ^ b).collect())
        }

        fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<[u8; 32]>, PairingError> {
            if protected.len() != 32 {
                return Err(PairingError::StoreCorrupted(format!(
                    "expected 32 protected bytes, got {}",
                    protected.len()
                )));
            }
            let mut psk = Zeroizing::new([0u8; 32]);
            for (i, b) in protected.iter().enumerate() {
                psk[i] = b ^ self.0[i];
            }
            Ok(psk)
        }
    }

    fn xor_key() -> [u8; 32] {
        [0xABu8; 32]
    }

    fn temp_store_with_protector() -> (FilePairingStore, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let store = FilePairingStore::with_path(dir.path().join("pairing.json"))
            .with_protector(Box::new(XorProtector(xor_key())));
        (store, dir)
    }

    #[test]
    fn save_and_load_with_protector() {
        let (mut store, _dir) = temp_store_with_protector();
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
    fn protector_encrypts_psk_in_json() {
        let (mut store, _dir) = temp_store_with_protector();
        let artifacts = test_artifacts();
        store.save_artifacts(&artifacts).unwrap();

        let json = fs::read_to_string(store.path()).unwrap();
        let data: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arts = &data["artifacts"];

        // The legacy plaintext field must NOT be present.
        assert!(
            arts.get("phone_psk").is_none(),
            "plaintext phone_psk field must not be present"
        );
        // The protected field MUST be present.
        assert!(
            arts.get("phone_psk_protected").is_some(),
            "phone_psk_protected field must be present"
        );
    }

    #[test]
    fn load_legacy_plaintext_without_protector() {
        // Write in old format (plaintext phone_psk), read back without protector.
        let (mut store, _dir) = temp_store();
        store.save_artifacts(&test_artifacts()).unwrap();

        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should load legacy plaintext");
        assert_eq!(*loaded.phone_psk, [0x42u8; 32]);
    }

    #[test]
    fn load_legacy_plaintext_with_protector() {
        // Write in old format, then read with a protector.
        // Backward compat: should still load the plaintext PSK.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");

        {
            let mut store = FilePairingStore::with_path(&path);
            store.save_artifacts(&test_artifacts()).unwrap();
        }

        let store =
            FilePairingStore::with_path(&path).with_protector(Box::new(XorProtector(xor_key())));
        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should load legacy plaintext even with protector");
        assert_eq!(*loaded.phone_psk, [0x42u8; 32]);
    }

    #[test]
    fn save_after_loading_legacy_encrypts() {
        // Write plaintext, attach protector, load, save → re-encrypted.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");

        {
            let mut store = FilePairingStore::with_path(&path);
            store.save_artifacts(&test_artifacts()).unwrap();
        }

        // Attach protector, load, and re-save.
        let mut store =
            FilePairingStore::with_path(&path).with_protector(Box::new(XorProtector(xor_key())));
        let artifacts = store.load_artifacts().unwrap().unwrap();
        store.save_artifacts(&artifacts).unwrap();

        // Verify the file uses protected format, not plaintext.
        let json = fs::read_to_string(store.path()).unwrap();
        let data: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arts = &data["artifacts"];
        assert!(
            arts.get("phone_psk").is_none(),
            "after re-save, plaintext phone_psk must not be present"
        );
        assert!(
            arts.get("phone_psk_protected").is_some(),
            "after re-save, phone_psk_protected must be present"
        );
    }

    #[test]
    fn encrypted_without_protector_fails() {
        // Write with protector, then try to read without one.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");

        {
            let mut store = FilePairingStore::with_path(&path)
                .with_protector(Box::new(XorProtector(xor_key())));
            store.save_artifacts(&test_artifacts()).unwrap();
        }

        let store = FilePairingStore::with_path(&path);
        let err = store.load_artifacts().unwrap_err();
        assert!(
            err.to_string().contains("no PskProtector"),
            "should report missing protector: {err}"
        );
    }

    #[test]
    fn neither_psk_field_returns_corrupted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        fs::create_dir_all(dir.path()).unwrap();
        // JSON with artifacts that have neither phone_psk nor phone_psk_protected.
        let json = r#"{
            "artifacts": {
                "gateway_public_key": "4242424242424242424242424242424242424242424242424242424242424242",
                "gateway_id": "01010101010101010101010101010101",
                "phone_key_hint": 4660,
                "rf_channel": 6,
                "phone_label": "test-phone"
            }
        }"#;
        fs::write(&path, json).unwrap();

        let store = FilePairingStore::with_path(&path);
        let err = store.load_artifacts().unwrap_err();
        assert!(
            err.to_string().contains("corrupted"),
            "should report corruption: {err}"
        );
    }

    #[test]
    fn gateway_identity_loads_from_encrypted_artifacts() {
        // Verify load_gateway_identity works even when phone_psk is encrypted.
        let (mut store, _dir) = temp_store_with_protector();
        store.save_artifacts(&test_artifacts()).unwrap();

        let loaded = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity from encrypted artifacts");
        assert_eq!(loaded.public_key, [0x42u8; 32]);
        assert_eq!(loaded.gateway_id, [0x01u8; 16]);
    }

    #[test]
    fn from_hex_var_round_trip() {
        let input = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01];
        let hex = to_hex(&input);
        let out = from_hex_var(&hex).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn from_hex_var_empty() {
        let out = from_hex_var("").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn from_hex_var_rejects_odd_length() {
        let err = from_hex_var("abc").unwrap_err();
        assert!(err.to_string().contains("odd hex length"));
    }
}
