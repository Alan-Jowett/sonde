// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB-CDC ACM driver for the ESP32-S3 native USB peripheral.
//!
//! Provides byte-level read/write. Connectivity is inferred from
//! read/write success — writes are always attempted so critical messages
//! like `MODEM_READY` are never silently dropped. Real DTR line-state
//! detection should be added when the ESP-IDF HAL exposes callbacks.

use esp_idf_hal::peripheral::Peripheral;
use esp_idf_hal::usb_serial::{UsbDMinGpio, UsbDPlusGpio, UsbSerialConfig, UsbSerialDriver, USB_SERIAL};
use log::{info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::bridge::SerialPort;

/// USB-CDC driver wrapping the ESP32-S3 native USB serial interface.
pub struct UsbCdcDriver {
    serial: UsbSerialDriver<'static>,
    connected: Arc<AtomicBool>,
}

impl UsbCdcDriver {
    pub fn new(
        usb: impl Peripheral<P = USB_SERIAL> + 'static,
        usb_d_min: impl Peripheral<P = UsbDMinGpio> + 'static,
        usb_d_plus: impl Peripheral<P = UsbDPlusGpio> + 'static,
    ) -> Self {
        let config = UsbSerialConfig::new();
        let serial = UsbSerialDriver::new(usb, usb_d_min, usb_d_plus, &config)
            .expect("failed to initialize USB-CDC");
        info!("USB-CDC initialized");
        Self {
            serial,
            // Start connected so boot MODEM_READY is sent. The flag will
            // flip to false on write/read errors and back to true when
            // data arrives.
            connected: Arc::new(AtomicBool::new(true)),
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

impl SerialPort for UsbCdcDriver {
    /// Read available bytes from the USB receive buffer into `buf`.
    /// Uses a non-blocking read (timeout = 0).
    fn read(&mut self, buf: &mut [u8]) -> (usize, bool) {
        match self.serial.read(buf, 0) {
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
    /// are sent even when the connection state is uncertain.
    fn write(&mut self, data: &[u8]) -> bool {
        let mut remaining = data;
        while !remaining.is_empty() {
            match self.serial.write(remaining, 100) {
                Ok(0) => {
                    self.connected.store(false, Ordering::Relaxed);
                    return false;
                }
                Ok(n) => remaining = &remaining[n..],
                Err(e) => {
                    warn!("USB-CDC write error: {:?}", e);
                    self.connected.store(false, Ordering::Relaxed);
                    return false;
                }
            }
        }
        self.connected.store(true, Ordering::Relaxed);
        true
    }

    /// Returns true if we have recently exchanged data successfully.
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}
