// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Master key rotation payload handling (GW-2006, evolve-962 §2.6).
//!
//! Provides [`RotationPayloadV1`] parsing and decryption, plus
//! rate-limiting for failed rotation attempts.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::Zeroizing;

/// Minimum total payload length: version(1) + ephemeral_public(32) + nonce(12) + tag(16) = 61.
const MIN_PAYLOAD_LEN: usize = 1 + 32 + 12 + 16;

/// Maximum total payload length. The plaintext is a small CBOR map (~100 bytes),
/// so the encrypted payload should be well under 1 KiB. Reject larger payloads
/// to prevent expensive AEAD work on oversized inputs.
const MAX_PAYLOAD_LEN: usize = 1024;

/// HKDF salt constant (17 bytes, ASCII "sonde-rotation-v1").
const HKDF_SALT: &[u8; 17] = b"sonde-rotation-v1";

/// Errors from rotation payload processing.
#[derive(Debug)]
pub enum RotationError {
    /// Payload too short.
    TooShort(usize),
    /// Payload too large.
    TooLarge(usize),
    /// Unknown version byte.
    UnknownVersion(u8),
    /// Ephemeral public key is a low-order point.
    LowOrderPoint,
    /// AES-GCM decryption failed (wrong key, corrupted, or AAD mismatch).
    DecryptionFailed,
    /// Rotation code does not match.
    WrongRotationCode,
    /// Rate limited — too many failed attempts.
    RateLimited,
    /// New master key has wrong length.
    InvalidKeyLength(usize),
    /// New master key ID has wrong length.
    InvalidKeyIdLength(usize),
    /// CBOR plaintext parse error.
    PlaintextParse(String),
    /// Internal error.
    Internal(String),
}

impl std::fmt::Display for RotationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort(len) => {
                write!(f, "payload too short: {len} bytes (min {MIN_PAYLOAD_LEN})")
            }
            Self::TooLarge(len) => {
                write!(f, "payload too large: {len} bytes (max {MAX_PAYLOAD_LEN})")
            }
            Self::UnknownVersion(v) => write!(f, "unknown rotation payload version: {v:#04x}"),
            Self::LowOrderPoint => write!(f, "ephemeral public key is a low-order point"),
            Self::DecryptionFailed => write!(f, "rotation payload decryption failed"),
            Self::WrongRotationCode => write!(f, "rotation code does not match"),
            Self::RateLimited => write!(f, "rotation attempt rate-limited"),
            Self::InvalidKeyLength(len) => {
                write!(f, "new_master_key must be 32 bytes, got {len}")
            }
            Self::InvalidKeyIdLength(len) => {
                write!(f, "new_master_key_id must be 16 bytes, got {len}")
            }
            Self::PlaintextParse(msg) => write!(f, "rotation plaintext parse error: {msg}"),
            Self::Internal(msg) => write!(f, "rotation internal error: {msg}"),
        }
    }
}

impl std::error::Error for RotationError {}

/// Parsed and decrypted rotation payload contents.
pub struct DecryptedRotation {
    /// New 32-byte master key.
    pub new_master_key: Zeroizing<[u8; 32]>,
    /// 6-character rotation code from modem display.
    pub rotation_code: String,
    /// Random 16-byte ID for the new key.
    pub new_master_key_id: [u8; 16],
    /// KDF salt (included on first rotation).
    pub salt: Option<Vec<u8>>,
    /// KDF parameters.
    pub kdf_params: Option<KdfParamsPayload>,
}

/// KDF parameters from the rotation payload.
#[derive(Clone, Debug)]
pub struct KdfParamsPayload {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub kdf_version: u32,
}

