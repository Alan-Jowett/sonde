// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Escrow blob format for encrypted PSK key escrow (GW-2002).
//!
//! Each escrow blob wraps one PSK encrypted with AES-256-GCM. The identity
//! fields (escrow_version, key_version, subject_kind, subject_id, key_hint)
//! are authenticated as AAD to prevent blob swap attacks.
//!
//! Wire format: CBOR map with integer keys 1–8 per the specification.

use alloc::string::String;
use alloc::vec::Vec;

use ciborium::Value;

use crate::constants::*;
use crate::error::{DecodeError, EncodeError};
use crate::traits::AeadProvider;

/// Identifies the subject of an escrow blob.
#[derive(Debug, Clone, PartialEq)]
pub enum SubjectKind {
    Node,
    Phone,
}

impl SubjectKind {
    /// CBOR wire encoding: `"node"` or `"phone"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            SubjectKind::Node => "node",
            SubjectKind::Phone => "phone",
        }
    }

    /// Parse from CBOR wire encoding.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<SubjectKind> {
        match s {
            "node" => Some(SubjectKind::Node),
            "phone" => Some(SubjectKind::Phone),
            _ => None,
        }
    }
}

/// An encrypted PSK escrow blob.
///
/// The identity fields (escrow_version through key_hint) form the AAD
/// that binds the ciphertext to a specific subject, preventing blob
/// swap attacks between different nodes/phones.
#[derive(Debug, Clone, PartialEq)]
pub struct EscrowBlob {
    /// Schema version (currently 1).
    pub escrow_version: u8,
    /// Master key version that encrypted this blob.
    pub key_version: u64,
    /// Subject type.
    pub subject_kind: SubjectKind,
    /// Node ID or phone ID.
    pub subject_id: String,
    /// PSK key_hint value.
    pub key_hint: u16,
    /// AES-256-GCM nonce (12 bytes).
    pub nonce: [u8; 12],
    /// Encrypted PSK (32 bytes).
    pub ciphertext: [u8; 32],
    /// AES-256-GCM authentication tag (16 bytes).
    pub tag: [u8; 16],
}

/// Build the deterministic AAD for an escrow blob from its identity fields.
///
/// The AAD is the deterministic CBOR encoding (RFC 8949 §4.2) of a map
/// containing fields 1–5 (escrow_version, key_version, subject_kind,
/// subject_id, key_hint). Sorted integer keys, definite-length encoding.
pub fn build_escrow_aad(
    escrow_version: u8,
    key_version: u64,
    subject_kind: &SubjectKind,
    subject_id: &str,
    key_hint: u16,
) -> Result<Vec<u8>, EncodeError> {
    let pairs: Vec<(Value, Value)> = alloc::vec![
        (
            Value::Integer(ESCROW_BLOB_KEY_VERSION_FIELD.into()),
            Value::Integer((escrow_version as u64).into()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_KEY_VERSION.into()),
            Value::Integer(key_version.into()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_SUBJECT_KIND.into()),
            Value::Text(String::from(subject_kind.as_str())),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_SUBJECT_ID.into()),
            Value::Text(String::from(subject_id)),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_KEY_HINT.into()),
            Value::Integer((key_hint as u64).into()),
        ),
    ];
    let value = Value::Map(pairs);
    let mut buf = Vec::new();
    ciborium::into_writer(&value, &mut buf)
        .map_err(|e| EncodeError::CborError(alloc::format!("{e}")))?;
    Ok(buf)
}

