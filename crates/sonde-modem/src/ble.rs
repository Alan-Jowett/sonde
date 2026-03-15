// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE GATT server driver for the Gateway Pairing Service.
//!
//! Hosts the Gateway Pairing Service (`0000FE60-0000-1000-8000-00805F9B34FB`)
//! with a Gateway Command characteristic (`0000FE61-0000-1000-8000-00805F9B34FB`,
//! Write + Indicate) on the ESP32-S3 using the NimBLE stack via esp32-nimble.
//!
//! # Design (MD-0400 – MD-0414)
//!
//! - BLE advertising is **OFF** by default after boot and after `RESET` (MD-0407/MD-0412).
//! - `enable()` starts advertising; `disable()` stops advertising and disconnects any client.
//! - Only one BLE connection at a time (MD-0405).
//! - LESC Numeric Comparison pairing: passkey relayed to gateway via `BleEvent::PairingConfirm`;
//!   gateway replies via `pairing_confirm_reply()` (MD-0404/MD-0414).
//! - ATT MTU negotiation ≥ 247 bytes; connections whose MTU remains below
//!   `BLE_MIN_MTU` after the ATT MTU Exchange are rejected at authentication
//!   complete time (MD-0402).
//! - Indication fragmentation: payloads larger than (MTU − 3) bytes are split into
//!   multiple indications with confirmation between chunks (MD-0403).
//! - GATT writes forwarded as `BleEvent::Recv`; empty writes discarded (MD-0409).
//! - `BleEvent::Connected` sent after LESC pairing completes (MD-0410).
//! - `BleEvent::Disconnected` sent on every disconnect (MD-0411).
//! - BLE and ESP-NOW run concurrently without interference (MD-0405).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    BLEAdvertisementData, BLEDevice, NimbleProperties,
};
use log::{info, warn};
use sonde_protocol::modem::BLE_MIN_MTU;
use sonde_protocol::modem::MAC_SIZE;

use crate::bridge::{Ble, BleEvent};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Gateway Pairing Service UUID (`0000FE60-0000-1000-8000-00805F9B34FB`).
const GATEWAY_SERVICE_UUID: BleUuid = BleUuid::Uuid16(0xFE60);

/// Gateway Command characteristic UUID (`0000FE61-0000-1000-8000-00805F9B34FB`).
const GATEWAY_COMMAND_UUID: BleUuid = BleUuid::Uuid16(0xFE61);

/// Maximum ATT payload per indication fragment = MTU - 3.
///
/// A minimum of 1 byte is enforced so that `data.chunks(chunk_size)` never
/// panics even if MTU is negotiated to a very small value before the
/// low-MTU disconnect is processed (MD-0402).
fn max_indication_payload(mtu: u16) -> usize {
    (mtu.saturating_sub(3)) as usize
}

// ---------------------------------------------------------------------------
// Shared state between callbacks and the main struct
// ---------------------------------------------------------------------------

struct BleState {
    /// Queued events to deliver to the bridge.
    events: VecDeque<BleEvent>,
    /// Indication in progress: remaining chunks to send.
    indication_queue: VecDeque<Vec<u8>>,
    /// True if waiting for ATT Handle Value Confirmation.
    awaiting_confirm: bool,
    /// BLE advertising is currently active.
    advertising: bool,
    /// Negotiated ATT MTU for the current connection (0 = not connected).
    /// Updated in `on_connect` with the initial connection MTU; by the time
    /// `on_authentication_complete` fires, the ATT MTU Exchange should have
    /// completed and the value should reflect the negotiated MTU.
    mtu: u16,
    /// Numeric Comparison passkey relayed to gateway; operator decision pending.
    /// `BleEvent::Connected` is deferred until the operator accepts (MD-0414).
    pairing_pending: bool,
    /// Deferred Connected event stored while awaiting operator confirmation.
    deferred_connected: Option<([u8; MAC_SIZE], u16)>,
}

impl BleState {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            indication_queue: VecDeque::new(),
            awaiting_confirm: false,
            advertising: false,
            mtu: 0,
            pairing_pending: false,
            deferred_connected: None,
        }
    }
}

// ---------------------------------------------------------------------------
// EspBleDriver
// ---------------------------------------------------------------------------

