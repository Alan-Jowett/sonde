// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod cbor;
pub mod crypto;
pub mod discovery;
#[cfg(all(windows, feature = "dpapi"))]
pub mod dpapi;
pub mod envelope;
pub mod error;
#[cfg(feature = "file-store")]
pub mod file_store;
pub mod fragmentation;
pub mod phase1;
pub mod phase2;
pub mod rng;
#[cfg(all(target_os = "linux", feature = "secret-service-store"))]
pub mod secret_service_store;
pub mod store;
pub mod transport;
pub mod types;
pub mod validation;

#[cfg(feature = "btleplug")]
pub mod btleplug_transport;

#[cfg(feature = "loopback-ble")]
pub mod loopback_transport;

#[cfg(feature = "android")]
pub mod android_store;
#[cfg(feature = "android")]
pub mod android_transport;

/// Validates: PT-1004 (T-PT-1004)
///
/// The core crate must compile and be usable without any platform features.
/// This test exercises the core types and functions that must remain
/// feature-independent to support Tauri UI, headless CLI, and mock-transport
/// integration tests.
#[cfg(test)]
mod core_feature_independence_tests {
    use crate::crypto;
    use crate::envelope::{build_envelope, parse_envelope};
    use crate::error::PairingError;
    use crate::rng::{MockRng, RngProvider};
    use crate::transport::MockBleTransport;
    use crate::types::*;
    use crate::validation::{compute_key_hint, validate_node_id, validate_rf_channel};

    /// Validates: PT-1004
    ///
    /// The core pairing types, transport trait, envelope codec,
    /// crypto, validation, and mock implementations must all be usable
    /// without enabling any platform feature flags.
    #[test]
    fn t_pt_1004_core_types_available_without_features() {
        // Types
        let _device = ScannedDevice {
            name: "test".into(),
            address: [0x01; 6],
            rssi: -50,
            service_uuids: vec![GATEWAY_SERVICE_UUID],
        };

        // Transport trait (MockBleTransport)
        let _transport = MockBleTransport::new(247);

        // Envelope codec
        let envelope = build_envelope(0x01, &[0xAA, 0xBB]).unwrap();
        let (msg_type, payload) = parse_envelope(&envelope).unwrap();
        assert_eq!(msg_type, 0x01);
        assert_eq!(payload, &[0xAA, 0xBB]);

        // Validation
        validate_node_id("sensor-1").unwrap();
        validate_rf_channel(6).unwrap();
        let _hint = compute_key_hint(&[0x42u8; 32]);

        // RNG (MockRng)
        let rng = MockRng::new([0x42u8; 32]);
        let mut buf = [0u8; 32];
        rng.fill_bytes(&mut buf).unwrap();

        // Crypto
        let hash = crypto::sha256(b"test");
        assert_eq!(hash.len(), 32);

        // Error types
        let _err = PairingError::DecryptionFailed;
    }
}
