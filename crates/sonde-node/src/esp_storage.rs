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
//! - BLE pairing (ND-0916): `"peer_payload"` (blob, variable), `"reg_complete"` (u32, 0 or 1)
//! - I2C pin config (ND-0608): `"i2c0_sda"` (u32, default 0), `"i2c0_scl"` (u32, default 1)
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
            return Err(NodeError::StorageError(
                "invalid active partition index (must be 0 or 1)",
            ));
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
            return Err(NodeError::StorageError(
                "invalid program partition index (must be 0 or 1)",
            ));
        }
        if image.len() > 4096 {
            return Err(NodeError::StorageError(
                "program image too large (max 4096 bytes)",
            ));
        }
        let key = if partition == 0 { "prog_a" } else { "prog_b" };
        self.nvs
            .set_blob(key, image)
            .map_err(|_| NodeError::StorageError("program write failed"))
    }

    fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
        if partition > 1 {
            return Err(NodeError::StorageError(
                "invalid program partition index (must be 0 or 1)",
            ));
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
        EARLY_WAKE_FLAG.store(1, Ordering::Relaxed);
        Ok(())
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
            return Err(NodeError::StorageError(
                "invalid WiFi channel (must be 1-13)",
            ));
        }
        self.nvs
            .set_u32("channel", channel as u32)
            .map_err(|_| NodeError::StorageError("channel write failed"))
    }

    // --- BLE pairing artifacts (ND-0916) ---

    fn read_peer_payload(&self) -> Option<Vec<u8>> {
        // ESP-IDF NVS blob reads require a caller-supplied buffer sized to the
        // stored blob.  We use a 512-byte buffer which is sufficient for the
        // AES-256-GCM encrypted pairing payload (44 + ≤ ~256 bytes of CBOR).
        let mut buf = vec![0u8; 512];
        match self.nvs.get_blob("peer_payload", &mut buf) {
            Ok(Some(slice)) => {
                let len = slice.len();
                buf.truncate(len);
                Some(buf)
            }
            _ => None,
        }
    }

    fn has_peer_payload(&self) -> bool {
        // Check NVS key existence without allocating a read buffer.
        // get_blob with a zero-length buffer returns Ok(None) when the key
        // is absent, and Err (buffer too small) when the key is present.
        let mut buf = [0u8; 0];
        self.nvs.get_blob("peer_payload", &mut buf).is_err()
    }

    fn write_peer_payload(&mut self, payload: &[u8]) -> NodeResult<()> {
        // Cap at the PEER_REQUEST wire limit (202 bytes) so a stored payload
        // always fits in a single ESP-NOW frame.  See ble_pairing::PEER_PAYLOAD_MAX_LEN.
        if payload.len() > crate::ble_pairing::PEER_PAYLOAD_MAX_LEN {
            return Err(NodeError::StorageError(
                "peer_payload too large (max 202 bytes for PEER_REQUEST frame)",
            ));
        }
        self.nvs
            .set_blob("peer_payload", payload)
            .map_err(|_| NodeError::StorageError("peer_payload write failed"))
    }

    fn erase_peer_payload(&mut self) -> NodeResult<()> {
        // Idempotent: treat "key not found" as success.
        match self.nvs.remove("peer_payload") {
            Ok(_) => Ok(()),
            Err(_) if !self.has_peer_payload() => Ok(()), // already absent
            Err(_) => Err(NodeError::StorageError("peer_payload erase failed")),
        }
    }

    fn read_reg_complete(&self) -> bool {
        self.nvs
            .get_u32("reg_complete")
            .ok()
            .flatten()
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    fn write_reg_complete(&mut self, complete: bool) -> NodeResult<()> {
        self.nvs
            .set_u32("reg_complete", if complete { 1 } else { 0 })
            .map_err(|_| NodeError::StorageError("reg_complete write failed"))
    }

    fn read_i2c0_pins(&self) -> (u8, u8) {
        const MAX_GPIO: u8 = 21;
        let sda = self
            .nvs
            .get_u32("i2c0_sda")
            .ok()
            .flatten()
            .and_then(|v| u8::try_from(v).ok())
            .filter(|&v| v <= MAX_GPIO)
            .unwrap_or(0);
        let scl = self
            .nvs
            .get_u32("i2c0_scl")
            .ok()
            .flatten()
            .and_then(|v| u8::try_from(v).ok())
            .filter(|&v| v <= MAX_GPIO)
            .unwrap_or(1);
        // If both decoded to the same pin, fall back to defaults to
        // avoid initializing I2C with SDA==SCL (ND-0608).
        if sda == scl {
            return (0, 1);
        }
        (sda, scl)
    }

    fn write_i2c0_pins(&mut self, sda: u8, scl: u8) -> NodeResult<()> {
        // Validate before persisting — an invalid config survives factory
        // reset (ND-0608 AC#4) and could permanently disable I2C.
        const MAX_GPIO: u8 = 21;
        if sda > MAX_GPIO || scl > MAX_GPIO {
            return Err(NodeError::StorageError("i2c0 pin out of GPIO range"));
        }
        if sda == scl {
            return Err(NodeError::StorageError("i2c0 SDA and SCL must differ"));
        }

        // Best-effort atomicity: if updating SCL fails after SDA was written,
        // restore the previous SDA value to avoid leaving a mismatched pair.
        let (old_sda, _old_scl) = self.read_i2c0_pins();

        self.nvs
            .set_u32("i2c0_sda", sda as u32)
            .map_err(|_| NodeError::StorageError("i2c0_sda write failed"))?;

        if let Err(_e) = self.nvs.set_u32("i2c0_scl", scl as u32) {
            // Attempt to roll back SDA; ignore rollback failure since we
            // can't do better than best-effort here.
            let _ = self.nvs.set_u32("i2c0_sda", old_sda as u32);
            return Err(NodeError::StorageError("i2c0_scl write failed"));
        }

        Ok(())
    }
}