/// Decrypt and validate a RotationPayloadV1 (evolve-962 §2.6.1).
///
/// # Parameters
/// - `payload`: Raw rotation payload bytes.
/// - `gw_x25519_secret`: Gateway's X25519 private key (from `GatewayIdentity.to_x25519()`).
/// - `gateway_id`: Raw 16-byte gateway identifier.
/// - `current_epoch`: Gateway's current `master_key_epoch`.
///
/// # Returns
/// The decrypted rotation contents, or a `RotationError`.
pub fn decrypt_rotation_payload(
    payload: &[u8],
    gw_x25519_secret: &X25519StaticSecret,
    gateway_id: &[u8; 16],
    current_epoch: u64,
) -> Result<DecryptedRotation, RotationError> {
    if payload.len() < MIN_PAYLOAD_LEN {
        return Err(RotationError::TooShort(payload.len()));
    }
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(RotationError::TooLarge(payload.len()));
    }

    // Version check
    let version = payload[0];
    if version != 0x01 {
        return Err(RotationError::UnknownVersion(version));
    }

    // Parse binary layout
    let sender_ephemeral_public: [u8; 32] = payload[1..33]
        .try_into()
        .map_err(|_| RotationError::Internal("ephemeral public key slice".into()))?;
    let nonce_bytes: [u8; 12] = payload[33..45]
        .try_into()
        .map_err(|_| RotationError::Internal("nonce slice".into()))?;
    let ciphertext_and_tag = &payload[45..];

    // Check for low-order point — an all-zero shared secret indicates
    // the sender used a low-order or invalid public key.
    let ephemeral_public = X25519PublicKey::from(sender_ephemeral_public);
    let shared_secret = gw_x25519_secret.diffie_hellman(&ephemeral_public);
    if shared_secret.as_bytes().iter().all(|&b| b == 0) {
        return Err(RotationError::LowOrderPoint);
    }

    // Build HKDF info: gateway_id_raw || current_master_key_epoch_be64
    let mut info = Vec::with_capacity(24);
    info.extend_from_slice(gateway_id);
    info.extend_from_slice(&current_epoch.to_be_bytes());

    // HKDF-SHA-256
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), shared_secret.as_bytes());
    let mut derived_key = Zeroizing::new([0u8; 32]);
    hk.expand(&info, &mut *derived_key)
        .map_err(|_| RotationError::Internal("HKDF expand failed".into()))?;

    // AES-256-GCM decryption with AAD = gateway_id_raw || epoch_be64
    let key = Key::<Aes256Gcm>::from_slice(&*derived_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let aad = info; // Same as HKDF info: gateway_id || epoch_be64
    let gcm_payload = Payload {
        msg: ciphertext_and_tag,
        aad: &aad,
    };

    let plaintext = Zeroizing::new(
        cipher
            .decrypt(nonce, gcm_payload)
            .map_err(|_| RotationError::DecryptionFailed)?,
    );

    // Parse CBOR plaintext: {1: new_master_key, 2: rotation_code, 3: new_master_key_id, 4: salt, 5: kdf_params}
    parse_rotation_plaintext(&plaintext)
}

/// Parse the CBOR plaintext of a rotation payload.
fn parse_rotation_plaintext(plaintext: &[u8]) -> Result<DecryptedRotation, RotationError> {
    let value: ciborium::Value = ciborium::from_reader(plaintext)
        .map_err(|e| RotationError::PlaintextParse(format!("CBOR decode: {e}")))?;

    let map = match value {
        ciborium::Value::Map(m) => m,
        _ => return Err(RotationError::PlaintextParse("expected CBOR map".into())),
    };

    // Key 1: new_master_key (bstr, 32 bytes)
    let new_key_bytes = Zeroizing::new(get_bytes(&map, 1, "new_master_key")?);
    if new_key_bytes.len() != 32 {
        return Err(RotationError::InvalidKeyLength(new_key_bytes.len()));
    }
    let mut new_master_key = Zeroizing::new([0u8; 32]);
    new_master_key.copy_from_slice(&new_key_bytes);

    // Key 2: rotation_code (tstr, must be [A-Z0-9]{6})
    let rotation_code = get_text(&map, 2, "rotation_code")?;
    if rotation_code.len() != 6
        || !rotation_code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(RotationError::PlaintextParse(format!(
            "rotation_code must be 6 characters from [A-Z0-9], got {:?}",
            rotation_code
        )));
    }

    // Key 3: new_master_key_id (bstr, 16 bytes)
    let new_id_bytes = get_bytes(&map, 3, "new_master_key_id")?;
    if new_id_bytes.len() != 16 {
        return Err(RotationError::InvalidKeyIdLength(new_id_bytes.len()));
    }
    let mut new_master_key_id = [0u8; 16];
    new_master_key_id.copy_from_slice(&new_id_bytes);

    // Key 4: salt (bstr/null, optional — 16 bytes when present)
    let salt = get_optional_bytes_strict(&map, 4, "salt", Some(16))?;

    // Key 5: kdf_params (map/null, optional)
    let kdf_params = get_optional_kdf_params(&map, 5)?;

    Ok(DecryptedRotation {
        new_master_key,
        rotation_code,
        new_master_key_id,
        salt,
        kdf_params,
    })
}