/// Encrypt a raw PSK into an escrow blob.
///
/// The `master_key` is used as the AES-256-GCM key. The `nonce` must be
/// 12 bytes of cryptographic randomness (generated via `getrandom::fill()`
/// by the caller).
#[allow(clippy::too_many_arguments)]
pub fn seal_escrow_blob(
    master_key: &[u8; 32],
    nonce: &[u8; 12],
    escrow_version: u8,
    key_version: u64,
    subject_kind: &SubjectKind,
    subject_id: &str,
    key_hint: u16,
    psk: &[u8; 32],
    aead: &(impl AeadProvider + ?Sized),
) -> Result<EscrowBlob, EncodeError> {
    let aad = build_escrow_aad(escrow_version, key_version, subject_kind, subject_id, key_hint)?;

    let ciphertext_and_tag = aead.seal(master_key, nonce, &aad, psk);

    // AES-256-GCM output: 32 bytes ciphertext + 16 bytes tag = 48 bytes
    if ciphertext_and_tag.len() != 48 {
        return Err(EncodeError::CborError(alloc::format!(
            "unexpected AEAD output length: {} (expected 48)",
            ciphertext_and_tag.len()
        )));
    }

    let mut ciphertext = [0u8; 32];
    ciphertext.copy_from_slice(&ciphertext_and_tag[..32]);
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&ciphertext_and_tag[32..]);

    Ok(EscrowBlob {
        escrow_version,
        key_version,
        subject_kind: subject_kind.clone(),
        subject_id: String::from(subject_id),
        key_hint,
        nonce: *nonce,
        ciphertext,
        tag,
    })
}

/// Decrypt an escrow blob to recover the raw PSK.
///
/// Returns the 32-byte PSK on success, or `None` if authentication fails
/// (wrong master key, tampered AAD, or corrupted ciphertext).
pub fn open_escrow_blob(
    blob: &EscrowBlob,
    master_key: &[u8; 32],
    aead: &(impl AeadProvider + ?Sized),
) -> Result<Option<[u8; 32]>, DecodeError> {
    let aad = build_escrow_aad(
        blob.escrow_version,
        blob.key_version,
        &blob.subject_kind,
        &blob.subject_id,
        blob.key_hint,
    )
    .map_err(|e| DecodeError::CborError(alloc::format!("AAD encoding error: {e}")))?;

    let mut ciphertext_and_tag = Vec::with_capacity(48);
    ciphertext_and_tag.extend_from_slice(&blob.ciphertext);
    ciphertext_and_tag.extend_from_slice(&blob.tag);

    match aead.open(master_key, &blob.nonce, &aad, &ciphertext_and_tag) {
        Some(plaintext) => {
            if plaintext.len() != 32 {
                return Err(DecodeError::CborError(alloc::format!(
                    "unexpected PSK length: {} (expected 32)",
                    plaintext.len()
                )));
            }
            let mut psk = [0u8; 32];
            psk.copy_from_slice(&plaintext);
            Ok(Some(psk))
        }
        None => Ok(None),
    }
}

/// Encode an escrow blob to CBOR bytes.
///
/// Uses integer keys 1–8 as defined in GW-2002.
pub fn encode_escrow_blob(blob: &EscrowBlob) -> Result<Vec<u8>, EncodeError> {
    let pairs: Vec<(Value, Value)> = alloc::vec![
        (
            Value::Integer(ESCROW_BLOB_KEY_VERSION_FIELD.into()),
            Value::Integer((blob.escrow_version as u64).into()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_KEY_VERSION.into()),
            Value::Integer(blob.key_version.into()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_SUBJECT_KIND.into()),
            Value::Text(String::from(blob.subject_kind.as_str())),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_SUBJECT_ID.into()),
            Value::Text(blob.subject_id.clone()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_KEY_HINT.into()),
            Value::Integer((blob.key_hint as u64).into()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_NONCE.into()),
            Value::Bytes(blob.nonce.to_vec()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_CIPHERTEXT.into()),
            Value::Bytes(blob.ciphertext.to_vec()),
        ),
        (
            Value::Integer(ESCROW_BLOB_KEY_TAG.into()),
            Value::Bytes(blob.tag.to_vec()),
        ),
    ];
    let value = Value::Map(pairs);
    let mut buf = Vec::new();
    ciborium::into_writer(&value, &mut buf)
        .map_err(|e| EncodeError::CborError(alloc::format!("{e}")))?;
    Ok(buf)
}

