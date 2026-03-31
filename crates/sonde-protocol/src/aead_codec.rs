// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! AES-256-GCM frame codec (feature-gated behind `aes-gcm-codec`).
//!
//! This module provides an authenticated-encryption frame codec that can
//! replace the HMAC-SHA256 codec once all consumers have migrated.
//!
//! Wire format:
//! ```text
//! header(11B) ‖ ciphertext(variable) ‖ tag(16B)
//! ```
//!
//! GCM nonce construction:
//! ```text
//! (first 3 bytes of SHA-256(psk)) ‖ msg_type[1] ‖ frame_nonce[8]   → 12 bytes
//! ```

use alloc::vec::Vec;

use crate::constants::{
    AEAD_TAG_SIZE, GCM_NONCE_SIZE, HEADER_SIZE, MAX_FRAME_SIZE, MIN_FRAME_SIZE_AEAD,
};
use crate::error::{DecodeError, EncodeError};
use crate::header::FrameHeader;
use crate::traits::{AeadProvider, Sha256Provider};

/// A decoded AEAD frame (pre-decryption).
///
/// Borrows the ciphertext+tag region directly from the raw frame to
/// avoid an extra heap allocation on every received frame.
#[derive(Debug, Clone)]
pub struct DecodedFrameAead<'a> {
    pub header: FrameHeader,
    pub ciphertext_and_tag: &'a [u8],
}

/// Construct the 12-byte GCM nonce from PSK, msg_type, and frame nonce.
///
/// ```text
/// gcm_nonce = (first 3 bytes of SHA-256(psk)) ‖ msg_type[1] ‖ frame_nonce[8]
/// ```
pub fn build_gcm_nonce(
    psk: &[u8; 32],
    msg_type: u8,
    frame_nonce: &[u8; 8],
    sha256: &impl Sha256Provider,
) -> [u8; GCM_NONCE_SIZE] {
    let hash = sha256.hash(psk);
    let mut nonce = [0u8; GCM_NONCE_SIZE];
    nonce[0..3].copy_from_slice(&hash[0..3]);
    nonce[3] = msg_type;
    nonce[4..12].copy_from_slice(frame_nonce);
    nonce
}

/// Encode a protocol frame with AES-256-GCM authenticated encryption.
///
/// Returns `header(11B) ‖ ciphertext ‖ tag(16B)`.
/// The 11-byte header is used as AAD (authenticated but not encrypted).
pub fn encode_frame_aead(
    header: &FrameHeader,
    payload_cbor: &[u8],
    psk: &[u8; 32],
    aead: &impl AeadProvider,
    sha: &impl Sha256Provider,
) -> Result<Vec<u8>, EncodeError> {
    let total_size = HEADER_SIZE + payload_cbor.len() + AEAD_TAG_SIZE;
    if total_size > MAX_FRAME_SIZE {
        return Err(EncodeError::FrameTooLarge);
    }

    let header_bytes = header.to_bytes();
    let frame_nonce_bytes = header.nonce.to_be_bytes();
    let gcm_nonce = build_gcm_nonce(psk, header.msg_type, &frame_nonce_bytes, sha);

    let ciphertext_and_tag = aead.seal(psk, &gcm_nonce, &header_bytes, payload_cbor);

    let frame_len = HEADER_SIZE + ciphertext_and_tag.len();
    if frame_len > MAX_FRAME_SIZE {
        return Err(EncodeError::FrameTooLarge);
    }

    let mut frame = Vec::with_capacity(frame_len);
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(&ciphertext_and_tag);

    Ok(frame)
}

/// Decode a raw AEAD frame into its components without decryption.
///
/// Splits: `header(11B) | ciphertext+tag(rest)`.
/// The returned [`DecodedFrameAead`] borrows the ciphertext+tag region
/// directly from `raw` (zero-copy).
/// The caller must use [`open_frame`] to decrypt and authenticate.
pub fn decode_frame_aead(raw: &[u8]) -> Result<DecodedFrameAead<'_>, DecodeError> {
    if raw.len() < MIN_FRAME_SIZE_AEAD {
        return Err(DecodeError::TooShort);
    }
    if raw.len() > MAX_FRAME_SIZE {
        return Err(DecodeError::TooLong);
    }

    let header_bytes: [u8; HEADER_SIZE] = raw[..HEADER_SIZE]
        .try_into()
        .map_err(|_| DecodeError::TooShort)?;
    let header = FrameHeader::from_bytes(&header_bytes);

    let ciphertext_and_tag = &raw[HEADER_SIZE..];

    Ok(DecodedFrameAead {
        header,
        ciphertext_and_tag,
    })
}

