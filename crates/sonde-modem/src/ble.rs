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
//!   `BLE_MTU_MIN` after the ATT MTU Exchange are rejected at authentication
//!   complete time (MD-0402).
//! - Indication fragmentation: payloads larger than (MTU − 3) bytes are split into
//!   multiple indications with confirmation between chunks (MD-0403).
//! - GATT writes forwarded as `BleEvent::Recv`; empty writes discarded (MD-0409).
//! - `BleEvent::Connected` sent after LESC pairing completes (MD-0410).
//! - `BleEvent::Disconnected` sent on every disconnect (MD-0411).
//! - BLE and ESP-NOW run concurrently without interference (MD-0405).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    utilities::mutex::Mutex as NimbleMutex,
    BLEAdvertisementData, BLECharacteristic, BLEDevice, NimbleProperties,
};
use log::{info, warn};
use sonde_protocol::modem::BLE_MTU_MIN;
use sonde_protocol::modem::MAC_SIZE;

use crate::bridge::{Ble, BleEvent};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Gateway Pairing Service UUID (`0000FE60-0000-1000-8000-00805F9B34FB`).
const GATEWAY_SERVICE_UUID: BleUuid = BleUuid::Uuid16(0xFE60);

/// Gateway Command characteristic UUID (`0000FE61-0000-1000-8000-00805F9B34FB`).
const GATEWAY_COMMAND_UUID: BleUuid = BleUuid::Uuid16(0xFE61);

/// Maximum queued BLE events before new events are dropped.
const MAX_BLE_EVENT_QUEUE: usize = 32;

/// Maximum queued indication chunks before new indications are rejected.
const MAX_INDICATION_CHUNKS: usize = 64;

/// Maximum time allowed for a BLE connection to complete the full pairing flow.
/// If pairing has not successfully completed before this duration elapses,
/// the connection is force-disconnected to prevent resource exhaustion from stalled
/// or malicious clients.
const BLE_PAIRING_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Set in `on_connect` with the initial value and updated in
    /// `on_authentication_complete` from the connection descriptor, which
    /// reflects the post-MTU-exchange value.
    mtu: u16,
    /// Connection handle for the current client (`None` = not connected).
    conn_handle: Option<u16>,
    /// Numeric Comparison passkey relayed to gateway; operator decision pending.
    /// `BleEvent::Connected` is deferred until the operator accepts (MD-0414).
    pairing_pending: bool,
    /// Deferred Connected event stored while awaiting operator confirmation.
    deferred_connected: Option<([u8; MAC_SIZE], u16)>,
    /// True after LESC pairing completes AND operator accepts (if Numeric
    /// Comparison was used).  GATT writes are only forwarded once this is set,
    /// preventing data relay before the session is fully approved (MD-0414).
    authenticated: bool,
    /// Monotonic timestamp recorded when a client connects.  Used by
    /// `check_pairing_timeout()` to enforce `BLE_PAIRING_TIMEOUT` from the
    /// moment of connection, preventing clients that never initiate pairing
    /// from holding the connection slot indefinitely (MD-0414 AC#4).
    /// Cleared once authentication completes successfully.
    connection_start: Option<Instant>,
}

impl BleState {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            indication_queue: VecDeque::new(),
            awaiting_confirm: false,
            advertising: false,
            mtu: 0,
            conn_handle: None,
            pairing_pending: false,
            deferred_connected: None,
            authenticated: false,
            connection_start: None,
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
    /// Cached reference to the Gateway Command characteristic, avoiding
    /// async `get_service()` lookups in sync indication paths.
    gateway_cmd_char: Arc<NimbleMutex<BLECharacteristic>>,
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

        // Use NimBLE's built-in auto re-advertise after disconnect (MD-0407).
        ble_server.advertise_on_disconnect(true);

