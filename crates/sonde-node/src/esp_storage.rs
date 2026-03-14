// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! NVS-backed persistent storage for the sensor node.
//!
//! Uses the ESP-IDF Non-Volatile Storage (NVS) API to persist keys,
//! schedule parameters, and program images across deep-sleep cycles.
//!
//! **NVS key mapping** (namespace `"sonde"`):
//! - Key partition: `"psk"` (32-byte blob), `"key_hint"` (u32), `"magic"` (u32)
//! - Schedule: `"interval"` (u32), `"active_p"` (u32, 0 or 1)
//! - Programs: `"prog_a"` (blob, ≤4096 B), `"prog_b"` (blob, ≤4096 B)
//! - WiFi channel: `"channel"` (u32, 1–13)
//!
//! The early-wake flag is stored in RTC slow SRAM (`.rtc.data` section)
//! rather than NVS, so it survives deep sleep without incurring flash wear.
//! It is reset on power loss or hardware reset, which is acceptable — a
//! missed early wake is harmless.

use core::sync::atomic::{AtomicU32, Ordering};

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsDefault};

use crate::error::{NodeError, NodeResult};

const NVS_NAMESPACE: &str = "sonde";
const MAGIC_VALUE: u32 = 0xDEAD_BEEF;

/// Default wake interval in seconds (5 minutes).
const DEFAULT_INTERVAL_S: u32 = 300;

/// Early-wake flag stored in RTC slow SRAM.
///
/// Survives ESP32 deep sleep but is reset to 0 on power loss or hardware
/// reset (acceptable — a missed early wake is harmless). Using RTC SRAM
/// eliminates all flash wear that the previous NVS-backed implementation
/// incurred on every wake cycle.
#[link_section = ".rtc.data"]
static EARLY_WAKE_FLAG: AtomicU32 = AtomicU32::new(0);

/// NVS-backed implementation of [`crate::traits::PlatformStorage`].
pub struct NvsStorage {
    nvs: EspNvs<NvsDefault>,
}

impl NvsStorage {
    /// Open (or create) the `"sonde"` NVS namespace.
    pub fn new(partition: EspNvsPartition<NvsDefault>) -> Result<Self, NodeError> {
        let nvs = EspNvs::new(partition, NVS_NAMESPACE, true)
            .map_err(|_| NodeError::StorageError("NVS open failed"))?;
        Ok(Self { nvs })
    }
}

impl crate::traits::PlatformStorage for NvsStorage {
    // --- Key partition ---

    fn read_key(&self) -> Option<(u16, [u8; 32])> {
        let magic = self.nvs.get_u32("magic").ok().flatten()?;
        if magic != MAGIC_VALUE {
            return None;
        }

        let key_hint = self.nvs.get_u32("key_hint").ok().flatten()?;
        if key_hint > u16::MAX as u32 {
            return None;
        }
        let key_hint = key_hint as u16;
        let mut buf = [0u8; 32];
        let slice = self.nvs.get_blob("psk", &mut buf).ok().flatten()?;
        if slice.len() != 32 {
            return None;
        }
        Some((key_hint, buf))
    }

    fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
        if self.read_key().is_some() {
            return Err(NodeError::StorageError("already paired"));
        }
        self.nvs
            .set_blob("psk", psk)
            .map_err(|_| NodeError::StorageError("psk write failed"))?;
        self.nvs
            .set_u32("key_hint", key_hint as u32)
            .map_err(|_| NodeError::StorageError("key_hint write failed"))?;
        self.nvs
            .set_u32("magic", MAGIC_VALUE)
            .map_err(|_| NodeError::StorageError("magic write failed"))?;
        Ok(())
    }

    fn erase_key(&mut self) -> NodeResult<()> {
        self.nvs
            .remove("psk")
            .map_err(|_| NodeError::StorageError("erase psk failed"))?;
        self.nvs
            .remove("key_hint")
            .map_err(|_| NodeError::StorageError("erase key_hint failed"))?;
        self.nvs
            .remove("magic")
            .map_err(|_| NodeError::StorageError("erase magic failed"))?;
        Ok(())
    }

    // --- Schedule partition ---

    fn read_schedule(&self) -> (u32, u8) {
        let interval = self
            .nvs
            .get_u32("interval")
            .ok()
            .flatten()
            .unwrap_or(DEFAULT_INTERVAL_S);
        let active = self
            .nvs
            .get_u32("active_p")
            .ok()
            .flatten()
            .unwrap_or(0)
            .min(1) as u8;
        (interval, active)
    }

    fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
        self.nvs
            .set_u32("interval", interval_s)
            .map_err(|_| NodeError::StorageError("interval write failed"))
    }

    fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError("invalid active partition index"));
        }
        self.nvs
            .set_u32("active_p", partition as u32)
            .map_err(|_| NodeError::StorageError("active_p write failed"))
    }

    fn reset_schedule(&mut self) -> NodeResult<()> {
        self.nvs
            .set_u32("interval", DEFAULT_INTERVAL_S)
            .map_err(|_| NodeError::StorageError("interval reset failed"))?;
        self.nvs
            .set_u32("active_p", 0)
            .map_err(|_| NodeError::StorageError("active_p reset failed"))
    }

    // --- Program partitions ---

    fn read_program(&self, partition: u8) -> Option<Vec<u8>> {
        if partition >= 2 {
            return None;
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        let mut buf = vec![0u8; 4096];
        match self.nvs.get_blob(key, &mut buf) {
            Ok(Some(slice)) => {
                let len = slice.len();
                buf.truncate(len);
                Some(buf)
            }
            _ => None,
        }
    }

    fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError("invalid program partition index"));
        }
        if image.len() > 4096 {
            return Err(NodeError::StorageError("program image too large"));
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        self.nvs
            .set_blob(key, image)
            .map_err(|_| NodeError::StorageError("program write failed"))
    }

    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError("invalid program partition index"));
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        self.nvs
            .remove(key)
            .map_err(|_| NodeError::StorageError("program erase failed"))?;
        Ok(())
    }

    // --- Wake reason flags ---

    fn take_early_wake_flag(&mut self) -> bool {
        EARLY_WAKE_FLAG.swap(0, Ordering::Relaxed) != 0
    }

    fn set_early_wake_flag(&mut self) -> NodeResult<()> {
<<<<<<< HEAD
        EARLY_WAKE_FLAG.store(1, Ordering::Relaxed);
        Ok(())
=======
        let current = self.nvs.get_u32("early_wake").ok().flatten().unwrap_or(0);
        if current == 1 {
            return Ok(());
        }
        self.nvs
            .set_u32("early_wake", 1)
            .map_err(|_| NodeError::StorageError("early_wake write failed"))
>>>>>>> bb23fa0 (Replace String payloads with &'static str in NodeError and BpfError to eliminate heap allocation in error paths)
    }

    // --- WiFi channel ---

    fn read_channel(&self) -> Option<u8> {
        let ch = self.nvs.get_u32("channel").ok().flatten()?;
        if ch >= 1 && ch <= 13 {
            Some(ch as u8)
        } else {
            None
        }
    }

    fn write_channel(&mut self, channel: u8) -> NodeResult<()> {
        if channel < 1 || channel > 13 {
            return Err(NodeError::StorageError("invalid WiFi channel"));
        }
        self.nvs
            .set_u32("channel", channel as u32)
            .map_err(|_| NodeError::StorageError("channel write failed"))
    }
}
