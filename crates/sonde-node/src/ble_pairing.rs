// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE pairing handler for the node firmware.
//!
//! Implements the platform-independent portion of BLE pairing mode:
//! - NODE_PROVISION message parsing (ble-pairing-protocol.md §6.6)
//! - `RUN_TEST_COMMAND` staging and `READ_TEST_RESULT` readback (§6a)
//! - NVS persistence of PSK, key_hint, channel, peer_payload, reg_complete
//! - NODE_ACK response encoding (ble-pairing-protocol.md §6.7)
//! - Factory-reset-before-provision when the pairing button was held at boot
//!
//! The BLE transport layer (GATT server, advertising, MTU negotiation, LESC
//! pairing) is in `esp_ble_pairing.rs` and is only compiled with the `esp`
//! feature.

use crate::error::NodeResult;
use crate::key_store::KeyStore;
use crate::map_storage::MapStorage;
use crate::traits::{PlatformStorage, StagedTestCommand, Transport};
use sonde_protocol::{decode_board_layout_cbor, BoardLayout};

// ---------------------------------------------------------------------------
// BLE message envelope constants (ble-pairing-protocol.md §4)
// ---------------------------------------------------------------------------

/// BLE envelope TYPE byte for NODE_PROVISION (Phone → Node).
pub const BLE_MSG_NODE_PROVISION: u8 = 0x01;

/// BLE envelope TYPE byte for NODE_ACK (Node → Phone).
pub const BLE_MSG_NODE_ACK: u8 = 0x81;

// ---------------------------------------------------------------------------
// NODE_ACK status codes (ble-pairing-protocol.md §6.7)
// ---------------------------------------------------------------------------

/// Credentials stored successfully.
pub const NODE_ACK_SUCCESS: u8 = 0x00;

/// Already paired and pairing button was not held (defense-in-depth).
/// Not reachable via the current boot path (ND-0905 note).
pub const NODE_ACK_ALREADY_PAIRED: u8 = 0x01;

/// NVS write failure.
pub const NODE_ACK_STORAGE_ERROR: u8 = 0x02;

// ---------------------------------------------------------------------------
// NODE_PROVISION body layout (ble-pairing-protocol.md §6.6)
//   Offset  Size         Field
//   0       2            node_key_hint  BE u16
//   2       32           node_psk       256-bit PSK
//   34      1            rf_channel     WiFi channel (1–13)
//   35      2            payload_len    BE u16
//   37      payload_len  encrypted_payload
// ---------------------------------------------------------------------------

/// Maximum encrypted_payload size accepted by `parse_node_provision`.
///
/// This must fit in a single PEER_REQUEST ESP-NOW frame (250 bytes total).
/// After the 11-byte header, 32-byte HMAC, and ~5 bytes of CBOR framing
/// for `{ 1: bstr(N) }`, at most 202 bytes remain for the payload.
/// See ble-pairing-protocol.md §11.1.
///
/// The NVS read buffer in `esp_storage` (512 bytes) is larger than this
/// limit, so NVS is never the bottleneck.
pub const PEER_PAYLOAD_MAX_LEN: usize = 202;

/// Minimum negotiated ATT MTU accepted for BLE pairing (ND-0904).
///
/// The BLE transport layer must negotiate at least this MTU. Connections
/// with a lower MTU must be disconnected. This constant is shared between
/// the platform-independent validation logic and the ESP-specific BLE
/// transport in `esp_ble_pairing.rs`.
pub const BLE_MIN_ATT_MTU: u16 = 247;

/// Check whether the negotiated ATT MTU meets the minimum requirement.
///
/// Returns `true` if `mtu >= BLE_MIN_ATT_MTU` (247). The caller should
/// disconnect the BLE peer if this returns `false` (ND-0904).
pub fn is_mtu_acceptable(mtu: u16) -> bool {
    mtu >= BLE_MIN_ATT_MTU
}

/// Minimum body length for a NODE_PROVISION with an empty encrypted_payload.
const NODE_PROVISION_MIN_LEN: usize = 37;

/// Parsed NODE_PROVISION body.
#[derive(Debug)]
pub struct NodeProvision {
    /// Key hint derived from the node PSK (SHA256(psk)[30..32], BE u16).
    pub key_hint: u16,
    /// Node pre-shared key (256 bits).
    pub psk: [u8; 32],
    /// WiFi / ESP-NOW RF channel (1–13).
    pub rf_channel: u8,
    /// Opaque encrypted payload for the gateway (ble-pairing-protocol.md §6.4).
    pub encrypted_payload: Vec<u8>,
    /// Provisioned board layout, when the pairing tool included one.
    pub board_layout: ProvisionedBoardLayout,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProvisionedBoardLayout {
    Absent,
    Provided(BoardLayout),
}

// Re-export BLE envelope codec from sonde-protocol (shared with gateway).
pub use sonde_protocol::{encode_ble_envelope, parse_ble_envelope};

/// Parse a NODE_PROVISION body (already unwrapped from the BLE envelope).
///
/// Returns `Err(&'static str)` if the body is malformed, truncated, or
/// contains an out-of-range channel value.
pub fn parse_node_provision(body: &[u8]) -> Result<NodeProvision, &'static str> {
    if body.len() < NODE_PROVISION_MIN_LEN {
        return Err("body too short");
    }
    let key_hint = u16::from_be_bytes([body[0], body[1]]);
    let mut psk = [0u8; 32];
    psk.copy_from_slice(&body[2..34]);
    let rf_channel = body[34];
    if !(1..=13).contains(&rf_channel) {
        return Err("rf_channel out of range (must be 1–13)");
    }
    let payload_len = u16::from_be_bytes([body[35], body[36]]) as usize;
    if payload_len > PEER_PAYLOAD_MAX_LEN {
        return Err("encrypted_payload too large");
    }
    let expected_len = NODE_PROVISION_MIN_LEN + payload_len;
    if body.len() < expected_len {
        return Err("encrypted_payload truncated");
    }
    let encrypted_payload = body[37..37 + payload_len].to_vec();

    let board_layout = if body.len() > expected_len {
        ProvisionedBoardLayout::Provided(
            decode_board_layout_cbor(&body[expected_len..])
                .map_err(|_| "board_layout CBOR decode failed")?,
        )
    } else {
        ProvisionedBoardLayout::Absent
    };

    Ok(NodeProvision {
        key_hint,
        psk,
        rf_channel,
        encrypted_payload,
        board_layout,
    })
}

/// Encode a NODE_ACK BLE envelope for the given status byte.
///
/// A 1-byte body always fits in `u16`, so this never returns `None`.
pub fn encode_node_ack(status: u8) -> Vec<u8> {
    encode_ble_envelope(BLE_MSG_NODE_ACK, &[status])
        .expect("NODE_ACK body (1 byte) always fits in u16 LEN")
}

