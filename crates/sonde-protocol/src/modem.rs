// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Modem serial protocol codec.
//!
//! Implements the length-prefixed framing protocol between the gateway and
//! a USB-attached ESP-NOW radio modem, as defined in `modem-protocol.md`.
//!
//! This module is `no_std`-compatible and shared between the gateway
//! (`sonde-gateway`) and the modem firmware (`sonde-modem`) to guarantee
//! wire-format compatibility.

use alloc::vec::Vec;
use core::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Size of the frame length prefix (2 bytes, big-endian u16).
pub const SERIAL_LEN_SIZE: usize = 2;

/// Maximum value of the `len` field (covers TYPE + BODY).
pub const SERIAL_MAX_LEN: u16 = 512;

/// Maximum on-wire frame size including the LEN field.
pub const SERIAL_MAX_FRAME_SIZE: usize = SERIAL_LEN_SIZE + SERIAL_MAX_LEN as usize; // 514

/// Maximum bytes the streaming decoder will buffer. Sized for multiple
/// back-to-back frames in a single read.
const DECODER_BUF_CAP: usize = SERIAL_MAX_FRAME_SIZE * 4;

/// Size of a MAC address.
pub const MAC_SIZE: usize = 6;

// -- Gateway → Modem (commands) --

/// Reset modem state; modem responds with `MODEM_READY`.
pub const MODEM_MSG_RESET: u8 = 0x01;

/// Transmit an ESP-NOW frame to a specified peer (fire-and-forget).
pub const MODEM_MSG_SEND_FRAME: u8 = 0x02;

/// Set the WiFi/ESP-NOW channel; modem responds with `SET_CHANNEL_ACK`.
pub const MODEM_MSG_SET_CHANNEL: u8 = 0x03;

/// Query modem status and counters; modem responds with `STATUS`.
pub const MODEM_MSG_GET_STATUS: u8 = 0x04;

/// Perform a WiFi AP scan; modem responds with `SCAN_RESULT`.
pub const MODEM_MSG_SCAN_CHANNELS: u8 = 0x05;

// -- BLE relay: Gateway → Modem (commands) --

/// Send a BLE indication to the connected phone; fire-and-forget.
pub const MODEM_MSG_BLE_INDICATE: u8 = 0x20;

/// Enable BLE advertising and accept connections for the Gateway Pairing Service.
pub const MODEM_MSG_BLE_ENABLE: u8 = 0x21;

/// Disable BLE advertising and disconnect any active BLE client.
pub const MODEM_MSG_BLE_DISABLE: u8 = 0x22;

/// Accept or reject the BLE Numeric Comparison pairing (response to `BLE_PAIRING_CONFIRM`).
pub const MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY: u8 = 0x23;

// -- BLE relay: Modem → Gateway (events) --

/// A BLE GATT write was received from the connected phone.
pub const MODEM_MSG_BLE_RECV: u8 = 0xA0;

/// A BLE client connected to the Gateway Pairing Service.
pub const MODEM_MSG_BLE_CONNECTED: u8 = 0xA1;

/// The BLE client disconnected from the Gateway Pairing Service.
pub const MODEM_MSG_BLE_DISCONNECTED: u8 = 0xA2;

/// Numeric Comparison pin display request; gateway must show the pin to the operator.
pub const MODEM_MSG_BLE_PAIRING_CONFIRM: u8 = 0xA3;

// -- GPIO / hardware events: Modem → Gateway --

/// A debounced button press was detected on the 1-Wire data line (MD-0603).
pub const MODEM_MSG_EVENT_BUTTON: u8 = 0xB0;

// -- Modem → Gateway (events / responses) --

/// Modem initialized and ready (sent on boot and after `RESET`).
pub const MODEM_MSG_MODEM_READY: u8 = 0x81;

/// An inbound ESP-NOW frame was received from a node.
pub const MODEM_MSG_RECV_FRAME: u8 = 0x82;

/// Confirms a channel change.
pub const MODEM_MSG_SET_CHANNEL_ACK: u8 = 0x84;

/// Modem status and counters (response to `GET_STATUS`).
pub const MODEM_MSG_STATUS: u8 = 0x85;

/// Per-channel AP survey results (response to `SCAN_CHANNELS`).
pub const MODEM_MSG_SCAN_RESULT: u8 = 0x86;

/// Unrecoverable modem error.
pub const MODEM_MSG_ERROR: u8 = 0x8F;

// -- Error codes (body of ERROR message) --

pub const MODEM_ERR_ESPNOW_INIT_FAILED: u8 = 0x01;
pub const MODEM_ERR_WIFI_INIT_FAILED: u8 = 0x02;
pub const MODEM_ERR_CHANNEL_SET_FAILED: u8 = 0x03;
pub const MODEM_ERR_UNKNOWN: u8 = 0xFF;

// -- Body sizes for fixed-layout messages --

/// MODEM_READY body: firmware_version (4B) + mac_address (6B).
pub const MODEM_READY_BODY_SIZE: usize = 4 + MAC_SIZE; // 10

/// STATUS body: channel (1B) + uptime_s (4B) + tx_count (4B) + rx_count (4B) + tx_fail_count (4B).
pub const STATUS_BODY_SIZE: usize = 1 + 4 + 4 + 4 + 4; // 17

/// Minimum SEND_FRAME body: peer_mac (6B) + at least 1 byte of frame_data.
pub const SEND_FRAME_MIN_BODY_SIZE: usize = MAC_SIZE + 1; // 7

/// Maximum SEND_FRAME body: peer_mac (6B) + 250 bytes of frame_data.
pub const SEND_FRAME_MAX_BODY_SIZE: usize = MAC_SIZE + 250; // 256

/// Minimum RECV_FRAME body: peer_mac (6B) + rssi (1B) + at least 1 byte of frame_data.
pub const RECV_FRAME_MIN_BODY_SIZE: usize = MAC_SIZE + 1 + 1; // 8

/// Maximum RECV_FRAME body: peer_mac (6B) + rssi (1B) + 250 bytes of frame_data.
pub const RECV_FRAME_MAX_BODY_SIZE: usize = MAC_SIZE + 1 + 250; // 257

/// Maximum ESP-NOW frame payload size.
pub const ESPNOW_MAX_DATA_SIZE: usize = 250;

/// MaximumBLE_INDICATE / BLE_RECV body size: the serial frame body is at most 511 bytes.
pub const BLE_DATA_MAX_BODY_SIZE: usize = (SERIAL_MAX_LEN as usize) - 1; // 511

/// BLE_CONNECTED body: peer_addr (6B) + mtu (2B BE).
pub const BLE_CONNECTED_BODY_SIZE: usize = MAC_SIZE + 2; // 8

/// BLE_DISCONNECTED body: peer_addr (6B) + reason (1B).
pub const BLE_DISCONNECTED_BODY_SIZE: usize = MAC_SIZE + 1; // 7

/// BLE_PAIRING_CONFIRM body: passkey (4B BE u32).
pub const BLE_PAIRING_CONFIRM_BODY_SIZE: usize = 4;

/// BLE_PAIRING_CONFIRM_REPLY body: accept (1B).
pub const BLE_PAIRING_CONFIRM_REPLY_BODY_SIZE: usize = 1;

/// EVENT_BUTTON body: button_type (1B).
pub const EVENT_BUTTON_BODY_SIZE: usize = 1;

/// Maximum valid BLE Numeric Comparison passkey value (6 digits, 0–999 999).
pub const BLE_PASSKEY_MAX: u32 = 999_999;

