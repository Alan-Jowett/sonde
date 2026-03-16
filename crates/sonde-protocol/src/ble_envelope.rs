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

#[cfg(test)]
mod tests {
    use super::*;

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
}
