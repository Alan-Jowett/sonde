// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE pairing handler for the node firmware.
//!
//! Implements the platform-independent portion of BLE pairing mode:
//! - NODE_PROVISION message parsing (ble-pairing-protocol.md §6.6)
//! - NVS persistence of PSK, key_hint, channel, peer_payload, reg_complete
//! - NODE_ACK response encoding (ble-pairing-protocol.md §6.7)
//! - Factory-reset-before-provision when the pairing button was held at boot
//!
//! The BLE transport layer (GATT server, advertising, MTU negotiation, LESC
//! pairing) is in `esp_ble_pairing.rs` and is only compiled with the `esp`
//! feature.

use crate::key_store::KeyStore;
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;

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
    /// Optional I2C pin configuration (ND-0608).
    /// `None` if the pairing tool did not include pin config (backward compatible).
    pub pin_config: Option<PinConfig>,
}

/// Board-specific I2C pin assignments (ND-0608).
#[derive(Debug, Clone, PartialEq)]
pub struct PinConfig {
    /// I2C0 SDA GPIO number.
    pub i2c0_sda: u8,
    /// I2C0 SCL GPIO number.
    pub i2c0_scl: u8,
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

    // Parse optional trailing pin config CBOR (ND-0608).
    // Best-effort: if trailing bytes exist but fail to decode as valid
    // CBOR pin config, treat as "no pin config" so provisioning still
    // succeeds (ND-0608 AC#6 backward compatibility).
    let pin_config = if body.len() > expected_len {
        parse_pin_config_cbor(&body[expected_len..]).ok()
    } else {
        None
    };

    Ok(NodeProvision {
        key_hint,
        psk,
        rf_channel,
        encrypted_payload,
        pin_config,
    })
}

