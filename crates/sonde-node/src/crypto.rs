// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Software HMAC-SHA256 and SHA-256 providers, plus ESP32 hardware RNG.
//!
//! The software crypto implementations use the `hmac`/`sha2` crates and
//! work on every platform (host tests, CI, ESP32). Hardware-accelerated
//! crypto can be layered on top later as an optimisation.
//!
//! [`EspRng`] is only available with the `esp` feature and uses the
//! ESP32 hardware true-RNG via `esp_random()`.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sonde_protocol::{HmacProvider, Sha256Provider};

/// HMAC-SHA256 provider backed by the `hmac` + `sha2` RustCrypto crates.
pub struct SoftwareHmac;

impl HmacProvider for SoftwareHmac {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        // Constant-time comparison via the `Mac` trait.
        mac.verify_slice(expected).is_ok()
    }
}

/// SHA-256 provider backed by the `sha2` RustCrypto crate.
pub struct SoftwareSha256;

impl Sha256Provider for SoftwareSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}

/// Hardware true-RNG on ESP32 via `esp_random()`.
#[cfg(feature = "esp")]
pub struct EspRng;

#[cfg(feature = "esp")]
impl crate::traits::Rng for EspRng {
    fn random_u64(&mut self) -> u64 {
        let hi = unsafe { esp_idf_sys::esp_random() } as u64;
        let lo = unsafe { esp_idf_sys::esp_random() } as u64;
        (hi << 32) | lo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_compute_and_verify() {
        let hmac = SoftwareHmac;
        let key = b"test-key";
        let data = b"hello world";
        let tag = hmac.compute(key, data);
        assert!(hmac.verify(key, data, &tag));
    }

    #[test]
    fn hmac_verify_rejects_tampered() {
        let hmac = SoftwareHmac;
        let key = b"test-key";
        let tag = hmac.compute(key, b"original");
        assert!(!hmac.verify(key, b"tampered", &tag));
    }

    #[test]
    fn sha256_known_vector() {
        let sha = SoftwareSha256;
        // SHA-256 of the empty string.
        let hash = sha.hash(b"");
        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(hash, expected);
    }
}
