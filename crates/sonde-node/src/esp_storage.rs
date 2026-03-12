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
    pub fn new(partition: EspNvsPartition<NvsDefault>) -> Result<Self, NodeError> {
        let nvs = EspNvs::new(partition, NVS_NAMESPACE, true)
            .map_err(|e| NodeError::StorageError(format!("NVS open: {:?}", e)))?;
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

        let key_hint = self.nvs.get_u32("key_hint").ok().flatten()? as u16;
        let mut buf = [0u8; 32];
        self.nvs.get_blob("psk", &mut buf).ok().flatten()?;
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
        let _ = self.nvs.remove("psk");
        let _ = self.nvs.remove("key_hint");
        let _ = self.nvs.remove("magic");
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
        let active = self.nvs.get_u32("active_p").ok().flatten().unwrap_or(0) as u8;
        (interval, active)
    }

    fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
        self.nvs
            .set_u32("interval", interval_s)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }

    fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
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
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        let mut buf = vec![0u8; 4096];
        match self.nvs.get_blob(key, &mut buf) {
            Ok(Some(slice)) => Some(slice.to_vec()),
            _ => None,
        }
    }

    fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        self.nvs
            .set_blob(key, image)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }

    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        let _ = self.nvs.remove(key);
        Ok(())
    }

    // --- Wake reason flags ---

    fn take_early_wake_flag(&mut self) -> bool {
        let flag = self.nvs.get_u32("early_wake").ok().flatten().unwrap_or(0);
        if flag != 0 {
            let _ = self.nvs.set_u32("early_wake", 0);
            true
        } else {
            false
        }
    }

    fn set_early_wake_flag(&mut self) -> NodeResult<()> {
        self.nvs
            .set_u32("early_wake", 1)
            .map_err(|e| NodeError::StorageError(format!("{:?}", e)))
    }
}
