// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32-specific BLE GATT server for node provisioning mode.
//!
//! Implements the hardware-facing portion of BLE pairing mode:
//! - BLE stack initialization via NimBLE (`esp32-nimble`).
//! - Node Provisioning Service (UUID `0000FE50-0000-1000-8000-00805F9B34FB`).
//! - Node Command characteristic (UUID `0000FE51-...`, Write+Indicate).
//! - Advertising as `sonde-XXXX` (last 4 hex digits of BLE MAC) (ND-0903).
//! - MTU negotiation >= 247 bytes (ND-0904).
//! - LESC Just Works pairing acceptance (ND-0904).
//! - Calls into the platform-independent handler in `ble_pairing.rs`.
//! - Returns on BLE disconnect so the caller can reboot (ND-0907).
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
    do_diag_relay, encode_diag_relay_response, encode_node_ack, handle_diag_relay_request,
    handle_node_provision, is_mtu_acceptable, parse_ble_envelope, parse_node_provision,
    BLE_MIN_ATT_MTU, BLE_MSG_NODE_PROVISION,
};
use crate::error::NodeResult;
use crate::esp_transport::EspNowTransport;
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;
use sonde_protocol::BLE_DIAG_RELAY_REQUEST;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Node Provisioning Service UUID (`0000FE50-0000-1000-8000-00805F9B34FB`).
const NODE_SERVICE_UUID: BleUuid = BleUuid::Uuid16(0xFE50);

