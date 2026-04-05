// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE message envelope codec (ble-pairing-protocol.md §4).
//!
//! ```text
//! ┌──────────┬──────────┬────────────────────────────┐
//! │ TYPE (1) │ LEN (2B) │ BODY (LEN bytes)            │
//! └──────────┴──────────┴────────────────────────────┘
//! ```
//!
//! Used by both the node (BLE pairing provisioning) and the gateway
//! (phone registration over BLE relay).

use alloc::vec::Vec;

use crate::constants::MAX_FRAME_SIZE;
use crate::error::{DecodeError, EncodeError};

/// Parse a BLE message envelope.
///
/// Returns `(msg_type, body_slice)` on success, or `None` if the buffer is
/// shorter than the 3-byte header, shorter than `3 + LEN`, or contains
/// trailing bytes after the envelope.
pub fn parse_ble_envelope(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() < 3 {
        return None;
    }
    let msg_type = data[0];
    let body_len = u16::from_be_bytes([data[1], data[2]]) as usize;
    if data.len() != 3 + body_len {
        return None;
    }
    Some((msg_type, &data[3..3 + body_len]))
}

/// Encode a BLE message envelope.
///
/// Returns `None` if `body.len()` exceeds `u16::MAX` (65535 bytes), since the
/// 2-byte LEN field cannot represent larger sizes.
pub fn encode_ble_envelope(msg_type: u8, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() > u16::MAX as usize {
        return None;
    }
    let len = body.len() as u16;
    let mut out = Vec::with_capacity(3 + body.len());
    out.push(msg_type);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    Some(out)
}

/// Encode a DIAG_RELAY_REQUEST BLE body.
///
/// Body format: `rf_channel (1B) | payload_len (2B BE) | payload`.
/// Returns `Err` if `rf_channel` is outside 1–13, `payload` is empty,
/// or `payload` exceeds `MAX_FRAME_SIZE`.
pub fn encode_diag_relay_request(rf_channel: u8, payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    if !(1..=13).contains(&rf_channel) {
        return Err(EncodeError::CborError(alloc::format!(
            "invalid DIAG_RELAY_REQUEST parameter: rf_channel {} out of range 1-13",
            rf_channel
        )));
    }
    if payload.is_empty() {
        return Err(EncodeError::CborError(
            "invalid DIAG_RELAY_REQUEST parameter: payload must not be empty".into(),
        ));
    }
    if payload.len() > MAX_FRAME_SIZE {
        return Err(EncodeError::FrameTooLarge);
    }
    let payload_len = payload.len() as u16;
    let mut body = Vec::with_capacity(3 + payload.len());
    body.push(rf_channel);
    body.extend_from_slice(&payload_len.to_be_bytes());
    body.extend_from_slice(payload);
    Ok(body)
}

/// Decode a DIAG_RELAY_REQUEST BLE body.
///
/// Returns `(rf_channel, payload)` on success. Rejects truncated bodies,
/// bodies with trailing bytes, out-of-range channels, empty payloads,
/// and oversized payloads.
pub fn decode_diag_relay_request(body: &[u8]) -> Result<(u8, &[u8]), DecodeError> {
    if body.len() < 3 {
        return Err(DecodeError::TooShort);
    }
    let rf_channel = body[0];
    if !(1..=13).contains(&rf_channel) {
        return Err(DecodeError::CborError(alloc::format!(
            "rf_channel {} out of range 1-13",
            rf_channel
        )));
    }
    let payload_len = u16::from_be_bytes([body[1], body[2]]) as usize;
    if payload_len == 0 || payload_len > MAX_FRAME_SIZE {
        return Err(DecodeError::CborError(alloc::format!(
            "payload_len {} out of range 1-{}",
            payload_len,
            MAX_FRAME_SIZE
        )));
    }
    if body.len() < 3 + payload_len {
        return Err(DecodeError::TooShort);
    }
    if body.len() > 3 + payload_len {
        return Err(DecodeError::TooLong);
    }
    Ok((rf_channel, &body[3..3 + payload_len]))
}

