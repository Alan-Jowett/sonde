// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;

/// Parse a BLE message envelope into (msg_type, payload).
///
/// Layout: `[msg_type: 1B] [len: 2B BE] [payload: len bytes]`
pub fn parse_envelope(data: &[u8]) -> Result<(u8, &[u8]), PairingError> {
    if data.len() < 3 {
        return Err(PairingError::InvalidResponse {
            msg_type: if data.is_empty() { 0 } else { data[0] },
            reason: "envelope too short (need at least 3 bytes)".into(),
        });
    }
    let msg_type = data[0];
    let len = u16::from_be_bytes([data[1], data[2]]) as usize;
    if data.len() != 3 + len {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!(
                "envelope length field says {} but body is {} bytes",
                len,
                data.len() - 3
            ),
        });
    }
    Ok((msg_type, &data[3..3 + len]))
}

/// Build a BLE message envelope: type byte + length (2B BE) + payload.
///
/// Returns `None` if `payload.len()` exceeds `u16::MAX`.
pub fn build_envelope(msg_type: u8, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > u16::MAX as usize {
        return None;
    }
    let len = payload.len() as u16;
    let mut buf = Vec::with_capacity(3 + payload.len());
    buf.push(msg_type);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
    Some(buf)
}

/// Parsed GW_INFO_RESPONSE fields.
pub struct GwInfoResponse {
    pub gw_public_key: [u8; 32],
    pub gateway_id: [u8; 16],
    pub signature: [u8; 64],
}

/// Parse a GW_INFO_RESPONSE payload.
///
/// Layout: `[gw_public_key: 32B] [gateway_id: 16B] [signature: 64B]` = 112 bytes
pub fn parse_gw_info_response(payload: &[u8]) -> Result<GwInfoResponse, PairingError> {
    const EXPECTED: usize = 32 + 16 + 64;
    if payload.len() != EXPECTED {
        return Err(PairingError::InvalidResponse {
            msg_type: 0x81,
            reason: format!("expected {EXPECTED} bytes, got {}", payload.len()),
        });
    }

    let mut gw_public_key = [0u8; 32];
    gw_public_key.copy_from_slice(&payload[..32]);

    let mut gateway_id = [0u8; 16];
    gateway_id.copy_from_slice(&payload[32..48]);

    let mut signature = [0u8; 64];
    signature.copy_from_slice(&payload[48..112]);

    Ok(GwInfoResponse {
        gw_public_key,
        gateway_id,
        signature,
    })
}

/// Parsed PHONE_REGISTERED fields.
pub struct PhoneRegisteredResponse {
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

/// Parse a PHONE_REGISTERED payload.
///
/// Layout: `[nonce: 12B] [ciphertext: rest]`
pub fn parse_phone_registered(payload: &[u8]) -> Result<PhoneRegisteredResponse, PairingError> {
    const MIN_LEN: usize = 12 + 1; // at least 1 byte of ciphertext
    if payload.len() < MIN_LEN {
        return Err(PairingError::InvalidResponse {
            msg_type: 0x82,
            reason: format!("expected at least {MIN_LEN} bytes, got {}", payload.len()),
        });
    }

    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&payload[..12]);

    let ciphertext = payload[12..].to_vec();

    Ok(PhoneRegisteredResponse { nonce, ciphertext })
}

/// Parse a NODE_ACK payload (single status byte).
pub fn parse_node_ack(payload: &[u8]) -> Result<u8, PairingError> {
    if payload.len() != 1 {
        return Err(PairingError::InvalidResponse {
            msg_type: 0x81,
            reason: format!("expected 1 byte, got {}", payload.len()),
        });
    }
    Ok(payload[0])
}

