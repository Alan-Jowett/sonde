// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32 hardware abstraction: clock, HAL, and battery reader.
//!
//! The clock uses `esp_timer_get_time()` for monotonic time and
//! `std::thread::sleep` for delays (portable across ESP-IDF versions).
//!
//! The HAL uses raw `esp_idf_sys` APIs for I2C, GPIO, and ADC access.
//! I2C bus 0 is initialized lazily on the first transaction so wake-time
//! pin preparation can leave provisioned pins in their safe default state.
//! SPI is left as a stub until device-specific CS pin configuration is
//! available.

use core::ptr;

use crate::hal;
use log::warn;
use sonde_protocol::BoardLayout;

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

    fn delay_us(&self, us: u32) {
        if us == 0 {
            return;
        }
        // For delays ≥ 1 ms, sleep via the FreeRTOS scheduler so other
        // tasks can run, then busy-wait any sub-ms remainder with true
        // µs precision using the ROM busy-wait loop.
        if us >= 1000 {
            self.delay_ms(us / 1000);
            let rem = us % 1000;
            if rem > 0 {
                unsafe { esp_idf_sys::esp_rom_delay_us(rem) };
            }
        } else {
            unsafe { esp_idf_sys::esp_rom_delay_us(us) };
        }
    }
}

/// Real ESP32 HAL backed by ESP-IDF sys APIs.
///
/// Initializes the provisioned GPIOs into their idle state on
/// construction. I2C bus 0 is still initialized lazily on the first
/// transaction so the wake-time baseline does not grab the bus pins.
/// Additional buses and SPI are left as stubs until needed. GPIO and
/// ADC use direct ESP-IDF calls with no pre-initialization.
pub struct EspHal {
    i2c0_initialized: bool,
    board_layout: BoardLayout,
    adc_width_configured: bool,
    adc_calibration_handle: esp_idf_sys::adc_cali_handle_t,
    adc_calibration_channel: Option<u32>,
    adc_calibration_attempted: bool,
    /// Bitmask of GPIO pins already configured as output.
    gpio_output_configured: u64,
    /// Bitmask of ADC channels already configured with attenuation.
    adc_channels_configured: u32,
}

impl EspHal {
    /// Create a new HAL with the current wake cycle's provisioned board layout.
    pub fn new(board_layout: BoardLayout) -> Self {
        let mut hal = Self {
            i2c0_initialized: false,
            board_layout,
            adc_width_configured: false,
            adc_calibration_handle: ptr::null_mut(),
            adc_calibration_channel: None,
            adc_calibration_attempted: false,
            gpio_output_configured: 0,
            adc_channels_configured: 0,
        };
        hal::Hal::enter_idle_gpio_state(&mut hal);
        hal
    }

    fn init_i2c0(&mut self, sda: i32, scl: i32) {
        unsafe {
            let port = esp_idf_sys::i2c_port_t_I2C_NUM_0;

            // Use zeroed struct and set fields individually to avoid
            // bindgen layout differences across esp-idf-sys versions.
            let mut conf: esp_idf_sys::i2c_config_t = core::mem::zeroed();
            conf.mode = esp_idf_sys::i2c_mode_t_I2C_MODE_MASTER;
            conf.sda_io_num = sda;
            conf.scl_io_num = scl;
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
    /// Lazily initializes the bus on first use when the paired board layout
    /// provides pins for it.
    fn i2c_port(&mut self, bus: u16) -> Option<esp_idf_sys::i2c_port_t> {
        match bus {
            0 => {
                if !self.i2c0_initialized {
                    let (Some(i2c0_sda), Some(i2c0_scl)) =
                        (self.board_layout.i2c0_sda, self.board_layout.i2c0_scl)
                    else {
                        return None;
                    };
                    self.init_i2c0(i2c0_sda as i32, i2c0_scl as i32);
                }
                self.i2c0_initialized
                    .then_some(esp_idf_sys::i2c_port_t_I2C_NUM_0)
            }
            _ => None,
        }
    }

    fn set_input_no_pull(pin: i32) {
        unsafe {
            let err =
                esp_idf_sys::gpio_set_direction(pin, esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_set_direction({pin}, INPUT) failed: {err}");
            }
            let err = esp_idf_sys::gpio_pullup_dis(pin);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_pullup_dis({pin}) failed: {err}");
            }
            let err = esp_idf_sys::gpio_pulldown_dis(pin);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_pulldown_dis({pin}) failed: {err}");
            }
        }
    }

    fn set_input_pull_up(pin: i32) {
        unsafe {
            let err =
                esp_idf_sys::gpio_set_direction(pin, esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_set_direction({pin}, INPUT) failed: {err}");
            }
            let err = esp_idf_sys::gpio_set_pull_mode(
                pin,
                esp_idf_sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_set_pull_mode({pin}, PULLUP_ONLY) failed: {err}");
            }
        }
    }