/// Handle a parsed NODE_PROVISION:
///
/// 1. If `paired_on_entry` is true and `button_held` is false, return
///    `NODE_ACK_ALREADY_PAIRED` (defense-in-depth — ND-0905 note).
///    `paired_on_entry` indicates the node was already paired when it
///    entered BLE mode; it does NOT block same-session re-provision
///    (ND-0907) after a successful first provision in this BLE session.
/// 2. If `button_held` is true, perform a factory reset before writing new
///    credentials (ND-0917).
/// 3. Erase any pre-existing PSK to allow same-session re-provision (ND-0907).
/// 4. Write PSK, key_hint, RF channel, and `encrypted_payload` to storage.
/// 5. Clear the `reg_complete` flag (ND-0906).
///
/// Returns a `NODE_ACK` status byte:
/// - `NODE_ACK_SUCCESS` (0x00) on success.
/// - `NODE_ACK_ALREADY_PAIRED` (0x01) if paired on entry without button override.
/// - `NODE_ACK_STORAGE_ERROR` (0x02) on any NVS write failure.
pub fn handle_node_provision<S: PlatformStorage>(
    provision: &NodeProvision,
    storage: &mut S,
    map_storage: &mut MapStorage,
    button_held: bool,
    paired_on_entry: bool,
) -> u8 {
    // Defense-in-depth: reject provisioning if the node was already paired
    // when it entered BLE mode and the pairing button was not held.
    // This does NOT block same-session re-provision (ND-0907) — the caller
    // passes `paired_on_entry = false` when the node entered BLE mode
    // unpaired and was provisioned during this session.
    if !button_held && paired_on_entry {
        return NODE_ACK_ALREADY_PAIRED;
    }

    // If the pairing button was held at boot, factory-reset all persistent
    // state before accepting new credentials (ND-0917).
    if button_held {
        let mut ks = KeyStore::new(storage);
        if ks.factory_reset(map_storage).is_err() {
            return NODE_ACK_STORAGE_ERROR;
        }
    }

    // Erase any pre-existing PSK to allow same-session re-provision (ND-0907).
    // Ignore errors: on a fresh unpaired node the key may not exist in NVS
    // ("not found" is expected), and after a factory reset above it is already
    // gone.  If the erase genuinely fails for another reason, the subsequent
    // write_key() will return an error and we propagate NODE_ACK_STORAGE_ERROR.
    let _ = storage.erase_key();

    // Write PSK + key_hint (includes magic sentinel).
    if storage
        .write_key(provision.key_hint, &provision.psk)
        .is_err()
    {
        return NODE_ACK_STORAGE_ERROR;
    }

    // Persist the opaque encrypted payload for PEER_REQUEST (ND-0916).
    if storage
        .write_peer_payload(&provision.encrypted_payload)
        .is_err()
    {
        let _ = storage.erase_key();
        return NODE_ACK_STORAGE_ERROR;
    }

    // Clear the registration-complete flag so the next boot enters the
    // PEER_REQUEST path instead of the normal WAKE cycle (ND-0906).
    if storage.write_reg_complete(false).is_err() {
        let _ = storage.erase_key();
        let _ = storage.erase_peer_payload();
        return NODE_ACK_STORAGE_ERROR;
    }

    // Persist the RF channel last among the critical fields so a failure
    // in any earlier write does not leave a stale channel value that could
    // leak across pairing attempts (ND-0908). Pin config (below) is
    // best-effort and non-fatal, so it is written after the channel.
    if storage.write_channel(provision.rf_channel).is_err() {
        let _ = storage.erase_key();
        let _ = storage.erase_peer_payload();
        return NODE_ACK_STORAGE_ERROR;
    }

    match &provision.board_layout {
        ProvisionedBoardLayout::Provided(layout) => {
            let previous_layout = storage.read_board_layout();
            if storage.write_board_layout(layout).is_err() {
                if let Some(previous_layout) = previous_layout {
                    let _ = storage.write_board_layout(&previous_layout);
                }
                let _ = storage.erase_key();
                let _ = storage.erase_peer_payload();
                return NODE_ACK_STORAGE_ERROR;
            }
        }
        ProvisionedBoardLayout::Absent => {
            if storage.read_board_layout().is_none()
                && storage
                    .write_board_layout(&BoardLayout::LEGACY_COMPAT)
                    .is_err()
            {
                let _ = storage.erase_key();
                let _ = storage.erase_peer_payload();
                return NODE_ACK_STORAGE_ERROR;
            }
        }
    }

    NODE_ACK_SUCCESS
}

// ---------------------------------------------------------------------------
// Pre-provisioning test mode (ble-pairing-protocol.md §6a, ND-1100..ND-1107)
// ---------------------------------------------------------------------------

const TEST_MAX_RETRIES: u64 = 3;
const TEST_RETRY_DELAY_MS: u32 = 200;
const TEST_LISTEN_TIMEOUT_MS: u32 = 2_000;

/// Encode a `RUN_TEST_ACK` BLE envelope.
pub fn encode_run_test_ack(status: u8) -> Vec<u8> {
    let body = sonde_protocol::encode_run_test_ack(status).unwrap_or_default();
    encode_ble_envelope(sonde_protocol::BLE_RUN_TEST_ACK, &body).unwrap_or_default()
}

/// Encode a `TEST_RESULT` BLE envelope.
pub fn encode_test_result_response(result: &sonde_protocol::TestResult) -> Vec<u8> {
    let sanitized = sanitize_test_result(result);
    let body = sonde_protocol::encode_test_result(&sanitized)
        .expect("sanitize_test_result must produce an encodable TEST_RESULT");
    encode_ble_envelope(sonde_protocol::BLE_TEST_RESULT, &body)
        .expect("TEST_RESULT body always fits in BLE envelope")
}

/// Parse, validate, and stage a `RUN_TEST_COMMAND`.
pub fn handle_run_test_command<S: PlatformStorage>(
    body: &[u8],
    storage: &mut S,
) -> (u8, Option<StagedTestCommand>) {
    let Ok(command) = sonde_protocol::decode_run_test_command(body) else {
        return (sonde_protocol::RUN_TEST_ACK_INVALID, None);
    };

    if command.test_type != sonde_protocol::TEST_TYPE_DIAG_FRAME {
        return (sonde_protocol::RUN_TEST_ACK_UNSUPPORTED, None);
    }

    let staged = StagedTestCommand {
        test_type: command.test_type,
        rf_channel: command.rf_channel,
        payload: command.payload,
    };

    if storage.write_staged_test_command(&staged).is_err() {
        return (sonde_protocol::RUN_TEST_ACK_INVALID, None);
    }

    (sonde_protocol::RUN_TEST_ACK_OK, Some(staged))
}

/// Read the retained latest `TEST_RESULT`.
pub fn handle_read_test_result<S: PlatformStorage>(
    body: &[u8],
    storage: &S,
) -> Result<sonde_protocol::TestResult, &'static str> {
    sonde_protocol::decode_read_test_result(body)
        .map_err(|_| "READ_TEST_RESULT body must be empty")?;
    Ok(storage
        .read_test_result()
        .map(|result| sanitize_test_result(&result))
        .unwrap_or_else(no_result_result))
}

/// Execute the staged pre-provisioning test command, store the latest result,
/// and clear the staged command.
pub fn execute_staged_test_command<S: PlatformStorage, T: Transport, C: crate::traits::Clock>(
    storage: &mut S,
    transport: &mut T,
    clock: &C,
) -> NodeResult<Option<sonde_protocol::TestResult>> {
    let Some(command) = storage.read_staged_test_command() else {
        return Ok(None);
    };

    let result = match command.test_type {
        sonde_protocol::TEST_TYPE_DIAG_FRAME => execute_diag_frame_test(transport, clock, &command),
        _ => sonde_protocol::TestResult {
            status: sonde_protocol::TEST_RESULT_EXECUTION_ERROR,
            test_type: Some(command.test_type),
            reply_frame: None,
            reply_rssi_dbm: None,
            attempt_count: 0,
            elapsed_ms: 0,
        },
    };

    if let Err(err) = storage.write_test_result(&result) {
        let _ = storage.clear_staged_test_command();
        return Err(err);
    }
    storage.clear_staged_test_command()?;
    Ok(Some(result))
}