/// Parse an ERROR envelope body into (status, diagnostic_message).
pub fn parse_error_body(payload: &[u8]) -> (u8, String) {
    if payload.is_empty() {
        return (0, String::new());
    }
    let status = payload[0];
    let message = if payload.len() > 1 {
        String::from_utf8_lossy(&payload[1..]).into_owned()
    } else {
        String::new()
    };
    (status, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_envelope_empty() {
        assert!(parse_envelope(&[]).is_err());
    }

    #[test]
    fn parse_envelope_too_short() {
        assert!(parse_envelope(&[0x01]).is_err());
        assert!(parse_envelope(&[0x01, 0x00]).is_err());
    }

    #[test]
    fn parse_envelope_type_only() {
        let (ty, payload) = parse_envelope(&[0x01, 0x00, 0x00]).unwrap();
        assert_eq!(ty, 0x01);
        assert!(payload.is_empty());
    }

    #[test]
    fn parse_envelope_with_payload() {
        let data = [0x81, 0x00, 0x03, 0xAA, 0xBB, 0xCC];
        let (ty, payload) = parse_envelope(&data).unwrap();
        assert_eq!(ty, 0x81);
        assert_eq!(payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_envelope_len_exceeds_data() {
        // LEN says 5 but only 3 bytes of body available
        let data = [0x01, 0x00, 0x05, 0xAA, 0xBB, 0xCC];
        assert!(parse_envelope(&data).is_err());
    }

    #[test]
    fn build_envelope_round_trip() {
        let msg = build_envelope(0x02, &[0x01, 0x02, 0x03]).unwrap();
        let (ty, payload) = parse_envelope(&msg).unwrap();
        assert_eq!(ty, 0x02);
        assert_eq!(payload, &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn parse_gw_info_response_valid() {
        let mut data = vec![0u8; 112];
        data[0] = 0xAA; // first byte of public key
        data[32] = 0xBB; // first byte of gateway_id
        data[48] = 0xCC; // first byte of signature

        let resp = parse_gw_info_response(&data).unwrap();
        assert_eq!(resp.gw_public_key[0], 0xAA);
        assert_eq!(resp.gateway_id[0], 0xBB);
        assert_eq!(resp.signature[0], 0xCC);
    }

    #[test]
    fn parse_gw_info_response_wrong_length() {
        assert!(parse_gw_info_response(&[0u8; 100]).is_err());
        assert!(parse_gw_info_response(&[0u8; 120]).is_err());
    }

    #[test]
    fn parse_phone_registered_valid() {
        let mut data = vec![0u8; 13]; // 12 nonce + 1 ciphertext
        data[0] = 0xAA; // first byte of nonce
        data[12] = 0xCC; // first byte of ciphertext

        let resp = parse_phone_registered(&data).unwrap();
        assert_eq!(resp.nonce[0], 0xAA);
        assert_eq!(resp.ciphertext, &[0xCC]);
    }

    #[test]
    fn parse_phone_registered_too_short() {
        assert!(parse_phone_registered(&[0u8; 12]).is_err());
    }

    #[test]
    fn parse_node_ack_valid() {
        assert_eq!(parse_node_ack(&[0x00]).unwrap(), 0x00);
        assert_eq!(parse_node_ack(&[0x01]).unwrap(), 0x01);
    }

    #[test]
    fn parse_node_ack_wrong_length() {
        assert!(parse_node_ack(&[]).is_err());
        assert!(parse_node_ack(&[0x00, 0x01]).is_err());
    }

    #[test]
    fn parse_error_body_empty() {
        let (status, msg) = parse_error_body(&[]);
        assert_eq!(status, 0);
        assert!(msg.is_empty());
    }

    #[test]
    fn parse_error_body_status_only() {
        let (status, msg) = parse_error_body(&[0x02]);
        assert_eq!(status, 0x02);
        assert!(msg.is_empty());
    }

    #[test]
    fn parse_error_body_with_message() {
        let mut data = vec![0x03];
        data.extend_from_slice(b"already paired");
        let (status, msg) = parse_error_body(&data);
        assert_eq!(status, 0x03);
        assert_eq!(msg, "already paired");
    }

    // --- PT-1200: Malformed BLE envelope tests ---

    /// Wrong TYPE byte: valid envelope but with an unexpected message type.
    #[test]
    fn parse_envelope_unexpected_type_byte() {
        // Build a valid envelope with type 0xAB (not a known type).
        let msg = build_envelope(0xAB, &[0x01, 0x02]).unwrap();
        let (ty, payload) = parse_envelope(&msg).unwrap();
        // Parsing succeeds — type validation is the caller's responsibility.
        assert_eq!(ty, 0xAB);
        assert_eq!(payload, &[0x01, 0x02]);
    }

    /// Truncated LEN field: only 2 bytes total (type + partial length).
    #[test]
    fn parse_envelope_truncated_len() {
        assert!(parse_envelope(&[0x81, 0x00]).is_err());
    }

    /// LEN is valid but body shorter than declared.
    #[test]
    fn parse_envelope_short_body() {
        // Header says 10 bytes of body but only 3 follow.
        let data = [0x01, 0x00, 0x0A, 0xAA, 0xBB, 0xCC];
        let err = parse_envelope(&data).unwrap_err();
        assert!(
            format!("{err}").contains("length field"),
            "error should mention length mismatch: {err}"
        );
    }

    /// LEN is zero but extra trailing bytes present.
    #[test]
    fn parse_envelope_extra_trailing_bytes() {
        // Header says 0 bytes of body but 2 bytes follow.
        let data = [0x01, 0x00, 0x00, 0xAA, 0xBB];
        assert!(
            parse_envelope(&data).is_err(),
            "trailing bytes after declared body must be rejected"
        );
    }

    /// §4.1.1: ERROR(0x01) generic status code parsed correctly.
    #[test]
    fn parse_error_body_generic_status() {
        let (status, msg) = parse_error_body(&[0x01]);
        assert_eq!(status, 0x01);
        assert!(msg.is_empty());
    }

    /// §4.1.1: ERROR(0x01) with diagnostic message.
    #[test]
    fn parse_error_body_generic_with_diagnostic() {
        let mut data = vec![0x01];
        data.extend_from_slice(b"internal error");
        let (status, msg) = parse_error_body(&data);
        assert_eq!(status, 0x01);
        assert_eq!(msg, "internal error");
    }
}
