// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB Serial/JTAG driver for pairing mode on ESP32-C3.
//!
//! Uses the ESP-IDF `usb_serial_jtag` driver API to read and write
//! binary modem frames over the USB Serial/JTAG peripheral (GPIO18 D-,
//! GPIO19 D+).
//!
//! If the driver is already installed (e.g. by the ESP-IDF secondary
//! console), we detect `ESP_ERR_INVALID_STATE` and reuse the existing
//! driver. Other install errors are propagated.

use crate::error::{NodeError, NodeResult};
use crate::traits::PairingSerial;

use log::info;

const TX_BUF_SIZE: u32 = 256;
const RX_BUF_SIZE: u32 = 256;

/// Convert milliseconds to FreeRTOS ticks using the ESP-IDF tick rate.
/// Rounds up to ensure non-zero ms always yields at least 1 tick.
fn ms_to_ticks(ms: u32) -> esp_idf_sys::TickType_t {
    if ms == 0 {
        return 0;
    }
    let period = unsafe { esp_idf_sys::portTICK_PERIOD_MS };
    if period == 0 {
        return ms; // Fallback: assume 1ms/tick
    }
    // Round up: (ms + period - 1) / period, minimum 1 tick.
    let ticks = (ms + period - 1) / period;
    ticks.max(1)
}

/// USB Serial/JTAG driver for ESP32-C3 pairing mode.
pub struct EspUsbSerialJtag {
    /// True if we installed the driver ourselves (and must uninstall on drop).
    owns_driver: bool,
}

impl EspUsbSerialJtag {
    /// Initialize the USB Serial/JTAG for pairing mode.
    ///
    /// If the driver is already installed (`ESP_ERR_INVALID_STATE`), we
    /// reuse it. Other install errors are propagated as `Err`.
    pub fn new() -> NodeResult<Self> {
        let mut config = esp_idf_sys::usb_serial_jtag_driver_config_t {
            tx_buffer_size: TX_BUF_SIZE,
            rx_buffer_size: RX_BUF_SIZE,
        };
        let ret = unsafe { esp_idf_sys::usb_serial_jtag_driver_install(&mut config) };
        let owns_driver = if ret == esp_idf_sys::ESP_OK as i32 {
            info!("USB Serial/JTAG driver installed for pairing mode");
            true
        } else if ret == esp_idf_sys::ESP_ERR_INVALID_STATE {
            info!("USB Serial/JTAG driver already active, reusing");
            false
        } else {
            return Err(NodeError::Transport(format!(
                "usb_serial_jtag_driver_install failed: {ret}"
            )));
        };

        // Brief delay to let any buffered boot text drain from the USB
        // FIFO before we start sending binary frames.
        unsafe { esp_idf_sys::vTaskDelay(ms_to_ticks(200)) };

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
        let n = unsafe {
            esp_idf_sys::usb_serial_jtag_read_bytes(
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                ms_to_ticks(timeout_ms),
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
            let n = unsafe {
                esp_idf_sys::usb_serial_jtag_write_bytes(
                    remaining.as_ptr().cast(),
                    remaining.len(),
                    ms_to_ticks(1000),
                )
            };
            if n < 0 {
                return Err(NodeError::Transport("USB Serial/JTAG write error".into()));
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