/// Minimum negotiated ATT MTU accepted by the codec (spec: always ≥ 247).
pub const BLE_MTU_MIN: u16 = 247;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ModemCodecError {
    /// The `len` field is zero (empty frame).
    EmptyFrame,
    /// The `len` field exceeds `SERIAL_MAX_LEN`.
    FrameTooLarge(u16),
    /// Body is too short for the given message type.
    BodyTooShort {
        msg_type: u8,
        expected_min: usize,
        actual: usize,
    },
    /// Body exceeds the maximum for the given message type.
    BodyTooLong {
        msg_type: u8,
        expected_max: usize,
        actual: usize,
    },
    /// A field value is outside its allowed range.
    InvalidFieldValue {
        msg_type: u8,
        field: &'static str,
        value: usize,
    },
    /// The encoded frame length (TYPE + BODY) exceeds `SERIAL_MAX_LEN`.
    EncodeTooLong,
    /// Need more bytes to complete the current frame.
    Incomplete,
}

impl fmt::Display for ModemCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModemCodecError::EmptyFrame => write!(f, "modem frame len is zero"),
            ModemCodecError::FrameTooLarge(len) => {
                write!(f, "modem frame len {} exceeds max {}", len, SERIAL_MAX_LEN)
            }
            ModemCodecError::BodyTooShort {
                msg_type,
                expected_min,
                actual,
            } => {
                write!(
                    f,
                    "modem msg 0x{:02x} body too short: need {} bytes, got {}",
                    msg_type, expected_min, actual
                )
            }
            ModemCodecError::BodyTooLong {
                msg_type,
                expected_max,
                actual,
            } => {
                write!(
                    f,
                    "modem msg 0x{:02x} body too long: max {} bytes, got {}",
                    msg_type, expected_max, actual
                )
            }
            ModemCodecError::EncodeTooLong => {
                write!(
                    f,
                    "modem frame (TYPE + BODY) exceeds max {} bytes",
                    SERIAL_MAX_LEN
                )
            }
            ModemCodecError::InvalidFieldValue {
                msg_type,
                field,
                value,
            } => {
                write!(
                    f,
                    "modem msg 0x{:02x} field '{}' value {} is out of range",
                    msg_type, field, value
                )
            }
            ModemCodecError::Incomplete => write!(f, "incomplete modem frame"),
        }
    }
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// A decoded modem serial protocol message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ModemMessage {
    // -- Gateway → Modem --
    Reset,
    SendFrame(SendFrame),
    SetChannel(u8),
    GetStatus,
    ScanChannels,

    // -- Modem → Gateway --
    ModemReady(ModemReady),
    RecvFrame(RecvFrame),
    SetChannelAck(u8),
    Status(ModemStatus),
    ScanResult(ScanResult),
    Error(ModemError),

    // -- BLE relay: Gateway → Modem --
    BleIndicate(BleIndicate),
    BleEnable,
    BleDisable,
    BlePairingConfirmReply(BlePairingConfirmReply),

    // -- BLE relay: Modem → Gateway --
    BleRecv(BleRecv),
    BleConnected(BleConnected),
    BleDisconnected(BleDisconnected),
    BlePairingConfirm(BlePairingConfirm),

    // -- GPIO / hardware events: Modem → Gateway --
    EventButton(EventButton),

    /// A message with a recognized framing but unknown type code.
    /// Kept for forward compatibility — receivers should silently discard.
    Unknown {
        msg_type: u8,
        body: Vec<u8>,
    },
}

/// SEND_FRAME (Gateway → Modem): transmit frame_data to peer_mac via ESP-NOW.
#[derive(Debug, Clone, PartialEq)]
pub struct SendFrame {
    pub peer_mac: [u8; MAC_SIZE],
    pub frame_data: Vec<u8>,
}

/// MODEM_READY (Modem → Gateway): modem initialized and ready.
#[derive(Debug, Clone, PartialEq)]
pub struct ModemReady {
    pub firmware_version: [u8; 4],
    pub mac_address: [u8; MAC_SIZE],
}

/// RECV_FRAME (Modem → Gateway): an inbound ESP-NOW frame.
#[derive(Debug, Clone, PartialEq)]
pub struct RecvFrame {
    pub peer_mac: [u8; MAC_SIZE],
    pub rssi: i8,
    pub frame_data: Vec<u8>,
}

/// STATUS (Modem → Gateway): modem health and counters.
#[derive(Debug, Clone, PartialEq)]
pub struct ModemStatus {
    pub channel: u8,
    pub uptime_s: u32,
    pub tx_count: u32,
    pub rx_count: u32,
    pub tx_fail_count: u32,
}

/// SCAN_RESULT entry: per-channel AP survey data.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanEntry {
    pub channel: u8,
    pub ap_count: u8,
    pub strongest_rssi: i8,
}

/// SCAN_RESULT (Modem → Gateway): channel survey results.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanResult {
    pub entries: Vec<ScanEntry>,
}

/// ERROR (Modem → Gateway): unrecoverable modem error.
#[derive(Debug, Clone, PartialEq)]
pub struct ModemError {
    pub error_code: u8,
    pub message: Vec<u8>,
}

/// BLE_INDICATE(Gateway → Modem): send a BLE indication to the connected phone.
#[derive(Debug, Clone, PartialEq)]
pub struct BleIndicate {
    /// Opaque payload relayed to the BLE client (1 .. [`BLE_DATA_MAX_BODY_SIZE`] bytes).
    pub ble_data: Vec<u8>,
}

/// BLE_RECV (Modem → Gateway): a BLE GATT write received from the connected phone.
#[derive(Debug, Clone, PartialEq)]
pub struct BleRecv {
    /// Opaque payload received from the BLE client (1 .. [`BLE_DATA_MAX_BODY_SIZE`] bytes).
    pub ble_data: Vec<u8>,
}

/// BLE_CONNECTED (Modem → Gateway): a BLE client connected and completed LESC pairing.
#[derive(Debug, Clone, PartialEq)]
pub struct BleConnected {
    /// BLE address of the connected phone.
    pub peer_addr: [u8; MAC_SIZE],
    /// Negotiated ATT MTU (always ≥ 247).
    pub mtu: u16,
}

/// BLE_DISCONNECTED (Modem → Gateway): the BLE client disconnected.
#[derive(Debug, Clone, PartialEq)]
pub struct BleDisconnected {
    /// BLE address of the disconnected phone.
    pub peer_addr: [u8; MAC_SIZE],
    /// BLE HCI disconnect reason code.
    pub reason: u8,
}

/// BLE_PAIRING_CONFIRM (Modem → Gateway): Numeric Comparison passkey to display.
#[derive(Debug, Clone, PartialEq)]
pub struct BlePairingConfirm {
    /// 6-digit Numeric Comparison passkey (0–999999). Display as zero-padded 6 digits.
    pub passkey: u32,
}

/// BLE_PAIRING_CONFIRM_REPLY (Gateway → Modem): accept or reject the Numeric Comparison pairing.
#[derive(Debug, Clone, PartialEq)]
pub struct BlePairingConfirmReply {
    /// `true` = accept (operator confirmed pin matches), `false` = reject.
    pub accept: bool,
}

/// EVENT_BUTTON (Modem → Gateway): a debounced button press was detected (MD-0603).
#[derive(Debug, Clone, PartialEq)]
pub struct EventButton {
    /// `0x00` = BUTTON_SHORT (press < 1 s), `0x01` = BUTTON_LONG (press ≥ 1 s).
    pub button_type: u8,
}

/// BUTTON_SHORT: press duration < 1 second.
pub const BUTTON_TYPE_SHORT: u8 = 0x00;

/// BUTTON_LONG: press duration ≥ 1 second.
pub const BUTTON_TYPE_LONG: u8 = 0x01;

// ---------------------------------------------------------------------------
// Frame encoding
// ---------------------------------------------------------------------------