/// Decode an escrow blob from CBOR bytes.
pub fn decode_escrow_blob(cbor: &[u8]) -> Result<EscrowBlob, DecodeError> {
    let value: Value =
        ciborium::from_reader(cbor).map_err(|e| DecodeError::CborError(alloc::format!("{e}")))?;
    let pairs = match value {
        Value::Map(pairs) => pairs,
        _ => return Err(DecodeError::CborError(String::from("expected CBOR map"))),
    };

    let mut escrow_version: Option<u8> = None;
    let mut key_version: Option<u64> = None;
    let mut subject_kind: Option<SubjectKind> = None;
    let mut subject_id: Option<String> = None;
    let mut key_hint: Option<u16> = None;
    let mut nonce: Option<[u8; 12]> = None;
    let mut ciphertext: Option<[u8; 32]> = None;
    let mut tag: Option<[u8; 16]> = None;

    for (k, v) in pairs {
        let key = match k {
            Value::Integer(i) => {
                let val: i128 = i.into();
                val as u64
            }
            _ => continue,
        };
        match key {
            ESCROW_BLOB_KEY_VERSION_FIELD => {
                if let Value::Integer(i) = v {
                    let val: i128 = i.into();
                    escrow_version = u8::try_from(val).ok();
                }
            }
            ESCROW_BLOB_KEY_KEY_VERSION => {
                if let Value::Integer(i) = v {
                    let val: i128 = i.into();
                    key_version = u64::try_from(val).ok();
                }
            }
            ESCROW_BLOB_KEY_SUBJECT_KIND => {
                if let Value::Text(s) = v {
                    subject_kind = SubjectKind::from_str(&s);
                }
            }
            ESCROW_BLOB_KEY_SUBJECT_ID => {
                if let Value::Text(s) = v {
                    subject_id = Some(s);
                }
            }
            ESCROW_BLOB_KEY_KEY_HINT => {
                if let Value::Integer(i) = v {
                    let val: i128 = i.into();
                    key_hint = u16::try_from(val).ok();
                }
            }
            ESCROW_BLOB_KEY_NONCE => {
                if let Value::Bytes(b) = v {
                    if b.len() == 12 {
                        let mut arr = [0u8; 12];
                        arr.copy_from_slice(&b);
                        nonce = Some(arr);
                    }
                }
            }
            ESCROW_BLOB_KEY_CIPHERTEXT => {
                if let Value::Bytes(b) = v {
                    if b.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&b);
                        ciphertext = Some(arr);
                    }
                }
            }
            ESCROW_BLOB_KEY_TAG => {
                if let Value::Bytes(b) = v {
                    if b.len() == 16 {
                        let mut arr = [0u8; 16];
                        arr.copy_from_slice(&b);
                        tag = Some(arr);
                    }
                }
            }
            _ => {} // ignore unknown keys
        }
    }

    Ok(EscrowBlob {
        escrow_version: escrow_version
            .ok_or_else(|| DecodeError::CborError(String::from("missing escrow_version")))?,
        key_version: key_version
            .ok_or_else(|| DecodeError::CborError(String::from("missing key_version")))?,
        subject_kind: subject_kind
            .ok_or_else(|| DecodeError::CborError(String::from("missing or invalid subject_kind")))?,
        subject_id: subject_id
            .ok_or_else(|| DecodeError::CborError(String::from("missing subject_id")))?,
        key_hint: key_hint
            .ok_or_else(|| DecodeError::CborError(String::from("missing key_hint")))?,
        nonce: nonce
            .ok_or_else(|| DecodeError::CborError(String::from("missing or invalid nonce")))?,
        ciphertext: ciphertext.ok_or_else(|| {
            DecodeError::CborError(String::from("missing or invalid ciphertext"))
        })?,
        tag: tag.ok_or_else(|| DecodeError::CborError(String::from("missing or invalid tag")))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AeadProvider;
    use alloc::vec;

    /// Stub AEAD that XORs plaintext with a key-derived byte and produces
    /// a key+AAD+nonce-dependent tag. Not cryptographically secure — test only.
    struct TestAead;

    fn compute_test_tag(key: &[u8; 32], aad: &[u8], nonce: &[u8; 12]) -> [u8; AEAD_TAG_SIZE] {
        let mut tag = [0u8; AEAD_TAG_SIZE];
        // Use wrapping_mul to break XOR cancellation from uniform keys
        for (i, &b) in key.iter().enumerate() {
            tag[i % AEAD_TAG_SIZE] = tag[i % AEAD_TAG_SIZE].wrapping_add(b.wrapping_mul((i as u8).wrapping_add(1)));
        }
        for (i, &b) in aad.iter().enumerate() {
            tag[i % AEAD_TAG_SIZE] ^= b.wrapping_add(i as u8);
        }
        for (i, &b) in nonce.iter().enumerate() {
            tag[i % AEAD_TAG_SIZE] ^= b;
        }
        tag
    }

    impl AeadProvider for TestAead {
        fn seal(
            &self,
            key: &[u8; 32],
            nonce: &[u8; 12],
            aad: &[u8],
            plaintext: &[u8],
        ) -> Vec<u8> {
            let mut ct = plaintext.to_vec();
            for b in &mut ct {
                *b ^= nonce[0];
            }
            let tag = compute_test_tag(key, aad, nonce);
            ct.extend_from_slice(&tag);
            ct
        }

        fn open(
            &self,
            key: &[u8; 32],
            nonce: &[u8; 12],
            aad: &[u8],
            ciphertext_and_tag: &[u8],
        ) -> Option<Vec<u8>> {
            if ciphertext_and_tag.len() < AEAD_TAG_SIZE {
                return None;
            }
            let ct_len = ciphertext_and_tag.len() - AEAD_TAG_SIZE;
            let received_tag = &ciphertext_and_tag[ct_len..];

            let expected_tag = compute_test_tag(key, aad, nonce);
            if received_tag != expected_tag {
                return None;
            }

            let mut pt = ciphertext_and_tag[..ct_len].to_vec();
            for b in &mut pt {
                *b ^= nonce[0];
            }
            Some(pt)
        }
    }

    // T-2002: Escrow blob format — round-trip
    #[test]
    fn test_escrow_blob_round_trip() {
        let master_key = [0x42u8; 32];
        let nonce = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let psk = [0xABu8; 32];
        let aead = TestAead;

        let blob = seal_escrow_blob(
            &master_key,
            &nonce,
            ESCROW_BLOB_VERSION,
            1,
            &SubjectKind::Node,
            "node-abc-123",
            0x1234,
            &psk,
            &aead,
        )
        .unwrap();

        // Verify fields
        assert_eq!(blob.escrow_version, ESCROW_BLOB_VERSION);
        assert_eq!(blob.key_version, 1);
        assert_eq!(blob.subject_kind, SubjectKind::Node);
        assert_eq!(blob.subject_id, "node-abc-123");
        assert_eq!(blob.key_hint, 0x1234);

        // Decrypt with correct key
        let recovered = open_escrow_blob(&blob, &master_key, &aead).unwrap();
        assert_eq!(recovered, Some(psk));
    }

    // T-2002: Wrong master key fails
    #[test]
    fn test_escrow_blob_wrong_key() {
        let master_key = [0x42u8; 32];
        let wrong_key = [0x99u8; 32];
        let nonce = [1u8; 12];
        let psk = [0xABu8; 32];
        let aead = TestAead;

        let blob = seal_escrow_blob(
            &master_key,
            &nonce,
            ESCROW_BLOB_VERSION,
            1,
            &SubjectKind::Node,
            "node-1",
            0x1234,
            &psk,
            &aead,
        )
        .unwrap();

        let result = open_escrow_blob(&blob, &wrong_key, &aead).unwrap();
        assert_eq!(result, None);
    }

    // T-2002: Tampered subject_id causes decryption failure
    #[test]
    fn test_escrow_blob_tampered_aad() {
        let master_key = [0x42u8; 32];
        let nonce = [2u8; 12];
        let psk = [0xABu8; 32];
        let aead = TestAead;

        let mut blob = seal_escrow_blob(
            &master_key,
            &nonce,
            ESCROW_BLOB_VERSION,
            1,
            &SubjectKind::Node,
            "node-1",
            0x1234,
            &psk,
            &aead,
        )
        .unwrap();

        // Tamper with subject_id
        blob.subject_id = String::from("node-2");
        let result = open_escrow_blob(&blob, &master_key, &aead).unwrap();
        assert_eq!(result, None, "tampered subject_id should fail decryption");
    }

    // T-2002: Swapping blobs between nodes fails
    #[test]
    fn test_escrow_blob_swap_attack() {
        let master_key = [0x42u8; 32];
        let psk_a = [0xAAu8; 32];
        let psk_b = [0xBBu8; 32];
        let aead = TestAead;

        let blob_a = seal_escrow_blob(
            &master_key,
            &[1u8; 12],
            ESCROW_BLOB_VERSION,
            1,
            &SubjectKind::Node,
            "node-a",
            0x1111,
            &psk_a,
            &aead,
        )
        .unwrap();

        let blob_b = seal_escrow_blob(
            &master_key,
            &[2u8; 12],
            ESCROW_BLOB_VERSION,
            1,
            &SubjectKind::Node,
            "node-b",
            0x2222,
            &psk_b,
            &aead,
        )
        .unwrap();

        // Take blob_a's ciphertext but blob_b's identity → should fail
        let swapped = EscrowBlob {
            escrow_version: blob_b.escrow_version,
            key_version: blob_b.key_version,
            subject_kind: blob_b.subject_kind.clone(),
            subject_id: blob_b.subject_id.clone(),
            key_hint: blob_b.key_hint,
            nonce: blob_a.nonce,
            ciphertext: blob_a.ciphertext,
            tag: blob_a.tag,
        };
        let result = open_escrow_blob(&swapped, &master_key, &aead).unwrap();
        assert_eq!(result, None, "swapped blob should fail decryption");
    }

    // T-2002: CBOR encode/decode round-trip
    #[test]
    fn test_escrow_blob_cbor_round_trip() {
        let master_key = [0x42u8; 32];
        let nonce = [3u8; 12];
        let psk = [0xCDu8; 32];
        let aead = TestAead;

        let blob = seal_escrow_blob(
            &master_key,
            &nonce,
            ESCROW_BLOB_VERSION,
            1,
            &SubjectKind::Phone,
            "phone-xyz",
            0x5678,
            &psk,
            &aead,
        )
        .unwrap();

        let encoded = encode_escrow_blob(&blob).unwrap();

        // Spec: ≤ 150 bytes
        assert!(
            encoded.len() <= 150,
            "encoded blob size {} exceeds 150-byte limit",
            encoded.len()
        );

        let decoded = decode_escrow_blob(&encoded).unwrap();
        assert_eq!(decoded, blob);

        // Verify decryption still works after CBOR round-trip
        let recovered = open_escrow_blob(&decoded, &master_key, &aead).unwrap();
        assert_eq!(recovered, Some(psk));
    }

    // SubjectKind string round-trip
    #[test]
    fn test_subject_kind_str() {
        assert_eq!(SubjectKind::Node.as_str(), "node");
        assert_eq!(SubjectKind::Phone.as_str(), "phone");
        assert_eq!(SubjectKind::from_str("node"), Some(SubjectKind::Node));
        assert_eq!(SubjectKind::from_str("phone"), Some(SubjectKind::Phone));
        assert_eq!(SubjectKind::from_str("other"), None);
    }

    // Out-of-range integers are rejected during decoding
    #[test]
    fn test_escrow_blob_decode_rejects_out_of_range_version() {
        // Manually build CBOR with escrow_version = 257 (> u8::MAX)
        let pairs: Vec<(Value, Value)> = alloc::vec![
            (Value::Integer(1u64.into()), Value::Integer(257u64.into())),
            (Value::Integer(2u64.into()), Value::Integer(1u64.into())),
            (Value::Integer(3u64.into()), Value::Text(String::from("node"))),
            (Value::Integer(4u64.into()), Value::Text(String::from("n1"))),
            (Value::Integer(5u64.into()), Value::Integer(0x1234u64.into())),
            (Value::Integer(6u64.into()), Value::Bytes(vec![0u8; 12])),
            (Value::Integer(7u64.into()), Value::Bytes(vec![0u8; 32])),
            (Value::Integer(8u64.into()), Value::Bytes(vec![0u8; 16])),
        ];
        let value = Value::Map(pairs);
        let mut buf = Vec::new();
        ciborium::into_writer(&value, &mut buf).unwrap();
        let result = decode_escrow_blob(&buf);
        assert!(result.is_err(), "escrow_version=257 should be rejected");
    }

    #[test]
    fn test_escrow_blob_decode_rejects_out_of_range_key_hint() {
        // Manually build CBOR with key_hint = 65536 (> u16::MAX)
        let pairs: Vec<(Value, Value)> = alloc::vec![
            (Value::Integer(1u64.into()), Value::Integer(1u64.into())),
            (Value::Integer(2u64.into()), Value::Integer(1u64.into())),
            (Value::Integer(3u64.into()), Value::Text(String::from("node"))),
            (Value::Integer(4u64.into()), Value::Text(String::from("n1"))),
            (Value::Integer(5u64.into()), Value::Integer(65536u64.into())),
            (Value::Integer(6u64.into()), Value::Bytes(vec![0u8; 12])),
            (Value::Integer(7u64.into()), Value::Bytes(vec![0u8; 32])),
            (Value::Integer(8u64.into()), Value::Bytes(vec![0u8; 16])),
        ];
        let value = Value::Map(pairs);
        let mut buf = Vec::new();
        ciborium::into_writer(&value, &mut buf).unwrap();
        let result = decode_escrow_blob(&buf);
        assert!(result.is_err(), "key_hint=65536 should be rejected");
    }

    #[test]
    fn test_escrow_blob_decode_rejects_negative_integers() {
        // Manually build CBOR with negative key_version (use i64 -1)
        use ciborium::value::Integer;
        let neg_one = Integer::from(-1i64);
        let pairs: Vec<(Value, Value)> = alloc::vec![
            (Value::Integer(1u64.into()), Value::Integer(1u64.into())),
            (Value::Integer(2u64.into()), Value::Integer(neg_one)),
            (Value::Integer(3u64.into()), Value::Text(String::from("node"))),
            (Value::Integer(4u64.into()), Value::Text(String::from("n1"))),
            (Value::Integer(5u64.into()), Value::Integer(0x1234u64.into())),
            (Value::Integer(6u64.into()), Value::Bytes(vec![0u8; 12])),
            (Value::Integer(7u64.into()), Value::Bytes(vec![0u8; 32])),
            (Value::Integer(8u64.into()), Value::Bytes(vec![0u8; 16])),
        ];
        let value = Value::Map(pairs);
        let mut buf = Vec::new();
        ciborium::into_writer(&value, &mut buf).unwrap();
        let result = decode_escrow_blob(&buf);
        assert!(result.is_err(), "negative key_version should be rejected");
    }
}
