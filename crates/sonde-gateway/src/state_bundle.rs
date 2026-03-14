// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! AES-256-GCM-encrypted CBOR state bundle for gateway export/import.
//!
//! # Bundle wire format
//!
//! ```text
//! ┌──────────────────┬───────────────────────────────────────────────────────┐
//! │  Field           │  Size / description                                   │
//! ├──────────────────┼───────────────────────────────────────────────────────┤
//! │  magic           │  8 bytes  – b"SNDESTAT"                               │
//! │  version         │  4 bytes  – little-endian u32, currently 1            │
//! │  salt            │ 16 bytes  – random PBKDF2-HMAC-SHA256 salt            │
//! │  nonce           │ 12 bytes  – random AES-256-GCM nonce                  │
//! │  ciphertext      │  n bytes  – AES-256-GCM ciphertext of CBOR payload    │
//! │  (GCM auth tag)  │ (16 bytes appended by AES-GCM, part of ciphertext)    │
//! └──────────────────┴───────────────────────────────────────────────────────┘
//! ```
//!
//! The key is derived from the operator passphrase with PBKDF2-HMAC-SHA256 at
//! 100 000 iterations.  The CBOR plaintext contains the full node registry
//! (including PSKs) and program library.  Handler routing configuration is not
//! included and must be restored separately (deferred per Phase 2C-iii).

use std::fmt;
use std::time::{Duration, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::program::{ProgramRecord, VerificationProfile};
use crate::registry::NodeRecord;

// ── Bundle constants ─────────────────────────────────────────────────────────

const MAGIC: &[u8; 8] = b"SNDESTAT";
const FORMAT_VERSION: u32 = 1;
const PBKDF2_ITERATIONS: u32 = 100_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
/// Total header size: magic(8) + version(4) + salt(16) + nonce(12).
const HEADER_LEN: usize = 8 + 4 + SALT_LEN + NONCE_LEN;
/// Minimum AES-GCM authentication tag size.
const GCM_TAG_LEN: usize = 16;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors from state-bundle operations.
#[derive(Debug)]
pub enum BundleError {
    /// Passphrase is empty — refused to process key material without protection.
    EmptyPassphrase,
    /// CBOR encoding error.
    Encode(String),
    /// CBOR decoding or structural error.
    Decode(String),
    /// AES-GCM decryption failure (wrong passphrase or tampered data).
    Crypto,
    /// CSPRNG failure — the OS could not provide random bytes.
    Rng,
    /// Bundle does not begin with the expected magic bytes.
    InvalidMagic,
    /// Bundle format version is not supported by this implementation.
    UnsupportedVersion(u32),
    /// Bundle is too short to contain a valid header + ciphertext.
    Truncated,
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleError::EmptyPassphrase => write!(f, "passphrase must not be empty"),
            BundleError::Encode(e) => write!(f, "CBOR encode error: {e}"),
            BundleError::Decode(e) => write!(f, "CBOR decode error: {e}"),
            BundleError::Crypto => {
                write!(f, "decryption failed — wrong passphrase or tampered bundle")
            }
            BundleError::Rng => {
                write!(f, "CSPRNG failure — could not obtain random bytes from OS")
            }
            BundleError::InvalidMagic => {
                write!(f, "not a valid state bundle (bad magic bytes)")
            }
            BundleError::UnsupportedVersion(v) => {
                write!(f, "unsupported state bundle version: {v}")
            }
            BundleError::Truncated => write!(f, "state bundle is truncated"),
        }
    }
}

impl std::error::Error for BundleError {}

// ── Public API ───────────────────────────────────────────────────────────────

/// Serialize and encrypt gateway state into a portable binary bundle.
///
/// `passphrase` must be non-empty; it is used to derive the AES-256 encryption
/// key via PBKDF2-HMAC-SHA256 with a random 16-byte salt.
///
/// Handler routing configuration is not included in the bundle (deferred to
/// Phase 2C-iii).
pub fn encrypt_state(
    nodes: &[NodeRecord],
    programs: &[ProgramRecord],
    passphrase: &str,
) -> Result<Vec<u8>, BundleError> {
    if passphrase.is_empty() {
        return Err(BundleError::EmptyPassphrase);
    }

    let plaintext = Zeroizing::new(encode_cbor(nodes, programs)?);

    // Random salt and nonce via OS CSPRNG — unique per export to prevent
    // key/nonce reuse.
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| BundleError::Rng)?;
    getrandom::fill(&mut nonce_bytes).map_err(|_| BundleError::Rng)?;

    let key = Zeroizing::new(derive_key(passphrase, &salt));
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()));
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_slice())
        .map_err(|_| BundleError::Crypto)?;

    let mut bundle = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    bundle.extend_from_slice(MAGIC);
    bundle.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bundle.extend_from_slice(&salt);
    bundle.extend_from_slice(&nonce_bytes);
    bundle.extend_from_slice(&ciphertext);
    Ok(bundle)
}

