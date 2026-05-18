// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! PSK key escrow subsystem (GW-2000–GW-2013).
//!
//! Manages the escrow lifecycle: keypair generation, master key rotation,
//! crash-safe PSK re-encryption, and unknown-node recovery.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use tracing::{debug, info, warn};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::storage::{EscrowKeypairRecord, Storage, StorageError};

/// 12-byte nonce + 32-byte ciphertext + 16-byte GCM tag = 60 bytes.
const ENCRYPTED_KEY_LEN: usize = 12 + 32 + 16;

/// Default recovery queue capacity.
const RECOVERY_QUEUE_CAPACITY: usize = 64;

/// Default recovery entry TTL.
const RECOVERY_ENTRY_TTL: Duration = Duration::from_secs(30);

/// Default rate limit: 1 request per key_hint per 60 seconds.
const RATE_LIMIT_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum candidates in a recovery response.
pub const MAX_RECOVERY_CANDIDATES: usize = 16;

/// Escrow keypair tuple: private key, public key, and monotonic epoch.
pub type EscrowKeypair = (Zeroizing<[u8; 32]>, [u8; 32], u64);

/// Escrow lifecycle state (GW-2004).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscrowState {
    /// No passphrase-derived key rotation has occurred.
    Disabled,
    /// First key rotation is in progress.
    Bootstrapping,
    /// All PSKs are encrypted with the current master key.
    Ready,
    /// A new key rotation is underway.
    RotationInProgress,
    /// A rotation was interrupted; gateway will auto-resume on startup.
    Degraded,
}

impl EscrowState {
    pub fn as_str(&self) -> &'static str {
        match self {
            EscrowState::Disabled => "disabled",
            EscrowState::Bootstrapping => "bootstrapping",
            EscrowState::Ready => "ready",
            EscrowState::RotationInProgress => "rotation_in_progress",
            EscrowState::Degraded => "degraded",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<EscrowState> {
        match s {
            "disabled" => Some(EscrowState::Disabled),
            "bootstrapping" => Some(EscrowState::Bootstrapping),
            "ready" => Some(EscrowState::Ready),
            "rotation_in_progress" => Some(EscrowState::RotationInProgress),
            "degraded" => Some(EscrowState::Degraded),
            _ => None,
        }
    }
}

/// A buffered frame awaiting recovery.
pub struct RecoveryEntry {
    pub key_hint: u16,
    pub raw_frame: Vec<u8>,
    pub peer_address: [u8; 6],
    pub created_at: Instant,
}

/// Bounded, rate-limited recovery queue (GW-2009, GW-2010).
pub struct RecoveryQueue {
    /// request_id → buffered entry
    entries: HashMap<[u8; 16], RecoveryEntry>,
    /// key_hint → last request timestamp (per-hint rate limiting)
    hint_rate: HashMap<u16, Instant>,
}

impl RecoveryQueue {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            hint_rate: HashMap::new(),
        }
    }

    /// Check whether a recovery request can be emitted for this key_hint.
    ///
    /// Returns `false` if:
    /// - Rate-limited (last request for this key_hint < 60s ago)
    /// - Queue is full (>= RECOVERY_QUEUE_CAPACITY entries)
    pub fn can_request(&self, key_hint: u16) -> bool {
        // Evict expired entries so stale entries don't block new requests.
        let active_count = self
            .entries
            .values()
            .filter(|e| e.created_at.elapsed() <= RECOVERY_ENTRY_TTL)
            .count();
        if active_count >= RECOVERY_QUEUE_CAPACITY {
            return false;
        }
        if let Some(&last) = self.hint_rate.get(&key_hint) {
            if last.elapsed() < RATE_LIMIT_INTERVAL {
                return false;
            }
        }
        true
    }

    /// Buffer a frame for recovery and generate a request_id.
    ///
    /// Returns `Ok(request_id)` or `Err` if the queue is full, the request is
    /// rate-limited, or the OS RNG fails.
    pub fn enqueue(
        &mut self,
        key_hint: u16,
        raw_frame: Vec<u8>,
        peer_address: [u8; 6],
    ) -> Result<[u8; 16], String> {
        self.evict_expired();
        if self.entries.len() >= RECOVERY_QUEUE_CAPACITY {
            return Err(format!(
                "recovery queue full (capacity {RECOVERY_QUEUE_CAPACITY})"
            ));
        }
        if let Some(&last) = self.hint_rate.get(&key_hint) {
            if last.elapsed() < RATE_LIMIT_INTERVAL {
                return Err(format!(
                    "recovery request rate-limited for key_hint {key_hint}"
                ));
            }
        }

        let mut request_id = [0u8; 16];
        getrandom::fill(&mut request_id).map_err(|e| format!("request_id rng failed: {e}"))?;

        self.entries.insert(
            request_id,
            RecoveryEntry {
                key_hint,
                raw_frame,
                peer_address,
                created_at: Instant::now(),
            },
        );
        self.hint_rate.insert(key_hint, Instant::now());
        Ok(request_id)
    }

    /// Look up and remove a buffered entry by request_id.
    ///
    /// Returns `None` if the entry doesn't exist or has expired.
    pub fn take(&mut self, request_id: &[u8; 16]) -> Option<RecoveryEntry> {
        if let Some(entry) = self.entries.remove(request_id) {
            if entry.created_at.elapsed() <= RECOVERY_ENTRY_TTL {
                return Some(entry);
            }
            warn!(
                key_hint = entry.key_hint,
                "recovery entry expired before response arrived"
            );
        }
        None
    }

    /// Remove expired entries.
    fn evict_expired(&mut self) {
        self.entries
            .retain(|_, entry| entry.created_at.elapsed() <= RECOVERY_ENTRY_TTL);
        // Also clean up stale rate-limit entries (keep for 2× the interval)
        let stale_cutoff = RATE_LIMIT_INTERVAL * 2;
        self.hint_rate
            .retain(|_, last| last.elapsed() <= stale_cutoff);
    }

    /// Number of pending recovery entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for RecoveryQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ── Keypair management (GW-2000) ────────────────────────────────────────

