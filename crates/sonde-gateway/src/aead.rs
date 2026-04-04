// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! AES-256-GCM `AeadProvider` implementation for the gateway.
//!
//! Uses the `aes-gcm` RustCrypto crate.

pub use inner::GatewayAead;

mod inner {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use sonde_protocol::AeadProvider;

    /// AES-256-GCM provider backed by the `aes-gcm` crate.
    pub struct GatewayAead;

    impl AeadProvider for GatewayAead {
        fn seal(&self, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
            let cipher = Aes256Gcm::new(key.into());
            let gcm_nonce = Nonce::from_slice(nonce);
            cipher
                .encrypt(
                    gcm_nonce,
                    Payload {
                        msg: plaintext,
                        aad,
                    },
                )
                .expect("AES-256-GCM encryption should not fail")
        }

        fn open(
            &self,
            key: &[u8; 32],
            nonce: &[u8; 12],
            aad: &[u8],
            ciphertext_and_tag: &[u8],
        ) -> Option<Vec<u8>> {
            let cipher = Aes256Gcm::new(key.into());
            let gcm_nonce = Nonce::from_slice(nonce);
            cipher
                .decrypt(
                    gcm_nonce,
                    Payload {
                        msg: ciphertext_and_tag,
                        aad,
                    },
                )
                .ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GatewayAead;
    use sonde_protocol::{
        decode_frame, encode_frame, open_frame, AeadProvider, FrameHeader, MSG_WAKE,
    };

    use crate::crypto::RustCryptoSha256;

    #[test]
    fn round_trip_encode_decode() {
        let aead = GatewayAead;
        let sha = RustCryptoSha256;
        let psk = [0x42u8; 32];

        let header = FrameHeader {
            key_hint: 1,
            msg_type: MSG_WAKE,
            nonce: 100,
        };
        let payload = vec![0xA1, 0x01, 0x02];

        let raw =
            encode_frame(&header, &payload, &psk, &aead, &sha).expect("encoding should succeed");

        let decoded = decode_frame(&raw).expect("decoding should succeed");
        assert_eq!(decoded.header.key_hint, 1);
        assert_eq!(decoded.header.msg_type, MSG_WAKE);
        assert_eq!(decoded.header.nonce, 100);

        let plaintext = open_frame(&decoded, &psk, &aead, &sha).expect("open should succeed");
        assert_eq!(plaintext, payload);
    }

    #[test]
    fn wrong_key_authentication_failure() {
        let aead = GatewayAead;
        let sha = RustCryptoSha256;
        let psk = [0x42u8; 32];
        let wrong_psk = [0x99u8; 32];

        let header = FrameHeader {
            key_hint: 2,
            msg_type: MSG_WAKE,
            nonce: 200,
        };
        let payload = vec![0xA0];

        let raw =
            encode_frame(&header, &payload, &psk, &aead, &sha).expect("encoding should succeed");

        let decoded = decode_frame(&raw).expect("decoding should succeed");

        // Attempting to open with a different PSK must fail.
        let result = open_frame(&decoded, &wrong_psk, &aead, &sha);
        assert!(
            result.is_err(),
            "wrong key must produce authentication failure"
        );
    }

    #[test]
    fn seal_open_direct() {
        let aead = GatewayAead;
        let key = [0x55u8; 32];
        let nonce = [1u8; 12];
        let aad = b"header";
        let plaintext = b"hello world";

        let ct = aead.seal(&key, &nonce, aad, plaintext);
        let pt = aead
            .open(&key, &nonce, aad, &ct)
            .expect("open should succeed");
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let aead = GatewayAead;
        let key = [0x55u8; 32];
        let nonce = [2u8; 12];
        let aad = b"header";

        let mut ct = aead.seal(&key, &nonce, aad, b"data");
        // Flip a bit in the ciphertext.
        ct[0] ^= 0x01;
        assert!(aead.open(&key, &nonce, aad, &ct).is_none());
    }

    #[test]
    fn empty_payload_round_trip() {
        let aead = GatewayAead;
        let sha = RustCryptoSha256;
        let psk = [0x42u8; 32];

        let header = FrameHeader {
            key_hint: 0,
            msg_type: 0,
            nonce: 0,
        };

        let raw = encode_frame(&header, &[], &psk, &aead, &sha)
            .expect("encoding empty payload should succeed");

        let decoded = decode_frame(&raw).expect("decoding should succeed");
        let plaintext = open_frame(&decoded, &psk, &aead, &sha).expect("open should succeed");
        assert!(plaintext.is_empty());
    }
}
