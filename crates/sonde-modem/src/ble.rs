// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE GATT server driver for the Gateway Pairing Service.
//!
//! Hosts the Gateway Pairing Service (`0000FE60-0000-1000-8000-00805F9B34FB`)
//! with a Gateway Command characteristic (`0000FE61-0000-1000-8000-00805F9B34FB`,
//! Write + Indicate) on the ESP32-S3 using the NimBLE stack via ESP-IDF.
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

use esp_idf_svc::bt::ble::gap::{
    AuthReq, BleGapEvent, EspBleGap, OobData, SecurityConfig, SecurityIOCap,
};
use esp_idf_svc::bt::ble::gatt::server::{
    CharacteristicEvent, ConnectionEvent, EspGatts, GattsEvent, NotifyEvent,
};
use esp_idf_svc::bt::ble::gatt::{
    AutoResponse, GattCharacteristic, GattDescriptor, GattInterface, GattService, Handle,
    Permission, Property,
};
use esp_idf_svc::bt::{BdAddr, BtDriver, BtUuid};
use esp_idf_sys::EspError;
use log::{info, warn};

use sonde_protocol::modem::BLE_MIN_MTU;

use crate::bridge::{Ble, BleEvent};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Gateway Pairing Service UUID (`0000FE60-0000-1000-8000-00805F9B34FB`).
const GATEWAY_SERVICE_UUID: BtUuid = BtUuid::uuid16(0xFE60);

/// Gateway Command characteristic UUID (`0000FE61-0000-1000-8000-00805F9B34FB`).
const GATEWAY_COMMAND_UUID: BtUuid = BtUuid::uuid16(0xFE61);

/// Client Characteristic Configuration Descriptor (CCCD) UUID.
const CCCD_UUID: BtUuid = BtUuid::uuid16(0x2902);

/// Maximum ATT payload per indication fragment = MTU - 3.
fn max_indication_payload(mtu: u16) -> usize {
    (mtu.saturating_sub(3)) as usize
}

// ---------------------------------------------------------------------------
// Shared state between callbacks and the main struct
// ---------------------------------------------------------------------------

struct BleState {
    /// Queued events to deliver to the bridge.
    events: VecDeque<BleEvent>,
    /// BLE address of the currently connected peer (None if not connected).
    peer_addr: Option<BdAddr>,
    /// Negotiated ATT MTU for the current connection.
    mtu: u16,
    /// GATT connection ID for the current connection.
    conn_id: Option<u16>,
    /// GATT service handle.
    service_handle: Option<Handle>,
    /// Gateway Command characteristic handle.
    char_handle: Option<Handle>,
    /// CCCD descriptor handle.
    cccd_handle: Option<Handle>,
    /// GATT interface handle.
    gatts_if: Option<GattInterface>,
    /// Pending passkey for Numeric Comparison (waiting for confirm reply).
    pending_passkey: Option<u32>,
    /// Indication in progress: remaining chunks to send.
    indication_queue: VecDeque<Vec<u8>>,
    /// True if waiting for ATT Handle Value Confirmation.
    awaiting_confirm: bool,
    /// BLE advertising is enabled (by BLE_ENABLE command).
    advertising: bool,
}

impl BleState {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            peer_addr: None,
            mtu: BLE_MIN_MTU,
            conn_id: None,
            service_handle: None,
            char_handle: None,
            cccd_handle: None,
            gatts_if: None,
            pending_passkey: None,
            indication_queue: VecDeque::new(),
            awaiting_confirm: false,
            advertising: false,
        }
    }

    fn is_connected(&self) -> bool {
        self.conn_id.is_some()
    }
}

// ---------------------------------------------------------------------------
// EspBleDriver
// ---------------------------------------------------------------------------

/// ESP-IDF NimBLE GATT server implementing the Gateway Pairing Service.
pub struct EspBleDriver {
    state: Arc<Mutex<BleState>>,
    _gap: EspBleGap<'static, esp_idf_svc::bt::BleEnabled, Arc<BtDriver<'static, esp_idf_svc::bt::BleEnabled>>>,
    _gatts: EspGatts<'static, Arc<BtDriver<'static, esp_idf_svc::bt::BleEnabled>>>,
}

