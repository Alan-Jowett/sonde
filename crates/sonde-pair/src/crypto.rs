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

    // Hex encoding helper for the SHA-256 test
    mod hex {
        pub fn encode(data: impl AsRef<[u8]>) -> String {
            data.as_ref().iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
