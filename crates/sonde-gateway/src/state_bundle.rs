// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! AES-256-GCM-encrypted CBOR state bundle for gateway export/import.
//!
//! # Bundle wire format
//!
//! The bundle is a flat byte sequence with five consecutive regions:
//! an 8-byte magic identifier (`b"SNDESTAT"`), a 4-byte little-endian version number
//! (currently `1`), a 16-byte random PBKDF2 salt, a 12-byte random AES-256-GCM nonce,
//! and finally the AES-256-GCM ciphertext of the CBOR payload (the 16-byte GCM
//! authentication tag is appended by AES-GCM and forms the last 16 bytes of the
//! ciphertext region).
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
//! (including PSKs), program library, and handler routing configuration.

use std::fmt;
use std::time::{Duration, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::gateway_identity::GatewayIdentity;
use crate::handler::{HandlerConfig, ProgramMatcher};
use crate::phone_trust::{PhonePskRecord, PhonePskStatus};
use crate::program::{ProgramRecord, VerificationProfile};
use crate::registry::{BatteryReading, NodeRecord, SensorDescriptor};

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

// ── CBOR key IDs (root map) ─────────────────────────────────────────────────

const ROOT_KEY_VERSION: i64 = 1;
const ROOT_KEY_NODES: i64 = 2;
const ROOT_KEY_PROGRAMS: i64 = 3;
const ROOT_KEY_IDENTITY: i64 = 4;
const ROOT_KEY_PHONE_PSKS: i64 = 5;
const ROOT_KEY_HANDLERS: i64 = 6;

// ── CBOR key IDs (node map) ─────────────────────────────────────────────────

const NODE_KEY_ID: i64 = 1;
const NODE_KEY_HINT: i64 = 2;
const NODE_KEY_PSK: i64 = 3;
const NODE_KEY_ASSIGNED_HASH: i64 = 4;
const NODE_KEY_CURRENT_HASH: i64 = 5;
const NODE_KEY_SCHEDULE: i64 = 6;
const NODE_KEY_FW_ABI: i64 = 7;
const NODE_KEY_BATTERY: i64 = 8;
const NODE_KEY_LAST_SEEN: i64 = 9;
const NODE_KEY_RF_CHANNEL: i64 = 10;
const NODE_KEY_SENSORS: i64 = 11;
const NODE_KEY_REGISTERED_BY: i64 = 12;
const NODE_KEY_BATTERY_HISTORY: i64 = 13;

// ── CBOR key IDs (program map) ──────────────────────────────────────────────

const PROG_KEY_HASH: i64 = 1;
const PROG_KEY_IMAGE: i64 = 2;
const PROG_KEY_SIZE: i64 = 3;
const PROG_KEY_PROFILE: i64 = 4;
const PROG_KEY_ABI: i64 = 5;

// ── CBOR key IDs (gateway identity map) ─────────────────────────────────────

const IDENT_KEY_SEED: i64 = 1;
const IDENT_KEY_GATEWAY_ID: i64 = 2;

// ── CBOR key IDs (phone PSK map) ────────────────────────────────────────────

const PHONE_KEY_HINT: i64 = 1;
const PHONE_KEY_PSK: i64 = 2;
const PHONE_KEY_LABEL: i64 = 3;
const PHONE_KEY_ISSUED_AT: i64 = 4;
const PHONE_KEY_STATUS: i64 = 5;

// ── CBOR key IDs (handler routing map) ──────────────────────────────────────

const HANDLER_KEY_MATCHERS: i64 = 1;
const HANDLER_KEY_COMMAND: i64 = 2;
const HANDLER_KEY_ARGS: i64 = 3;

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

/// Full state bundle contents including gateway identity, phone PSKs,
/// and handler routing configuration.
type FullState = (
    Vec<NodeRecord>,
    Vec<ProgramRecord>,
    Option<GatewayIdentity>,
    Vec<PhonePskRecord>,
    Vec<HandlerConfig>,
);

// ── Public API ───────────────────────────────────────────────────────────────

/// Serialize and encrypt gateway state into a portable binary bundle.
///
/// `passphrase` must be non-empty; it is used to derive the AES-256 encryption
/// key via PBKDF2-HMAC-SHA256 with a random 16-byte salt.
pub fn encrypt_state(
    nodes: &[NodeRecord],
    programs: &[ProgramRecord],
    passphrase: &str,
) -> Result<Vec<u8>, BundleError> {
    encrypt_state_full(nodes, programs, None, &[], &[], passphrase)
}

/// Extended version of [`encrypt_state`] that also includes gateway identity,
/// phone PSKs, and handler routing configuration (GW-0805, GW-1001, GW-1203, GW-1210).
pub fn encrypt_state_full(
    nodes: &[NodeRecord],
    programs: &[ProgramRecord],
    identity: Option<&GatewayIdentity>,
    phone_psks: &[PhonePskRecord],
    handler_configs: &[HandlerConfig],
    passphrase: &str,
) -> Result<Vec<u8>, BundleError> {
    if passphrase.is_empty() {
        return Err(BundleError::EmptyPassphrase);
    }

    let plaintext = Zeroizing::new(encode_cbor(
        nodes,
        programs,
        identity,
        phone_psks,
        handler_configs,
    )?);

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
    let full = decrypt_state_full(bundle, passphrase)?;
    Ok((full.0, full.1))
}

/// Extended version of [`decrypt_state`] that also returns gateway identity
/// and phone PSKs if present in the bundle.
pub fn decrypt_state_full(bundle: &[u8], passphrase: &str) -> Result<FullState, BundleError> {
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

fn encode_cbor(
    nodes: &[NodeRecord],
    programs: &[ProgramRecord],
    identity: Option<&GatewayIdentity>,
    phone_psks: &[PhonePskRecord],
    handler_configs: &[HandlerConfig],
) -> Result<Vec<u8>, BundleError> {
    use ciborium::value::Value;

    let node_values: Vec<Value> = nodes.iter().map(node_to_cbor).collect();
    let program_values: Vec<Value> = programs.iter().map(program_to_cbor).collect();

    let mut root_entries = vec![
        (
            Value::Integer(ROOT_KEY_VERSION.into()),
            Value::Integer(FORMAT_VERSION.into()),
        ),
        (
            Value::Integer(ROOT_KEY_NODES.into()),
            Value::Array(node_values),
        ),
        (
            Value::Integer(ROOT_KEY_PROGRAMS.into()),
            Value::Array(program_values),
        ),
    ];

    if let Some(id) = identity {
        root_entries.push((
            Value::Integer(ROOT_KEY_IDENTITY.into()),
            identity_to_cbor(id),
        ));
    }

    if !phone_psks.is_empty() {
        let phone_values: Vec<Value> = phone_psks.iter().map(phone_psk_to_cbor).collect();
        root_entries.push((
            Value::Integer(ROOT_KEY_PHONE_PSKS.into()),
            Value::Array(phone_values),
        ));
    }

    if !handler_configs.is_empty() {
        let handler_values: Vec<Value> =
            handler_configs.iter().map(handler_config_to_cbor).collect();
        root_entries.push((
            Value::Integer(ROOT_KEY_HANDLERS.into()),
            Value::Array(handler_values),
        ));
    }

    let mut root = Value::Map(root_entries);

    let mut buf = Vec::new();
    let result =
        ciborium::ser::into_writer(&root, &mut buf).map_err(|e| BundleError::Encode(e.to_string()));
    // Zeroize PSK / image byte buffers inside the Value tree before drop.
    zeroize_cbor_bytes(&mut root);
    result?;
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
        (
            Value::Integer(NODE_KEY_ID.into()),
            Value::Text(n.node_id.clone()),
        ),
        (
            Value::Integer(NODE_KEY_HINT.into()),
            Value::Integer(n.key_hint.into()),
        ),
        (
            Value::Integer(NODE_KEY_PSK.into()),
            Value::Bytes(n.psk.to_vec()),
        ),
        (
            Value::Integer(NODE_KEY_ASSIGNED_HASH.into()),
            opt_bytes_val(&n.assigned_program_hash),
        ),
        (
            Value::Integer(NODE_KEY_CURRENT_HASH.into()),
            opt_bytes_val(&n.current_program_hash),
        ),
        (
            Value::Integer(NODE_KEY_SCHEDULE.into()),
            Value::Integer(n.schedule_interval_s.into()),
        ),
        (
            Value::Integer(NODE_KEY_FW_ABI.into()),
            opt_u32_val(n.firmware_abi_version),
        ),
        (
            Value::Integer(NODE_KEY_BATTERY.into()),
            opt_u32_val(n.last_battery_mv),
        ),
        (
            Value::Integer(NODE_KEY_LAST_SEEN.into()),
            opt_i64_val(last_seen_s),
        ),
        (
            Value::Integer(NODE_KEY_RF_CHANNEL.into()),
            match n.rf_channel {
                Some(ch) => Value::Integer(ch.into()),
                None => Value::Null,
            },
        ),
        (
            Value::Integer(NODE_KEY_SENSORS.into()),
            if n.sensors.is_empty() {
                Value::Null
            } else {
                Value::Array(
                    n.sensors
                        .iter()
                        .map(|s| {
                            let mut entries = vec![
                                (
                                    Value::Integer(1.into()),
                                    Value::Integer(s.sensor_type.into()),
                                ),
                                (Value::Integer(2.into()), Value::Integer(s.sensor_id.into())),
                            ];
                            if let Some(ref label) = s.label {
                                entries
                                    .push((Value::Integer(3.into()), Value::Text(label.clone())));
                            }
                            Value::Map(entries)
                        })
                        .collect(),
                )
            },
        ),
        (
            Value::Integer(NODE_KEY_REGISTERED_BY.into()),
            match n.registered_by_phone_id {
                Some(id) => Value::Integer(id.into()),
                None => Value::Null,
            },
        ),
        (
            Value::Integer(NODE_KEY_BATTERY_HISTORY.into()),
            if n.battery_history.is_empty() {
                Value::Null
            } else {
                Value::Array(
                    n.battery_history
                        .iter()
                        .map(|r| {
                            let ts = r
                                .timestamp
                                .duration_since(UNIX_EPOCH)
                                .ok()
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or(0);
                            Value::Array(vec![
                                Value::Integer(ts.into()),
                                Value::Integer(r.battery_mv.into()),
                            ])
                        })
                        .collect(),
                )
            },
        ),
    ])
}

