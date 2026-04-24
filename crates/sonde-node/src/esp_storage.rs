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
//! - Board layout (ND-0608): `"board_layout"` (blob, deterministic CBOR)
//!
//! The early-wake flag is stored in RTC slow SRAM (`.rtc.data` section)
//! rather than NVS, so it survives deep sleep without incurring flash wear.
//! It is reset on power loss or hardware reset, which is acceptable — a
//! missed early wake is harmless.

use core::sync::atomic::{AtomicU32, Ordering};

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsDefault};
use sonde_protocol::{decode_board_layout_cbor, encode_board_layout_cbor, BoardLayout};

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

#[link_section = ".rtc.data"]
static LAST_BATTERY_MV: AtomicU32 = AtomicU32::new(0);

#[link_section = ".rtc.data"]
static LAST_BATTERY_VALID: AtomicU32 = AtomicU32::new(0);

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

    fn legacy_i2c0_pins(&self) -> (u8, u8) {
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
        if sda == scl {
            return (0, 1);
        }
        (sda, scl)
    }

    fn has_legacy_i2c0_pins(&self) -> bool {
        self.nvs.get_u32("i2c0_sda").ok().flatten().is_some()
            || self.nvs.get_u32("i2c0_scl").ok().flatten().is_some()
    }

    fn legacy_i2c0_pin_state(&self) -> (Option<u8>, Option<u8>) {
        const MAX_GPIO: u8 = 21;
        let read_pin = |key: &str| {
            self.nvs
                .get_u32(key)
                .ok()
                .flatten()
                .and_then(|v| u8::try_from(v).ok())
                .filter(|&v| v <= MAX_GPIO)
        };
        (read_pin("i2c0_sda"), read_pin("i2c0_scl"))
    }

    fn restore_legacy_i2c0_pins(
        &mut self,
        i2c0_sda: Option<u8>,
        i2c0_scl: Option<u8>,
    ) -> NodeResult<()> {
        match i2c0_sda {
            Some(pin) => self
                .nvs
                .set_u32("i2c0_sda", pin as u32)
                .map_err(|_| NodeError::StorageError("legacy i2c0_sda write failed"))?,
            None => {
                self.nvs
                    .remove("i2c0_sda")
                    .map_err(|_| NodeError::StorageError("legacy i2c0_sda erase failed"))?;
            }
        }
        match i2c0_scl {
            Some(pin) => self
                .nvs
                .set_u32("i2c0_scl", pin as u32)
                .map_err(|_| NodeError::StorageError("legacy i2c0_scl write failed"))?,
            None => {
                self.nvs
                    .remove("i2c0_scl")
                    .map_err(|_| NodeError::StorageError("legacy i2c0_scl erase failed"))?;
            }
        }
        Ok(())
    }

    fn read_blob_exact(&self, key: &str) -> NodeResult<Option<Vec<u8>>> {
        let Some(len) = self
            .nvs
            .blob_len(key)
            .map_err(|_| NodeError::StorageError("blob length read failed"))?
        else {
            return Ok(None);
        };

        let mut buf = vec![0u8; len];
        let slice_len = self
            .nvs
            .get_blob(key, &mut buf)
            .map_err(|_| NodeError::StorageError("blob read failed"))?
            .ok_or(NodeError::StorageError("blob disappeared during read"))?
            .len();
        buf.truncate(slice_len);
        Ok(Some(buf))
    }

    fn restore_board_layout_blob(&mut self, blob: Option<&[u8]>) -> NodeResult<()> {
        match blob {
            Some(blob) => self
                .nvs
                .set_blob("board_layout", blob)
                .map_err(|_| NodeError::StorageError("board_layout rollback failed"))?,
            None => {
                self.nvs
                    .remove("board_layout")
                    .map_err(|_| NodeError::StorageError("board_layout erase failed"))?;
            }
        }
        Ok(())
    }

    fn rollback_board_layout_update(
        &mut self,
        board_layout_blob: Option<&[u8]>,
        legacy_i2c0_sda: Option<u8>,
        legacy_i2c0_scl: Option<u8>,
    ) -> NodeResult<()> {
        self.restore_board_layout_blob(board_layout_blob)?;
        self.restore_legacy_i2c0_pins(legacy_i2c0_sda, legacy_i2c0_scl)
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

    fn read_board_layout(&self) -> Option<BoardLayout> {
        match self.read_blob_exact("board_layout") {
            Ok(Some(blob)) => match decode_board_layout_cbor(&blob) {
                Ok(layout) => return Some(layout),
                Err(err) => {
                    log::warn!("failed to decode stored board_layout: {}", err);
                }
            },
            Ok(None) => {}
            Err(err) => {
                log::warn!("failed to read stored board_layout: {}", err);
            }
        }

        if self.has_legacy_i2c0_pins() {
            let (i2c0_sda, i2c0_scl) = self.legacy_i2c0_pins();
            return Some(BoardLayout {
                i2c0_sda: Some(i2c0_sda),
                i2c0_scl: Some(i2c0_scl),
                one_wire_data: None,
                battery_adc: None,
                sensor_enable: None,
            });
        }

        None
    }

    fn write_board_layout(&mut self, layout: &BoardLayout) -> NodeResult<()> {
        let encoded = encode_board_layout_cbor(layout)
            .map_err(|_| NodeError::StorageError("board_layout encode failed"))?;
        let previous_board_layout = self.read_blob_exact("board_layout")?;
        let (previous_i2c0_sda, previous_i2c0_scl) = self.legacy_i2c0_pin_state();

        if let Err(err) = self
            .nvs
            .set_blob("board_layout", &encoded)
            .map_err(|_| NodeError::StorageError("board_layout write failed"))
        {
            let _ = self.rollback_board_layout_update(
                previous_board_layout.as_deref(),
                previous_i2c0_sda,
                previous_i2c0_scl,
            );
            return Err(err);
        }

        if let Err(err) = self.restore_legacy_i2c0_pins(layout.i2c0_sda, layout.i2c0_scl) {
            self.rollback_board_layout_update(
                previous_board_layout.as_deref(),
                previous_i2c0_sda,
                previous_i2c0_scl,
            )
            .map_err(|_| NodeError::StorageError("board_layout rollback failed"))?;
            return Err(err);
        }
        Ok(())
    }

    fn read_last_battery_mv(&self) -> Option<u32> {
        if LAST_BATTERY_VALID.load(Ordering::Relaxed) == 0 {
            None
        } else {
            Some(LAST_BATTERY_MV.load(Ordering::Relaxed))
        }
    }

    fn write_last_battery_mv(&mut self, battery_mv: u32) -> NodeResult<()> {
        LAST_BATTERY_MV.store(battery_mv, Ordering::Relaxed);
        LAST_BATTERY_VALID.store(1, Ordering::Relaxed);
        Ok(())
    }
}