/// Encode a DIAG_RELAY_RESPONSE BLE body.
///
/// Body format: `status (1B) | payload_len (2B BE) | payload`.
/// When `status` ≠ `DIAG_RELAY_STATUS_OK`, `payload` must be empty.
pub fn encode_diag_relay_response(status: u8, payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    if status != crate::constants::DIAG_RELAY_STATUS_OK && !payload.is_empty() {
        return Err(EncodeError::CborError(
            "DIAG_RELAY_RESPONSE: payload must be empty for non-OK status".into(),
        ));
    }
    if payload.len() > MAX_FRAME_SIZE {
        return Err(EncodeError::FrameTooLarge);
    }
    let payload_len = payload.len() as u16;
    let mut body = Vec::with_capacity(3 + payload.len());
    body.push(status);
    body.extend_from_slice(&payload_len.to_be_bytes());
    body.extend_from_slice(payload);
    Ok(body)
}

/// Decode a DIAG_RELAY_RESPONSE BLE body.
///
/// Returns `(status, payload)` on success. Rejects truncated bodies,
/// bodies with trailing bytes, and payloads exceeding `MAX_FRAME_SIZE`.
pub fn decode_diag_relay_response(body: &[u8]) -> Result<(u8, &[u8]), DecodeError> {
    if body.len() < 3 {
        return Err(DecodeError::TooShort);
    }
    let status = body[0];
    let payload_len = u16::from_be_bytes([body[1], body[2]]) as usize;
    if payload_len > MAX_FRAME_SIZE {
        return Err(DecodeError::CborError(alloc::format!(
            "DIAG_RELAY_RESPONSE payload_len {} exceeds MAX_FRAME_SIZE {}",
            payload_len,
            MAX_FRAME_SIZE
        )));
    }
    if body.len() < 3 + payload_len {
        return Err(DecodeError::TooShort);
    }
    if body.len() > 3 + payload_len {
        return Err(DecodeError::TooLong);
    }
    if status != crate::DIAG_RELAY_STATUS_OK && payload_len != 0 {
        return Err(DecodeError::CborError(
            "non-OK DIAG_RELAY_RESPONSE must have empty payload".into(),
        ));
    }
    Ok((status, &body[3..3 + payload_len]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{BLE_DIAG_RELAY_REQUEST, BLE_DIAG_RELAY_RESPONSE};
    use alloc::vec;

    #[test]
    fn round_trip() {
        let body = [0x42u8; 10];
        let encoded = encode_ble_envelope(0x01, &body).unwrap();
        let (msg_type, decoded) = parse_ble_envelope(&encoded).unwrap();
        assert_eq!(msg_type, 0x01);
        assert_eq!(decoded, &body);
    }

    #[test]
    fn empty_body() {
        let encoded = encode_ble_envelope(0x81, &[]).unwrap();
        let (msg_type, body) = parse_ble_envelope(&encoded).unwrap();
        assert_eq!(msg_type, 0x81);
        assert!(body.is_empty());
    }

    #[test]
    fn too_short() {
        assert!(parse_ble_envelope(&[0x01, 0x00]).is_none());
    }

    #[test]
    fn truncated() {
        assert!(parse_ble_envelope(&[0x01, 0x00, 0x04, 0xAA, 0xBB]).is_none());
    }

    #[test]
    fn trailing_bytes() {
        assert!(parse_ble_envelope(&[0x01, 0x00, 0x02, 0xAA, 0xBB, 0xCC]).is_none());
    }

    // T-P114: DIAG_RELAY_REQUEST round-trip
    #[test]
    fn diag_relay_request_round_trip() {
        let payload = [0x42u8; 50];
        let body = encode_diag_relay_request(6, &payload).unwrap();
        let envelope = encode_ble_envelope(BLE_DIAG_RELAY_REQUEST, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&envelope).unwrap();
        assert_eq!(msg_type, BLE_DIAG_RELAY_REQUEST);
        let (rf_channel, decoded_payload) = decode_diag_relay_request(decoded_body).unwrap();
        assert_eq!(rf_channel, 6);
        assert_eq!(decoded_payload, &payload);
    }

    // T-P115: DIAG_RELAY_REQUEST invalid channel rejected
    #[test]
    fn diag_relay_request_invalid_channel() {
        let payload = [0x42u8; 50];
        assert!(encode_diag_relay_request(0, &payload).is_err());
        assert!(encode_diag_relay_request(14, &payload).is_err());
        assert!(encode_diag_relay_request(1, &payload).is_ok());
        assert!(encode_diag_relay_request(13, &payload).is_ok());
    }

    // T-P116: DIAG_RELAY_RESPONSE round-trip (success, timeout, channel_error)
    #[test]
    fn diag_relay_response_round_trip() {
        // status=0x00 with payload
        let payload = [0xAA; 30];
        let body = encode_diag_relay_response(0x00, &payload).unwrap();
        let envelope = encode_ble_envelope(BLE_DIAG_RELAY_RESPONSE, &body).unwrap();
        let (msg_type, decoded_body) = parse_ble_envelope(&envelope).unwrap();
        assert_eq!(msg_type, BLE_DIAG_RELAY_RESPONSE);
        let (status, decoded_payload) = decode_diag_relay_response(decoded_body).unwrap();
        assert_eq!(status, 0x00);
        assert_eq!(decoded_payload, &payload);

        // status=0x01 (timeout), empty payload
        let body = encode_diag_relay_response(0x01, &[]).unwrap();
        let (status, decoded_payload) = decode_diag_relay_response(&body).unwrap();
        assert_eq!(status, 0x01);
        assert!(decoded_payload.is_empty());

        // status=0x02 (channel_error), empty payload
        let body = encode_diag_relay_response(0x02, &[]).unwrap();
        let (status, decoded_payload) = decode_diag_relay_response(&body).unwrap();
        assert_eq!(status, 0x02);
        assert!(decoded_payload.is_empty());
    }

    // T-P115 additional: empty payload rejected
    #[test]
    fn diag_relay_request_empty_payload_rejected() {
        assert!(encode_diag_relay_request(6, &[]).is_err());
    }

    #[test]
    fn diag_relay_request_decode_out_of_range_channel() {
        let body = [14, 0, 1, 0xAA]; // rf_channel=14
        assert!(decode_diag_relay_request(&body).is_err());
    }

    #[test]
    fn diag_relay_request_decode_zero_payload_len() {
        let body = [6, 0, 0]; // payload_len=0
        assert!(decode_diag_relay_request(&body).is_err());
    }

    #[test]
    fn diag_relay_request_decode_trailing_bytes() {
        let body = [6, 0, 1, 0xAA, 0xBB]; // payload_len=1 but 2 data bytes
        assert!(decode_diag_relay_request(&body).is_err());
    }

    #[test]
    fn diag_relay_request_decode_truncated() {
        let body = [6, 0, 5, 0xAA, 0xBB]; // payload_len=5 but only 2 data bytes
        assert!(decode_diag_relay_request(&body).is_err());
    }

    #[test]
    fn diag_relay_response_non_ok_with_payload_rejected() {
        assert!(encode_diag_relay_response(0x01, &[0xAA]).is_err());
        assert!(encode_diag_relay_response(0x02, &[0xBB]).is_err());
    }

    #[test]
    fn diag_relay_response_decode_trailing_bytes() {
        let body = [0x00, 0, 1, 0xAA, 0xBB]; // payload_len=1 but 2 data bytes
        assert!(decode_diag_relay_response(&body).is_err());
    }

    #[test]
    fn diag_relay_response_decode_oversized_payload_rejected() {
        // payload_len > MAX_FRAME_SIZE should be rejected
        let oversized_len = (MAX_FRAME_SIZE as u16) + 1;
        let mut body = vec![0x00]; // status OK
        body.extend_from_slice(&oversized_len.to_be_bytes());
        // Don't need to provide actual payload bytes — the length check
        // should reject before reaching the slice bounds check.
        assert!(decode_diag_relay_response(&body).is_err());
    }
}
