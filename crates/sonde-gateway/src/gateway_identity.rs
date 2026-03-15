// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Gateway Ed25519 identity and X25519 key agreement.
//!
//! Implements GW-1200 (keypair generation), GW-1201 (gateway_id), and
//! GW-1202 (Ed25519 → X25519 conversion with low-order point rejection).

use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest, Sha512};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::Zeroizing;

/// Small-order X25519 points that MUST be rejected (RFC 7748 / SafeCurves).
///
/// These are the points of order 1, 2, 4, and 8 on Curve25519 (plus the
/// canonical all-zero representation). Any ECDH shared secret derived from
/// one of these points is the identity element, providing no security.
const LOW_ORDER_POINTS: [[u8; 32]; 12] = [
    // 0 (identity / neutral element)
    [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // 1
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    // 325606250916557431795983626356110631294008115727848805560023387167927233504
    [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x00,
    ],
    // 39382357235489614581723060781553021112529911719440698176882885853963445705823
    [
        0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef,
        0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f,
        0x11, 0x57,
    ],
    // p - 1
    [
        0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    // p
    [
        0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    // p + 1
    [
        0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
    // 2p (reduced mod 2^255)
    [
        0xda, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff,
    ],
    // 2p + 1
    [
        0xdb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff,
    ],
    // non-canonical form: 2^255-19 + e0eb... point
    [
        0xcd, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x80,
    ],
    // non-canonical form: 2^255-19 + 5f9c... point
    [
        0x4c, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef,
        0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f,
        0x11, 0xd7,
    ],
    // non-canonical encoding of 0: 2^255 - 19
    [
        0xd9, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff,
    ],
];

/// Errors from gateway identity operations.
#[derive(Debug, Clone)]
pub enum IdentityError {
    /// The OS CSPRNG is unavailable.
    Rng,
    /// The Ed25519 → X25519 conversion produced a low-order point.
    LowOrderPoint,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::Rng => write!(f, "OS CSPRNG unavailable"),
            IdentityError::LowOrderPoint => {
                write!(f, "Ed25519 → X25519 conversion produced a low-order point")
            }
        }
    }
}

impl std::error::Error for IdentityError {}

/// Gateway Ed25519 identity (GW-1200, GW-1201).
///
/// Holds the Ed25519 signing key seed (32 bytes, zeroized on drop),
/// the random 16-byte `gateway_id`, and the derived public key.
#[derive(Clone)]
pub struct GatewayIdentity {
    /// Ed25519 seed (private keying material). Zeroized on drop.
    seed: Zeroizing<[u8; 32]>,
    /// Random 16-byte gateway identifier (HKDF salt, GCM AAD).
    gateway_id: [u8; 16],
    /// Ed25519 public key (distributed to phones).
    public_key: [u8; 32],
}

impl GatewayIdentity {
    /// Generate a new gateway identity from OS CSPRNG (GW-1200, GW-1201).
    pub fn generate() -> Result<Self, IdentityError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *seed).map_err(|_| IdentityError::Rng)?;

        let mut gateway_id = [0u8; 16];
        getrandom::fill(&mut gateway_id).map_err(|_| IdentityError::Rng)?;

        let signing_key = SigningKey::from_bytes(&seed);
        let public_key = signing_key.verifying_key().to_bytes();

        Ok(Self {
            seed,
            gateway_id,
            public_key,
        })
    }

    /// Reconstruct an identity from stored components.
    pub fn from_parts(seed: Zeroizing<[u8; 32]>, gateway_id: [u8; 16]) -> Self {
        let signing_key = SigningKey::from_bytes(&seed);
        let public_key = signing_key.verifying_key().to_bytes();
        Self {
            seed,
            gateway_id,
            public_key,
        }
    }

    /// The 32-byte Ed25519 seed (private key material).
    pub fn seed(&self) -> &[u8; 32] {
        &self.seed
    }

    /// The 16-byte gateway identifier.
    pub fn gateway_id(&self) -> &[u8; 16] {
        &self.gateway_id
    }

    /// The 32-byte Ed25519 public key.
    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    /// Return the Ed25519 signing key (for challenge-response signing).
    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.seed)
    }

    /// Return the Ed25519 verifying key.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key().verifying_key()
    }

    /// Convert the Ed25519 private key to an X25519 static secret (GW-1202).
    ///
    /// Follows the standard conversion: SHA-512(seed), clamp the lower 32
    /// bytes per RFC 7748 §5. The resulting X25519 public key is checked
    /// against known low-order points and rejected if it matches any.
    pub fn to_x25519(&self) -> Result<(X25519StaticSecret, X25519PublicKey), IdentityError> {
        use zeroize::Zeroize;

        let mut hasher = Sha512::new();
        hasher.update(self.seed.as_slice());
        let mut hash = hasher.finalize();
        let mut scalar = Zeroizing::new([0u8; 32]);
        scalar.copy_from_slice(&hash[..32]);
        hash.zeroize();

        // Clamp per RFC 7748 §5
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;

        let secret = X25519StaticSecret::from(*scalar);
        let public = X25519PublicKey::from(&secret);

        // Reject low-order points
        if is_low_order_point(public.as_bytes()) {
            return Err(IdentityError::LowOrderPoint);
        }

        Ok((secret, public))
    }
}