// ── CBOR map helpers ───────────────────────────────────────────

fn get_value(map: &[(ciborium::Value, ciborium::Value)], key: u64) -> Option<&ciborium::Value> {
    map.iter()
        .find(|(k, _)| matches!(k, ciborium::Value::Integer(i) if u64::try_from(*i) == Ok(key)))
        .map(|(_, v)| v)
}

fn get_bytes(
    map: &[(ciborium::Value, ciborium::Value)],
    key: u64,
    field: &str,
) -> Result<Vec<u8>, RotationError> {
    match get_value(map, key) {
        Some(ciborium::Value::Bytes(b)) => Ok(b.clone()),
        Some(_) => Err(RotationError::PlaintextParse(format!(
            "`{field}` must be bstr"
        ))),
        None => Err(RotationError::PlaintextParse(format!("missing `{field}`"))),
    }
}

fn get_text(
    map: &[(ciborium::Value, ciborium::Value)],
    key: u64,
    field: &str,
) -> Result<String, RotationError> {
    match get_value(map, key) {
        Some(ciborium::Value::Text(t)) => Ok(t.clone()),
        Some(_) => Err(RotationError::PlaintextParse(format!(
            "`{field}` must be tstr"
        ))),
        None => Err(RotationError::PlaintextParse(format!("missing `{field}`"))),
    }
}

fn get_optional_bytes_strict(
    map: &[(ciborium::Value, ciborium::Value)],
    key: u64,
    field: &str,
    expected_len: Option<usize>,
) -> Result<Option<Vec<u8>>, RotationError> {
    match get_value(map, key) {
        Some(ciborium::Value::Bytes(b)) => {
            if let Some(len) = expected_len {
                if b.len() != len {
                    return Err(RotationError::PlaintextParse(format!(
                        "`{field}` must be {len} bytes, got {}",
                        b.len()
                    )));
                }
            }
            Ok(Some(b.clone()))
        }
        Some(ciborium::Value::Null) | None => Ok(None),
        Some(_) => Err(RotationError::PlaintextParse(format!(
            "`{field}` must be bstr or null"
        ))),
    }
}

fn get_optional_kdf_params(
    map: &[(ciborium::Value, ciborium::Value)],
    key: u64,
) -> Result<Option<KdfParamsPayload>, RotationError> {
    match get_value(map, key) {
        Some(ciborium::Value::Map(kdf_map)) => {
            let m_cost = get_u32(kdf_map, 1, "m_cost")?;
            let t_cost = get_u32(kdf_map, 2, "t_cost")?;
            let p_cost = get_u32(kdf_map, 3, "p_cost")?;
            let kdf_version = get_u32(kdf_map, 4, "kdf_version")?;
            Ok(Some(KdfParamsPayload {
                m_cost,
                t_cost,
                p_cost,
                kdf_version,
            }))
        }
        Some(ciborium::Value::Null) | None => Ok(None),
        Some(_) => Err(RotationError::PlaintextParse(
            "kdf_params must be a map or null".into(),
        )),
    }
}

