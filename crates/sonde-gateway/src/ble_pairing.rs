// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Gateway BLE pairing state machine (Phase 1: phone registration).
//!
//! Processes BLE messages relayed from the modem and implements:
//! - REQUEST_GW_INFO (0x01): Ed25519 challenge-response (GW-1206)
//! - REGISTER_PHONE (0x02): ECDH + HKDF + AES-GCM phone PSK issuance (GW-1209)
//! - Registration window enforcement (GW-1207/GW-1208)
//!
//! Messages arrive as raw BLE envelope bytes via `BLE_RECV` and responses are
//! sent back as BLE envelope bytes via `BLE_INDICATE`.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use ed25519_dalek::Signer;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
use x25519_dalek::PublicKey as X25519PublicKey;
use zeroize::Zeroizing;

use sonde_protocol::{encode_ble_envelope, parse_ble_envelope};

use crate::gateway_identity::GatewayIdentity;
use crate::phone_trust::{PhonePskRecord, PhonePskStatus, PHONE_LABEL_MAX_BYTES};
use crate::storage::Storage;

// ---------------------------------------------------------------------------
// BLE message type codes (ble-pairing-protocol.md §4)
// ---------------------------------------------------------------------------

/// Phone → Gateway: request gateway info (challenge-response).
const BLE_MSG_REQUEST_GW_INFO: u8 = 0x01;
/// Phone → Gateway: register phone (ECDH key exchange).
const BLE_MSG_REGISTER_PHONE: u8 = 0x02;

/// Gateway → Phone: gateway info response (public key + signature).
const BLE_MSG_GW_INFO_RESPONSE: u8 = 0x81;
/// Gateway → Phone: phone registered (encrypted PSK).
const BLE_MSG_PHONE_REGISTERED: u8 = 0x82;
/// Gateway → Phone: error response.
const BLE_MSG_ERROR: u8 = 0xFF;

/// Error code: registration window not open.
const ERROR_WINDOW_CLOSED: u8 = 0x02;

/// HKDF info string for phone registration AES key derivation.
const HKDF_INFO: &[u8] = b"sonde-phone-reg-v1";

// ---------------------------------------------------------------------------
// Registration window
// ---------------------------------------------------------------------------

/// Registration window state.
pub struct RegistrationWindow {
    open: bool,
    deadline: Option<Instant>,
}

impl Default for RegistrationWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistrationWindow {
    pub fn new() -> Self {
        Self {
            open: false,
            deadline: None,
        }
    }

    /// Open the window for `duration_s` seconds.
    pub fn open(&mut self, duration_s: u32) {
        self.open = true;
        let dur = std::time::Duration::from_secs(duration_s as u64);
        self.deadline = Instant::now().checked_add(dur);
        // If checked_add returns None (overflow), the window stays open
        // indefinitely until explicitly closed. This is safe — the admin
        // can always call close().
    }

    /// Close the window immediately.
    pub fn close(&mut self) {
        self.open = false;
        self.deadline = None;
    }

    /// Check whether the window is currently open (auto-closes on expiry).
    pub fn is_open(&mut self) -> bool {
        if self.open {
            if let Some(deadline) = self.deadline {
                if Instant::now() >= deadline {
                    self.open = false;
                    self.deadline = None;
                    return false;
                }
            }
            true
        } else {
            false
        }
    }
}

/// Shared BLE pairing controller accessible from the admin gRPC service.
///
/// Wraps the registration window and provides methods to open/close it.
/// The actual BLE commands (BLE_ENABLE/BLE_DISABLE) are sent by the caller
/// through the modem transport.
pub struct BlePairingController {
    window: tokio::sync::Mutex<RegistrationWindow>,
    /// Channel for forwarding passkey confirmation requests to the admin CLI.
    passkey_tx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
    /// Cancel token for the auto-close timeout task.
    timeout_cancel: tokio::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
}

impl Default for BlePairingController {
    fn default() -> Self {
        Self::new()
    }
}

impl BlePairingController {
    pub fn new() -> Self {
        Self {
            window: tokio::sync::Mutex::new(RegistrationWindow::new()),
            passkey_tx: tokio::sync::Mutex::new(None),
            timeout_cancel: tokio::sync::Mutex::new(None),
        }
    }

