// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW transport for the sensor node.
//!
//! The node communicates with the gateway over ESP-NOW using broadcast
//! frames (destination MAC `FF:FF:FF:FF:FF:FF`). Received frames are
//! buffered in a fixed-slot ring buffer populated by an ESP-NOW receive
//! callback, eliminating per-frame heap allocation from the WiFi task
//! context.

use std::sync::atomic::{AtomicU32, Ordering};
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

/// Capacity of the receive ring buffer (number of frame slots).
const RX_RING_CAP: usize = 16;

/// A single pre-allocated frame slot in the ring buffer.
#[derive(Clone, Copy)]
struct FrameSlot {
    data: [u8; ESPNOW_MAX_DATA_SIZE],
    len: usize,
}

impl Default for FrameSlot {
    fn default() -> Self {
        Self {
            data: [0u8; ESPNOW_MAX_DATA_SIZE],
            len: 0,
        }
    }
}

/// Fixed-capacity ring buffer for received ESP-NOW frames.
///
/// All storage is pre-allocated; [`RxRing::push`] never allocates heap
/// memory, making it safe to call from the WiFi task receive callback.
struct RxRing {
    slots: [FrameSlot; RX_RING_CAP],
    head: usize,
    tail: usize,
    count: usize,
    drop_count: u32,
}

impl Default for RxRing {
    fn default() -> Self {
        Self {
            slots: [FrameSlot::default(); RX_RING_CAP],
            head: 0,
            tail: 0,
            count: 0,
            drop_count: 0,
        }
    }
}

impl RxRing {
    /// Copy `payload` into the next ring slot. Returns `false` if full.
    ///
    /// No heap allocation; safe to call from the WiFi task context.
    fn push(&mut self, payload: &[u8]) -> bool {
        if self.count >= RX_RING_CAP || payload.len() > ESPNOW_MAX_DATA_SIZE {
            return false;
        }
        let slot = &mut self.slots[self.head];
        slot.len = payload.len();
        slot.data[..payload.len()].copy_from_slice(payload);
        self.head = (self.head + 1) % RX_RING_CAP;
        self.count += 1;
        true
    }

    /// Copy the oldest frame's payload into `buf`, returning the number of
    /// bytes copied. Only `data[..len]` bytes are copied under the lock,
    /// avoiding a full 250-byte memcpy. Returns `None` if the ring is empty.
    fn pop_into(&mut self, buf: &mut [u8; ESPNOW_MAX_DATA_SIZE]) -> Option<usize> {
        if self.count == 0 {
            if self.drop_count > 0 {
                log::warn!(
                    "ESP-NOW recv ring: {} full drop(s)",
                    self.drop_count,
                );
                self.drop_count = 0;
            }
            return None;
        }
        let slot = &self.slots[self.tail];
        let len = slot.len;
        buf[..len].copy_from_slice(&slot.data[..len]);
        self.tail = (self.tail + 1) % RX_RING_CAP;
        self.count -= 1;
        Some(len)
    }
}

/// Shared state for the raw ESP-NOW receive callback.
struct RecvState {
    rx_ring: Arc<Mutex<RxRing>>,
    condvar: Arc<Condvar>,
    contention_drops: AtomicU32,
}

/// Global callback state — set once during [`EspNowTransport::new`].
static RECV_STATE: std::sync::OnceLock<RecvState> = std::sync::OnceLock::new();

/// Raw ESP-NOW receive callback — copies frame data into the ring buffer.
///
/// Uses `try_lock` to avoid blocking the ESP-NOW/WiFi task and drops
/// frames when the ring is full. No heap allocation occurs here.
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
        let enqueued = {
            if let Ok(mut ring) = state.rx_ring.try_lock() {
                if ring.push(payload) {
                    true
                } else {
                    ring.drop_count += 1;
                    false
                }
            } else {
                state.contention_drops.fetch_add(1, Ordering::Relaxed);
                false
            }
        };
        // Notify after releasing the lock to avoid waking the consumer
        // into immediate contention on the same mutex.
        if enqueued {
            state.condvar.notify_one();
        }
    }
}