/// NimBLE GATT server driver implementing the Gateway Pairing Service.
///
/// # Singleton pattern
///
/// `esp32-nimble` manages `BLEDevice` as a static singleton.  `BLEDevice::take()`
/// returns the same static `&'static mut BLEDevice` reference every time it is
/// called --- it does not consume or move the device.  All methods in this struct
/// call `BLEDevice::take()` independently, which is safe and idiomatic for the
/// esp32-nimble v0.11 API.
pub struct EspBleDriver {
    state: Arc<Mutex<BleState>>,
}

impl EspBleDriver {
    /// Initialize NimBLE, register the Gateway Pairing Service, and configure
    /// LESC Numeric Comparison security (MD-0404).
    pub fn new() -> Self {
        let ble_device = BLEDevice::take();

        // Configure LESC Numeric Comparison security (MD-0404).
        ble_device
            .security()
            .set_auth(AuthReq::all())
            .set_io_cap(SecurityIOCap::DisplayYesNo);

        let state = Arc::new(Mutex::new(BleState::new()));

        let ble_server = ble_device.get_server();

        // --- Connection event handler ---
        //
        // The initial `desc.conn_params.mtu` may report the default ATT MTU
        // (23 bytes) before the ATT MTU Exchange has completed.  We store it
        // but defer MTU enforcement to `on_authentication_complete`, by which
        // time the exchange should have occurred (MD-0402).
        let state_connect = Arc::clone(&state);
        ble_server.on_connect(move |server, desc| {
            let peer_addr: [u8; MAC_SIZE] = desc.address().val;
            let mtu = desc.conn_params.mtu as u16;
            info!("BLE: client connected addr={:?} mtu={}", peer_addr, mtu);

            // MD-0405: Only one connection at a time.
            // If a second client connects, disconnect immediately.
            if server.connected_count() > 1 {
                warn!("BLE: second connection rejected (MD-0405)");
                let _ = server.disconnect_with_code(
                    desc.conn_handle,
                    esp32_nimble::enums::DisconnReason::ConnTermByLocalHost,
                );
                return;
            }

            if let Ok(mut s) = state_connect.lock() {
                // NimBLE stops advertising when a client connects.  Clear the
                // flag so that a subsequent enable() will restart advertising
                // after this connection ends (MD-0407).
                s.advertising = false;
                s.mtu = mtu;
            }
        });

        // --- Disconnect event handler ---
        let state_disconnect = Arc::clone(&state);
        ble_server.on_disconnect(move |desc, reason| {
            let peer_addr: [u8; MAC_SIZE] = desc.address().val;
            info!(
                "BLE: client disconnected addr={:?} reason={:?}",
                peer_addr, reason
            );
            if let Ok(mut s) = state_disconnect.lock() {
                s.mtu = 0;
                s.indication_queue.clear();
                s.awaiting_confirm = false;
                s.pairing_pending = false;
                s.deferred_connected = None;
                s.events.push_back(BleEvent::Disconnected {
                    peer_addr,
                    reason: reason as u8,
                });
            }
        });

        // --- Passkey request handler ---
        // This callback fires for PasskeyDisplay and PasskeyInput IO capabilities.
        // For DisplayYesNo (Numeric Comparison), this callback is not invoked by
        // NimBLE --- the passkey arrives via on_confirm_pin instead.
        ble_server.on_passkey_request(move || {
            0u32 // Not used for Numeric Comparison; return 0 as a no-op.
        });

        // --- Numeric Comparison passkey relay (MD-0414) ---
        //
        // NimBLE calls on_confirm_pin synchronously during the SMP exchange and
        // requires an immediate yes/no decision.  We return `true` to let
        // pairing proceed, then relay the passkey to the gateway for operator
        // verification.  `BleEvent::Connected` is deferred until the operator
        // replies via `pairing_confirm_reply()`.
        //
        // If the operator rejects, `pairing_confirm_reply(false)` disconnects
        // the client and suppresses Connected.  NVS bond persistence is
        // disabled (`CONFIG_BT_NIMBLE_NVS_PERSIST=n`) so no unapproved bond
        // is stored; each pairing session is independent.
        let state_confirm = Arc::clone(&state);
        ble_server.on_confirm_pin(move |passkey| {
            info!("BLE: Numeric Comparison passkey = {:06}", passkey);
            if let Ok(mut s) = state_confirm.lock() {
                s.pairing_pending = true;
                s.events.push_back(BleEvent::PairingConfirm { passkey });
            }
            true
        });

        // --- Pairing complete handler ---
        let state_auth = Arc::clone(&state);
        ble_server.on_authentication_complete(move |desc, result| {
            if result.is_ok() {
                let peer_addr: [u8; MAC_SIZE] = desc.address().val;
                if let Ok(mut s) = state_auth.lock() {
                    // MD-0402: Reject connections whose MTU is still below the
                    // minimum after authentication completes.  By this point the
                    // ATT MTU Exchange should have finished; if the peer doesn't
                    // support the required MTU, we disconnect.
                    if s.mtu < BLE_MIN_MTU {
                        warn!(
                            "BLE: pairing complete but MTU too low ({}); disconnecting (MD-0402)",
                            s.mtu
                        );
                        // Queue a disconnect; on_disconnect will emit Disconnected.
                        let ble_device = BLEDevice::take();
                        ble_device.get_server().disconnect_all();
                        return;
                    }
                    // MD-0414: If Numeric Comparison is pending operator
                    // confirmation, defer BLE_CONNECTED until accepted.
                    if s.pairing_pending {
                        info!("BLE: LESC pairing complete — deferring BLE_CONNECTED until operator confirms");
                        s.deferred_connected = Some((peer_addr, s.mtu));
                    } else {
                        info!("BLE: pairing complete — sending BLE_CONNECTED (MD-0410)");
                        s.events.push_back(BleEvent::Connected {
                            peer_addr,
                            mtu: s.mtu,
                        });
                    }
                }
            } else {
                warn!("BLE: pairing failed: {:?}", result);
            }
        });

        // --- GATT service + Gateway Command characteristic ---
        let ble_service = ble_server.create_service(GATEWAY_SERVICE_UUID);

        let gateway_cmd_char = ble_service.lock().create_characteristic(
            GATEWAY_COMMAND_UUID,
            NimbleProperties::WRITE | NimbleProperties::INDICATE,
        );

        // GATT write handler: forward to gateway as BLE_RECV (MD-0409).
        // The on_connect handler enforces single-connection (MD-0405) so any
        // write arriving here is from the one accepted client.
        let state_write = Arc::clone(&state);
        gateway_cmd_char.lock().on_write(move |args| {
            let value = args.recv_data();
            if value.is_empty() {
                return; // Empty writes discarded (MD-0409).
            }
            // Only forward writes when a connection is active (mtu > 0 means
            // a client is connected and MTU was accepted).
            if let Ok(mut s) = state_write.lock() {
                if s.mtu > 0 {
                    info!("BLE: GATT write {} bytes", value.len());
                    s.events.push_back(BleEvent::Recv(value.to_vec()));
                }
            }
        });

        info!("BLE GATT Gateway Pairing Service registered (UUID 0xFE60)");

        Self { state }
    }
}

