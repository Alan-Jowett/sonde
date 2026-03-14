// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW driver: WiFi station mode init, ESP-NOW send/receive, channel
//! configuration, and channel scanning.

use esp_idf_hal::modem::Modem;
use esp_idf_svc::espnow::{EspNow, PeerInfo, SendStatus};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use log::{info, warn};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sonde_protocol::modem::{RecvFrame, MAC_SIZE};

use crate::bridge::Radio;
use crate::peer_table::PeerTable;
use crate::status::ModemCounters;

/// Shared state for the raw ESP-NOW receive callback.
struct RecvCallbackState {
    rx_queue: Arc<Mutex<VecDeque<RecvFrame>>>,
    usb_connected: Arc<AtomicBool>,
}

/// Global callback state — set once during `EspNowDriver::new()`.
static RECV_CB_STATE: std::sync::OnceLock<RecvCallbackState> = std::sync::OnceLock::new();

/// Raw ESP-NOW receive callback that extracts RSSI from `rx_ctrl`.
///
/// This bypasses `esp-idf-svc`'s `register_recv_cb` because `ReceiveInfo`
/// in v0.50 does not expose the `rx_ctrl` field containing RSSI.
unsafe extern "C" fn raw_recv_cb(
    recv_info: *const esp_idf_sys::esp_now_recv_info_t,
    data: *const u8,
    data_len: core::ffi::c_int,
) {
    // Defensive guards: ESP-IDF guarantees valid pointers but we check
    // to avoid UB if the contract is ever violated.
    if recv_info.is_null() || data.is_null() || data_len <= 0 {
        return;
    }

    let info = unsafe { &*recv_info };

    if info.src_addr.is_null() {
        return;
    }

    let src_addr = unsafe { &*(info.src_addr as *const [u8; 6]) };

    // Guard against invalid length — ESP-NOW max payload is 250 bytes.
    let len = data_len as usize;
    if len > 250 {
        return;
    }
    let payload = unsafe { core::slice::from_raw_parts(data, len) };

    // Extract RSSI from the rx_ctrl metadata.
    let rssi = if info.rx_ctrl.is_null() {
        i8::MIN
    } else {
        unsafe { (*info.rx_ctrl).rssi() as i8 }
    };

    if let Some(state) = RECV_CB_STATE.get() {
        // Discard frames when USB is disconnected (MD-0301).
        if !state.usb_connected.load(Ordering::Relaxed) {
            return;
        }
        // Use try_lock to avoid blocking the ESP-NOW/WiFi task if the
        // queue is being drained. Drop the frame if contended.
        // Also handle poisoned mutex — recover the guard so frames
        // aren't silently dropped forever after a panic in the consumer.
        let mut guard = match state.rx_queue.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::WouldBlock) => return,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        };
        if guard.len() < 64 {
            guard.push_back(RecvFrame {
                peer_mac: *src_addr,
                rssi,
                frame_data: payload.to_vec(),
            });
        }
    }
}

/// Wraps ESP-NOW initialization, send, receive, and channel management.
pub struct EspNowDriver {
    wifi: BlockingWifi<EspWifi<'static>>,
    espnow: EspNow<'static>,
    peer_table: PeerTable,
    counters: Arc<ModemCounters>,
    rx_queue: Arc<Mutex<VecDeque<RecvFrame>>>,
    current_channel: u8,
    /// Set to true after the first poisoned-mutex warning to avoid log spam.
    poison_warned: AtomicBool,
}

impl EspNowDriver {
    pub fn new(
        modem: Modem,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        counters: &Arc<ModemCounters>,
        usb_connected: Arc<AtomicBool>,
    ) -> Result<Self, esp_idf_sys::EspError> {
        // Guard: fail fast before touching any ESP-IDF state if the driver
        // was already constructed (OnceLock is already populated).
        if RECV_CB_STATE.get().is_some() {
            return Err(esp_idf_sys::EspError::from_non_zero(
                core::num::NonZeroI32::new(esp_idf_sys::ESP_ERR_INVALID_STATE).unwrap(),
            ));
        }

        // Initialize WiFi in station mode (required for ESP-NOW).
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)?;

        wifi.start()?;
        info!("WiFi started in station mode");

        let espnow = EspNow::take()?;
        let rx_queue = Arc::new(Mutex::new(VecDeque::with_capacity(64)));

