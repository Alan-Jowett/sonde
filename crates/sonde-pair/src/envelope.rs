// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;

/// Parse a BLE message envelope into (msg_type, payload).
///
/// Layout: `[msg_type: 1B] [payload: rest]`
pub fn parse_envelope(data: &[u8]) -> Result<(u8, &[u8]), PairingError> {
    if data.is_empty() {
        return Err(PairingError::InvalidResponse {
            msg_type: 0,
            reason: "empty message".into(),
        });
    }
    Ok((data[0], &data[1..]))
}

/// Build a BLE message envelope: type byte followed by payload.
pub fn build_envelope(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + payload.len());
    buf.push(msg_type);
    buf.extend_from_slice(payload);
    buf
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
    pub gw_ephemeral_public_key: [u8; 32],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

/// Parse a PHONE_REGISTERED payload.
///
/// Layout: `[gw_ephemeral_public_key: 32B] [nonce: 12B] [ciphertext: rest]`
pub fn parse_phone_registered(payload: &[u8]) -> Result<PhoneRegisteredResponse, PairingError> {
    const MIN_LEN: usize = 32 + 12 + 1; // at least 1 byte of ciphertext
    if payload.len() < MIN_LEN {
        return Err(PairingError::InvalidResponse {
            msg_type: 0x82,
            reason: format!("expected at least {MIN_LEN} bytes, got {}", payload.len()),
        });
    }

    let mut gw_ephemeral_public_key = [0u8; 32];
    gw_ephemeral_public_key.copy_from_slice(&payload[..32]);

    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&payload[32..44]);

    let ciphertext = payload[44..].to_vec();

    Ok(PhoneRegisteredResponse {
        gw_ephemeral_public_key,
        nonce,
        ciphertext,
    })
}

/// Parse a NODE_ACK payload (single status byte).
pub fn parse_node_ack(payload: &[u8]) -> Result<u8, PairingError> {
    if payload.len() != 1 {
        return Err(PairingError::InvalidResponse {
            msg_type: 0x83,
            reason: format!("expected 1 byte, got {}", payload.len()),
        });
    }
    Ok(payload[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_envelope_empty() {
        assert!(parse_envelope(&[]).is_err());
    }

    #[test]
    fn parse_envelope_type_only() {
        let (ty, payload) = parse_envelope(&[0x01]).unwrap();
        assert_eq!(ty, 0x01);
        assert!(payload.is_empty());
    }

    #[test]
    fn parse_envelope_with_payload() {
        let data = [0x81, 0xAA, 0xBB, 0xCC];
        let (ty, payload) = parse_envelope(&data).unwrap();
        assert_eq!(ty, 0x81);
        assert_eq!(payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn build_envelope_round_trip() {
        let msg = build_envelope(0x02, &[0x01, 0x02, 0x03]);
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
        let mut data = vec![0u8; 45]; // 32 + 12 + 1
        data[0] = 0xAA;
        data[32] = 0xBB;
        data[44] = 0xCC;

        let resp = parse_phone_registered(&data).unwrap();
        assert_eq!(resp.gw_ephemeral_public_key[0], 0xAA);
        assert_eq!(resp.nonce[0], 0xBB);
        assert_eq!(resp.ciphertext, &[0xCC]);
    }

    #[test]
    fn parse_phone_registered_too_short() {
        assert!(parse_phone_registered(&[0u8; 44]).is_err());
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
}