/// ESP-NOW transport backed by `esp-idf-svc`.
///
/// Holds the WiFi + ESP-NOW handles for the lifetime of the transport
/// and maintains a fixed-slot ring buffer filled by a global callback.
/// No heap allocation occurs in the receive callback path.
pub struct EspNowTransport {
    _wifi: BlockingWifi<EspWifi<'static>>,
    espnow: EspNow<'static>,
    rx_ring: Arc<Mutex<RxRing>>,
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
            return Err(NodeError::Transport(format!(
                "invalid channel {}: must be 1–13",
                channel
            )));
        }

        // WiFi STA mode (required for ESP-NOW)
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))
            .map_err(|e| NodeError::Transport(format!("WiFi init: {:?}", e)))?;
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)
            .map_err(|e| NodeError::Transport(format!("WiFi wrap: {:?}", e)))?;
        wifi.start()
            .map_err(|e| NodeError::Transport(format!("WiFi start: {:?}", e)))?;

        // Set the WiFi channel before ESP-NOW init so the node and gateway
        // communicate on the same channel.
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_wifi_set_channel(
                channel,
                esp_idf_sys::wifi_second_chan_t_WIFI_SECOND_CHAN_NONE,
            ))
            .map_err(|e| NodeError::Transport(format!("set channel {}: {:?}", channel, e)))?;
        }

        let espnow =
            EspNow::take().map_err(|e| NodeError::Transport(format!("ESP-NOW init: {:?}", e)))?;

        // Register broadcast peer (channel = 0 means "use current WiFi channel")
        let peer_info = PeerInfo {
            peer_addr: BROADCAST_MAC,
            channel: 0,
            ..Default::default()
        };
        espnow
            .add_peer(peer_info)
            .map_err(|e| NodeError::Transport(format!("add peer: {:?}", e)))?;

        // Set up receive ring buffer and callback.
        let rx_ring = Arc::new(Mutex::new(RxRing::default()));
        let rx_condvar = Arc::new(Condvar::new());
        RECV_STATE
            .set(RecvState {
                rx_ring: Arc::clone(&rx_ring),
                condvar: Arc::clone(&rx_condvar),
                contention_drops: AtomicU32::new(0),
            })
            .map_err(|_| NodeError::Transport("recv callback already registered".into()))?;
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_now_register_recv_cb(Some(raw_recv_cb)))
                .map_err(|e| NodeError::Transport(format!("register recv cb: {:?}", e)))?;
        }

        Ok(Self {
            _wifi: wifi,
            espnow,
            rx_ring,
            rx_condvar,
        })
    }
}

impl crate::traits::Transport for EspNowTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        if frame.len() > ESPNOW_MAX_DATA_SIZE {
            return Err(NodeError::Transport("frame too large".into()));
        }
        self.espnow
            .send(BROADCAST_MAC, frame)
            .map_err(|e| NodeError::Transport(format!("send: {:?}", e)))
    }

    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
        // Pre-allocate buffer outside the lock for pop_into to copy into.
        let mut buf = [0u8; ESPNOW_MAX_DATA_SIZE];
        let mut ring = self
            .rx_ring
            .lock()
            .map_err(|_| NodeError::Transport("rx_ring lock poisoned".into()))?;
        // Drain any contention_drops accumulated by the callback into the
        // ring's deferred log on each entry so the warning is emitted.
        if let Some(state) = RECV_STATE.get() {
            let cd = state.contention_drops.swap(0, Ordering::Relaxed);
            if cd > 0 {
                log::warn!("ESP-NOW recv ring: {} contention drop(s)", cd);
            }
        }
        loop {
            if let Some(len) = ring.pop_into(&mut buf) {
                drop(ring);
                return Ok(Some(buf[..len].to_vec()));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline - now;
            let (guard, _timeout_result) = self
                .rx_condvar
                .wait_timeout(ring, remaining)
                .map_err(|_| NodeError::Transport("rx_ring lock poisoned".into()))?;
            ring = guard;
        }
    }
}