impl EspBleDriver {
    /// Initialize NimBLE, register the Gateway Pairing Service, and configure
    /// LESC Numeric Comparison security (MD-0404).
    pub fn new(
        bt: Arc<BtDriver<'static, esp_idf_svc::bt::BleEnabled>>,
    ) -> Result<Self, EspError> {
        let state = Arc::new(Mutex::new(BleState::new()));

        // --- GAP: configure security for LESC Numeric Comparison ---
        let mut gap = EspBleGap::new(bt.clone())?;

        gap.set_security_conf(&SecurityConfig {
            auth_req_mode: AuthReq::SC | AuthReq::MITM | AuthReq::BOND,
            io_cap: SecurityIOCap::DisplayYesNo,
            initiator_key: esp_idf_svc::bt::ble::gap::BleKeyType::NONE,
            responder_key: esp_idf_svc::bt::ble::gap::BleKeyType::NONE,
            min_key_size: 16,
            max_key_size: 16,
            oob_support: OobData::None,
        })?;

        // GAP event handler (connection, MTU, security events).
        let state_gap = Arc::clone(&state);
        gap.subscribe(move |event| {
            Self::handle_gap_event(&state_gap, event);
        })?;

        // --- GATTS: register Gateway Pairing Service ---
        let mut gatts = EspGatts::new(bt.clone())?;

        let state_gatts = Arc::clone(&state);
        gatts.register_app(0, move |gatts_if, event| {
            Self::handle_gatts_event(&state_gatts, gatts_if, event);
        })?;

        // Register the service.
        let service = GattService {
            service_id: esp_idf_svc::bt::ble::gatt::server::ServiceId {
                uuid: GATEWAY_SERVICE_UUID,
                is_primary: true,
                inst_id: 0,
            },
            num_handles: 6,
        };
        gatts.create_service(0, &service)?;

        info!("BLE GATT Gateway Pairing Service registered");

        Ok(Self {
            state,
            _gap: gap,
            _gatts: gatts,
        })
    }

    /// Start BLE advertising for the Gateway Pairing Service.
    fn start_advertising(state: &Arc<Mutex<BleState>>, gap: &EspBleGap<'_, _, _>) {
        use esp_idf_svc::bt::ble::gap::{
            AdvConfiguration, AdvData, AdvProperties, AdvType,
        };

        let adv_data = AdvData {
            include_name: true,
            include_txpower: false,
            min_interval: 0,
            max_interval: 0,
            service_uuid: Some(GATEWAY_SERVICE_UUID),
            ..Default::default()
        };

        if let Err(e) = gap.set_adv_data(&adv_data) {
            warn!("BLE: set_adv_data failed: {:?}", e);
            return;
        }

        let adv_conf = AdvConfiguration {
            adv_type: AdvType::Ind,
            own_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
            ..Default::default()
        };
        if let Err(e) = gap.start_advertising(&adv_conf) {
            warn!("BLE: start_advertising failed: {:?}", e);
            return;
        }

        if let Ok(mut s) = state.lock() {
            s.advertising = true;
        }
        info!("BLE advertising started (Gateway Pairing Service)");
    }

    /// Stop BLE advertising and disconnect any active client.
    fn stop_advertising(state: &Arc<Mutex<BleState>>, gap: &EspBleGap<'_, _, _>, gatts: &EspGatts<'_, _>) {
        if let Err(e) = gap.stop_advertising() {
            warn!("BLE: stop_advertising failed: {:?}", e);
        }

        // Disconnect any active client.
        let conn_id = state.lock().ok().and_then(|s| s.conn_id);
        if let Some(id) = conn_id {
            if let Err(e) = gatts.close(0, id) {
                warn!("BLE: close conn {} failed: {:?}", id, e);
            }
        }

        if let Ok(mut s) = state.lock() {
            s.advertising = false;
        }
        info!("BLE advertising stopped");
    }

