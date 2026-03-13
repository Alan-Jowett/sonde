// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP32 hardware abstraction: clock, stub HAL, and battery reader.
//!
//! The clock uses `esp_timer_get_time()` for monotonic time and
//! `std::thread::sleep` for delays (portable across ESP-IDF versions).
//!
//! The HAL stub returns errors for all bus operations. Real I2C/SPI/
//! GPIO/ADC drivers will be configured per-device when specific sensor
//! hardware is selected. The stub ensures the firmware boots and runs
//! BPF programs that don't use bus helpers.

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
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

/// Stub HAL — returns errors for all bus operations.
///
/// Real I2C/SPI/GPIO/ADC drivers will be configured per-device
/// when specific sensor hardware is selected. The stub ensures the
/// firmware boots and runs BPF programs that don't use bus helpers.
pub struct EspHal;

impl crate::hal::Hal for EspHal {
    fn i2c_read(&mut self, _handle: u32, _buf: &mut [u8]) -> i32 {
        -1
    }
    fn i2c_write(&mut self, _handle: u32, _data: &[u8]) -> i32 {
        -1
    }
    fn i2c_write_read(&mut self, _handle: u32, _w: &[u8], _r: &mut [u8]) -> i32 {
        -1
    }
    fn spi_transfer(
        &mut self,
        _handle: u32,
        _tx: Option<&[u8]>,
        _rx: Option<&mut [u8]>,
        _len: usize,
    ) -> i32 {
        -1
    }
    fn gpio_read(&self, _pin: u32) -> i32 {
        -1
    }
    fn gpio_write(&mut self, _pin: u32, _value: u32) -> i32 {
        -1
    }
    fn adc_read(&self, _channel: u32) -> i32 {
        -1
    }
}

/// Battery reader using a fixed estimate.
///
/// On real hardware this would read an ADC channel connected to a
/// voltage divider on the battery. For initial bring-up, return a
/// fixed value indicating "battery OK".
pub struct EspBatteryReader;

impl crate::hal::BatteryReader for EspBatteryReader {
    fn battery_mv(&self) -> u32 {
        3300 // Fixed estimate until ADC channel is configured
    }
}