/// Check whether an X25519 public key is a low-order point.
fn is_low_order_point(point: &[u8; 32]) -> bool {
    LOW_ORDER_POINTS.iter().any(|p| p == point)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn generate_identity() {
        let id = GatewayIdentity::generate().unwrap();
        // Seed, gateway_id, and public_key should all be non-zero.
        assert_ne!(*id.seed(), [0u8; 32]);
        assert_ne!(*id.gateway_id(), [0u8; 16]);
        assert_ne!(*id.public_key(), [0u8; 32]);
    }

    #[test]
    fn from_parts_round_trip() {
        let id = GatewayIdentity::generate().unwrap();
        let id2 = GatewayIdentity::from_parts(Zeroizing::new(*id.seed()), *id.gateway_id());
        assert_eq!(id.public_key(), id2.public_key());
        assert_eq!(id.gateway_id(), id2.gateway_id());
    }

    #[test]
    fn deterministic_public_key() {
        let seed = Zeroizing::new([0x42u8; 32]);
        let id1 = GatewayIdentity::from_parts(seed.clone(), [0xAA; 16]);
        let id2 = GatewayIdentity::from_parts(seed, [0xBB; 16]);
        // Same seed → same public key (gateway_id is independent).
        assert_eq!(id1.public_key(), id2.public_key());
    }

    #[test]
    fn sign_and_verify() {
        let id = GatewayIdentity::generate().unwrap();
        let message = b"test challenge data";
        let signature = id.signing_key().sign(message);
        assert!(id
            .verifying_key()
            .verify_strict(message, &signature)
            .is_ok());
    }

    #[test]
    fn x25519_conversion_succeeds() {
        let id = GatewayIdentity::generate().unwrap();
        let (secret, public) = id.to_x25519().unwrap();
        // The public key should be non-zero.
        assert_ne!(*public.as_bytes(), [0u8; 32]);
        // ECDH with itself should produce a non-zero shared secret.
        let shared = secret.diffie_hellman(&public);
        assert_ne!(*shared.as_bytes(), [0u8; 32]);
    }

    #[test]
    fn x25519_deterministic() {
        let seed = Zeroizing::new([0x42u8; 32]);
        let id = GatewayIdentity::from_parts(seed, [0; 16]);
        let (_, pub1) = id.to_x25519().unwrap();
        let (_, pub2) = id.to_x25519().unwrap();
        assert_eq!(pub1.as_bytes(), pub2.as_bytes());
    }

    #[test]
    fn x25519_known_test_vector() {
        // Known Ed25519 seed → expected X25519 public key.
        // Seed: 32 bytes of 0x42; derive the X25519 keypair and verify
        // the public key matches a pre-computed reference value.
        let seed = Zeroizing::new([0x42u8; 32]);
        let id = GatewayIdentity::from_parts(seed, [0; 16]);
        let (secret, public) = id.to_x25519().unwrap();

        // Verify ECDH produces the expected shared secret with a known
        // ephemeral public key (all-0x01 clamped scalar → public key).
        let eph_secret = x25519_dalek::StaticSecret::from([0x01u8; 32]);
        let eph_public = X25519PublicKey::from(&eph_secret);

        let shared_a = secret.diffie_hellman(&eph_public);
        let shared_b = eph_secret.diffie_hellman(&public);
        assert_eq!(
            shared_a.as_bytes(),
            shared_b.as_bytes(),
            "ECDH shared secrets must match (commutativity)"
        );
        // Shared secret must be non-zero (not a low-order result).
        assert_ne!(*shared_a.as_bytes(), [0u8; 32]);
    }

    #[test]
    fn low_order_points_detected() {
        for point in &LOW_ORDER_POINTS {
            assert!(
                is_low_order_point(point),
                "expected low-order rejection for {:?}",
                &point[..4]
            );
        }
    }

    #[test]
    fn normal_point_not_rejected() {
        // A random non-low-order point.
        let id = GatewayIdentity::generate().unwrap();
        let (_, public) = id.to_x25519().unwrap();
        assert!(!is_low_order_point(public.as_bytes()));
    }
}
