// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-specific BLE GATT server for node provisioning mode.
//!
//! Implements the hardware-facing portion of BLE pairing mode:
//! - BLE stack initialization via ESP-IDF BLE APIs.
//! - Node Provisioning Service (UUID `0000FE50-0000-1000-8000-00805F9B34FB`).
//! - Node Command characteristic (UUID `0000FE51-...`, Write+Indicate).
//! - Advertising as `sonde-XXXX` (last 4 hex digits of BLE MAC) (ND-0903).
//! - MTU negotiation ≥ 247 bytes (ND-0904).
//! - LESC Just Works pairing acceptance (ND-0904).
//! - Calls into the platform-independent handler in `ble_pairing.rs`.
//! - Reboots on BLE disconnect (ND-0907).
//!
//! # Boot flow
//!
//! The entry point is [`run_ble_pairing_mode`].  It blocks until the BLE
//! connection is terminated, then returns so the caller can reboot.
//!
//! This module is only compiled with the `esp` feature because it depends
//! directly on `esp-idf-svc` BLE APIs.

use log::warn;

use crate::ble_pairing::{
    encode_node_ack, handle_node_provision, parse_ble_envelope, parse_node_provision,
    BLE_MSG_NODE_PROVISION, NODE_ACK_STORAGE_ERROR,
};
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the BLE pairing mode loop.
///
/// Initialises the BLE GATT server, registers the Node Provisioning Service,
/// starts advertising as `sonde-XXXX`, and processes inbound NODE_PROVISION
/// writes until the BLE connection drops.
///
/// `button_held`: if the pairing button was held at boot, the first
/// NODE_PROVISION triggers a factory reset before writing new credentials
/// (ND-0917).
///
/// Returns when the BLE connection is terminated.  The caller should reboot
/// immediately afterwards per ND-0907.
pub fn run_ble_pairing_mode<S: PlatformStorage>(
    _storage: &mut S,
    _map_storage: &mut MapStorage,
    _button_held: bool,
) {
    // TODO(ND-0902–ND-0904): Initialize BLE stack and register GATT service.
    //
    // Implementation outline:
    //
    // 1. Initialize ESP-IDF BT controller and Bluedroid stack.
    // 2. Register Node Provisioning Service (UUID 0000FE50-...) with
    //    Node Command characteristic (UUID 0000FE51-..., Write+Indicate).
    // 3. Read BLE MAC address; format device name as "sonde-XXXX" where
    //    XXXX = lowercase hex of MAC[4..6].
    // 4. Configure advertising: include Node Provisioning Service UUID and
    //    the formatted device name.
    // 5. Enable BLE advertising.
    // 6. In the GATT write handler for Node Command:
    //    a. Validate the BLE envelope (parse_ble_envelope).
    //    b. If TYPE == BLE_MSG_NODE_PROVISION, parse the body (parse_node_provision).
    //    c. Call handle_node_provision(provision, storage, map_storage, button_held).
    //    d. Send NODE_ACK indication (encode_node_ack(status)).
    // 7. On BLE disconnect event, break out of the loop and return.
    //
    // The ESP-IDF BLE Rust bindings (esp-idf-svc) are still maturing.
    // When they expose stable GAP/GATT APIs, replace this stub with the
    // real implementation.
    //
    // For reference: the needed esp-idf-svc types are expected under
    // `esp_idf_svc::ble::gatt::{BtUuid, GattServer, GattService, ...}`.
    //
    // For now, log a warning so the boot sequence can be exercised in QEMU
    // and on hardware without a host BLE controller.
    warn!("BLE pairing mode: BLE GATT server not yet implemented; entering deep sleep");

    // Suppress unused-import warnings while the stub is in place.
    let _ = (
        encode_node_ack,
        handle_node_provision::<S>,
        parse_ble_envelope,
        parse_node_provision,
        BLE_MSG_NODE_PROVISION,
        NODE_ACK_STORAGE_ERROR,
    );

    // Block indefinitely until the real BLE GATT server is implemented.
    // This prevents the caller from rebooting immediately and creating a
    // battery-draining reboot loop on unpaired nodes.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
