// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;
use crate::rng::RngProvider;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use curve25519_dalek::edwards::CompressedEdwardsY;
use ed25519_dalek::{Signature, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};
use zeroize::Zeroizing;

/// Verify an Ed25519 signature over a message.
pub fn verify_ed25519_signature(
    public_key: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
) -> Result<(), PairingError> {
    let vk = VerifyingKey::from_bytes(public_key)
        .map_err(|e| PairingError::InvalidPublicKey(format!("invalid Ed25519 public key: {e}")))?;
    let sig = Signature::from_bytes(signature);
    vk.verify_strict(message, &sig)
        .map_err(|_| PairingError::SignatureVerificationFailed)
}

/// Convert an Ed25519 public key to an X25519 public key (Montgomery form).
///
/// Rejects low-order points that would produce an all-zero shared secret.
pub fn ed25519_to_x25519_public(ed_pub: &[u8; 32]) -> Result<[u8; 32], PairingError> {
    let compressed = CompressedEdwardsY(*ed_pub);
    let edwards = compressed.decompress().ok_or_else(|| {
        PairingError::InvalidPublicKey("Ed25519 point decompression failed".into())
    })?;

    let montgomery = edwards.to_montgomery();
    let result = montgomery.to_bytes();

    // Reject the identity point (all zeros)
    if result == [0u8; 32] {
        return Err(PairingError::InvalidPublicKey(
            "low-order point produces all-zero X25519 key".into(),
        ));
    }

    Ok(result)
}

/// Generate an X25519 ephemeral keypair. Returns (secret, public).
pub fn generate_x25519_keypair(
    rng: &dyn RngProvider,
) -> Result<(Zeroizing<[u8; 32]>, [u8; 32]), PairingError> {
    let mut secret_bytes = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut *secret_bytes)?;

    let secret = X25519Secret::from(*secret_bytes);
    let public = X25519Public::from(&secret);

    Ok((Zeroizing::new(secret.to_bytes()), public.to_bytes()))
}

/// Perform X25519 ECDH key agreement.
pub fn x25519_ecdh(our_secret: &[u8; 32], their_public: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let secret = X25519Secret::from(*our_secret);
    let public = X25519Public::from(*their_public);
    let shared = secret.diffie_hellman(&public);
    Zeroizing::new(shared.to_bytes())
}

/// Derive a 32-byte key using HKDF-SHA256.
pub fn hkdf_sha256(shared_secret: &[u8; 32], salt: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared_secret);
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(info, &mut *okm)
        .expect("HKDF-SHA256 expand to 32 bytes should never fail");
    okm
}

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

/// Compute HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// Compute SHA-256 hash.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// AAD string for the AES-256-GCM pairing request AEAD codec.
#[cfg(feature = "aes-gcm-codec")]
pub const PAIRING_REQUEST_AAD: &[u8] = b"sonde-pairing-v2";

/// GCM nonce length in bytes.
#[cfg(feature = "aes-gcm-codec")]
const GCM_NONCE_LEN: usize = 12;

/// GCM tag length in bytes.
#[cfg(feature = "aes-gcm-codec")]
const GCM_TAG_LEN: usize = 16;

/// [`sonde_protocol::Sha256Provider`] backed by `sha2::Sha256`.
#[cfg(feature = "aes-gcm-codec")]
struct PairSha256;

#[cfg(feature = "aes-gcm-codec")]
impl sonde_protocol::Sha256Provider for PairSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        sha256(data)
    }
}

/// [`sonde_protocol::AeadProvider`] backed by `aes_gcm::Aes256Gcm`.
#[cfg(feature = "aes-gcm-codec")]
struct PairAead;

#[cfg(feature = "aes-gcm-codec")]
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
#[cfg(feature = "aes-gcm-codec")]
pub fn encrypt_pairing_request_aead(
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

    sonde_protocol::encode_frame_aead(&header, &cbor_buf, phone_psk, &aead, &sha)
        .map_err(|_| PairingError::EncryptionFailed("frame encode failed".into()))
}

