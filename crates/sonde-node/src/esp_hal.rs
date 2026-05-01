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
    adc_oneshot_unit: esp_idf_sys::adc_oneshot_unit_handle_t,
    /// Bitmask of GPIO pins already configured as output.
    gpio_output_configured: u64,
    /// Bitmask of ADC1 channels already configured for oneshot reads.
    adc_oneshot_channels_configured: u32,
    /// Per-channel curve-fitting calibration handles for ADC1 channels 0-4.
    adc_cali_handles: [esp_idf_sys::adc_cali_handle_t; 5],
}

impl EspHal {
    /// Create a new HAL with the current wake cycle's provisioned board layout.
    pub fn new(board_layout: BoardLayout) -> Self {
        let mut hal = Self {
            i2c0_initialized: false,
            board_layout,
            adc_oneshot_unit: ptr::null_mut(),
            gpio_output_configured: 0,
            adc_oneshot_channels_configured: 0,
            adc_cali_handles: [ptr::null_mut(); 5],
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

    fn log_gpio_level(pin: i32, phase: &str, target_level: u32) {
        let observed_level = unsafe { esp_idf_sys::gpio_get_level(pin) };
        warn!(
            "sensor_enable gpio={} phase={} target_level={} observed_level={}",
            pin, phase, target_level, observed_level
        );
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

    fn delete_oneshot_unit(unit: esp_idf_sys::adc_oneshot_unit_handle_t) {
        if unit.is_null() {
            return;
        }
        let err = unsafe { esp_idf_sys::adc_oneshot_del_unit(unit) };
        if err != esp_idf_sys::ESP_OK as i32 {
            warn!("adc_oneshot_del_unit failed: {err}");
        }
    }

    fn delete_curve_fitting_calibration(handle: esp_idf_sys::adc_cali_handle_t) {
        if handle.is_null() {
            return;
        }
        let err = unsafe { esp_idf_sys::adc_cali_delete_scheme_curve_fitting(handle) };
        if err != esp_idf_sys::ESP_OK as i32 {
            warn!("adc_cali_delete_scheme_curve_fitting failed: {err}");
        }
    }

    fn ensure_adc_oneshot_unit(&mut self) -> bool {
        if !self.adc_oneshot_unit.is_null() {
            return true;
        }

        unsafe {
            let mut unit_config: esp_idf_sys::adc_oneshot_unit_init_cfg_t = core::mem::zeroed();
            unit_config.unit_id = esp_idf_sys::adc_unit_t_ADC_UNIT_1;
            let err = esp_idf_sys::adc_oneshot_new_unit(&unit_config, &mut self.adc_oneshot_unit);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("adc_oneshot_new_unit failed: {err}");
                self.adc_oneshot_unit = ptr::null_mut();
                return false;
            }
        }

        true
    }

    fn ensure_adc_oneshot_channel(&mut self, channel: u32) -> bool {
        const ADC_ONESHOT_ATTEN: esp_idf_sys::adc_atten_t =
            esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11;
        const ADC_ONESHOT_BITWIDTH: esp_idf_sys::adc_bitwidth_t =
            esp_idf_sys::adc_bitwidth_t_ADC_BITWIDTH_12;

        if channel > 4 {
            warn!("adc_oneshot invalid ADC1 channel: {channel}");
            return false;
        }
        if !self.ensure_adc_oneshot_unit() {
            return false;
        }
        if self.adc_oneshot_channels_configured & (1u32 << channel) != 0 {
            return true;
        }

        unsafe {
            let mut chan_config: esp_idf_sys::adc_oneshot_chan_cfg_t = core::mem::zeroed();
            chan_config.atten = ADC_ONESHOT_ATTEN;
            chan_config.bitwidth = ADC_ONESHOT_BITWIDTH;
            let err = esp_idf_sys::adc_oneshot_config_channel(
                self.adc_oneshot_unit,
                channel as esp_idf_sys::adc_channel_t,
                &chan_config,
            );
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("adc_oneshot_config_channel failed for channel {channel}: {err}");
                return false;
            }
            self.adc_oneshot_channels_configured |= 1u32 << channel;
        }

        true
    }

    fn ensure_adc_calibration(&mut self, channel: u32) -> bool {
        const ADC_ONESHOT_ATTEN: esp_idf_sys::adc_atten_t =
            esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11;
        const ADC_ONESHOT_BITWIDTH: esp_idf_sys::adc_bitwidth_t =
            esp_idf_sys::adc_bitwidth_t_ADC_BITWIDTH_12;

        if channel > 4 {
            warn!("adc calibration invalid ADC1 channel: {channel}");
            return false;
        }
        if !self.adc_cali_handles[channel as usize].is_null() {
            return true;
        }
        if !self.ensure_adc_oneshot_channel(channel) {
            return false;
        }

        unsafe {
            let mut cali_config: esp_idf_sys::adc_cali_curve_fitting_config_t = core::mem::zeroed();
            cali_config.unit_id = esp_idf_sys::adc_unit_t_ADC_UNIT_1;
            #[cfg(esp_idf_version_at_least_5_1_1)]
            {
                cali_config.chan = channel as esp_idf_sys::adc_channel_t;
            }
            cali_config.atten = ADC_ONESHOT_ATTEN;
            cali_config.bitwidth = ADC_ONESHOT_BITWIDTH;
            let mut handle: esp_idf_sys::adc_cali_handle_t = ptr::null_mut();
            let err = esp_idf_sys::adc_cali_create_scheme_curve_fitting(&cali_config, &mut handle);
            if err != esp_idf_sys::ESP_OK as i32 {
                warn!("adc_cali_create_scheme_curve_fitting failed for channel {channel}: {err}");
                return false;
            }
            self.adc_cali_handles[channel as usize] = handle;
        }

        true
    }

    fn adc_read_raw_oneshot(&mut self, channel: u32) -> i32 {
        if !self.ensure_adc_oneshot_channel(channel) {
            return -1;
        }

        let mut raw = 0i32;
        let err = unsafe {
            esp_idf_sys::adc_oneshot_read(
                self.adc_oneshot_unit,
                channel as esp_idf_sys::adc_channel_t,
                &mut raw,
            )
        };
        if err != esp_idf_sys::ESP_OK as i32 {
            warn!("adc_oneshot_read failed for channel {channel}: {err}");
            return -1;
        }

        raw
    }

    fn reset_adc_state(&mut self) {
        for handle in &mut self.adc_cali_handles {
            Self::delete_curve_fitting_calibration(*handle);
            *handle = ptr::null_mut();
        }
        Self::delete_oneshot_unit(self.adc_oneshot_unit);
        self.adc_oneshot_unit = ptr::null_mut();
        self.adc_oneshot_channels_configured = 0;
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
        self.adc_read_raw_oneshot(channel)
    }

    fn adc_read_mv(&mut self, channel: u32) -> i32 {
        let (_, mv) = self.adc_read_diagnostics(channel);
        mv
    }

    fn adc_read_diagnostics(&mut self, channel: u32) -> (i32, i32) {
        let raw = self.adc_read_raw_oneshot(channel);
        if raw < 0 {
            return (raw, raw);
        }
        if !self.ensure_adc_calibration(channel) {
            return (raw, -1);
        }

        let mut mv = 0i32;
        let err = unsafe {
            esp_idf_sys::adc_cali_raw_to_voltage(
                self.adc_cali_handles[channel as usize],
                raw,
                &mut mv,
            )
        };
        if err != esp_idf_sys::ESP_OK as i32 {
            warn!("adc_cali_raw_to_voltage failed for channel {channel}: {err}");
            return (raw, -1);
        }

        (raw, mv)
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
            Self::log_gpio_level(sensor_enable as i32, "idle-live", 1);
            Self::configure_sleep_output(sensor_enable as i32, 1);
            Self::log_gpio_level(sensor_enable as i32, "idle-sleep", 1);
        }

        self.gpio_output_configured = 0;
        self.reset_adc_state();
    }

    fn enter_active_gpio_state(&mut self) {
        if let Some(sensor_enable) = self.board_layout.sensor_enable {
            Self::set_output_level(sensor_enable as i32, 0);
            Self::log_gpio_level(sensor_enable as i32, "active-live", 0);
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
