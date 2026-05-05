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
//! - Legacy board-layout compatibility keys: `"i2c0_sda"` / `"i2c0_scl"` (u32)
//!
//! The early-wake flag is stored in RTC slow SRAM (`.rtc.data` section)
//! rather than NVS, so it survives deep sleep without incurring flash wear.
//! It is reset on power loss or hardware reset, which is acceptable — a
//! missed early wake is harmless. The retained battery value used for the
//! next `WAKE.battery_mv` is also stored in RTC slow SRAM via `LAST_BATTERY_*`.
//! Pre-provisioning test staging and latest-result retention use RTC no-init
//! storage so one software-rebooted test run can hand off state without a
//! flash write.

use core::sync::atomic::{AtomicU32, Ordering};

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsDefault};
use sonde_protocol::{
    decode_board_layout_cbor, encode_board_layout_cbor, BoardLayout, TestResult, MAX_FRAME_SIZE,
};

use crate::error::{NodeError, NodeResult};
use crate::traits::StagedTestCommand;

const NVS_NAMESPACE: &str = "sonde";
const MAGIC_VALUE: u32 = 0xDEAD_BEEF;
const RTC_STAGED_TEST_MAGIC: u32 = 0x5354_434D;
const RTC_TEST_RESULT_MAGIC: u32 = 0x5452_4553;

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

#[repr(C)]
struct RtcStagedTestCommand {
    magic: u32,
    valid: u32,
    test_type: u64,
    rf_channel_present: u32,
    rf_channel: u8,
    payload_len: u16,
    payload: [u8; MAX_FRAME_SIZE],
}

impl RtcStagedTestCommand {
    const fn zero() -> Self {
        Self {
            magic: 0,
            valid: 0,
            test_type: 0,
            rf_channel_present: 0,
            rf_channel: 0,
            payload_len: 0,
            payload: [0u8; MAX_FRAME_SIZE],
        }
    }
}

#[repr(C)]
struct RtcTestResult {
    magic: u32,
    valid: u32,
    status: u8,
    test_type_present: u32,
    test_type: u64,
    reply_frame_len: u16,
    reply_frame: [u8; MAX_FRAME_SIZE],
    reply_rssi_present: u32,
    reply_rssi_dbm: i8,
    attempt_count: u64,
    elapsed_ms: u64,
}

impl RtcTestResult {
    const fn zero() -> Self {
        Self {
            magic: 0,
            valid: 0,
            status: 0,
            test_type_present: 0,
            test_type: 0,
            reply_frame_len: 0,
            reply_frame: [0u8; MAX_FRAME_SIZE],
            reply_rssi_present: 0,
            reply_rssi_dbm: 0,
            attempt_count: 0,
            elapsed_ms: 0,
        }
    }
}

#[link_section = ".rtc_noinit"]
static mut STAGED_TEST_COMMAND: RtcStagedTestCommand = RtcStagedTestCommand::zero();

#[link_section = ".rtc_noinit"]
static mut LATEST_TEST_RESULT: RtcTestResult = RtcTestResult::zero();

fn staged_test_command_is_valid(command: &RtcStagedTestCommand) -> bool {
    command.magic == RTC_STAGED_TEST_MAGIC && command.valid == 1
}