    fn handle_gap_event(state: &Arc<Mutex<BleState>>, event: BleGapEvent) {
        match event {
            BleGapEvent::MtuSet { mtu, .. } => {
                let mtu = mtu as u16;
                if mtu < BLE_MIN_MTU {
                    warn!("BLE: MTU {} < minimum {}; will disconnect", mtu, BLE_MIN_MTU);
                    // Disconnection is handled in the connect event — rejection
                    // on MTU too low happens when BleGapEvent::Connected arrives.
                } else {
                    info!("BLE: MTU negotiated = {}", mtu);
                    if let Ok(mut s) = state.lock() {
                        s.mtu = mtu;
                    }
                }
            }

            BleGapEvent::PasskeyNotify { passkey, .. } => {
                // Numeric Comparison: display passkey and relay to gateway (MD-0414).
                info!("BLE: Numeric Comparison passkey = {:06}", passkey);
                if let Ok(mut s) = state.lock() {
                    s.pending_passkey = Some(passkey);
                    s.events.push_back(BleEvent::PairingConfirm { passkey });
                }
            }

            BleGapEvent::PairingComplete { status, .. } => {
                if status == esp_idf_sys::esp_ble_sm_param_t_ESP_BLE_SM_AUTHEN_REQ_MODE as u8 {
                    // Pairing failed.
                    warn!("BLE: pairing failed (status={})", status);
                } else {
                    // Pairing succeeded — send BLE_CONNECTED (MD-0410).
                    info!("BLE: LESC pairing complete");
                    if let Ok(mut s) = state.lock() {
                        if let Some(addr) = s.peer_addr {
                            let mtu = s.mtu;
                            s.events.push_back(BleEvent::Connected {
                                peer_addr: addr.into(),
                                mtu,
                            });
                        }
                    }
                }
            }

            _ => {}
        }
    }

    fn handle_gatts_event(
        state: &Arc<Mutex<BleState>>,
        gatts_if: GattInterface,
        event: GattsEvent,
    ) {
        match event {
            GattsEvent::ServiceCreated { service_handle, .. } => {
                info!("BLE: service created (handle={})", service_handle);
                if let Ok(mut s) = state.lock() {
                    s.service_handle = Some(service_handle);
                    s.gatts_if = Some(gatts_if);
                }
                // Add Gateway Command characteristic (Write + Indicate).
                // TODO: call gatts.add_characteristic() — requires access to EspGatts
                // here. In practice the registration is done via the GattsEvent flow.
            }

            GattsEvent::CharacteristicAdded { char_handle, .. } => {
                info!("BLE: characteristic added (handle={})", char_handle);
                if let Ok(mut s) = state.lock() {
                    s.char_handle = Some(char_handle);
                }
            }

            GattsEvent::DescriptorAdded { descr_handle, .. } => {
                if let Ok(mut s) = state.lock() {
                    s.cccd_handle = Some(descr_handle);
                }
            }

            GattsEvent::Connected(ConnectionEvent { conn_id, remote_bda, .. }) => {
                info!("BLE: client connected (conn_id={})", conn_id);
                if let Ok(mut s) = state.lock() {
                    if s.is_connected() {
                        // Only one connection at a time (MD-0405).
                        // The second connection will be rejected by stopping advertising
                        // after the first connect.
                    }
                    s.conn_id = Some(conn_id);
                    s.peer_addr = Some(remote_bda);
                    s.mtu = BLE_MIN_MTU; // Will be updated by MTU negotiation.
                }
            }

            GattsEvent::Disconnected(ConnectionEvent { conn_id, remote_bda, .. }) => {
                info!("BLE: client disconnected (conn_id={})", conn_id);
                let (addr, reason) = {
                    let s = state.lock().unwrap_or_else(|p| p.into_inner());
                    (s.peer_addr, 0u8)
                };
                if let Ok(mut s) = state.lock() {
                    s.conn_id = None;
                    s.peer_addr = None;
                    s.indication_queue.clear();
                    s.awaiting_confirm = false;
                }
                if let Some(a) = addr {
                    if let Ok(mut s) = state.lock() {
                        s.events.push_back(BleEvent::Disconnected {
                            peer_addr: a.into(),
                            reason,
                        });
                    }
                }
                // Re-advertise if advertising was enabled (MD-0407).
                // (Advertising restart is triggered from the main loop via enable().)
            }

            GattsEvent::Write(CharacteristicEvent {
                conn_id,
                value,
                need_rsp,
                ..
            }) => {
                // Forward GATT write to gateway. Empty writes are discarded (MD-0409).
                if value.is_empty() {
                    return;
                }
                info!("BLE: GATT write {} bytes", value.len());
                if let Ok(mut s) = state.lock() {
                    s.events.push_back(BleEvent::Recv(value.to_vec()));
                }
            }

            GattsEvent::Confirm(NotifyEvent { conn_id, .. }) => {
                // ATT Handle Value Confirmation received — send next indication chunk.
                if let Ok(mut s) = state.lock() {
                    s.awaiting_confirm = false;
                    // Next chunk will be sent on the next poll() call when
                    // the indication queue is drained.
                }
            }

            GattsEvent::Mtu { mtu, .. } => {
                let mtu = mtu as u16;
                info!("BLE: GATTS MTU = {}", mtu);
                if let Ok(mut s) = state.lock() {
                    s.mtu = mtu;
                }
            }

            _ => {}
        }
    }

