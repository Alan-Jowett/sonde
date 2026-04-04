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
//! - Phase 1 AEAD flow through the real transport layer
//! - PSK registration and artifact construction

use sonde_e2e::fake_peripheral::{self, FakePeripheralConfig};
use sonde_pair::loopback_transport::LoopbackBleTransport;
use sonde_pair::phase1;
use sonde_pair::rng::OsRng;
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

/// Phase 1 AEAD happy path: scan → connect → register phone → verify artifacts.
#[tokio::test]
async fn phase1_loopback_happy_path() {
    let (mut transport, peripheral) = setup().await;
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    // Scan returns a fake device
    let devices = transport.get_discovered_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Sonde-GW-Loopback");

    // Phase 1: pair with the fake gateway (AEAD)
    let artifacts = phase1::pair_with_gateway(
        &mut transport,
        &rng,
        &device_addr,
        "integration-test-phone",
        None,
    )
    .await
    .expect("Phase 1 AEAD pairing should succeed");

    // Verify artifacts
    assert_eq!(artifacts.rf_channel, 6);
    assert_eq!(artifacts.phone_label, "integration-test-phone");

    // PSK should not be all-zeros
    assert_ne!(&*artifacts.phone_psk, &[0u8; 32]);

    // Verify key hint matches PSK
    let expected_hint = sonde_pair::validation::compute_key_hint(&artifacts.phone_psk);
    assert_eq!(artifacts.phone_key_hint, expected_hint);

    peripheral.cancel();
}

/// Phase 1 AEAD re-pairing: running Phase 1 twice should succeed each time.
#[tokio::test]
async fn phase1_loopback_re_pair() {
    let (mut transport, peripheral) = setup().await;
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    // First pairing
    let first = phase1::pair_with_gateway(&mut transport, &rng, &device_addr, "first-phone", None)
        .await
        .expect("first pairing should succeed");

    // Second pairing — new PSK generated each time
    let second =
        phase1::pair_with_gateway(&mut transport, &rng, &device_addr, "second-phone", None)
            .await
            .expect("re-pairing should succeed");

    // PSKs should differ (fresh random each time)
    assert_ne!(*first.phone_psk, *second.phone_psk);
    assert_eq!(second.phone_label, "second-phone");

    peripheral.cancel();
}

/// Phase 1 AEAD with empty phone label (allowed by spec: 0–64 bytes).
#[tokio::test]
async fn phase1_loopback_empty_label() {
    let (mut transport, peripheral) = setup().await;
    let rng = OsRng;
    let device_addr = [0x10, 0x0B, 0xAC, 0x00, 0x00, 0x01];

    let artifacts = phase1::pair_with_gateway(&mut transport, &rng, &device_addr, "", None)
        .await
        .expect("pairing with empty label should succeed");

    assert_eq!(artifacts.phone_label, "");

    peripheral.cancel();
}