/// Encrypt a 32-byte secret key using AES-256-GCM with a random nonce.
///
/// Returns `nonce(12) || ciphertext+tag(48)`.
fn encrypt_secret_key(master_key: &[u8; 32], secret: &[u8; 32]) -> Result<Vec<u8>, StorageError> {
    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);

    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| StorageError::Internal(format!("nonce rng: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, secret.as_slice())
        .map_err(|e| StorageError::Internal(format!("secret key encrypt: {e}")))?;

    let mut out = Vec::with_capacity(ENCRYPTED_KEY_LEN);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a secret key blob produced by [`encrypt_secret_key`].
fn decrypt_secret_key(
    master_key: &[u8; 32],
    blob: &[u8],
) -> Result<Zeroizing<[u8; 32]>, StorageError> {
    if blob.len() != ENCRYPTED_KEY_LEN {
        return Err(StorageError::Internal(format!(
            "encrypted key blob has wrong length: expected {ENCRYPTED_KEY_LEN}, got {}",
            blob.len()
        )));
    }

    let key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&blob[..12]);

    let plaintext = Zeroizing::new(cipher.decrypt(nonce, &blob[12..]).map_err(|_| {
        StorageError::Internal(
            "secret key decryption failed — wrong master key or data corruption".into(),
        )
    })?);

    let mut secret = Zeroizing::new([0u8; 32]);
    if plaintext.len() != 32 {
        return Err(StorageError::Internal(
            "decrypted secret key is not 32 bytes".into(),
        ));
    }
    secret.copy_from_slice(&plaintext);
    Ok(secret)
}

/// Load or generate the escrow keypair (GW-2000).
///
/// - If a keypair exists and can be decrypted, returns it.
/// - If decryption fails (master key mismatch), generates a new keypair
///   with incremented epoch and logs a warning.
/// - If no keypair exists, generates a new one with epoch 1.
pub async fn load_or_generate_keypair(
    storage: &dyn Storage,
    master_key: &[u8; 32],
) -> Result<EscrowKeypair, StorageError> {
    if let Some(record) = storage.get_escrow_keypair().await? {
        match decrypt_secret_key(master_key, &record.secret_enc) {
            Ok(secret) => {
                // Validate that the stored public key matches the decrypted private key.
                let derived_secret = StaticSecret::from(*secret);
                let derived_public = PublicKey::from(&derived_secret);
                if *derived_public.as_bytes() != record.public_key {
                    warn!(
                        epoch = record.epoch,
                        "stored public key does not match decrypted private key, regenerating"
                    );
                    let (secret, public, epoch) = generate_keypair(record.epoch + 1)?;
                    let secret_enc = encrypt_secret_key(master_key, &secret)?;
                    let now_ms = now_ms();
                    storage
                        .store_escrow_keypair(&EscrowKeypairRecord {
                            secret_enc,
                            public_key: public,
                            epoch,
                            created_at: now_ms,
                        })
                        .await?;
                    info!(epoch, "regenerated escrow keypair (public key mismatch)");
                    return Ok((secret, public, epoch));
                }
                debug!(epoch = record.epoch, "loaded existing escrow keypair");
                return Ok((secret, record.public_key, record.epoch));
            }
            Err(e) => {
                warn!(
                    epoch = record.epoch,
                    "failed to decrypt escrow private key, generating new keypair: {e}"
                );
                // Generate new keypair with incremented epoch
                let (secret, public, epoch) = generate_keypair(record.epoch + 1)?;
                let secret_enc = encrypt_secret_key(master_key, &secret)?;
                let now_ms = now_ms();
                storage
                    .store_escrow_keypair(&EscrowKeypairRecord {
                        secret_enc,
                        public_key: public,
                        epoch,
                        created_at: now_ms,
                    })
                    .await?;
                info!(epoch, "generated new escrow keypair (master key changed)");
                return Ok((secret, public, epoch));
            }
        }
    }

    // No keypair exists — generate one
    let (secret, public, epoch) = generate_keypair(1)?;
    let secret_enc = encrypt_secret_key(master_key, &secret)?;
    let now_ms = now_ms();
    storage
        .store_escrow_keypair(&EscrowKeypairRecord {
            secret_enc,
            public_key: public,
            epoch,
            created_at: now_ms,
        })
        .await?;
    info!(epoch, "generated initial escrow keypair");
    Ok((secret, public, epoch))
}

fn generate_keypair(epoch: u64) -> Result<EscrowKeypair, StorageError> {
    let mut secret_bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(secret_bytes.as_mut_slice())
        .map_err(|e| StorageError::Internal(format!("escrow keypair rng failed: {e}")))?;
    let secret = StaticSecret::from(*secret_bytes);
    let public = PublicKey::from(&secret);
    Ok((secret_bytes, *public.as_bytes(), epoch))
}

// ── Escrow state persistence ────────────────────────────────────────────

/// Load the current escrow state from storage.
pub async fn load_escrow_state(storage: &dyn Storage) -> Result<EscrowState, StorageError> {
    match storage.get_config("escrow_state").await? {
        Some(s) => match EscrowState::from_str(&s) {
            Some(state) => Ok(state),
            None => Err(StorageError::Internal(format!(
                "unrecognized escrow_state: {s}"
            ))),
        },
        None => Ok(EscrowState::Disabled),
    }
}

/// Persist the escrow state.
pub async fn store_escrow_state(
    storage: &dyn Storage,
    state: &EscrowState,
) -> Result<(), StorageError> {
    storage.set_config("escrow_state", state.as_str()).await
}

/// Load the current key version from storage.
pub async fn load_key_version(storage: &dyn Storage) -> Result<u64, StorageError> {
    match storage.get_config("escrow_key_version").await? {
        Some(s) => s
            .parse::<u64>()
            .map_err(|e| StorageError::Internal(format!("corrupt escrow_key_version: {e}"))),
        None => Ok(0),
    }
}

/// Persist the current key version.
pub async fn store_key_version(storage: &dyn Storage, version: u64) -> Result<(), StorageError> {
    storage
        .set_config("escrow_key_version", &version.to_string())
        .await
}

/// Load the last processed rotation counter.
pub async fn load_rotation_counter(storage: &dyn Storage) -> Result<u64, StorageError> {
    match storage.get_config("escrow_rotation_counter").await? {
        Some(s) => s
            .parse::<u64>()
            .map_err(|e| StorageError::Internal(format!("corrupt escrow_rotation_counter: {e}"))),
        None => Ok(0),
    }
}

/// Persist the rotation counter.
pub async fn store_rotation_counter(
    storage: &dyn Storage,
    counter: u64,
) -> Result<(), StorageError> {
    storage
        .set_config("escrow_rotation_counter", &counter.to_string())
        .await
}

// ── MASTER_KEY_INSTALL decryption (GW-2006) ─────────────────────────────

/// Decrypt a MASTER_KEY_INSTALL payload using X25519 + HKDF-SHA-256 + AES-256-GCM.
///
/// Returns the decrypted 32-byte master key.
pub fn decrypt_master_key_install(
    gateway_secret: &[u8; 32],
    sender_public_key: &[u8; 32],
    target_key_epoch: u64,
    operation_id: &[u8; 16],
    encrypted_master_key: &[u8],
    nonce: &[u8; 12],
    tag: &[u8; 16],
) -> Result<Zeroizing<[u8; 32]>, String> {
    // X25519 key agreement
    let secret = StaticSecret::from(*gateway_secret);
    let peer_public = PublicKey::from(*sender_public_key);
    let shared_secret = secret.diffie_hellman(&peer_public);
    if shared_secret.as_bytes().iter().all(|&b| b == 0) {
        return Err("X25519 produced all-zero shared secret (low-order public key)".into());
    }

    // HKDF-SHA-256 key derivation
    // salt = "sonde-escrow-v1"
    // info = target_key_epoch (8 bytes BE) || operation_id
    let mut info = Vec::with_capacity(8 + operation_id.len());
    info.extend_from_slice(&target_key_epoch.to_be_bytes());
    info.extend_from_slice(operation_id);

    let hk = Hkdf::<Sha256>::new(Some(b"sonde-escrow-v1"), shared_secret.as_bytes());
    let mut derived_key = Zeroizing::new([0u8; 32]);
    hk.expand(&info, derived_key.as_mut_slice())
        .map_err(|e| format!("HKDF expansion failed: {e}"))?;

    // AES-256-GCM decryption
    // AAD = operation_id || target_key_epoch (8 bytes BE)
    let mut aad = Vec::with_capacity(operation_id.len() + 8);
    aad.extend_from_slice(operation_id);
    aad.extend_from_slice(&target_key_epoch.to_be_bytes());

    let key = Key::<Aes256Gcm>::from_slice(&*derived_key);
    let cipher = Aes256Gcm::new(key);
    let gcm_nonce = Nonce::from_slice(nonce);

    let mut ct_and_tag = Vec::with_capacity(encrypted_master_key.len() + 16);
    ct_and_tag.extend_from_slice(encrypted_master_key);
    ct_and_tag.extend_from_slice(tag);

    let payload = Payload {
        msg: &ct_and_tag,
        aad: &aad,
    };

    let plaintext = Zeroizing::new(
        cipher
            .decrypt(gcm_nonce, payload)
            .map_err(|_| "AES-256-GCM decryption failed — wrong key or tampered message")?,
    );

    if plaintext.len() != 32 {
        return Err(format!(
            "decrypted master key has wrong length: {} (expected 32)",
            plaintext.len()
        ));
    }

    let mut master_key = Zeroizing::new([0u8; 32]);
    master_key.copy_from_slice(&plaintext);
    Ok(master_key)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryStorage;

    #[tokio::test]
    async fn test_keypair_generation_and_persistence() {
        let storage = InMemoryStorage::new();
        let master_key = [0x42u8; 32];

        // First call generates a new keypair
        let (secret1, public1, epoch1) = load_or_generate_keypair(&storage, &master_key)
            .await
            .unwrap();
        assert_eq!(epoch1, 1);
        assert_ne!(public1, [0u8; 32]);

        // Second call with same master key returns the same keypair
        let (secret2, public2, epoch2) = load_or_generate_keypair(&storage, &master_key)
            .await
            .unwrap();
        assert_eq!(epoch2, 1);
        assert_eq!(public2, public1);
        assert_eq!(*secret2, *secret1);

        // Call with different master key generates a new keypair with incremented epoch
        let different_key = [0x99u8; 32];
        let (_secret3, public3, epoch3) = load_or_generate_keypair(&storage, &different_key)
            .await
            .unwrap();
        assert_eq!(epoch3, 2);
        assert_ne!(public3, public1);
    }

    #[tokio::test]
    async fn test_escrow_state_persistence() {
        let storage = InMemoryStorage::new();

        // Default state is disabled
        let state = load_escrow_state(&storage).await.unwrap();
        assert_eq!(state, EscrowState::Disabled);

        // Store and reload
        store_escrow_state(&storage, &EscrowState::Ready)
            .await
            .unwrap();
        let state = load_escrow_state(&storage).await.unwrap();
        assert_eq!(state, EscrowState::Ready);

        // All states round-trip
        for s in &[
            EscrowState::Disabled,
            EscrowState::Bootstrapping,
            EscrowState::Ready,
            EscrowState::RotationInProgress,
            EscrowState::Degraded,
        ] {
            store_escrow_state(&storage, s).await.unwrap();
            let loaded = load_escrow_state(&storage).await.unwrap();
            assert_eq!(&loaded, s);
        }
    }

    #[tokio::test]
    async fn test_invalid_escrow_state_returns_error() {
        let storage = InMemoryStorage::new();
        storage
            .set_config("escrow_state", "definitely-invalid")
            .await
            .unwrap();

        let err = load_escrow_state(&storage).await.unwrap_err();
        assert!(err.to_string().contains("unrecognized escrow_state"));
    }

    #[tokio::test]
    async fn test_key_version_persistence() {
        let storage = InMemoryStorage::new();

        assert_eq!(load_key_version(&storage).await.unwrap(), 0);
        store_key_version(&storage, 42).await.unwrap();
        assert_eq!(load_key_version(&storage).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_corrupt_key_version_returns_error() {
        let storage = InMemoryStorage::new();
        storage
            .set_config("escrow_key_version", "not-a-u64")
            .await
            .unwrap();

        let err = load_key_version(&storage).await.unwrap_err();
        assert!(err.to_string().contains("corrupt escrow_key_version"));
    }

    #[tokio::test]
    async fn test_corrupt_rotation_counter_returns_error() {
        let storage = InMemoryStorage::new();
        storage
            .set_config("escrow_rotation_counter", "not-a-u64")
            .await
            .unwrap();

        let err = load_rotation_counter(&storage).await.unwrap_err();
        assert!(err.to_string().contains("corrupt escrow_rotation_counter"));
    }

    #[test]
    fn test_recovery_queue_basic() {
        let mut queue = RecoveryQueue::new();

        // Can request initially
        assert!(queue.can_request(0x1234));

        // Enqueue a frame
        let rid = queue.enqueue(0x1234, vec![1, 2, 3], [0; 6]).unwrap();
        assert_eq!(queue.len(), 1);

        // Cannot request same key_hint again (rate limited)
        assert!(!queue.can_request(0x1234));

        // Can request a different key_hint
        assert!(queue.can_request(0x5678));

        // Take the entry
        let entry = queue.take(&rid).unwrap();
        assert_eq!(entry.key_hint, 0x1234);
        assert_eq!(entry.raw_frame, vec![1, 2, 3]);
        assert_eq!(queue.len(), 0);

        // Second take returns None
        assert!(queue.take(&rid).is_none());
    }

    #[test]
    fn test_recovery_queue_capacity_limit() {
        let mut queue = RecoveryQueue::new();

        // Fill to capacity limit
        for i in 0..RECOVERY_QUEUE_CAPACITY {
            let _ = queue.enqueue(i as u16, vec![0], [0; 6]).unwrap();
        }

        // Next request should be rejected (capacity limit)
        assert!(!queue.can_request(0xFFFF));
        let err = queue.enqueue(0xFFFF, vec![0], [0; 6]).unwrap_err();
        assert!(err.contains("recovery queue full"));
    }

    #[test]
    fn test_recovery_queue_enqueue_rejects_rate_limited_key_hint() {
        let mut queue = RecoveryQueue::new();

        queue.enqueue(0x1234, vec![1], [0; 6]).unwrap();
        let err = queue.enqueue(0x1234, vec![2], [0; 6]).unwrap_err();
        assert!(err.contains("rate-limited"));
    }

    #[test]
    fn test_recovery_queue_expired_entry() {
        let mut queue = RecoveryQueue::new();
        let request_id = [0x42u8; 16];
        queue.entries.insert(
            request_id,
            RecoveryEntry {
                key_hint: 0x1234,
                raw_frame: vec![1, 2, 3],
                peer_address: [0; 6],
                created_at: Instant::now() - Duration::from_secs(31),
            },
        );

        assert!(queue.take(&request_id).is_none());
        assert!(queue.entries.is_empty());
    }

    #[test]
    fn test_encrypt_decrypt_secret_key_round_trip() {
        let master_key = [0x42u8; 32];
        let secret = [0xABu8; 32];

        let encrypted = encrypt_secret_key(&master_key, &secret).unwrap();
        assert_eq!(encrypted.len(), ENCRYPTED_KEY_LEN);

        let decrypted = decrypt_secret_key(&master_key, &encrypted).unwrap();
        assert_eq!(*decrypted, secret);
    }

    #[test]
    fn test_encrypt_decrypt_wrong_key_fails() {
        let master_key = [0x42u8; 32];
        let wrong_key = [0x99u8; 32];
        let secret = [0xABu8; 32];

        let encrypted = encrypt_secret_key(&master_key, &secret).unwrap();
        let result = decrypt_secret_key(&wrong_key, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_master_key_install() {
        // Simulate admin-side encryption and gateway-side decryption
        let mut gw_secret_bytes = [0u8; 32];
        getrandom::fill(&mut gw_secret_bytes).unwrap();
        let gw_secret = StaticSecret::from(gw_secret_bytes);
        let gw_public = PublicKey::from(&gw_secret);

        let mut admin_secret_bytes = [0u8; 32];
        getrandom::fill(&mut admin_secret_bytes).unwrap();
        let admin_secret = StaticSecret::from(admin_secret_bytes);
        let admin_public = PublicKey::from(&admin_secret);

        let target_key_epoch: u64 = 1;
        let operation_id = [0x11u8; 16];
        let master_key_to_install = [0xCDu8; 32];

        // Admin side: encrypt the master key
        let shared = admin_secret.diffie_hellman(&gw_public);
        let mut info = Vec::new();
        info.extend_from_slice(&target_key_epoch.to_be_bytes());
        info.extend_from_slice(&operation_id);

        let hk = Hkdf::<Sha256>::new(Some(b"sonde-escrow-v1"), shared.as_bytes());
        let mut derived_key = [0u8; 32];
        hk.expand(&info, &mut derived_key).unwrap();

        let key = Key::<Aes256Gcm>::from_slice(&derived_key);
        let cipher = Aes256Gcm::new(key);

        let mut nonce_bytes = [0u8; 12];
        getrandom::fill(&mut nonce_bytes).unwrap();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let mut aad = Vec::new();
        aad.extend_from_slice(&operation_id);
        aad.extend_from_slice(&target_key_epoch.to_be_bytes());

        let payload = Payload {
            msg: master_key_to_install.as_slice(),
            aad: &aad,
        };
        let ciphertext_and_tag = cipher.encrypt(nonce, payload).unwrap();
        let encrypted_master_key = &ciphertext_and_tag[..32];
        let tag: [u8; 16] = ciphertext_and_tag[32..].try_into().unwrap();

        // Gateway side: decrypt
        let result = decrypt_master_key_install(
            &gw_secret_bytes,
            admin_public.as_bytes(),
            target_key_epoch,
            &operation_id,
            encrypted_master_key,
            &nonce_bytes,
            &tag,
        );
        assert!(result.is_ok());
        assert_eq!(*result.unwrap(), master_key_to_install);
    }

    #[test]
    fn test_decrypt_master_key_install_wrong_gateway_key() {
        // Use a different gateway key for decryption
        let mut gw_secret_bytes = [0u8; 32];
        getrandom::fill(&mut gw_secret_bytes).unwrap();
        let gw_secret = StaticSecret::from(gw_secret_bytes);
        let gw_public = PublicKey::from(&gw_secret);

        let mut admin_secret_bytes = [0u8; 32];
        getrandom::fill(&mut admin_secret_bytes).unwrap();
        let admin_secret = StaticSecret::from(admin_secret_bytes);
        let admin_public = PublicKey::from(&admin_secret);

        let target_key_epoch: u64 = 1;
        let operation_id = [0x22u8; 16];
        let master_key_to_install = [0xEEu8; 32];

        // Admin encrypts with real gateway public key
        let shared = admin_secret.diffie_hellman(&gw_public);
        let mut info = Vec::new();
        info.extend_from_slice(&target_key_epoch.to_be_bytes());
        info.extend_from_slice(&operation_id);

        let hk = Hkdf::<Sha256>::new(Some(b"sonde-escrow-v1"), shared.as_bytes());
        let mut derived_key = [0u8; 32];
        hk.expand(&info, &mut derived_key).unwrap();

        let key = Key::<Aes256Gcm>::from_slice(&derived_key);
        let cipher = Aes256Gcm::new(key);

        let mut nonce_bytes = [0u8; 12];
        getrandom::fill(&mut nonce_bytes).unwrap();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let mut aad = Vec::new();
        aad.extend_from_slice(&operation_id);
        aad.extend_from_slice(&target_key_epoch.to_be_bytes());

        let payload = Payload {
            msg: master_key_to_install.as_slice(),
            aad: &aad,
        };
        let ct_tag = cipher.encrypt(nonce, payload).unwrap();

        // Try decryption with a DIFFERENT gateway secret
        let mut wrong_gw_secret = [0u8; 32];
        getrandom::fill(&mut wrong_gw_secret).unwrap();

        let result = decrypt_master_key_install(
            &wrong_gw_secret,
            admin_public.as_bytes(),
            target_key_epoch,
            &operation_id,
            &ct_tag[..32],
            &nonce_bytes,
            &ct_tag[32..].try_into().unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_master_key_install_rejects_low_order_public_key() {
        let gateway_secret = [0x42u8; 32];
        let sender_public_key = [0u8; 32];
        let result = decrypt_master_key_install(
            &gateway_secret,
            &sender_public_key,
            1,
            &[0x11u8; 16],
            &[0x22u8; 32],
            &[0x33u8; 12],
            &[0x44u8; 16],
        );
        assert_eq!(
            result.unwrap_err(),
            "X25519 produced all-zero shared secret (low-order public key)"
        );
    }
}
