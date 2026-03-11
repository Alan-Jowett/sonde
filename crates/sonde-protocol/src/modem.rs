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

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
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
                write!(f, "modem frame body exceeds max {}", SERIAL_MAX_LEN)
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
            if sf.frame_data.is_empty() || sf.frame_data.len() > ESPNOW_MAX_DATA_SIZE {
                return Err(ModemCodecError::BodyTooLong {
                    msg_type: MODEM_MSG_SEND_FRAME,
                    expected_max: ESPNOW_MAX_DATA_SIZE,
                    actual: sf.frame_data.len(),
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
            if rf.frame_data.is_empty() || rf.frame_data.len() > ESPNOW_MAX_DATA_SIZE {
                return Err(ModemCodecError::BodyTooLong {
                    msg_type: MODEM_MSG_RECV_FRAME,
                    expected_max: ESPNOW_MAX_DATA_SIZE,
                    actual: rf.frame_data.len(),
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
            let count = core::cmp::min(sr.entries.len(), u8::MAX as usize);
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
        ModemMessage::Unknown { msg_type, body } => Ok((*msg_type, body.clone())),
    }
}

// ---------------------------------------------------------------------------
// Frame decoding (single complete frame)
// ---------------------------------------------------------------------------

/// Decode a complete serial frame (LEN || TYPE || BODY) into a `ModemMessage`.
///
/// Returns `Err(EmptyFrame)` if `len` = 0, `Err(FrameTooLarge)` if `len` > 512.
/// Unknown message types are returned as `ModemMessage::Unknown`.
pub fn decode_modem_frame(data: &[u8]) -> Result<ModemMessage, ModemCodecError> {
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

    decode_typed_message(msg_type, body)
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
    /// To prevent unbounded memory growth from garbage or desynchronized
    /// streams, the buffer is capped at `SERIAL_MAX_FRAME_SIZE`. Any input
    /// beyond the cap is silently dropped.
    pub fn push(&mut self, data: &[u8]) {
        let remaining = SERIAL_MAX_FRAME_SIZE.saturating_sub(self.buf.len());
        let to_take = core::cmp::min(remaining, data.len());
        if to_take > 0 {
            self.buf.extend_from_slice(&data[..to_take]);
        }
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
            // Framing error — do NOT consume bytes (value is untrusted).
            // Caller must send RESET and call `reset()`.
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

    // -- Round-trip tests --

    #[test]
    fn round_trip_reset() {
        let msg = ModemMessage::Reset;
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_send_frame() {
        let msg = ModemMessage::SendFrame(SendFrame {
            peer_mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            frame_data: alloc::vec![1, 2, 3, 4, 5],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_set_channel() {
        let msg = ModemMessage::SetChannel(6);
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_get_status() {
        let msg = ModemMessage::GetStatus;
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_scan_channels() {
        let msg = ModemMessage::ScanChannels;
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_modem_ready() {
        let msg = ModemMessage::ModemReady(ModemReady {
            firmware_version: [1, 0, 3, 7],
            mac_address: [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
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
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_set_channel_ack() {
        let msg = ModemMessage::SetChannelAck(11);
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
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
        let decoded = decode_modem_frame(&frame).unwrap();
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
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_error() {
        let msg = ModemMessage::Error(ModemError {
            error_code: MODEM_ERR_ESPNOW_INIT_FAILED,
            message: b"ESP-NOW init failed".to_vec(),
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn round_trip_unknown_type() {
        let msg = ModemMessage::Unknown {
            msg_type: 0x7F,
            body: alloc::vec![1, 2, 3],
        };
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    // -- Frame envelope tests --

    #[test]
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

    #[test]
    fn decode_empty_frame() {
        let data = [0x00, 0x00]; // len = 0
        let err = decode_modem_frame(&data).unwrap_err();
        assert_eq!(err, ModemCodecError::EmptyFrame);
    }

    #[test]
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

    #[test]
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

    #[test]
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
        // Buffer is NOT drained (caller must reset)
        assert_eq!(decoder.buffered(), 2);
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

    #[test]
    fn recv_frame_negative_rssi() {
        let msg = ModemMessage::RecvFrame(RecvFrame {
            peer_mac: [1, 2, 3, 4, 5, 6],
            rssi: -90,
            frame_data: alloc::vec![0x42],
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
        if let ModemMessage::RecvFrame(rf) = decoded {
            assert_eq!(rf.rssi, -90);
        } else {
            panic!("expected RecvFrame");
        }
    }

    // -- STATUS counter boundary test --

    #[test]
    fn status_max_counters() {
        let msg = ModemMessage::Status(ModemStatus {
            channel: 14,
            uptime_s: u32::MAX,
            tx_count: u32::MAX,
            rx_count: u32::MAX,
            tx_fail_count: u32::MAX,
        });
        let frame = encode_modem_frame(&msg).unwrap();
        let decoded = decode_modem_frame(&frame).unwrap();
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
        let decoded = decode_modem_frame(&frame).unwrap();
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
        let decoded = decode_modem_frame(&frame).unwrap();
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
    fn push_caps_buffer_at_max_frame_size() {
        let mut decoder = FrameDecoder::new();
        let big_data = alloc::vec![0u8; SERIAL_MAX_FRAME_SIZE + 100];
        decoder.push(&big_data);
        assert_eq!(decoder.buffered(), SERIAL_MAX_FRAME_SIZE);
    }
}
