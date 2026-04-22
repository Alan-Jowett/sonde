// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! SSD1306 OLED display driver for the ESP32-S3 modem target.

use crate::bridge::{Display, DisplayError};
use sonde_protocol::modem::DISPLAY_FRAME_BODY_SIZE;

const I2C0_FREQ_HZ: u32 = 100_000;
const I2C_TIMEOUT_TICKS: u32 = 1000;
const OLED_ADDR: u8 = 0x3C;
const OLED_SDA_GPIO: i32 = esp_idf_sys::gpio_num_t_GPIO_NUM_5;
const OLED_SCL_GPIO: i32 = esp_idf_sys::gpio_num_t_GPIO_NUM_6;

const SSD1306_INIT: &[u8] = &[
    0xAE, 0x20, 0x02, 0xB0, 0xC8, 0x00, 0x10, 0x40, 0x81, 0x7F, 0xA1, 0xA6, 0xA8, 0x3F, 0xA4, 0xD3,
    0x00, 0xD5, 0x80, 0xD9, 0xF1, 0xDA, 0x12, 0xDB, 0x20, 0x8D, 0x14, 0xAF,
];

pub struct EspSsd1306Display {
    framebuffer: [u8; DISPLAY_FRAME_BODY_SIZE],
    page_buffer: [u8; 128],
    flush_page: u8,
    flush_pending: bool,
    initialized: bool,
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
        })
    }

    fn write_transaction(&self, control: u8, payload: &[u8]) -> Result<(), DisplayError> {
        unsafe {
            let cmd = esp_idf_sys::i2c_cmd_link_create();
            if cmd.is_null() {
                return Err(DisplayError::WriteFailed);
            }
            esp_idf_sys::i2c_master_start(cmd);
            esp_idf_sys::i2c_master_write_byte(cmd, OLED_ADDR << 1, true);
            esp_idf_sys::i2c_master_write_byte(cmd, control, true);
            if !payload.is_empty() {
                esp_idf_sys::i2c_master_write(cmd, payload.as_ptr(), payload.len(), true);
            }
            esp_idf_sys::i2c_master_stop(cmd);
            let err = esp_idf_sys::i2c_master_cmd_begin(
                esp_idf_sys::i2c_port_t_I2C_NUM_0,
                cmd,
                I2C_TIMEOUT_TICKS,
            );
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

impl Display for EspSsd1306Display {
    fn queue_frame(&mut self, framebuffer: [u8; DISPLAY_FRAME_BODY_SIZE]) {
        self.framebuffer = framebuffer;
        self.flush_page = 0;
        self.flush_pending = true;
    }

    fn reset(&mut self) {
        self.flush_page = 0;
        self.flush_pending = false;
    }

    fn poll(&mut self) -> Result<(), DisplayError> {
        if !self.flush_pending {
            return Ok(());
        }

        if !self.initialized {
            if let Err(err) = self.write_commands(SSD1306_INIT) {
                self.flush_pending = false;
                return Err(err);
            }
            self.initialized = true;
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

impl Drop for EspSsd1306Display {
    fn drop(&mut self) {
        unsafe {
            let _ = esp_idf_sys::i2c_driver_delete(esp_idf_sys::i2c_port_t_I2C_NUM_0);
        }
    }
}
