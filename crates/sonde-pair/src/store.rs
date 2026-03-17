// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use crate::types::{GatewayIdentity, PairingArtifacts};

/// Persistent storage for pairing artifacts.
pub trait PairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError>;
    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError>;
    fn clear(&mut self) -> Result<(), PairingError>;
    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError>;
    /// Persist the gateway identity independently of full artifacts.
    ///
    /// Called immediately after TOFU signature verification so the pinned
    /// identity survives even if later protocol steps (e.g. registration) fail.
    fn save_gateway_identity(&mut self, identity: &GatewayIdentity) -> Result<(), PairingError>;
}

/// Check whether a gateway identity is already stored (PT-0601).
///
/// Returns `Some(identity)` if the store already has a pinned gateway,
/// indicating that Phase 1 has been run before.  The caller (typically the
/// UI layer) should warn the operator before re-pairing.
pub fn is_already_paired(
    store: &dyn PairingStore,
) -> Result<Option<GatewayIdentity>, PairingError> {
    store.load_gateway_identity()
}

/// In-memory pairing store for testing.
pub struct MemoryPairingStore {
    artifacts: Option<PairingArtifacts>,
    gateway_identity: Option<GatewayIdentity>,
}

impl MemoryPairingStore {
    pub fn new() -> Self {
        Self {
            artifacts: None,
            gateway_identity: None,
        }
    }
}

impl Default for MemoryPairingStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingStore for MemoryPairingStore {
    fn save_artifacts(&mut self, artifacts: &PairingArtifacts) -> Result<(), PairingError> {
        self.artifacts = Some(artifacts.clone());
        Ok(())
    }

    fn load_artifacts(&self) -> Result<Option<PairingArtifacts>, PairingError> {
        Ok(self.artifacts.clone())
    }

    fn clear(&mut self) -> Result<(), PairingError> {
        self.artifacts = None;
        self.gateway_identity = None;
        Ok(())
    }

    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError> {
        if let Some(ref id) = self.gateway_identity {
            return Ok(Some(id.clone()));
        }
        Ok(self.artifacts.as_ref().map(|a| a.gateway_identity.clone()))
    }

    fn save_gateway_identity(&mut self, identity: &GatewayIdentity) -> Result<(), PairingError> {
        self.gateway_identity = Some(identity.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

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

    #[test]
    fn save_and_load_round_trip() {
        let mut store = MemoryPairingStore::new();
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
    fn load_empty_returns_none() {
        let store = MemoryPairingStore::new();
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn clear_removes_artifacts() {
        let mut store = MemoryPairingStore::new();
        store.save_artifacts(&test_artifacts()).unwrap();
        store.clear().unwrap();
        assert!(store.load_artifacts().unwrap().is_none());
        assert!(store.load_gateway_identity().unwrap().is_none());
    }

    #[test]
    fn load_gateway_identity() {
        let mut store = MemoryPairingStore::new();
        assert!(store.load_gateway_identity().unwrap().is_none());

        store.save_artifacts(&test_artifacts()).unwrap();
        let identity = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity");
        assert_eq!(identity.public_key, [0x42u8; 32]);
        assert_eq!(identity.gateway_id, [0x01u8; 16]);
    }

    #[test]
    fn save_gateway_identity_standalone() {
        let mut store = MemoryPairingStore::new();
        let identity = GatewayIdentity {
            public_key: [0x42u8; 32],
            gateway_id: [0x01u8; 16],
        };
        store.save_gateway_identity(&identity).unwrap();

        // Identity available even without full artifacts
        let loaded = store
            .load_gateway_identity()
            .unwrap()
            .expect("should have identity");
        assert_eq!(loaded, identity);
        assert!(store.load_artifacts().unwrap().is_none());
    }

    #[test]
    fn save_gateway_identity_takes_precedence() {
        let mut store = MemoryPairingStore::new();
        store.save_artifacts(&test_artifacts()).unwrap();

        // Save a different standalone identity
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
}
