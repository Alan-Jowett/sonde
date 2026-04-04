// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use sha2::{Digest, Sha256};
use sonde_protocol::Sha256Provider;

/// SHA-256 provider using the `sha2` RustCrypto crate.
pub struct RustCryptoSha256;

impl Sha256Provider for RustCryptoSha256 {
    fn hash(&self, data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}
