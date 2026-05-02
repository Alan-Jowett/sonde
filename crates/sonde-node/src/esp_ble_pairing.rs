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
    BLEAdvertisementData, BLECharacteristic, BLEDevice, NimbleProperties, NotifyTxStatus,
};
use log::{info, warn};

use crate::ble_pairing::{
    do_diag_relay, encode_diag_relay_ack, encode_diag_relay_result, encode_node_ack,
    handle_node_provision, handle_start_diag_relay, is_mtu_acceptable, parse_ble_envelope,
    parse_node_provision, DiagRelayParams, DiagRelayResult, BLE_MIN_ATT_MTU,
    BLE_MSG_NODE_PROVISION,
};
use crate::error::NodeResult;
use crate::esp_transport::EspNowTransport;
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;
use sonde_protocol::{BLE_FETCH_DIAG_RESULT, BLE_START_DIAG_RELAY};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Node Provisioning Service UUID (`0000FE50-0000-1000-8000-00805F9B34FB`).
const NODE_SERVICE_UUID: BleUuid = BleUuid::Uuid16(0xFE50);

/// Node Command characteristic UUID (`0000FE51-0000-1000-8000-00805F9B34FB`).
const NODE_COMMAND_UUID: BleUuid = BleUuid::Uuid16(0xFE51);

/// Polling interval for the main loop waiting for disconnect.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingIndication {
    NodeAck,
    DiagAckAccepted,
    DiagAckRejected,
    DiagResult { clear_on_success: bool },
}

#[derive(Debug)]
enum SessionExit {
    Disconnect,
    StartDiagnostic(DiagRelayParams),
}

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
/// `transport`: optional ESP-NOW transport for async diagnostic relay support
/// (ND-1100). When `Some`, accepted diagnostics are executed over the radio
/// with channel switching (ND-1102, ND-1106). When `None`, the node still
/// accepts the request, suspends BLE, and later returns a channel-error result.
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
    let mut stored_diag_result: Option<DiagRelayResult> = None;

    loop {
        match run_ble_pairing_session(
            storage,
            map_storage,
            button_held,
            paired_on_entry,
            &mut stored_diag_result,
        )? {
            SessionExit::Disconnect => {
                info!("BLE: disconnect detected -- exiting pairing mode");
                return Ok(());
            }
            SessionExit::StartDiagnostic(params) => {
                stored_diag_result = Some(execute_diag_relay(
                    storage,
                    transport.as_deref_mut(),
                    &params,
                ));
            }
        }
    }
}

