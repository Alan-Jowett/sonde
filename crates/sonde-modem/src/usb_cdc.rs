// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB-CDC ACM driver for the ESP32-S3 native USB peripheral.
//!
//! Provides byte-level read/write. Connectivity is inferred from
//! read/write success — writes are always attempted so critical messages
//! like `MODEM_READY` are never silently dropped. Real DTR line-state
//! detection should be added when the ESP-IDF HAL exposes callbacks.

use esp_idf_hal::usb::UsbSerial;
use log::{info, warn};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// USB-CDC driver wrapping the ESP32-S3 native USB serial interface.
pub struct UsbCdcDriver {
    serial: UsbSerial,
    connected: Arc<AtomicBool>,
}

impl UsbCdcDriver {
    pub fn new() -> Self {
        let serial = UsbSerial::new().expect("failed to initialize USB-CDC");
        info!("USB-CDC initialized");
        Self {
            serial,
            // Start connected so boot MODEM_READY is sent. The flag will
            // flip to false on write/read errors and back to true when
            // data arrives.
            connected: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Returns true if we have recently exchanged data successfully.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Read available bytes from the USB receive buffer into `buf`.
    /// Returns `(bytes_read, reconnected)` where `reconnected` is true
    /// if this read transitioned from disconnected to connected state.
    /// Always attempts the read so reconnection can be detected.
    pub fn read(&mut self, buf: &mut [u8]) -> (usize, bool) {
        match self.serial.read(buf) {
            Ok(0) => (0, false),
            Ok(n) => {
                let was_disconnected = !self.connected.swap(true, Ordering::Relaxed);
                (n, was_disconnected)
            }
            Err(_) => {
                self.connected.store(false, Ordering::Relaxed);
                (0, false)
            }
        }
    }

    /// Write bytes to the USB transmit buffer.
    /// Always attempts the write so critical messages like MODEM_READY
    /// are sent even when the connection state is uncertain. Updates
    /// `connected` based on the result. Returns true on success.
    pub fn write(&mut self, data: &[u8]) -> bool {
        match self.serial.write_all(data) {
            Ok(()) => {
                self.connected.store(true, Ordering::Relaxed);
                true
            }
            Err(e) => {
                warn!("USB-CDC write error: {:?}", e);
                self.connected.store(false, Ordering::Relaxed);
                false
            }
        }
    }

    /// Mark the connection as dropped (called when DTR de-asserts).
    pub fn set_disconnected(&self) {
        self.connected.store(false, Ordering::Relaxed);
    }

    /// Mark the connection as re-established and return true if it was
    /// previously disconnected (triggers MODEM_READY re-send).
    pub fn set_connected(&self) -> bool {
        !self.connected.swap(true, Ordering::Relaxed)
    }
}