/// Decrypt and authenticate a decoded AEAD frame.
///
/// Returns the plaintext CBOR payload on success, or
/// `DecodeError::AuthenticationFailed` if the GCM tag check fails.
pub fn open_frame(
    frame: &DecodedFrameAead<'_>,
    psk: &[u8; 32],
    aead: &impl AeadProvider,
    sha: &impl Sha256Provider,
) -> Result<Vec<u8>, DecodeError> {
    let header_bytes = frame.header.to_bytes();
    let frame_nonce_bytes = frame.header.nonce.to_be_bytes();
    let gcm_nonce = build_gcm_nonce(psk, frame.header.msg_type, &frame_nonce_bytes, sha);

    aead.open(psk, &gcm_nonce, &header_bytes, frame.ciphertext_and_tag)
        .ok_or(DecodeError::AuthenticationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::*;
    use alloc::vec;

    // Minimal stub providers for unit tests within the crate.
    // The integration tests in tests/validation.rs use real crypto.

    struct StubSha256;
    impl Sha256Provider for StubSha256 {
        fn hash(&self, data: &[u8]) -> [u8; 32] {
            // Simple non-cryptographic hash for deterministic testing.
            let mut out = [0u8; 32];
            for (i, &b) in data.iter().enumerate() {
                out[i % 32] ^= b;
            }
            out
        }
    }

    struct StubAead;
    impl AeadProvider for StubAead {
        fn seal(
            &self,
            _key: &[u8; 32],
            nonce: &[u8; 12],
            _aad: &[u8],
            plaintext: &[u8],
        ) -> Vec<u8> {
            // XOR plaintext with nonce byte, append 16-byte fake tag.
            let mut ct = plaintext.to_vec();
            for b in &mut ct {
                *b ^= nonce[0];
            }
            ct.extend_from_slice(&[0xAA; AEAD_TAG_SIZE]);
            ct
        }

        fn open(
            &self,
            _key: &[u8; 32],
            nonce: &[u8; 12],
            _aad: &[u8],
            ciphertext_and_tag: &[u8],
        ) -> Option<Vec<u8>> {
            if ciphertext_and_tag.len() < AEAD_TAG_SIZE {
                return None;
            }
            let ct_len = ciphertext_and_tag.len() - AEAD_TAG_SIZE;
            let tag = &ciphertext_and_tag[ct_len..];
            // Test-only stub: constant-time comparison is required for
            // production AeadProvider implementations, but this stub uses
            // a simple equality check since it is not security-sensitive.
            if tag != [0xAA; AEAD_TAG_SIZE] {
                return None;
            }
            let mut pt = ciphertext_and_tag[..ct_len].to_vec();
            for b in &mut pt {
                *b ^= nonce[0];
            }
            Some(pt)
        }
    }

    #[test]
    fn nonce_construction_length() {
        let psk = [0x42u8; 32];
        let frame_nonce: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let nonce = build_gcm_nonce(&psk, MSG_WAKE, &frame_nonce, &StubSha256);
        assert_eq!(nonce.len(), 12);
        // msg_type at index 3
        assert_eq!(nonce[3], MSG_WAKE);
        // frame_nonce occupies bytes 4..12
        assert_eq!(&nonce[4..12], &frame_nonce);
    }

    #[test]
    fn encode_decode_round_trip() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 42,
        };
        let payload = vec![0xA1, 0x01, 0x02];
        let psk = [0x42u8; 32];

        let raw = encode_frame_aead(&hdr, &payload, &psk, &StubAead, &StubSha256).unwrap();
        let decoded = decode_frame_aead(&raw).unwrap();
        assert_eq!(decoded.header.key_hint, 1);
        assert_eq!(decoded.header.msg_type, MSG_WAKE);
        assert_eq!(decoded.header.nonce, 42);

        let plaintext = open_frame(&decoded, &psk, &StubAead, &StubSha256).unwrap();
        assert_eq!(plaintext, payload);
    }

    #[test]
    fn tampered_tag_fails() {
        let hdr = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 1,
        };
        let psk = [0x42u8; 32];
        let mut raw = encode_frame_aead(&hdr, &[0xA0], &psk, &StubAead, &StubSha256).unwrap();
        // Flip a bit in the tag (last byte).
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let decoded = decode_frame_aead(&raw).unwrap();
        let result = open_frame(&decoded, &psk, &StubAead, &StubSha256);
        assert_eq!(result, Err(DecodeError::AuthenticationFailed));
    }

    #[test]
    fn too_short_frame() {
        let short = vec![0u8; MIN_FRAME_SIZE_AEAD - 1];
        let err = decode_frame_aead(&short).unwrap_err();
        assert!(matches!(err, DecodeError::TooShort));
    }

    #[test]
    fn max_payload_fits() {
        let hdr = FrameHeader {
            key_hint: 0,
            msg_type: 0,
            nonce: 0,
        };
        let psk = [0x42u8; 32];
        let payload = vec![0u8; MAX_PAYLOAD_SIZE_AEAD]; // 223
        let raw = encode_frame_aead(&hdr, &payload, &psk, &StubAead, &StubSha256).unwrap();
        assert_eq!(raw.len(), MAX_FRAME_SIZE);
    }

    #[test]
    fn payload_too_large() {
        let hdr = FrameHeader {
            key_hint: 0,
            msg_type: 0,
            nonce: 0,
        };
        let psk = [0x42u8; 32];
        let big = vec![0u8; MAX_PAYLOAD_SIZE_AEAD + 1];
        let err = encode_frame_aead(&hdr, &big, &psk, &StubAead, &StubSha256).unwrap_err();
        assert!(matches!(err, EncodeError::FrameTooLarge));
    }

    #[test]
    fn empty_payload_round_trip() {
        let hdr = FrameHeader {
            key_hint: 0,
            msg_type: 0,
            nonce: 0,
        };
        let psk = [0x42u8; 32];
        let raw = encode_frame_aead(&hdr, &[], &psk, &StubAead, &StubSha256).unwrap();
        assert_eq!(raw.len(), HEADER_SIZE + AEAD_TAG_SIZE);
        assert_eq!(raw.len(), MIN_FRAME_SIZE_AEAD);

        let decoded = decode_frame_aead(&raw).unwrap();
        let plaintext = open_frame(&decoded, &psk, &StubAead, &StubSha256).unwrap();
        assert!(plaintext.is_empty());
    }
}