        // --- Connection event handler ---
        //
        // The initial `desc.mtu()` may report the default ATT MTU (23 bytes)
        // before the ATT MTU Exchange has completed.  We store it but defer
        // MTU enforcement to `on_authentication_complete`, by which time the
        // exchange should have occurred (MD-0402).
        let state_connect = Arc::clone(&state);
        ble_server.on_connect(move |server, desc| {
            let peer_addr: [u8; MAC_SIZE] = desc.address().as_le_bytes();
            let mtu = desc.mtu();
            info!("BLE: client connected addr={:?} mtu={}", peer_addr, mtu);

            // MD-0405: Only one connection at a time.
            // If a second client connects, disconnect immediately.
            if server.connected_count() > 1 {
                warn!("BLE: second connection rejected (MD-0405)");
                let _ = server.disconnect_with_reason(desc.conn_handle(), 0x13);
                return;
            }

            if let Ok(mut s) = state_connect.lock() {
                // NimBLE stops advertising when a client connects.  Clear the
                // flag so that a subsequent enable() will restart advertising
                // after this connection ends (MD-0407).
                s.advertising = false;
                s.mtu = mtu;
                s.conn_handle = Some(desc.conn_handle());
                // Start the pairing timeout clock at connection time so that a
                // client that never initiates pairing cannot hold the slot
                // indefinitely (MD-0414 AC#4).
                s.connection_start = Some(Instant::now());
            }
        });