/// Node Command characteristic UUID (`0000FE51-0000-1000-8000-00805F9B34FB`).
const NODE_COMMAND_UUID: BleUuid = BleUuid::Uuid16(0xFE51);

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
/// `transport`: optional ESP-NOW transport for DIAG_RELAY_REQUEST support
/// (ND-1100). When `Some`, diagnostic relay requests are forwarded over
/// the radio with channel switching (ND-1101, ND-1106). When `None`,
/// relay requests return `DIAG_RELAY_STATUS_CHANNEL_ERROR`.
///
/// Returns `Ok(())` when the BLE connection is terminated (the caller should
/// reboot per ND-0907), or `Err` if BLE initialisation fails.
pub fn run_ble_pairing_mode<S: PlatformStorage>(
    storage: &mut S,
    map_storage: &mut MapStorage,
    button_held: bool,
    mut transport: Option<&mut EspNowTransport>,
) -> NodeResult<()> {
    let paired_on_entry = storage.read_key().is_some();

    // The GATT write callback cannot hold &mut storage (not Send, lifetime
    // issues). Instead, the callback stores raw write bytes in a shared
    // Option, and the main loop polls it with direct &mut storage access.
    let pending_write: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let disconnected = Arc::new(Mutex::new(false));
    let authenticated = Arc::new(Mutex::new(false));
    let conn_handle: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));

    // --- NimBLE initialisation ---
    let ble_device = BLEDevice::take();

    // Configure LESC Just Works security (ND-0904).
    // AuthReq::all() includes SC (Secure Connections) + Bond + MITM,
    // matching the modem's configuration. With NoInputNoOutput IO cap,
    // MITM is downgraded to Just Works but LESC is still enforced.
    ble_device
        .security()
        .set_auth(AuthReq::all())
        .set_io_cap(SecurityIOCap::NoInputNoOutput);

    let ble_server = ble_device.get_server();

    // --- Connection event ---
    let disc_connect = Arc::clone(&disconnected);
    let handle_connect = Arc::clone(&conn_handle);
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

        if let Ok(mut d) = disc_connect.lock() {
            *d = false;
        }
        if let Ok(mut h) = handle_connect.lock() {
            *h = Some(desc.conn_handle());
        }

        // Proactively initiate LESC pairing from the server side so that
        // clients that don't trigger pairing on their own (e.g. btleplug
        // on WinRT) still go through LESC Just Works (ND-0904 criterion 3).
        let conn_handle = desc.conn_handle();
        unsafe {
            esp_idf_sys::ble_gap_security_initiate(conn_handle);
        }
        info!(
            "BLE: server-initiated security for conn_handle={}",
            conn_handle
        );
    });

    // --- Disconnect event ---
    let disc_disconnect = Arc::clone(&disconnected);
    let auth_disconnect = Arc::clone(&authenticated);
    let handle_disconnect = Arc::clone(&conn_handle);
    ble_server.on_disconnect(move |desc, _reason| {
        info!("BLE: client disconnected addr={:?}", desc.address());
        if let Ok(mut d) = disc_disconnect.lock() {
            *d = true;
        }
        if let Ok(mut a) = auth_disconnect.lock() {
            *a = false;
        }
        if let Ok(mut h) = handle_disconnect.lock() {
            *h = None;
        }
    });

    // --- Authentication complete ---
    let auth_complete = Arc::clone(&authenticated);
    ble_server.on_authentication_complete(move |server, desc, result| {
        if result.is_ok() {
            let mtu = desc.mtu();
            if !is_mtu_acceptable(mtu) {
                warn!(
                    "BLE: MTU too low ({} < {}); disconnecting (ND-0904)",
                    mtu, BLE_MIN_ATT_MTU
                );
                let _ = server.disconnect(desc.conn_handle());
            } else {
                info!("BLE: LESC pairing complete, MTU={}", mtu);
                if let Ok(mut a) = auth_complete.lock() {
                    *a = true;
                }
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

    // GATT write handler: all writes are stored into `pending_write`.
    // Writes received before LESC pairing completes (ND-0904 criterion 4)
    // are accepted but only processed by the main loop on a later poll.
    let write_pending = Arc::clone(&pending_write);
    let write_auth = Arc::clone(&authenticated);
    node_cmd_char.lock().on_write(move |args| {
        let value = args.recv_data();
        if value.is_empty() {
            return;
        }
        let is_auth = write_auth.lock().map(|a| *a).unwrap_or(false);
        if !is_auth {
            info!(
                "BLE: GATT write {} bytes buffered (awaiting authentication)",
                value.len()
            );
        } else {
            info!("BLE: GATT write {} bytes", value.len());
        }
        if let Ok(mut p) = write_pending.lock() {
            *p = Some(value.to_vec());
        }
    });

    // --- Advertising ---
    let mac = ble_device
        .get_addr()
        .map_err(|e| {
            warn!("BLE: failed to read MAC address: {:?}", e);
            crate::error::NodeError::Transport("BLE: failed to read MAC address")
        })?
        .as_le_bytes();
    let device_name = format!("sonde-{:02x}{:02x}", mac[1], mac[0]);
    info!("BLE: advertising as '{}' (ND-0903)", device_name);

    // Set the GAP device name so connected clients (e.g. Windows) see
    // the correct name instead of the NimBLE default ("nimble") (ND-0903).
    if let Err(e) = BLEDevice::set_device_name(&device_name) {
        warn!("BLE: failed to set GAP device name: {:?}", e);
    }

    let ble_advertising = ble_device.get_advertising();
    let mut adv_data = BLEAdvertisementData::new();
    adv_data.name(&device_name);
    adv_data.add_service_uuid(NODE_SERVICE_UUID);

    ble_advertising
        .lock()
        .set_data(&mut adv_data)
        .map_err(|e| {
            warn!("BLE: set_data failed: {:?}", e);
            crate::error::NodeError::Transport("BLE: set_data failed")
        })?;
    ble_advertising.lock().start().map_err(|e| {
        warn!("BLE: start_advertising failed: {:?}", e);
        crate::error::NodeError::Transport("BLE: start_advertising failed")
    })?;

    info!("BLE Node Provisioning Service registered (UUID 0xFE50, ND-0902)");

    // --- Main loop: poll for writes and disconnects ---
    loop {
        // Check for disconnect.
        if let Ok(d) = disconnected.lock() {
            if *d {
                info!("BLE: disconnect detected -- exiting pairing mode");
                break;
            }
        }

        // Check for a pending GATT write (only process after auth).
        // Primary path: on_authentication_complete sets `authenticated`.
        // Fallback: if on_authentication_complete didn't fire (e.g.,
        // esp32-nimble doesn't dispatch BLE_GAP_EVENT_ENC_CHANGE on this
        // build), check the connection's encryption status directly.
        let is_auth = authenticated.lock().map(|a| *a).unwrap_or(false);
        let is_auth = is_auth || check_encryption_fallback(&conn_handle, &authenticated);
        let write_data = if is_auth {
            if let Ok(mut p) = pending_write.lock() {
                p.take()
            } else {
                None
            }
        } else {
            None
        };

        if let Some(data) = write_data {
            info!("BLE: GATT write received ({} bytes)", data.len());

            // Parse BLE envelope. Silently discard malformed/unknown
            // messages -- the phone will time out waiting for NODE_ACK.
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
                            Some(encode_node_ack(status))
                        }
                        Err(e) => {
                            warn!("BLE: NODE_PROVISION parse error: {}", e);
                            None // silently discard
                        }
                    }
                }
                Some((msg_type, body)) if msg_type == BLE_DIAG_RELAY_REQUEST => {
                    match (handle_diag_relay_request(body), transport.as_deref_mut()) {
                        (Ok(params), Some(t)) => {
                            info!(
                                "BLE: DIAG_RELAY_REQUEST rf_channel={} (ND-1100)",
                                params.rf_channel
                            );
                            // Save current channel, switch, relay, restore (ND-1101, ND-1106).
                            let mut orig_primary: u8 = 0;
                            let mut orig_secondary: esp_idf_sys::wifi_second_chan_t = 0;
                            let got_channel = unsafe {
                                esp_idf_sys::esp_wifi_get_channel(
                                    &mut orig_primary,
                                    &mut orig_secondary,
                                ) == esp_idf_sys::ESP_OK
                            };
                            if !got_channel {
                                // Fall back to stored channel so we can still restore (ND-1106).
                                orig_primary = storage.read_channel().unwrap_or(1);
                                orig_secondary =
                                    esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE;
                                warn!(
                                    "BLE: esp_wifi_get_channel failed, will restore to channel {}",
                                    orig_primary
                                );
                            }
                            let set_ok = unsafe {
                                let rc = esp_idf_sys::esp_wifi_set_channel(
                                    params.rf_channel,
                                    esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE,
                                );
                                if rc != esp_idf_sys::ESP_OK {
                                    warn!("BLE: failed to set Wi-Fi channel {} for DIAG relay: err={}", params.rf_channel, rc);
                                }
                                rc == esp_idf_sys::ESP_OK
                            };
                            if !set_ok {
                                Some(encode_diag_relay_response(
                                    sonde_protocol::DIAG_RELAY_STATUS_CHANNEL_ERROR,
                                    &[],
                                ))
                            } else {
                                let response = do_diag_relay(t, &params);
                                // Always restore channel (ND-1106).
                                unsafe {
                                    let rc = esp_idf_sys::esp_wifi_set_channel(
                                        orig_primary,
                                        orig_secondary,
                                    );
                                    if rc != esp_idf_sys::ESP_OK {
                                        warn!("BLE: failed to restore Wi-Fi channel after DIAG relay: err={}", rc);
                                    }
                                }
                                Some(response)
                            }
                        }
                        (Ok(_), None) => {
                            warn!("BLE: DIAG_RELAY_REQUEST but no transport available");
                            Some(encode_diag_relay_response(
                                sonde_protocol::DIAG_RELAY_STATUS_CHANNEL_ERROR,
                                &[],
                            ))
                        }
                        (Err(error_response), _) => Some(error_response),
                    }
                }
                Some((msg_type, _)) => {
                    warn!(
                        "BLE: unexpected message type 0x{:02x}, discarding",
                        msg_type
                    );
                    None // silently discard
                }
                None => {
                    warn!("BLE: envelope parse error, discarding");
                    None // silently discard
                }
            };

            // Send NODE_ACK indication if we have a valid response.
            if let Some(ack) = ack_data {
                let current_handle = conn_handle.lock().ok().and_then(|h| *h);
                if let Some(handle) = current_handle {
                    let chr = node_cmd_char.lock();
                    if let Err(e) = chr.notify_with(&ack, handle) {
                        warn!("BLE: NODE_ACK indication failed: {:?}", e);
                    }
                } else {
                    warn!("BLE: no active connection for NODE_ACK indication");
                }
            }
        }

        // Busy-wait with a short sleep to avoid spinning.
        // Feed the task watchdog on each iteration so the indefinite-duration
        // BLE pairing session does not trigger the 20 s watchdog (ND-0919 AC 7).
        unsafe {
            esp_idf_svc::sys::esp_task_wdt_reset();
            esp_idf_svc::sys::vTaskDelay(
                (POLL_INTERVAL.as_millis() as u32 * esp_idf_svc::sys::CONFIG_FREERTOS_HZ) / 1000,
            );
        }
    }

    Ok(())
}

