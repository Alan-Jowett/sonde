// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW transport for the sensor node.
//!
//! The node communicates with the gateway over ESP-NOW using broadcast
//! frames (destination MAC `FF:FF:FF:FF:FF:FF`). Received frames are
//! buffered in a shared queue populated by an ESP-NOW receive callback.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use esp_idf_hal::modem::Modem;
use esp_idf_svc::espnow::{EspNow, PeerInfo};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};

use sonde_protocol::modem::ESPNOW_MAX_DATA_SIZE;

use crate::error::{NodeError, NodeResult};

/// Broadcast MAC used for all node → gateway transmissions.
const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

/// Shared state for the raw ESP-NOW receive callback.
struct RecvState {
    rx_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
    condvar: Arc<Condvar>,
}

/// Global callback state — set once during [`EspNowTransport::new`].
static RECV_STATE: std::sync::OnceLock<RecvState> = std::sync::OnceLock::new();

/// Raw ESP-NOW receive callback — pushes frame data to the shared queue.
///
/// Uses `try_lock` to avoid blocking the ESP-NOW/WiFi task and caps
/// the queue at 64 entries to bound memory usage.
unsafe extern "C" fn raw_recv_cb(
    recv_info: *const esp_idf_sys::esp_now_recv_info_t,
    data: *const u8,
    data_len: core::ffi::c_int,
) {
    if recv_info.is_null() || data.is_null() || data_len <= 0 {
        return;
    }
    let len = data_len as usize;
    if len > ESPNOW_MAX_DATA_SIZE {
        return;
    }
    let payload = unsafe { core::slice::from_raw_parts(data, len) };
    if let Some(state) = RECV_STATE.get() {
        if let Ok(mut q) = state.rx_queue.try_lock() {
            if q.len() < 64 {
                q.push_back(payload.to_vec());
                state.condvar.notify_one();
            } else {
                log::warn!("ESP-NOW recv queue full, dropping frame");
            }
        }
    }
}

/// ESP-NOW transport backed by `esp-idf-svc`.
///
/// Holds the WiFi + ESP-NOW handles for the lifetime of the transport
/// and maintains a receive queue filled by a global callback.
pub struct EspNowTransport {
    _wifi: BlockingWifi<EspWifi<'static>>,
    espnow: EspNow<'static>,
    rx_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
    rx_condvar: Arc<Condvar>,
}

impl EspNowTransport {
    /// Initialise WiFi in STA mode and start ESP-NOW.
    ///
    /// Sets the WiFi channel to `channel` before starting ESP-NOW so that
    /// the node communicates on the same channel as the gateway. Registers
    /// a broadcast peer and installs the raw receive callback.
    /// Must only be called once per process (the global `RECV_STATE` is
    /// a `OnceLock`).
    pub fn new(
        modem: Modem,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        channel: u8,
    ) -> Result<Self, NodeError> {
        if channel < 1 || channel > 13 {
            return Err(NodeError::Transport(
                "invalid WiFi channel (must be 1–13)",
            ));
        }

        // WiFi STA mode (required for ESP-NOW)
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))
            .map_err(|_| NodeError::Transport("WiFi init failed"))?;
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)
            .map_err(|_| NodeError::Transport("WiFi wrap failed"))?;
        wifi.start()
            .map_err(|_| NodeError::Transport("WiFi start failed"))?;

        // Set the WiFi channel before ESP-NOW init so the node and gateway
        // communicate on the same channel.
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_wifi_set_channel(
                channel,
                esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE,
            ))
            .map_err(|_| NodeError::Transport("set WiFi channel failed"))?;
        }

        let espnow =
            EspNow::take().map_err(|_| NodeError::Transport("ESP-NOW init failed"))?;

        // Register broadcast peer (channel = 0 means "use current WiFi channel")
        let peer_info = PeerInfo {
            peer_addr: BROADCAST_MAC,
            channel: 0,
            ..Default::default()
        };
        espnow
            .add_peer(peer_info)
            .map_err(|_| NodeError::Transport("add ESP-NOW peer failed"))?;

        // Set up receive callback
        let rx_queue = Arc::new(Mutex::new(VecDeque::with_capacity(16)));
        let rx_condvar = Arc::new(Condvar::new());
        RECV_STATE
            .set(RecvState {
                rx_queue: Arc::clone(&rx_queue),
                condvar: Arc::clone(&rx_condvar),
            })
            .map_err(|_| NodeError::Transport("recv callback already registered"))?;
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_now_register_recv_cb(Some(raw_recv_cb)))
                .map_err(|_| NodeError::Transport("register recv callback failed"))?;
        }

        Ok(Self {
            _wifi: wifi,
            espnow,
            rx_queue,
            rx_condvar,
        })
    }
}

impl crate::traits::Transport for EspNowTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        if frame.len() > ESPNOW_MAX_DATA_SIZE {
            return Err(NodeError::Transport("frame too large"));
        }
        self.espnow
            .send(BROADCAST_MAC, frame)
            .map_err(|_| NodeError::Transport("ESP-NOW send failed"))
    }

    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
        let mut q = self
            .rx_queue
            .lock()
            .map_err(|_| NodeError::Transport("rx_queue lock poisoned"))?;
        loop {
            if let Some(frame) = q.pop_front() {
                return Ok(Some(frame));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline - now;
            let (guard, _timeout_result) = self
                .rx_condvar
                .wait_timeout(q, remaining)
                .map_err(|_| NodeError::Transport("rx_queue lock poisoned"))?;
            q = guard;
        }
    }
}