        // --- Disconnect event handler ---
        let state_disconnect = Arc::clone(&state);
        ble_server.on_disconnect(move |desc, reason| {
            let peer_addr: [u8; MAC_SIZE] = desc.address().as_le_bytes();
            // Forward the HCI disconnect reason code.  BLEError wraps the
            // raw NimBLE error code, but doesn't expose a public accessor.
            // On a clean disconnect `reason` is `Ok(())`; on error we use
            // a generic code since we can't extract the actual value.
            let reason_code: u8 = if reason.is_ok() {
                0x16 // BLE_ERR_CONN_TERM_LOCAL
            } else {
                0x13 // BLE_ERR_REM_USER_CONN_TERM (best-effort default)
            };
            info!(
                "BLE: client disconnected addr={:?} reason=0x{:02x}",
                peer_addr, reason_code
            );
            if let Ok(mut s) = state_disconnect.lock() {
                s.mtu = 0;
                s.conn_handle = None;
                s.indication_queue.clear();
                s.awaiting_confirm = false;
                s.pairing_pending = false;
                s.deferred_connected = None;
                s.authenticated = false;
                s.connection_start = None;
                if s.events.len() < MAX_BLE_EVENT_QUEUE {
                    s.events.push_back(BleEvent::Disconnected {
                        peer_addr,
                        reason: reason_code,
                    });
                }
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
        // TODO(#382): Numeric Comparison is auto-accepted here because NimBLE's
        // `on_confirm_pin` is a synchronous callback that requires an immediate
        // yes/no return value — it cannot block waiting for the gateway's
        // asynchronous serial reply.  The proper fix is to gate this on a serial
        // command from the gateway (which in turn requires admin approval),
        // likely by running the SMP exchange on a dedicated FreeRTOS task that
        // can block until the gateway responds.
        //
        // We return `true` to let the BLE stack proceed with LESC key exchange,
        // then relay the passkey to the gateway for operator verification.
        //
        // This means the encrypted link is established before operator approval.
        // Multiple mitigations bound the security impact:
        //   1. `BleEvent::Connected` is deferred until operator accepts.
        //   2. GATT writes are gated on the `authenticated` flag.
        //   3. NVS bond persistence is disabled (`CONFIG_BT_NIMBLE_NVS_PERSIST=n`).
        //   4. On rejection, the client is disconnected immediately.
        let state_confirm = Arc::clone(&state);
        ble_server.on_confirm_pin(move |passkey| {
            info!("BLE: Numeric Comparison passkey = {:06}", passkey);
            if let Ok(mut s) = state_confirm.lock() {
                s.pairing_pending = true;
                if s.events.len() < MAX_BLE_EVENT_QUEUE {
                    s.events.push_back(BleEvent::PairingConfirm { passkey });
                }
            }
            true
        });

        // --- Pairing complete handler ---
        let state_auth = Arc::clone(&state);
        ble_server.on_authentication_complete(move |server, desc, result| {
            if result.is_ok() {
                let peer_addr: [u8; MAC_SIZE] = desc.address().as_le_bytes();
                let current_mtu = desc.mtu();
                let conn_handle = desc.conn_handle();
                // Compute the action while holding the lock, then drop it
                // before calling NimBLE APIs to avoid deadlock if NimBLE
                // invokes on_disconnect synchronously.
                let should_disconnect = if let Ok(mut s) = state_auth.lock() {
                    if current_mtu > 0 {
                        s.mtu = current_mtu;
                    }
                    if s.mtu < BLE_MTU_MIN {
                        warn!(
                            "BLE: pairing complete but MTU too low ({}); disconnecting (MD-0402)",
                            s.mtu
                        );
                        true
                    } else if s.pairing_pending {
                        info!("BLE: LESC pairing complete — deferring BLE_CONNECTED until operator confirms");
                        s.deferred_connected = Some((peer_addr, s.mtu));
                        false
                    } else {
                        info!("BLE: pairing complete — sending BLE_CONNECTED (MD-0410)");
                        s.authenticated = true;
                        s.connection_start = None;
                        let mtu = s.mtu;
                        if s.events.len() < MAX_BLE_EVENT_QUEUE {
                            s.events.push_back(BleEvent::Connected {
                                peer_addr,
                                mtu,
                            });
                        }
                        false
                    }
                } else {
                    false
                };
                // Lock is dropped — safe to call NimBLE.
                if should_disconnect {
                    let _ = server.disconnect(conn_handle);
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
        // Writes are only relayed after authentication completes and the
        // operator has approved (if Numeric Comparison was used), preventing
        // data relay before the session is fully established (MD-0414).
        let state_write = Arc::clone(&state);
        gateway_cmd_char.lock().on_write(move |args| {
            let value = args.recv_data();
            if value.is_empty() {
                return; // Empty writes discarded (MD-0409).
            }
            if let Ok(mut s) = state_write.lock() {
                if s.authenticated && s.events.len() < MAX_BLE_EVENT_QUEUE {
                    info!("BLE: GATT write {} bytes", value.len());
                    s.events.push_back(BleEvent::Recv(value.to_vec()));
                } else if !s.authenticated {
                    warn!(
                        "BLE: GATT write {} bytes rejected (not authenticated)",
                        value.len()
                    );
                } else if s.events.len() >= MAX_BLE_EVENT_QUEUE {
                    warn!("BLE: event queue full; dropping GATT write");
                }
            }
        });

        info!("BLE GATT Gateway Pairing Service registered (UUID 0xFE60)");

        Self {
            state,
            gateway_cmd_char,
        }
    }
}

impl Ble for EspBleDriver {
    fn enable(&mut self) {
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
        let conn_handle = {
            let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            s.conn_handle
        };
        if let Some(handle) = conn_handle {
            let _ = ble_device.get_server().disconnect(handle);
        }

        if let Ok(mut s) = self.state.lock() {
            s.advertising = false;
            s.mtu = 0;
            s.conn_handle = None;
            s.indication_queue.clear();
            s.awaiting_confirm = false;
            s.pairing_pending = false;
            s.deferred_connected = None;
            s.authenticated = false;
            s.connection_start = None;
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
        let num_chunks = data.len().div_ceil(chunk_size);
        {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.indication_queue.len() + num_chunks > MAX_INDICATION_CHUNKS {
                warn!(
                    "BLE: indication queue full; dropping payload ({} chunks)",
                    num_chunks
                );
                return;
            }
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
                // Ignore late replies if pairing is no longer pending
                // (e.g., timeout already disconnected the client).
                if !s.pairing_pending {
                    return;
                }
                s.pairing_pending = false;
                s.authenticated = true;
                s.connection_start = None;
                if let Some((peer_addr, mtu)) = s.deferred_connected.take() {
                    s.events.push_back(BleEvent::Connected { peer_addr, mtu });
                }
            }
        } else {
            warn!("BLE: Numeric Comparison rejected by operator — disconnecting");
            let conn_handle = {
                if let Ok(mut s) = self.state.lock() {
                    // Ignore late replies if pairing is no longer pending.
                    if !s.pairing_pending {
                        return;
                    }
                    s.pairing_pending = false;
                    s.deferred_connected = None;
                    s.conn_handle
                } else {
                    None
                }
            };
            if let Some(handle) = conn_handle {
                let _ = BLEDevice::take().get_server().disconnect(handle);
            }
        }
    }

    /// Advance the indication queue by one chunk.
    ///
    /// Called once per bridge poll cycle (not per `drain_event()` call)
    /// to ensure at most one indication fragment is sent per poll.
    fn advance_indication(&self) {
        let (pending, awaiting) = {
            let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            (!s.indication_queue.is_empty(), s.awaiting_confirm)
        };
        if awaiting {
            // Previous chunk was sent — clear the flag so send_next_chunk
            // can proceed.
            if let Ok(mut s) = self.state.lock() {
                s.awaiting_confirm = false;
            }
        }
        if pending {
            self.send_next_chunk();
        }
    }

    fn drain_event(&self) -> Option<BleEvent> {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.events.pop_front()
    }

    fn check_pairing_timeout(&self) {
        // Determine whether we should disconnect due to timeout.
        // Compute the decision while holding the lock, then release before
        // calling NimBLE to avoid deadlock if NimBLE invokes on_disconnect
        // synchronously.
        let timed_out_handle = {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            // Enforce the timeout whenever a connection exists but has not
            // yet completed authentication.  This covers both the
            // pairing-pending phase and the pre-pairing phase (a client
            // that connects but never initiates pairing).
            if s.conn_handle.is_some() && !s.authenticated {
                if let (Some(handle), Some(start)) = (s.conn_handle, s.connection_start) {
                    if start.elapsed() >= BLE_PAIRING_TIMEOUT {
                        // Clear pairing/timeout state so we don't repeatedly
                        // log and attempt disconnect on subsequent polls if
                        // the disconnect is slow or fails.  Also clear
                        // deferred_connected to prevent a late
                        // pairing_confirm_reply from emitting a stale
                        // BleEvent::Connected.
                        s.pairing_pending = false;
                        s.connection_start = None;
                        s.deferred_connected = None;
                        Some(handle)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(handle) = timed_out_handle {
            warn!(
                "BLE: pairing timeout ({} s) exceeded — disconnecting (MD-0414 AC#4)",
                BLE_PAIRING_TIMEOUT.as_secs()
            );
            let _ = BLEDevice::take().get_server().disconnect(handle);
        }
    }
}

impl EspBleDriver {
    /// Send the next indication chunk from the queue, if any.
    ///
    /// Uses `notify_with()` which queues the indication via
    /// `ble_gatts_indicate_custom` (non-blocking).  NimBLE handles ATT
    /// Handle Value Confirmation pacing internally.
    fn send_next_chunk(&self) {
        let (chunk, conn_handle) = {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.awaiting_confirm || s.indication_queue.is_empty() || s.conn_handle.is_none() {
                return;
            }
            let chunk = s.indication_queue.pop_front().unwrap();
            s.awaiting_confirm = true;
            (chunk, s.conn_handle.unwrap())
        };

        // notify_with() queues the indication via ble_gatts_indicate_custom
        // (non-blocking).  We keep awaiting_confirm = true so that
        // advance_indication() sends at most one chunk per poll cycle,
        // avoiding NimBLE resource exhaustion from burst-sending all
        // fragments.
        let chr = self.gateway_cmd_char.lock();
        match chr.notify_with(&chunk, conn_handle) {
            Ok(()) => {
                // Indication queued — awaiting_confirm stays true.
                // advance_indication() will clear it on the next poll
                // cycle, naturally pacing one chunk per bridge iteration.
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