/// Decrypt and deserialize a state bundle, returning `(nodes, programs)`.
///
/// Returns an error if the passphrase is wrong, the bundle is tampered,
/// or the bundle is malformed.
pub fn decrypt_state(
    bundle: &[u8],
    passphrase: &str,
) -> Result<(Vec<NodeRecord>, Vec<ProgramRecord>), BundleError> {
    if passphrase.is_empty() {
        return Err(BundleError::EmptyPassphrase);
    }
    if bundle.len() < HEADER_LEN + GCM_TAG_LEN {
        return Err(BundleError::Truncated);
    }

    // Validate magic.
    if &bundle[..8] != MAGIC.as_slice() {
        return Err(BundleError::InvalidMagic);
    }

    // Validate version.
    let version = u32::from_le_bytes(bundle[8..12].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(BundleError::UnsupportedVersion(version));
    }

    let salt: &[u8; SALT_LEN] = bundle[12..28].try_into().unwrap();
    let nonce_bytes: &[u8; NONCE_LEN] = bundle[28..40].try_into().unwrap();
    let ciphertext = &bundle[HEADER_LEN..];

    let key = Zeroizing::new(derive_key(passphrase, salt));
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()));
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = Zeroizing::new(
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| BundleError::Crypto)?,
    );

    decode_cbor(&plaintext)
}

// ── Key derivation ───────────────────────────────────────────────────────────

fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, PBKDF2_ITERATIONS, &mut key);
    key
}

// ── CBOR encoding ─────────────────────────────────────────────────────────────

fn encode_cbor(nodes: &[NodeRecord], programs: &[ProgramRecord]) -> Result<Vec<u8>, BundleError> {
    use ciborium::value::Value;

    let node_values: Vec<Value> = nodes.iter().map(node_to_cbor).collect();
    let program_values: Vec<Value> = programs.iter().map(program_to_cbor).collect();

    let root = Value::Map(vec![
        (
            Value::Integer(1u8.into()),
            Value::Integer(FORMAT_VERSION.into()),
        ),
        (Value::Integer(2u8.into()), Value::Array(node_values)),
        (Value::Integer(3u8.into()), Value::Array(program_values)),
    ]);

    let mut buf = Vec::new();
    ciborium::ser::into_writer(&root, &mut buf).map_err(|e| BundleError::Encode(e.to_string()))?;
    Ok(buf)
}

fn node_to_cbor(n: &NodeRecord) -> ciborium::value::Value {
    use ciborium::value::Value;

    let last_seen_s: Option<i64> = n.last_seen.and_then(|t| {
        t.duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    });

    Value::Map(vec![
        (Value::Integer(1u8.into()), Value::Text(n.node_id.clone())),
        (
            Value::Integer(2u8.into()),
            Value::Integer(n.key_hint.into()),
        ),
        (Value::Integer(3u8.into()), Value::Bytes(n.psk.to_vec())),
        (
            Value::Integer(4u8.into()),
            opt_bytes_val(&n.assigned_program_hash),
        ),
        (
            Value::Integer(5u8.into()),
            opt_bytes_val(&n.current_program_hash),
        ),
        (
            Value::Integer(6u8.into()),
            Value::Integer(n.schedule_interval_s.into()),
        ),
        (
            Value::Integer(7u8.into()),
            opt_u32_val(n.firmware_abi_version),
        ),
        (Value::Integer(8u8.into()), opt_u32_val(n.last_battery_mv)),
        (Value::Integer(9u8.into()), opt_i64_val(last_seen_s)),
    ])
}

fn program_to_cbor(p: &ProgramRecord) -> ciborium::value::Value {
    use ciborium::value::Value;

    let profile_u8: u8 = match p.verification_profile {
        VerificationProfile::Resident => 1,
        VerificationProfile::Ephemeral => 2,
    };

    Value::Map(vec![
        (Value::Integer(1u8.into()), Value::Bytes(p.hash.clone())),
        (Value::Integer(2u8.into()), Value::Bytes(p.image.clone())),
        (Value::Integer(3u8.into()), Value::Integer(p.size.into())),
        (
            Value::Integer(4u8.into()),
            Value::Integer(profile_u8.into()),
        ),
        (Value::Integer(5u8.into()), opt_u32_val(p.abi_version)),
    ])
}