fn get_u64(
    map: &[(ciborium::Value, ciborium::Value)],
    key: u64,
    field: &str,
) -> Result<u64, RotationError> {
    match get_value(map, key) {
        Some(ciborium::Value::Integer(i)) => u64::try_from(*i)
            .map_err(|_| RotationError::PlaintextParse(format!("`{field}` out of range"))),
        Some(_) => Err(RotationError::PlaintextParse(format!(
            "`{field}` must be uint"
        ))),
        None => Err(RotationError::PlaintextParse(format!("missing `{field}`"))),
    }
}

fn get_u32(
    map: &[(ciborium::Value, ciborium::Value)],
    key: u64,
    field: &str,
) -> Result<u32, RotationError> {
    let v = get_u64(map, key, field)?;
    u32::try_from(v)
        .map_err(|_| RotationError::PlaintextParse(format!("`{field}` exceeds u32::MAX ({v})")))
}

/// Rate limiter for failed rotation attempts (GW-2006 validation rule 8).
///
/// Tracks failed attempts per epoch. At most 3 failures per 5-minute window.
pub struct RotationRateLimiter {
    /// (epoch, timestamps of failed attempts within window)
    epoch: u64,
    failures: Vec<std::time::Instant>,
    max_failures: usize,
    window: std::time::Duration,
}

impl RotationRateLimiter {
    pub fn new() -> Self {
        Self {
            epoch: 0,
            failures: Vec::new(),
            max_failures: 3,
            window: std::time::Duration::from_secs(300),
        }
    }

    /// Check whether a rotation attempt is allowed.
    pub fn check(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch {
            self.epoch = epoch;
            self.failures.clear();
        }
        let now = std::time::Instant::now();
        self.failures
            .retain(|t| now.duration_since(*t) < self.window);
        self.failures.len() < self.max_failures
    }

    /// Record a failed rotation attempt.
    pub fn record_failure(&mut self, epoch: u64) {
        if epoch != self.epoch {
            self.epoch = epoch;
            self.failures.clear();
        }
        self.failures.push(std::time::Instant::now());
    }
}

impl Default for RotationRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_first_attempts() {
        let mut rl = RotationRateLimiter::new();
        assert!(rl.check(1));
        rl.record_failure(1);
        assert!(rl.check(1));
        rl.record_failure(1);
        assert!(rl.check(1));
        rl.record_failure(1);
        // 4th attempt should be blocked
        assert!(!rl.check(1));
    }

    #[test]
    fn test_rate_limiter_resets_on_epoch_change() {
        let mut rl = RotationRateLimiter::new();
        for _ in 0..3 {
            rl.record_failure(1);
        }
        assert!(!rl.check(1));
        // New epoch resets
        assert!(rl.check(2));
    }

    #[test]
    fn test_decrypt_rejects_short_payload() {
        let secret = X25519StaticSecret::from([0x42u8; 32]);
        let gw_id = [0x42u8; 16];
        let result = decrypt_rotation_payload(&[0x01; 10], &secret, &gw_id, 1);
        assert!(matches!(result, Err(RotationError::TooShort(10))));
    }

    #[test]
    fn test_decrypt_rejects_unknown_version() {
        let secret = X25519StaticSecret::from([0x42u8; 32]);
        let gw_id = [0x42u8; 16];
        let payload = [0x02; MIN_PAYLOAD_LEN];
        let result = decrypt_rotation_payload(&payload, &secret, &gw_id, 1);
        assert!(matches!(result, Err(RotationError::UnknownVersion(0x02))));
    }

    #[test]
    fn test_decrypt_rejects_bad_ciphertext() {
        let secret = X25519StaticSecret::from([0x42u8; 32]);
        let gw_id = [0x42u8; 16];
        // Valid version, random ephemeral, random nonce, garbage ciphertext
        let mut payload = vec![0x01];
        payload.extend_from_slice(&[0x42; 32]); // ephemeral public
        payload.extend_from_slice(&[0x00; 12]); // nonce
        payload.extend_from_slice(&[0xAA; 48]); // garbage ciphertext+tag
        let result = decrypt_rotation_payload(&payload, &secret, &gw_id, 1);
        assert!(matches!(result, Err(RotationError::DecryptionFailed)));
    }
}
