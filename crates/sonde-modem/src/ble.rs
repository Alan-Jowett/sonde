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
//! - ATT MTU negotiation ≥ 247 bytes; connections with MTU < 247 are rejected (MD-0402).
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
    mtu: u16,
    /// True if this connection should be rejected due to low MTU (MD-0402).
    mtu_too_low: bool,
}

impl BleState {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            indication_queue: VecDeque::new(),
            awaiting_confirm: false,
            advertising: false,
            mtu: 0,
            mtu_too_low: false,
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
        let state_connect = Arc::clone(&state);
        ble_server.on_connect(move |server, desc| {
            let peer_addr: [u8; MAC_SIZE] = desc.address().val;
            let mtu = desc.conn_params.mtu as u16;
            info!("BLE: client connected addr={:?} mtu={}", peer_addr, mtu);

            // MD-0405: Only one connection at a time.
            // If a second client connects, disconnect immediately.
            if server.connected_count() > 1 {
                warn!("BLE: second connection rejected (MD-0405)");
                // Disconnect the second (latest) client by stopping advertising
                // and the extra connection will be cleaned up by the stack.
                let _ = server.disconnect_with_code(
                    desc.conn_handle,
                    esp32_nimble::enums::DisconnReason::ConnTermByLocalHost,
                );
                return;
            }

            // MD-0402: Reject connections with ATT MTU < 247.
            if mtu < BLE_MIN_MTU {
                warn!(
                    "BLE: MTU {} < minimum {}; disconnecting (MD-0402)",
                    mtu, BLE_MIN_MTU
                );
                if let Ok(mut s) = state_connect.lock() {
                    s.mtu_too_low = true;
                }
                return;
            }

            if let Ok(mut s) = state_connect.lock() {
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
                s.mtu_too_low = false;
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
        // requires an immediate yes/no decision.  Because our confirmation is an
        // asynchronous round-trip (Modem -> Gateway -> Modem), we return `true` to
        // let the pairing proceed, then relay the passkey to the gateway for
        // operator verification.  If the gateway responds with accept=false, we
        // call `pairing_confirm_reply(false)` which disconnects the client; this
        // is best-effort --- the BLE link may already have completed LESC key
        // exchange by then, but the gateway will not accept the session.
        let state_confirm = Arc::clone(&state);
        ble_server.on_confirm_pin(move |passkey| {
            info!("BLE: Numeric Comparison passkey = {:06}", passkey);
            if let Ok(mut s) = state_confirm.lock() {
                s.events.push_back(BleEvent::PairingConfirm { passkey });
            }
            // Return true to allow pairing to proceed; operator confirmation is
            // handled asynchronously via pairing_confirm_reply() (MD-0414).
            true
        });

        // --- Pairing complete handler ---
        let state_auth = Arc::clone(&state);
        ble_server.on_authentication_complete(move |desc, result| {
            if result.is_ok() {
                let peer_addr: [u8; MAC_SIZE] = desc.address().val;
                if let Ok(mut s) = state_auth.lock() {
                    // MD-0402/MD-0410: Only emit Connected when MTU is accepted.
                    // Low-MTU connections are pending disconnect and must not
                    // produce a spurious BLE_CONNECTED event.
                    if s.mtu_too_low || s.mtu < BLE_MIN_MTU {
                        warn!(
                            "BLE: pairing complete but MTU too low ({}); suppressing BLE_CONNECTED",
                            s.mtu
                        );
                        return;
                    }
                    info!("BLE: LESC pairing complete — sending BLE_CONNECTED (MD-0410)");
                    s.events.push_back(BleEvent::Connected {
                        peer_addr,
                        mtu: s.mtu,
                    });
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
        // Forward the operator's Numeric Comparison decision to NimBLE (MD-0414).
        // The on_confirm_pin callback returned `true` tentatively; if the gateway
        // replies with accept=false we need to disconnect the client.
        if !accept {
            let ble_device = BLEDevice::take();
            let ble_server = ble_device.get_server();
            // Disconnect all clients to abort the pairing.
            ble_server.disconnect_all();
            warn!("BLE: Numeric Comparison rejected by operator — disconnecting");
        } else {
            info!("BLE: Numeric Comparison accepted by operator");
        }
    }

    fn drain_event(&self) -> Option<BleEvent> {
        // MD-0402: disconnect if the negotiated MTU is below the minimum.
        {
            let (too_low, mtu) = {
                let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
                (s.mtu_too_low, s.mtu)
            };
            if too_low {
                warn!("BLE: disconnecting due to low MTU {} (MD-0402)", mtu);
                let ble_device = BLEDevice::take();
                let ble_server = ble_device.get_server();
                ble_server.disconnect_all();
                if let Ok(mut s) = self.state.lock() {
                    s.mtu_too_low = false;
                }
            }
        }

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
        let char = svc.lock().get_characteristic(GATEWAY_COMMAND_UUID);
        let Some(ch) = char else {
            warn!("BLE: GATT characteristic not found; discarding indication chunk");
            if let Ok(mut s) = self.state.lock() {
                s.awaiting_confirm = false;
            }
            return;
        };

        let notify_result = ch.lock().set_value(&chunk).indicate();
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
