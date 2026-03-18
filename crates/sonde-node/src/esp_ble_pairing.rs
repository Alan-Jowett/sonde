// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-specific BLE GATT server for node provisioning mode.
//!
//! Implements the hardware-facing portion of BLE pairing mode:
//! - BLE stack initialization via NimBLE (`esp32-nimble`).
//! - Node Provisioning Service (UUID `0000FE50-0000-1000-8000-00805F9B34FB`).
//! - Node Command characteristic (UUID `0000FE51-...`, Write+Indicate).
//! - Advertising as `sonde-XXXX` (last 4 hex digits of BLE MAC) (ND-0903).
//! - MTU negotiation ≥ 247 bytes (ND-0904).
//! - LESC Just Works pairing acceptance (ND-0904).
//! - Calls into the platform-independent handler in `ble_pairing.rs`.
//! - Returns on BLE disconnect after provisioning (ND-0907).
//!
//! # Boot flow
//!
//! The entry point is [`run_ble_pairing_mode`].  It blocks until the BLE
//! connection is terminated, then returns so the caller can reboot.
//!
//! This module is only compiled with the `esp` feature because it depends
//! directly on `esp32-nimble` BLE APIs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    utilities::mutex::Mutex as NimbleMutex,
    BLEAdvertisementData, BLECharacteristic, BLEDevice, NimbleProperties,
};
use log::{info, warn};

use crate::ble_pairing::{
    encode_node_ack, handle_node_provision, parse_ble_envelope, parse_node_provision,
    BLE_MSG_NODE_PROVISION, NODE_ACK_STORAGE_ERROR,
};
use crate::error::NodeResult;
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Node Provisioning Service UUID (`0000FE50-0000-1000-8000-00805F9B34FB`).
const NODE_SERVICE_UUID: BleUuid = BleUuid::Uuid16(0xFE50);

/// Node Command characteristic UUID (`0000FE51-0000-1000-8000-00805F9B34FB`).
const NODE_COMMAND_UUID: BleUuid = BleUuid::Uuid16(0xFE51);

/// Minimum negotiated ATT MTU accepted (ND-0904).
const BLE_MTU_MIN: u16 = 247;

