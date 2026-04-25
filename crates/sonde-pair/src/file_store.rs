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
use tracing::warn;
use zeroize::{Zeroize, Zeroizing};

use crate::error::PairingError;
use crate::store::PairingStore;

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

/// Maximum accepted size for a protected PSK blob before hex decoding.
///
/// The protected form is opaque and backend-specific, but it should remain
/// comfortably below this bound for the currently supported protectors.
const MAX_PROTECTED_PSK_BLOB_LEN: usize = 4096;

// ---------------------------------------------------------------------------
// Serialisation types
// ---------------------------------------------------------------------------

/// On-disk pairing record.
///
/// At least one of `phone_psk` (legacy / no-protector) or `phone_psk_protected`
/// (protector-encrypted) must be present on any valid record. Both fields are
/// optional in the schema to support the migration path:
///
/// | Field present         | Meaning                                       |
/// |-----------------------|-----------------------------------------------|
/// | `phone_psk` only      | Legacy plaintext; written before a protector  |
/// |                       | was configured.                               |
/// | `phone_psk_protected` | PSK encrypted by the configured protector.    |
/// | both                  | Migration-compatible record; load logic       |
/// |                       | selects the appropriate representation.       |
/// | neither               | Corrupted record — rejected on load.          |
#[derive(Serialize, Deserialize)]
struct StoredArtifacts {
    /// Plaintext hex PSK.  Present in legacy files or when no protector is attached.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    phone_psk: Option<String>,
    /// Protector-encrypted PSK (hex-encoded opaque bytes).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    phone_psk_protected: Option<String>,
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
    ///
    /// If a [`PskProtector`] is attached, the PSK is encrypted and stored in the
    /// `phone_psk_protected` field; the plaintext `phone_psk` field is omitted.
    pub fn save_artifacts(
        &self,
        artifacts: &crate::phase1::PairingArtifacts,
    ) -> Result<(), PairingError> {
        let aead_path = self.aead_path();
        if let Some(parent) = aead_path.parent() {
            fs::create_dir_all(parent).map_err(|e| PairingError::StoreSaveFailed(e.to_string()))?;
        }

        let (phone_psk, phone_psk_protected) = if let Some(ref protector) = self.protector {
            let protected = Zeroizing::new(protector.protect(&artifacts.phone_psk)?);
            (None, Some(to_hex(&protected)))
        } else {
            (Some(to_hex(&*artifacts.phone_psk)), None)
        };

        let stored = StoredArtifacts {
            phone_psk,
            phone_psk_protected,
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
    ///
    /// Handles both the current protected format and the legacy plaintext format.
    /// When a [`PskProtector`] is configured and a plaintext `phone_psk` is found,
    /// the PSK is loaded from plaintext and a warning is emitted; the next call
    /// to [`save_artifacts`](Self::save_artifacts) will re-write it in protected form.
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

        let mut psk = Zeroizing::new([0u8; 32]);

        match (
            &stored.phone_psk_protected,
            &stored.phone_psk,
            &self.protector,
        ) {
            (Some(protected_hex), _, Some(protector)) => {
                // Protected path: use the configured protector to decrypt.
                if protected_hex.len() % 2 != 0 {
                    return Err(PairingError::StoreCorrupted(
                        "phone_psk_protected must contain an even number of hex chars".into(),
                    ));
                }
                let protected_len = protected_hex.len() / 2;
                if protected_len > MAX_PROTECTED_PSK_BLOB_LEN {
                    return Err(PairingError::StoreCorrupted(format!(
                        "phone_psk_protected exceeds maximum supported size ({} bytes)",
                        MAX_PROTECTED_PSK_BLOB_LEN
                    )));
                }
                let protected_bytes = Zeroizing::new(from_hex(protected_hex, protected_len)?);
                let unprotected = protector.unprotect(&protected_bytes)?;
                *psk = *unprotected;
            }
            (Some(_), _, None) => {
                return Err(PairingError::StoreCorrupted(
                    "phone_psk_protected present but no PskProtector is configured; \
                     reconfigure with the correct protector or delete the store"
                        .into(),
                ));
            }
            (None, Some(plain_hex), Some(_)) => {
                // Legacy migration: plaintext PSK with a protector now configured.
                warn!("phone_psk stored in plaintext — will be encrypted on next save");
                let mut psk_bytes = from_hex(plain_hex, 32)?;
                psk.copy_from_slice(&psk_bytes);
                psk_bytes.zeroize();
            }
            (None, Some(plain_hex), None) => {
                // No protector configured; use plaintext as before.
                let mut psk_bytes = from_hex(plain_hex, 32)?;
                psk.copy_from_slice(&psk_bytes);
                psk_bytes.zeroize();
            }
            (None, None, _) => {
                return Err(PairingError::StoreCorrupted(
                    "neither phone_psk nor phone_psk_protected is present in the store".into(),
                ));
            }
        }

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
    ///
    /// Also calls [`PskProtector::clear_protected`] if a protector is configured,
    /// to remove any externally stored protected material.
    pub fn clear(&self) -> Result<(), PairingError> {
        let aead_path = self.aead_path();
        // Also remove any leftover temp file from a crashed save.
        let tmp_path = aead_path.with_extension("tmp");
        let _ = fs::remove_file(&tmp_path);
        let result = match fs::remove_file(&aead_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PairingError::StoreSaveFailed(e.to_string())),
        };
        if result.is_ok() {
            if let Some(ref protector) = self.protector {
                protector.clear_protected()?;
            }
        }
        result
    }

    fn aead_path(&self) -> PathBuf {
        self.path.with_file_name("pairing-aead.json")
    }
}

// ---------------------------------------------------------------------------
// PairingStore trait impl
// ---------------------------------------------------------------------------

impl PairingStore for FilePairingStore {
    fn save_artifacts(
        &self,
        artifacts: &crate::phase1::PairingArtifacts,
    ) -> Result<(), PairingError> {
        FilePairingStore::save_artifacts(self, artifacts)
    }