fn opt_bytes_val(v: &Option<Vec<u8>>) -> ciborium::value::Value {
    match v {
        Some(b) => ciborium::value::Value::Bytes(b.clone()),
        None => ciborium::value::Value::Null,
    }
}

fn opt_u32_val(v: Option<u32>) -> ciborium::value::Value {
    match v {
        Some(n) => ciborium::value::Value::Integer(n.into()),
        None => ciborium::value::Value::Null,
    }
}

fn opt_i64_val(v: Option<i64>) -> ciborium::value::Value {
    match v {
        Some(n) => ciborium::value::Value::Integer(n.into()),
        None => ciborium::value::Value::Null,
    }
}

// ── CBOR decoding ─────────────────────────────────────────────────────────────

fn decode_cbor(data: &[u8]) -> Result<(Vec<NodeRecord>, Vec<ProgramRecord>), BundleError> {
    use ciborium::value::Value;

    let root: Value =
        ciborium::de::from_reader(data).map_err(|e| BundleError::Decode(e.to_string()))?;

    let map = match root {
        Value::Map(m) => m,
        _ => return Err(BundleError::Decode("root must be a CBOR map".into())),
    };

    let mut version_opt: Option<u32> = None;
    let mut nodes_opt: Option<Vec<Value>> = None;
    let mut programs_opt: Option<Vec<Value>> = None;

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(1) => {
                    version_opt = Some(match v {
                        Value::Integer(i) => u32::try_from(i)
                            .map_err(|_| BundleError::Decode("version out of range".into()))?,
                        _ => return Err(BundleError::Decode("version must be integer".into())),
                    });
                }
                Some(2) => {
                    nodes_opt = Some(match v {
                        Value::Array(a) => a,
                        _ => return Err(BundleError::Decode("nodes must be array".into())),
                    });
                }
                Some(3) => {
                    programs_opt = Some(match v {
                        Value::Array(a) => a,
                        _ => return Err(BundleError::Decode("programs must be array".into())),
                    });
                }
                _ => {} // ignore unknown keys for forward compatibility
            }
        }
    }

    let version = version_opt.ok_or_else(|| BundleError::Decode("missing version field".into()))?;
    if version != FORMAT_VERSION {
        return Err(BundleError::UnsupportedVersion(version));
    }
    let nodes_arr = nodes_opt.ok_or_else(|| BundleError::Decode("missing nodes field".into()))?;
    let programs_arr =
        programs_opt.ok_or_else(|| BundleError::Decode("missing programs field".into()))?;

    let nodes = nodes_arr
        .into_iter()
        .map(node_from_cbor)
        .collect::<Result<Vec<_>, _>>()?;

    let programs = programs_arr
        .into_iter()
        .map(program_from_cbor)
        .collect::<Result<Vec<_>, _>>()?;

    Ok((nodes, programs))
}

