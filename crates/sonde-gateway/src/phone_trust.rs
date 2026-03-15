// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phone PSK trust store record types (GW-1210).
//!
//! Each authorized phone receives a unique 256-bit PSK from the gateway.
//! The gateway stores PSKs alongside metadata for auditing and revocation.

use std::fmt;
use std::time::SystemTime;

use zeroize::Zeroizing;

/// Status of a phone PSK in the trust store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhonePskStatus {
    /// The phone PSK is active and can be used for pairing requests.
    Active,
    /// The phone PSK has been revoked; pairing requests signed with it
    /// are silently discarded.
    Revoked,
}

impl fmt::Display for PhonePskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PhonePskStatus::Active => write!(f, "active"),
            PhonePskStatus::Revoked => write!(f, "revoked"),
        }
    }
}

impl PhonePskStatus {
    /// Parse from a database string value.
    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "active" => Some(PhonePskStatus::Active),
            "revoked" => Some(PhonePskStatus::Revoked),
            _ => None,
        }
    }
}

/// A phone PSK record in the gateway's trust store (GW-1210).
#[derive(Debug, Clone)]
pub struct PhonePskRecord {
    /// Auto-increment primary key (stable identifier for audit).
    pub phone_id: u32,
    /// Lookup hint: `SHA-256(psk)[30..32]` as big-endian u16.
    /// Non-unique — collisions are possible.
    pub phone_key_hint: u16,
    /// The 256-bit phone PSK (zeroized on drop, encrypted at rest).
    pub psk: Zeroizing<[u8; 32]>,
    /// Human-readable label (UTF-8, max 64 bytes).
    pub label: String,
    /// When this PSK was issued.
    pub issued_at: SystemTime,
    /// Current status (active or revoked).
    pub status: PhonePskStatus,
}

/// Maximum label length in bytes (UTF-8).
pub const PHONE_LABEL_MAX_BYTES: usize = 64;
