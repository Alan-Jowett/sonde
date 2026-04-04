// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Encrypt with AES-256-GCM.
pub fn aes256gcm_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, PairingError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(nonce);

    let payload = aes_gcm::aead::Payload {
        msg: plaintext,
        aad,
    };

    cipher
        .encrypt(nonce, payload)
        .map_err(|e| PairingError::EncryptionFailed(e.to_string()))
}

/// Decrypt with AES-256-GCM.
pub fn aes256gcm_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, PairingError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(nonce);

    let payload = aes_gcm::aead::Payload {
        msg: ciphertext,
        aad,
    };

    let plaintext = cipher
        .decrypt(nonce, payload)
        .map_err(|_| PairingError::DecryptionFailed)?;

    Ok(Zeroizing::new(plaintext))
}

/// Compute SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// AAD string for the AES-256-GCM pairing request AEAD codec.
pub const PAIRING_REQUEST_AAD: &[u8] = b"sonde-pairing-v2";

/// GCM nonce length in bytes.
const GCM_NONCE_LEN: usize = 12;

/// GCM tag length in bytes.
const GCM_TAG_LEN: usize = 16;

/// [`sonde_protocol::Sha256Provider`] backed by `sha2::Sha256`.
struct PairSha256;

impl sonde_protocol::Sha256Provider for PairSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        sha256(data)
    }
}

/// [`sonde_protocol::AeadProvider`] backed by `aes_gcm::Aes256Gcm`.
struct PairAead;

impl sonde_protocol::AeadProvider for PairAead {
    fn seal(&self, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        aes256gcm_encrypt(key, nonce, plaintext, aad).expect("AES-256-GCM seal failed")
    }

    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        aad: &[u8],
        ciphertext_and_tag: &[u8],
    ) -> Option<Vec<u8>> {
        aes256gcm_decrypt(key, nonce, ciphertext_and_tag, aad)
            .ok()
            .map(|z| z.to_vec())
    }
}

