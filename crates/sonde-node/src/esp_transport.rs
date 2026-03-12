// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW transport for the sensor node.
//!
//! The node communicates with the gateway over ESP-NOW using broadcast
//! frames (destination MAC `FF:FF:FF:FF:FF:FF`). Received frames are
//! buffered in a shared queue populated by an ESP-NOW receive callback.
//!
//! **Status:** Stub — compiles under `--features esp` but panics at
//! runtime. The real implementation will be filled in during hardware
//! bring-up once WiFi driver lifecycle management is integrated.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::error::{NodeError, NodeResult};

/// Broadcast MAC used for all node → gateway transmissions.
const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

/// ESP-NOW transport backed by `esp-idf-svc`.
///
/// Holds the WiFi + ESP-NOW handles for the lifetime of the transport
/// and maintains a receive queue filled by a global callback.
pub struct EspNowTransport {
    rx_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl EspNowTransport {
    /// Initialise WiFi in STA mode and start ESP-NOW.
    ///
    /// This requires the WiFi modem peripheral and NVS partition, which
    /// will be threaded through once the full platform init sequence is
    /// implemented.
    pub fn new() -> Result<Self, NodeError> {
        // WiFi + ESP-NOW initialisation is deferred to hardware bring-up.
        todo!("ESP-NOW transport requires WiFi initialisation")
    }
}

impl crate::traits::Transport for EspNowTransport {
    fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
        // Will call `esp_now_send` with `BROADCAST_MAC`.
        let _ = (frame, BROADCAST_MAC);
        todo!("EspNowTransport::send")
    }

    fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
        // Will poll `rx_queue` with the given timeout.
        let _ = (timeout_ms, &self.rx_queue);
        todo!("EspNowTransport::recv")
    }
}
