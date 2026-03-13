// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB Serial/JTAG driver for pairing mode on ESP32-C3.
//!
//! Uses the ESP-IDF `usb_serial_jtag` driver API to read and write
//! binary modem frames over the USB Serial/JTAG peripheral (GPIO18 D-,
//! GPIO19 D+).
//!
//! When the secondary console is active (CONFIG_ESP_CONSOLE_SECONDARY_
//! USB_SERIAL_JTAG=y), the driver is already installed by ESP-IDF.
//! We detect this and reuse the existing driver instead of installing
//! a second one.

use crate::error::{NodeError, NodeResult};
use crate::traits::PairingSerial;

use log::info;

const TX_BUF_SIZE: u32 = 256;
const RX_BUF_SIZE: u32 = 256;

/// Milliseconds per FreeRTOS tick. ESP-IDF defaults to
/// `configTICK_RATE_HZ = 1000`, so 1 tick = 1 ms.
const MS_PER_TICK: u32 = 1;

/// USB Serial/JTAG driver for ESP32-C3 pairing mode.
pub struct EspUsbSerialJtag {
    /// True if we installed the driver ourselves (and must uninstall on drop).
    owns_driver: bool,
}

impl EspUsbSerialJtag {
    /// Initialize the USB Serial/JTAG for pairing mode.
    ///
    /// If the driver is already installed (e.g. by the secondary console),
    /// we reuse it. Otherwise we install our own.
    pub fn new() -> NodeResult<Self> {
        let mut config = esp_idf_sys::usb_serial_jtag_driver_config_t {
            tx_buffer_size: TX_BUF_SIZE,
            rx_buffer_size: RX_BUF_SIZE,
        };
        let ret = unsafe { esp_idf_sys::usb_serial_jtag_driver_install(&mut config) };
        let owns_driver = ret == esp_idf_sys::ESP_OK as i32;
        if owns_driver {
            info!("USB Serial/JTAG driver installed for pairing mode");
        } else {
            info!("USB Serial/JTAG driver already active, reusing");
        }

        // Brief delay to let any buffered boot text drain from the USB
        // FIFO before we start sending binary frames.
        unsafe { esp_idf_sys::vTaskDelay(200) };

        Ok(Self { owns_driver })
    }
}

impl Drop for EspUsbSerialJtag {
    fn drop(&mut self) {
        if self.owns_driver {
            unsafe {
                esp_idf_sys::usb_serial_jtag_driver_uninstall();
            }
        }
    }
}

impl PairingSerial for EspUsbSerialJtag {
    fn read(&mut self, buf: &mut [u8], timeout_ms: u32) -> NodeResult<usize> {
        let ticks: esp_idf_sys::TickType_t = timeout_ms / MS_PER_TICK;
        let n = unsafe {
            esp_idf_sys::usb_serial_jtag_read_bytes(
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                ticks,
            )
        };
        if n < 0 {
            return Err(NodeError::Transport("USB Serial/JTAG read error".into()));
        }
        Ok(n as usize)
    }

    fn write(&mut self, data: &[u8]) -> NodeResult<()> {
        let mut remaining = data;
        while !remaining.is_empty() {
            let ticks: esp_idf_sys::TickType_t = 1000 / MS_PER_TICK;
            let n = unsafe {
                esp_idf_sys::usb_serial_jtag_write_bytes(
                    remaining.as_ptr().cast(),
                    remaining.len(),
                    ticks,
                )
            };
            if n < 0 {
                return Err(NodeError::Transport(
                    "USB Serial/JTAG write error".into(),
                ));
            }
            if n == 0 {
                // Timeout -- no host reading. Not fatal; caller decides.
                return Ok(());
            }
            remaining = &remaining[n as usize..];
        }
        Ok(())
    }
}
