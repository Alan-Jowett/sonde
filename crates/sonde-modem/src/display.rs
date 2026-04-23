// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! SSD1306 OLED display driver for the ESP32-S3 modem target.

use std::time::{Duration, Instant};

use esp_idf_hal::delay::TICK_RATE_HZ;

use crate::bridge::{Display, DisplayError};
use sonde_protocol::modem::DISPLAY_FRAME_BODY_SIZE;

const I2C0_FREQ_HZ: u32 = 100_000;
const I2C_TIMEOUT_MS: u32 = 25;
const OLED_ADDR: u8 = 0x3C;
const OLED_SDA_GPIO: i32 = esp_idf_sys::gpio_num_t_GPIO_NUM_5;
const OLED_SCL_GPIO: i32 = esp_idf_sys::gpio_num_t_GPIO_NUM_6;
const DISPLAY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const SSD1306_DISPLAY_OFF: u8 = 0xAE;
const SSD1306_DISPLAY_ON: u8 = 0xAF;

const SSD1306_INIT: &[u8] = &[
    SSD1306_DISPLAY_OFF,
    0x20,
    0x02,
    0xB0,
    0xC8,
    0x00,
    0x10,
    0x40,
    0x81,
    0x7F,
    0xA1,
    0xA6,
    0xA8,
    0x3F,
    0xA4,
    0xD3,
    0x00,
    0xD5,
    0x80,
    0xD9,
    0xF1,
    0xDA,
    0x12,
    0xDB,
    0x20,
    0x8D,
    0x14,
    SSD1306_DISPLAY_ON,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelPowerCommand {
    None,
    Wake,
    Sleep,
}

#[derive(Debug, Clone)]
struct IdlePowerState {
    panel_awake: bool,
    last_frame_at: Option<Instant>,
}

impl Default for IdlePowerState {
    fn default() -> Self {
        Self {
            panel_awake: false,
            last_frame_at: None,
        }
    }
}

impl IdlePowerState {
    fn note_frame_accepted_at(&mut self, now: Instant) {
        self.last_frame_at = Some(now);
    }

    fn mark_initialized(&mut self) {
        self.panel_awake = true;
    }

    fn reset(&mut self) {}

    fn poll_at(
        &mut self,
        now: Instant,
        flush_pending: bool,
        initialized: bool,
    ) -> PanelPowerCommand {
        if flush_pending {
            if initialized && !self.panel_awake {
                self.panel_awake = true;
                return PanelPowerCommand::Wake;
            }
            return PanelPowerCommand::None;
        }

        if self.panel_awake
            && self.last_frame_at.is_some_and(|last_frame_at| {
                now.duration_since(last_frame_at) >= DISPLAY_IDLE_TIMEOUT
            })
        {
            self.panel_awake = false;
            return PanelPowerCommand::Sleep;
        }

        PanelPowerCommand::None
    }
}

pub struct EspSsd1306Display {
    framebuffer: [u8; DISPLAY_FRAME_BODY_SIZE],
    page_buffer: [u8; 128],
    flush_page: u8,
    flush_pending: bool,
    initialized: bool,
    idle_power: IdlePowerState,
}

/// Display wrapper that degrades to recoverable write failures if the display
/// path is unavailable.
///
/// `EspSsd1306Display::new()` performs the I2C setup at boot. The SSD1306 init
/// sequence itself is deferred until the first flush in `poll()`.
pub struct ModemDisplay {
    inner: Option<EspSsd1306Display>,
    pending_error: bool,
}

impl EspSsd1306Display {
    pub fn new() -> Result<Self, i32> {
        unsafe {
            let port = esp_idf_sys::i2c_port_t_I2C_NUM_0;
            let mut conf: esp_idf_sys::i2c_config_t = core::mem::zeroed();
            conf.mode = esp_idf_sys::i2c_mode_t_I2C_MODE_MASTER;
            conf.sda_io_num = OLED_SDA_GPIO;
            conf.scl_io_num = OLED_SCL_GPIO;
            conf.sda_pullup_en = true;
            conf.scl_pullup_en = true;
            conf.__bindgen_anon_1.master.clk_speed = I2C0_FREQ_HZ;

            let err = esp_idf_sys::i2c_param_config(port, &conf);
            if err != esp_idf_sys::ESP_OK as i32 {
                return Err(err);
            }
            let err = esp_idf_sys::i2c_driver_install(port, conf.mode, 0, 0, 0);
            if err != esp_idf_sys::ESP_OK as i32 {
                return Err(err);
            }
        }

        Ok(Self {
            framebuffer: [0u8; DISPLAY_FRAME_BODY_SIZE],
            page_buffer: [0u8; 128],
            flush_page: 0,
            flush_pending: false,
            initialized: false,
            idle_power: IdlePowerState::default(),
        })
    }

    fn write_transaction(&self, control: u8, payload: &[u8]) -> Result<(), DisplayError> {
        unsafe {
            let cmd = esp_idf_sys::i2c_cmd_link_create();
            if cmd.is_null() {
                return Err(DisplayError::WriteFailed);
            }

            let mut err = esp_idf_sys::i2c_master_start(cmd);
            if err == esp_idf_sys::ESP_OK as i32 {
                err = esp_idf_sys::i2c_master_write_byte(cmd, OLED_ADDR << 1, true);
            }
            if err == esp_idf_sys::ESP_OK as i32 {
                err = esp_idf_sys::i2c_master_write_byte(cmd, control, true);
            }
            if err == esp_idf_sys::ESP_OK as i32 && !payload.is_empty() {
                err = esp_idf_sys::i2c_master_write(cmd, payload.as_ptr(), payload.len(), true);
            }
            if err == esp_idf_sys::ESP_OK as i32 {
                err = esp_idf_sys::i2c_master_stop(cmd);
            }
            if err == esp_idf_sys::ESP_OK as i32 {
                err = esp_idf_sys::i2c_master_cmd_begin(
                    esp_idf_sys::i2c_port_t_I2C_NUM_0,
                    cmd,
                    i2c_timeout_ticks(),
                );
            }

            esp_idf_sys::i2c_cmd_link_delete(cmd);
            if err != esp_idf_sys::ESP_OK as i32 {
                return Err(DisplayError::WriteFailed);
            }
        }

        Ok(())
    }

    fn write_commands(&self, commands: &[u8]) -> Result<(), DisplayError> {
        self.write_transaction(0x00, commands)
    }

    fn write_data(&self, data: &[u8]) -> Result<(), DisplayError> {
        self.write_transaction(0x40, data)
    }

    fn fill_page_buffer(&mut self, page: u8) {
        let page_y = (page as usize) * 8;
        for x in 0..128usize {
            let mut page_byte = 0u8;
            for bit in 0..8usize {
                let y = page_y + bit;
                let src_index = y * 16 + (x / 8);
                let src_mask = 0x80 >> (x % 8);
                if self.framebuffer[src_index] & src_mask != 0 {
                    page_byte |= 1 << bit;
                }
            }
            self.page_buffer[x] = page_byte;
        }
    }
}

impl ModemDisplay {
    pub fn new(inner: EspSsd1306Display) -> Self {
        Self {
            inner: Some(inner),
            pending_error: false,
        }
    }

    pub fn disabled() -> Self {
        Self {
            inner: None,
            pending_error: false,
        }
    }
}

fn i2c_timeout_ticks() -> u32 {
    I2C_TIMEOUT_MS
        .saturating_mul(TICK_RATE_HZ.max(1))
        .div_ceil(1000)
        .max(1)
}

impl Display for EspSsd1306Display {
    fn queue_frame(&mut self, framebuffer: [u8; DISPLAY_FRAME_BODY_SIZE]) {
        self.framebuffer = framebuffer;
        self.flush_page = 0;
        self.flush_pending = true;
        self.idle_power.note_frame_accepted_at(Instant::now());
    }

    fn reset(&mut self) {
        self.flush_page = 0;
        self.flush_pending = false;
        self.idle_power.reset();
    }

    fn poll(&mut self) -> Result<(), DisplayError> {
        let now = Instant::now();

        if !self.flush_pending {
            if self.idle_power.poll_at(now, false, self.initialized) == PanelPowerCommand::Sleep {
                self.write_commands(&[SSD1306_DISPLAY_OFF])?;
            }
            return Ok(());
        }

        if !self.initialized {
            if let Err(err) = self.write_commands(SSD1306_INIT) {
                self.flush_pending = false;
                return Err(err);
            }
            self.initialized = true;
            self.idle_power.mark_initialized();
            return Ok(());
        }

        if self.idle_power.poll_at(now, true, self.initialized) == PanelPowerCommand::Wake {
            if let Err(err) = self.write_commands(&[SSD1306_DISPLAY_ON]) {
                self.flush_pending = false;
                return Err(err);
            }
            return Ok(());
        }

        let page = self.flush_page;
        self.fill_page_buffer(page);
        if let Err(err) = self.write_commands(&[0xB0 + page, 0x00, 0x10]) {
            self.flush_pending = false;
            return Err(err);
        }
        if let Err(err) = self.write_data(&self.page_buffer) {
            self.flush_pending = false;
            return Err(err);
        }

        if page == 7 {
            self.flush_pending = false;
        } else {
            self.flush_page += 1;
        }
        Ok(())
    }
}

impl Display for ModemDisplay {
    fn queue_frame(&mut self, framebuffer: [u8; DISPLAY_FRAME_BODY_SIZE]) {
        match self.inner.as_mut() {
            Some(inner) => inner.queue_frame(framebuffer),
            None => self.pending_error = true,
        }
    }

    fn reset(&mut self) {
        self.pending_error = false;
        if let Some(inner) = self.inner.as_mut() {
            inner.reset();
        }
    }

    fn poll(&mut self) -> Result<(), DisplayError> {
        if self.pending_error {
            self.pending_error = false;
            return Err(DisplayError::WriteFailed);
        }

        match self.inner.as_mut() {
            Some(inner) => inner.poll(),
            None => Ok(()),
        }
    }
}

impl Drop for EspSsd1306Display {
    fn drop(&mut self) {
        unsafe {
            let _ = esp_idf_sys::i2c_driver_delete(esp_idf_sys::i2c_port_t_I2C_NUM_0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_power_state_sleeps_after_timeout() {
        let mut idle_power = IdlePowerState::default();
        let t0 = Instant::now();
        idle_power.mark_initialized();
        idle_power.note_frame_accepted_at(t0);

        assert_eq!(
            idle_power.poll_at(
                t0 + DISPLAY_IDLE_TIMEOUT - Duration::from_secs(1),
                false,
                true
            ),
            PanelPowerCommand::None
        );
        assert_eq!(
            idle_power.poll_at(t0 + DISPLAY_IDLE_TIMEOUT, false, true),
            PanelPowerCommand::Sleep
        );
        assert_eq!(
            idle_power.poll_at(
                t0 + DISPLAY_IDLE_TIMEOUT + Duration::from_secs(1),
                false,
                true
            ),
            PanelPowerCommand::None
        );
    }

    #[test]
    fn idle_power_state_wakes_on_new_frame_after_sleep() {
        let mut idle_power = IdlePowerState::default();
        let t0 = Instant::now();
        idle_power.mark_initialized();
        idle_power.note_frame_accepted_at(t0);
        assert_eq!(
            idle_power.poll_at(t0 + DISPLAY_IDLE_TIMEOUT, false, true),
            PanelPowerCommand::Sleep
        );

        let t1 = t0 + DISPLAY_IDLE_TIMEOUT + Duration::from_secs(5);
        idle_power.note_frame_accepted_at(t1);
        assert_eq!(idle_power.poll_at(t1, true, true), PanelPowerCommand::Wake);
        assert_eq!(
            idle_power.poll_at(
                t1 + DISPLAY_IDLE_TIMEOUT - Duration::from_secs(1),
                false,
                true
            ),
            PanelPowerCommand::None
        );
    }

    #[test]
    fn idle_power_state_reset_preserves_sleep_deadline() {
        let mut idle_power = IdlePowerState::default();
        let t0 = Instant::now();
        idle_power.mark_initialized();
        idle_power.note_frame_accepted_at(t0);
        idle_power.reset();

        assert_eq!(
            idle_power.poll_at(t0 + DISPLAY_IDLE_TIMEOUT, false, true),
            PanelPowerCommand::Sleep
        );
    }
}
