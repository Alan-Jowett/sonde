// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Handle encoding for I2C: `(bus << 16) | 7-bit_addr`.
pub const fn i2c_handle(bus: u16, addr: u8) -> u32 {
    ((bus as u32) << 16) | (addr as u32)
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
    fn i2c_write_read(
        &mut self,
        handle: u32,
        write_data: &[u8],
        read_buf: &mut [u8],
    ) -> i32;

    /// Full-duplex SPI transfer.
    fn spi_transfer(
        &mut self,
        handle: u32,
        tx: Option<&[u8]>,
        rx: Option<&mut [u8]>,
        len: usize,
    ) -> i32;

    /// Read the state of a GPIO pin. Returns 0 (low), 1 (high), or negative on error.
    fn gpio_read(&self, pin: u32) -> i32;

    /// Set the state of a GPIO pin. Returns 0 on success, negative on error.
    fn gpio_write(&mut self, pin: u32, value: u32) -> i32;

    /// Read a raw value from an ADC channel.
    /// Returns the ADC reading on success, negative on error.
    fn adc_read(&self, channel: u32) -> i32;
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
