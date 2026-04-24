// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! `PairingStore` trait and [`MockPairingStore`] in-memory implementation.
//!
//! # Trait
//!
//! All platform-specific storage backends implement [`PairingStore`] (PT-0802):
//!
//! - [`crate::file_store::FilePairingStore`] — JSON file (desktop, `file-store` feature).
//! - [`crate::android_store::AndroidPairingStore`] — `EncryptedSharedPreferences` via JNI
//!   (`android` feature).
//! - [`MockPairingStore`] — in-memory implementation for tests.
//!
//! # Testing
//!
//! [`MockPairingStore`] is an in-memory implementation backed by a `Mutex`.  It can be
//! pre-loaded with test data and optionally configured to inject a corruption error on
//! `load_artifacts()`.

use std::sync::{Mutex, MutexGuard};

use crate::error::PairingError;
use crate::phase1::PairingArtifacts;

// ---------------------------------------------------------------------------
// PairingStore trait
// ---------------------------------------------------------------------------

/// Abstraction over persistent pairing-artifact storage (PT-0802).
///
/// Every method takes `&self` — implementations that need internal mutation
/// (e.g. `Mutex`-protected state) do so internally.
pub trait PairingStore: Send + Sync {
    /// Persist pairing artifacts, overwriting any previously stored value.
    fn save_artifacts(&self, artifacts: &PairingArtifacts) -> Result<(), PairingError>;

    /// Load previously persisted artifacts.
    ///
    /// Returns `Ok(None)` if nothing has been saved yet.
    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError>;

    /// Erase all persisted artifacts.
    ///
    /// Idempotent — returns `Ok(())` even if nothing was stored.
    fn clear(&self) -> Result<(), PairingError>;
}

// ---------------------------------------------------------------------------
// MockPairingStore
// ---------------------------------------------------------------------------

struct MockInner {
    artifacts: Option<PairingArtifacts>,
    /// When `Some`, the next `load_artifacts` call returns this error.
    load_error: Option<PairingError>,
}

/// In-memory [`PairingStore`] for tests (PT-0802).
///
/// Thread-safe via an internal `Mutex`.  Can be pre-loaded with artifacts
/// and optionally configured to simulate corruption on the next load.
pub struct MockPairingStore {
    inner: Mutex<MockInner>,
}

impl Default for MockPairingStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPairingStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MockInner {
                artifacts: None,
                load_error: None,
            }),
        }
    }

    /// Create a store pre-loaded with `artifacts`.
    pub fn with_artifacts(artifacts: PairingArtifacts) -> Self {
        Self {
            inner: Mutex::new(MockInner {
                artifacts: Some(artifacts),
                load_error: None,
            }),
        }
    }

    /// Configure the store to return `error` on the **next** `load_artifacts` call.
    ///
    /// The injected error is consumed once; subsequent calls return the stored value.
    pub fn set_load_error(&self, error: PairingError) {
        self
            .lock_inner()
            .expect("MockPairingStore mutex poisoned during test setup")
            .load_error = Some(error);
    }

    fn lock_inner(&self) -> Result<MutexGuard<'_, MockInner>, PairingError> {
        self.inner
            .lock()
            .map_err(|_| PairingError::StoreCorrupted("MockPairingStore mutex poisoned".into()))
    }
}

impl PairingStore for MockPairingStore {
    fn save_artifacts(&self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        self.lock_inner()?.artifacts = Some(artifacts.clone());
        Ok(())
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        let mut inner = self.lock_inner()?;
        if let Some(err) = inner.load_error.take() {
            return Err(err);
        }
        Ok(inner.artifacts.clone())
    }

    fn clear(&self) -> Result<(), PairingError> {
        self.lock_inner()?.artifacts = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase1::PairingArtifacts;
    use crate::validation::compute_key_hint;
    use zeroize::Zeroizing;

    fn test_artifacts() -> PairingArtifacts {
        let psk = [0x42u8; 32];
        PairingArtifacts {
            phone_psk: Zeroizing::new(psk),
            phone_key_hint: compute_key_hint(&psk),
            rf_channel: 6,
            phone_label: "test-phone".into(),
        }
    }

    /// Validates: PT-0802 (T-PT-601) — MockPairingStore implements PairingStore trait.
    ///
    /// Accepts a `&dyn PairingStore` to confirm trait-object dispatch works.
    fn exercise_store(store: &dyn PairingStore) {
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

        store.clear().unwrap();
        assert!(store.load_artifacts().unwrap().is_none());
    }

    /// Validates: PT-0802 — mock store works via `PairingStore` trait object.
    #[test]
    fn t_pt_601_mock_store_implements_pairing_store_trait() {
        let store = MockPairingStore::new();
        exercise_store(&store);
    }

    #[test]
    fn mock_store_load_missing_returns_none() {
        let store = MockPairingStore::new();
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn mock_store_clear_missing_is_ok() {
        let store = MockPairingStore::new();
        store.clear().unwrap();
    }

    #[test]
    fn mock_store_with_artifacts_preloaded() {
        let artifacts = test_artifacts();
        let store = MockPairingStore::with_artifacts(artifacts.clone());
        let loaded = store.load_artifacts().unwrap().unwrap();
        assert_eq!(*loaded.phone_psk, *artifacts.phone_psk);
    }

    #[test]
    fn mock_store_load_error_injection() {
        let store = MockPairingStore::new();
        store.set_load_error(PairingError::StoreCorrupted("injected".into()));

        let err = store.load_artifacts().unwrap_err();
        assert!(err.to_string().contains("corrupted"));

        // Error consumed — subsequent load returns None (empty store).
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn mock_store_overwrite() {
        let store = MockPairingStore::new();
        let artifacts = test_artifacts();
        store.save_artifacts(&artifacts).unwrap();

        let mut updated = artifacts.clone();
        updated.rf_channel = 11;
        store.save_artifacts(&updated).unwrap();

        let loaded = store.load_artifacts().unwrap().unwrap();
        assert_eq!(loaded.rf_channel, 11);
    }
}