/// Encode a `ModemMessage` into a complete serial frame (LEN || TYPE || BODY).
///
/// Returns `Err(EncodeTooLong)` if the encoded body exceeds the protocol
/// maximum (`SERIAL_MAX_LEN` minus 1 byte for the TYPE field).
pub fn encode_modem_frame(msg: &ModemMessage) -> Result<Vec<u8>, ModemCodecError> {
    let (msg_type, body) = encode_body(msg)?;
    let total_len = 1 + body.len(); // TYPE + BODY
    if total_len > SERIAL_MAX_LEN as usize {
        return Err(ModemCodecError::EncodeTooLong);
    }
    let len = total_len as u16;
    let mut frame = Vec::with_capacity(SERIAL_LEN_SIZE + total_len);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.push(msg_type);
    frame.extend_from_slice(&body);
    Ok(frame)
}

fn encode_body(msg: &ModemMessage) -> Result<(u8, Vec<u8>), ModemCodecError> {
    match msg {
        ModemMessage::Reset => Ok((MODEM_MSG_RESET, Vec::new())),
        ModemMessage::SendFrame(sf) => {
            if sf.frame_data.is_empty() {
                return Err(ModemCodecError::BodyTooShort {
                    msg_type: MODEM_MSG_SEND_FRAME,
                    expected_min: SEND_FRAME_MIN_BODY_SIZE,
                    actual: MAC_SIZE, // only MAC, no data
                });
            }
            if sf.frame_data.len() > ESPNOW_MAX_DATA_SIZE {
                return Err(ModemCodecError::BodyTooLong {
                    msg_type: MODEM_MSG_SEND_FRAME,
                    expected_max: SEND_FRAME_MAX_BODY_SIZE,
                    actual: MAC_SIZE + sf.frame_data.len(),
                });
            }
            let mut body = Vec::with_capacity(MAC_SIZE + sf.frame_data.len());
            body.extend_from_slice(&sf.peer_mac);
            body.extend_from_slice(&sf.frame_data);
            Ok((MODEM_MSG_SEND_FRAME, body))
        }
        ModemMessage::SetChannel(ch) => Ok((MODEM_MSG_SET_CHANNEL, alloc::vec![*ch])),
        ModemMessage::GetStatus => Ok((MODEM_MSG_GET_STATUS, Vec::new())),
        ModemMessage::ScanChannels => Ok((MODEM_MSG_SCAN_CHANNELS, Vec::new())),
        ModemMessage::ModemReady(mr) => {
            let mut body = Vec::with_capacity(MODEM_READY_BODY_SIZE);
            body.extend_from_slice(&mr.firmware_version);
            body.extend_from_slice(&mr.mac_address);
            Ok((MODEM_MSG_MODEM_READY, body))
        }
        ModemMessage::RecvFrame(rf) => {
            if rf.frame_data.is_empty() {
                return Err(ModemCodecError::BodyTooShort {
                    msg_type: MODEM_MSG_RECV_FRAME,
                    expected_min: RECV_FRAME_MIN_BODY_SIZE,
                    actual: MAC_SIZE + 1, // MAC + RSSI, no data
                });
            }
            if rf.frame_data.len() > ESPNOW_MAX_DATA_SIZE {
                return Err(ModemCodecError::BodyTooLong {
                    msg_type: MODEM_MSG_RECV_FRAME,
                    expected_max: RECV_FRAME_MAX_BODY_SIZE,
                    actual: MAC_SIZE + 1 + rf.frame_data.len(),
                });
            }
            let mut body = Vec::with_capacity(MAC_SIZE + 1 + rf.frame_data.len());
            body.extend_from_slice(&rf.peer_mac);
            body.push(rf.rssi as u8);
            body.extend_from_slice(&rf.frame_data);
            Ok((MODEM_MSG_RECV_FRAME, body))
        }
        ModemMessage::SetChannelAck(ch) => Ok((MODEM_MSG_SET_CHANNEL_ACK, alloc::vec![*ch])),
        ModemMessage::Status(s) => {
            let mut body = Vec::with_capacity(STATUS_BODY_SIZE);
            body.push(s.channel);
            body.extend_from_slice(&s.uptime_s.to_be_bytes());
            body.extend_from_slice(&s.tx_count.to_be_bytes());
            body.extend_from_slice(&s.rx_count.to_be_bytes());
            body.extend_from_slice(&s.tx_fail_count.to_be_bytes());
            Ok((MODEM_MSG_STATUS, body))
        }
        ModemMessage::ScanResult(sr) => {
            // Max body = SERIAL_MAX_LEN - 1 (TYPE) = 511 bytes.
            // Body = 1 (count) + count * 3 (entries), so max count = (511 - 1) / 3 = 170.
            let max_entries = core::cmp::min(u8::MAX as usize, 170);
            let count = core::cmp::min(sr.entries.len(), max_entries);
            let mut body = Vec::with_capacity(1 + count * 3);
            body.push(count as u8);
            for entry in sr.entries.iter().take(count) {
                body.push(entry.channel);
                body.push(entry.ap_count);
                body.push(entry.strongest_rssi as u8);
            }
            Ok((MODEM_MSG_SCAN_RESULT, body))
        }
        ModemMessage::Error(e) => {
            let mut body = Vec::with_capacity(1 + e.message.len());
            body.push(e.error_code);
            body.extend_from_slice(&e.message);
            Ok((MODEM_MSG_ERROR, body))
        }
        ModemMessage::BleIndicate(bi) => {
            if bi.ble_data.is_empty() {
                return Err(ModemCodecError::BodyTooShort {
                    msg_type: MODEM_MSG_BLE_INDICATE,
                    expected_min: 1,
                    actual: 0,
                });
            }
            if bi.ble_data.len() > BLE_DATA_MAX_BODY_SIZE {
                return Err(ModemCodecError::BodyTooLong {
                    msg_type: MODEM_MSG_BLE_INDICATE,
                    expected_max: BLE_DATA_MAX_BODY_SIZE,
                    actual: bi.ble_data.len(),
                });
            }
            Ok((MODEM_MSG_BLE_INDICATE, bi.ble_data.clone()))
        }
        ModemMessage::BleEnable => Ok((MODEM_MSG_BLE_ENABLE, Vec::new())),
        ModemMessage::BleDisable => Ok((MODEM_MSG_BLE_DISABLE, Vec::new())),
        ModemMessage::BlePairingConfirmReply(r) => Ok((
            MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY,
            alloc::vec![r.accept as u8],
        )),
        ModemMessage::BleRecv(br) => {
            if br.ble_data.is_empty() {
                return Err(ModemCodecError::BodyTooShort {
                    msg_type: MODEM_MSG_BLE_RECV,
                    expected_min: 1,
                    actual: 0,
                });
            }
            if br.ble_data.len() > BLE_DATA_MAX_BODY_SIZE {
                return Err(ModemCodecError::BodyTooLong {
                    msg_type: MODEM_MSG_BLE_RECV,
                    expected_max: BLE_DATA_MAX_BODY_SIZE,
                    actual: br.ble_data.len(),
                });
            }
            Ok((MODEM_MSG_BLE_RECV, br.ble_data.clone()))
        }
        ModemMessage::BleConnected(bc) => {
            if bc.mtu < BLE_MTU_MIN {
                return Err(ModemCodecError::InvalidFieldValue {
                    msg_type: MODEM_MSG_BLE_CONNECTED,
                    field: "mtu",
                    value: bc.mtu as usize,
                });
            }
            let mut body = Vec::with_capacity(BLE_CONNECTED_BODY_SIZE);
            body.extend_from_slice(&bc.peer_addr);
            body.extend_from_slice(&bc.mtu.to_be_bytes());
            Ok((MODEM_MSG_BLE_CONNECTED, body))
        }
        ModemMessage::BleDisconnected(bd) => {
            let mut body = Vec::with_capacity(BLE_DISCONNECTED_BODY_SIZE);
            body.extend_from_slice(&bd.peer_addr);
            body.push(bd.reason);
            Ok((MODEM_MSG_BLE_DISCONNECTED, body))
        }
        ModemMessage::BlePairingConfirm(pc) => {
            if pc.passkey > BLE_PASSKEY_MAX {
                return Err(ModemCodecError::InvalidFieldValue {
                    msg_type: MODEM_MSG_BLE_PAIRING_CONFIRM,
                    field: "passkey",
                    value: pc.passkey as usize,
                });
            }
            let mut body = Vec::with_capacity(BLE_PAIRING_CONFIRM_BODY_SIZE);
            body.extend_from_slice(&pc.passkey.to_be_bytes());
            Ok((MODEM_MSG_BLE_PAIRING_CONFIRM, body))
        }
        ModemMessage::EventButton(eb) => {
            if eb.button_type > BUTTON_TYPE_LONG {
                return Err(ModemCodecError::InvalidFieldValue {
                    msg_type: MODEM_MSG_EVENT_BUTTON,
                    field: "button_type",
                    value: eb.button_type as usize,
                });
            }
            Ok((MODEM_MSG_EVENT_BUTTON, alloc::vec![eb.button_type]))
        }
        ModemMessage::Unknown { msg_type, body } => Ok((*msg_type, body.clone())),
    }
}

