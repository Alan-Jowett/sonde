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
}

/// In-memory pairing store for testing.
pub struct MemoryPairingStore {
    artifacts: Option<PairingArtifacts>,
}

impl MemoryPairingStore {
    pub fn new() -> Self {
        Self { artifacts: None }
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
        Ok(())
    }

    fn load_gateway_identity(&self) -> Result<Option<GatewayIdentity>, PairingError> {
        Ok(self.artifacts.as_ref().map(|a| a.gateway_identity.clone()))
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
}