    /// Open the registration window for `duration_s` seconds.
    pub async fn open_window(&self, duration_s: u32) {
        self.window.lock().await.open(duration_s);
    }

    /// Close the registration window.
    pub async fn close_window(&self) {
        self.window.lock().await.close();
    }

    /// Check whether the window is currently open.
    pub async fn is_window_open(&self) -> bool {
        self.window.lock().await.is_open()
    }

    /// Register a oneshot channel for passkey confirmation from the admin CLI.
    pub async fn set_passkey_responder(&self, tx: tokio::sync::oneshot::Sender<bool>) {
        *self.passkey_tx.lock().await = Some(tx);
    }

    /// Send a passkey confirmation response (from admin CLI).
    pub async fn confirm_passkey(&self, accept: bool) -> bool {
        if let Some(tx) = self.passkey_tx.lock().await.take() {
            tx.send(accept).is_ok()
        } else {
            false
        }
    }

    /// Store a cancellation token for the auto-close timeout task.
    pub async fn set_timeout_cancel(&self, token: tokio_util::sync::CancellationToken) {
        // Cancel any previous timeout task before storing the new one.
        if let Some(old) = self.timeout_cancel.lock().await.replace(token) {
            old.cancel();
        }
    }

    /// Cancel the auto-close timeout task (called by CloseBlePairing).
    pub async fn cancel_timeout(&self) {
        if let Some(token) = self.timeout_cancel.lock().await.take() {
            token.cancel();
        }
    }
}

// ---------------------------------------------------------------------------
// BLE pairing handler
// ---------------------------------------------------------------------------

