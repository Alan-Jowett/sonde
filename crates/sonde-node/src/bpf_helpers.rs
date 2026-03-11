// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// BPF helper call numbers. These are part of the firmware ABI and
/// MUST NOT change between firmware versions.
pub mod helper_ids {
    pub const I2C_READ: u32 = 1;
    pub const I2C_WRITE: u32 = 2;
    pub const I2C_WRITE_READ: u32 = 3;
    pub const SPI_TRANSFER: u32 = 4;
    pub const GPIO_READ: u32 = 5;
    pub const GPIO_WRITE: u32 = 6;
    pub const ADC_READ: u32 = 7;
    pub const SEND: u32 = 8;
    pub const SEND_RECV: u32 = 9;
    pub const MAP_LOOKUP_ELEM: u32 = 10;
    pub const MAP_UPDATE_ELEM: u32 = 11;
    pub const GET_TIME: u32 = 12;
    pub const GET_BATTERY_MV: u32 = 13;
    pub const DELAY_US: u32 = 14;
    pub const SET_NEXT_WAKE: u32 = 15;
    pub const BPF_TRACE_PRINTK: u32 = 16;
}

/// Identifies whether the currently executing program is resident or ephemeral.
/// Used by the helper dispatcher to enforce ephemeral restrictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramClass {
    Resident,
    Ephemeral,
}

/// The execution context structure passed to BPF programs as R1.
///
/// This must match the C struct layout in bpf-environment.md §4:
/// ```c
/// struct sonde_context {
///     uint64_t timestamp;
///     uint16_t battery_mv;
///     uint16_t firmware_abi_version;
///     uint8_t  wake_reason;
///     uint8_t  _padding[3];
/// };
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SondeContext {
    pub timestamp: u64,
    pub battery_mv: u16,
    pub firmware_abi_version: u16,
    pub wake_reason: u8,
    /// Explicit padding to match C struct layout (3 bytes trailing padding).
    pub _padding: [u8; 3],
}

impl SondeContext {
    /// Size of this struct in bytes (for BPF memory access bounds checking).
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sonde_context_size() {
        // timestamp(8) + battery_mv(2) + firmware_abi_version(2) + wake_reason(1) + padding(3) = 16
        assert_eq!(SondeContext::SIZE, 16);
    }

    #[test]
    fn test_helper_ids_are_sequential() {
        assert_eq!(helper_ids::I2C_READ, 1);
        assert_eq!(helper_ids::BPF_TRACE_PRINTK, 16);
    }

    #[test]
    fn test_program_class() {
        assert_ne!(ProgramClass::Resident, ProgramClass::Ephemeral);
    }
}