impl Ble for EspBleDriver {
    fn enable(&mut self) {
        {
            let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.advertising {
                return; // Already enabled — no-op (MD-0407).
            }
        }

        let ble_device = BLEDevice::take();
        let ble_advertising = ble_device.get_advertising();

        let mut adv_data = BLEAdvertisementData::new();
        adv_data.name("sonde-modem");
        adv_data.add_service_uuid(GATEWAY_SERVICE_UUID);

        if let Err(e) = ble_advertising.lock().set_data(&mut adv_data) {
            warn!("BLE: set_data failed: {:?}", e);
            return;
        }
        if let Err(e) = ble_advertising.lock().start() {
            warn!("BLE: start_advertising failed: {:?}", e);
            return;
        }

        if let Ok(mut s) = self.state.lock() {
            s.advertising = true;
        }
        info!("BLE advertising started (MD-0407)");
    }

    fn disable(&mut self) {
        let ble_device = BLEDevice::take();
        let ble_advertising = ble_device.get_advertising();

        if let Err(e) = ble_advertising.lock().stop() {
            warn!("BLE: stop_advertising failed: {:?}", e);
        }

        // Disconnect any active client.
        let ble_server = ble_device.get_server();
        ble_server.disconnect_all();

        if let Ok(mut s) = self.state.lock() {
            s.advertising = false;
            s.indication_queue.clear();
            s.awaiting_confirm = false;
            s.pairing_pending = false;
            s.deferred_connected = None;
        }
        info!("BLE advertising stopped (MD-0407)");
    }

