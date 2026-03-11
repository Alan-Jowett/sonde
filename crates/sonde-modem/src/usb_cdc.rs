// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB-CDC ACM driver for the ESP32-S3 native USB peripheral.
//!
//! Provides byte-level read/write. The `connected` flag is set to false
//! on read/write errors and must be re-asserted by the caller (e.g., on
//! receiving data after a gap). Real DTR line-state detection should be
//! added when the ESP-IDF HAL exposes line-state callbacks.

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
    /// Returns the number of bytes read, or 0 if no data is available.
    /// On I/O errors, sets `connected` to false.
    /// Always attempts the read even when `connected` is false so that
    /// reconnection can be detected when bytes arrive again.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        match self.serial.read(buf) {
            Ok(0) => 0,
            Ok(n) => {
                if !self.connected.swap(true, Ordering::Relaxed) {
                    // Was disconnected, now have data — caller should
                    // send MODEM_READY.
                }
                n
            }
            Err(_) => {
                self.connected.store(false, Ordering::Relaxed);
                0
            }
        }
    }

    /// Write bytes to the USB transmit buffer.
    /// Always attempts the write so critical messages like MODEM_READY
    /// are sent even when the connection state is uncertain. Updates
    /// `connected` based on the result.
    pub fn write(&mut self, data: &[u8]) {
        match self.serial.write_all(data) {
            Ok(()) => {
                self.connected.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                warn!("USB-CDC write error: {:?}", e);
                self.connected.store(false, Ordering::Relaxed);
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
