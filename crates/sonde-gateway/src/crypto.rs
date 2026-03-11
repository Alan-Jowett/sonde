// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sonde_protocol::{HmacProvider, Sha256Provider};

/// HMAC-SHA256 provider using the `hmac` + `sha2` RustCrypto crates.
pub struct RustCryptoHmac;

impl HmacProvider for RustCryptoHmac {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        // constant-time comparison via the `Mac` trait
        mac.verify_slice(expected).is_ok()
    }
}

/// SHA-256 provider using the `sha2` RustCrypto crate.
pub struct RustCryptoSha256;

impl Sha256Provider for RustCryptoSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}