/// Handle an incoming BLE_RECV message from the modem.
///
/// Parses the BLE envelope, dispatches to the appropriate handler, and
/// returns an optional BLE envelope response to send via BLE_INDICATE.
pub async fn handle_ble_recv(
    data: &[u8],
    identity: &GatewayIdentity,
    storage: &Arc<dyn Storage>,
    window: &mut RegistrationWindow,
    rf_channel: u8,
) -> Option<Vec<u8>> {
    let (msg_type, body) = parse_ble_envelope(data)?;

    match msg_type {
        BLE_MSG_REQUEST_GW_INFO => handle_request_gw_info(body, identity),
        BLE_MSG_REGISTER_PHONE => {
            handle_register_phone(body, identity, storage, window, rf_channel).await
        }
        _ => {
            debug!(msg_type, "ignoring unknown BLE message type");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// REQUEST_GW_INFO (GW-1206)
// ---------------------------------------------------------------------------

/// Handle REQUEST_GW_INFO: sign (challenge ‖ gateway_id), return GW_INFO_RESPONSE.
///
/// REQUEST_GW_INFO body: 32-byte challenge.
/// GW_INFO_RESPONSE body: gw_public_key(32) + gateway_id(16) + signature(64) = 112 bytes.
fn handle_request_gw_info(body: &[u8], identity: &GatewayIdentity) -> Option<Vec<u8>> {
    if body.len() != 32 {
        warn!(
            len = body.len(),
            "REQUEST_GW_INFO: invalid challenge length (expected 32)"
        );
        return None;
    }

    let challenge = body;
    let gateway_id = identity.gateway_id();

    // Sign (challenge ‖ gateway_id)
    let mut sign_input = Vec::with_capacity(32 + 16);
    sign_input.extend_from_slice(challenge);
    sign_input.extend_from_slice(gateway_id);
    let signature = identity.signing_key().sign(&sign_input);

    // Build response: gw_public_key(32) + gateway_id(16) + signature(64)
    let mut response = Vec::with_capacity(112);
    response.extend_from_slice(identity.public_key());
    response.extend_from_slice(gateway_id);
    response.extend_from_slice(&signature.to_bytes());

    encode_ble_envelope(BLE_MSG_GW_INFO_RESPONSE, &response)
}

// ---------------------------------------------------------------------------
// REGISTER_PHONE (GW-1207, GW-1209)
// ---------------------------------------------------------------------------

/// Handle REGISTER_PHONE: check window, perform ECDH, issue phone PSK.
///
/// REGISTER_PHONE body: ephemeral_pubkey(32) + label_len(1) + label(label_len).
/// PHONE_REGISTERED body: nonce(12) + ciphertext (AES-256-GCM encrypted inner).
/// Inner plaintext: status(1) + phone_psk(32) + phone_key_hint(2) + rf_channel(1) = 36 bytes.
async fn handle_register_phone(
    body: &[u8],
    identity: &GatewayIdentity,
    storage: &Arc<dyn Storage>,
    window: &mut RegistrationWindow,
    rf_channel: u8,
) -> Option<Vec<u8>> {
    // Check registration window
    if !window.is_open() {
        info!("REGISTER_PHONE rejected: registration window closed");
        return encode_ble_envelope(BLE_MSG_ERROR, &[ERROR_WINDOW_CLOSED]);
    }

    // Parse body
    if body.len() < 33 {
        warn!(
            len = body.len(),
            "REGISTER_PHONE: body too short (min 33 bytes)"
        );
        return None;
    }

    let ephemeral_pubkey_bytes: [u8; 32] = body[..32].try_into().unwrap();
    let label_len = body[32] as usize;

    if label_len > PHONE_LABEL_MAX_BYTES {
        warn!(label_len, "REGISTER_PHONE: label too long (max 64 bytes)");
        return None;
    }

    if body.len() != 33 + label_len {
        warn!(
            expected = 33 + label_len,
            actual = body.len(),
            "REGISTER_PHONE: body length mismatch"
        );
        return None;
    }

    let label = match std::str::from_utf8(&body[33..33 + label_len]) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            warn!("REGISTER_PHONE: label is not valid UTF-8");
            return None;
        }
    };

    // 1. Generate phone PSK from OS CSPRNG
    let mut phone_psk = Zeroizing::new([0u8; 32]);
    if getrandom::fill(phone_psk.as_mut_slice()).is_err() {
        warn!("REGISTER_PHONE: CSPRNG failure");
        return None;
    }

    // 2. Derive phone_key_hint = SHA-256(psk)[30..32] as BE u16
    let psk_hash = Sha256::digest(phone_psk.as_slice());
    let phone_key_hint = u16::from_be_bytes([psk_hash[30], psk_hash[31]]);

    // 3. ECDH key agreement
    let (x25519_secret, _x25519_pub) = match identity.to_x25519() {
        Ok(pair) => pair,
        Err(e) => {
            warn!(?e, "REGISTER_PHONE: Ed25519 → X25519 conversion failed");
            return None;
        }
    };

    let phone_x25519_pub = X25519PublicKey::from(ephemeral_pubkey_bytes);
    let shared_secret = x25519_secret.diffie_hellman(&phone_x25519_pub);

    // Reject low-order shared secret (all zeros)
    if shared_secret.as_bytes() == &[0u8; 32] {
        warn!("REGISTER_PHONE: ECDH shared secret is zero (low-order point)");
        return None;
    }

    // 4. Derive AES key via HKDF-SHA256
    let gateway_id = identity.gateway_id();
    let hkdf = Hkdf::<Sha256>::new(Some(gateway_id), shared_secret.as_bytes());
    let mut aes_key = Zeroizing::new([0u8; 32]);
    if hkdf.expand(HKDF_INFO, &mut *aes_key).is_err() {
        warn!("REGISTER_PHONE: HKDF expansion failed");
        return None;
    }

    // 5. Build plaintext: status(1) + phone_psk(32) + phone_key_hint(2) + rf_channel(1)
    // Wrapped in Zeroizing to wipe key material after encryption.
    let mut plaintext = Zeroizing::new(vec![0u8; 36]);
    plaintext[0] = 0x00; // status = accepted
    plaintext[1..33].copy_from_slice(&*phone_psk);
    plaintext[33..35].copy_from_slice(&phone_key_hint.to_be_bytes());
    plaintext[35] = rf_channel;

    // 6. Encrypt with AES-256-GCM (AAD = gateway_id)
    let cipher = match Aes256Gcm::new_from_slice(&*aes_key) {
        Ok(c) => c,
        Err(_) => {
            warn!("REGISTER_PHONE: AES-256-GCM key init failed");
            return None;
        }
    };

    let mut nonce_bytes = [0u8; 12];
    if getrandom::fill(&mut nonce_bytes).is_err() {
        warn!("REGISTER_PHONE: CSPRNG failure for GCM nonce");
        return None;
    }
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = match cipher.encrypt(
        nonce,
        aes_gcm::aead::Payload {
            msg: &plaintext,
            aad: gateway_id,
        },
    ) {
        Ok(ct) => ct,
        Err(_) => {
            warn!("REGISTER_PHONE: AES-256-GCM encryption failed");
            return None;
        }
    };

    // 7. Build PHONE_REGISTERED response: nonce(12) + ciphertext
    let mut response = Vec::with_capacity(12 + ciphertext.len());
    response.extend_from_slice(&nonce_bytes);
    response.extend_from_slice(&ciphertext);

    // 8. Store phone PSK record
    let record = PhonePskRecord {
        phone_id: 0, // auto-assigned by storage
        phone_key_hint,
        psk: phone_psk,
        label,
        issued_at: SystemTime::now(),
        status: PhonePskStatus::Active,
    };

    if let Err(e) = storage.store_phone_psk(&record).await {
        warn!(?e, "REGISTER_PHONE: failed to store phone PSK");
        return None;
    }

    info!(
        phone_key_hint,
        label = record.label,
        "phone registered successfully"
    );

    encode_ble_envelope(BLE_MSG_PHONE_REGISTERED, &response)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_identity::GatewayIdentity;
    use crate::storage::InMemoryStorage;
    use ed25519_dalek::Verifier;

    fn test_identity() -> GatewayIdentity {
        GatewayIdentity::generate().unwrap()
    }

    // -- T-1203: REQUEST_GW_INFO happy path --

    #[test]
    fn t_1203_request_gw_info_happy_path() {
        let identity = test_identity();

        // Build REQUEST_GW_INFO with random 32-byte challenge
        let mut challenge = [0u8; 32];
        getrandom::fill(&mut challenge).unwrap();
        let request = encode_ble_envelope(BLE_MSG_REQUEST_GW_INFO, &challenge).unwrap();

        // Parse the envelope
        let (msg_type, body) = parse_ble_envelope(&request).unwrap();
        assert_eq!(msg_type, BLE_MSG_REQUEST_GW_INFO);

        // Handle it
        let response = handle_request_gw_info(body, &identity).unwrap();

        // Parse response envelope
        let (resp_type, resp_body) = parse_ble_envelope(&response).unwrap();
        assert_eq!(resp_type, BLE_MSG_GW_INFO_RESPONSE);
        assert_eq!(resp_body.len(), 112);

        // Extract fields
        let gw_pub = &resp_body[..32];
        let gw_id = &resp_body[32..48];
        let sig_bytes = &resp_body[48..112];

        assert_eq!(gw_pub, identity.public_key());
        assert_eq!(gw_id, identity.gateway_id());

        // Verify signature over (challenge ‖ gateway_id)
        let mut signed_data = Vec::new();
        signed_data.extend_from_slice(&challenge);
        signed_data.extend_from_slice(identity.gateway_id());

        let signature = ed25519_dalek::Signature::from_bytes(sig_bytes.try_into().unwrap());
        let verifying_key = identity.verifying_key();
        assert!(verifying_key.verify(&signed_data, &signature).is_ok());
    }

    // -- T-1204: GW_INFO_RESPONSE signature fails with wrong challenge --

    #[test]
    fn t_1204_wrong_challenge_fails_verification() {
        let identity = test_identity();

        let mut challenge_a = [0u8; 32];
        getrandom::fill(&mut challenge_a).unwrap();

        let response = handle_request_gw_info(&challenge_a, &identity).unwrap();
        let (_, resp_body) = parse_ble_envelope(&response).unwrap();

        let sig_bytes: &[u8; 64] = resp_body[48..112].try_into().unwrap();
        let signature = ed25519_dalek::Signature::from_bytes(sig_bytes);

        // Verify against a different challenge
        let mut challenge_b = [0u8; 32];
        getrandom::fill(&mut challenge_b).unwrap();
        let mut wrong_data = Vec::new();
        wrong_data.extend_from_slice(&challenge_b);
        wrong_data.extend_from_slice(identity.gateway_id());

        let verifying_key = identity.verifying_key();
        assert!(verifying_key.verify(&wrong_data, &signature).is_err());
    }

    // -- T-1205: REGISTER_PHONE rejected when window closed --

    #[tokio::test]
    async fn t_1205_register_phone_window_closed() {
        let identity = test_identity();
        let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
        let mut window = RegistrationWindow::new(); // closed by default

        // Build a minimal REGISTER_PHONE body
        let mut body = vec![0u8; 33];
        body[32] = 0; // label_len = 0

        let response = handle_register_phone(&body, &identity, &storage, &mut window, 6).await;

        let response = response.unwrap();
        let (msg_type, resp_body) = parse_ble_envelope(&response).unwrap();
        assert_eq!(msg_type, BLE_MSG_ERROR);
        assert_eq!(resp_body, &[ERROR_WINDOW_CLOSED]);
    }

    // -- T-1207: REGISTER_PHONE happy path (ECDH + decrypt) --

    #[tokio::test]
    async fn t_1207_register_phone_happy_path() {
        let identity = test_identity();
        let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
        let mut window = RegistrationWindow::new();
        window.open(120);

        // Generate a phone ephemeral X25519 keypair from random bytes.
        let mut phone_secret_bytes = [0u8; 32];
        getrandom::fill(&mut phone_secret_bytes).unwrap();
        let phone_secret = x25519_dalek::StaticSecret::from(phone_secret_bytes);
        let phone_public = x25519_dalek::PublicKey::from(&phone_secret);

        // Build REGISTER_PHONE body
        let label = b"Test Phone";
        let mut body = Vec::with_capacity(33 + label.len());
        body.extend_from_slice(phone_public.as_bytes());
        body.push(label.len() as u8);
        body.extend_from_slice(label);

        let response = handle_register_phone(&body, &identity, &storage, &mut window, 6).await;
        let response = response.expect("should get PHONE_REGISTERED response");

        let (msg_type, resp_body) = parse_ble_envelope(&response).unwrap();
        assert_eq!(msg_type, BLE_MSG_PHONE_REGISTERED);

        // Decrypt: phone derives the same AES key via ECDH + HKDF
        let nonce_bytes = &resp_body[..12];
        let ciphertext = &resp_body[12..];

        // Phone-side ECDH: shared = X25519(phone_secret, gw_x25519_public)
        let (_, gw_x25519_pub) = identity.to_x25519().unwrap();
        let phone_shared_secret = phone_secret.diffie_hellman(&gw_x25519_pub);

        let hkdf = Hkdf::<Sha256>::new(Some(identity.gateway_id()), phone_shared_secret.as_bytes());
        let mut aes_key = [0u8; 32];
        hkdf.expand(HKDF_INFO, &mut aes_key).unwrap();

        let cipher = Aes256Gcm::new_from_slice(&aes_key).unwrap();
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = cipher
            .decrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: ciphertext,
                    aad: identity.gateway_id(),
                },
            )
            .expect("AES-GCM decryption should succeed");

        assert_eq!(plaintext.len(), 36);
        assert_eq!(plaintext[0], 0x00); // status = accepted
        let phone_psk = &plaintext[1..33];
        let key_hint = u16::from_be_bytes([plaintext[33], plaintext[34]]);
        let channel = plaintext[35];

        assert_eq!(channel, 6);
        assert_ne!(phone_psk, &[0u8; 32]); // PSK should be non-zero

        // Verify key_hint = SHA-256(psk)[30..32]
        let psk_hash = Sha256::digest(phone_psk);
        let expected_hint = u16::from_be_bytes([psk_hash[30], psk_hash[31]]);
        assert_eq!(key_hint, expected_hint);

        // Verify phone PSK was stored
        let records = storage.list_phone_psks().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].label, "Test Phone");
        assert_eq!(records[0].phone_key_hint, key_hint);
        assert!(matches!(records[0].status, PhonePskStatus::Active));
    }

    // -- REQUEST_GW_INFO: wrong challenge length --

    #[test]
    fn request_gw_info_wrong_length() {
        let identity = test_identity();
        assert!(handle_request_gw_info(&[0u8; 31], &identity).is_none());
        assert!(handle_request_gw_info(&[0u8; 33], &identity).is_none());
    }

    // -- Registration window auto-close --

    #[test]
    fn window_auto_closes() {
        let mut w = RegistrationWindow::new();
        assert!(!w.is_open());

        w.open(0); // open with 0 duration → immediately expired
                   // Give a tiny margin for the check
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(!w.is_open());
    }
}