fn node_from_cbor(v: ciborium::value::Value) -> Result<NodeRecord, BundleError> {
    use ciborium::value::Value;

    let map = match v {
        Value::Map(m) => m,
        _ => return Err(BundleError::Decode("node entry must be a CBOR map".into())),
    };

    let mut node_id: Option<String> = None;
    let mut key_hint: Option<u16> = None;
    let mut psk: Option<[u8; 32]> = None;
    let mut assigned_program_hash: Option<Option<Vec<u8>>> = None;
    let mut current_program_hash: Option<Option<Vec<u8>>> = None;
    let mut schedule_interval_s: Option<u32> = None;
    let mut firmware_abi_version: Option<Option<u32>> = None;
    let mut last_battery_mv: Option<Option<u32>> = None;
    let mut last_seen_epoch_s: Option<Option<i64>> = None;

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(1) => {
                    node_id = Some(match v {
                        Value::Text(s) => s,
                        _ => return Err(BundleError::Decode("node_id must be text".into())),
                    });
                }
                Some(2) => {
                    key_hint = Some(match v {
                        Value::Integer(i) => u16::try_from(i)
                            .map_err(|_| BundleError::Decode("key_hint out of range".into()))?,
                        _ => return Err(BundleError::Decode("key_hint must be integer".into())),
                    });
                }
                Some(3) => {
                    psk = Some(match v {
                        Value::Bytes(b) => b
                            .try_into()
                            .map_err(|_| BundleError::Decode("psk must be 32 bytes".into()))?,
                        _ => return Err(BundleError::Decode("psk must be bytes".into())),
                    });
                }
                Some(4) => {
                    assigned_program_hash = Some(opt_bytes_from_cbor(v, "assigned_program_hash")?);
                }
                Some(5) => {
                    current_program_hash = Some(opt_bytes_from_cbor(v, "current_program_hash")?);
                }
                Some(6) => {
                    schedule_interval_s = Some(match v {
                        Value::Integer(i) => u32::try_from(i).map_err(|_| {
                            BundleError::Decode("schedule_interval_s out of range".into())
                        })?,
                        _ => {
                            return Err(BundleError::Decode(
                                "schedule_interval_s must be integer".into(),
                            ))
                        }
                    });
                }
                Some(7) => {
                    firmware_abi_version = Some(opt_u32_from_cbor(v, "firmware_abi_version")?);
                }
                Some(8) => {
                    last_battery_mv = Some(opt_u32_from_cbor(v, "last_battery_mv")?);
                }
                Some(9) => {
                    last_seen_epoch_s = Some(opt_i64_from_cbor(v, "last_seen_epoch_s")?);
                }
                _ => {} // ignore unknown fields for forward compatibility
            }
        }
    }

    let node_id =
        node_id.ok_or_else(|| BundleError::Decode("missing node_id in node entry".into()))?;
    let key_hint =
        key_hint.ok_or_else(|| BundleError::Decode("missing key_hint in node entry".into()))?;
    let psk = psk.ok_or_else(|| BundleError::Decode("missing psk in node entry".into()))?;
    let schedule_interval_s = schedule_interval_s.unwrap_or(60);

    let last_seen = last_seen_epoch_s
        .flatten()
        .filter(|&s| s >= 0)
        .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s as u64)));

    // Validate hash fields if present: must be 32 bytes (SHA-256).
    if let Some(Some(ref h)) = assigned_program_hash {
        if h.len() != 32 {
            return Err(BundleError::Decode(format!(
                "assigned_program_hash must be 32 bytes, got {}",
                h.len()
            )));
        }
    }
    if let Some(Some(ref h)) = current_program_hash {
        if h.len() != 32 {
            return Err(BundleError::Decode(format!(
                "current_program_hash must be 32 bytes, got {}",
                h.len()
            )));
        }
    }

    Ok(NodeRecord {
        node_id,
        key_hint,
        psk,
        assigned_program_hash: assigned_program_hash.flatten(),
        current_program_hash: current_program_hash.flatten(),
        schedule_interval_s,
        firmware_abi_version: firmware_abi_version.flatten(),
        last_battery_mv: last_battery_mv.flatten(),
        last_seen,
    })
}

fn program_from_cbor(v: ciborium::value::Value) -> Result<ProgramRecord, BundleError> {
    use ciborium::value::Value;

    let map = match v {
        Value::Map(m) => m,
        _ => {
            return Err(BundleError::Decode(
                "program entry must be a CBOR map".into(),
            ))
        }
    };

    let mut hash: Option<Vec<u8>> = None;
    let mut image: Option<Vec<u8>> = None;
    let mut size: Option<u32> = None;
    let mut profile_int: Option<u32> = None;
    let mut abi_version: Option<Option<u32>> = None;

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(1) => {
                    hash = Some(match v {
                        Value::Bytes(b) => b,
                        _ => return Err(BundleError::Decode("hash must be bytes".into())),
                    });
                }
                Some(2) => {
                    image = Some(match v {
                        Value::Bytes(b) => b,
                        _ => return Err(BundleError::Decode("image must be bytes".into())),
                    });
                }
                Some(3) => {
                    size = Some(match v {
                        Value::Integer(i) => u32::try_from(i)
                            .map_err(|_| BundleError::Decode("size out of range".into()))?,
                        _ => return Err(BundleError::Decode("size must be integer".into())),
                    });
                }
                Some(4) => {
                    profile_int = Some(match v {
                        Value::Integer(i) => u32::try_from(i).map_err(|_| {
                            BundleError::Decode("verification_profile out of range".into())
                        })?,
                        _ => {
                            return Err(BundleError::Decode(
                                "verification_profile must be integer".into(),
                            ))
                        }
                    });
                }
                Some(5) => {
                    abi_version = Some(opt_u32_from_cbor(v, "abi_version")?);
                }
                _ => {}
            }
        }
    }

    let hash = hash.ok_or_else(|| BundleError::Decode("missing hash in program entry".into()))?;
    let image =
        image.ok_or_else(|| BundleError::Decode("missing image in program entry".into()))?;
    let size = size.ok_or_else(|| BundleError::Decode("missing size in program entry".into()))?;
    let profile_int = profile_int.ok_or_else(|| {
        BundleError::Decode("missing verification_profile in program entry".into())
    })?;

    // Validate invariants: hash must be 32 bytes, size must match image length.
    if hash.len() != 32 {
        return Err(BundleError::Decode(format!(
            "program hash must be 32 bytes, got {}",
            hash.len()
        )));
    }
    let image_len = u32::try_from(image.len()).map_err(|_| {
        BundleError::Decode(format!("program image too large: {} bytes", image.len()))
    })?;
    if size != image_len {
        return Err(BundleError::Decode(format!(
            "program size field ({size}) does not match image length ({image_len})"
        )));
    }

    let verification_profile = match profile_int {
        1 => VerificationProfile::Resident,
        2 => VerificationProfile::Ephemeral,
        v => {
            return Err(BundleError::Decode(format!(
                "unknown verification_profile value: {v}"
            )))
        }
    };

    Ok(ProgramRecord {
        hash,
        image,
        size,
        verification_profile,
        abi_version: abi_version.flatten(),
    })
}