// ---------------------------------------------------------------------------
// Frame decoding (single complete frame)
// ---------------------------------------------------------------------------

/// Decode a complete serial frame (LEN || TYPE || BODY) into a `ModemMessage`.
///
/// Decodes the first frame from `data` based on the LEN prefix. Returns
/// `(message, bytes_consumed)`. If `data` contains trailing bytes beyond
/// the first frame, they are not consumed — use the returned byte count
/// or the streaming `FrameDecoder` for multi-frame input.
///
/// Returns `Err(EmptyFrame)` if `len` = 0, `Err(FrameTooLarge)` if `len` > 512.
/// Unknown message types are returned as `ModemMessage::Unknown`.
pub fn decode_modem_frame(data: &[u8]) -> Result<(ModemMessage, usize), ModemCodecError> {
    if data.len() < SERIAL_LEN_SIZE {
        return Err(ModemCodecError::Incomplete);
    }

    let len = u16::from_be_bytes([data[0], data[1]]);
    if len == 0 {
        return Err(ModemCodecError::EmptyFrame);
    }
    if len > SERIAL_MAX_LEN {
        return Err(ModemCodecError::FrameTooLarge(len));
    }

    let expected_total = SERIAL_LEN_SIZE + len as usize;
    if data.len() < expected_total {
        return Err(ModemCodecError::Incomplete);
    }

    let msg_type = data[SERIAL_LEN_SIZE];
    let body = &data[SERIAL_LEN_SIZE + 1..expected_total];

    decode_typed_message(msg_type, body).map(|msg| (msg, expected_total))
}

/// Validate that `body` has exactly `expected` bytes.
fn check_exact_body(msg_type: u8, body: &[u8], expected: usize) -> Result<(), ModemCodecError> {
    if body.len() < expected {
        return Err(ModemCodecError::BodyTooShort {
            msg_type,
            expected_min: expected,
            actual: body.len(),
        });
    }
    if body.len() > expected {
        return Err(ModemCodecError::BodyTooLong {
            msg_type,
            expected_max: expected,
            actual: body.len(),
        });
    }
    Ok(())
}

/// Validate that `body.len()` is within `[min, max]`.
fn check_body_range(
    msg_type: u8,
    body: &[u8],
    min: usize,
    max: usize,
) -> Result<(), ModemCodecError> {
    if body.len() < min {
        return Err(ModemCodecError::BodyTooShort {
            msg_type,
            expected_min: min,
            actual: body.len(),
        });
    }
    if body.len() > max {
        return Err(ModemCodecError::BodyTooLong {
            msg_type,
            expected_max: max,
            actual: body.len(),
        });
    }
    Ok(())
}