    fn configure_sleep_input_no_pull(pin: i32) {
        unsafe {
            let err = esp_idf_sys::gpio_sleep_sel_en(pin);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_sleep_sel_en({pin}) failed: {err}");
            }
            let err = esp_idf_sys::gpio_sleep_set_direction(
                pin,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_DISABLE,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_sleep_set_direction({pin}, DISABLE) failed: {err}");
            }
            let err = esp_idf_sys::gpio_sleep_set_pull_mode(
                pin,
                esp_idf_sys::gpio_pull_mode_t_GPIO_FLOATING,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_sleep_set_pull_mode({pin}, FLOATING) failed: {err}");
            }
        }
    }

    fn configure_sleep_output(pin: i32, level: u32) {
        unsafe {
            let err = esp_idf_sys::gpio_sleep_sel_en(pin);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_sleep_sel_en({pin}) failed: {err}");
            }
            let err = esp_idf_sys::gpio_sleep_set_direction(
                pin,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_sleep_set_direction({pin}, OUTPUT) failed: {err}");
            }
            let err = esp_idf_sys::gpio_sleep_set_pull_mode(
                pin,
                esp_idf_sys::gpio_pull_mode_t_GPIO_FLOATING,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_sleep_set_pull_mode({pin}, FLOATING) failed: {err}");
            }
            let err = esp_idf_sys::gpio_set_level(pin, if level != 0 { 1 } else { 0 });
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_set_level({pin}, {level}) failed: {err}");
            }
        }
    }

    fn set_output_level(pin: i32, level: u32) {
        unsafe {
            let err =
                esp_idf_sys::gpio_set_direction(pin, esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_set_direction({pin}, OUTPUT) failed: {err}");
                return;
            }
            let err = esp_idf_sys::gpio_pullup_dis(pin);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_pullup_dis({pin}) failed: {err}");
            }
            let err = esp_idf_sys::gpio_pulldown_dis(pin);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_pulldown_dis({pin}) failed: {err}");
            }
            let err = esp_idf_sys::gpio_set_level(pin, if level != 0 { 1 } else { 0 });
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("gpio_set_level({pin}, {level}) failed: {err}");
            }
        }
    }

    fn set_idle_inputs(board_layout: &BoardLayout) {
        if let Some(battery_adc) = board_layout.battery_adc {
            Self::set_input_no_pull(battery_adc as i32);
            Self::configure_sleep_input_no_pull(battery_adc as i32);
        }
        if let Some(one_wire_data) = board_layout.one_wire_data {
            Self::set_input_no_pull(one_wire_data as i32);
            Self::configure_sleep_input_no_pull(one_wire_data as i32);
        }
        if let Some(i2c0_sda) = board_layout.i2c0_sda {
            Self::set_input_no_pull(i2c0_sda as i32);
            Self::configure_sleep_input_no_pull(i2c0_sda as i32);
        }
        if let Some(i2c0_scl) = board_layout.i2c0_scl {
            Self::set_input_no_pull(i2c0_scl as i32);
            Self::configure_sleep_input_no_pull(i2c0_scl as i32);
        }
    }

    fn ensure_adc_calibration(&mut self, channel: u32) -> bool {
        if self.adc_calibration_attempted && self.adc_calibration_channel == Some(channel) {
            return !self.adc_calibration_handle.is_null();
        }

        if !self.adc_calibration_handle.is_null() {
            unsafe {
                let err =
                    esp_idf_sys::adc_cali_delete_scheme_curve_fitting(self.adc_calibration_handle);
                if err != esp_idf_sys::ESP_OK as i32 {
                    warn!("adc_cali_delete_scheme_curve_fitting failed: {err}");
                }
            }
            self.adc_calibration_handle = ptr::null_mut();
            self.adc_calibration_channel = None;
        }

        self.adc_calibration_attempted = true;

        unsafe {
            let config = esp_idf_sys::adc_cali_curve_fitting_config_t {
                unit_id: esp_idf_sys::adc_unit_t_ADC_UNIT_1,
                chan: channel,
                atten: esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11,
                bitwidth: esp_idf_sys::adc_bits_width_t_ADC_WIDTH_BIT_12,
            };
            let mut handle: esp_idf_sys::adc_cali_handle_t = ptr::null_mut();
            let err = esp_idf_sys::adc_cali_create_scheme_curve_fitting(&config, &mut handle);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("adc_cali_create_scheme_curve_fitting failed: {err}");
                return false;
            }
            self.adc_calibration_handle = handle;
            self.adc_calibration_channel = Some(channel);
        }

        true
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

    fn spi_transfer(&mut self, _handle: u32, _buf: &mut [u8]) -> i32 {
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
            // Only configure direction on first write to this pin.
            if self.gpio_output_configured & (1u64 << pin) == 0 {
                let err = esp_idf_sys::gpio_set_direction(
                    pin as i32,
                    esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
                );
                if err != esp_idf_sys::ESP_OK as i32 {
                    return -1;
                }
                self.gpio_output_configured |= 1u64 << pin;
            }
            let level = if value != 0 { 1 } else { 0 };
            let err = esp_idf_sys::gpio_set_level(pin as i32, level);
            if err != esp_idf_sys::ESP_OK as i32 {
                return -1;
            }
            0
        }
    }

    fn adc_read(&mut self, channel: u32) -> i32 {
        // ESP32-C3 exposes ADC1 channels 0-4 on GPIO0-4. GPIO5 is ADC2 and
        // is not handled by this ADC1-only path.
        if channel > 4 {
            return -1;
        }
        unsafe {
            if !self.adc_width_configured {
                let err =
                    esp_idf_sys::adc1_config_width(esp_idf_sys::adc_bits_width_t_ADC_WIDTH_BIT_12);
                if err != esp_idf_sys::ESP_OK as i32 {
                    return -1;
                }
                self.adc_width_configured = true;
            }
            // Configure channel attenuation once per channel.
            if self.adc_channels_configured & (1u32 << channel) == 0 {
                let err = esp_idf_sys::adc1_config_channel_atten(
                    channel,
                    esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11,
                );
                if err != esp_idf_sys::ESP_OK as i32 {
                    return -1;
                }
                self.adc_channels_configured |= 1u32 << channel;
            }
            esp_idf_sys::adc1_get_raw(channel)
        }
    }

    fn adc_read_mv(&mut self, channel: u32) -> i32 {
        let raw = self.adc_read(channel);
        if raw < 0 {
            return raw;
        }

        if !self.ensure_adc_calibration(channel) {
            return hal::Hal::adc_read_mv(self, channel);
        }

        unsafe {
            let mut mv = 0i32;
            let err =
                esp_idf_sys::adc_cali_raw_to_voltage(self.adc_calibration_handle, raw, &mut mv);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("adc_cali_raw_to_voltage failed: {err}");
                return hal::Hal::adc_read_mv(self, channel);
            }
            mv
        }
    }

    fn enter_idle_gpio_state(&mut self) {
        if self.i2c0_initialized {
            let err = unsafe { esp_idf_sys::i2c_driver_delete(esp_idf_sys::i2c_port_t_I2C_NUM_0) };
            if err == esp_idf_sys::ESP_OK as i32 {
                self.i2c0_initialized = false;
            } else {
                warn!("i2c_driver_delete failed: {err}");
            }
        }

        let mut mask = self.gpio_output_configured;
        while mask != 0 {
            let pin = mask.trailing_zeros();
            Self::set_input_no_pull(pin as i32);
            mask &= !(1u64 << pin);
        }

        Self::set_idle_inputs(&self.board_layout);
        if let Some(sensor_enable) = self.board_layout.sensor_enable {
            Self::set_output_level(sensor_enable as i32, 1);
            Self::configure_sleep_output(sensor_enable as i32, 1);
        }

        self.gpio_output_configured = 0;
        self.adc_width_configured = false;
        self.adc_channels_configured = 0;
        if !self.adc_calibration_handle.is_null() {
            unsafe {
                let err =
                    esp_idf_sys::adc_cali_delete_scheme_curve_fitting(self.adc_calibration_handle);
                if err != esp_idf_sys::ESP_OK as i32 {
                    warn!("adc_cali_delete_scheme_curve_fitting failed: {err}");
                }
            }
            self.adc_calibration_handle = ptr::null_mut();
        }
        self.adc_calibration_channel = None;
        self.adc_calibration_attempted = false;
    }

    fn enter_active_gpio_state(&mut self) {
        if let Some(sensor_enable) = self.board_layout.sensor_enable {
            Self::set_output_level(sensor_enable as i32, 0);
        }
        if let Some(i2c0_sda) = self.board_layout.i2c0_sda {
            Self::set_input_pull_up(i2c0_sda as i32);
        }
        if let Some(i2c0_scl) = self.board_layout.i2c0_scl {
            Self::set_input_pull_up(i2c0_scl as i32);
        }
        if let Some(one_wire_data) = self.board_layout.one_wire_data {
            Self::set_input_pull_up(one_wire_data as i32);
        }
        if let Some(battery_adc) = self.board_layout.battery_adc {
            Self::set_input_no_pull(battery_adc as i32);
        }
    }

    fn prepare_for_sleep(&mut self) {
        self.enter_idle_gpio_state();
    }
}