fn program_to_cbor(p: &ProgramRecord) -> ciborium::value::Value {
    use ciborium::value::Value;

    let profile_u8: u8 = match p.verification_profile {
        VerificationProfile::Resident => 1,
        VerificationProfile::Ephemeral => 2,
    };

    Value::Map(vec![
        (
            Value::Integer(PROG_KEY_HASH.into()),
            Value::Bytes(p.hash.clone()),
        ),
        (
            Value::Integer(PROG_KEY_IMAGE.into()),
            Value::Bytes(p.image.clone()),
        ),
        (
            Value::Integer(PROG_KEY_SIZE.into()),
            Value::Integer(p.size.into()),
        ),
        (
            Value::Integer(PROG_KEY_PROFILE.into()),
            Value::Integer(profile_u8.into()),
        ),
        (
            Value::Integer(PROG_KEY_ABI.into()),
            opt_u32_val(p.abi_version),
        ),
    ])
}

fn identity_to_cbor(id: &GatewayIdentity) -> ciborium::value::Value {
    use ciborium::value::Value;

    Value::Map(vec![
        (
            Value::Integer(IDENT_KEY_SEED.into()),
            Value::Bytes(id.seed().to_vec()),
        ),
        (
            Value::Integer(IDENT_KEY_GATEWAY_ID.into()),
            Value::Bytes(id.gateway_id().to_vec()),
        ),
    ])
}

