// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! NVS-backed persistent storage for the sensor node.
//!
//! Uses the ESP-IDF Non-Volatile Storage (NVS) API to persist keys,
//! schedule parameters, and program images across deep-sleep cycles.
//!
//! **NVS key mapping:**
//! - Key partition: `"psk"`, `"key_hint"`, `"magic"`
//! - Schedule: `"interval"`, `"active_part"`
//! - Programs: `"prog_a"`, `"prog_b"`
//! - Early wake flag: stored in RTC slow memory (survives deep sleep).
//!
//! **Status:** Stub — compiles under `--features esp` but panics at
//! runtime. The real implementation will be filled in during hardware
//! bring-up.

use crate::error::{NodeError, NodeResult};

/// Default wake interval in seconds (5 minutes).
const DEFAULT_INTERVAL_S: u32 = 300;

/// NVS-backed implementation of [`crate::traits::PlatformStorage`].
pub struct NvsStorage {
    _private: (),
}

impl NvsStorage {
    /// Open (or create) the `"sonde"` NVS namespace.
    pub fn new() -> Result<Self, NodeError> {
        todo!("NVS storage initialisation")
    }
}

impl crate::traits::PlatformStorage for NvsStorage {
    // --- Key partition ---

    fn read_key(&self) -> Option<(u16, [u8; 32])> {
        todo!("NvsStorage::read_key")
    }

    fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
        let _ = (key_hint, psk);
        todo!("NvsStorage::write_key")
    }

    fn erase_key(&mut self) -> NodeResult<()> {
        todo!("NvsStorage::erase_key")
    }

    // --- Schedule partition ---

    fn read_schedule(&self) -> (u32, u8) {
        let _ = DEFAULT_INTERVAL_S;
        todo!("NvsStorage::read_schedule")
    }

    fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
        let _ = interval_s;
        todo!("NvsStorage::write_schedule_interval")
    }

    fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
        let _ = partition;
        todo!("NvsStorage::write_active_partition")
    }

    fn reset_schedule(&mut self) -> NodeResult<()> {
        todo!("NvsStorage::reset_schedule")
    }

    // --- Program partitions ---

    fn read_program(&self, partition: u8) -> Option<Vec<u8>> {
        let _ = partition;
        todo!("NvsStorage::read_program")
    }

    fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
        let _ = (partition, image);
        todo!("NvsStorage::write_program")
    }

    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        let _ = partition;
        todo!("NvsStorage::erase_program")
    }

    // --- Wake reason flags ---

    fn take_early_wake_flag(&mut self) -> bool {
        todo!("NvsStorage::take_early_wake_flag")
    }

    fn set_early_wake_flag(&mut self) -> NodeResult<()> {
        todo!("NvsStorage::set_early_wake_flag")
    }
}
