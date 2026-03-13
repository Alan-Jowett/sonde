// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32 hardware abstraction: clock, HAL, and battery reader.
//!
//! The clock uses `esp_timer_get_time()` for monotonic time and
//! `std::thread::sleep` for delays (portable across ESP-IDF versions).
//!
//! The HAL uses raw `esp_idf_sys` APIs for I2C, GPIO, and ADC access.
//! I2C bus 0 is initialized on construction using the command-builder
//! API (`i2c_cmd_link_create` / `i2c_master_cmd_begin`), which is
//! available in all ESP-IDF versions. SPI is left as a stub until
//! device-specific CS pin configuration is available.

use crate::hal;
use log::warn;

/// Default I2C0 SDA pin for ESP32 dev boards.
/// Override in sdkconfig or a board-specific configuration module.
const I2C0_SDA: i32 = 4;

/// Default I2C0 SCL pin for ESP32 dev boards.
/// Override in sdkconfig or a board-specific configuration module.
const I2C0_SCL: i32 = 5;
const I2C0_FREQ_HZ: u32 = 100_000; // 100 kHz standard mode

// Timeout for I2C operations in FreeRTOS ticks (1 tick ≈ 1 ms at default rate).
const I2C_TIMEOUT_TICKS: u32 = 1000;

/// ESP-IDF monotonic clock using `esp_timer_get_time()`.
pub struct EspClock;

impl crate::traits::Clock for EspClock {
    fn elapsed_ms(&self) -> u64 {
        // esp_timer_get_time returns microseconds since boot
        (unsafe { esp_idf_sys::esp_timer_get_time() } as u64) / 1000
    }

    fn delay_ms(&self, ms: u32) {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }

    // delay_us: use the default trait implementation (rounds up to ms).
    // ESP-IDF's ets_delay_us is not reliably exported by esp-idf-sys
    // across all target versions. The default is sufficient for BPF
    // helper use cases where sub-ms precision is best-effort.
}

/// Real ESP32 HAL backed by ESP-IDF sys APIs.
///
/// Initializes I2C bus 0 on construction. Additional buses and
/// SPI are left as stubs until needed. GPIO and ADC use direct
/// ESP-IDF calls with no pre-initialization.
pub struct EspHal {
    i2c0_initialized: bool,
    adc_width_configured: bool,
}

impl EspHal {
    pub fn new() -> Self {
        let mut hal = Self {
            i2c0_initialized: false,
            adc_width_configured: false,
        };
        hal.init_i2c0();
        hal
    }

    fn init_i2c0(&mut self) {
        unsafe {
            let port = esp_idf_sys::i2c_port_t_I2C_NUM_0;

            // Use zeroed struct and set fields individually to avoid
            // bindgen layout differences across esp-idf-sys versions.
            let mut conf: esp_idf_sys::i2c_config_t = core::mem::zeroed();
            conf.mode = esp_idf_sys::i2c_mode_t_I2C_MODE_MASTER;
            conf.sda_io_num = I2C0_SDA;
            conf.scl_io_num = I2C0_SCL;
            conf.sda_pullup_en = true;
            conf.scl_pullup_en = true;
            conf.__bindgen_anon_1.master.clk_speed = I2C0_FREQ_HZ;

            let err = esp_idf_sys::i2c_param_config(port, &conf);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("i2c_param_config failed: {err}");
                return;
            }
            let err = esp_idf_sys::i2c_driver_install(port, conf.mode, 0, 0, 0);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("i2c_driver_install failed: {err}");
                return;
            }
            self.i2c0_initialized = true;
        }
    }

    /// Map a BPF handle bus number to an ESP-IDF I2C port.
    /// Returns `None` if the bus is not initialized.
    fn i2c_port(&self, bus: u16) -> Option<esp_idf_sys::i2c_port_t> {
        match bus {
            0 if self.i2c0_initialized => Some(esp_idf_sys::i2c_port_t_I2C_NUM_0),
            _ => None,
        }
    }
}

impl hal::Hal for EspHal {
    fn i2c_read(&mut self, handle: u32, buf: &mut [u8]) -> i32 {
        let bus = hal::handle_bus(handle);
        let addr = hal::handle_addr(handle);
        let port = match self.i2c_port(bus) {
            Some(p) => p,
            None => return -1,
        };
        if buf.is_empty() {
            return 0;
        }
        unsafe {
            let cmd = esp_idf_sys::i2c_cmd_link_create();
            if cmd.is_null() {
                return -1;
            }
            esp_idf_sys::i2c_master_start(cmd);
            esp_idf_sys::i2c_master_write_byte(cmd, (addr << 1) | 0x01, true);
            if buf.len() > 1 {
                esp_idf_sys::i2c_master_read(
                    cmd,
                    buf.as_mut_ptr(),
                    buf.len() - 1,
                    esp_idf_sys::i2c_ack_type_t_I2C_MASTER_ACK,
                );
            }
            // NACK the last byte to signal end of read
            esp_idf_sys::i2c_master_read_byte(
                cmd,
                buf.as_mut_ptr().add(buf.len() - 1),
                esp_idf_sys::i2c_ack_type_t_I2C_MASTER_NACK,
            );
            esp_idf_sys::i2c_master_stop(cmd);
            let err = esp_idf_sys::i2c_master_cmd_begin(port, cmd, I2C_TIMEOUT_TICKS);
            esp_idf_sys::i2c_cmd_link_delete(cmd);
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            0
        }
    }

