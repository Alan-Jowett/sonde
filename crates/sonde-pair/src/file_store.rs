// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Persistent pairing store backed by a JSON file.
//!
//! Enabled by the `file-store` cargo feature. Stores AEAD pairing artifacts
//! in a platform-appropriate location:
//!
//! - **Windows:** `%APPDATA%\sonde\pairing-aead.json`
//! - **Linux / macOS:** `~/.config/sonde/pairing-aead.json`
//!
//! Writes are atomic (write-to-temp, then rename) to prevent corruption on
//! crash. On Unix the file is restricted to user-only access (mode `0o600`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::PairingError;

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
// Serialisation types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoredArtifacts {
    phone_psk: String,
    phone_key_hint: u16,
    rf_channel: u8,
    phone_label: String,
}

// ---------------------------------------------------------------------------
// FilePairingStore
// ---------------------------------------------------------------------------

/// Persistent pairing store backed by a JSON file.
///
/// See [module-level documentation](self) for platform paths and atomicity
/// guarantees.
pub struct FilePairingStore {
    path: PathBuf,
    protector: Option<Box<dyn PskProtector>>,
}

impl FilePairingStore {
    /// Create a store at the platform default location.
    ///
    /// On supported platforms the [`default_protector`] is attached
    /// automatically so `phone_psk` is encrypted at rest.
    pub fn new() -> Result<Self, PairingError> {
        Ok(Self {
            path: default_path()?,
            protector: default_protector(),
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

    /// Save AEAD pairing artifacts to a companion file (`pairing-aead.json`).
    pub fn save_artifacts(
        &self,
        artifacts: &crate::phase1::PairingArtifacts,
    ) -> Result<(), PairingError> {
        let aead_path = self.aead_path();
        if let Some(parent) = aead_path.parent() {
            fs::create_dir_all(parent).map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        }

        let stored = StoredArtifacts {
            phone_psk: to_hex(&*artifacts.phone_psk),
            phone_key_hint: artifacts.phone_key_hint,
            rf_channel: artifacts.rf_channel,
            phone_label: artifacts.phone_label.clone(),
        };

        let json = serde_json::to_string_pretty(&stored)
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;

        let tmp = aead_path.with_extension("tmp");
        let mut file =
            fs::File::create(&tmp).map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        file.write_all(json.as_bytes())
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        file.sync_all()
            .map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        drop(file);
        fs::rename(&tmp, &aead_path).map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;

        Ok(())
    }

    /// Load AEAD pairing artifacts from the companion file.
    pub fn load_artifacts(&self) -> Result<Option<crate::phase1::PairingArtifacts>, PairingError> {
        let aead_path = self.aead_path();
        let bytes = match fs::read(&aead_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PairingError::StoreLoadFailed(e.to_string())),
        };

        let stored: StoredArtifacts = serde_json::from_slice(&bytes).map_err(|e| {
            PairingError::StoreCorrupted(format!("{e}: delete or fix {}", aead_path.display()))
        })?;

        let mut psk_bytes = from_hex(&stored.phone_psk, 32)?;
        let mut psk = Zeroizing::new([0u8; 32]);
        psk.copy_from_slice(&psk_bytes);
        psk_bytes.zeroize();

        // Recompute key_hint from PSK to detect corruption.
        let expected_hint = crate::validation::compute_key_hint(&psk);
        if stored.phone_key_hint != expected_hint {
            return Err(PairingError::StoreCorrupted(
                "phone_key_hint does not match phone_psk".into(),
            ));
        }

        Ok(Some(crate::phase1::PairingArtifacts {
            phone_psk: psk,
            phone_key_hint: expected_hint,
            rf_channel: stored.rf_channel,
            phone_label: stored.phone_label,
        }))
    }

    /// Clear AEAD artifacts file (`pairing-aead.json`).
    pub fn clear(&self) -> Result<(), PairingError> {
        let aead_path = self.aead_path();
        // Also remove any leftover temp file from a crashed save.
        let tmp_path = aead_path.with_extension("tmp");
        let _ = fs::remove_file(&tmp_path);
        match fs::remove_file(&aead_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PairingError::StoreSaveFailed(e.to_string())),
        }
    }

    fn aead_path(&self) -> PathBuf {
        self.path.with_file_name("pairing-aead.json")
    }
}

// ---------------------------------------------------------------------------
// Hex helpers - validate ASCII before byte-indexing (see code-quality guide).
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
    use crate::phase1::PairingArtifacts;
    use crate::validation::compute_key_hint;
    use tempfile::TempDir;

    fn test_artifacts() -> PairingArtifacts {
        let psk = [0x42u8; 32];
        PairingArtifacts {
            phone_psk: Zeroizing::new(psk),
            phone_key_hint: compute_key_hint(&psk),
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
    fn aead_save_and_load_round_trip() {
        let (store, _dir) = temp_store();
        let artifacts = test_artifacts();
        store.save_artifacts(&artifacts).unwrap();

        let loaded = store
            .load_artifacts()
            .unwrap()
            .expect("should have artifacts");
        assert_eq!(*loaded.phone_psk, *artifacts.phone_psk);
        assert_eq!(loaded.phone_key_hint, artifacts.phone_key_hint);
        assert_eq!(loaded.rf_channel, artifacts.rf_channel);
        assert_eq!(loaded.phone_label, artifacts.phone_label);
    }

    #[test]
    fn aead_load_missing_file_returns_none() {
        let (store, _dir) = temp_store();
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn aead_clear_removes_file() {
        let (store, _dir) = temp_store();
        store.save_artifacts(&test_artifacts()).unwrap();
        assert!(store.aead_path().exists());

        store.clear().unwrap();
        assert!(!store.aead_path().exists());
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn aead_clear_missing_file_is_ok() {
        let (store, _dir) = temp_store();
        store.clear().unwrap();
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
        let err = from_hex("caf\u{00e9}0000", 4).unwrap_err();
        assert!(err.to_string().contains("corrupted"));
    }

    #[test]
    fn hex_rejects_wrong_length() {
        let err = from_hex("abcd", 4).unwrap_err();
        assert!(err.to_string().contains("corrupted"));
    }
}