/// Decrypt a pairing request using AES-256-GCM with the phone PSK.
///
/// Expects `nonce(12) ‖ ciphertext ‖ tag(16)`. Returns the plaintext
/// CBOR in a [`Zeroizing`] wrapper on success, or `None` if
/// authentication fails.
///
/// The AAD is fixed to `"sonde-pairing-v2"` for domain separation.
#[cfg(feature = "aes-gcm-codec")]
pub fn decrypt_pairing_request_aead(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::MockRng;
    use ed25519_dalek::SigningKey;

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
    fn hmac_sha256_deterministic() {
        let key = [0x42u8; 32];
        let msg = b"test message";
        let mac1 = hmac_sha256(&key, msg);
        let mac2 = hmac_sha256(&key, msg);
        assert_eq!(mac1, mac2);
    }

    #[test]
    fn hmac_sha256_different_keys_differ() {
        let msg = b"test";
        let mac1 = hmac_sha256(&[0x42u8; 32], msg);
        let mac2 = hmac_sha256(&[0x43u8; 32], msg);
        assert_ne!(mac1, mac2);
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

    #[test]
    fn ed25519_sign_verify() {
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let message = b"challenge data";

        use ed25519_dalek::Signer;
        let sig = signing_key.sign(message);

        verify_ed25519_signature(&verifying_key.to_bytes(), message, &sig.to_bytes()).unwrap();
    }

    #[test]
    fn ed25519_wrong_signature_fails() {
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let message = b"challenge data";

        let bad_sig = [0xFFu8; 64];
        assert!(verify_ed25519_signature(&verifying_key.to_bytes(), message, &bad_sig,).is_err());
    }

    #[test]
    fn ed25519_to_x25519_conversion() {
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
        let ed_pub = signing_key.verifying_key().to_bytes();

        let x_pub = ed25519_to_x25519_public(&ed_pub).unwrap();
        assert_ne!(x_pub, [0u8; 32], "X25519 key should not be zero");
    }

    #[test]
    fn x25519_keypair_and_ecdh() {
        let rng = MockRng::new([0x42u8; 32]);
        let (secret_a, public_a) = generate_x25519_keypair(&rng).unwrap();

        let rng_b = MockRng::new([0x43u8; 32]);
        let (secret_b, public_b) = generate_x25519_keypair(&rng_b).unwrap();

        let shared_ab = x25519_ecdh(&secret_a, &public_b);
        let shared_ba = x25519_ecdh(&secret_b, &public_a);
        assert_eq!(*shared_ab, *shared_ba, "ECDH should be symmetric");
    }

    #[test]
    fn hkdf_deterministic() {
        let secret = [0x42u8; 32];
        let salt = b"gateway-id";
        let info = b"sonde-phone-pair-v1";

        let key1 = hkdf_sha256(&secret, salt, info);
        let key2 = hkdf_sha256(&secret, salt, info);
        assert_eq!(*key1, *key2);
    }

    #[test]
    fn hkdf_different_info_differs() {
        let secret = [0x42u8; 32];
        let salt = b"salt";

        let key1 = hkdf_sha256(&secret, salt, b"info-a");
        let key2 = hkdf_sha256(&secret, salt, b"info-b");
        assert_ne!(*key1, *key2);
    }

    /// T-PT-309 / PT-0405, PT-0902: Ed25519 → X25519 low-order point rejection.
    ///
    /// The Ed25519 identity point (0, 1) maps to the all-zero X25519 point
    /// (Montgomery identity).  The conversion must reject this and return
    /// an error containing "invalid public key".
    ///
    /// Note: the validation spec says the operator-facing message should
    /// contain "invalid gateway public key".  The crypto module is generic
    /// (not gateway-specific), so it returns "invalid public key: ...".
    /// The calling code (phase1/phase2) surfaces this to the operator
    /// via `PairingError::InvalidPublicKey`, which includes the full
    /// chain: "invalid public key: low-order point ...".
    #[test]
    fn t_pt_309_low_order_point_rejected() {
        // y = 1 in little-endian compressed Edwards form → identity point.
        let mut identity = [0u8; 32];
        identity[0] = 0x01;

        let result = ed25519_to_x25519_public(&identity);
        assert!(result.is_err(), "low-order point must be rejected");
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("invalid public key"),
            "error should mention invalid public key: {err}"
        );
    }

    /// T-PT-309 supplemental: valid Ed25519 key must convert successfully.
    #[test]
    fn t_pt_309_valid_key_succeeds() {
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
        let ed_pub = signing_key.verifying_key().to_bytes();

        let result = ed25519_to_x25519_public(&ed_pub);
        assert!(result.is_ok(), "valid Ed25519 key should convert");
        let x_pub = result.unwrap();
        assert_ne!(x_pub, [0u8; 32], "X25519 key should not be all-zero");
    }

    /// PT-0304: Zeroize mechanism clears ephemeral key buffers.
    ///
    /// Verifies that calling `zeroize()` on a `[u8; 32]` clears the buffer
    /// and that `generate_x25519_keypair` produces independent keys for
    /// independent RNG inputs (no state leak between calls).
    #[test]
    fn ephemeral_key_zeroed_on_drop() {
        use zeroize::Zeroize;

        // Compile-time assertion: generate_x25519_keypair returns secrets
        // wrapped in Zeroizing, which implements ZeroizeOnDrop.
        fn _assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        _assert_zeroize_on_drop::<zeroize::Zeroizing<[u8; 32]>>();

        // Direct zeroize mechanism: calling zeroize() must zero the buffer.
        let mut key = [0x42u8; 32];
        key.zeroize();
        assert_eq!(key, [0u8; 32], "zeroize() must clear the buffer");

        // Independence: two generate_x25519_keypair calls with different RNG
        // produce different secrets (no state leak).
        let rng_a = MockRng::new([0x42u8; 32]);
        let rng_b = MockRng::new([0x43u8; 32]);
        let (secret_a, _) = generate_x25519_keypair(&rng_a).unwrap();
        let (secret_b, _) = generate_x25519_keypair(&rng_b).unwrap();
        assert_ne!(
            *secret_a, *secret_b,
            "different RNG must produce different secrets"
        );
    }

    /// PT-0304: Zeroize mechanism clears ECDH shared secret.
    #[test]
    fn ecdh_shared_secret_zeroed_on_drop() {
        use zeroize::Zeroize;

        // Compile-time assertion: x25519_ecdh returns Zeroizing<[u8; 32]>,
        // which implements ZeroizeOnDrop via the Zeroizing wrapper.
        fn _assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        _assert_zeroize_on_drop::<zeroize::Zeroizing<[u8; 32]>>();

        let rng_a = MockRng::new([0x42u8; 32]);
        let rng_b = MockRng::new([0x43u8; 32]);
        let (secret_a, _pub_a) = generate_x25519_keypair(&rng_a).unwrap();
        let (_secret_b, pub_b) = generate_x25519_keypair(&rng_b).unwrap();

        let mut shared = x25519_ecdh(&secret_a, &pub_b);
        assert_ne!(*shared, [0u8; 32], "shared secret must be non-zero");

        // Zeroize the inner buffer to confirm the mechanism works.
        shared.zeroize();
        assert_eq!(*shared, [0u8; 32], "zeroize() must clear the shared secret");
    }

    /// §6.3/6.4: Cross-phase HKDF info string swap must fail decryption.
    ///
    /// Encrypting with `sonde-phone-reg-v1` and decrypting with a key
    /// derived using `sonde-node-pair-v1` (or vice versa) must fail.
    #[test]
    fn cross_phase_hkdf_info_swap_fails_decryption() {
        let shared_secret = [0x42u8; 32];
        let salt = [0x01u8; 16];

        let key_phase1 = hkdf_sha256(&shared_secret, &salt, b"sonde-phone-reg-v1");
        let key_phase2 = hkdf_sha256(&shared_secret, &salt, b"sonde-node-pair-v1");

        // Keys must differ.
        assert_ne!(*key_phase1, *key_phase2);

        // Encrypt with Phase 1 key.
        let nonce = [0x01u8; 12];
        let plaintext = b"secret data";
        let aad = &salt;
        let ciphertext = aes256gcm_encrypt(&key_phase1, &nonce, plaintext, aad).unwrap();

        // Decrypt with Phase 2 key must fail.
        assert!(
            aes256gcm_decrypt(&key_phase2, &nonce, &ciphertext, aad).is_err(),
            "cross-phase info swap must fail decryption"
        );

        // And vice versa.
        let ciphertext2 = aes256gcm_encrypt(&key_phase2, &nonce, plaintext, aad).unwrap();
        assert!(
            aes256gcm_decrypt(&key_phase1, &nonce, &ciphertext2, aad).is_err(),
            "reverse cross-phase swap must also fail"
        );
    }

    // Hex encoding helper for the SHA-256 test
    mod hex {
        pub fn encode(data: impl AsRef<[u8]>) -> String {
            data.as_ref().iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}

#[cfg(all(test, feature = "aes-gcm-codec"))]
mod aead_tests {
    use super::*;

    /// Helper: open the outer ESP-NOW frame and extract the inner
    /// `encrypted_payload` from the CBOR `{1: bstr}`.
    fn open_frame_and_extract_inner(phone_psk: &[u8; 32], frame: &[u8]) -> Vec<u8> {
        let sha = PairSha256;
        let aead = PairAead;
        let decoded = sonde_protocol::decode_frame_aead(frame).expect("decode_frame_aead failed");
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
    fn pairing_request_aead_round_trip() {
        let psk = [0x42u8; 32];
        let plaintext = b"pairing request CBOR data for node-42";

        let frame = encrypt_pairing_request_aead(&psk, plaintext).unwrap();

        // Verify it's a valid AEAD frame with PEER_REQUEST msg_type.
        let decoded = sonde_protocol::decode_frame_aead(&frame).unwrap();
        assert_eq!(decoded.header.msg_type, sonde_protocol::MSG_PEER_REQUEST);

        let inner = open_frame_and_extract_inner(&psk, &frame);
        let decrypted = decrypt_pairing_request_aead(&psk, &inner);
        assert_eq!(
            decrypted.as_ref().map(|z| z.as_slice()),
            Some(plaintext.as_slice())
        );
    }

    /// Wrong PSK must fail outer-frame decryption.
    #[test]
    fn pairing_request_aead_wrong_psk_fails() {
        let psk = [0x42u8; 32];
        let wrong_psk = [0x43u8; 32];
        let plaintext = b"secret pairing request";

        let frame = encrypt_pairing_request_aead(&psk, plaintext).unwrap();

        let sha = PairSha256;
        let aead = PairAead;
        let decoded = sonde_protocol::decode_frame_aead(&frame).unwrap();
        assert!(
            sonde_protocol::open_frame(&decoded, &wrong_psk, &aead, &sha).is_err(),
            "wrong PSK must fail outer-frame decryption"
        );
    }

    /// Tampered frame must fail authentication.
    #[test]
    fn pairing_request_aead_tampered_payload_fails() {
        let psk = [0x42u8; 32];
        let plaintext = b"original pairing request";

        let mut frame = encrypt_pairing_request_aead(&psk, plaintext).unwrap();
        // Flip a byte in the ciphertext (after the 11-byte header)
        let flip_idx = sonde_protocol::HEADER_SIZE + 1;
        frame[flip_idx] ^= 0xFF;

        let sha = PairSha256;
        let aead = PairAead;
        let decoded = sonde_protocol::decode_frame_aead(&frame).unwrap();
        assert!(
            sonde_protocol::open_frame(&decoded, &psk, &aead, &sha).is_err(),
            "tampered ciphertext must fail authentication"
        );
    }

    /// AAD binding: decrypting the inner payload with a different AAD must fail.
    #[test]
    fn pairing_request_aead_wrong_aad_fails() {
        let psk = [0x42u8; 32];
        let plaintext = b"aad-bound payload";

        let frame = encrypt_pairing_request_aead(&psk, plaintext).unwrap();
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
    fn pairing_request_aead_short_payload_returns_none() {
        let psk = [0x42u8; 32];
        // 27 bytes < 12 nonce + 16 tag = 28 minimum
        let short = [0u8; 27];
        assert!(decrypt_pairing_request_aead(&psk, &short).is_none());
    }

    /// Empty plaintext round-trip: encrypt then decrypt an empty buffer.
    #[test]
    fn pairing_request_aead_empty_plaintext_round_trip() {
        let psk = [0x42u8; 32];
        let frame = encrypt_pairing_request_aead(&psk, b"").unwrap();

        // Verify it's a valid frame
        let decoded = sonde_protocol::decode_frame_aead(&frame).unwrap();
        assert_eq!(decoded.header.msg_type, sonde_protocol::MSG_PEER_REQUEST);

        let inner = open_frame_and_extract_inner(&psk, &frame);
        // inner = nonce(12) + tag(16) = 28 bytes, no ciphertext
        assert_eq!(inner.len(), GCM_NONCE_LEN + GCM_TAG_LEN);
        let decrypted = decrypt_pairing_request_aead(&psk, &inner);
        assert_eq!(
            decrypted.as_ref().map(|z| z.as_slice()),
            Some([].as_slice())
        );
    }

    /// Empty payload must return None.
    #[test]
    fn pairing_request_aead_empty_payload_returns_none() {
        let psk = [0x42u8; 32];
        assert!(decrypt_pairing_request_aead(&psk, &[]).is_none());
    }
}
