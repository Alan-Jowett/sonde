// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Provides HMAC-SHA256 computation and verification.
/// Implementations MUST use constant-time comparison in `verify`
/// to prevent timing side-channel attacks.
pub trait HmacProvider {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32];
    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool;
}

/// Provides SHA-256 hashing. Used for program image hashing and GCM nonce derivation.
pub trait Sha256Provider {
    fn hash(&self, data: &[u8]) -> [u8; 32];
}

/// Provides AES-256-GCM authenticated encryption.
///
/// Key parameters are `&[u8; 32]` to enforce the AES-256 key-size
/// requirement at compile time.
///
/// Implementations MUST use the GCM tag verification built into the AEAD
/// primitive (constant-time). Manual tag comparison with `==` or `PartialEq`
/// is NOT acceptable.
#[cfg(feature = "aes-gcm-codec")]
pub trait AeadProvider {
    /// Encrypt `plaintext` with AES-256-GCM.
    ///
    /// Returns `ciphertext ‖ 16-byte tag`.
    /// `nonce` is 12 bytes; `aad` is the additional authenticated data.
    fn seal(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        aad: &[u8],
        plaintext: &[u8],
    ) -> alloc::vec::Vec<u8>;

    /// Decrypt `ciphertext_and_tag` with AES-256-GCM.
    ///
    /// Returns the plaintext on success, or `None` if the tag check fails.
    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        aad: &[u8],
        ciphertext_and_tag: &[u8],
    ) -> Option<alloc::vec::Vec<u8>>;
}