fn phone_psk_to_cbor(p: &PhonePskRecord) -> ciborium::value::Value {
    use ciborium::value::Value;

    let issued_at_s: i64 = p
        .issued_at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);

    Value::Map(vec![
        (
            Value::Integer(PHONE_KEY_HINT.into()),
            Value::Integer(p.phone_key_hint.into()),
        ),
        (
            Value::Integer(PHONE_KEY_PSK.into()),
            Value::Bytes(p.psk.to_vec()),
        ),
        (
            Value::Integer(PHONE_KEY_LABEL.into()),
            Value::Text(p.label.clone()),
        ),
        (
            Value::Integer(PHONE_KEY_ISSUED_AT.into()),
            Value::Integer(issued_at_s.into()),
        ),
        (
            Value::Integer(PHONE_KEY_STATUS.into()),
            Value::Text(p.status.to_string()),
        ),
    ])
}

fn handler_config_to_cbor(h: &HandlerConfig) -> ciborium::value::Value {
    use ciborium::value::Value;
    use std::fmt::Write;

    let matcher_values: Vec<Value> = h
        .matchers
        .iter()
        .map(|m| match m {
            ProgramMatcher::Any => Value::Text("*".into()),
            ProgramMatcher::Hash(bytes) => {
                let mut s = String::with_capacity(bytes.len() * 2);
                for b in bytes {
                    let _ = write!(s, "{b:02x}");
                }
                Value::Text(s)
            }
        })
        .collect();

    let mut entries = vec![
        (
            Value::Integer(HANDLER_KEY_MATCHERS.into()),
            Value::Array(matcher_values),
        ),
        (
            Value::Integer(HANDLER_KEY_COMMAND.into()),
            Value::Text(h.command.clone()),
        ),
    ];

    if !h.args.is_empty() {
        entries.push((
            Value::Integer(HANDLER_KEY_ARGS.into()),
            Value::Array(h.args.iter().map(|a| Value::Text(a.clone())).collect()),
        ));
    }

    Value::Map(entries)
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

// ── CBOR zeroization ──────────────────────────────────────────────────────────

/// Recursively zeroize all `Bytes` buffers in a ciborium `Value` tree so that
/// key material (PSKs, program images) is not left in freed heap memory.
fn zeroize_cbor_bytes(v: &mut ciborium::value::Value) {
    use ciborium::value::Value;
    use zeroize::Zeroize;
    match v {
        Value::Bytes(b) => b.zeroize(),
        Value::Array(arr) => {
            for item in arr {
                zeroize_cbor_bytes(item);
            }
        }
        Value::Map(pairs) => {
            for (k, val) in pairs {
                zeroize_cbor_bytes(k);
                zeroize_cbor_bytes(val);
            }
        }
        _ => {}
    }
}

// ── CBOR decoding ─────────────────────────────────────────────────────────────

fn decode_cbor(data: &[u8]) -> Result<FullState, BundleError> {
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
    let mut identity_opt: Option<Value> = None;
    let mut phone_psks_opt: Option<Vec<Value>> = None;
    let mut handlers_opt: Option<Vec<Value>> = None;

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(ROOT_KEY_VERSION) => {
                    version_opt = Some(match v {
                        Value::Integer(i) => u32::try_from(i)
                            .map_err(|_| BundleError::Decode("version out of range".into()))?,
                        _ => return Err(BundleError::Decode("version must be integer".into())),
                    });
                }
                Some(ROOT_KEY_NODES) => {
                    nodes_opt = Some(match v {
                        Value::Array(a) => a,
                        _ => return Err(BundleError::Decode("nodes must be array".into())),
                    });
                }
                Some(ROOT_KEY_PROGRAMS) => {
                    programs_opt = Some(match v {
                        Value::Array(a) => a,
                        _ => return Err(BundleError::Decode("programs must be array".into())),
                    });
                }
                Some(ROOT_KEY_IDENTITY) => {
                    identity_opt = Some(v);
                }
                Some(ROOT_KEY_PHONE_PSKS) => {
                    phone_psks_opt = Some(match v {
                        Value::Array(a) => a,
                        _ => return Err(BundleError::Decode("phone_psks must be array".into())),
                    });
                }
                Some(ROOT_KEY_HANDLERS) => {
                    handlers_opt = Some(match v {
                        Value::Array(a) => a,
                        _ => return Err(BundleError::Decode("handlers must be array".into())),
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

    let identity = identity_opt.map(identity_from_cbor).transpose()?;

    let phone_psks = phone_psks_opt
        .unwrap_or_default()
        .into_iter()
        .map(phone_psk_from_cbor)
        .collect::<Result<Vec<_>, _>>()?;

    let handler_configs = handlers_opt
        .unwrap_or_default()
        .into_iter()
        .map(handler_config_from_cbor)
        .collect::<Result<Vec<_>, _>>()?;

    Ok((nodes, programs, identity, phone_psks, handler_configs))
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
    let mut rf_channel: Option<u8> = None;
    let mut sensors: Vec<SensorDescriptor> = Vec::new();
    let mut registered_by_phone_id: Option<u32> = None;
    let mut battery_history: Vec<BatteryReading> = Vec::new();

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(NODE_KEY_ID) => {
                    node_id = Some(match v {
                        Value::Text(s) => s,
                        _ => return Err(BundleError::Decode("node_id must be text".into())),
                    });
                }
                Some(NODE_KEY_HINT) => {
                    key_hint = Some(match v {
                        Value::Integer(i) => u16::try_from(i)
                            .map_err(|_| BundleError::Decode("key_hint out of range".into()))?,
                        _ => return Err(BundleError::Decode("key_hint must be integer".into())),
                    });
                }
                Some(NODE_KEY_PSK) => {
                    psk = Some(match v {
                        Value::Bytes(mut b) => {
                            use zeroize::Zeroize;
                            if b.len() != 32 {
                                b.zeroize();
                                return Err(BundleError::Decode("psk must be 32 bytes".into()));
                            }
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&b);
                            b.zeroize();
                            arr
                        }
                        _ => return Err(BundleError::Decode("psk must be bytes".into())),
                    });
                }
                Some(NODE_KEY_ASSIGNED_HASH) => {
                    assigned_program_hash = Some(opt_bytes_from_cbor(v, "assigned_program_hash")?);
                }
                Some(NODE_KEY_CURRENT_HASH) => {
                    current_program_hash = Some(opt_bytes_from_cbor(v, "current_program_hash")?);
                }
                Some(NODE_KEY_SCHEDULE) => {
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
                Some(NODE_KEY_FW_ABI) => {
                    firmware_abi_version = Some(opt_u32_from_cbor(v, "firmware_abi_version")?);
                }
                Some(NODE_KEY_BATTERY) => {
                    last_battery_mv = Some(opt_u32_from_cbor(v, "last_battery_mv")?);
                }
                Some(NODE_KEY_LAST_SEEN) => {
                    last_seen_epoch_s = Some(opt_i64_from_cbor(v, "last_seen_epoch_s")?);
                }
                Some(NODE_KEY_RF_CHANNEL) => match v {
                    Value::Null => {}
                    Value::Integer(i) => {
                        rf_channel = Some(u8::try_from(i).map_err(|_| {
                            BundleError::Decode("rf_channel out of u8 range".into())
                        })?);
                    }
                    _ => {
                        return Err(BundleError::Decode(
                            "rf_channel must be integer or null".into(),
                        ))
                    }
                },
                Some(NODE_KEY_SENSORS) => {
                    if let Value::Array(arr) = v {
                        for item in arr {
                            if let Value::Map(sensor_map) = item {
                                let mut st: Option<u8> = None;
                                let mut si: Option<u8> = None;
                                let mut label: Option<String> = None;
                                for (sk, sv) in sensor_map {
                                    if let Value::Integer(skey) = sk {
                                        match i64::try_from(skey).ok() {
                                            Some(1) => {
                                                if let Value::Integer(i) = sv {
                                                    st = u8::try_from(i).ok();
                                                }
                                            }
                                            Some(2) => {
                                                if let Value::Integer(i) = sv {
                                                    si = u8::try_from(i).ok();
                                                }
                                            }
                                            Some(3) => {
                                                if let Value::Text(s) = sv {
                                                    label = Some(s);
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                if let (Some(sensor_type), Some(sensor_id)) = (st, si) {
                                    sensors.push(SensorDescriptor {
                                        sensor_type,
                                        sensor_id,
                                        label,
                                    });
                                }
                            }
                        }
                    }
                }
                Some(NODE_KEY_REGISTERED_BY) => match v {
                    Value::Null => {}
                    Value::Integer(i) => {
                        registered_by_phone_id = Some(u32::try_from(i).map_err(|_| {
                            BundleError::Decode("registered_by_phone_id out of u32 range".into())
                        })?);
                    }
                    _ => {
                        return Err(BundleError::Decode(
                            "registered_by_phone_id must be integer or null".into(),
                        ))
                    }
                },
                Some(NODE_KEY_BATTERY_HISTORY) => {
                    if let Value::Array(arr) = v {
                        // Cap imported history to the same limit used at runtime
                        // to avoid excessive memory usage from large/modified bundles.
                        const MAX_BATTERY_HISTORY: usize = 100;
                        let start = arr.len().saturating_sub(MAX_BATTERY_HISTORY);
                        for item in &arr[start..] {
                            if let Value::Array(pair) = item {
                                if pair.len() == 2 {
                                    if let (Value::Integer(ts_int), Value::Integer(mv_int)) =
                                        (&pair[0], &pair[1])
                                    {
                                        if let (Ok(ts), Ok(mv)) =
                                            (i64::try_from(*ts_int), u32::try_from(*mv_int))
                                        {
                                            if ts >= 0 {
                                                if let Some(t) = UNIX_EPOCH
                                                    .checked_add(Duration::from_secs(ts as u64))
                                                {
                                                    battery_history.push(BatteryReading {
                                                        timestamp: t,
                                                        battery_mv: mv,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
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
        rf_channel,
        sensors,
        registered_by_phone_id,
        battery_history,
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
                Some(PROG_KEY_HASH) => {
                    hash = Some(match v {
                        Value::Bytes(b) => b,
                        _ => return Err(BundleError::Decode("hash must be bytes".into())),
                    });
                }
                Some(PROG_KEY_IMAGE) => {
                    image = Some(match v {
                        Value::Bytes(b) => b,
                        _ => return Err(BundleError::Decode("image must be bytes".into())),
                    });
                }
                Some(PROG_KEY_SIZE) => {
                    size = Some(match v {
                        Value::Integer(i) => u32::try_from(i)
                            .map_err(|_| BundleError::Decode("size out of range".into()))?,
                        _ => return Err(BundleError::Decode("size must be integer".into())),
                    });
                }
                Some(PROG_KEY_PROFILE) => {
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
                Some(PROG_KEY_ABI) => {
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

fn identity_from_cbor(v: ciborium::value::Value) -> Result<GatewayIdentity, BundleError> {
    use ciborium::value::Value;

    let map = match v {
        Value::Map(m) => m,
        _ => return Err(BundleError::Decode("identity must be a CBOR map".into())),
    };

    let mut seed_opt: Option<[u8; 32]> = None;
    let mut gateway_id_opt: Option<[u8; 16]> = None;

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(IDENT_KEY_SEED) => {
                    seed_opt = Some(match v {
                        Value::Bytes(mut b) => {
                            use zeroize::Zeroize;
                            if b.len() != 32 {
                                b.zeroize();
                                return Err(BundleError::Decode("seed must be 32 bytes".into()));
                            }
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&b);
                            b.zeroize();
                            arr
                        }
                        _ => return Err(BundleError::Decode("seed must be bytes".into())),
                    });
                }
                Some(IDENT_KEY_GATEWAY_ID) => {
                    gateway_id_opt = Some(match v {
                        Value::Bytes(b) => {
                            if b.len() != 16 {
                                return Err(BundleError::Decode(
                                    "gateway_id must be 16 bytes".into(),
                                ));
                            }
                            let mut arr = [0u8; 16];
                            arr.copy_from_slice(&b);
                            arr
                        }
                        _ => return Err(BundleError::Decode("gateway_id must be bytes".into())),
                    });
                }
                _ => {}
            }
        }
    }

    let seed = seed_opt.ok_or_else(|| BundleError::Decode("missing seed in identity".into()))?;
    let gateway_id = gateway_id_opt
        .ok_or_else(|| BundleError::Decode("missing gateway_id in identity".into()))?;

    Ok(GatewayIdentity::from_parts(
        Zeroizing::new(seed),
        gateway_id,
    ))
}

fn phone_psk_from_cbor(v: ciborium::value::Value) -> Result<PhonePskRecord, BundleError> {
    use ciborium::value::Value;

    let map = match v {
        Value::Map(m) => m,
        _ => {
            return Err(BundleError::Decode(
                "phone_psk entry must be a CBOR map".into(),
            ))
        }
    };

    let mut hint_opt: Option<u16> = None;
    let mut psk_opt: Option<[u8; 32]> = None;
    let mut label_opt: Option<String> = None;
    let mut issued_at_opt: Option<i64> = None;
    let mut status_opt: Option<PhonePskStatus> = None;

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(PHONE_KEY_HINT) => {
                    hint_opt = Some(match v {
                        Value::Integer(i) => u16::try_from(i).map_err(|_| {
                            BundleError::Decode("phone_key_hint out of range".into())
                        })?,
                        _ => {
                            return Err(BundleError::Decode(
                                "phone_key_hint must be integer".into(),
                            ))
                        }
                    });
                }
                Some(PHONE_KEY_PSK) => {
                    psk_opt = Some(match v {
                        Value::Bytes(mut b) => {
                            use zeroize::Zeroize;
                            if b.len() != 32 {
                                b.zeroize();
                                return Err(BundleError::Decode(
                                    "phone psk must be 32 bytes".into(),
                                ));
                            }
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&b);
                            b.zeroize();
                            arr
                        }
                        _ => return Err(BundleError::Decode("phone psk must be bytes".into())),
                    });
                }
                Some(PHONE_KEY_LABEL) => {
                    label_opt = Some(match v {
                        Value::Text(s) => {
                            if s.len() > crate::phone_trust::PHONE_LABEL_MAX_BYTES {
                                return Err(BundleError::Decode(format!(
                                    "phone label exceeds {}-byte limit: {} bytes",
                                    crate::phone_trust::PHONE_LABEL_MAX_BYTES,
                                    s.len()
                                )));
                            }
                            s
                        }
                        _ => return Err(BundleError::Decode("label must be text".into())),
                    });
                }
                Some(PHONE_KEY_ISSUED_AT) => {
                    issued_at_opt = Some(match v {
                        Value::Integer(i) => i64::try_from(i)
                            .map_err(|_| BundleError::Decode("issued_at out of range".into()))?,
                        _ => return Err(BundleError::Decode("issued_at must be integer".into())),
                    });
                }
                Some(PHONE_KEY_STATUS) => {
                    status_opt = Some(match v {
                        Value::Text(s) => PhonePskStatus::from_str_value(&s).ok_or_else(|| {
                            BundleError::Decode(format!("unknown phone psk status: {s}"))
                        })?,
                        _ => return Err(BundleError::Decode("status must be text".into())),
                    });
                }
                _ => {}
            }
        }
    }

    let phone_key_hint =
        hint_opt.ok_or_else(|| BundleError::Decode("missing phone_key_hint".into()))?;
    let psk = psk_opt.ok_or_else(|| BundleError::Decode("missing phone psk".into()))?;
    let label = label_opt.ok_or_else(|| BundleError::Decode("missing phone label".into()))?;
    let issued_at_s =
        issued_at_opt.ok_or_else(|| BundleError::Decode("missing phone issued_at".into()))?;
    let status = status_opt.ok_or_else(|| BundleError::Decode("missing phone status".into()))?;

    let issued_at = if issued_at_s >= 0 {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(issued_at_s as u64))
            .ok_or_else(|| BundleError::Decode("issued_at overflows SystemTime".into()))?
    } else {
        UNIX_EPOCH
    };

    Ok(PhonePskRecord {
        phone_id: 0, // assigned by storage on import
        phone_key_hint,
        psk: Zeroizing::new(psk),
        label,
        issued_at,
        status,
    })
}

/// Parse a hex string into bytes.
fn parse_hex_str(s: &str) -> Result<Vec<u8>, String> {
    if !s.is_ascii() {
        return Err(format!("hex string contains non-ASCII characters: {s}"));
    }
    if !s.len().is_multiple_of(2) {
        return Err(format!("hex string has odd length: {s}"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| format!("invalid hex character in: {s}"))
        })
        .collect()
}

fn handler_config_from_cbor(v: ciborium::value::Value) -> Result<HandlerConfig, BundleError> {
    use ciborium::value::Value;

    let map = match v {
        Value::Map(m) => m,
        _ => {
            return Err(BundleError::Decode(
                "handler entry must be a CBOR map".into(),
            ))
        }
    };

    let mut matchers: Vec<ProgramMatcher> = Vec::new();
    let mut command: Option<String> = None;
    let mut args: Vec<String> = Vec::new();

    for (k, v) in map {
        if let Value::Integer(key_int) = k {
            match i64::try_from(key_int).ok() {
                Some(HANDLER_KEY_MATCHERS) => {
                    if let Value::Array(arr) = v {
                        for item in arr {
                            if let Value::Text(s) = item {
                                if s == "*" {
                                    matchers.push(ProgramMatcher::Any);
                                } else {
                                    let bytes = parse_hex_str(&s).map_err(|e| {
                                        BundleError::Decode(format!(
                                            "invalid hex in handler matcher: {e}"
                                        ))
                                    })?;
                                    if bytes.len() != 32 {
                                        return Err(BundleError::Decode(format!(
                                            "handler matcher hash must be 32 bytes, got {}",
                                            bytes.len()
                                        )));
                                    }
                                    matchers.push(ProgramMatcher::Hash(bytes));
                                }
                            }
                        }
                    }
                }
                Some(HANDLER_KEY_COMMAND) => {
                    command = Some(match v {
                        Value::Text(s) => s,
                        _ => {
                            return Err(BundleError::Decode("handler command must be text".into()))
                        }
                    });
                }
                Some(HANDLER_KEY_ARGS) => {
                    if let Value::Array(arr) = v {
                        for item in arr {
                            if let Value::Text(s) = item {
                                args.push(s);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let command =
        command.ok_or_else(|| BundleError::Decode("missing command in handler entry".into()))?;

    Ok(HandlerConfig {
        matchers,
        command,
        args,
    })
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

    #[test]
    fn roundtrip_abi_version_fields() {
        let mut node = make_node("abi-node", 0x0099);
        node.firmware_abi_version = Some(3);

        let prog = ProgramRecord {
            hash: vec![0xCC; 32],
            image: vec![0xDD; 48],
            size: 48,
            verification_profile: VerificationProfile::Ephemeral,
            abi_version: Some(2),
        };

        let bundle = encrypt_state(&[node], &[prog], "abi-pass").unwrap();
        let (out_nodes, out_programs) = decrypt_state(&bundle, "abi-pass").unwrap();

        assert_eq!(out_nodes[0].firmware_abi_version, Some(3));
        assert_eq!(out_programs[0].abi_version, Some(2));
    }

    #[test]
    fn roundtrip_identity_and_phone_psks() {
        use crate::gateway_identity::GatewayIdentity;
        use crate::phone_trust::{PhonePskRecord, PhonePskStatus};

        let identity = GatewayIdentity::generate().unwrap();
        let phone = PhonePskRecord {
            phone_id: 0,
            phone_key_hint: 0x1234,
            psk: Zeroizing::new([0xDD; 32]),
            label: "Test Phone".to_string(),
            issued_at: UNIX_EPOCH + Duration::from_secs(1700000000),
            status: PhonePskStatus::Active,
        };
        let phone_revoked = PhonePskRecord {
            phone_id: 0,
            phone_key_hint: 0x5678,
            psk: Zeroizing::new([0xEE; 32]),
            label: "Revoked Phone".to_string(),
            issued_at: UNIX_EPOCH + Duration::from_secs(1700001000),
            status: PhonePskStatus::Revoked,
        };

        let bundle = encrypt_state_full(
            &[],
            &[],
            Some(&identity),
            &[phone.clone(), phone_revoked.clone()],
            &[],
            "id-pass",
        )
        .unwrap();

        let (nodes, programs, loaded_id, loaded_phones, loaded_handlers) =
            decrypt_state_full(&bundle, "id-pass").unwrap();

        assert!(nodes.is_empty());
        assert!(programs.is_empty());

        // Identity round-trips.
        let loaded_id = loaded_id.unwrap();
        assert_eq!(loaded_id.public_key(), identity.public_key());
        assert_eq!(loaded_id.gateway_id(), identity.gateway_id());
        assert_eq!(loaded_id.seed(), identity.seed());

        // Phone PSKs round-trip.
        assert_eq!(loaded_phones.len(), 2);
        assert_eq!(loaded_phones[0].phone_key_hint, 0x1234);
        assert_eq!(*loaded_phones[0].psk, [0xDD; 32]);
        assert_eq!(loaded_phones[0].label, "Test Phone");
        assert_eq!(loaded_phones[0].status, PhonePskStatus::Active);
        assert_eq!(loaded_phones[1].phone_key_hint, 0x5678);
        assert_eq!(loaded_phones[1].status, PhonePskStatus::Revoked);
        assert!(loaded_handlers.is_empty());
    }

    #[test]
    fn backward_compatible_bundle_without_identity() {
        // A bundle without identity/phone PSKs/handlers (old format) still decodes.
        let bundle = encrypt_state(&[], &[], "compat").unwrap();
        let (_, _, identity, phones, handlers) = decrypt_state_full(&bundle, "compat").unwrap();
        assert!(identity.is_none());
        assert!(phones.is_empty());
        assert!(handlers.is_empty());
    }

    #[test]
    fn roundtrip_handler_configs() {
        use crate::handler::{HandlerConfig, ProgramMatcher};

        let hash = vec![0x42u8; 32];
        let configs = vec![
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(hash.clone())],
                command: "/usr/bin/handler".to_string(),
                args: vec!["--verbose".to_string()],
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Any],
                command: "/usr/bin/catch-all".to_string(),
                args: Vec::new(),
            },
        ];

        let bundle = encrypt_state_full(&[], &[], None, &[], &configs, "handler-pass").unwrap();
        let (_, _, _, _, loaded) = decrypt_state_full(&bundle, "handler-pass").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].command, "/usr/bin/handler");
        assert_eq!(loaded[0].args, vec!["--verbose"]);
        assert_eq!(loaded[0].matchers.len(), 1);
        assert!(matches!(&loaded[0].matchers[0], ProgramMatcher::Hash(h) if *h == hash));

        assert_eq!(loaded[1].command, "/usr/bin/catch-all");
        assert!(loaded[1].args.is_empty());
        assert!(matches!(loaded[1].matchers[0], ProgramMatcher::Any));
    }

    #[test]
    fn roundtrip_battery_history() {
        use crate::registry::BatteryReading;
        use std::time::{Duration, UNIX_EPOCH};

        let mut node = make_node("batt-node", 0xAAAA);
        node.battery_history = vec![
            BatteryReading {
                timestamp: UNIX_EPOCH + Duration::from_secs(1700000000),
                battery_mv: 3300,
            },
            BatteryReading {
                timestamp: UNIX_EPOCH + Duration::from_secs(1700001000),
                battery_mv: 3250,
            },
        ];

        let bundle = encrypt_state(&[node], &[], "batt-pass").unwrap();
        let (nodes, _) = decrypt_state(&bundle, "batt-pass").unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].battery_history.len(), 2);
        assert_eq!(nodes[0].battery_history[0].battery_mv, 3300);
        assert_eq!(nodes[0].battery_history[1].battery_mv, 3250);
    }
}