/// Build a complete ESP-NOW PEER_REQUEST frame using AES-256-GCM.
///
/// 1. Encrypts `pairing_request_cbor` with `phone_psk` (inner layer,
///    AAD = `"sonde-pairing-v2"`).
/// 2. Wraps the inner ciphertext in CBOR `{1: encrypted_payload}`.
/// 3. Builds a complete ESP-NOW AEAD frame (header + outer-layer
///    AES-256-GCM encryption with `phone_psk`).
///
/// The returned blob is what the node stores and forwards verbatim.
pub fn encrypt_pairing_request(
    phone_psk: &[u8; 32],
    pairing_request_cbor: &[u8],
) -> Result<Vec<u8>, PairingError> {
    // Inner layer: encrypt PairingRequest CBOR with phone_psk.
    let mut inner_nonce = [0u8; GCM_NONCE_LEN];
    getrandom::fill(&mut inner_nonce).map_err(|e| PairingError::RngFailed(e.to_string()))?;

    let ciphertext_and_tag = aes256gcm_encrypt(
        phone_psk,
        &inner_nonce,
        pairing_request_cbor,
        PAIRING_REQUEST_AAD,
    )?;

    let mut encrypted_payload = Vec::with_capacity(GCM_NONCE_LEN + ciphertext_and_tag.len());
    encrypted_payload.extend_from_slice(&inner_nonce);
    encrypted_payload.extend_from_slice(&ciphertext_and_tag);

    // Wrap in CBOR: { 1: encrypted_payload }
    let cbor_map = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(sonde_protocol::PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut cbor_buf = Vec::new();
    ciborium::into_writer(&cbor_map, &mut cbor_buf)
        .map_err(|_| PairingError::EncryptionFailed("CBOR encode failed".into()))?;

    // Outer layer: build complete ESP-NOW AEAD frame.
    let sha = PairSha256;
    let aead = PairAead;
    let phone_key_hint = sonde_protocol::key_hint_from_psk(phone_psk, &sha);

    let mut frame_nonce_bytes = [0u8; 8];
    getrandom::fill(&mut frame_nonce_bytes).map_err(|e| PairingError::RngFailed(e.to_string()))?;
    let frame_nonce = u64::from_be_bytes(frame_nonce_bytes);

    let header = sonde_protocol::FrameHeader {
        key_hint: phone_key_hint,
        msg_type: sonde_protocol::MSG_PEER_REQUEST,
        nonce: frame_nonce,
    };

    sonde_protocol::encode_frame(&header, &cbor_buf, phone_psk, &aead, &sha)
        .map_err(|_| PairingError::EncryptionFailed("frame encode failed".into()))
}

/// Decrypt a pairing request using AES-256-GCM with the phone PSK.
///
/// Expects `nonce(12) ‖ ciphertext ‖ tag(16)`. Returns the plaintext
/// CBOR in a [`Zeroizing`] wrapper on success, or `None` if
/// authentication fails.
///
/// The AAD is fixed to `"sonde-pairing-v2"` for domain separation.
pub fn decrypt_pairing_request(
    phone_psk: &[u8; 32],
    encrypted_payload: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    // Minimum length: 12-byte nonce + 16-byte tag (ciphertext may be empty)
    if encrypted_payload.len() < GCM_NONCE_LEN + GCM_TAG_LEN {
        return None;
    }

    let nonce: &[u8; GCM_NONCE_LEN] = encrypted_payload[..GCM_NONCE_LEN].try_into().ok()?;
    let ciphertext_and_tag = &encrypted_payload[GCM_NONCE_LEN..];

    let plaintext =
        aes256gcm_decrypt(phone_psk, nonce, ciphertext_and_tag, PAIRING_REQUEST_AAD).ok()?;
    Some(plaintext)
}

/// Build a DIAG_REQUEST ESP-NOW frame authenticated with phone_psk.
///
/// Returns `(frame_bytes, nonce)`. The nonce is needed to verify the reply.
pub fn build_diag_request_frame(phone_psk: &[u8; 32]) -> Result<(Vec<u8>, u64), PairingError> {
    let sha = PairSha256;
    let aead = PairAead;

    let msg = sonde_protocol::NodeMessage::DiagRequest {
        diagnostic_type: sonde_protocol::DIAG_TYPE_RSSI,
    };
    let cbor = msg
        .encode()
        .map_err(|_| PairingError::EncryptionFailed("CBOR encode failed".into()))?;

    let mut frame_nonce_bytes = [0u8; 8];
    getrandom::fill(&mut frame_nonce_bytes).map_err(|e| PairingError::RngFailed(e.to_string()))?;
    let frame_nonce = u64::from_be_bytes(frame_nonce_bytes);

    let phone_key_hint = sonde_protocol::key_hint_from_psk(phone_psk, &sha);
    let header = sonde_protocol::FrameHeader {
        key_hint: phone_key_hint,
        msg_type: sonde_protocol::MSG_DIAG_REQUEST,
        nonce: frame_nonce,
    };

    let frame = sonde_protocol::encode_frame(&header, &cbor, phone_psk, &aead, &sha)
        .map_err(|_| PairingError::EncryptionFailed("frame encode failed".into()))?;

    Ok((frame, frame_nonce))
}

/// Decrypt a DIAG_REPLY ESP-NOW frame and extract the diagnostic result.
///
/// Verifies the reply nonce matches the request nonce.
pub fn decrypt_diag_reply(
    raw_frame: &[u8],
    phone_psk: &[u8; 32],
    expected_nonce: u64,
) -> Result<(i8, u8), PairingError> {
    let sha = PairSha256;
    let aead = PairAead;

    let decoded = sonde_protocol::decode_frame(raw_frame)
        .map_err(|_| PairingError::DiagnosticFailed("malformed DIAG_REPLY frame".into()))?;

    if decoded.header.nonce != expected_nonce {
        return Err(PairingError::DiagnosticFailed(
            "DIAG_REPLY nonce mismatch".into(),
        ));
    }

    let payload = sonde_protocol::open_frame(&decoded, phone_psk, &aead, &sha)
        .map_err(|_| PairingError::EncryptionFailed("DIAG_REPLY decryption failed".into()))?;

    let msg = sonde_protocol::GatewayMessage::decode(sonde_protocol::MSG_DIAG_REPLY, &payload)
        .map_err(|e| PairingError::DiagnosticFailed(format!("DIAG_REPLY CBOR decode: {}", e)))?;

    match msg {
        sonde_protocol::GatewayMessage::DiagReply {
            rssi_dbm,
            signal_quality,
            ..
        } => Ok((rssi_dbm, signal_quality)),
        _ => Err(PairingError::DiagnosticFailed(
            "unexpected message variant".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = sha256(b"");
        assert_eq!(
            hex::encode(hash),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn aes256gcm_round_trip() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; 12];
        let plaintext = b"hello pairing protocol";
        let aad = b"gateway-id";

        let ciphertext = aes256gcm_encrypt(&key, &nonce, plaintext, aad).unwrap();
        let decrypted = aes256gcm_decrypt(&key, &nonce, &ciphertext, aad).unwrap();
        assert_eq!(&*decrypted, plaintext);
    }

    #[test]
    fn aes256gcm_wrong_key_fails() {
        let key = [0x42u8; 32];
        let wrong_key = [0x43u8; 32];
        let nonce = [0x01u8; 12];
        let plaintext = b"secret";

        let ciphertext = aes256gcm_encrypt(&key, &nonce, plaintext, b"").unwrap();
        assert!(aes256gcm_decrypt(&wrong_key, &nonce, &ciphertext, b"").is_err());
    }

    #[test]
    fn aes256gcm_wrong_aad_fails() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; 12];
        let plaintext = b"secret";

        let ciphertext = aes256gcm_encrypt(&key, &nonce, plaintext, b"correct").unwrap();
        assert!(aes256gcm_decrypt(&key, &nonce, &ciphertext, b"wrong").is_err());
    }

    // Hex encoding helper for the SHA-256 test
    mod hex {
        pub fn encode(data: impl AsRef<[u8]>) -> String {
            data.as_ref().iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}

#[cfg(test)]
mod aead_tests {
    use super::*;

    /// Helper: open the outer ESP-NOW frame and extract the inner
    /// `encrypted_payload` from the CBOR `{1: bstr}`.
    fn open_frame_and_extract_inner(phone_psk: &[u8; 32], frame: &[u8]) -> Vec<u8> {
        let sha = PairSha256;
        let aead = PairAead;
        let decoded = sonde_protocol::decode_frame(frame).expect("decode_frame failed");
        let cbor_payload = sonde_protocol::open_frame(&decoded, phone_psk, &aead, &sha)
            .expect("open_frame failed");
        let cbor: ciborium::Value =
            ciborium::from_reader(&cbor_payload[..]).expect("CBOR parse failed");
        let map = cbor.as_map().expect("expected CBOR map");
        assert_eq!(map.len(), 1);
        let (k, v) = &map[0];
        assert_eq!(u64::try_from(k.as_integer().unwrap()).unwrap(), 1);
        v.as_bytes().expect("expected bytes").clone()
    }

    /// Round-trip: encrypt then decrypt must recover the original plaintext.
    #[test]
    fn pairing_request_round_trip() {
        let psk = [0x42u8; 32];
        let plaintext = b"pairing request CBOR data for node-42";

        let frame = encrypt_pairing_request(&psk, plaintext).unwrap();

        // Verify it's a valid AEAD frame with PEER_REQUEST msg_type.
        let decoded = sonde_protocol::decode_frame(&frame).unwrap();
        assert_eq!(decoded.header.msg_type, sonde_protocol::MSG_PEER_REQUEST);

        let inner = open_frame_and_extract_inner(&psk, &frame);
        let decrypted = decrypt_pairing_request(&psk, &inner);
        assert_eq!(
            decrypted.as_ref().map(|z| z.as_slice()),
            Some(plaintext.as_slice())
        );
    }

    /// Wrong PSK must fail outer-frame decryption.
    #[test]
    fn pairing_request_wrong_psk_fails() {
        let psk = [0x42u8; 32];
        let wrong_psk = [0x43u8; 32];
        let plaintext = b"secret pairing request";

        let frame = encrypt_pairing_request(&psk, plaintext).unwrap();

        let sha = PairSha256;
        let aead = PairAead;
        let decoded = sonde_protocol::decode_frame(&frame).unwrap();
        assert!(
            sonde_protocol::open_frame(&decoded, &wrong_psk, &aead, &sha).is_err(),
            "wrong PSK must fail outer-frame decryption"
        );
    }

    /// Tampered frame must fail authentication.
    #[test]
    fn pairing_request_tampered_payload_fails() {
        let psk = [0x42u8; 32];
        let plaintext = b"original pairing request";

        let mut frame = encrypt_pairing_request(&psk, plaintext).unwrap();
        // Flip a byte in the ciphertext (after the 11-byte header)
        let flip_idx = sonde_protocol::HEADER_SIZE + 1;
        frame[flip_idx] ^= 0xFF;

        let sha = PairSha256;
        let aead = PairAead;
        let decoded = sonde_protocol::decode_frame(&frame).unwrap();
        assert!(
            sonde_protocol::open_frame(&decoded, &psk, &aead, &sha).is_err(),
            "tampered ciphertext must fail authentication"
        );
    }

    /// AAD binding: decrypting the inner payload with a different AAD must fail.
    #[test]
    fn pairing_request_wrong_aad_fails() {
        let psk = [0x42u8; 32];
        let plaintext = b"aad-bound payload";

        let frame = encrypt_pairing_request(&psk, plaintext).unwrap();
        let inner = open_frame_and_extract_inner(&psk, &frame);

        // Extract nonce and ciphertext_and_tag from inner payload
        let nonce: [u8; 12] = inner[..12].try_into().unwrap();
        let ciphertext_and_tag = &inner[12..];

        // Decrypt with wrong AAD must fail
        assert!(
            aes256gcm_decrypt(&psk, &nonce, ciphertext_and_tag, b"wrong-aad").is_err(),
            "wrong AAD must fail decryption"
        );

        // Correct AAD must succeed
        assert!(
            aes256gcm_decrypt(&psk, &nonce, ciphertext_and_tag, PAIRING_REQUEST_AAD).is_ok(),
            "correct AAD must succeed"
        );
    }

    /// Payload too short (less than nonce + tag) must return None.
    #[test]
    fn pairing_request_short_payload_returns_none() {
        let psk = [0x42u8; 32];
        // 27 bytes < 12 nonce + 16 tag = 28 minimum
        let short = [0u8; 27];
        assert!(decrypt_pairing_request(&psk, &short).is_none());
    }

    /// Empty plaintext round-trip: encrypt then decrypt an empty buffer.
    #[test]
    fn pairing_request_empty_plaintext_round_trip() {
        let psk = [0x42u8; 32];
        let frame = encrypt_pairing_request(&psk, b"").unwrap();

        // Verify it's a valid frame
        let decoded = sonde_protocol::decode_frame(&frame).unwrap();
        assert_eq!(decoded.header.msg_type, sonde_protocol::MSG_PEER_REQUEST);

        let inner = open_frame_and_extract_inner(&psk, &frame);
        // inner = nonce(12) + tag(16) = 28 bytes, no ciphertext
        assert_eq!(inner.len(), GCM_NONCE_LEN + GCM_TAG_LEN);
        let decrypted = decrypt_pairing_request(&psk, &inner);
        assert_eq!(
            decrypted.as_ref().map(|z| z.as_slice()),
            Some([].as_slice())
        );
    }

    /// Empty payload must return None.
    #[test]
    fn pairing_request_empty_payload_returns_none() {
        let psk = [0x42u8; 32];
        assert!(decrypt_pairing_request(&psk, &[]).is_none());
    }
}