/// Parse a CBOR map of pin assignments from trailing NODE_PROVISION bytes (ND-0608).
///
/// CBOR integer keys: 1 = i2c0_sda, 2 = i2c0_scl. Unknown keys are ignored for
/// forward compatibility (values of unknown keys are not validated). Returns
/// `Err` if the CBOR is malformed, a known key has a non-integer value,
/// trailing bytes remain after the map, SDA == SCL, or a pin exceeds the
/// ESP32-C3 GPIO range (0–21); missing keys are allowed and fall back
/// to defaults (`i2c0_sda = 0`, `i2c0_scl = 1`).
fn parse_pin_config_cbor(data: &[u8]) -> Result<PinConfig, &'static str> {
    // Decode from a mutable slice reference so ciborium advances it past
    // the consumed bytes — lets us detect trailing data (ND-0608).
    let mut remaining = data;
    let value: ciborium::Value =
        ciborium::from_reader(&mut remaining).map_err(|_| "pin_config CBOR decode failed")?;
    if !remaining.is_empty() {
        return Err("pin_config: trailing bytes after CBOR map");
    }

    let map = value.as_map().ok_or("pin_config: expected CBOR map")?;

    let mut sda: Option<u8> = None;
    let mut scl: Option<u8> = None;

    for (k, v) in map {
        // Parse keys as i128 so future keys >255 are silently ignored
        // instead of erroring out (forward compatibility).
        let key = k
            .as_integer()
            .map(i128::from)
            .ok_or("pin_config: non-integer key")?;
        match key {
            1 => {
                let val = v
                    .as_integer()
                    .and_then(|i| u8::try_from(i).ok())
                    .ok_or("pin_config: non-integer value for key 1")?;
                sda = Some(val);
            }
            2 => {
                let val = v
                    .as_integer()
                    .and_then(|i| u8::try_from(i).ok())
                    .ok_or("pin_config: non-integer value for key 2")?;
                scl = Some(val);
            }
            _ => {
                // Ignore unknown keys for forward compatibility without
                // validating their value type.
            }
        }
    }

    // Apply defaults for missing keys (backward-compatible with older
    // provisioners that omit the pin config entirely).
    let sda = sda.unwrap_or(0);
    let scl = scl.unwrap_or(1);

    // Semantic validation: SDA and SCL must be distinct and within the
    // ESP32-C3 GPIO range (0–21). Returning Err causes parse_node_provision
    // to treat pin_config as absent rather than persisting an invalid,
    // non-recoverable config (factory reset does not erase pin config).
    const MAX_GPIO: u8 = 21;
    if sda == scl {
        return Err("pin_config: SDA and SCL must be different pins");
    }
    if sda > MAX_GPIO || scl > MAX_GPIO {
        return Err("pin_config: GPIO number out of range (0-21)");
    }

    Ok(PinConfig {
        i2c0_sda: sda,
        i2c0_scl: scl,
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

    // Persist optional pin config (ND-0608) on a best-effort basis.
    // Pin config is non-fatal: if the pairing tool provided a pin config
    // but we fail to persist it, log a warning and continue with
    // NODE_ACK_SUCCESS. The node is effectively provisioned (PSK + peer
    // payload + channel persisted) and pin config falls back to defaults.
    if let Some(ref pins) = provision.pin_config {
        if storage
            .write_i2c0_pins(pins.i2c0_sda, pins.i2c0_scl)
            .is_err()
        {
            log::warn!("failed to persist I2C pin config during provisioning");
        }
    }

    NODE_ACK_SUCCESS
}

// ---------------------------------------------------------------------------
// Diagnostic relay (ble-pairing-protocol.md §6a, ND-1100 through ND-1106)
// ---------------------------------------------------------------------------

/// Parsed DIAG_RELAY_REQUEST parameters.
pub struct DiagRelayParams {
    pub rf_channel: u8,
    pub payload: Vec<u8>,
}

/// Parse and validate a DIAG_RELAY_REQUEST BLE envelope body.
///
/// Returns `Ok(params)` on success, or `Err(encoded_error_response)` if
/// the request is invalid (bad channel or payload size).
pub fn handle_diag_relay_request(body: &[u8]) -> Result<DiagRelayParams, Vec<u8>> {
    use sonde_protocol::{
        decode_diag_relay_request, BLE_DIAG_RELAY_RESPONSE, DIAG_RELAY_STATUS_CHANNEL_ERROR,
        MAX_FRAME_SIZE,
    };

    let (rf_channel, payload) = decode_diag_relay_request(body).map_err(|_| {
        encode_ble_envelope(
            BLE_DIAG_RELAY_RESPONSE,
            &encode_diag_relay_status(DIAG_RELAY_STATUS_CHANNEL_ERROR),
        )
        .expect("error response fits")
    })?;

    if !(1..=13).contains(&rf_channel) || payload.is_empty() || payload.len() > MAX_FRAME_SIZE {
        return Err(encode_ble_envelope(
            BLE_DIAG_RELAY_RESPONSE,
            &encode_diag_relay_status(DIAG_RELAY_STATUS_CHANNEL_ERROR),
        )
        .expect("error response fits"));
    }

    Ok(DiagRelayParams {
        rf_channel,
        payload: payload.to_vec(),
    })
}

fn encode_diag_relay_status(status: u8) -> Vec<u8> {
    sonde_protocol::encode_diag_relay_response(status, &[]).expect("status response fits")
}

/// Encode a DIAG_RELAY_RESPONSE BLE envelope.
pub fn encode_diag_relay_response(status: u8, payload: &[u8]) -> Vec<u8> {
    let body =
        sonde_protocol::encode_diag_relay_response(status, payload).expect("response fits");
    encode_ble_envelope(sonde_protocol::BLE_DIAG_RELAY_RESPONSE, &body)
        .expect("response envelope fits")
}

/// Execute the diagnostic relay: broadcast on ESP-NOW, listen for DIAG_REPLY.
///
/// Retries up to 3 times with 200ms backoff and 2s listen window per attempt
/// (matching WAKE retry parameters per ND-1103).
pub fn do_diag_relay<T: crate::traits::Transport>(
    transport: &mut T,
    params: &DiagRelayParams,
) -> Vec<u8> {
    const DIAG_MAX_RETRIES: u32 = 3;
    const DIAG_RETRY_DELAY_MS: u64 = 200;
    const DIAG_LISTEN_TIMEOUT_MS: u32 = 2000;

    for attempt in 0..=DIAG_MAX_RETRIES {
        if attempt > 0 {
            #[cfg(feature = "esp")]
            std::thread::sleep(std::time::Duration::from_millis(DIAG_RETRY_DELAY_MS));
            #[cfg(not(feature = "esp"))]
            {
                let _ = DIAG_RETRY_DELAY_MS; // avoid unused warning in tests
            }
        }

        if transport.send(&params.payload).is_err() {
            continue;
        }

        // Listen for DIAG_REPLY (msg_type 0x85 at header byte offset 2).
        match transport.recv(DIAG_LISTEN_TIMEOUT_MS) {
            Ok(Some(raw)) => {
                if raw.len() >= 3 && raw[sonde_protocol::OFFSET_MSG_TYPE] == sonde_protocol::MSG_DIAG_REPLY {
                    return encode_diag_relay_response(
                        sonde_protocol::DIAG_RELAY_STATUS_OK,
                        &raw,
                    );
                }
                // Wrong msg_type — discard and continue listening
            }
            _ => continue,
        }
    }

    encode_diag_relay_response(sonde_protocol::DIAG_RELAY_STATUS_TIMEOUT, &[])
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
        i2c0_pins: Option<(u8, u8)>,
        fail_write_key: bool,
        fail_write_channel: bool,
        fail_write_peer_payload: bool,
        fail_write_reg_complete: bool,
        fail_write_i2c0_pins: bool,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                key: None,
                channel: None,
                peer_payload: None,
                reg_complete: false,
                i2c0_pins: None,
                fail_write_key: false,
                fail_write_channel: false,
                fail_write_peer_payload: false,
                fail_write_reg_complete: false,
                fail_write_i2c0_pins: false,
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
        fn read_i2c0_pins(&self) -> (u8, u8) {
            self.i2c0_pins.unwrap_or((0, 1))
        }
        fn write_i2c0_pins(&mut self, sda: u8, scl: u8) -> NodeResult<()> {
            if self.fail_write_i2c0_pins {
                return Err(NodeError::StorageError("injected write_i2c0_pins failure"));
            }
            self.i2c0_pins = Some((sda, scl));
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
            pin_config: None,
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
    fn parse_node_provision_trailing_bytes_accepted_as_pin_config() {
        // Valid 37-byte header + 0-byte payload + CBOR pin config
        // CBOR: {1: 4, 2: 5} = A2 01 04 02 05
        let pin_cbor = [0xA2, 0x01, 0x04, 0x02, 0x05];
        let mut body = vec![0u8; 37 + pin_cbor.len()];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0
        body[37..].copy_from_slice(&pin_cbor);
        let provision = parse_node_provision(&body).unwrap();
        let pins = provision.pin_config.expect("pin_config should be present");
        assert_eq!(pins.i2c0_sda, 4);
        assert_eq!(pins.i2c0_scl, 5);
    }

    #[test]
    fn parse_node_provision_trailing_non_cbor_treated_as_no_pin_config() {
        // Trailing bytes that aren't valid CBOR — best-effort parsing
        // treats this as "no pin config" (ND-0608 AC#6 backward compat).
        let mut body = vec![0u8; 38];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0
        body[37] = 0xFF; // invalid CBOR
        let provision = parse_node_provision(&body).unwrap();
        assert!(
            provision.pin_config.is_none(),
            "invalid trailing CBOR should be treated as no pin config"
        );
    }

    #[test]
    fn parse_pin_config_cbor_trailing_bytes_rejected() {
        // Valid CBOR map followed by extra bytes — now rejected because
        // parse_pin_config_cbor enforces full consumption (ND-0608 wire
        // format: trailing bytes are exactly the CBOR map, no junk).
        // CBOR: {1: 4, 2: 5} = A2 01 04 02 05, then 0x00 trailing
        let data = [0xA2, 0x01, 0x04, 0x02, 0x05, 0x00];
        let result = parse_pin_config_cbor(&data);
        assert!(
            result.is_err(),
            "trailing bytes after CBOR map should be rejected"
        );
    }

    #[test]
    fn parse_node_provision_cbor_trailing_junk_treated_as_no_pin_config() {
        // Valid CBOR map + trailing junk at provision level — best-effort
        // catches the error from parse_pin_config_cbor and sets pin_config=None.
        // CBOR: {1: 4, 2: 5} = A2 01 04 02 05, then 0x00 trailing
        let data = [0xA2, 0x01, 0x04, 0x02, 0x05, 0x00];
        let mut body = vec![0u8; 37 + data.len()];
        body[2..34].fill(0x42); // psk
        body[34] = 1; // channel 1
        body[35] = 0x00;
        body[36] = 0x00; // payload_len = 0
        body[37..].copy_from_slice(&data);
        let provision = parse_node_provision(&body).unwrap();
        assert!(
            provision.pin_config.is_none(),
            "CBOR trailing junk should be treated as no pin config"
        );
    }

    #[test]
    fn parse_pin_config_cbor_unknown_key_non_integer_value_ignored() {
        // Unknown key 99 with a text string value — should be ignored
        // without failing, even though the value isn't an integer.
        // CBOR: {1: 4, 2: 5, 99: "x"} — forward compatibility.
        let mut buf = Vec::new();
        ciborium::into_writer(
            &ciborium::Value::Map(vec![
                (
                    ciborium::Value::Integer(1.into()),
                    ciborium::Value::Integer(4.into()),
                ),
                (
                    ciborium::Value::Integer(2.into()),
                    ciborium::Value::Integer(5.into()),
                ),
                (
                    ciborium::Value::Integer(99.into()),
                    ciborium::Value::Text("x".into()),
                ),
            ]),
            &mut buf,
        )
        .unwrap();
        let result = parse_pin_config_cbor(&buf).unwrap();
        assert_eq!(result.i2c0_sda, 4);
        assert_eq!(result.i2c0_scl, 5);
    }

    #[test]
    fn parse_pin_config_cbor_key_above_255_ignored() {
        // Key 256 exceeds u8 range — should be silently ignored (forward
        // compat), not cause a parse error.
        let mut buf = Vec::new();
        ciborium::into_writer(
            &ciborium::Value::Map(vec![
                (
                    ciborium::Value::Integer(1.into()),
                    ciborium::Value::Integer(4.into()),
                ),
                (
                    ciborium::Value::Integer(2.into()),
                    ciborium::Value::Integer(5.into()),
                ),
                (
                    ciborium::Value::Integer(256.into()),
                    ciborium::Value::Integer(42.into()),
                ),
            ]),
            &mut buf,
        )
        .unwrap();
        let result = parse_pin_config_cbor(&buf).unwrap();
        assert_eq!(result.i2c0_sda, 4);
        assert_eq!(result.i2c0_scl, 5);
    }

    #[test]
    fn parse_pin_config_cbor_sda_equals_scl_rejected() {
        // SDA == SCL is invalid — would disable I2C. Should return Err so
        // parse_node_provision treats it as absent (backward-compatible).
        // CBOR: {1: 4, 2: 4}
        let mut buf = Vec::new();
        ciborium::into_writer(
            &ciborium::Value::Map(vec![
                (
                    ciborium::Value::Integer(1.into()),
                    ciborium::Value::Integer(4.into()),
                ),
                (
                    ciborium::Value::Integer(2.into()),
                    ciborium::Value::Integer(4.into()),
                ),
            ]),
            &mut buf,
        )
        .unwrap();
        let err = parse_pin_config_cbor(&buf).unwrap_err();
        assert_eq!(err, "pin_config: SDA and SCL must be different pins");
    }

    #[test]
    fn parse_pin_config_cbor_gpio_out_of_range_rejected() {
        // GPIO 22 is out of range for ESP32-C3 (0–21). Should return Err.
        // CBOR: {1: 4, 2: 22}
        let mut buf = Vec::new();
        ciborium::into_writer(
            &ciborium::Value::Map(vec![
                (
                    ciborium::Value::Integer(1.into()),
                    ciborium::Value::Integer(4.into()),
                ),
                (
                    ciborium::Value::Integer(2.into()),
                    ciborium::Value::Integer(22.into()),
                ),
            ]),
            &mut buf,
        )
        .unwrap();
        let err = parse_pin_config_cbor(&buf).unwrap_err();
        assert_eq!(err, "pin_config: GPIO number out of range (0-21)");
    }

    #[test]
    fn parse_pin_config_cbor_boundary_gpio_21_accepted() {
        // GPIO 21 is the maximum valid pin for ESP32-C3 — should succeed.
        // CBOR: {1: 20, 2: 21}
        let mut buf = Vec::new();
        ciborium::into_writer(
            &ciborium::Value::Map(vec![
                (
                    ciborium::Value::Integer(1.into()),
                    ciborium::Value::Integer(20.into()),
                ),
                (
                    ciborium::Value::Integer(2.into()),
                    ciborium::Value::Integer(21.into()),
                ),
            ]),
            &mut buf,
        )
        .unwrap();
        let result = parse_pin_config_cbor(&buf).unwrap();
        assert_eq!(result.i2c0_sda, 20);
        assert_eq!(result.i2c0_scl, 21);
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

    // --- handle_node_provision: pin config persistence (ND-0608) ---

    /// Pin config present → persisted to NVS and NODE_ACK_SUCCESS returned.
    #[test]
    fn handle_provision_with_pin_config_persists() {
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);
        let provision = NodeProvision {
            key_hint: 0x0001,
            psk: [0x42u8; 32],
            rf_channel: 6,
            encrypted_payload: vec![0xAA],
            pin_config: Some(PinConfig {
                i2c0_sda: 4,
                i2c0_scl: 5,
            }),
        };

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);
        assert_eq!(storage.read_i2c0_pins(), (4, 5));
    }

    /// Pin config write failure → NODE_ACK_SUCCESS (non-fatal, ND-0608).
    /// The node is effectively provisioned; pin config falls back to defaults.
    #[test]
    fn handle_provision_pin_config_write_failure() {
        let mut storage = MockStorage::new();
        storage.fail_write_i2c0_pins = true;
        let mut maps = MapStorage::new(1024);
        let provision = NodeProvision {
            key_hint: 0x0001,
            psk: [0x42u8; 32],
            rf_channel: 6,
            encrypted_payload: vec![0xAA],
            pin_config: Some(PinConfig {
                i2c0_sda: 4,
                i2c0_scl: 5,
            }),
        };

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);
    }

    /// Pin config absent → NVS pins untouched, NODE_ACK_SUCCESS returned (backward compat).
    #[test]
    fn handle_provision_without_pin_config_ok() {
        let mut storage = MockStorage::new();
        let mut maps = MapStorage::new(1024);
        let provision = make_provision(0x0001, [0x42u8; 32], 6, &[0xAA]);

        let status = handle_node_provision(&provision, &mut storage, &mut maps, false, false);
        assert_eq!(status, NODE_ACK_SUCCESS);
        // No pin config → NVS pins should still be default
        assert!(storage.i2c0_pins.is_none());
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