fn run_ble_pairing_session<S: PlatformStorage>(
    storage: &mut S,
    map_storage: &mut MapStorage,
    button_held: bool,
    paired_on_entry: bool,
    stored_diag_result: &mut Option<DiagRelayResult>,
) -> NodeResult<SessionExit> {
    let pending_write: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let disconnected = Arc::new(Mutex::new(false));
    let authenticated = Arc::new(Mutex::new(false));
    let conn_handle: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));
    let notify_pending: Arc<Mutex<Option<PendingIndication>>> = Arc::new(Mutex::new(None));
    let notify_complete: Arc<Mutex<Option<(PendingIndication, bool)>>> = Arc::new(Mutex::new(None));
    let mut pending_diag_start: Option<DiagRelayParams> = None;

    let ble_device = BLEDevice::take();
    ble_device
        .security()
        .set_auth(AuthReq::all())
        .set_io_cap(SecurityIOCap::NoInputNoOutput);

    let ble_server = ble_device.get_server();

    let disc_connect = Arc::clone(&disconnected);
    let handle_connect = Arc::clone(&conn_handle);
    ble_server.on_connect(move |server, desc| {
        let peer_addr = desc.address();
        let mtu = desc.mtu();
        info!("BLE: client connected addr={:?} mtu={}", peer_addr, mtu);
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
        let conn_handle = desc.conn_handle();
        unsafe {
            esp_idf_sys::ble_gap_security_initiate(conn_handle);
        }
        info!(
            "BLE: server-initiated security for conn_handle={}",
            conn_handle
        );
    });

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

    ble_server.on_passkey_request(move || 0u32);

    let ble_service = ble_server.create_service(NODE_SERVICE_UUID);
    let node_cmd_char: Arc<NimbleMutex<BLECharacteristic>> =
        ble_service.lock().create_characteristic(
            NODE_COMMAND_UUID,
            NimbleProperties::WRITE | NimbleProperties::INDICATE,
        );

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

    let notify_pending_cb = Arc::clone(&notify_pending);
    let notify_complete_cb = Arc::clone(&notify_complete);
    node_cmd_char.lock().on_notify_tx(move |notify| {
        let success = matches!(notify.status(), NotifyTxStatus::SuccessIndicate);
        if let Ok(mut pending) = notify_pending_cb.lock() {
            if let Some(kind) = pending.take() {
                if let Ok(mut complete) = notify_complete_cb.lock() {
                    *complete = Some((kind, success));
                }
            }
        }
    });

    let mac = ble_device
        .get_addr()
        .map_err(|e| {
            warn!("BLE: failed to read MAC address: {:?}", e);
            crate::error::NodeError::Transport("BLE: failed to read MAC address")
        })?
        .as_le_bytes();
    let device_name = format!("sonde-{:02x}{:02x}", mac[1], mac[0]);
    info!("BLE: advertising as '{}' (ND-0903)", device_name);
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

    loop {
        if let Some((kind, success)) = notify_complete.lock().ok().and_then(|mut c| c.take()) {
            match kind {
                PendingIndication::DiagAckAccepted if success => {
                    info!("BLE: START_DIAG_RELAY acknowledged -- suspending BLE");
                    if let Some(handle) = conn_handle.lock().ok().and_then(|h| *h) {
                        let _ = ble_server.disconnect(handle);
                    }
                    if let Err(e) = ble_advertising.lock().stop() {
                        warn!("BLE: failed to stop advertising before diagnostic: {:?}", e);
                    }
                    if let Err(e) = BLEDevice::deinit_full() {
                        warn!(
                            "BLE: failed to deinitialize BLE stack before diagnostic: {:?}",
                            e
                        );
                    }
                    return Ok(SessionExit::StartDiagnostic(
                        pending_diag_start
                            .take()
                            .expect("accepted diagnostic ack should retain request"),
                    ));
                }
                PendingIndication::DiagAckAccepted => {
                    warn!("BLE: START_DIAG_RELAY indication failed");
                    pending_diag_start = None;
                }
                PendingIndication::DiagResult {
                    clear_on_success: true,
                } if success => {
                    info!("BLE: DIAG_RELAY_RESULT delivered -- clearing stored result");
                    *stored_diag_result = None;
                }
                PendingIndication::DiagResult { .. } if !success => {
                    warn!("BLE: DIAG_RELAY_RESULT indication failed");
                }
                _ => {}
            }
        }

        if let Ok(d) = disconnected.lock() {
            if *d {
                let _ = BLEDevice::deinit_full();
                return Ok(SessionExit::Disconnect);
            }
        }

        let is_auth = authenticated.lock().map(|a| *a).unwrap_or(false);
        let is_auth = is_auth || check_encryption_fallback(&conn_handle, &authenticated);
        let write_data = if is_auth {
            pending_write.lock().ok().and_then(|mut p| p.take())
        } else {
            None
        };

        if let Some(data) = write_data {
            info!("BLE: GATT write received ({} bytes)", data.len());
            let response = match parse_ble_envelope(&data) {
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
                            Some((PendingIndication::NodeAck, encode_node_ack(status)))
                        }
                        Err(e) => {
                            warn!("BLE: NODE_PROVISION parse error: {}", e);
                            None
                        }
                    }
                }
                Some((msg_type, body)) if msg_type == BLE_START_DIAG_RELAY => {
                    let (kind, payload) = if pending_diag_start.is_some()
                        || stored_diag_result.is_some()
                    {
                        (
                            PendingIndication::DiagAckRejected,
                            encode_diag_relay_ack(sonde_protocol::DIAG_RELAY_ACK_BUSY),
                        )
                    } else {
                        match handle_start_diag_relay(body) {
                            Ok(params) => {
                                info!(
                                    "BLE: START_DIAG_RELAY rf_channel={} payload_len={} (ND-1100)",
                                    params.rf_channel,
                                    params.payload.len()
                                );
                                pending_diag_start = Some(params);
                                (
                                    PendingIndication::DiagAckAccepted,
                                    encode_diag_relay_ack(sonde_protocol::DIAG_RELAY_ACK_ACCEPTED),
                                )
                            }
                            Err(status) => (
                                PendingIndication::DiagAckRejected,
                                encode_diag_relay_ack(status),
                            ),
                        }
                    };
                    Some((kind, payload))
                }
                Some((msg_type, _)) if msg_type == BLE_FETCH_DIAG_RESULT => {
                    let clear_on_success = stored_diag_result.is_some();
                    Some((
                        PendingIndication::DiagResult { clear_on_success },
                        encode_diag_relay_result(stored_diag_result.as_ref()),
                    ))
                }
                Some((msg_type, _)) => {
                    warn!(
                        "BLE: unexpected message type 0x{:02x}, discarding",
                        msg_type
                    );
                    None
                }
                None => {
                    warn!("BLE: envelope parse error, discarding");
                    None
                }
            };

            if let Some((kind, payload)) = response {
                if let Some(handle) = conn_handle.lock().ok().and_then(|h| *h) {
                    if let Ok(mut pending) = notify_pending.lock() {
                        *pending = Some(kind);
                    }
                    let chr = node_cmd_char.lock();
                    if let Err(e) = chr.notify_with(&payload, handle) {
                        warn!("BLE: indication failed: {:?}", e);
                        if let Ok(mut pending) = notify_pending.lock() {
                            *pending = None;
                        }
                        if kind == PendingIndication::DiagAckAccepted {
                            pending_diag_start = None;
                        }
                    }
                } else {
                    warn!("BLE: no active connection for indication");
                    if kind == PendingIndication::DiagAckAccepted {
                        pending_diag_start = None;
                    }
                }
            }
        }

        unsafe {
            esp_idf_svc::sys::esp_task_wdt_reset();
            esp_idf_svc::sys::vTaskDelay(
                (POLL_INTERVAL.as_millis() as u32 * esp_idf_svc::sys::CONFIG_FREERTOS_HZ) / 1000,
            );
        }
    }
}