    fn i2c_write(&mut self, handle: u32, data: &[u8]) -> i32 {
        let bus = hal::handle_bus(handle);
        let addr = hal::handle_addr(handle);
        let port = match self.i2c_port(bus) {
            Some(p) => p,
            None => return -1,
        };
        unsafe {
            let cmd = esp_idf_sys::i2c_cmd_link_create();
            if cmd.is_null() {
                return -1;
            }
            esp_idf_sys::i2c_master_start(cmd);
            esp_idf_sys::i2c_master_write_byte(cmd, addr << 1, true);
            if !data.is_empty() {
                esp_idf_sys::i2c_master_write(cmd, data.as_ptr(), data.len(), true);
            }
            esp_idf_sys::i2c_master_stop(cmd);
            let err = esp_idf_sys::i2c_master_cmd_begin(port, cmd, I2C_TIMEOUT_TICKS);
            esp_idf_sys::i2c_cmd_link_delete(cmd);
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            0
        }
    }

    fn i2c_write_read(&mut self, handle: u32, write_data: &[u8], read_buf: &mut [u8]) -> i32 {
        let bus = hal::handle_bus(handle);
        let addr = hal::handle_addr(handle);
        let port = match self.i2c_port(bus) {
            Some(p) => p,
            None => return -1,
        };
        if read_buf.is_empty() {
            return self.i2c_write(handle, write_data);
        }
        unsafe {
            let cmd = esp_idf_sys::i2c_cmd_link_create();
            if cmd.is_null() {
                return -1;
            }
            // Write phase
            esp_idf_sys::i2c_master_start(cmd);
            esp_idf_sys::i2c_master_write_byte(cmd, addr << 1, true);
            if !write_data.is_empty() {
                esp_idf_sys::i2c_master_write(cmd, write_data.as_ptr(), write_data.len(), true);
            }
            // Repeated start + read phase
            esp_idf_sys::i2c_master_start(cmd);
            esp_idf_sys::i2c_master_write_byte(cmd, (addr << 1) | 0x01, true);
            if read_buf.len() > 1 {
                esp_idf_sys::i2c_master_read(
                    cmd,
                    read_buf.as_mut_ptr(),
                    read_buf.len() - 1,
                    esp_idf_sys::i2c_ack_type_t_I2C_MASTER_ACK,
                );
            }
            // NACK the last byte to signal end of read
            esp_idf_sys::i2c_master_read_byte(
                cmd,
                read_buf.as_mut_ptr().add(read_buf.len() - 1),
                esp_idf_sys::i2c_ack_type_t_I2C_MASTER_NACK,
            );
            esp_idf_sys::i2c_master_stop(cmd);
            let err = esp_idf_sys::i2c_master_cmd_begin(port, cmd, I2C_TIMEOUT_TICKS);
            esp_idf_sys::i2c_cmd_link_delete(cmd);
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            0
        }
    }

    fn spi_transfer(
        &mut self,
        _handle: u32,
        _tx: Option<&[u8]>,
        _rx: Option<&mut [u8]>,
        _len: usize,
    ) -> i32 {
        -1 // SPI requires device-specific CS pin configuration
    }

    fn gpio_read(&self, pin: u32) -> i32 {
        if pin > 39 {
            return -1;
        }
        unsafe { esp_idf_sys::gpio_get_level(pin as i32) }
    }

    fn gpio_write(&mut self, pin: u32, value: u32) -> i32 {
        if pin > 39 {
            return -1;
        }
        unsafe {
            let err = esp_idf_sys::gpio_set_direction(
                pin as i32,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            let err = esp_idf_sys::gpio_set_level(pin as i32, value);
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            0
        }
    }

    fn adc_read(&mut self, channel: u32) -> i32 {
        unsafe {
            if !self.adc_width_configured {
                let err =
                    esp_idf_sys::adc1_config_width(esp_idf_sys::adc_bits_width_t_ADC_WIDTH_BIT_12);
                if err != esp_idf_sys::ESP_OK as i32 {
                    return -1;
                }
                self.adc_width_configured = true;
            }
            // Configure channel attenuation (11 dB for full 0–3.3 V range).
            let err = esp_idf_sys::adc1_config_channel_atten(
                channel,
                esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            esp_idf_sys::adc1_get_raw(channel)
        }
    }
}

/// Battery reader using a fixed estimate.
///
/// On real hardware this would read an ADC channel connected to a
/// voltage divider on the battery. For initial bring-up, return a
/// fixed value indicating "battery OK".
pub struct EspBatteryReader;

impl hal::BatteryReader for EspBatteryReader {
    fn battery_mv(&self) -> u32 {
        3300 // Fixed estimate until ADC channel is configured
    }
}