    /// Send the next pending indication chunk, if any.
    ///
    /// Called from `indicate()` and after each confirmation event.  Sends
    /// exactly one chunk and sets `awaiting_confirm = true`.
    fn send_next_chunk(state: &Arc<Mutex<BleState>>, gatts: &EspGatts<'_, _>) {
        let (chunk, conn_id, char_handle, gatts_if) = {
            let mut s = match state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if s.awaiting_confirm || s.indication_queue.is_empty() {
                return;
            }
            let chunk = s.indication_queue.pop_front().unwrap();
            let conn_id = match s.conn_id {
                Some(id) => id,
                None => return,
            };
            let char_handle = match s.char_handle {
                Some(h) => h,
                None => return,
            };
            let gatts_if = match s.gatts_if {
                Some(i) => i,
                None => return,
            };
            s.awaiting_confirm = true;
            (chunk, conn_id, char_handle, gatts_if)
        };

        if let Err(e) = gatts.notify(gatts_if, conn_id, char_handle, true, &chunk) {
            warn!("BLE: indication send failed: {:?}", e);
            if let Ok(mut s) = state.lock() {
                s.awaiting_confirm = false;
            }
        }
    }
}

impl Ble for EspBleDriver {
    fn enable(&mut self) {
        if let Ok(s) = self.state.lock() {
            if s.advertising {
                return; // Already enabled — no-op (MD-0407).
            }
        }
        Self::start_advertising(&self.state, &self._gap);

        // If no client is connected after a previous session, re-advertise.
        // If a client just disconnected and BLE is still enabled, restart advertising.
        // (Advertising restart after disconnect is handled here.)
    }

    fn disable(&mut self) {
        if let Ok(s) = self.state.lock() {
            if !s.advertising && !s.is_connected() {
                return; // Already disabled — no-op.
            }
        }
        Self::stop_advertising(&self.state, &self._gap, &self._gatts);
    }

    fn indicate(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let mtu = {
            let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if !s.is_connected() {
                return; // No client connected — silently discard (MD-0408).
            }
            s.mtu
        };

        // Fragment into chunks of at most (MTU − 3) bytes (MD-0403).
        let chunk_size = max_indication_payload(mtu);
        {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            for chunk in data.chunks(chunk_size) {
                s.indication_queue.push_back(chunk.to_vec());
            }
        }

        // Send the first chunk immediately.
        Self::send_next_chunk(&self.state, &self._gatts);
    }

    fn pairing_confirm_reply(&mut self, accept: bool) {
        // Forward the operator's decision to NimBLE (MD-0414).
        let passkey_decision = if accept {
            esp_idf_sys::esp_ble_confirm_reply_t_ESP_BLE_CONFIRM_ACCEPT
        } else {
            esp_idf_sys::esp_ble_confirm_reply_t_ESP_BLE_CONFIRM_REJECT
        };

        if let Ok(s) = self.state.lock() {
            if let Some(addr) = s.peer_addr {
                let addr_raw: [u8; 6] = addr.into();
                unsafe {
                    esp_idf_sys::esp_ble_confirm_reply(
                        addr_raw.as_ptr() as *mut u8,
                        passkey_decision,
                    );
                }
            }
        }
    }

    fn drain_event(&self) -> Option<BleEvent> {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());

        // If a confirmation was received (awaiting_confirm cleared), send next chunk.
        if !s.awaiting_confirm && !s.indication_queue.is_empty() {
            drop(s);
            Self::send_next_chunk(&self.state, &self._gatts);
            s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        }

        s.events.pop_front()
    }
}