        // Register callbacks before setting RECV_CB_STATE so that a
        // failure in any registration does not leave the OnceLock
        // permanently populated (it cannot be cleared once set).
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_now_register_recv_cb(Some(raw_recv_cb)))?;
        }

        // Register the send callback to track delivery failures (MD-0202).
        let counters_for_send = Arc::clone(counters);
        espnow.register_send_cb(move |_mac, status| {
            if matches!(status, SendStatus::FAIL) {
                counters_for_send.inc_tx_fail();
            }
        })?;

        // All fallible init is done — install recv callback state last.
        // The early guard above ensures this is the first call, so set()
        // should always succeed. The recv callback harmlessly drops any
        // frames that arrived before this point.
        if RECV_CB_STATE
            .set(RecvCallbackState {
                rx_queue: Arc::clone(&rx_queue),
                usb_connected,
            })
            .is_err()
        {
            return Err(esp_idf_sys::EspError::from_non_zero(
                core::num::NonZeroI32::new(esp_idf_sys::ESP_ERR_INVALID_STATE).unwrap(),
            ));
        }

        info!("ESP-NOW initialized on channel 1");

        Ok(Self {
            wifi,
            espnow,
            peer_table: PeerTable::new(),
            counters: Arc::clone(counters),
            rx_queue,
            current_channel: 1,
            poison_warned: AtomicBool::new(false),
        })
    }

    /// Remove all peers from both the ESP-NOW stack and the local table.
    fn clear_all_peers(&mut self) {
        for mac in self.peer_table.all_macs() {
            let _ = self.espnow.del_peer(mac);
        }
        self.peer_table.clear();
    }

    /// Set the WiFi/ESP-NOW channel via the raw ESP-IDF API.
    fn raw_set_channel(&self, channel: u8) -> Result<(), esp_idf_sys::EspError> {
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_wifi_set_channel(
                channel,
                esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE
            ))
        }
    }
}

impl Radio for EspNowDriver {
    /// Send an ESP-NOW frame to the specified peer MAC.
    /// Auto-registers the peer if not already in the peer table.
    fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) {
        // Auto-register peer if needed.
        if let Some(evicted) = self.peer_table.ensure_peer(peer_mac) {
            let _ = self.espnow.del_peer(evicted);
        }
        if !self.espnow.peer_exists(*peer_mac).unwrap_or(false) {
            let peer_info = PeerInfo {
                peer_addr: *peer_mac,
                channel: self.current_channel,
                ..Default::default()
            };
            if let Err(e) = self.espnow.add_peer(peer_info) {
                warn!("failed to add ESP-NOW peer: {:?}", e);
            }
        }

        self.counters.inc_tx();
        if let Err(e) = self.espnow.send(*peer_mac, data) {
            warn!("esp_now_send failed: {:?}", e);
        }
    }

    /// Drain one received frame from the queue.
    fn drain_one(&self) -> Option<RecvFrame> {
        match self.rx_queue.lock() {
            Ok(mut q) => q.pop_front(),
            Err(poisoned) => {
                if !self.poison_warned.swap(true, Ordering::Relaxed) {
                    warn!("rx_queue mutex poisoned, recovering");
                }
                poisoned.into_inner().pop_front()
            }
        }
    }

    /// Set the WiFi/ESP-NOW channel. Clears the peer table and removes
    /// all peers from the ESP-NOW stack.
    fn set_channel(&mut self, channel: u8) -> Result<(), &'static str> {
        if channel == 0 || channel > 14 {
            return Err("invalid channel");
        }
        self.raw_set_channel(channel)
            .map_err(|_| "ESP-IDF set_channel failed")?;

        self.clear_all_peers();
        self.current_channel = channel;
        info!("channel set to {}", channel);
        Ok(())
    }

    /// Get the current channel.
    fn channel(&self) -> u8 {
        self.current_channel
    }

    /// Perform a WiFi AP scan across all channels and return per-channel results.
    /// Restores ESP-NOW on `current_channel` after the scan completes.
    fn scan_channels(&mut self) -> Vec<(u8, u8, i8)> {
        let scan_result = self.wifi.scan().unwrap_or_default();
        // Use i8::MIN as sentinel for "no APs seen on this channel".
        let mut channels = [(0u16, i8::MIN); 15];

        for ap in &scan_result {
            let ch = ap.channel as usize;
            if ch >= 1 && ch <= 14 {
                channels[ch].0 = channels[ch].0.saturating_add(1);
                if ap.signal_strength > channels[ch].1 {
                    channels[ch].1 = ap.signal_strength;
                }
            }
        }

        // Restore the WiFi channel after scanning (scanning disrupts ESP-NOW).
        if let Err(e) = self.raw_set_channel(self.current_channel) {
            warn!(
                "failed to restore channel {} after scan: {:?}",
                self.current_channel, e
            );
        }

        (1..=14)
            .map(|ch| {
                let count = core::cmp::min(channels[ch].0, 255) as u8;
                // Per spec: strongest_rssi is 0 if no APs detected.
                let rssi = if count == 0 { 0 } else { channels[ch].1 };
                (ch as u8, count, rssi)
            })
            .collect()
    }

    /// Get the modem's own MAC address.
    fn mac_address(&self) -> [u8; MAC_SIZE] {
        let mut mac = [0u8; MAC_SIZE];
        unsafe {
            let _ = esp_idf_sys::esp!(esp_idf_sys::esp_wifi_get_mac(
                esp_idf_sys::wifi_interface_t_WIFI_IF_STA,
                mac.as_mut_ptr()
            ));
        }
        mac
    }

    /// Reset ESP-NOW state: clear peers from the stack, reset WiFi channel
    /// to 1, and drain the receive queue. Called on RESET command.
    fn reset_state(&mut self) {
        self.clear_all_peers();

        if let Err(e) = self.raw_set_channel(1) {
            warn!("failed to reset WiFi channel to 1: {:?}", e);
        }
        self.current_channel = 1;

        if let Ok(mut q) = self.rx_queue.lock() {
            q.clear();
        }
        info!("ESP-NOW re-initialized on channel 1");
    }
}