/// Fallback encryption check for when `on_authentication_complete` doesn't
/// fire (e.g., esp32-nimble build that doesn't dispatch ENC_CHANGE event 38).
/// Returns `true` only if the link is encrypted AND MTU is acceptable,
/// promoting `authenticated` to `true`.  Returns `false` if not encrypted,
/// not connected, or MTU is too low (disconnects in that case).
#[cfg(feature = "esp")]
fn check_encryption_fallback(
    conn_handle: &Arc<Mutex<Option<u16>>>,
    authenticated: &Arc<Mutex<bool>>,
) -> bool {
    let handle = match conn_handle.lock().ok().and_then(|h| *h) {
        Some(h) => h,
        None => return false,
    };
    let desc = match esp32_nimble::utilities::ble_gap_conn_find(handle) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if !desc.encrypted() {
        return false;
    }
    let Ok(mut a) = authenticated.lock() else {
        return false;
    };
    if *a {
        return true;
    }
    let mtu = desc.mtu();
    if !is_mtu_acceptable(mtu) {
        warn!(
            "BLE: encrypted but MTU too low ({} < {}); disconnecting (ND-0904)",
            mtu, BLE_MIN_ATT_MTU
        );
        let server = BLEDevice::take().get_server();
        let _ = server.disconnect(handle);
        return false;
    }
    info!("BLE: encryption detected via poll, MTU={}", mtu);
    *a = true;
    true
}