fn execute_diag_frame_test<T: Transport, C: crate::traits::Clock>(
    transport: &mut T,
    clock: &C,
    command: &StagedTestCommand,
) -> sonde_protocol::TestResult {
    let start_ms = clock.elapsed_ms();

    let Some(rf_channel) = command.rf_channel else {
        return execution_error_result(command.test_type, 0, 0);
    };
    if !(1..=13).contains(&rf_channel)
        || command.payload.is_empty()
        || command.payload.len() > sonde_protocol::MAX_FRAME_SIZE
    {
        return execution_error_result(command.test_type, 0, 0);
    }

    let mut attempt_count = 0u64;
    let mut send_succeeded = false;
    for attempt in 0..=TEST_MAX_RETRIES {
        if attempt > 0 {
            clock.delay_ms(TEST_RETRY_DELAY_MS);
        }
        attempt_count = attempt + 1;
        log::info!(
            "diag test attempt {} sending {} bytes on channel {}",
            attempt_count,
            command.payload.len(),
            rf_channel
        );
        if let Err(err) = transport.send(&command.payload) {
            log::warn!("diag test attempt {} send failed: {}", attempt_count, err);
            continue;
        }
        send_succeeded = true;

        #[cfg(feature = "esp")]
        {
            let mut remaining_ms = TEST_LISTEN_TIMEOUT_MS;
            loop {
                if remaining_ms == 0 {
                    break;
                }
                let before = std::time::Instant::now();
                match transport.recv_with_metadata(remaining_ms) {
                    Ok(Some(frame))
                        if frame.data.len() >= sonde_protocol::MIN_FRAME_SIZE
                            && frame.data[sonde_protocol::OFFSET_MSG_TYPE]
                                == sonde_protocol::MSG_DIAG_REPLY =>
                    {
                        log::info!(
                            "diag test attempt {} received DIAG_REPLY len={} rssi={:?}",
                            attempt_count,
                            frame.data.len(),
                            frame.rssi_dbm
                        );
                        return success_result(
                            command.test_type,
                            frame.data,
                            frame.rssi_dbm,
                            attempt_count,
                            clock.elapsed_ms().saturating_sub(start_ms),
                        );
                    }
                    Ok(Some(frame)) => {
                        let msg_type = if frame.data.len() > sonde_protocol::OFFSET_MSG_TYPE {
                            frame.data[sonde_protocol::OFFSET_MSG_TYPE]
                        } else {
                            0xFF
                        };
                        log::debug!(
                            "diag test attempt {} ignored frame msg_type=0x{:02x} len={} rssi={:?}",
                            attempt_count,
                            msg_type,
                            frame.data.len(),
                            frame.rssi_dbm
                        );
                        let elapsed = before.elapsed().as_millis() as u32;
                        remaining_ms = remaining_ms.saturating_sub(elapsed.max(1));
                    }
                    Ok(None) => {
                        log::info!(
                            "diag test attempt {} receive wait expired after {} ms",
                            attempt_count,
                            before.elapsed().as_millis()
                        );
                        break;
                    }
                    Err(err) => {
                        log::warn!(
                            "diag test attempt {} receive failed: {}",
                            attempt_count,
                            err
                        );
                        break;
                    }
                }
            }
        }

        #[cfg(not(feature = "esp"))]
        {
            if let Ok(Some(frame)) = transport.recv_with_metadata(TEST_LISTEN_TIMEOUT_MS) {
                if frame.data.len() >= sonde_protocol::MIN_FRAME_SIZE
                    && frame.data[sonde_protocol::OFFSET_MSG_TYPE] == sonde_protocol::MSG_DIAG_REPLY
                {
                    return success_result(
                        command.test_type,
                        frame.data,
                        frame.rssi_dbm,
                        attempt_count,
                        clock.elapsed_ms().saturating_sub(start_ms),
                    );
                }
            }
        }
    }

    if !send_succeeded {
        return execution_error_result(
            command.test_type,
            attempt_count,
            clock.elapsed_ms().saturating_sub(start_ms),
        );
    }

    sonde_protocol::TestResult {
        status: sonde_protocol::TEST_RESULT_TIMEOUT,
        test_type: Some(command.test_type),
        reply_frame: None,
        reply_rssi_dbm: None,
        attempt_count,
        elapsed_ms: clock.elapsed_ms().saturating_sub(start_ms),
    }
}

fn success_result(
    test_type: u64,
    reply_frame: Vec<u8>,
    reply_rssi_dbm: Option<i8>,
    attempt_count: u64,
    elapsed_ms: u64,
) -> sonde_protocol::TestResult {
    sonde_protocol::TestResult {
        status: sonde_protocol::TEST_RESULT_OK,
        test_type: Some(test_type),
        reply_frame: Some(reply_frame),
        reply_rssi_dbm,
        attempt_count,
        elapsed_ms,
    }
}

fn no_result_result() -> sonde_protocol::TestResult {
    sonde_protocol::TestResult {
        status: sonde_protocol::TEST_RESULT_NO_RESULT,
        test_type: None,
        reply_frame: None,
        reply_rssi_dbm: None,
        attempt_count: 0,
        elapsed_ms: 0,
    }
}

fn execution_error_result(
    test_type: u64,
    attempt_count: u64,
    elapsed_ms: u64,
) -> sonde_protocol::TestResult {
    sonde_protocol::TestResult {
        status: sonde_protocol::TEST_RESULT_EXECUTION_ERROR,
        test_type: Some(test_type),
        reply_frame: None,
        reply_rssi_dbm: None,
        attempt_count,
        elapsed_ms,
    }
}

