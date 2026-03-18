// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE loopback integration tests.
//!
//! These tests exercise the BLE pairing flow through the
//! [`LoopbackBleTransport`] and [`fake_peripheral`] — no Bluetooth
//! hardware required.
//!
//! # What these tests catch
//!
//! - BleTransport initialization crashes
//! - BLE envelope encoding/decoding mismatches between `sonde-pair` and `sonde-gateway`
//! - Phase 1 flow issues through the real transport layer
//! - Cryptographic handshake regressions (ECDH, HKDF, AES-GCM)
//! - TOFU identity pinning via the store

use sonde_e2e::fake_peripheral::{self, FakePeripheralConfig};
use sonde_pair::loopback_transport::LoopbackBleTransport;
use sonde_pair::phase1;
use sonde_pair::rng::OsRng;
use sonde_pair::store::{MemoryPairingStore, PairingStore};
use sonde_pair::transport::BleTransport;

/// Start a fake peripheral and return (transport, peripheral) pair.
async fn setup() -> (LoopbackBleTransport, fake_peripheral::FakePeripheral) {
    let config = FakePeripheralConfig {
        bind_addr: "127.0.0.1:0".into(),
        ..Default::default()
    };
    let peripheral = fake_peripheral::start(config)
        .await
        .expect("failed to start fake GATT peripheral");

    let transport = LoopbackBleTransport::new(&peripheral.addr().to_string());
    (transport, peripheral)
}

/// Phase 1 happy path: scan → connect → pair with gateway → verify artifacts.
#[tokio::test]
async fn phase1_loopback_happy_path() {
    let (mut transport, peripheral) = setup().await;
    let mut store = MemoryPairingStore::new();
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    // Scan returns a fake device
    let devices = transport.get_discovered_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Sonde-GW-Loopback");

    // Phase 1: pair with the fake gateway
    let artifacts = phase1::pair_with_gateway(
        &mut transport,
        &mut store,
        &rng,
        &device_addr,
        "integration-test-phone",
    )
    .await
    .expect("Phase 1 pairing should succeed");

    // Verify artifacts
    assert_eq!(artifacts.rf_channel, 6);
    assert_eq!(artifacts.phone_label, "integration-test-phone");
    assert_eq!(artifacts.gateway_identity.public_key.len(), 32);
    assert_eq!(artifacts.gateway_identity.gateway_id.len(), 16);

    // PSK should not be all-zeros
    assert_ne!(&*artifacts.phone_psk, &[0u8; 32]);

    // Verify key hint matches PSK
    let expected_hint = sonde_pair::validation::compute_key_hint(&artifacts.phone_psk);
    assert_eq!(artifacts.phone_key_hint, expected_hint);

    // Artifacts should be persisted in the store
    let loaded = store
        .load_artifacts()
        .unwrap()
        .expect("artifacts should be saved");
    assert_eq!(
        loaded.gateway_identity.public_key,
        artifacts.gateway_identity.public_key
    );
    assert_eq!(loaded.rf_channel, artifacts.rf_channel);

    peripheral.cancel();
}

/// Phase 1 with re-pairing: running Phase 1 twice should succeed and
/// overwrite the previous artifacts cleanly (PT-0600 / PT-0601).
#[tokio::test]
async fn phase1_loopback_re_pair() {
    let (mut transport, peripheral) = setup().await;
    let mut store = MemoryPairingStore::new();
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    // First pairing
    let first = phase1::pair_with_gateway(
        &mut transport,
        &mut store,
        &rng,
        &device_addr,
        "first-phone",
    )
    .await
    .expect("first pairing should succeed");

    // Second pairing — same gateway, new label
    let second = phase1::pair_with_gateway(
        &mut transport,
        &mut store,
        &rng,
        &device_addr,
        "second-phone",
    )
    .await
    .expect("re-pairing should succeed");

    // Gateway identity should be the same (same fake peripheral)
    assert_eq!(
        first.gateway_identity.public_key,
        second.gateway_identity.public_key
    );

    // But artifacts should differ (new PSK issued each time)
    assert_ne!(*first.phone_psk, *second.phone_psk);
    assert_eq!(second.phone_label, "second-phone");

    // Store should have the latest artifacts
    let loaded = store.load_artifacts().unwrap().unwrap();
    assert_eq!(loaded.phone_label, "second-phone");

    peripheral.cancel();
}

/// Phase 1 with empty phone label (allowed by spec: 0–64 bytes).
#[tokio::test]
async fn phase1_loopback_empty_label() {
    let (mut transport, peripheral) = setup().await;
    let mut store = MemoryPairingStore::new();
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    let artifacts = phase1::pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "")
        .await
        .expect("pairing with empty label should succeed");

    assert_eq!(artifacts.phone_label, "");

    peripheral.cancel();
}

/// TOFU violation: if the store already has a different gateway identity,
/// Phase 1 should fail with `PublicKeyMismatch`.
#[tokio::test]
async fn phase1_loopback_tofu_violation() {
    let (mut transport, peripheral) = setup().await;
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    // Pre-seed the store with a different gateway identity
    let mut store = MemoryPairingStore::new();
    let fake_identity = sonde_pair::types::GatewayIdentity {
        public_key: [0x99u8; 32], // different from the real peripheral's key
        gateway_id: [0x01u8; 16],
    };
    store.save_gateway_identity(&fake_identity).unwrap();

    let result =
        phase1::pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "test").await;

    assert!(
        matches!(
            result,
            Err(sonde_pair::error::PairingError::PublicKeyMismatch)
        ),
        "expected PublicKeyMismatch, got {result:?}"
    );

    peripheral.cancel();
}