fn opt_bytes_from_cbor(
    v: ciborium::value::Value,
    field: &str,
) -> Result<Option<Vec<u8>>, BundleError> {
    use ciborium::value::Value;
    match v {
        Value::Null => Ok(None),
        Value::Bytes(b) => Ok(Some(b)),
        _ => Err(BundleError::Decode(format!(
            "{field} must be bytes or null"
        ))),
    }
}

fn opt_u32_from_cbor(v: ciborium::value::Value, field: &str) -> Result<Option<u32>, BundleError> {
    use ciborium::value::Value;
    match v {
        Value::Null => Ok(None),
        Value::Integer(i) => u32::try_from(i)
            .map(Some)
            .map_err(|_| BundleError::Decode(format!("{field} out of range"))),
        _ => Err(BundleError::Decode(format!(
            "{field} must be integer or null"
        ))),
    }
}

fn opt_i64_from_cbor(v: ciborium::value::Value, field: &str) -> Result<Option<i64>, BundleError> {
    use ciborium::value::Value;
    match v {
        Value::Null => Ok(None),
        Value::Integer(i) => i64::try_from(i)
            .map(Some)
            .map_err(|_| BundleError::Decode(format!("{field} out of range"))),
        _ => Err(BundleError::Decode(format!(
            "{field} must be integer or null"
        ))),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::VerificationProfile;
    use crate::registry::NodeRecord;

    fn make_node(id: &str, key_hint: u16) -> NodeRecord {
        let mut psk = [0u8; 32];
        psk[0] = key_hint as u8;
        psk[1] = (key_hint >> 8) as u8;
        NodeRecord::new(id.to_string(), key_hint, psk)
    }

    fn make_program(seed: u8, profile: VerificationProfile) -> ProgramRecord {
        let image = vec![seed; 64];
        let hash = vec![seed; 32];
        ProgramRecord {
            hash,
            image,
            size: 64,
            verification_profile: profile,
            abi_version: None,
        }
    }

    #[test]
    fn roundtrip_empty_state() {
        let bundle = encrypt_state(&[], &[], "hunter2").unwrap();
        let (nodes, programs) = decrypt_state(&bundle, "hunter2").unwrap();
        assert!(nodes.is_empty());
        assert!(programs.is_empty());
    }

    #[test]
    fn roundtrip_nodes_and_programs() {
        let mut node1 = make_node("node-a", 0x1234);
        node1.schedule_interval_s = 120;
        node1.last_battery_mv = Some(3700);
        let node2 = make_node("node-b", 0x5678);

        let prog1 = make_program(0xAA, VerificationProfile::Resident);
        let prog2 = make_program(0xBB, VerificationProfile::Ephemeral);

        let nodes = vec![node1, node2];
        let programs = vec![prog1, prog2];

        let bundle = encrypt_state(&nodes, &programs, "correct-pass").unwrap();
        let (out_nodes, out_programs) = decrypt_state(&bundle, "correct-pass").unwrap();

        assert_eq!(out_nodes.len(), 2);
        assert_eq!(out_programs.len(), 2);

        let na = out_nodes.iter().find(|n| n.node_id == "node-a").unwrap();
        assert_eq!(na.key_hint, 0x1234);
        assert_eq!(na.psk[0], 0x34);
        assert_eq!(na.schedule_interval_s, 120);
        assert_eq!(na.last_battery_mv, Some(3700));

        let nb = out_nodes.iter().find(|n| n.node_id == "node-b").unwrap();
        assert_eq!(nb.key_hint, 0x5678);

        let pr = out_programs
            .iter()
            .find(|p| p.hash == vec![0xAAu8; 32])
            .unwrap();
        assert_eq!(pr.verification_profile, VerificationProfile::Resident);
        assert_eq!(pr.size, 64);

        let pe = out_programs
            .iter()
            .find(|p| p.hash == vec![0xBBu8; 32])
            .unwrap();
        assert_eq!(pe.verification_profile, VerificationProfile::Ephemeral);
    }

    #[test]
    fn wrong_passphrase_returns_crypto_error() {
        let bundle = encrypt_state(&[], &[], "correct").unwrap();
        let err = decrypt_state(&bundle, "wrong").unwrap_err();
        assert!(matches!(err, BundleError::Crypto));
    }

    #[test]
    fn empty_passphrase_rejected_on_encrypt() {
        let err = encrypt_state(&[], &[], "").unwrap_err();
        assert!(matches!(err, BundleError::EmptyPassphrase));
    }

    #[test]
    fn empty_passphrase_rejected_on_decrypt() {
        let bundle = encrypt_state(&[], &[], "pass").unwrap();
        let err = decrypt_state(&bundle, "").unwrap_err();
        assert!(matches!(err, BundleError::EmptyPassphrase));
    }

    #[test]
    fn tampered_ciphertext_rejected() {
        let mut bundle = encrypt_state(&[], &[], "secret").unwrap();
        // Flip a byte in the ciphertext region.
        let last = bundle.len() - 1;
        bundle[last] ^= 0xFF;
        let err = decrypt_state(&bundle, "secret").unwrap_err();
        assert!(matches!(err, BundleError::Crypto));
    }

    #[test]
    fn invalid_magic_rejected() {
        let mut bundle = encrypt_state(&[], &[], "pass").unwrap();
        bundle[0] = b'X';
        let err = decrypt_state(&bundle, "pass").unwrap_err();
        assert!(matches!(err, BundleError::InvalidMagic));
    }

    #[test]
    fn truncated_bundle_rejected() {
        let err = decrypt_state(&[0u8; 10], "pass").unwrap_err();
        assert!(matches!(err, BundleError::Truncated));
    }

    #[test]
    fn node_psk_preserved_exactly() {
        let mut psk = [0u8; 32];
        for (i, b) in psk.iter_mut().enumerate() {
            *b = i as u8;
        }
        let node = NodeRecord::new("psk-node".to_string(), 42, psk);
        let bundle = encrypt_state(&[node], &[], "p@ssw0rd").unwrap();
        let (out_nodes, _) = decrypt_state(&bundle, "p@ssw0rd").unwrap();
        assert_eq!(out_nodes[0].psk, psk);
    }

    #[test]
    fn different_exports_produce_different_bundles() {
        // Each export uses a fresh random salt and nonce.
        let b1 = encrypt_state(&[], &[], "pass").unwrap();
        let b2 = encrypt_state(&[], &[], "pass").unwrap();
        assert_ne!(b1, b2, "two exports with the same passphrase must differ");
    }

    #[test]
    fn node_with_optional_fields_none_roundtrips() {
        let node = NodeRecord::new("opt-node".to_string(), 7, [0xFFu8; 32]);
        assert!(node.assigned_program_hash.is_none());
        assert!(node.current_program_hash.is_none());
        assert!(node.firmware_abi_version.is_none());
        assert!(node.last_battery_mv.is_none());
        assert!(node.last_seen.is_none());

        let bundle = encrypt_state(&[node], &[], "pass").unwrap();
        let (out, _) = decrypt_state(&bundle, "pass").unwrap();
        let n = &out[0];
        assert!(n.assigned_program_hash.is_none());
        assert!(n.current_program_hash.is_none());
        assert!(n.firmware_abi_version.is_none());
        assert!(n.last_battery_mv.is_none());
        assert!(n.last_seen.is_none());
    }
}
