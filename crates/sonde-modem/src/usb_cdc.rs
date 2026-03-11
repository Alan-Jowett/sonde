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
            connected: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Returns true if the USB host has the port open (DTR asserted).
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Read available bytes from the USB receive buffer into `buf`.
    /// Returns the number of bytes read, or 0 if no data is available.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        if !self.is_connected() {
            return 0;
        }
        match self.serial.read(buf) {
            Ok(n) => n,
            Err(_) => 0,
        }
    }

    /// Write bytes to the USB transmit buffer.
    /// If the host is disconnected, the data is silently discarded.
    pub fn write(&mut self, data: &[u8]) {
        if !self.is_connected() {
            return;
        }
        if let Err(e) = self.serial.write_all(data) {
            warn!("USB-CDC write error: {:?}", e);
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
