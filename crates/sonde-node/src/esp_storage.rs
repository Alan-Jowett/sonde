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
//! - Early wake flag: `"early_wake"` (u32, 0 or 1)

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsDefault};

use crate::error::{NodeError, NodeResult};

const NVS_NAMESPACE: &str = "sonde";
const MAGIC_VALUE: u32 = 0xDEAD_BEEF;

/// Default wake interval in seconds (5 minutes).
const DEFAULT_INTERVAL_S: u32 = 300;

/// NVS-backed implementation of [`crate::traits::PlatformStorage`].
pub struct NvsStorage {
    nvs: EspNvs<NvsDefault>,
}

impl NvsStorage {
    /// Open (or create) the `"sonde"` NVS namespace.
    ///
    /// Clears the `early_wake` flag on construction. Because NVS
    /// survives power loss (unlike RTC SRAM), a stale flag from a
    /// prior cycle could persist across an unexpected reboot. Clearing
    /// on boot ensures the flag only takes effect for the wake cycle
    /// that set it.
    pub fn new(partition: EspNvsPartition<NvsDefault>) -> Result<Self, NodeError> {
        let mut nvs = EspNvs::new(partition, NVS_NAMESPACE, true)
            .map_err(|e| NodeError::StorageError(format!("NVS open: {:?}", e)))?;
        // Clear stale early-wake flag from prior cycle / unexpected reboot.
        let _ = nvs.set_u32("early_wake", 0);
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
            return Err(NodeError::StorageError("already paired".into()));
        }
        self.nvs
            .set_blob("psk", psk)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))?;
        self.nvs
            .set_u32("key_hint", key_hint as u32)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))?;
        self.nvs
            .set_u32("magic", MAGIC_VALUE)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))?;
        Ok(())
    }

    fn erase_key(&mut self) -> NodeResult<()> {
        self.nvs
            .remove("psk")
            .map_err(|e| NodeError::StorageError(format!("erase psk: {:?}", e)))?;
        self.nvs
            .remove("key_hint")
            .map_err(|e| NodeError::StorageError(format!("erase key_hint: {:?}", e)))?;
        self.nvs
            .remove("magic")
            .map_err(|e| NodeError::StorageError(format!("erase magic: {:?}", e)))?;
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
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }

    fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError(format!(
                "invalid partition: {} (must be 0 or 1)",
                partition
            )));
        }
        self.nvs
            .set_u32("active_p", partition as u32)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }

    fn reset_schedule(&mut self) -> NodeResult<()> {
        self.nvs
            .set_u32("interval", DEFAULT_INTERVAL_S)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))?;
        self.nvs
            .set_u32("active_p", 0)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }

    // --- Program partitions ---

    fn read_program(&self, partition: u8) -> Option<Vec<u8>> {
        if partition >= 2 {
            return None;
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        let mut buf = vec![0u8; 4096];
        match self.nvs.get_blob(key, &mut buf) {
            Ok(Some(slice)) => Some(slice.to_vec()),
            _ => None,
        }
    }

    fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError(format!(
                "invalid partition: {} (must be 0 or 1)",
                partition
            )));
        }
        if image.len() > 4096 {
            return Err(NodeError::StorageError(format!(
                "program image too large: {} bytes (max 4096)",
                image.len()
            )));
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        self.nvs
            .set_blob(key, image)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }

    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError(format!(
                "invalid partition: {} (must be 0 or 1)",
                partition
            )));
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        self.nvs
            .remove(key)
            .map_err(|e| NodeError::StorageError(format!("erase {}: {:?}", key, e)))?;
        Ok(())
    }

    // --- Wake reason flags ---

    fn take_early_wake_flag(&mut self) -> bool {
        let flag = self.nvs.get_u32("early_wake").ok().flatten().unwrap_or(0);
        if flag != 0 {
            if let Err(e) = self.nvs.set_u32("early_wake", 0) {
                log::warn!("failed to clear early_wake flag: {:?}", e);
            }
            true
        } else {
            false
        }
    }

    // NOTE: Early-wake flag is stored in NVS, not RTC SRAM. NVS is simpler
    // and survives power loss, but incurs flash wear on every cycle. RTC SRAM
    // would eliminate wear since it is a raw memory region, but requires
    // linker-section tricks and is lost on power-off. This is a known
    // tradeoff; NVS is acceptable for the current wake-interval range.
    fn set_early_wake_flag(&mut self) -> NodeResult<()> {
        let current = self.nvs.get_u32("early_wake").ok().flatten().unwrap_or(0);
        if current == 1 {
            return Ok(());
        }
        self.nvs
            .set_u32("early_wake", 1)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }
}