/// Polling interval for the main loop waiting for disconnect.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

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
/// Returns `Ok(())` when the BLE connection is terminated (the caller should
/// reboot per ND-0907), or `Err` if BLE initialisation fails.
pub fn run_ble_pairing_mode<S: PlatformStorage>(
    storage: &mut S,
    map_storage: &mut MapStorage,
    button_held: bool,
) -> NodeResult<()> {
    let paired_on_entry = storage.read_key().is_some();

    // Take ownership of storage and map_storage for the duration of BLE mode.
    // We'll return them via the Arc<Mutex> when done.
    //
    // Since PlatformStorage is behind a mutable reference, we need to work
    // with the writes happening inside the GATT callback. We use a channel
    // approach: the callback stores the raw write data, and the main loop
    // processes it with mutable access to storage.
    let pending_write: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let disconnected = Arc::new(Mutex::new(false));

    // --- NimBLE initialisation ---
    let ble_device = BLEDevice::take();

    // Configure LESC Just Works security (ND-0904).
    ble_device
        .security()
        .set_auth(AuthReq::Bond)
        .set_io_cap(SecurityIOCap::NoInputNoOutput);

    let ble_server = ble_device.get_server();

    // --- Connection event ---
    let disc_connect = Arc::clone(&disconnected);
    ble_server.on_connect(move |server, desc| {
        let peer_addr = desc.address();
        let mtu = desc.mtu();
        info!("BLE: client connected addr={:?} mtu={}", peer_addr, mtu);

        // Only one connection at a time.
        if server.connected_count() > 1 {
            warn!("BLE: second connection rejected");
            let _ = server.disconnect_with_reason(desc.conn_handle(), 0x13);
            return;
        }

        // Clear disconnected flag on new connection.
        if let Ok(mut d) = disc_connect.lock() {
            *d = false;
        }
    });

    // --- Disconnect event ---
    let disc_disconnect = Arc::clone(&disconnected);
    ble_server.on_disconnect(move |desc, _reason| {
        info!("BLE: client disconnected addr={:?}", desc.address());
        if let Ok(mut d) = disc_disconnect.lock() {
            *d = true;
        }
    });

    // --- Authentication complete ---
    ble_server.on_authentication_complete(move |server, desc, result| {
        if result.is_ok() {
            let mtu = desc.mtu();
            if mtu < BLE_MTU_MIN {
                warn!(
                    "BLE: MTU too low ({} < {}); disconnecting (ND-0904)",
                    mtu, BLE_MTU_MIN
                );
                let _ = server.disconnect(desc.conn_handle());
            } else {
                info!("BLE: LESC pairing complete, MTU={}", mtu);
            }
        } else {
            warn!("BLE: pairing failed: {:?}", result);
        }
    });

    // Passkey request (no-op for Just Works).
    ble_server.on_passkey_request(move || 0u32);

    // --- GATT service + Node Command characteristic ---
    let ble_service = ble_server.create_service(NODE_SERVICE_UUID);

    let node_cmd_char: Arc<NimbleMutex<BLECharacteristic>> =
        ble_service.lock().create_characteristic(
            NODE_COMMAND_UUID,
            NimbleProperties::WRITE | NimbleProperties::INDICATE,
        );

    // GATT write handler: store the raw bytes for the main loop to process.
    let write_pending = Arc::clone(&pending_write);
    node_cmd_char.lock().on_write(move |args| {
        let value = args.recv_data();
        if value.is_empty() {
            return;
        }
        if let Ok(mut p) = write_pending.lock() {
            *p = Some(value.to_vec());
        }
    });

    // --- Advertising ---
    let mac = ble_device
        .get_addr()
        .map_err(|_| crate::error::NodeError::StorageError("BLE: failed to read MAC address"))?
        .as_le_bytes();
    let device_name = format!("sonde-{:02x}{:02x}", mac[1], mac[0]);
    info!("BLE: advertising as '{}' (ND-0903)", device_name);

    let ble_advertising = ble_device.get_advertising();
    let mut adv_data = BLEAdvertisementData::new();
    adv_data.name(&device_name);
    adv_data.add_service_uuid(NODE_SERVICE_UUID);

    ble_advertising
        .lock()
        .set_data(&mut adv_data)
        .map_err(|_| crate::error::NodeError::StorageError("BLE: set_data failed"))?;
    ble_advertising
        .lock()
        .start()
        .map_err(|_| crate::error::NodeError::StorageError("BLE: start_advertising failed"))?;

    info!("BLE Node Provisioning Service registered (UUID 0xFE50, ND-0902)");

    // --- Main loop: poll for writes and disconnects ---
    loop {
        // Check for disconnect.
        if let Ok(d) = disconnected.lock() {
            if *d {
                info!("BLE: disconnect detected — exiting pairing mode");
                break;
            }
        }

        // Check for a pending GATT write.
        let write_data = {
            if let Ok(mut p) = pending_write.lock() {
                p.take()
            } else {
                None
            }
        };

        if let Some(data) = write_data {
            info!("BLE: GATT write received ({} bytes)", data.len());

            // Parse BLE envelope.
            let ack_data = match parse_ble_envelope(&data) {
                Some((msg_type, body)) if msg_type == BLE_MSG_NODE_PROVISION => {
                    match parse_node_provision(body) {
                        Ok(provision) => {
                            let status = handle_node_provision(
                                &provision,
                                storage,
                                map_storage,
                                button_held,
                                paired_on_entry,
                            );
                            info!("BLE: NODE_PROVISION handled, status=0x{:02x}", status);
                            encode_node_ack(status)
                        }
                        Err(e) => {
                            warn!("BLE: NODE_PROVISION parse error: {}", e);
                            encode_node_ack(NODE_ACK_STORAGE_ERROR)
                        }
                    }
                }
                Some((msg_type, _)) => {
                    warn!("BLE: unexpected message type 0x{:02x}", msg_type);
                    encode_node_ack(NODE_ACK_STORAGE_ERROR)
                }
                None => {
                    warn!("BLE: envelope parse error (too short or malformed)");
                    encode_node_ack(NODE_ACK_STORAGE_ERROR)
                }
            };

            // Send NODE_ACK indication via set_value + notify.
            let mut chr = node_cmd_char.lock();
            chr.set_value(&ack_data);
            chr.notify();
        }

        // Busy-wait with a short sleep to avoid spinning.
        unsafe {
            esp_idf_svc::sys::vTaskDelay(
                (POLL_INTERVAL.as_millis() as u32 * esp_idf_svc::sys::CONFIG_FREERTOS_HZ) / 1000,
            );
        }
    }

    Ok(())
}
