// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::PairingError;

/// Abstraction over random number generation for testability.
pub trait RngProvider {
    fn fill_bytes(&self, dest: &mut [u8]) -> Result<(), PairingError>;
}

/// Production RNG backed by the OS CSPRNG via `getrandom`.
pub struct OsRng;

impl RngProvider for OsRng {
    fn fill_bytes(&self, dest: &mut [u8]) -> Result<(), PairingError> {
        getrandom::fill(dest).map_err(|e| PairingError::RngFailed(e.to_string()))
    }
}

/// Deterministic RNG for tests. Fills output by repeating the seed.
pub struct MockRng {
    pub seed: [u8; 32],
}

impl MockRng {
    pub fn new(seed: [u8; 32]) -> Self {
        Self { seed }
    }
}

impl RngProvider for MockRng {
    fn fill_bytes(&self, dest: &mut [u8]) -> Result<(), PairingError> {
        for (i, byte) in dest.iter_mut().enumerate() {
            *byte = self.seed[i % self.seed.len()];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_rng_fills_nonzero() {
        let rng = OsRng;
        let mut buf = [0u8; 32];
        rng.fill_bytes(&mut buf).unwrap();
        // Extremely unlikely all 32 bytes are zero from a CSPRNG
        assert_ne!(buf, [0u8; 32]);
    }

    #[test]
    fn mock_rng_is_deterministic() {
        let rng = MockRng::new([0x42u8; 32]);
        let mut buf1 = [0u8; 64];
        let mut buf2 = [0u8; 64];
        rng.fill_bytes(&mut buf1).unwrap();
        rng.fill_bytes(&mut buf2).unwrap();
        assert_eq!(buf1, buf2);
        assert!(buf1.iter().all(|&b| b == 0x42));
    }
}