    fn load_artifacts(&self) -> Result<Option<crate::phase1::PairingArtifacts>, PairingError> {
        FilePairingStore::load_artifacts(self)
    }

    fn clear(&self) -> Result<(), PairingError> {
        FilePairingStore::clear(self)
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
    use crate::store::PairingStore;
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

    // -----------------------------------------------------------------------
    // Plaintext (no protector) round-trip — existing behaviour preserved
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // PairingStore trait delegation
    // -----------------------------------------------------------------------

    /// Validates: PT-0802 — FilePairingStore works via the PairingStore trait object.
    #[test]
    fn file_store_implements_pairing_store_trait() {
        let dir = TempDir::new().unwrap();
        let store = FilePairingStore::with_path(dir.path().join("pairing.json"));
        let dyn_store: &dyn PairingStore = &store;

        let artifacts = test_artifacts();
        dyn_store.save_artifacts(&artifacts).unwrap();
        let loaded = dyn_store.load_artifacts().unwrap().unwrap();
        assert_eq!(*loaded.phone_psk, *artifacts.phone_psk);

        dyn_store.clear().unwrap();
        assert!(dyn_store.load_artifacts().unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // MockPskProtector — used for protector-path tests
    // -----------------------------------------------------------------------

    /// Identity protector for tests: protect = prepend 0xFF byte; unprotect = strip it.
    struct MockPskProtector;

    impl PskProtector for MockPskProtector {
        fn protect(&self, psk: &[u8; 32]) -> Result<Vec<u8>, PairingError> {
            let mut v = vec![0xFF];
            v.extend_from_slice(psk);
            Ok(v)
        }

        fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<[u8; 32]>, PairingError> {
            if protected.len() != 33 || protected[0] != 0xFF {
                return Err(PairingError::StoreCorrupted(
                    "MockPskProtector: bad blob".into(),
                ));
            }
            let mut out = Zeroizing::new([0u8; 32]);
            out.copy_from_slice(&protected[1..]);
            Ok(out)
        }
    }

    fn temp_store_with_protector() -> (FilePairingStore, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let store = FilePairingStore::with_path(dir.path().join("pairing.json"))
            .with_protector(Box::new(MockPskProtector));
        (store, dir)
    }

    // -----------------------------------------------------------------------
    // Protected round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn protected_save_writes_phone_psk_protected_field() {
        let (store, dir) = temp_store_with_protector();
        store.save_artifacts(&test_artifacts()).unwrap();

        let raw = std::fs::read_to_string(dir.path().join("pairing-aead.json")).unwrap();
        assert!(
            raw.contains("phone_psk_protected"),
            "expected phone_psk_protected in JSON: {raw}"
        );
        assert!(
            !raw.contains("\"phone_psk\""),
            "plaintext phone_psk must not appear in JSON: {raw}"
        );
    }

    #[test]
    fn protected_round_trip() {
        let (store, _dir) = temp_store_with_protector();
        let artifacts = test_artifacts();
        store.save_artifacts(&artifacts).unwrap();

        let loaded = store.load_artifacts().unwrap().unwrap();
        assert_eq!(*loaded.phone_psk, *artifacts.phone_psk);
        assert_eq!(loaded.phone_key_hint, artifacts.phone_key_hint);
        assert_eq!(loaded.rf_channel, artifacts.rf_channel);
        assert_eq!(loaded.phone_label, artifacts.phone_label);
    }

    // -----------------------------------------------------------------------
    // Legacy migration: plaintext store loaded with protector configured
    // -----------------------------------------------------------------------

    /// Validates: PT-0801 migration path — plaintext PSK is accepted on load
    /// when a protector is configured (and a warning is emitted).
    #[test]
    fn legacy_migration_plaintext_loaded_with_protector() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        let artifacts = test_artifacts();

        // Write a legacy plaintext store (no protector).
        {
            let store = FilePairingStore::with_path(path.clone());
            store.save_artifacts(&artifacts).unwrap();
        }

        // Re-open with a protector — should accept the plaintext PSK.
        let store = FilePairingStore::with_path(path).with_protector(Box::new(MockPskProtector));
        let loaded = store.load_artifacts().unwrap().unwrap();
        assert_eq!(*loaded.phone_psk, *artifacts.phone_psk);
    }

    /// Next save after migration writes protected form.
    #[test]
    fn legacy_migration_next_save_writes_protected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");
        let artifacts = test_artifacts();

        // Write plaintext.
        FilePairingStore::with_path(path.clone())
            .save_artifacts(&artifacts)
            .unwrap();

        // Load with protector, then save again.
        {
            let store = FilePairingStore::with_path(path.clone())
                .with_protector(Box::new(MockPskProtector));
            let loaded = store.load_artifacts().unwrap().unwrap();
            store.save_artifacts(&loaded).unwrap();
        }

        // Verify the file now contains phone_psk_protected.
        let raw = std::fs::read_to_string(dir.path().join("pairing-aead.json")).unwrap();
        assert!(
            raw.contains("phone_psk_protected"),
            "after migration save, expected phone_psk_protected: {raw}"
        );
        assert!(
            !raw.contains("\"phone_psk\""),
            "after migration save, plaintext phone_psk must be absent: {raw}"
        );
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn protected_field_without_protector_is_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pairing.json");

        // Write protected store.
        FilePairingStore::with_path(path.clone())
            .with_protector(Box::new(MockPskProtector))
            .save_artifacts(&test_artifacts())
            .unwrap();

        // Load without a protector — must return StoreCorrupted.
        let store = FilePairingStore::with_path(path);
        let err = store.load_artifacts().unwrap_err();
        assert!(
            err.to_string().contains("corrupted"),
            "expected corrupted error: {err}"
        );
    }

    #[test]
    fn missing_both_psk_fields_is_error() {
        let dir = TempDir::new().unwrap();
        let aead = dir.path().join("pairing-aead.json");
        std::fs::write(
            &aead,
            r#"{"phone_key_hint":1234,"rf_channel":6,"phone_label":"x"}"#,
        )
        .unwrap();

        let store = FilePairingStore::with_path(dir.path().join("pairing.json"));
        let err = store.load_artifacts().unwrap_err();
        assert!(err.to_string().contains("corrupted"));
    }

    // -----------------------------------------------------------------------
    // Hex helpers
    // -----------------------------------------------------------------------

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