fn decode_typed_message(msg_type: u8, body: &[u8]) -> Result<ModemMessage, ModemCodecError> {
    match msg_type {
        MODEM_MSG_RESET => {
            check_exact_body(msg_type, body, 0)?;
            Ok(ModemMessage::Reset)
        }

        MODEM_MSG_SEND_FRAME => {
            check_body_range(
                msg_type,
                body,
                SEND_FRAME_MIN_BODY_SIZE,
                SEND_FRAME_MAX_BODY_SIZE,
            )?;
            let mut peer_mac = [0u8; MAC_SIZE];
            peer_mac.copy_from_slice(&body[..MAC_SIZE]);
            let frame_data = body[MAC_SIZE..].to_vec();
            Ok(ModemMessage::SendFrame(SendFrame {
                peer_mac,
                frame_data,
            }))
        }

        MODEM_MSG_SET_CHANNEL => {
            check_exact_body(msg_type, body, 1)?;
            Ok(ModemMessage::SetChannel(body[0]))
        }

        MODEM_MSG_GET_STATUS => {
            check_exact_body(msg_type, body, 0)?;
            Ok(ModemMessage::GetStatus)
        }

        MODEM_MSG_SCAN_CHANNELS => {
            check_exact_body(msg_type, body, 0)?;
            Ok(ModemMessage::ScanChannels)
        }

        MODEM_MSG_MODEM_READY => {
            check_exact_body(msg_type, body, MODEM_READY_BODY_SIZE)?;
            let mut firmware_version = [0u8; 4];
            firmware_version.copy_from_slice(&body[..4]);
            let mut mac_address = [0u8; MAC_SIZE];
            mac_address.copy_from_slice(&body[4..4 + MAC_SIZE]);
            Ok(ModemMessage::ModemReady(ModemReady {
                firmware_version,
                mac_address,
            }))
        }

        MODEM_MSG_RECV_FRAME => {
            check_body_range(
                msg_type,
                body,
                RECV_FRAME_MIN_BODY_SIZE,
                RECV_FRAME_MAX_BODY_SIZE,
            )?;
            let mut peer_mac = [0u8; MAC_SIZE];
            peer_mac.copy_from_slice(&body[..MAC_SIZE]);
            let rssi = body[MAC_SIZE] as i8;
            let frame_data = body[MAC_SIZE + 1..].to_vec();
            Ok(ModemMessage::RecvFrame(RecvFrame {
                peer_mac,
                rssi,
                frame_data,
            }))
        }

        MODEM_MSG_SET_CHANNEL_ACK => {
            check_exact_body(msg_type, body, 1)?;
            Ok(ModemMessage::SetChannelAck(body[0]))
        }

        MODEM_MSG_STATUS => {
            check_exact_body(msg_type, body, STATUS_BODY_SIZE)?;
            let channel = body[0];
            let uptime_s = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
            let tx_count = u32::from_be_bytes([body[5], body[6], body[7], body[8]]);
            let rx_count = u32::from_be_bytes([body[9], body[10], body[11], body[12]]);
            let tx_fail_count = u32::from_be_bytes([body[13], body[14], body[15], body[16]]);
            Ok(ModemMessage::Status(ModemStatus {
                channel,
                uptime_s,
                tx_count,
                rx_count,
                tx_fail_count,
            }))
        }

        MODEM_MSG_SCAN_RESULT => {
            if body.is_empty() {
                return Err(ModemCodecError::BodyTooShort {
                    msg_type,
                    expected_min: 1,
                    actual: 0,
                });
            }
            let count = body[0] as usize;
            let expected_len = 1 + count * 3;
            if body.len() != expected_len {
                if body.len() < expected_len {
                    return Err(ModemCodecError::BodyTooShort {
                        msg_type,
                        expected_min: expected_len,
                        actual: body.len(),
                    });
                } else {
                    return Err(ModemCodecError::BodyTooLong {
                        msg_type,
                        expected_max: expected_len,
                        actual: body.len(),
                    });
                }
            }
            let entries_data = &body[1..];
            let mut entries = Vec::with_capacity(count);
            for i in 0..count {
                let offset = i * 3;
                entries.push(ScanEntry {
                    channel: entries_data[offset],
                    ap_count: entries_data[offset + 1],
                    strongest_rssi: entries_data[offset + 2] as i8,
                });
            }
            Ok(ModemMessage::ScanResult(ScanResult { entries }))
        }

        MODEM_MSG_ERROR => {
            if body.is_empty() {
                return Err(ModemCodecError::BodyTooShort {
                    msg_type,
                    expected_min: 1,
                    actual: 0,
                });
            }
            Ok(ModemMessage::Error(ModemError {
                error_code: body[0],
                message: body[1..].to_vec(),
            }))
        }

        MODEM_MSG_BLE_INDICATE => {
            check_body_range(msg_type, body, 1, BLE_DATA_MAX_BODY_SIZE)?;
            Ok(ModemMessage::BleIndicate(BleIndicate {
                ble_data: body.to_vec(),
            }))
        }

        MODEM_MSG_BLE_ENABLE => {
            check_exact_body(msg_type, body, 0)?;
            Ok(ModemMessage::BleEnable)
        }

        MODEM_MSG_BLE_DISABLE => {
            check_exact_body(msg_type, body, 0)?;
            Ok(ModemMessage::BleDisable)
        }

        MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY => {
            check_exact_body(msg_type, body, BLE_PAIRING_CONFIRM_REPLY_BODY_SIZE)?;
            let accept = match body[0] {
                0 => false,
                1 => true,
                other => {
                    return Err(ModemCodecError::InvalidFieldValue {
                        msg_type,
                        field: "accept",
                        value: other as usize,
                    });
                }
            };
            Ok(ModemMessage::BlePairingConfirmReply(
                BlePairingConfirmReply { accept },
            ))
        }

        MODEM_MSG_BLE_RECV => {
            check_body_range(msg_type, body, 1, BLE_DATA_MAX_BODY_SIZE)?;
            Ok(ModemMessage::BleRecv(BleRecv {
                ble_data: body.to_vec(),
            }))
        }

        MODEM_MSG_BLE_CONNECTED => {
            check_exact_body(msg_type, body, BLE_CONNECTED_BODY_SIZE)?;
            let mut peer_addr = [0u8; MAC_SIZE];
            peer_addr.copy_from_slice(&body[..MAC_SIZE]);
            let mtu = u16::from_be_bytes([body[MAC_SIZE], body[MAC_SIZE + 1]]);
            if mtu < BLE_MTU_MIN {
                return Err(ModemCodecError::InvalidFieldValue {
                    msg_type,
                    field: "mtu",
                    value: mtu as usize,
                });
            }
            Ok(ModemMessage::BleConnected(BleConnected { peer_addr, mtu }))
        }

        MODEM_MSG_BLE_DISCONNECTED => {
            check_exact_body(msg_type, body, BLE_DISCONNECTED_BODY_SIZE)?;
            let mut peer_addr = [0u8; MAC_SIZE];
            peer_addr.copy_from_slice(&body[..MAC_SIZE]);
            let reason = body[MAC_SIZE];
            Ok(ModemMessage::BleDisconnected(BleDisconnected {
                peer_addr,
                reason,
            }))
        }

        MODEM_MSG_BLE_PAIRING_CONFIRM => {
            check_exact_body(msg_type, body, BLE_PAIRING_CONFIRM_BODY_SIZE)?;
            let passkey = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
            if passkey > BLE_PASSKEY_MAX {
                return Err(ModemCodecError::InvalidFieldValue {
                    msg_type,
                    field: "passkey",
                    value: passkey as usize,
                });
            }
            Ok(ModemMessage::BlePairingConfirm(BlePairingConfirm {
                passkey,
            }))
        }

        MODEM_MSG_EVENT_BUTTON => {
            check_exact_body(msg_type, body, EVENT_BUTTON_BODY_SIZE)?;
            let button_type = body[0];
            if button_type > BUTTON_TYPE_LONG {
                return Err(ModemCodecError::InvalidFieldValue {
                    msg_type,
                    field: "button_type",
                    value: button_type as usize,
                });
            }
            Ok(ModemMessage::EventButton(EventButton { button_type }))
        }

        _ => Ok(ModemMessage::Unknown {
            msg_type,
            body: body.to_vec(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Streaming frame decoder
// ---------------------------------------------------------------------------

/// Incremental decoder for the modem serial framing protocol.
///
/// Feed bytes via `push()` and extract decoded messages via `decode()`.
/// Handles partial reads, zero-length frames, and oversized frames.
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(SERIAL_MAX_FRAME_SIZE),
        }
    }

    /// Append bytes to the internal buffer.
    ///
    /// The buffer is capped at `DECODER_BUF_CAP` to prevent unbounded growth.
    /// If appending `data` would exceed the cap, the buffer is reset to
    /// recover from a stuck state (e.g., a partial frame that can never
    /// be completed because subsequent bytes were dropped).
    pub fn push(&mut self, data: &[u8]) {
        if self.buf.len() + data.len() > DECODER_BUF_CAP {
            // Overflow — reset rather than silently dropping bytes,
            // which would leave the decoder stuck on an incomplete frame.
            self.buf.clear();
        }
        self.buf.extend_from_slice(data);
    }

    /// Reset the decoder state, discarding any buffered data.
    pub fn reset(&mut self) {
        self.buf.clear();
    }

    /// Try to decode the next complete frame from the buffer.
    ///
    /// Returns `Ok(Some(msg))` if a complete frame was decoded and consumed.
    /// Returns `Ok(None)` if more data is needed.
    /// Returns `Err(EmptyFrame)` if a zero-length frame was consumed (caller
    /// should call `decode()` again for the next frame).
    /// Returns `Err(FrameTooLarge)` if `len` > 512 — the caller should
    /// trigger a RESET-based resynchronization.
    pub fn decode(&mut self) -> Result<Option<ModemMessage>, ModemCodecError> {
        if self.buf.len() < SERIAL_LEN_SIZE {
            return Ok(None);
        }

        let len = u16::from_be_bytes([self.buf[0], self.buf[1]]);

        if len == 0 {
            // Consume the 2-byte zero-length prefix and report.
            self.buf.drain(..SERIAL_LEN_SIZE);
            return Err(ModemCodecError::EmptyFrame);
        }

        if len > SERIAL_MAX_LEN {
            // Framing error — the len value is untrusted so we cannot skip
            // that many bytes. Clear the entire buffer to avoid getting stuck
            // returning FrameTooLarge on every subsequent decode() call.
            // The caller should send RESET to resynchronize.
            self.buf.clear();
            return Err(ModemCodecError::FrameTooLarge(len));
        }

        let total = SERIAL_LEN_SIZE + len as usize;
        if self.buf.len() < total {
            return Ok(None); // need more data
        }

        let msg_type = self.buf[SERIAL_LEN_SIZE];
        let body = &self.buf[SERIAL_LEN_SIZE + 1..total];

        let result = decode_typed_message(msg_type, body);
        self.buf.drain(..total);
        result.map(Some)
    }

    /// Returns the number of bytes currently buffered.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Round-trip tests (T-P080 through T-P082) --

    #[test] // T-P080: ModemMessage round-trip — RESET
    fn round_trip_reset() {
        let msg = ModemMessage::Reset;
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test] // T-P081: ModemMessage round-trip — SEND_FRAME
    fn round_trip_send_frame() {
        let msg = ModemMessage::SendFrame(SendFrame {
            peer_mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            frame_data: alloc::vec![1, 2, 3, 4, 5],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test] // T-P082: ModemMessage round-trip — SET_CHANNEL
    fn round_trip_set_channel() {
        let msg = ModemMessage::SetChannel(6);
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_get_status() {
        let msg = ModemMessage::GetStatus;
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_scan_channels() {
        let msg = ModemMessage::ScanChannels;
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_modem_ready() {
        let msg = ModemMessage::ModemReady(ModemReady {
            firmware_version: [1, 0, 3, 7],
            mac_address: [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_recv_frame() {
        let msg = ModemMessage::RecvFrame(RecvFrame {
            peer_mac: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            rssi: -42,
            frame_data: alloc::vec![0xDE, 0xAD, 0xBE, 0xEF],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_set_channel_ack() {
        let msg = ModemMessage::SetChannelAck(11);
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_status() {
        let msg = ModemMessage::Status(ModemStatus {
            channel: 6,
            uptime_s: 12345,
            tx_count: 100,
            rx_count: 200,
            tx_fail_count: 3,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_scan_result() {
        let msg = ModemMessage::ScanResult(ScanResult {
            entries: alloc::vec![
                ScanEntry {
                    channel: 1,
                    ap_count: 5,
                    strongest_rssi: -30,
                },
                ScanEntry {
                    channel: 6,
                    ap_count: 2,
                    strongest_rssi: -55,
                },
                ScanEntry {
                    channel: 11,
                    ap_count: 0,
                    strongest_rssi: 0,
                },
            ],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_error() {
        let msg = ModemMessage::Error(ModemError {
            error_code: MODEM_ERR_ESPNOW_INIT_FAILED,
            message: b"ESP-NOW init failed".to_vec(),
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_unknown_type() {
        let msg = ModemMessage::Unknown {
            msg_type: 0x7F,
            body: alloc::vec![1, 2, 3],
        };
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- Frame envelope tests --

    #[test] // T-P083: Frame envelope structure
    fn frame_envelope_structure() {
        let msg = ModemMessage::SetChannel(6);
        let frame = encode_modem_frame(&msg).unwrap();
        // LEN = 2 (TYPE + 1 byte body)
        assert_eq!(frame[0], 0x00);
        assert_eq!(frame[1], 0x02);
        // TYPE
        assert_eq!(frame[2], MODEM_MSG_SET_CHANNEL);
        // BODY
        assert_eq!(frame[3], 6);
        assert_eq!(frame.len(), 4);
    }

    #[test]
    fn modem_ready_frame_structure() {
        let msg = ModemMessage::ModemReady(ModemReady {
            firmware_version: [1, 2, 3, 4],
            mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        // LEN = 11 (TYPE + 10 bytes body) → 0x00 0x0B
        assert_eq!(frame[0], 0x00);
        assert_eq!(frame[1], 0x0B);
        assert_eq!(frame[2], MODEM_MSG_MODEM_READY);
        assert_eq!(frame.len(), 2 + 11); // LEN field + len value
    }

    // -- Error handling tests --

    #[test] // T-P084: Decode empty frame rejected
    fn decode_empty_frame() {
        let data = [0x00, 0x00]; // len = 0
        let err = decode_modem_frame(&data).unwrap_err();
        assert_eq!(err, ModemCodecError::EmptyFrame);
    }

    #[test] // T-P085: Decode oversized frame rejected
    fn decode_oversized_frame() {
        let data = [0x02, 0x01]; // len = 513, exceeds 512
        let err = decode_modem_frame(&data).unwrap_err();
        assert_eq!(err, ModemCodecError::FrameTooLarge(513));
    }

    #[test]
    fn decode_incomplete_frame() {
        let data = [0x00, 0x05, MODEM_MSG_RESET]; // says len=5 but only 1 byte of payload
        let err = decode_modem_frame(&data).unwrap_err();
        assert_eq!(err, ModemCodecError::Incomplete);
    }

    #[test]
    fn decode_send_frame_body_too_short() {
        // SEND_FRAME needs at least 7 bytes (6 MAC + 1 data), give it 3
        let mut frame = Vec::new();
        frame.extend_from_slice(&4u16.to_be_bytes()); // len = 4 (TYPE + 3 body)
        frame.push(MODEM_MSG_SEND_FRAME);
        frame.extend_from_slice(&[0x01, 0x02, 0x03]);
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_SEND_FRAME,
                ..
            }
        ));
    }

    #[test]
    fn decode_status_body_too_short() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&5u16.to_be_bytes()); // len = 5 (TYPE + 4 body bytes)
        frame.push(MODEM_MSG_STATUS);
        frame.extend_from_slice(&[1, 0, 0, 0]); // only 4 bytes, need 17
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_STATUS,
                ..
            }
        ));
    }

    // -- FrameDecoder (streaming) tests --

    #[test] // T-P086: Streaming decoder — complete frame
    fn streaming_complete_frame() {
        let mut decoder = FrameDecoder::new();
        let frame = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        decoder.push(&frame);
        let msg = decoder.decode().unwrap().unwrap();
        assert_eq!(msg, ModemMessage::GetStatus);
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn streaming_partial_then_complete() {
        let mut decoder = FrameDecoder::new();
        let frame = encode_modem_frame(&ModemMessage::SetChannel(3)).unwrap();

        // Push one byte at a time
        for (i, &byte) in frame.iter().enumerate() {
            decoder.push(&[byte]);
            if i < frame.len() - 1 {
                assert_eq!(decoder.decode().unwrap(), None);
            }
        }

        let msg = decoder.decode().unwrap().unwrap();
        assert_eq!(msg, ModemMessage::SetChannel(3));
    }

    #[test] // T-P087: Streaming decoder — multiple frames
    fn streaming_multiple_frames() {
        let mut decoder = FrameDecoder::new();
        let f1 = encode_modem_frame(&ModemMessage::Reset).unwrap();
        let f2 = encode_modem_frame(&ModemMessage::GetStatus).unwrap();
        let f3 = encode_modem_frame(&ModemMessage::SetChannel(11)).unwrap();

        // Push all frames at once
        decoder.push(&f1);
        decoder.push(&f2);
        decoder.push(&f3);

        assert_eq!(decoder.decode().unwrap().unwrap(), ModemMessage::Reset);
        assert_eq!(decoder.decode().unwrap().unwrap(), ModemMessage::GetStatus);
        assert_eq!(
            decoder.decode().unwrap().unwrap(),
            ModemMessage::SetChannel(11)
        );
        assert_eq!(decoder.decode().unwrap(), None);
    }

    #[test]
    fn streaming_empty_frame_error() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x00, 0x00]); // len = 0
        let err = decoder.decode().unwrap_err();
        assert_eq!(err, ModemCodecError::EmptyFrame);
        // Buffer should be drained past the zero-length prefix
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn streaming_oversized_frame_error() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0xFF, 0xFF]); // len = 65535
        let err = decoder.decode().unwrap_err();
        assert_eq!(err, ModemCodecError::FrameTooLarge(65535));
        // Buffer is cleared to avoid getting stuck.
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn streaming_reset_clears_buffer() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0xFF, 0xFF, 0x01, 0x02, 0x03]);
        decoder.reset();
        assert_eq!(decoder.buffered(), 0);
        assert_eq!(decoder.decode().unwrap(), None);
    }

    #[test]
    fn streaming_unknown_type_decoded() {
        let mut decoder = FrameDecoder::new();
        // type 0x7F with 2 bytes body
        let mut frame = Vec::new();
        frame.extend_from_slice(&3u16.to_be_bytes()); // len = 3
        frame.push(0x7F);
        frame.extend_from_slice(&[0xAB, 0xCD]);
        decoder.push(&frame);
        let msg = decoder.decode().unwrap().unwrap();
        assert_eq!(
            msg,
            ModemMessage::Unknown {
                msg_type: 0x7F,
                body: alloc::vec![0xAB, 0xCD],
            }
        );
    }

    // -- RSSI sign preservation test --

    #[test] // T-P088: RecvFrame with negative RSSI
    fn recv_frame_negative_rssi() {
        let msg = ModemMessage::RecvFrame(RecvFrame {
            peer_mac: [1, 2, 3, 4, 5, 6],
            rssi: -90,
            frame_data: alloc::vec![0x42],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        if let ModemMessage::RecvFrame(rf) = decoded {
            assert_eq!(rf.rssi, -90);
        } else {
            panic!("expected RecvFrame");
        }
    }

    // -- STATUS counter boundary test --

    #[test] // T-P089: Status with max counters
    fn status_max_counters() {
        let msg = ModemMessage::Status(ModemStatus {
            channel: 14,
            uptime_s: u32::MAX,
            tx_count: u32::MAX,
            rx_count: u32::MAX,
            tx_fail_count: u32::MAX,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- Scan result with 14 channels --

    #[test]
    fn scan_result_all_channels() {
        let entries: Vec<ScanEntry> = (1..=14)
            .map(|ch| ScanEntry {
                channel: ch,
                ap_count: ch * 2,
                strongest_rssi: -(ch as i8 * 5),
            })
            .collect();
        let msg = ModemMessage::ScanResult(ScanResult { entries });
        let frame = encode_modem_frame(&msg).unwrap();
        // body = 1 (count) + 14*3 (entries) = 43, total frame = 2 + 1 + 43 = 46
        assert_eq!(frame.len(), 46);
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- Max-size SEND_FRAME --

    #[test]
    fn send_frame_max_payload() {
        let msg = ModemMessage::SendFrame(SendFrame {
            peer_mac: [0xFF; MAC_SIZE],
            frame_data: alloc::vec![0xAA; 250], // max ESP-NOW payload
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- Encode overflow test --

    #[test]
    fn encode_oversized_body_returns_error() {
        // Body of 512 bytes + 1 byte TYPE = 513 > SERIAL_MAX_LEN (512)
        let msg = ModemMessage::Unknown {
            msg_type: 0x7F,
            body: alloc::vec![0u8; 512],
        };
        let err = encode_modem_frame(&msg).unwrap_err();
        assert_eq!(err, ModemCodecError::EncodeTooLong);
    }

    // -- Push buffer cap test --

    #[test]
    fn push_resets_buffer_on_overflow() {
        let mut decoder = FrameDecoder::new();
        // Fill with some data
        let initial = alloc::vec![0u8; DECODER_BUF_CAP - 10];
        decoder.push(&initial);
        assert_eq!(decoder.buffered(), DECODER_BUF_CAP - 10);

        // Push more than remaining capacity — triggers reset + fresh append
        let overflow = alloc::vec![0xAAu8; 20];
        decoder.push(&overflow);
        assert_eq!(decoder.buffered(), 20); // buffer was cleared then refilled
    }

    // -- BLE relay round-trip tests --

    #[test]
    fn round_trip_ble_indicate() {
        let msg = ModemMessage::BleIndicate(BleIndicate {
            ble_data: alloc::vec![0x01, 0x02, 0x03, 0x04],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, consumed) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn round_trip_ble_enable() {
        let msg = ModemMessage::BleEnable;
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_ble_disable() {
        let msg = ModemMessage::BleDisable;
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_ble_pairing_confirm_reply_accept() {
        let msg = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: true });
        let frame = encode_modem_frame(&msg).unwrap();
        // TYPE = 0x23, BODY = 0x01
        assert_eq!(frame[2], MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY);
        assert_eq!(frame[3], 0x01);
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_ble_pairing_confirm_reply_reject() {
        let msg = ModemMessage::BlePairingConfirmReply(BlePairingConfirmReply { accept: false });
        let frame = encode_modem_frame(&msg).unwrap();
        // BODY = 0x00
        assert_eq!(frame[3], 0x00);
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_ble_recv() {
        let msg = ModemMessage::BleRecv(BleRecv {
            ble_data: alloc::vec![0xDE, 0xAD, 0xBE, 0xEF],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, consumed) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn round_trip_ble_connected() {
        let msg = ModemMessage::BleConnected(BleConnected {
            peer_addr: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            mtu: BLE_MTU_MIN,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        // TYPE = 0xA1, BODY = 6B addr + 2B mtu (BE)
        assert_eq!(frame[2], MODEM_MSG_BLE_CONNECTED);
        assert_eq!(frame[3..9], [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mtu_bytes = BLE_MTU_MIN.to_be_bytes();
        assert_eq!(frame[9], mtu_bytes[0]);
        assert_eq!(frame[10], mtu_bytes[1]);
        let (decoded, consumed) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn round_trip_ble_disconnected() {
        let msg = ModemMessage::BleDisconnected(BleDisconnected {
            peer_addr: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            reason: 0x13,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        assert_eq!(frame[2], MODEM_MSG_BLE_DISCONNECTED);
        assert_eq!(frame[frame.len() - 1], 0x13);
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_ble_pairing_confirm() {
        let msg = ModemMessage::BlePairingConfirm(BlePairingConfirm { passkey: 123456 });
        let frame = encode_modem_frame(&msg).unwrap();
        // TYPE = 0xA3, passkey 123456 = 0x0001E240 in BE
        assert_eq!(frame[2], MODEM_MSG_BLE_PAIRING_CONFIRM);
        assert_eq!(frame[3..7], [0x00, 0x01, 0xE2, 0x40]);
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- BLE relay boundary / error tests --

    #[test]
    fn ble_indicate_empty_body_encode_error() {
        let msg = ModemMessage::BleIndicate(BleIndicate {
            ble_data: alloc::vec![],
        });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_INDICATE,
                ..
            }
        ));
    }

    #[test]
    fn ble_indicate_empty_body_decode_error() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&1u16.to_be_bytes()); // len = 1 (type only, no body)
        frame.push(MODEM_MSG_BLE_INDICATE);
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_INDICATE,
                ..
            }
        ));
    }

    #[test]
    fn ble_recv_empty_body_encode_error() {
        let msg = ModemMessage::BleRecv(BleRecv {
            ble_data: alloc::vec![],
        });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_RECV,
                ..
            }
        ));
    }

    #[test]
    fn ble_recv_empty_body_decode_error() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.push(MODEM_MSG_BLE_RECV);
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_RECV,
                ..
            }
        ));
    }

    #[test]
    fn ble_indicate_max_body() {
        let msg = ModemMessage::BleIndicate(BleIndicate {
            ble_data: alloc::vec![0x42u8; BLE_DATA_MAX_BODY_SIZE],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn ble_recv_max_body() {
        let msg = ModemMessage::BleRecv(BleRecv {
            ble_data: alloc::vec![0x42u8; BLE_DATA_MAX_BODY_SIZE],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn ble_indicate_body_over_max_encode_error() {
        let msg = ModemMessage::BleIndicate(BleIndicate {
            ble_data: alloc::vec![0x42u8; BLE_DATA_MAX_BODY_SIZE + 1],
        });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooLong {
                msg_type: MODEM_MSG_BLE_INDICATE,
                ..
            }
        ));
    }

    #[test]
    fn ble_recv_body_over_max_encode_error() {
        let msg = ModemMessage::BleRecv(BleRecv {
            ble_data: alloc::vec![0x42u8; BLE_DATA_MAX_BODY_SIZE + 1],
        });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooLong {
                msg_type: MODEM_MSG_BLE_RECV,
                ..
            }
        ));
    }

    #[test]
    fn ble_connected_body_too_short_decode_error() {
        // BLE_CONNECTED needs exactly 8 bytes, give it 7
        let mut frame = Vec::new();
        frame.extend_from_slice(&8u16.to_be_bytes()); // len = 8 (TYPE + 7 body)
        frame.push(MODEM_MSG_BLE_CONNECTED);
        frame.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x00]); // 7 bytes
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_CONNECTED,
                ..
            }
        ));
    }

    #[test]
    fn ble_disconnected_body_too_short_decode_error() {
        // BLE_DISCONNECTED needs exactly 7 bytes, give it 6
        let mut frame = Vec::new();
        frame.extend_from_slice(&7u16.to_be_bytes()); // len = 7 (TYPE + 6 body)
        frame.push(MODEM_MSG_BLE_DISCONNECTED);
        frame.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]); // 6 bytes (no reason)
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_DISCONNECTED,
                ..
            }
        ));
    }

    #[test]
    fn ble_pairing_confirm_body_too_short_decode_error() {
        // BLE_PAIRING_CONFIRM needs 4 bytes, give it 3
        let mut frame = Vec::new();
        frame.extend_from_slice(&4u16.to_be_bytes()); // len = 4 (TYPE + 3 body)
        frame.push(MODEM_MSG_BLE_PAIRING_CONFIRM);
        frame.extend_from_slice(&[0x00, 0x01, 0xE2]); // 3 bytes
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooShort {
                msg_type: MODEM_MSG_BLE_PAIRING_CONFIRM,
                ..
            }
        ));
    }

    #[test]
    fn ble_enable_nonempty_body_decode_error() {
        // BLE_ENABLE should have empty body
        let mut frame = Vec::new();
        frame.extend_from_slice(&2u16.to_be_bytes()); // len = 2 (TYPE + 1 body)
        frame.push(MODEM_MSG_BLE_ENABLE);
        frame.push(0xFF);
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooLong {
                msg_type: MODEM_MSG_BLE_ENABLE,
                ..
            }
        ));
    }

    #[test]
    fn ble_disable_nonempty_body_decode_error() {
        // BLE_DISABLE should have empty body
        let mut frame = Vec::new();
        frame.extend_from_slice(&2u16.to_be_bytes()); // len = 2 (TYPE + 1 body)
        frame.push(MODEM_MSG_BLE_DISABLE);
        frame.push(0xFF);
        let err = decode_modem_frame(&frame).unwrap_err();
        assert!(matches!(
            err,
            ModemCodecError::BodyTooLong {
                msg_type: MODEM_MSG_BLE_DISABLE,
                ..
            }
        ));
    }

    #[test]
    fn ble_pairing_confirm_reply_invalid_accept_byte() {
        // Only 0x00 and 0x01 are valid; 0xFF must be rejected
        let mut frame = Vec::new();
        frame.extend_from_slice(&2u16.to_be_bytes());
        frame.push(MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY);
        frame.push(0xFF);
        assert!(matches!(
            decode_modem_frame(&frame),
            Err(ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY,
                field: "accept",
                value: 0xFF,
            })
        ));
    }

    #[test]
    fn ble_message_type_constants() {
        // Verify the constants match the spec values
        assert_eq!(MODEM_MSG_BLE_INDICATE, 0x20);
        assert_eq!(MODEM_MSG_BLE_ENABLE, 0x21);
        assert_eq!(MODEM_MSG_BLE_DISABLE, 0x22);
        assert_eq!(MODEM_MSG_BLE_PAIRING_CONFIRM_REPLY, 0x23);
        assert_eq!(MODEM_MSG_BLE_RECV, 0xA0);
        assert_eq!(MODEM_MSG_BLE_CONNECTED, 0xA1);
        assert_eq!(MODEM_MSG_BLE_DISCONNECTED, 0xA2);
        assert_eq!(MODEM_MSG_BLE_PAIRING_CONFIRM, 0xA3);
    }

    // -- BLE_PAIRING_CONFIRM passkey range tests --

    #[test]
    fn ble_pairing_confirm_passkey_zero() {
        let msg = ModemMessage::BlePairingConfirm(BlePairingConfirm { passkey: 0 });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn ble_pairing_confirm_passkey_boundary() {
        // Exact boundary: BLE_PASSKEY_MAX is valid, BLE_PASSKEY_MAX+1 is not
        let msg = ModemMessage::BlePairingConfirm(BlePairingConfirm {
            passkey: BLE_PASSKEY_MAX,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn ble_pairing_confirm_passkey_over_max_encode_error() {
        let msg = ModemMessage::BlePairingConfirm(BlePairingConfirm {
            passkey: BLE_PASSKEY_MAX + 1,
        });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert_eq!(
            err,
            ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_BLE_PAIRING_CONFIRM,
                field: "passkey",
                value: (BLE_PASSKEY_MAX + 1) as usize,
            }
        );
    }

    #[test]
    fn ble_pairing_confirm_passkey_over_max_decode_error() {
        // Craft a raw frame with passkey = BLE_PASSKEY_MAX+1 = 0x000F4240
        let mut frame = Vec::new();
        frame.extend_from_slice(&5u16.to_be_bytes()); // len = 5 (TYPE + 4 body)
        frame.push(MODEM_MSG_BLE_PAIRING_CONFIRM);
        frame.extend_from_slice(&(BLE_PASSKEY_MAX + 1).to_be_bytes());
        let err = decode_modem_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_BLE_PAIRING_CONFIRM,
                field: "passkey",
                value: (BLE_PASSKEY_MAX + 1) as usize,
            }
        );
    }

    // -- BLE_CONNECTED MTU range tests --

    #[test]
    fn ble_connected_mtu_boundary() {
        // Exact boundary: BLE_MTU_MIN is valid, BLE_MTU_MIN-1 is not
        let msg = ModemMessage::BleConnected(BleConnected {
            peer_addr: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            mtu: BLE_MTU_MIN,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, _) = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn ble_connected_mtu_below_min_encode_error() {
        let msg = ModemMessage::BleConnected(BleConnected {
            peer_addr: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            mtu: BLE_MTU_MIN - 1,
        });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert_eq!(
            err,
            ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_BLE_CONNECTED,
                field: "mtu",
                value: (BLE_MTU_MIN - 1) as usize,
            }
        );
    }

    #[test]
    fn ble_connected_mtu_below_min_decode_error() {
        // Craft a raw frame with mtu = 100 (below BLE_MTU_MIN)
        let mut frame = Vec::new();
        frame.extend_from_slice(&9u16.to_be_bytes()); // len = 9 (TYPE + 8 body)
        frame.push(MODEM_MSG_BLE_CONNECTED);
        frame.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]); // peer_addr
        frame.extend_from_slice(&100u16.to_be_bytes()); // mtu = 100
        let err = decode_modem_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_BLE_CONNECTED,
                field: "mtu",
                value: 100,
            }
        );
    }

    // -- EVENT_BUTTON tests (T-0808) --

    #[test]
    fn event_button_short_roundtrip() {
        let msg = ModemMessage::EventButton(EventButton {
            button_type: BUTTON_TYPE_SHORT,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, consumed) = decode_modem_frame(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn event_button_long_roundtrip() {
        let msg = ModemMessage::EventButton(EventButton {
            button_type: BUTTON_TYPE_LONG,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let (decoded, consumed) = decode_modem_frame(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn event_button_invalid_type_encode_error() {
        let msg = ModemMessage::EventButton(EventButton { button_type: 0x02 });
        let err = encode_modem_frame(&msg).unwrap_err();
        assert_eq!(
            err,
            ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_EVENT_BUTTON,
                field: "button_type",
                value: 2,
            }
        );
    }

    #[test]
    fn event_button_invalid_type_decode_error() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&2u16.to_be_bytes()); // len = 2 (TYPE + 1 body)
        frame.push(MODEM_MSG_EVENT_BUTTON);
        frame.push(0x02); // invalid button_type
        let err = decode_modem_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            ModemCodecError::InvalidFieldValue {
                msg_type: MODEM_MSG_EVENT_BUTTON,
                field: "button_type",
                value: 2,
            }
        );
    }

    #[test]
    fn event_button_wire_format() {
        let msg = ModemMessage::EventButton(EventButton {
            button_type: BUTTON_TYPE_SHORT,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        // LEN = 2 (TYPE + 1 body), TYPE = 0xB0, BODY = 0x00
        assert_eq!(frame, &[0x00, 0x02, 0xB0, 0x00]);
    }
}