fn execute_diag_relay<S: PlatformStorage>(
    storage: &mut S,
    transport: Option<&mut EspNowTransport>,
    params: &DiagRelayParams,
) -> DiagRelayResult {
    let Some(transport) = transport else {
        warn!("BLE: START_DIAG_RELAY accepted but no transport available");
        return DiagRelayResult::ChannelError;
    };

    let mut orig_primary: u8 = 0;
    let mut orig_secondary: esp_idf_sys::wifi_second_chan_t = 0;
    let got_channel = unsafe {
        esp_idf_sys::esp_wifi_get_channel(&mut orig_primary, &mut orig_secondary)
            == esp_idf_sys::ESP_OK
    };
    if !got_channel {
        orig_primary = storage.read_channel().unwrap_or(1);
        orig_secondary = esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE;
        warn!(
            "BLE: esp_wifi_get_channel failed, will restore to channel {}",
            orig_primary
        );
    } else {
        info!(
            "BLE: current Wi-Fi channel before DIAG relay primary={} secondary={}",
            orig_primary, orig_secondary
        );
    }

    let set_ok = unsafe {
        let rc = esp_idf_sys::esp_wifi_set_channel(
            params.rf_channel,
            esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE,
        );
        if rc != esp_idf_sys::ESP_OK {
            warn!(
                "BLE: failed to set Wi-Fi channel {} for DIAG relay: err={}",
                params.rf_channel, rc
            );
        }
        rc == esp_idf_sys::ESP_OK
    };
    if !set_ok {
        return DiagRelayResult::ChannelError;
    }

    transport.log_recv_debug_snapshot("before do_diag_relay");
    let result = do_diag_relay(transport, params);
    transport.log_recv_debug_snapshot("after do_diag_relay");

    unsafe {
        let rc = esp_idf_sys::esp_wifi_set_channel(orig_primary, orig_secondary);
        if rc != esp_idf_sys::ESP_OK {
            warn!(
                "BLE: failed to restore Wi-Fi channel after DIAG relay: err={}",
                rc
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {}

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
    // Use raw NimBLE API — esp32_nimble::utilities::ble_gap_conn_find is
    // pub(crate) and not accessible from application code.
    let mut desc: esp_idf_sys::ble_gap_conn_desc = unsafe { core::mem::zeroed() };
    let rc = unsafe { esp_idf_sys::ble_gap_conn_find(handle, &mut desc) };
    if rc != 0 {
        return false;
    }
    let encrypted = desc.sec_state.encrypted() != 0;
    if !encrypted {
        return false;
    }
    let Ok(mut a) = authenticated.lock() else {
        return false;
    };
    if *a {
        return true;
    }
    let mtu = unsafe { esp_idf_sys::ble_att_mtu(handle) };
    // The ATT default MTU (23) means the MTU exchange hasn't happened yet.
    // Android negotiates MTU after bonding completes (BleHelper Step 5),
    // which is after encryption is established.  Don't enforce the MTU
    // minimum until the exchange has occurred — check again next poll.
    const ATT_DEFAULT_MTU: u16 = 23;
    if mtu <= ATT_DEFAULT_MTU {
        return false;
    }
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
