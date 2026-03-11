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
use std::sync::{Arc, Mutex};

use sonde_protocol::modem::{RecvFrame, MAC_SIZE};

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
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))
            .expect("failed to create WiFi");
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)
            .expect("failed to wrap WiFi");

        wifi.start().expect("failed to start WiFi");
        info!("WiFi started in station mode");

        let espnow = EspNow::take().expect("failed to take ESP-NOW");
        let rx_queue = Arc::new(Mutex::new(Vec::new()));
        let counters_clone = Arc::clone(counters);
        let rx_clone = Arc::clone(&rx_queue);

        // Register the receive callback.
        espnow
            .register_recv_cb(move |mac, data| {
                let mut peer_mac = [0u8; MAC_SIZE];
                peer_mac.copy_from_slice(mac);

                let frame = RecvFrame {
                    peer_mac,
                    rssi: 0, // RSSI is available from esp_now_recv_info_t in newer APIs
                    frame_data: data.to_vec(),
                };

                if let Ok(mut q) = rx_clone.lock() {
                    q.push(frame);
                }
                counters_clone.inc_rx();
            })
            .expect("failed to register ESP-NOW recv callback");

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

    /// Send an ESP-NOW frame to the specified peer MAC.
    /// Auto-registers the peer if not already in the peer table.
    pub fn send(&mut self, peer_mac: &[u8; MAC_SIZE], data: &[u8]) {
        // Auto-register peer if needed.
        if let Some(evicted) = self.peer_table.ensure_peer(peer_mac) {
            let _ = self.espnow.del_peer(&evicted);
        }
        if !self.espnow.peer_exists(&peer_mac).unwrap_or(false) {
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
        match self.espnow.send(peer_mac, data) {
            Ok(SendStatus::SUCCESS) => {}
            _ => {
                self.counters.inc_tx_fail();
            }
        }
    }

    /// Drain received frames from the queue.
    pub fn drain_rx(&self) -> Vec<RecvFrame> {
        if let Ok(mut q) = self.rx_queue.lock() {
            std::mem::take(&mut *q)
        } else {
            Vec::new()
        }
    }

    /// Set the WiFi/ESP-NOW channel. Clears the peer table and removes
    /// all peers from the ESP-NOW stack.
    pub fn set_channel(&mut self, channel: u8) -> Result<(), ()> {
        if channel == 0 || channel > 14 {
            return Err(());
        }
        self.wifi
            .wifi()
            .set_channel(channel)
            .map_err(|_| ())?;

        // Remove all peers from the ESP-NOW stack and local table.
        self.clear_all_peers();
        self.current_channel = channel;
        info!("channel set to {}", channel);
        Ok(())
    }

    /// Get the current channel.
    pub fn channel(&self) -> u8 {
        self.current_channel
    }

    /// Perform a WiFi AP scan across all channels and return per-channel results.
    pub fn scan_channels(&mut self) -> Vec<(u8, u8, i8)> {
        let scan_result = self.wifi.scan().unwrap_or_default();
        let mut channels = [(0u16, 0i8); 15]; // index 1-14: (count, strongest_rssi)

        for ap in &scan_result {
            let ch = ap.channel as usize;
            if ch >= 1 && ch <= 14 {
                channels[ch].0 = channels[ch].0.saturating_add(1);
                if channels[ch].1 == 0 || ap.signal_strength > channels[ch].1 {
                    channels[ch].1 = ap.signal_strength;
                }
            }
        }

        (1..=14)
            .map(|ch| {
                let count = core::cmp::min(channels[ch].0, 255) as u8;
                (ch as u8, count, channels[ch].1)
            })
            .collect()
    }

    /// Get the modem's own MAC address.
    pub fn mac_address(&self) -> [u8; MAC_SIZE] {
        self.wifi.wifi().sta_netif().mac().unwrap_or([0u8; MAC_SIZE])
    }

    /// De-initialize and re-initialize ESP-NOW (used during RESET).
    pub fn reinit(&mut self) {
        // Remove all peers from the ESP-NOW stack.
        self.clear_all_peers();

        // Reset WiFi channel back to 1.
        if let Err(e) = self.wifi.wifi().set_channel(1) {
            warn!("failed to reset WiFi channel to 1: {:?}", e);
        }
        self.current_channel = 1;

        // Clear the RX queue.
        if let Ok(mut q) = self.rx_queue.lock() {
            q.clear();
        }
        info!("ESP-NOW re-initialized on channel 1");
    }

    /// Remove all peers from both the ESP-NOW stack and the local table.
    fn clear_all_peers(&mut self) {
        // Remove each tracked peer from the ESP-NOW stack.
        for mac in self.peer_table.all_macs() {
            let _ = self.espnow.del_peer(&mac);
        }
        self.peer_table.clear();
    }
}
