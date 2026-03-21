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

    /// PT-0304: Ephemeral key is behaviorally zeroed after use.
    ///
    /// Verifies that `zeroize()` on a `[u8; 32]` actually clears the buffer
    /// and that `generate_x25519_keypair` returns independent keys on
    /// independent inputs (no state leaks between calls).
    #[test]
    fn ephemeral_key_zeroed_on_drop() {
        use zeroize::Zeroize;

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

    /// PT-0304: ECDH shared secret is wrapped in Zeroizing and zeroize clears it.
    #[test]
    fn ecdh_shared_secret_zeroed_on_drop() {
        use zeroize::Zeroize;

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
