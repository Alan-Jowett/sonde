// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW transport for the sensor node.
//!
//! The node communicates with the gateway over ESP-NOW using broadcast
//! frames (destination MAC `FF:FF:FF:FF:FF:FF`). Received frames are
//! buffered in a fixed-slot ring buffer populated by an ESP-NOW receive
//! callback, eliminating per-frame heap allocation from the WiFi task
//! context.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use esp_idf_hal::modem::Modem;
use esp_idf_svc::espnow::{EspNow, PeerInfo};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};

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
    /// Copy `payload` into the next ring slot. Returns `false` if the ring
    /// is full or the payload exceeds `ESPNOW_MAX_DATA_SIZE`.
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
            // Match try_lock errors explicitly: recover on Poisoned so
            // RX doesn't go permanently silent, only count WouldBlock
            // as contention.
            let mut guard = match state.rx_ring.try_lock() {
                Ok(g) => g,
                Err(std::sync::TryLockError::WouldBlock) => {
                    state.contention_drops.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            };
            if guard.push(payload) {
                true
            } else {
                guard.drop_count = guard.drop_count.saturating_add(1);
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
    poison_warned: AtomicBool,
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
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        channel: u8,
    ) -> Result<Self, NodeError> {
        if channel < 1 || channel > 13 {
            return Err(NodeError::Transport("invalid WiFi channel (must be 1–13)"));
        }

        // WiFi STA mode (required for ESP-NOW TX)
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))
            .map_err(|_| NodeError::Transport("WiFi init failed"))?;
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)
            .map_err(|_| NodeError::Transport("WiFi wrap failed"))?;
        wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))
            .map_err(|_| NodeError::Transport("WiFi set STA mode failed"))?;
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

        let espnow = EspNow::take().map_err(|_| NodeError::Transport("ESP-NOW init failed"))?;

        // Register broadcast peer (channel = 0 means "use current WiFi channel")
        let peer_info = PeerInfo {
            peer_addr: BROADCAST_MAC,
            channel: 0,
            ..Default::default()
        };
        espnow
            .add_peer(peer_info)
            .map_err(|_| NodeError::Transport("add ESP-NOW peer failed"))?;

        // Set up receive ring buffer and callback.
        let rx_ring = Arc::new(Mutex::new(RxRing::default()));
        let rx_condvar = Arc::new(Condvar::new());
        RECV_STATE
            .set(RecvState {
                rx_ring: Arc::clone(&rx_ring),
                condvar: Arc::clone(&rx_condvar),
                contention_drops: AtomicU32::new(0),
            })
            .map_err(|_| NodeError::Transport("recv callback already registered"))?;
        unsafe {
            esp_idf_sys::esp!(esp_idf_sys::esp_now_register_recv_cb(Some(raw_recv_cb)))
                .map_err(|_| NodeError::Transport("register recv callback failed"))?;
        }

        Ok(Self {
            _wifi: wifi,
            espnow,
            rx_ring,
            rx_condvar,
            poison_warned: AtomicBool::new(false),
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
        // Pre-allocate buffer outside the lock for pop_into to copy into.
        let mut buf = [0u8; ESPNOW_MAX_DATA_SIZE];
        // Read+clear contention drops before locking — the counter is
        // atomic so no lock is needed, and logging outside the critical
        // section avoids extending try_lock contention in raw_recv_cb.
        if let Some(state) = RECV_STATE.get() {
            let cd = state.contention_drops.swap(0, Ordering::Relaxed);
            if cd > 0 {
                log::warn!("ESP-NOW recv ring: {} contention drop(s)", cd);
            }
        }
        let mut ring = match self.rx_ring.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                if !self.poison_warned.swap(true, Ordering::Relaxed) {
                    log::warn!("rx_ring mutex poisoned in recv(), recovering");
                }
                poisoned.into_inner()
            }
        };
        loop {
            if let Some(len) = ring.pop_into(&mut buf) {
                // Re-read drop_count right before returning so drops
                // that occurred during wait_timeout are captured.
                let full_drops = ring.drop_count;
                ring.drop_count = 0;
                drop(ring);
                if full_drops > 0 {
                    log::warn!("ESP-NOW recv ring: {} full drop(s)", full_drops);
                }
                return Ok(Some(buf[..len].to_vec()));
            }
            let now = Instant::now();
            if now >= deadline {
                let full_drops = ring.drop_count;
                ring.drop_count = 0;
                drop(ring);
                if full_drops > 0 {
                    log::warn!("ESP-NOW recv ring: {} full drop(s)", full_drops);
                }
                return Ok(None);
            }
            let remaining = deadline - now;
            let (guard, _timeout_result) = match self.rx_condvar.wait_timeout(ring, remaining) {
                Ok(result) => result,
                Err(poisoned) => {
                    if !self.poison_warned.swap(true, Ordering::Relaxed) {
                        log::warn!("rx_ring mutex poisoned in wait_timeout, recovering");
                    }
                    poisoned.into_inner()
                }
            };
            ring = guard;
        }
    }
}
