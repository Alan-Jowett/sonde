// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Deep-sleep and reboot controller for ESP32.
//!
//! Uses the ESP-IDF timer-based deep-sleep API. When [`EspSleepController::enter_deep_sleep`]
//! is called the CPU is powered down and RAM contents are lost (except
//! RTC slow memory). On wake-up the firmware starts from the reset
//! vector — there is no "return" from deep sleep.

/// [`crate::traits::SleepController`] implementation for ESP32.
pub struct EspSleepController;

impl crate::traits::SleepController for EspSleepController {
    fn enter_deep_sleep(&mut self, seconds: u32) -> ! {
        let micros = (seconds as u64) * 1_000_000;
        unsafe {
            esp_idf_sys::esp_sleep_enable_timer_wakeup(micros);
            esp_idf_sys::esp_deep_sleep_start();
        }
        unreachable!()
    }

    fn reboot(&mut self) -> ! {
        unsafe {
            esp_idf_sys::esp_restart();
        }
        unreachable!()
    }
}