    fn indicate(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let mtu = {
            let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.mtu == 0 {
                return; // No client connected — silently discard (MD-0408).
            }
            s.mtu
        };

        // Fragment into chunks of at most (MTU − 3) bytes (MD-0403).
        // Enforce at least 1 byte per chunk to prevent chunks(0) panic (MD-0402).
        let chunk_size = max_indication_payload(mtu).max(1);
        {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            for chunk in data.chunks(chunk_size) {
                s.indication_queue.push_back(chunk.to_vec());
            }
        }

        self.send_next_chunk();
    }

    fn pairing_confirm_reply(&mut self, accept: bool) {
        if accept {
            info!("BLE: Numeric Comparison accepted by operator");
            if let Ok(mut s) = self.state.lock() {
                s.pairing_pending = false;
                if let Some((peer_addr, mtu)) = s.deferred_connected.take() {
                    s.events.push_back(BleEvent::Connected { peer_addr, mtu });
                }
            }
        } else {
            warn!("BLE: Numeric Comparison rejected by operator — disconnecting");
            if let Ok(mut s) = self.state.lock() {
                s.pairing_pending = false;
                s.deferred_connected = None;
            }
            let ble_device = BLEDevice::take();
            let ble_server = ble_device.get_server();
            ble_server.disconnect_all();
        }
    }

    fn drain_event(&self) -> Option<BleEvent> {
        // Send the next pending indication chunk if confirmed.
        {
            let (pending, awaiting) = {
                let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
                (!s.indication_queue.is_empty(), s.awaiting_confirm)
            };
            if pending && !awaiting {
                self.send_next_chunk();
            }
        }

        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.events.pop_front()
    }
}

impl EspBleDriver {
    /// Send the next indication chunk from the queue, if any.
    ///
    /// # Blocking behavior
    ///
    /// `indicate()` in esp32-nimble v0.11 blocks until the peer sends an ATT
    /// Handle Value Confirmation (or returns an error on timeout/disconnect).
    /// Because this is driven from the bridge `poll()` loop, a slow peer can
    /// stall USB/radio processing.  The blocking duration is bounded by the
    /// NimBLE ATT confirmation timeout (typically 30s), which must be kept
    /// below `CONFIG_ESP_TASK_WDT_TIMEOUT_S` to avoid watchdog panics.
    /// Moving indication sending to a dedicated FreeRTOS task would remove
    /// this coupling but is deferred as a future improvement.
    fn send_next_chunk(&self) {
        let chunk = {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.awaiting_confirm || s.indication_queue.is_empty() || s.mtu == 0 {
                return;
            }
            let chunk = s.indication_queue.pop_front().unwrap();
            s.awaiting_confirm = true;
            chunk
        };

        let ble_device = BLEDevice::take();
        let ble_server = ble_device.get_server();
        let service = ble_server.get_service(GATEWAY_SERVICE_UUID);
        let Some(svc) = service else {
            warn!("BLE: GATT service not found; discarding indication chunk");
            if let Ok(mut s) = self.state.lock() {
                s.awaiting_confirm = false;
            }
            return;
        };
        let characteristic = svc.lock().get_characteristic(GATEWAY_COMMAND_UUID);
        let Some(chr) = characteristic else {
            warn!("BLE: GATT characteristic not found; discarding indication chunk");
            if let Ok(mut s) = self.state.lock() {
                s.awaiting_confirm = false;
            }
            return;
        };

        let notify_result = chr.lock().set_value(&chunk).indicate();
        // In esp32-nimble v0.11, `indicate()` blocks until the ATT
        // Handle Value Confirmation is received from the peer (or
        // returns an error on timeout/disconnect). Clearing
        // awaiting_confirm immediately is correct because the
        // confirmation has already been received before `Ok(())` is
        // returned.
        match notify_result {
            Ok(()) => {
                if let Ok(mut s) = self.state.lock() {
                    s.awaiting_confirm = false;
                }
            }
            Err(e) => {
                warn!("BLE: indication failed: {:?}", e);
                if let Ok(mut s) = self.state.lock() {
                    s.awaiting_confirm = false;
                }
            }
        }
    }
}
