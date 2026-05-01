// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Handle encoding for I2C: `(bus << 16) | 7-bit_addr` (low 7 bits of `addr`).
pub const fn i2c_handle(bus: u16, addr: u8) -> u32 {
    ((bus as u32) << 16) | ((addr as u32) & 0x7F)
}

/// Handle encoding for SPI: `(bus << 16)`.
pub const fn spi_handle(bus: u16) -> u32 {
    (bus as u32) << 16
}

/// Extract bus number from an I2C or SPI handle.
pub const fn handle_bus(handle: u32) -> u16 {
    (handle >> 16) as u16
}

/// Extract 7-bit device address from an I2C handle.
pub const fn handle_addr(handle: u32) -> u8 {
    (handle & 0x7F) as u8
}

/// Hardware abstraction layer for bus peripherals.
///
/// All methods return 0 on success, negative on error (NACK, timeout,
/// invalid pin/channel). The BPF program decides how to handle errors.
pub trait Hal {
    /// Read `buf_len` bytes from the I2C device at `handle`.
    fn i2c_read(&mut self, handle: u32, buf: &mut [u8]) -> i32;

    /// Write `data` bytes to the I2C device at `handle`.
    fn i2c_write(&mut self, handle: u32, data: &[u8]) -> i32;

    /// Combined I2C write-then-read in a single transaction (repeated start).
    fn i2c_write_read(&mut self, handle: u32, write_data: &[u8], read_buf: &mut [u8]) -> i32;

    /// In-place full-duplex SPI transfer.
    ///
    /// The buffer is read for transmit data, then overwritten with received
    /// data.  For receive-only transfers, fill the buffer with zeros.
    fn spi_transfer(&mut self, handle: u32, buf: &mut [u8]) -> i32;

    /// Read the state of a GPIO pin. Returns 0 (low), 1 (high), or negative on error.
    fn gpio_read(&self, pin: u32) -> i32;

    /// Set the state of a GPIO pin. Returns 0 on success, negative on error.
    fn gpio_write(&mut self, pin: u32, value: u32) -> i32;

    /// Read a raw value from an ADC channel.
    /// Returns the ADC reading on success, negative on error.
    fn adc_read(&mut self, channel: u32) -> i32;

    /// Read an ADC channel and convert it to millivolts.
    ///
    /// The default implementation matches the effective ESP32-C3 ADC1 raw
    /// range observed by this firmware path.
    fn adc_read_mv(&mut self, channel: u32) -> i32 {
        const ADC_APPROX_FULL_SCALE_MV: i64 = 2500;
        const ADC_APPROX_RAW_MAX: i64 = 2047;

        let raw = self.adc_read(channel);
        if raw < 0 {
            return raw;
        }

        ((raw as i64).saturating_mul(ADC_APPROX_FULL_SCALE_MV) / ADC_APPROX_RAW_MAX) as i32
    }

    /// Read an ADC channel and return both the raw code and converted millivolts.
    ///
    /// The default implementation uses a single raw sample and the same
    /// conversion used by `adc_read_mv` so callers can log the exact sample
    /// used for higher-level calculations.
    fn adc_read_diagnostics(&mut self, channel: u32) -> (i32, i32) {
        const ADC_APPROX_FULL_SCALE_MV: i64 = 2500;
        const ADC_APPROX_RAW_MAX: i64 = 2047;

        let raw = self.adc_read(channel);
        if raw < 0 {
            return (raw, raw);
        }

        let mv =
            ((raw as i64).saturating_mul(ADC_APPROX_FULL_SCALE_MV) / ADC_APPROX_RAW_MAX) as i32;
        (raw, mv)
    }

    /// Enter the paired board layout's idle GPIO state.
    ///
    /// In the idle state, provisioned I2C, 1-Wire, and battery-sense pins
    /// are high-impedance inputs with no pull resistors, while the
    /// provisioned `sensor_enable` pin is driven high so the sensor rail is off.
    ///
    /// The default implementation is a no-op (suitable for test mocks).
    fn enter_idle_gpio_state(&mut self) {}

    /// Enter the paired board layout's active GPIO state for BPF execution.
    ///
    /// In the active state, the provisioned `sensor_enable` pin is driven low
    /// so the sensor rail is on, the provisioned I2C and 1-Wire pins are
    /// configured as inputs with pull-ups enabled, and the provisioned
    /// battery-sense pin is configured as an input with no pull resistors.
    ///
    /// The default implementation is a no-op (suitable for test mocks).
    fn enter_active_gpio_state(&mut self) {}

    /// Prepare hardware for deep sleep by placing all peripherals and
    /// GPIOs into low-power states.
    ///
    /// Implementations should:
    /// - Deinitialize bus peripherals (I2C, SPI) to release their pins
    /// - Reset GPIO pins configured during the wake cycle
    /// - Restore the paired board layout's idle GPIO state
    /// - Clear ADC configuration
    ///
    /// This must be called immediately before entering deep sleep to
    /// minimize leakage current. See issue #517 for background.
    ///
    /// The default implementation is a no-op (suitable for test mocks).
    fn prepare_for_sleep(&mut self) {}
}

/// Read the current battery voltage in millivolts.
///
/// This is a system-level function, not a HAL bus operation.
/// Provided separately because `battery_mv` also appears in the
/// execution context and WAKE message.
pub trait BatteryReader {
    fn battery_mv(&self) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_encoding() {
        let h = i2c_handle(0, 0x48);
        assert_eq!(handle_bus(h), 0);
        assert_eq!(handle_addr(h), 0x48);

        let h2 = i2c_handle(1, 0x76);
        assert_eq!(handle_bus(h2), 1);
        assert_eq!(handle_addr(h2), 0x76);
    }

    #[test]
    fn test_spi_handle() {
        let h = spi_handle(2);
        assert_eq!(handle_bus(h), 2);
    }
}
