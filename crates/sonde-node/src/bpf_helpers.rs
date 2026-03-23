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

    /// ND-0600: Helper ABI conformance — every helper ID matches its
    /// documented value and the numbering is contiguous 1..=16.
    /// A firmware update must never renumber helpers; this test catches
    /// any accidental change.
    #[test]
    fn test_helper_abi_conformance() {
        let expected: [(u32, &str); 16] = [
            (1, "I2C_READ"),
            (2, "I2C_WRITE"),
            (3, "I2C_WRITE_READ"),
            (4, "SPI_TRANSFER"),
            (5, "GPIO_READ"),
            (6, "GPIO_WRITE"),
            (7, "ADC_READ"),
            (8, "SEND"),
            (9, "SEND_RECV"),
            (10, "MAP_LOOKUP_ELEM"),
            (11, "MAP_UPDATE_ELEM"),
            (12, "GET_TIME"),
            (13, "GET_BATTERY_MV"),
            (14, "DELAY_US"),
            (15, "SET_NEXT_WAKE"),
            (16, "BPF_TRACE_PRINTK"),
        ];

        let actual = [
            helper_ids::I2C_READ,
            helper_ids::I2C_WRITE,
            helper_ids::I2C_WRITE_READ,
            helper_ids::SPI_TRANSFER,
            helper_ids::GPIO_READ,
            helper_ids::GPIO_WRITE,
            helper_ids::ADC_READ,
            helper_ids::SEND,
            helper_ids::SEND_RECV,
            helper_ids::MAP_LOOKUP_ELEM,
            helper_ids::MAP_UPDATE_ELEM,
            helper_ids::GET_TIME,
            helper_ids::GET_BATTERY_MV,
            helper_ids::DELAY_US,
            helper_ids::SET_NEXT_WAKE,
            helper_ids::BPF_TRACE_PRINTK,
        ];

        for (i, ((expected_id, name), actual_id)) in expected.iter().zip(actual.iter()).enumerate()
        {
            assert_eq!(
                *actual_id, *expected_id,
                "helper {name} at index {i}: expected ID {expected_id}, got {actual_id}"
            );
        }

        // Verify contiguous range 1..=16 (no gaps).
        assert_eq!(actual.len(), 16);
        for (i, id) in actual.iter().enumerate() {
            assert_eq!(*id, (i + 1) as u32, "helper IDs must be contiguous 1..=N");
        }
    }

    #[test]
    fn test_program_class() {
        assert_ne!(ProgramClass::Resident, ProgramClass::Ephemeral);
    }
}
