// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW driver: WiFi station mode init, ESP-NOW send/receive, channel
//! configuration, and channel scanning.

use esp_idf_hal::modem::Modem;
use esp_idf_svc::espnow::{EspNow, PeerInfo, ReceiveInfo, SendStatus};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use log::{info, warn};
use std::sync::{Arc, Mutex};

use sonde_protocol::modem::{RecvFrame, MAC_SIZE};

use crate::bridge::Radio;
use crate::peer_table::PeerTable;
use crate::status::ModemCounters;

/// Wraps ESP-NOW initialization, send, receive, and channel management.
pub struct EspNowDriver {
    wifi: BlockingWifi<EspWifi<'static>>,
    espnow: EspNow<'static>,
    peer_table: PeerTable,
    counters: Arc<ModemCounters>,
    rx_queue: Arc<Mutex<Vec<RecvFrame>>>,
    current_channel: u8,
}

impl EspNowDriver {
    pub fn new(
        modem: Modem,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        counters: &Arc<ModemCounters>,
    ) -> Self {
        // Initialize WiFi in station mode (required for ESP-NOW).
        let esp_wifi =
            EspWifi::new(modem, sysloop.clone(), Some(nvs)).expect("failed to create WiFi");
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop).expect("failed to wrap WiFi");

        wifi.start().expect("failed to start WiFi");
        info!("WiFi started in station mode");

        let espnow = EspNow::take().expect("failed to take ESP-NOW");
        let rx_queue = Arc::new(Mutex::new(Vec::new()));
        let rx_clone = Arc::clone(&rx_queue);

        // Register the receive callback.
        espnow
            .register_recv_cb(move |info: &ReceiveInfo, data: &[u8]| {
                let peer_mac = *info.src_addr;

                let frame = RecvFrame {
                    peer_mac,
                    // TODO: Extract real RSSI from esp_now_recv_info_t when
                    // esp-idf-svc exposes it in the recv callback signature.
                    // i8::MIN (−128) signals "not available" — outside the
                    // typical −30..−90 dBm range so it won't be confused
                    // with a real measurement.
                    rssi: i8::MIN,
                    frame_data: data.to_vec(),
                };

                if let Ok(mut q) = rx_clone.lock() {
                    // Cap the queue to prevent unbounded memory growth
                    // if USB is disconnected or the host can't keep up.
                    if q.len() < 64 {
                        q.push(frame);
                    }
                    // rx_count is incremented by the bridge when the frame
                    // is actually forwarded to USB (per MD-0303).
                }
            })
            .expect("failed to register ESP-NOW recv callback");

        // Register the send callback to track delivery failures (MD-0202).
        let counters_for_send = Arc::clone(counters);
        espnow
            .register_send_cb(move |_mac, status| {
                if matches!(status, SendStatus::FAIL) {
                    counters_for_send.inc_tx_fail();
                }
            })
            .expect("failed to register ESP-NOW send callback");

        info!("ESP-NOW initialized on channel 1");

        Self {
            wifi,
            espnow,
            peer_table: PeerTable::new(),
            counters: Arc::clone(counters),
            rx_queue,
            current_channel: 1,
        }
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

    /// Drain received frames from the queue.
    fn drain_rx(&self) -> Vec<RecvFrame> {
        if let Ok(mut q) = self.rx_queue.lock() {
            std::mem::take(&mut *q)
        } else {
            Vec::new()
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
