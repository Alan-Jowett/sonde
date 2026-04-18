// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Gateway BLE pairing state machine (Phase 1: phone registration).
//!
//! Processes BLE messages relayed from the modem and implements:
//! - REQUEST_GW_INFO (0x01): Ed25519 challenge-response (GW-1206)
//! - REGISTER_PHONE (0x02): phone PSK registration (GW-1209)
//! - Registration window enforcement (GW-1207/GW-1208)
//!
//! Messages arrive as raw BLE envelope bytes via `BLE_RECV` and responses are
//! sent back as BLE envelope bytes via `BLE_INDICATE`.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use ed25519_dalek::Signer;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
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
    /// JoinHandle for the active open_ble_pairing event-forwarding task.
    event_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Broadcast channel for forwarding BLE pairing events to admin streams.
    event_tx: tokio::sync::broadcast::Sender<BlePairingEventKind>,
}

/// Events broadcast from the BLE loop to admin CLI streams.
#[derive(Debug, Clone)]
pub enum BlePairingEventKind {
    PhoneConnected { peer_addr: [u8; 6], mtu: u16 },
    PhoneDisconnected { peer_addr: [u8; 6] },
    PasskeyRequest { passkey: u32 },
    PhoneRegistered { label: String, phone_key_hint: u16 },
}

impl Default for BlePairingController {
    fn default() -> Self {
        Self::new()
    }
}

impl BlePairingController {
    pub fn new() -> Self {
        let (event_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            window: tokio::sync::Mutex::new(RegistrationWindow::new()),
            passkey_tx: tokio::sync::Mutex::new(None),
            timeout_cancel: tokio::sync::Mutex::new(None),
            event_task: tokio::sync::Mutex::new(None),
            event_tx,
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

    /// Store the JoinHandle of the event-forwarding task spawned by OpenBlePairing.
    pub async fn set_event_task(&self, handle: tokio::task::JoinHandle<()>) {
        if let Some(old) = self.event_task.lock().await.replace(handle) {
            old.abort();
        }
    }

    /// Cancel the timeout token and await the event-forwarding task.
    ///
    /// Used during warm reboot recovery to ensure the task releases its
    /// `Arc<UsbEspNowTransport>` before the transport is dropped.
    pub async fn cancel_and_wait(&self) {
        // Fire the cancel token so the task's cancel arm fires.
        if let Some(token) = self.timeout_cancel.lock().await.take() {
            token.cancel();
        }
        // Await graceful exit up to 500 ms (task sends WindowClosed before
        // returning). If the channel is full or the task is otherwise stuck,
        // abort it so warm-reboot recovery is never blocked indefinitely.
        let handle = self.event_task.lock().await.take();
        if let Some(handle) = handle {
            let abort = handle.abort_handle();
            if tokio::time::timeout(std::time::Duration::from_millis(500), handle)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }

    /// Broadcast a BLE pairing event to all admin CLI subscribers.
    pub fn broadcast_event(&self, event: BlePairingEventKind) {
        // Ignore send errors (no active subscribers).
        let _ = self.event_tx.send(event);
    }

    /// Subscribe to BLE pairing events (used by admin CLI streams).
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<BlePairingEventKind> {
        self.event_tx.subscribe()
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
    controller: Option<&BlePairingController>,
) -> Option<Vec<u8>> {
    let (msg_type, body) = parse_ble_envelope(data)?;

    match msg_type {
        BLE_MSG_REQUEST_GW_INFO => handle_request_gw_info(body, identity),
        BLE_MSG_REGISTER_PHONE => {
            handle_register_phone(body, storage, window, rf_channel, controller).await
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
// REGISTER_PHONE — phone generates PSK
// ---------------------------------------------------------------------------

/// Handle REGISTER_PHONE (AEAD): phone sends its PSK, gateway stores it.
///
/// REGISTER_PHONE body: phone_psk(32) + label_len(1) + label(label_len).
/// PHONE_REGISTERED body: status(1) + rf_channel(1) + phone_key_hint(2 BE).
async fn handle_register_phone(
    body: &[u8],
    storage: &Arc<dyn Storage>,
    window: &mut RegistrationWindow,
    rf_channel: u8,
    controller: Option<&BlePairingController>,
) -> Option<Vec<u8>> {
    const ERROR_GENERIC: u8 = 0x01;

    if !window.is_open() {
        info!("REGISTER_PHONE (AEAD) rejected: registration window closed");
        return encode_ble_envelope(BLE_MSG_ERROR, &[ERROR_WINDOW_CLOSED]);
    }

    if body.len() < 33 {
        warn!(
            len = body.len(),
            "REGISTER_PHONE (AEAD): body too short (min 33 bytes)"
        );
        return encode_ble_envelope(BLE_MSG_ERROR, &[ERROR_GENERIC]);
    }

    let mut phone_psk = Zeroizing::new([0u8; 32]);
    phone_psk.copy_from_slice(&body[..32]);
    let label_len = body[32] as usize;

    if label_len > PHONE_LABEL_MAX_BYTES {
        warn!(
            label_len,
            "REGISTER_PHONE (AEAD): label too long (max 64 bytes)"
        );
        return encode_ble_envelope(BLE_MSG_ERROR, &[ERROR_GENERIC]);
    }

    if body.len() != 33 + label_len {
        warn!(
            expected = 33 + label_len,
            actual = body.len(),
            "REGISTER_PHONE (AEAD): body length mismatch"
        );
        return encode_ble_envelope(BLE_MSG_ERROR, &[ERROR_GENERIC]);
    }

    let label = match std::str::from_utf8(&body[33..33 + label_len]) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            warn!("REGISTER_PHONE (AEAD): label is not valid UTF-8");
            return encode_ble_envelope(BLE_MSG_ERROR, &[ERROR_GENERIC]);
        }
    };

    // Derive phone_key_hint = SHA-256(psk)[30..32] as BE u16
    let psk_hash = Sha256::digest(phone_psk.as_slice());
    let phone_key_hint = u16::from_be_bytes([psk_hash[30], psk_hash[31]]);

    // Store phone PSK
    let record = PhonePskRecord {
        phone_id: 0,
        phone_key_hint,
        psk: phone_psk,
        label: label.clone(),
        issued_at: SystemTime::now(),
        status: PhonePskStatus::Active,
    };

    if let Err(e) = storage.store_phone_psk(&record).await {
        warn!(?e, "REGISTER_PHONE (AEAD): failed to store phone PSK");
        return None;
    }

    info!(
        phone_key_hint,
        label = record.label,
        "phone registered successfully (AEAD)"
    );

    if let Some(ctrl) = controller {
        ctrl.broadcast_event(BlePairingEventKind::PhoneRegistered {
            label,
            phone_key_hint,
        });
    }

    // Build PHONE_REGISTERED response: status(1) + rf_channel(1) + phone_key_hint(2 BE)
    let response = [0x00, rf_channel, psk_hash[30], psk_hash[31]];
    encode_ble_envelope(BLE_MSG_PHONE_REGISTERED, &response)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_identity::GatewayIdentity;
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
