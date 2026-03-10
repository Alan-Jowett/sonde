/// Provides HMAC-SHA256 computation and verification.
/// Implementations MUST use constant-time comparison in `verify`
/// to prevent timing side-channel attacks.
pub trait HmacProvider {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32];
    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool;
}

/// Provides SHA-256 hashing. Used for program image hashing.
pub trait Sha256Provider {
    fn hash(&self, data: &[u8]) -> [u8; 32];
}