fn retained_test_result_is_valid(result: &RtcTestResult) -> bool {
    result.magic == RTC_TEST_RESULT_MAGIC && result.valid == 1
}

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

    fn read_staged_test_command(&self) -> Option<StagedTestCommand> {
        let command = unsafe { &STAGED_TEST_COMMAND };
        if !staged_test_command_is_valid(command) {
            return None;
        }
        let payload_len = usize::from(command.payload_len);
        if payload_len == 0 || payload_len > MAX_FRAME_SIZE {
            return None;
        }
        Some(StagedTestCommand {
            test_type: command.test_type,
            rf_channel: (command.rf_channel_present != 0).then_some(command.rf_channel),
            payload: command.payload[..payload_len].to_vec(),
        })
    }

    fn write_staged_test_command(&mut self, command: &StagedTestCommand) -> NodeResult<()> {
        if command.payload.is_empty() || command.payload.len() > MAX_FRAME_SIZE {
            return Err(NodeError::StorageError(
                "staged test payload must be 1..=250 bytes",
            ));
        }
        let payload_len = u16::try_from(command.payload.len())
            .map_err(|_| NodeError::StorageError("staged test payload too large"))?;
        let rtc = unsafe { &mut STAGED_TEST_COMMAND };
        rtc.magic = 0;
        rtc.valid = 0;
        rtc.test_type = command.test_type;
        rtc.rf_channel_present = u32::from(command.rf_channel.is_some());
        rtc.rf_channel = command.rf_channel.unwrap_or(0);
        rtc.payload_len = payload_len;
        rtc.payload[..command.payload.len()].copy_from_slice(&command.payload);
        rtc.payload[command.payload.len()..].fill(0);
        rtc.magic = RTC_STAGED_TEST_MAGIC;
        rtc.valid = 1;
        Ok(())
    }

    fn clear_staged_test_command(&mut self) -> NodeResult<()> {
        let rtc = unsafe { &mut STAGED_TEST_COMMAND };
        rtc.magic = 0;
        rtc.valid = 0;
        rtc.test_type = 0;
        rtc.rf_channel_present = 0;
        rtc.rf_channel = 0;
        rtc.payload_len = 0;
        rtc.payload.fill(0);
        Ok(())
    }

    fn read_test_result(&self) -> Option<TestResult> {
        let result = unsafe { &LATEST_TEST_RESULT };
        if !retained_test_result_is_valid(result) {
            return None;
        }
        let reply_frame = if result.reply_frame_len == 0 {
            None
        } else {
            let len = usize::from(result.reply_frame_len);
            if len > MAX_FRAME_SIZE {
                return None;
            }
            Some(result.reply_frame[..len].to_vec())
        };
        let decoded = TestResult {
            status: result.status,
            test_type: (result.test_type_present != 0).then_some(result.test_type),
            reply_frame,
            reply_rssi_dbm: (result.reply_rssi_present != 0).then_some(result.reply_rssi_dbm),
            attempt_count: result.attempt_count,
            elapsed_ms: result.elapsed_ms,
        };
        sonde_protocol::validate_test_result(&decoded).ok()?;
        Some(decoded)
    }

    fn write_test_result(&mut self, result: &TestResult) -> NodeResult<()> {
        sonde_protocol::validate_test_result(result)
            .map_err(|_| NodeError::StorageError("invalid retained test result"))?;
        let reply_len = result
            .reply_frame
            .as_ref()
            .map(|frame| frame.len())
            .unwrap_or(0);
        if reply_len > MAX_FRAME_SIZE {
            return Err(NodeError::StorageError(
                "retained test reply frame too large",
            ));
        }
        let reply_len = u16::try_from(reply_len)
            .map_err(|_| NodeError::StorageError("retained test reply frame too large"))?;

        let rtc = unsafe { &mut LATEST_TEST_RESULT };
        rtc.magic = 0;
        rtc.valid = 0;
        rtc.status = result.status;
        rtc.test_type_present = u32::from(result.test_type.is_some());
        rtc.test_type = result.test_type.unwrap_or(0);
        rtc.reply_frame_len = reply_len;
        rtc.reply_rssi_present = u32::from(result.reply_rssi_dbm.is_some());
        rtc.reply_rssi_dbm = result.reply_rssi_dbm.unwrap_or(0);
        rtc.attempt_count = result.attempt_count;
        rtc.elapsed_ms = result.elapsed_ms;
        if let Some(frame) = &result.reply_frame {
            rtc.reply_frame[..frame.len()].copy_from_slice(frame);
            rtc.reply_frame[frame.len()..].fill(0);
        } else {
            rtc.reply_frame.fill(0);
        }
        rtc.magic = RTC_TEST_RESULT_MAGIC;
        rtc.valid = 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        retained_test_result_is_valid, staged_test_command_is_valid, RtcStagedTestCommand,
        RtcTestResult, RTC_STAGED_TEST_MAGIC, RTC_TEST_RESULT_MAGIC,
    };

    #[test]
    fn staged_test_command_requires_magic_and_valid_flag() {
        let mut command = RtcStagedTestCommand::zero();
        assert!(!staged_test_command_is_valid(&command));

        command.magic = RTC_STAGED_TEST_MAGIC;
        assert!(!staged_test_command_is_valid(&command));

        command.valid = 1;
        assert!(staged_test_command_is_valid(&command));
    }

    #[test]
    fn retained_test_result_requires_magic_and_valid_flag() {
        let mut result = RtcTestResult::zero();
        assert!(!retained_test_result_is_valid(&result));

        result.magic = RTC_TEST_RESULT_MAGIC;
        assert!(!retained_test_result_is_valid(&result));

        result.valid = 1;
        assert!(retained_test_result_is_valid(&result));
    }
}