fn sanitize_test_result(result: &sonde_protocol::TestResult) -> sonde_protocol::TestResult {
    if sonde_protocol::validate_test_result(result).is_ok() {
        return result.clone();
    }

    match result.test_type {
        Some(test_type) => {
            execution_error_result(test_type, result.attempt_count, result.elapsed_ms)
        }
        None => no_result_result(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{NodeError, NodeResult};
    use crate::traits::PlatformStorage;

    // --- Minimal mock storage for BLE pairing tests ---

    struct MockStorage {
        key: Option<(u16, [u8; 32])>,
        channel: Option<u8>,
        peer_payload: Option<Vec<u8>>,
        reg_complete: bool,
        board_layout: Option<BoardLayout>,
        staged_test_command: Option<StagedTestCommand>,
        test_result: Option<sonde_protocol::TestResult>,
        fail_write_key: bool,
        fail_write_channel: bool,
        fail_write_peer_payload: bool,
        fail_write_reg_complete: bool,
        fail_write_board_layout: bool,
        fail_write_staged_test_command: bool,
        fail_write_test_result: bool,
        fail_clear_staged_test_command: bool,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                key: None,
                channel: None,
                peer_payload: None,
                reg_complete: false,
                board_layout: None,
                staged_test_command: None,
                test_result: None,
                fail_write_key: false,
                fail_write_channel: false,
                fail_write_peer_payload: false,
                fail_write_reg_complete: false,
                fail_write_board_layout: false,
                fail_write_staged_test_command: false,
                fail_write_test_result: false,
                fail_clear_staged_test_command: false,
            }
        }

        fn with_key(key_hint: u16, psk: [u8; 32]) -> Self {
            let mut s = Self::new();
            s.key = Some((key_hint, psk));
            s
        }
    }

    impl PlatformStorage for MockStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            self.key
        }
        fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
            if self.fail_write_key {
                return Err(NodeError::StorageError("injected write_key failure"));
            }
            if self.key.is_some() {
                return Err(NodeError::StorageError("already paired"));
            }
            self.key = Some((key_hint, *psk));
            Ok(())
        }
        fn erase_key(&mut self) -> NodeResult<()> {
            self.key = None;
            Ok(())
        }
        fn read_schedule(&self) -> (u32, u8) {
            (60, 0)
        }
        fn write_schedule_interval(&mut self, _: u32) -> NodeResult<()> {
            Ok(())
        }
        fn write_active_partition(&mut self, _: u8) -> NodeResult<()> {
            Ok(())
        }
        fn reset_schedule(&mut self) -> NodeResult<()> {
            Ok(())
        }
        fn read_program(&self, _: u8) -> Option<Vec<u8>> {
            None
        }
        fn write_program(&mut self, _: u8, _: &[u8]) -> NodeResult<()> {
            Ok(())
        }
        fn erase_program(&mut self, _: u8) -> NodeResult<()> {
            Ok(())
        }
        fn take_early_wake_flag(&mut self) -> bool {
            false
        }
        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            Ok(())
        }
        fn read_channel(&self) -> Option<u8> {
            self.channel
        }
        fn write_channel(&mut self, channel: u8) -> NodeResult<()> {
            if self.fail_write_channel {
                return Err(NodeError::StorageError("injected write_channel failure"));
            }
            self.channel = Some(channel);
            Ok(())
        }
        fn read_peer_payload(&self) -> Option<Vec<u8>> {
            self.peer_payload.clone()
        }
        fn write_peer_payload(&mut self, payload: &[u8]) -> NodeResult<()> {
            if self.fail_write_peer_payload {
                return Err(NodeError::StorageError(
                    "injected write_peer_payload failure",
                ));
            }
            self.peer_payload = Some(payload.to_vec());
            Ok(())
        }
        fn erase_peer_payload(&mut self) -> NodeResult<()> {
            self.peer_payload = None;
            Ok(())
        }
        fn read_reg_complete(&self) -> bool {
            self.reg_complete
        }
        fn write_reg_complete(&mut self, complete: bool) -> NodeResult<()> {
            if self.fail_write_reg_complete {
                return Err(NodeError::StorageError(
                    "injected write_reg_complete failure",
                ));
            }
            self.reg_complete = complete;
            Ok(())
        }
        fn read_board_layout(&self) -> Option<BoardLayout> {
            self.board_layout
        }
        fn write_board_layout(&mut self, layout: &BoardLayout) -> NodeResult<()> {
            if self.fail_write_board_layout {
                return Err(NodeError::StorageError(
                    "injected write_board_layout failure",
                ));
            }
            self.board_layout = Some(*layout);
            Ok(())
        }
        fn read_staged_test_command(&self) -> Option<StagedTestCommand> {
            self.staged_test_command.clone()
        }
        fn write_staged_test_command(&mut self, command: &StagedTestCommand) -> NodeResult<()> {
            if self.fail_write_staged_test_command {
                return Err(NodeError::StorageError(
                    "injected write_staged_test_command failure",
                ));
            }
            self.staged_test_command = Some(command.clone());
            Ok(())
        }
        fn clear_staged_test_command(&mut self) -> NodeResult<()> {
            if self.fail_clear_staged_test_command {
                return Err(NodeError::StorageError(
                    "injected clear_staged_test_command failure",
                ));
            }
            self.staged_test_command = None;
            Ok(())
        }
        fn read_test_result(&self) -> Option<sonde_protocol::TestResult> {
            self.test_result.clone()
        }
        fn write_test_result(&mut self, result: &sonde_protocol::TestResult) -> NodeResult<()> {
            if self.fail_write_test_result {
                return Err(NodeError::StorageError(
                    "injected write_test_result failure",
                ));
            }
            self.test_result = Some(result.clone());
            Ok(())
        }
    }

    // --- Helper ---

    fn make_provision(key_hint: u16, psk: [u8; 32], channel: u8, payload: &[u8]) -> NodeProvision {
        NodeProvision {
            key_hint,
            psk,
            rf_channel: channel,
            encrypted_payload: payload.to_vec(),
            board_layout: ProvisionedBoardLayout::Absent,
        }
    }

    #[derive(Default)]
    struct MockClock {
        elapsed_ms: std::cell::Cell<u64>,
        delays_ms: std::cell::RefCell<Vec<u32>>,
    }

    impl crate::traits::Clock for MockClock {
        fn elapsed_ms(&self) -> u64 {
            self.elapsed_ms.get()
        }

        fn delay_ms(&self, ms: u32) {
            self.delays_ms.borrow_mut().push(ms);
            self.elapsed_ms
                .set(self.elapsed_ms.get().saturating_add(ms as u64));
        }
    }

    struct MockTransport {
        sends: Vec<Vec<u8>>,
        replies: std::collections::VecDeque<Option<crate::traits::ReceivedFrame>>,
    }

    impl MockTransport {
        fn new(replies: Vec<Option<crate::traits::ReceivedFrame>>) -> Self {
            Self {
                sends: Vec::new(),
                replies: replies.into(),
            }
        }
    }

    impl crate::traits::Transport for MockTransport {
        fn send(&mut self, frame: &[u8]) -> NodeResult<()> {
            self.sends.push(frame.to_vec());
            Ok(())
        }

        fn recv(&mut self, timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
            self.recv_with_metadata(timeout_ms)
                .map(|frame| frame.map(|frame| frame.data))
        }

        fn recv_with_metadata(
            &mut self,
            _timeout_ms: u32,
        ) -> NodeResult<Option<crate::traits::ReceivedFrame>> {
            Ok(self.replies.pop_front().flatten())
        }
    }

    // --- BLE envelope parsing ---

    #[test]
    fn parse_ble_envelope_ok() {
        let data = vec![0x01, 0x00, 0x03, 0xAA, 0xBB, 0xCC];
        let (msg_type, body) = parse_ble_envelope(&data).unwrap();
        assert_eq!(msg_type, 0x01);
        assert_eq!(body, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_ble_envelope_empty_body() {
        let data = vec![0x81, 0x00, 0x00];
        let (msg_type, body) = parse_ble_envelope(&data).unwrap();
        assert_eq!(msg_type, 0x81);
        assert!(body.is_empty());
    }

    #[test]
    fn parse_ble_envelope_too_short() {
        assert!(parse_ble_envelope(&[0x01, 0x00]).is_none());
    }

    #[test]
    fn parse_ble_envelope_body_truncated() {
        // LEN=4 but only 2 bytes follow
        let data = vec![0x01, 0x00, 0x04, 0xAA, 0xBB];
        assert!(parse_ble_envelope(&data).is_none());
    }

    #[test]
    fn parse_ble_envelope_trailing_bytes_rejected() {
        // LEN=2, 2 body bytes, plus 1 trailing byte
        let data = vec![0x01, 0x00, 0x02, 0xAA, 0xBB, 0xCC];
        assert!(parse_ble_envelope(&data).is_none());
    }

    #[test]
    fn encode_ble_envelope_round_trip() {
        let body = [0x42u8; 10];
        let encoded = encode_ble_envelope(0x01, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&encoded).unwrap();
        assert_eq!(msg_type, 0x01);
        assert_eq!(decoded_body, &body);
    }

    #[test]
    fn encode_ble_envelope_rejects_oversize_body() {
        // A body larger than u16::MAX must return None.
        let big_body = vec![0xAAu8; u16::MAX as usize + 1];
        assert!(encode_ble_envelope(0x01, &big_body).is_none());
    }

    #[test]
    fn encode_ble_envelope_accepts_max_body() {
        // Exactly u16::MAX bytes must succeed.
        let max_body = vec![0xBBu8; u16::MAX as usize];
        assert!(encode_ble_envelope(0x01, &max_body).is_some());
    }

    // --- NODE_PROVISION parsing ---

    #[test]
    fn parse_node_provision_ok() {
        let mut body = vec![0u8; 37 + 16];
        // key_hint = 0x1234 BE
        body[0] = 0x12;
        body[1] = 0x34;
        // psk: 32 bytes of 0x42
        for b in &mut body[2..34] {
            *b = 0x42;
        }
        // rf_channel = 6
        body[34] = 6;
        // payload_len = 16 BE
        body[35] = 0x00;
        body[36] = 0x10;
        // payload: 16 bytes of 0xAB
        for b in &mut body[37..53] {
            *b = 0xAB;
        }

        let p = parse_node_provision(&body).unwrap();
        assert_eq!(p.key_hint, 0x1234);
        assert_eq!(p.psk, [0x42u8; 32]);
        assert_eq!(p.rf_channel, 6);
        assert_eq!(p.encrypted_payload, vec![0xABu8; 16]);
    }

    #[test]
    fn parse_node_provision_empty_payload() {
        let mut body = vec![0u8; 37];
        body[0] = 0x00;
        body[1] = 0x01; // key_hint = 1
        for b in &mut body[2..34] {
            *b = 0x42;
        }
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0

        let p = parse_node_provision(&body).unwrap();
        assert_eq!(p.key_hint, 1);
        assert!(p.encrypted_payload.is_empty());
    }

    #[test]
    fn parse_node_provision_too_short() {
        assert!(parse_node_provision(&[0u8; 36]).is_err());
    }

    #[test]
    fn parse_node_provision_payload_truncated() {
        let mut body = vec![0u8; 39]; // claims 4-byte payload but only 2 bytes follow
        body[35] = 0x00;
        body[36] = 0x04; // payload_len = 4
                         // Only 2 bytes after offset 37 (body is 39 bytes = 37 + 2)
        assert!(parse_node_provision(&body).is_err());
    }

    #[test]
    fn parse_node_provision_trailing_bytes_decode_board_layout() {
        let board_layout_cbor =
            sonde_protocol::encode_board_layout_cbor(&BoardLayout::SONDE_SENSOR_NODE_REV_A)
                .unwrap();
        let mut body = vec![0u8; 37 + board_layout_cbor.len()];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0
        body[37..].copy_from_slice(&board_layout_cbor);
        let provision = parse_node_provision(&body).unwrap();
        assert_eq!(
            provision.board_layout,
            ProvisionedBoardLayout::Provided(BoardLayout::SONDE_SENSOR_NODE_REV_A)
        );
    }

    #[test]
    fn parse_node_provision_trailing_non_cbor_rejected() {
        let mut body = vec![0u8; 38];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0
        body[37] = 0xFF; // invalid CBOR
        assert!(parse_node_provision(&body).is_err());
    }

    #[test]
    fn parse_node_provision_missing_board_layout_bytes_is_absent() {
        let mut body = vec![0u8; 37];
        body[2..34].fill(0x42);
        body[34] = 1;
        body[35] = 0x00;
        body[36] = 0x00;
        let provision = parse_node_provision(&body).unwrap();
        assert_eq!(provision.board_layout, ProvisionedBoardLayout::Absent);
    }

    #[test]
    fn parse_node_provision_board_layout_with_trailing_junk_rejected() {
        let mut data =
            sonde_protocol::encode_board_layout_cbor(&BoardLayout::SONDE_SENSOR_NODE_REV_A)
                .unwrap();
        data.push(0x00);
        let mut body = vec![0u8; 37 + data.len()];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0
        body[37..].copy_from_slice(&data);
        assert!(parse_node_provision(&body).is_err());
    }

    #[test]
    fn parse_node_provision_oversize_payload_rejected() {
        // payload_len exceeds PEER_PAYLOAD_MAX_LEN — rejected before allocation
        let payload_len = PEER_PAYLOAD_MAX_LEN + 1;
        let mut body = vec![0u8; 37 + payload_len];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = ((payload_len >> 8) & 0xFF) as u8;
        body[36] = (payload_len & 0xFF) as u8;
        let err = parse_node_provision(&body).unwrap_err();
        assert_eq!(err, "encrypted_payload too large");
    }

    #[test]
    fn parse_node_provision_invalid_channel_zero() {
        let mut body = vec![0u8; 37];
        body[34] = 0; // channel 0 — invalid
        assert!(parse_node_provision(&body).is_err());
    }

    #[test]
    fn parse_node_provision_invalid_channel_14() {
        let mut body = vec![0u8; 37];
        body[34] = 14; // channel 14 — out of range
        assert!(parse_node_provision(&body).is_err());
    }

    // --- NODE_ACK encoding ---

    #[test]
    fn encode_node_ack_success() {
        let frame = encode_node_ack(NODE_ACK_SUCCESS);
        let (msg_type, body) = parse_ble_envelope(&frame).unwrap();
        assert_eq!(msg_type, BLE_MSG_NODE_ACK);
        assert_eq!(body, &[NODE_ACK_SUCCESS]);
    }

    #[test]
    fn encode_node_ack_storage_error() {
        let frame = encode_node_ack(NODE_ACK_STORAGE_ERROR);
        let (msg_type, body) = parse_ble_envelope(&frame).unwrap();
        assert_eq!(msg_type, BLE_MSG_NODE_ACK);
        assert_eq!(body, &[NODE_ACK_STORAGE_ERROR]);
    }

    // -----------------------------------------------------------------------
    // T-N940: NODE_PROVISION with invalid payload_len rejected (ND-0905)
    // -----------------------------------------------------------------------

    #[test]
    fn t_n940_payload_len_exceeds_remaining_data() {
        // T-N940: payload_len field exceeds the remaining data in the buffer.
        // The parser must reject the message without reading past the end.
        let claimed_payload: usize = 10; // must be <= PEER_PAYLOAD_MAX_LEN
        assert!(claimed_payload <= PEER_PAYLOAD_MAX_LEN);
        let actual_data_bytes = 4;
        let mut body = vec![0u8; NODE_PROVISION_MIN_LEN + actual_data_bytes];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // valid channel
                      // Claim `claimed_payload` bytes of payload, but only `actual_data_bytes` follow.
        body[35] = (claimed_payload >> 8) as u8;
        body[36] = claimed_payload as u8;
        body[NODE_PROVISION_MIN_LEN..NODE_PROVISION_MIN_LEN + actual_data_bytes].fill(0xAA);

        let err = parse_node_provision(&body).unwrap_err();
        assert_eq!(err, "encrypted_payload truncated");
    }

    #[test]
    fn t_n940_payload_len_max_u16_rejected() {
        // T-N940 boundary: payload_len = 0xFFFF (65535) — far exceeds both
        // the buffer and PEER_PAYLOAD_MAX_LEN.
        let mut body = vec![0u8; NODE_PROVISION_MIN_LEN]; // minimum-length body, no payload data
        body[2..34].fill(0x42); // psk
        body[34] = 1; // valid channel
        body[35] = 0xFF;
        body[36] = 0xFF; // payload_len = 65535

        let err = parse_node_provision(&body).unwrap_err();
        assert_eq!(err, "encrypted_payload too large");
    }

    // --- handle_node_provision: T-N904 happy path ---

    /// T-N904: NODE_PROVISION on unpaired node → NODE_ACK(0x00), all NVS fields written.
    #[test]
    fn t_n904_happy_path() {
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);
        let psk = [0x42u8; 32];
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let provision = make_provision(0xABCD, psk, 6, &payload);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);

        // PSK and key_hint stored
        let key = storage.read_key().expect("key should be stored");
        assert_eq!(key.0, 0xABCD);
        assert_eq!(key.1, psk);

        // Channel stored
        assert_eq!(storage.read_channel(), Some(6));

        // Encrypted payload stored
        assert_eq!(
            storage.read_peer_payload().as_deref(),
            Some(payload.as_slice())
        );

        // reg_complete cleared
        assert!(!storage.read_reg_complete());
    }

    // --- handle_node_provision: T-N905 same-session re-provision ---

    /// T-N905: Second NODE_PROVISION on same BLE connection overwrites credentials
    /// (ND-0907). The caller passes `paired_on_entry = false` because the node
    /// was unpaired when it entered BLE mode.
    #[test]
    fn t_n905_same_session_reprovision() {
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);

        // First provision (unpaired) — succeeds
        let psk_a = [0x11u8; 32];
        let payload_a = vec![0x01, 0x02];
        let provision_a = make_provision(0x0001, psk_a, 3, &payload_a);
        let status_a = handle_node_provision(&provision_a, &mut storage, &mut maps, false, false);
        assert_eq!(status_a, NODE_ACK_SUCCESS);
        assert_eq!(storage.read_key().unwrap().1, psk_a);

        // Second provision on same session — still paired_on_entry=false
        let psk_b = [0x22u8; 32];
        let payload_b = vec![0x03, 0x04, 0x05];
        let provision_b = make_provision(0x0002, psk_b, 11, &payload_b);
        let status_b = handle_node_provision(&provision_b, &mut storage, &mut maps, false, false);
        assert_eq!(
            status_b, NODE_ACK_SUCCESS,
            "same-session re-provision must succeed"
        );

        // NVS now contains credentials B
        let key = storage
            .read_key()
            .expect("key should be stored after re-provision");
        assert_eq!(key.0, 0x0002);
        assert_eq!(key.1, psk_b);
        assert_eq!(storage.read_channel(), Some(11));
        assert_eq!(
            storage.read_peer_payload().as_deref(),
            Some(payload_b.as_slice())
        );
    }

    /// Already-paired node (paired_on_entry=true) without button held returns
    /// NODE_ACK_ALREADY_PAIRED (defense-in-depth).
    #[test]
    fn handle_node_provision_already_paired_on_entry_no_button() {
        let mut storage = MockStorage::with_key(0x0099, [0x55u8; 32]);
        let mut maps = MapStorage::new(1024);

        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);
        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, true);
        assert_eq!(status, NODE_ACK_ALREADY_PAIRED);

        // Original key is unchanged
        let key = storage.read_key().unwrap();
        assert_eq!(key.0, 0x0099);
        assert_eq!(key.1, [0x55u8; 32]);
    }

    // --- handle_node_provision: T-N906 factory reset on button hold ---

    /// T-N906: Pairing button held → factory reset before writing new credentials.
    #[test]
    fn t_n906_factory_reset_on_button_hold() {
        // Node already has credentials and a stored payload.
        let mut storage = MockStorage::with_key(0x0099, [0x55u8; 32]);
        storage.peer_payload = Some(vec![0xFF; 10]);
        storage.reg_complete = true;
        let mut maps = MapStorage::new(1024);

        let psk_new = [0x77u8; 32];
        let payload_new = vec![0x12, 0x34];
        let provision = make_provision(0x00AA, psk_new, 7, &payload_new);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, true, true);
        assert_eq!(status, NODE_ACK_SUCCESS);

        // New credentials written
        let key = storage.read_key().expect("new key must be stored");
        assert_eq!(key.0, 0x00AA);
        assert_eq!(key.1, psk_new);
        assert_eq!(storage.read_channel(), Some(7));
        assert_eq!(
            storage.read_peer_payload().as_deref(),
            Some(payload_new.as_slice())
        );
        // reg_complete cleared by factory reset + provision
        assert!(!storage.read_reg_complete());
    }

    // --- handle_node_provision: T-N907 NVS write failure ---

    /// T-N907: write_key failure → NODE_ACK(0x02).
    #[test]
    fn t_n907_nvs_write_key_failure() {
        let mut storage = MockStorage::new();
        storage.fail_write_key = true;
        let mut maps = MapStorage::new(1024);
        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_STORAGE_ERROR);
    }

    /// T-N907 variant: write_channel failure → NODE_ACK(0x02), key+payload rolled back.
    #[test]
    fn t_n907_nvs_write_channel_failure() {
        let mut storage = MockStorage::new();
        storage.fail_write_channel = true;
        let mut maps = MapStorage::new(1024);
        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_STORAGE_ERROR);
        // Key and peer_payload must be rolled back (ND-0908)
        assert!(storage.read_key().is_none());
        assert!(storage.read_peer_payload().is_none());
    }

    /// T-N907 variant: write_peer_payload failure → NODE_ACK(0x02), key rolled back.
    #[test]
    fn t_n907_nvs_write_peer_payload_failure() {
        let mut storage = MockStorage::new();
        storage.fail_write_peer_payload = true;
        let mut maps = MapStorage::new(1024);
        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_STORAGE_ERROR);
        // Key must be rolled back — no partial credentials (ND-0908)
        assert!(storage.read_key().is_none());
    }

    /// T-N907 variant: write_reg_complete failure → NODE_ACK(0x02), key+payload rolled back.
    #[test]
    fn t_n907_nvs_write_reg_complete_failure() {
        let mut storage = MockStorage::new();
        storage.fail_write_reg_complete = true;
        let mut maps = MapStorage::new(1024);
        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_STORAGE_ERROR);
        // Key and peer_payload must be rolled back (ND-0908)
        assert!(storage.read_key().is_none());
        assert!(storage.read_peer_payload().is_none());
    }

    // --- handle_node_provision: board layout persistence (ND-0608) ---

    /// Board layout present → persisted to flash and NODE_ACK_SUCCESS returned.
    #[test]
    fn handle_provision_with_board_layout_persists() {
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);
        let provision = NodeProvision {
            key_hint: 0x0001,
            psk: [0x42u8; 32],
            rf_channel: 6,
            encrypted_payload: vec![0xAA],
            board_layout: ProvisionedBoardLayout::Provided(BoardLayout::SONDE_SENSOR_NODE_REV_A),
        };

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);
        assert_eq!(
            storage.read_board_layout(),
            Some(BoardLayout::SONDE_SENSOR_NODE_REV_A)
        );
    }

    /// Board layout write failure is fatal and rolls back critical provisioning state.
    #[test]
    fn handle_provision_board_layout_write_failure() {
        let mut storage = MockStorage::new();
        storage.fail_write_board_layout = true;
        let mut maps = MapStorage::new(1024);
        let provision = NodeProvision {
            key_hint: 0x0001,
            psk: [0x42u8; 32],
            rf_channel: 6,
            encrypted_payload: vec![0xAA],
            board_layout: ProvisionedBoardLayout::Provided(BoardLayout::SONDE_SENSOR_NODE_REV_A),
        };

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_STORAGE_ERROR);
        assert!(storage.read_key().is_none());
        assert!(storage.read_peer_payload().is_none());
    }

    /// Layout absent with no stored layout → legacy compatibility layout is synthesized.
    #[test]
    fn handle_provision_without_board_layout_writes_legacy_compat() {
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);
        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);
        assert_eq!(
            storage.read_board_layout(),
            Some(BoardLayout::LEGACY_COMPAT)
        );
    }

    // --- Full round-trip: parse envelope → handle → encode ACK ---

    #[test]
    fn full_roundtrip_from_ble_write() {
        // Build a raw BLE GATT write as it would arrive from the phone
        let psk = [0x42u8; 32];
        let payload = vec![0xDE, 0xAD];

        let mut body = vec![0u8; 37 + payload.len()];
        body[0] = 0x00;
        body[1] = 0x01; // key_hint = 1
        body[2..34].copy_from_slice(&psk);
        body[34] = 6; // channel
        body[35] = 0x00;
        body[36] = payload.len() as u8;
        body[37..].copy_from_slice(&payload);

        let gatt_write = encode_ble_envelope(BLE_MSG_NODE_PROVISION, &body).unwrap();

        // Parse envelope
        let (msg_type, body_slice) = parse_ble_envelope(&gatt_write).unwrap();
        assert_eq!(msg_type, BLE_MSG_NODE_PROVISION);

        // Parse provision
        let provision = parse_node_provision(body_slice).unwrap();

        // Handle
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);
        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        let ack = encode_node_ack(status);
        let (ack_type, ack_body) = parse_ble_envelope(&ack).unwrap();
        assert_eq!(ack_type, BLE_MSG_NODE_ACK);
        assert_eq!(ack_body, &[NODE_ACK_SUCCESS]);

        // Verify NVS
        assert_eq!(storage.read_key().unwrap().1, psk);
        assert_eq!(storage.read_channel(), Some(6));
        assert_eq!(
            storage.read_peer_payload().as_deref(),
            Some(payload.as_slice())
        );
        assert!(!storage.read_reg_complete());
    }

    // ===================================================================
    // Gap 11 (ND-0904): MTU < 247 rejection
    // ===================================================================

    #[test]
    fn test_mtu_below_minimum_rejected() {
        // ND-0904: The negotiated ATT MTU must be >= 247. Connections
        // with a lower MTU must be disconnected.
        assert!(!is_mtu_acceptable(246), "MTU 246 (< 247) must be rejected");
        assert!(!is_mtu_acceptable(100), "MTU 100 must be rejected");
        assert!(
            !is_mtu_acceptable(23),
            "MTU 23 (BLE default) must be rejected"
        );
        assert!(!is_mtu_acceptable(0), "MTU 0 must be rejected");
    }

    #[test]
    fn test_mtu_at_minimum_accepted() {
        // ND-0904: MTU == 247 is the exact boundary — must be accepted.
        assert!(
            is_mtu_acceptable(247),
            "MTU 247 (exact minimum) must be accepted"
        );
    }

    #[test]
    fn test_mtu_above_minimum_accepted() {
        // ND-0904: MTU > 247 must be accepted.
        assert!(is_mtu_acceptable(248), "MTU 248 must be accepted");
        assert!(is_mtu_acceptable(512), "MTU 512 must be accepted");
    }

    #[test]
    fn test_ble_min_att_mtu_constant() {
        // Ensure the shared constant matches the protocol requirement.
        assert_eq!(
            BLE_MIN_ATT_MTU, 247,
            "BLE_MIN_ATT_MTU must be 247 per ND-0904"
        );
    }

    #[test]
    fn run_test_command_valid_is_staged() {
        let body = sonde_protocol::encode_run_test_command(
            sonde_protocol::TEST_TYPE_DIAG_FRAME,
            Some(6),
            &[0x42; 50],
        )
        .unwrap();
        let mut storage = MockStorage::new();

        let (status, staged) = handle_run_test_command(&body, &mut storage);
        assert_eq!(status, sonde_protocol::RUN_TEST_ACK_OK);
        assert_eq!(staged, storage.read_staged_test_command());
        assert_eq!(
            storage.read_staged_test_command().unwrap().rf_channel,
            Some(6)
        );
    }

    #[test]
    fn run_test_command_invalid_diag_frame_is_rejected() {
        let body = sonde_protocol::encode_ble_envelope(
            sonde_protocol::BLE_RUN_TEST_COMMAND,
            &[0xA3, 0x01, 0x01, 0x02, 0x0E, 0x03, 0x41, 0xAA],
        )
        .unwrap();
        let (_, body) = sonde_protocol::parse_ble_envelope(&body).unwrap();
        let mut storage = MockStorage::new();

        let (status, staged) = handle_run_test_command(body, &mut storage);
        assert_eq!(status, sonde_protocol::RUN_TEST_ACK_INVALID);
        assert!(staged.is_none());
        assert!(storage.read_staged_test_command().is_none());
    }

    #[test]
    fn run_test_command_unsupported_type_is_rejected() {
        let body = sonde_protocol::encode_run_test_command(0x99, None, &[]).unwrap();
        let mut storage = MockStorage::new();

        let (status, staged) = handle_run_test_command(&body, &mut storage);
        assert_eq!(status, sonde_protocol::RUN_TEST_ACK_UNSUPPORTED);
        assert!(staged.is_none());
        assert!(storage.read_staged_test_command().is_none());
    }

    #[test]
    fn read_test_result_returns_retained_result_and_no_result_fallback() {
        let mut storage = MockStorage::new();
        let no_result = handle_read_test_result(&[], &storage).unwrap();
        assert_eq!(no_result.status, sonde_protocol::TEST_RESULT_NO_RESULT);
        assert!(no_result.test_type.is_none());

        let retained = sonde_protocol::TestResult {
            status: sonde_protocol::TEST_RESULT_TIMEOUT,
            test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
            reply_frame: None,
            reply_rssi_dbm: None,
            attempt_count: 4,
            elapsed_ms: 8_600,
        };
        storage.test_result = Some(retained.clone());
        assert_eq!(handle_read_test_result(&[], &storage).unwrap(), retained);
    }

    #[test]
    fn read_test_result_sanitizes_invalid_retained_result() {
        let mut storage = MockStorage::new();
        storage.test_result = Some(sonde_protocol::TestResult {
            status: sonde_protocol::TEST_RESULT_OK,
            test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
            reply_frame: None,
            reply_rssi_dbm: Some(-67),
            attempt_count: 2,
            elapsed_ms: 1_500,
        });

        let result = handle_read_test_result(&[], &storage).unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_EXECUTION_ERROR);
        assert_eq!(result.test_type, Some(sonde_protocol::TEST_TYPE_DIAG_FRAME));
        assert!(result.reply_frame.is_none());
        assert!(result.reply_rssi_dbm.is_none());
        assert_eq!(result.attempt_count, 2);
        assert_eq!(result.elapsed_ms, 1_500);
    }

    #[test]
    fn encode_test_result_response_sanitizes_invalid_payload() {
        let invalid = sonde_protocol::TestResult {
            status: sonde_protocol::TEST_RESULT_OK,
            test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
            reply_frame: None,
            reply_rssi_dbm: Some(-67),
            attempt_count: 2,
            elapsed_ms: 1_500,
        };

        let response = encode_test_result_response(&invalid);
        let (msg_type, body) = parse_ble_envelope(&response).unwrap();
        let decoded = sonde_protocol::decode_test_result(body).unwrap();

        assert_eq!(msg_type, sonde_protocol::BLE_TEST_RESULT);
        assert_eq!(decoded.status, sonde_protocol::TEST_RESULT_EXECUTION_ERROR);
        assert_eq!(
            decoded.test_type,
            Some(sonde_protocol::TEST_TYPE_DIAG_FRAME)
        );
        assert!(decoded.reply_frame.is_none());
        assert!(decoded.reply_rssi_dbm.is_none());
        assert_eq!(decoded.attempt_count, 2);
        assert_eq!(decoded.elapsed_ms, 1_500);
    }

    #[test]
    fn execute_staged_test_command_stores_success_result() {
        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let reply = crate::traits::ReceivedFrame {
            data: vec![
                0x12,
                0x34,
                sonde_protocol::MSG_DIAG_REPLY,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rssi_dbm: Some(-67),
        };
        let mut transport = MockTransport::new(vec![Some(reply.clone())]);
        let clock = MockClock::default();

        let result = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_OK);
        assert_eq!(result.test_type, Some(sonde_protocol::TEST_TYPE_DIAG_FRAME));
        assert_eq!(result.reply_frame, Some(reply.data));
        assert_eq!(result.reply_rssi_dbm, Some(-67));
        assert_eq!(result.attempt_count, 1);
        assert!(storage.read_staged_test_command().is_none());
        assert_eq!(storage.read_test_result(), Some(result));
    }

    #[test]
    fn execute_staged_test_command_allows_success_without_reply_rssi() {
        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let reply = crate::traits::ReceivedFrame {
            data: vec![
                0x12,
                0x34,
                sonde_protocol::MSG_DIAG_REPLY,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rssi_dbm: None,
        };
        let mut transport = MockTransport::new(vec![Some(reply)]);
        let clock = MockClock::default();

        let result = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_OK);
        assert_eq!(result.test_type, Some(sonde_protocol::TEST_TYPE_DIAG_FRAME));
        assert!(result.reply_frame.is_some());
        assert!(result.reply_rssi_dbm.is_none());
        assert_eq!(result.attempt_count, 1);
        assert!(storage.read_staged_test_command().is_none());
        assert_eq!(storage.read_test_result(), Some(result));
    }

    #[test]
    fn execute_staged_test_command_times_out_after_retries() {
        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let mut transport = MockTransport::new(vec![None, None, None, None]);
        let clock = MockClock::default();

        let result = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_TIMEOUT);
        assert_eq!(result.attempt_count, 4);
        assert_eq!(transport.sends.len(), 4);
        assert_eq!(*clock.delays_ms.borrow(), vec![200, 200, 200]);
    }

    #[test]
    fn execute_staged_test_command_rejects_truncated_diag_reply() {
        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let truncated_reply = crate::traits::ReceivedFrame {
            data: vec![
                0x12,
                0x34,
                sonde_protocol::MSG_DIAG_REPLY,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1,
            ],
            rssi_dbm: Some(-67),
        };
        let mut transport = MockTransport::new(vec![Some(truncated_reply), None, None, None]);
        let clock = MockClock::default();

        let result = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_TIMEOUT);
        assert_eq!(result.attempt_count, 4);
        assert_eq!(transport.sends.len(), 4);
    }

    #[test]
    fn execute_staged_test_command_rejects_invalid_staged_command() {
        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: None,
            payload: vec![0x42; 50],
        });
        let mut transport = MockTransport::new(vec![]);
        let clock = MockClock::default();

        let result = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_EXECUTION_ERROR);
        assert_eq!(result.test_type, Some(sonde_protocol::TEST_TYPE_DIAG_FRAME));
        assert!(storage.read_staged_test_command().is_none());
    }

    #[test]
    fn execute_staged_test_command_returns_execution_error_when_all_sends_fail() {
        struct FailingSendTransport {
            send_calls: usize,
        }

        impl crate::traits::Transport for FailingSendTransport {
            fn send(&mut self, _frame: &[u8]) -> NodeResult<()> {
                self.send_calls += 1;
                Err(NodeError::Transport("injected send failure"))
            }

            fn recv(&mut self, _timeout_ms: u32) -> NodeResult<Option<Vec<u8>>> {
                Ok(None)
            }
        }

        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let mut transport = FailingSendTransport { send_calls: 0 };
        let clock = MockClock::default();

        let result = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(result.status, sonde_protocol::TEST_RESULT_EXECUTION_ERROR);
        assert_eq!(result.attempt_count, 4);
        assert_eq!(transport.send_calls, 4);
        assert_eq!(*clock.delays_ms.borrow(), vec![200, 200, 200]);
    }

    #[test]
    fn execute_staged_test_command_clears_staged_command_when_result_write_fails() {
        let mut storage = MockStorage::new();
        storage.fail_write_test_result = true;
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let reply = crate::traits::ReceivedFrame {
            data: vec![
                0x12,
                0x34,
                sonde_protocol::MSG_DIAG_REPLY,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1,
            ],
            rssi_dbm: Some(-67),
        };
        let mut transport = MockTransport::new(vec![Some(reply)]);
        let clock = MockClock::default();

        let err = execute_staged_test_command(&mut storage, &mut transport, &clock).unwrap_err();

        assert!(matches!(err, NodeError::StorageError(_)));
        assert!(storage.read_staged_test_command().is_none());
        assert!(storage.read_test_result().is_none());
    }

    #[test]
    fn retained_result_remains_readable_until_overwritten() {
        let mut storage = MockStorage::new();
        let first = sonde_protocol::TestResult {
            status: sonde_protocol::TEST_RESULT_OK,
            test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
            reply_frame: Some(vec![0x12, 0x34, sonde_protocol::MSG_DIAG_REPLY]),
            reply_rssi_dbm: Some(-61),
            attempt_count: 1,
            elapsed_ms: 900,
        };
        storage.write_test_result(&first).unwrap();

        assert_eq!(handle_read_test_result(&[], &storage).unwrap(), first);
        assert_eq!(handle_read_test_result(&[], &storage).unwrap(), first);

        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let reply = crate::traits::ReceivedFrame {
            data: vec![
                0x12,
                0x34,
                sonde_protocol::MSG_DIAG_REPLY,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                2,
            ],
            rssi_dbm: Some(-72),
        };
        let mut transport = MockTransport::new(vec![Some(reply.clone())]);
        let clock = MockClock::default();

        let second = execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        assert_eq!(handle_read_test_result(&[], &storage).unwrap(), second);
        assert_ne!(second, first);
    }

    #[test]
    fn node_provision_succeeds_after_test_execution() {
        let mut storage = MockStorage::new();
        storage.staged_test_command = Some(StagedTestCommand {
            test_type: sonde_protocol::TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0x42; 50],
        });
        let reply = crate::traits::ReceivedFrame {
            data: vec![
                0x12,
                0x34,
                sonde_protocol::MSG_DIAG_REPLY,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                3,
            ],
            rssi_dbm: Some(-67),
        };
        let mut transport = MockTransport::new(vec![Some(reply)]);
        let clock = MockClock::default();
        let mut map_storage = MapStorage::new(1024);

        execute_staged_test_command(&mut storage, &mut transport, &clock)
            .unwrap()
            .unwrap();

        let provision = NodeProvision {
            key_hint: 0x1234,
            psk: [0x42; 32],
            rf_channel: 6,
            encrypted_payload: vec![0xAA; 32],
            board_layout: ProvisionedBoardLayout::Absent,
        };
        let status =
            handle_node_provision(&provision, &mut storage, &mut map_storage, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);
        assert_eq!(storage.read_key(), Some((0x1234, [0x42; 32])));
        assert_eq!(storage.read_channel(), Some(6));
        assert_eq!(storage.read_peer_payload(), Some(vec![0xAA; 32]));
    }

    // -----------------------------------------------------------------------
    // T-N940: NODE_PROVISION with invalid payload_len — rejected (ND-0905)
    // -----------------------------------------------------------------------

    /// T-N940: A NODE_PROVISION where `payload_len` exceeds the remaining
    /// data in the buffer MUST be rejected without reading beyond the buffer
    /// boundary.
    #[test]
    fn t_n940_node_provision_invalid_payload_len_rejected() {
        // payload_len claims 100 bytes, but only 4 follow the header.
        let actual_payload = [0xAA, 0xBB, 0xCC, 0xDD];
        let claimed_len: u16 = 100;
        let mut body = vec![0u8; 37 + actual_payload.len()];

        // key_hint = 0x1234
        body[0] = 0x12;
        body[1] = 0x34;
        // psk: 32 bytes of 0x42
        body[2..34].fill(0x42);
        // rf_channel = 6
        body[34] = 6;
        // payload_len = 100 (BE) — exceeds remaining bytes
        body[35] = (claimed_len >> 8) as u8;
        body[36] = (claimed_len & 0xFF) as u8;
        // actual payload: only 4 bytes
        body[37..37 + actual_payload.len()].copy_from_slice(&actual_payload);

        let err = parse_node_provision(&body).unwrap_err();
        assert_eq!(
            err, "encrypted_payload truncated",
            "must reject before reading beyond the buffer"
        );
    }

    /// T-N940 variant: payload_len = 0xFFFF (maximum u16) with a minimal
    /// body — rejects as "too large" before any read.
    #[test]
    fn t_n940_node_provision_payload_len_max_u16_rejected() {
        let mut body = vec![0u8; 37 + 2]; // only 2 payload bytes
        body[2..34].fill(0x42);
        body[34] = 1; // channel
        body[35] = 0xFF;
        body[36] = 0xFF; // payload_len = 65535

        let err = parse_node_provision(&body).unwrap_err();
        assert_eq!(err, "encrypted_payload too large");
    }
}
