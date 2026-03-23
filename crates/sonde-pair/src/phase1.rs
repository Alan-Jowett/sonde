// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::crypto;
use crate::envelope::{
    build_envelope, parse_envelope, parse_error_body, parse_gw_info_response,
    parse_phone_registered,
};
use crate::error::PairingError;
use crate::rng::RngProvider;
use crate::store::PairingStore;
use crate::transport::{enforce_lesc, BleTransport};
use crate::types::*;
use crate::validation::validate_rf_channel;
use tracing::{debug, info, trace, warn};
use zeroize::Zeroizing;

/// Map a BLE pairing message type byte to its spec name (PT-0702).
fn msg_type_name(t: u8) -> &'static str {
    match t {
        REQUEST_GW_INFO => "REQUEST_GW_INFO",
        GW_INFO_RESPONSE => "GW_INFO_RESPONSE",
        REGISTER_PHONE => "REGISTER_PHONE",
        PHONE_REGISTERED => "PHONE_REGISTERED",
        MSG_ERROR => "ERROR",
        _ => "UNKNOWN",
    }
}

/// Callback for reporting Phase 1 sub-phase progress (PT-0701).
///
/// Phase 1 transitions through these sub-phases in order:
/// - `"Connecting"` — BLE connection and MTU negotiation
/// - `"Authenticating"` — gateway signature verification and TOFU check
/// - `"Registering"` — phone registration and key exchange
pub trait PairingProgress: Send + Sync {
    /// Called when the pairing state machine enters a new sub-phase.
    fn on_phase(&self, phase: &str);
}

/// Phase 1: Pair with a gateway via BLE.
///
/// Establishes trust-on-first-use (TOFU) identity, performs ECDH key exchange,
/// and receives the phone PSK and RF channel from the gateway.
///
/// # Progress reporting (PT-0701)
///
/// If `progress` is `Some`, the callback is invoked at each sub-phase
/// transition: Connecting → Authenticating → Registering.
///
/// # Re-run safety (PT-0600)
///
/// This function takes `&mut` references to `transport` and `store`, which
/// provides compile-time mutual exclusion via the Rust borrow checker.
/// Callers using `Arc<Mutex<..>>` for async sharing get serialized access
/// through the mutex.  Re-running Phase 1 against the same gateway
/// overwrites artifacts cleanly without corrupting local state.
///
/// # Already-paired warning (PT-0601)
///
/// If the store already contains a gateway identity, a `tracing::warn!` is
/// emitted before proceeding.  Callers that need interactive confirmation
/// should call [`crate::store::is_already_paired`] first and prompt the
/// operator.
pub async fn pair_with_gateway(
    transport: &mut dyn BleTransport,
    store: &mut dyn PairingStore,
    rng: &dyn RngProvider,
    device_address: &[u8; 6],
    phone_label: &str,
    progress: Option<&dyn PairingProgress>,
) -> Result<PairingArtifacts, PairingError> {
    // Validate phone label length (spec §5.4: 0–64 bytes)
    if phone_label.len() > 64 {
        return Err(PairingError::InvalidPhoneLabel(format!(
            "phone label must be at most 64 bytes, got {}",
            phone_label.len()
        )));
    }

    // PT-0601: warn if already paired with a gateway.
    if let Some(existing) = store.load_gateway_identity()? {
        warn!(
            gateway_id = ?existing.gateway_id,
            "gateway identity already stored — pairing may overwrite existing state if it succeeds"
        );
    }

    // Step 1: Connect and check MTU
    if let Some(cb) = progress {
        cb.on_phase("Connecting");
    }
    info!("connecting to gateway");
    let mtu = transport.connect(device_address).await?;
    if mtu < BLE_MTU_MIN {
        transport.disconnect().await.ok();
        return Err(PairingError::MtuTooLow {
            negotiated: mtu,
            required: BLE_MTU_MIN,
        });
    }
    debug!(mtu, "connected to gateway");

    // LESC enforcement (PT-0904): reject insecure pairing methods.
    enforce_lesc(transport).await?;

    // Use a closure-like scope to ensure cleanup on error
    let result = do_pair_with_gateway(transport, store, rng, phone_label, progress).await;

    // Step 14: Always disconnect
    transport.disconnect().await.ok();

    result
}

async fn do_pair_with_gateway(
    transport: &mut dyn BleTransport,
    store: &mut dyn PairingStore,
    rng: &dyn RngProvider,
    phone_label: &str,
    progress: Option<&dyn PairingProgress>,
) -> Result<PairingArtifacts, PairingError> {
    // Step 2: Generate 32-byte challenge
    let mut challenge = [0u8; 32];
    rng.fill_bytes(&mut challenge)?;
    trace!("generated 32-byte random challenge");

    // Step 3: Write REQUEST_GW_INFO
    let request =
        build_envelope(REQUEST_GW_INFO, &challenge).ok_or(PairingError::PayloadTooLarge {
            size: challenge.len(),
            max: u16::MAX as usize,
        })?;
    trace!(msg = "REQUEST_GW_INFO", len = request.len(), "BLE write");
    transport
        .write_characteristic(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, &request)
        .await?;

    // Step 4: Read indication (timeout 5s) — enters Authenticating sub-phase
    if let Some(cb) = progress {
        cb.on_phase("Authenticating");
    }
    trace!("waiting for GW_INFO_RESPONSE indication (5 s timeout)");
    let response = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 5000)
        .await?;
    let (msg_type, payload) = parse_envelope(&response)?;
    trace!(
        msg_type = format_args!("0x{msg_type:02x}"),
        msg_name = msg_type_name(msg_type),
        len = payload.len(),
        "BLE indication received"
    );

    // Check for error response
    if msg_type == MSG_ERROR {
        let (status, diagnostic) = parse_error_body(payload);
        let reason = if diagnostic.is_empty() {
            format!("gateway error code 0x{status:02x}")
        } else {
            format!("gateway error code 0x{status:02x}: {diagnostic}")
        };
        return Err(PairingError::GatewayAuthFailed(reason));
    }
    if msg_type != GW_INFO_RESPONSE {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!("expected GW_INFO_RESPONSE (0x81), got 0x{msg_type:02x}"),
        });
    }

    let gw_info = parse_gw_info_response(payload)?;

    // Step 5: Verify Ed25519 signature over (challenge || gateway_id)
    let mut signed_message = Vec::with_capacity(48);
    signed_message.extend_from_slice(&challenge);
    signed_message.extend_from_slice(&gw_info.gateway_id);
    crypto::verify_ed25519_signature(&gw_info.gw_public_key, &signed_message, &gw_info.signature)?;
    info!("gateway signature verified");
    trace!("Ed25519 signature verification succeeded");

    // Step 6: TOFU — check stored identity
    if let Some(stored) = store.load_gateway_identity()? {
        if stored.public_key != gw_info.gw_public_key {
            warn!(
                stored_key = ?stored.public_key,
                presented_key = ?gw_info.gw_public_key,
                "stored gateway public key does not match presented public key"
            );
            return Err(PairingError::PublicKeyMismatch);
        }
        if stored.gateway_id != gw_info.gateway_id {
            warn!(
                stored_gateway_id = ?stored.gateway_id,
                presented_gateway_id = ?gw_info.gateway_id,
                "gateway identity mismatch: stored gateway_id does not match presented gateway_id; if the gateway was reinstalled or reset, clear local pairing and re-pair"
            );
            return Err(PairingError::GatewayIdMismatch);
        }
        debug!("gateway identity matches stored TOFU record");
    } else {
        // Pin the gateway identity immediately so it survives even if
        // later steps (e.g. registration window closed) fail.
        let identity = GatewayIdentity {
            public_key: gw_info.gw_public_key,
            gateway_id: gw_info.gateway_id,
        };
        store.save_gateway_identity(&identity)?;
        debug!("first-time gateway identity — TOFU record pinned");
    }

    // Step 7: Generate ephemeral X25519 keypair — enters Registering sub-phase
    if let Some(cb) = progress {
        cb.on_phase("Registering");
    }
    let (eph_secret, eph_public) = crypto::generate_x25519_keypair(rng)?;
    trace!("generated ephemeral X25519 keypair");

    // Step 8: Write REGISTER_PHONE (ephemeral_pubkey || label_len || label)
    let mut register_body = Vec::with_capacity(32 + 1 + phone_label.len());
    register_body.extend_from_slice(&eph_public);
    register_body.push(phone_label.len() as u8);
    register_body.extend_from_slice(phone_label.as_bytes());
    let register =
        build_envelope(REGISTER_PHONE, &register_body).ok_or(PairingError::PayloadTooLarge {
            size: register_body.len(),
            max: u16::MAX as usize,
        })?;
    trace!(msg = "REGISTER_PHONE", len = register.len(), "BLE write");
    transport
        .write_characteristic(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, &register)
        .await?;

    // Step 9: Read indication (timeout 30s)
    trace!("waiting for PHONE_REGISTERED indication (30 s timeout)");
    let response2 = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 30_000)
        .await?;
    let (msg_type2, payload2) = parse_envelope(&response2)?;
    trace!(
        msg_type = format_args!("0x{msg_type2:02x}"),
        msg_name = msg_type_name(msg_type2),
        len = payload2.len(),
        "BLE indication received (step 9)"
    );

    if msg_type2 == MSG_ERROR {
        let (status, diagnostic) = parse_error_body(payload2);
        return match status {
            0x02 => Err(PairingError::RegistrationWindowClosed),
            0x03 => Err(PairingError::GatewayAlreadyPaired),
            code => {
                let reason = if diagnostic.is_empty() {
                    format!("gateway error code 0x{code:02x}")
                } else {
                    format!("gateway error code 0x{code:02x}: {diagnostic}")
                };
                Err(PairingError::GatewayAuthFailed(reason))
            }
        };
    }
    if msg_type2 != PHONE_REGISTERED {
        return Err(PairingError::InvalidResponse {
            msg_type: msg_type2,
            reason: format!("expected PHONE_REGISTERED (0x82), got 0x{msg_type2:02x}"),
        });
    }

    let phone_reg = parse_phone_registered(payload2)?;

    // Step 10: Convert gw Ed25519 public → X25519, ECDH with gateway static key, HKDF, decrypt
    let gw_x25519 = crypto::ed25519_to_x25519_public(&gw_info.gw_public_key)?;
    let shared_secret = crypto::x25519_ecdh(&eph_secret, &gw_x25519);
    let aes_key = crypto::hkdf_sha256(&shared_secret, &gw_info.gateway_id, b"sonde-phone-reg-v1");

    let decrypted = crypto::aes256gcm_decrypt(
        &aes_key,
        &phone_reg.nonce,
        &phone_reg.ciphertext,
        &gw_info.gateway_id,
    )?;
    trace!("AES-256-GCM decryption succeeded");

    // Step 11: Parse decrypted inner: status[1] + phone_psk[32] + phone_key_hint[2] + rf_channel[1] = 36 bytes
    if decrypted.len() != 36 {
        return Err(PairingError::InvalidResponse {
            msg_type: PHONE_REGISTERED,
            reason: format!(
                "expected 36 bytes in decrypted payload, got {}",
                decrypted.len()
            ),
        });
    }

    let status = decrypted[0];
    if status != 0x00 {
        return Err(PairingError::GatewayAuthFailed(format!(
            "PHONE_REGISTERED inner status: 0x{status:02x}"
        )));
    }

    let mut phone_psk = Zeroizing::new([0u8; 32]);
    phone_psk.copy_from_slice(&decrypted[1..33]);
    let phone_key_hint = u16::from_be_bytes([decrypted[33], decrypted[34]]);
    let rf_channel = decrypted[35];

    // Validate rf_channel (spec: 1–13)
    validate_rf_channel(rf_channel)?;

    // Verify phone_key_hint matches the PSK
    let expected_hint = crate::validation::compute_key_hint(&phone_psk);
    if phone_key_hint != expected_hint {
        return Err(PairingError::InvalidKeyHint);
    }

    // Step 12: Build and save artifacts
    let artifacts = PairingArtifacts {
        gateway_identity: GatewayIdentity {
            public_key: gw_info.gw_public_key,
            gateway_id: gw_info.gateway_id,
        },
        phone_psk,
        phone_key_hint,
        rf_channel,
        phone_label: phone_label.to_string(),
    };
    store.save_artifacts(&artifacts)?;
    info!(
        phone_key_hint,
        rf_channel, "Phase 1 complete — paired with gateway"
    );

    // Step 15: Ephemeral keys dropped and zeroized via Zeroizing wrappers

    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::MockRng;
    use crate::store::MemoryPairingStore;
    use crate::transport::MockBleTransport;
    use crate::validation::compute_key_hint;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha512};
    use tracing_test::traced_test;

    /// Convert an Ed25519 signing key seed to an X25519 static secret.
    fn ed25519_seed_to_x25519_secret(seed: &[u8; 32]) -> x25519_dalek::StaticSecret {
        let hash = Sha512::digest(seed);
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash[..32]);
        x25519_dalek::StaticSecret::from(key)
    }

    /// Build a valid mock GW_INFO_RESPONSE for testing.
    fn build_gw_info_response(
        signing_key: &SigningKey,
        gateway_id: &[u8; 16],
        challenge: &[u8; 32],
    ) -> Vec<u8> {
        let vk = signing_key.verifying_key();
        // Sign (challenge || gateway_id) per spec §5.3
        let mut message = Vec::with_capacity(48);
        message.extend_from_slice(challenge);
        message.extend_from_slice(gateway_id);
        let sig = signing_key.sign(&message);

        let mut body = Vec::with_capacity(112);
        body.extend_from_slice(&vk.to_bytes());
        body.extend_from_slice(gateway_id);
        body.extend_from_slice(&sig.to_bytes());
        build_envelope(GW_INFO_RESPONSE, &body).unwrap()
    }

    /// Build a valid mock PHONE_REGISTERED response for testing.
    fn build_phone_registered_response(
        gw_signing_key: &SigningKey,
        gateway_id: &[u8; 16],
        phone_eph_public: &[u8; 32],
        phone_psk: &[u8; 32],
        rf_channel: u8,
    ) -> Vec<u8> {
        // Convert Ed25519 private → X25519 private (SHA-512(seed)[0..32])
        let gw_x25519_secret = ed25519_seed_to_x25519_secret(&gw_signing_key.to_bytes());

        // ECDH with phone's ephemeral public key
        let shared =
            gw_x25519_secret.diffie_hellman(&x25519_dalek::PublicKey::from(*phone_eph_public));

        // HKDF with spec info string
        let aes_key = crypto::hkdf_sha256(&shared.to_bytes(), gateway_id, b"sonde-phone-reg-v1");

        // Compute phone_key_hint from PSK
        let phone_key_hint = compute_key_hint(phone_psk);

        // Plaintext: status[1] + phone_psk[32] + phone_key_hint[2] + rf_channel[1] = 36 bytes
        let mut plaintext = Vec::with_capacity(36);
        plaintext.push(0x00); // status = accepted
        plaintext.extend_from_slice(phone_psk);
        plaintext.extend_from_slice(&phone_key_hint.to_be_bytes());
        plaintext.push(rf_channel);

        let nonce = [0x01u8; 12];
        let ciphertext =
            crypto::aes256gcm_encrypt(&aes_key, &nonce, &plaintext, gateway_id).unwrap();

        // Wire: nonce[12] + ciphertext (no ephemeral key)
        let mut body = Vec::new();
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&ciphertext);
        build_envelope(PHONE_REGISTERED, &body).unwrap()
    }

    /// The mock RNG seed determines the challenge and ephemeral keypair.
    /// We need to predict what challenge will be generated to pre-sign it.
    fn predicted_challenge(rng: &MockRng) -> [u8; 32] {
        let mut challenge = [0u8; 32];
        rng.fill_bytes(&mut challenge).unwrap();
        challenge
    }

    fn predicted_eph_public(rng: &MockRng) -> [u8; 32] {
        // The RNG fills the secret with seed bytes, then X25519 clamps.
        // We need the actual public key from that secret.
        let mut secret_bytes = [0u8; 32];
        rng.fill_bytes(&mut secret_bytes).unwrap();
        let secret = x25519_dalek::StaticSecret::from(secret_bytes);
        let public = x25519_dalek::PublicKey::from(&secret);
        public.to_bytes()
    }

    #[test]
    fn t_pt_200_happy_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            let artifacts = result.unwrap();

            assert_eq!(*artifacts.phone_psk, phone_psk);
            assert_eq!(artifacts.rf_channel, rf_channel);
            assert_eq!(artifacts.phone_key_hint, compute_key_hint(&phone_psk));
            assert_eq!(
                artifacts.gateway_identity.public_key,
                signing_key.verifying_key().to_bytes()
            );
            assert_eq!(artifacts.gateway_identity.gateway_id, gateway_id);

            // Verify artifacts were stored
            let stored = store.load_artifacts().unwrap().unwrap();
            assert_eq!(*stored.phone_psk, phone_psk);
        });
    }

    #[test]
    fn t_pt_201_signature_verification_failure() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let wrong_key = SigningKey::from_bytes(&[0x43u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            // Build a response with correct public key but signature from wrong key
            let vk_correct = signing_key.verifying_key();
            let mut wrong_msg = Vec::with_capacity(48);
            wrong_msg.extend_from_slice(&challenge);
            wrong_msg.extend_from_slice(&gateway_id);
            let sig_wrong = wrong_key.sign(&wrong_msg);

            let mut body = Vec::with_capacity(112);
            body.extend_from_slice(&vk_correct.to_bytes());
            body.extend_from_slice(&gateway_id);
            body.extend_from_slice(&sig_wrong.to_bytes());
            let response = build_envelope(GW_INFO_RESPONSE, &body).unwrap();

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(response));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(
                result,
                Err(PairingError::SignatureVerificationFailed)
            ));
        });
    }

    #[test]
    fn t_pt_202_tofu_mismatch() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));

            // Pre-store a different gateway identity
            let mut store = MemoryPairingStore::new();
            let different_artifacts = PairingArtifacts {
                gateway_identity: GatewayIdentity {
                    public_key: [0x99u8; 32], // different key
                    gateway_id,
                },
                phone_psk: Zeroizing::new([0x42u8; 32]),
                phone_key_hint: 0x1234,
                rf_channel: 1,
                phone_label: String::new(),
            };
            store.save_artifacts(&different_artifacts).unwrap();

            let device_addr = [0xAA; 6];
            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(result, Err(PairingError::PublicKeyMismatch)));
        });
    }

    #[test]
    fn t_pt_203_registration_window_closed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            // Second response is an error with code 0x02 (registration window closed)
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &[0x02]).unwrap()));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(
                result,
                Err(PairingError::RegistrationWindowClosed)
            ));
        });
    }

    #[test]
    fn t_pt_203b_already_paired_with_gateway() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            // Second response is an error with code 0x03 (already paired)
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &[0x03]).unwrap()));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(result, Err(PairingError::GatewayAlreadyPaired)));
        });
    }

    #[test]
    fn t_pt_204_timeout_gw_info() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // No responses queued — will return IndicationTimeout

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(result, Err(PairingError::IndicationTimeout)));
        });
    }

    #[test]
    fn t_pt_205_timeout_phone_registered() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            // No second response — timeout on PHONE_REGISTERED

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(result, Err(PairingError::IndicationTimeout)));
        });
    }

    #[test]
    fn t_pt_206_decryption_failure() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));

            // Build a PHONE_REGISTERED with garbage ciphertext (no ephemeral key per spec)
            let mut body = Vec::new();
            body.extend_from_slice(&[0x01u8; 12]); // nonce
            body.extend_from_slice(&[0xFFu8; 49]); // garbage ciphertext
            transport.queue_response(Ok(build_envelope(PHONE_REGISTERED, &body).unwrap()));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(result, Err(PairingError::DecryptionFailed)));
        });
    }

    #[test]
    fn t_pt_207_mtu_too_low() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(100); // too low
            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            match result {
                Err(PairingError::MtuTooLow {
                    negotiated,
                    required,
                }) => {
                    assert_eq!(negotiated, 100);
                    assert_eq!(required, BLE_MTU_MIN);
                }
                other => panic!("expected MtuTooLow, got {other:?}"),
            }
        });
    }

    // --- PT-0600: Re-run safety ---

    /// Validates: PT-0600 criterion 1
    ///
    /// Running Phase 1 twice against the same gateway must not corrupt local
    /// state. The second run succeeds and overwrites artifacts cleanly.
    #[test]
    fn t_pt_600_repeated_phase1_does_not_corrupt_state() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            // First pairing.
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            pair_with_gateway(
                &mut transport,
                &mut store,
                &rng,
                &device_addr,
                "first",
                None,
            )
            .await
            .unwrap();
            let first = store.load_artifacts().unwrap().unwrap();
            assert_eq!(first.phone_label, "first");

            // Second pairing (same gateway, same store).
            let rng2 = MockRng::new([0x42u8; 32]);
            let challenge2 = predicted_challenge(&rng2);
            let eph_public2 = predicted_eph_public(&rng2);

            let mut transport2 = MockBleTransport::new(247);
            transport2.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge2,
            )));
            transport2.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public2,
                &phone_psk,
                rf_channel,
            )));

            pair_with_gateway(
                &mut transport2,
                &mut store,
                &rng2,
                &device_addr,
                "second",
                None,
            )
            .await
            .unwrap();

            // State must be consistent — second pairing overwrites cleanly.
            let second = store.load_artifacts().unwrap().unwrap();
            assert_eq!(second.phone_label, "second");
            assert_eq!(*second.phone_psk, phone_psk);
            assert_eq!(second.rf_channel, rf_channel);
        });
    }

    // --- PT-0601: Already-paired detection ---

    /// Validates: PT-0601
    ///
    /// If a gateway identity is already stored, `is_already_paired` returns
    /// `Some(identity)` so the caller can warn the operator.
    #[test]
    fn t_pt_601_already_paired_detection() {
        use crate::store::is_already_paired;

        let mut store = MemoryPairingStore::new();

        // Initially not paired.
        assert!(is_already_paired(&store).unwrap().is_none());

        // After saving a gateway identity.
        let identity = GatewayIdentity {
            public_key: [0x42u8; 32],
            gateway_id: [0x01u8; 16],
        };
        store.save_gateway_identity(&identity).unwrap();

        let existing = is_already_paired(&store).unwrap();
        assert!(existing.is_some(), "should detect existing pairing");
        assert_eq!(existing.unwrap(), identity);
    }

    // --- PT-0701: Phase 1 sub-phase progress reporting ---

    /// Mock progress callback that records the phases it receives.
    struct RecordingProgress {
        phases: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingProgress {
        fn new() -> Self {
            Self {
                phases: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn phases(&self) -> Vec<String> {
            self.phases.lock().unwrap().clone()
        }
    }

    impl PairingProgress for RecordingProgress {
        fn on_phase(&self, phase: &str) {
            self.phases.lock().unwrap().push(phase.to_string());
        }
    }

    /// Validates: PT-0701
    ///
    /// A successful Phase 1 run must invoke the progress callback in order:
    /// Connecting → Authenticating → Registering.
    #[test]
    fn t_pt_701_progress_reports_phases_in_order() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];
            let progress = RecordingProgress::new();

            let result = pair_with_gateway(
                &mut transport,
                &mut store,
                &rng,
                &device_addr,
                "test",
                Some(&progress),
            )
            .await;
            result.unwrap();

            assert_eq!(
                progress.phases(),
                vec!["Connecting", "Authenticating", "Registering"],
            );
        });
    }

    /// Validates: PT-0701 (None case)
    ///
    /// When `progress` is `None`, the pairing succeeds without invoking any
    /// callback — no panic or error from the absent observer.
    #[test]
    fn t_pt_701_progress_none_succeeds() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            // None progress — should succeed without any callback issues.
            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "test", None)
                    .await;
            result.unwrap();
        });
    }

    // --- PT-0502: BLE disconnect on error paths ---

    /// Validates: PT-0502 (BLE disconnect on error)
    ///
    /// After Phase 1 failure (timeout on PHONE_REGISTERED), the BLE connection
    /// must be released.  T-PT-402 checks store integrity; this test asserts
    /// the transport is disconnected.
    #[test]
    fn t_pt_402_disconnect_on_phase1_failure() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            // No second response → timeout on PHONE_REGISTERED

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(result.is_err());

            assert!(
                !transport.connected,
                "BLE connection must be released after Phase 1 failure"
            );
            assert!(
                transport.disconnect_count > 0,
                "disconnect() must be called on error path"
            );
        });
    }

    // --- PT-0904: LESC pairing method enforcement ---

    /// T-PT-109: Just Works fallback rejected at transport layer.
    ///
    /// When the transport rejects the connection because the peripheral only
    /// supports Just Works, Phase 1 must fail with `ConnectionFailed` and
    /// no GATT writes must occur.
    #[test]
    fn t_pt_109_just_works_connect_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.fail_connect = true;

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(
                matches!(result, Err(PairingError::ConnectionFailed(_))),
                "expected ConnectionFailed, got {result:?}"
            );
            assert!(
                transport.written.is_empty(),
                "no GATT writes should occur when connect fails"
            );
        });
    }

    /// T-PT-804: Numeric Comparison enforced — pairing succeeds.
    ///
    /// When the transport reports Numeric Comparison as the pairing method,
    /// Phase 1 must proceed normally and complete.
    #[test]
    fn t_pt_804_numeric_comparison_enforced() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.pairing_method = Some(PairingMethod::NumericComparison);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(
                result.is_ok(),
                "pairing should succeed with NumericComparison"
            );
        });
    }

    /// T-PT-805: Just Works fallback rejected at application layer.
    ///
    /// When the transport silently falls back to Just Works (connect succeeds
    /// but `pairing_method()` returns `JustWorks`), Phase 1 must reject the
    /// connection before sending `REQUEST_GW_INFO` and report an insecure
    /// pairing method error.
    #[test]
    fn t_pt_805_just_works_fallback_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.pairing_method = Some(PairingMethod::JustWorks);

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(
                matches!(
                    result,
                    Err(PairingError::InsecurePairingMethod {
                        method: PairingMethod::JustWorks
                    })
                ),
                "expected InsecurePairingMethod(JustWorks), got {result:?}"
            );
            assert!(
                transport.written.is_empty(),
                "no GATT writes should occur before LESC check"
            );
        });
    }

    // --- PT-0702: Verbose diagnostic mode ---

    /// T-PT-112: Verbose mode includes message type names but never key
    /// material.
    ///
    /// Enables tracing at TRACE level, runs a full Phase 1, and asserts:
    /// 1. Message type names appear in logs.
    /// 2. Raw key bytes never appear in logs.
    #[traced_test]
    #[test]
    fn t_pt_112_verbose_mode() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None)
                .await
                .unwrap();
        });

        // PT-0702 §1: Verbose output includes message type names.
        assert!(logs_contain("REQUEST_GW_INFO"));
        assert!(logs_contain("GW_INFO_RESPONSE"));
        assert!(logs_contain("REGISTER_PHONE"));

        // PT-0702 §2: Key material never appears in verbose output.
        // The phone PSK in hex would be "5555..." (0x55 repeated).
        let psk_hex = "5555555555555555555555555555555555555555555555555555555555555555";
        assert!(
            !logs_contain(psk_hex),
            "phone PSK must not appear in verbose output"
        );
    }

    // --- PT-1000: Transient failure tolerance ---

    /// Validates: PT-1000
    ///
    /// Inject a BLE disconnect during Phase 1 GATT read (ConnectionDropped),
    /// then verify the tool returns an error cleanly without crashing.
    #[test]
    fn t_pt_800_phase1_connection_dropped_during_read() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // Inject ConnectionDropped on indication read
            transport.queue_response(Err(PairingError::ConnectionDropped));

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(
                matches!(result, Err(PairingError::ConnectionDropped)),
                "expected ConnectionDropped, got {result:?}"
            );
            assert!(
                !transport.connected,
                "transport must be disconnected after failure"
            );

            // Verify operator can retry: create a fresh transport.
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rng2 = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng2);
            let eph_public = predicted_eph_public(&rng2);

            let mut transport2 = MockBleTransport::new(247);
            transport2.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport2.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                6,
            )));

            let result2 =
                pair_with_gateway(&mut transport2, &mut store, &rng2, &device_addr, "", None).await;
            assert!(result2.is_ok(), "retry after disconnect must succeed");
        });
    }

    // --- PT-1001: No resource leaks on failure ---

    /// Validates: PT-1001
    ///
    /// Run 10 consecutive Phase 1 attempts that fail at different stages.
    /// After each failure, verify no open connections remain.
    #[test]
    fn t_pt_801_no_resource_leaks_phase1() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let device_addr = [0xAA; 6];

            // Each iteration fails at a different stage.
            let failure_scenarios: Vec<Box<dyn Fn() -> MockBleTransport>> = vec![
                // 1. Connection failure
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.connect_error = Some(PairingError::ConnectionFailed("test".into()));
                    t
                }),
                // 2. MTU too low
                Box::new(|| MockBleTransport::new(100)),
                // 3. Timeout on GW_INFO_RESPONSE
                Box::new(|| MockBleTransport::new(247)),
                // 4. ConnectionDropped during read
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Err(PairingError::ConnectionDropped));
                    t
                }),
                // 5. Signature verification failure (wrong key)
                Box::new(|| {
                    let rng = MockRng::new([0x42u8; 32]);
                    let challenge = predicted_challenge(&rng);
                    let wrong_key = SigningKey::from_bytes(&[0x43u8; 32]);
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_gw_info_response(
                        &wrong_key,
                        &gateway_id,
                        &challenge,
                    )));
                    t
                }),
                // 6. GATT write failure
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.write_error = Some(PairingError::GattWriteFailed("test".into()));
                    t
                }),
                // 7. Timeout on PHONE_REGISTERED
                Box::new(|| {
                    let rng = MockRng::new([0x42u8; 32]);
                    let challenge = predicted_challenge(&rng);
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_gw_info_response(
                        &signing_key,
                        &gateway_id,
                        &challenge,
                    )));
                    t
                }),
                // 8. Decryption failure
                Box::new(|| {
                    let rng = MockRng::new([0x42u8; 32]);
                    let challenge = predicted_challenge(&rng);
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_gw_info_response(
                        &signing_key,
                        &gateway_id,
                        &challenge,
                    )));
                    let mut bad_body = Vec::new();
                    bad_body.extend_from_slice(&[0x01u8; 12]);
                    bad_body.extend_from_slice(&[0xFFu8; 49]);
                    t.queue_response(Ok(build_envelope(PHONE_REGISTERED, &bad_body).unwrap()));
                    t
                }),
                // 9. Registration window closed
                Box::new(|| {
                    let rng = MockRng::new([0x42u8; 32]);
                    let challenge = predicted_challenge(&rng);
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_gw_info_response(
                        &signing_key,
                        &gateway_id,
                        &challenge,
                    )));
                    t.queue_response(Ok(build_envelope(MSG_ERROR, &[0x02]).unwrap()));
                    t
                }),
                // 10. Already-paired error
                Box::new(|| {
                    let rng = MockRng::new([0x42u8; 32]);
                    let challenge = predicted_challenge(&rng);
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_gw_info_response(
                        &signing_key,
                        &gateway_id,
                        &challenge,
                    )));
                    t.queue_response(Ok(build_envelope(MSG_ERROR, &[0x03]).unwrap()));
                    t
                }),
            ];

            for (i, make_transport) in failure_scenarios.iter().enumerate() {
                let mut transport = make_transport();
                let rng = MockRng::new([0x42u8; 32]);
                let mut store = MemoryPairingStore::new();

                let result =
                    pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None)
                        .await;
                assert!(result.is_err(), "scenario {i} should fail");
                assert!(
                    !transport.connected,
                    "scenario {i}: transport must not be connected after failure"
                );
            }
        });
    }

    // --- PT-1002: Connection timeout exercised ---

    /// Validates: PT-1002
    ///
    /// Exercise the BLE connection timeout path by injecting a connection
    /// failure. The existing T-PT-802 checks constant values; this test
    /// verifies the code path that handles a connection-level error.
    #[test]
    fn t_pt_802b_connection_timeout_exercised() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.connect_error = Some(PairingError::Timeout {
                operation: "BLE connection",
                duration_secs: 10,
            });

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            match result {
                Err(PairingError::Timeout {
                    operation,
                    duration_secs,
                }) => {
                    assert_eq!(operation, "BLE connection");
                    assert_eq!(duration_secs, 10);
                }
                other => panic!("expected Timeout, got {other:?}"),
            }
            // Connection never established so no disconnect needed,
            // but transport must not be in connected state.
            assert!(!transport.connected);
        });
    }

    // --- PT-1003: No implicit retries ---

    /// Validates: PT-1003
    ///
    /// Inject a GATT write failure on the first REQUEST_GW_INFO write.
    /// Assert that exactly one write was recorded (the failed write is not
    /// retried) and the error propagates to the caller.
    #[test]
    fn t_pt_803_no_implicit_retries_phase1_write() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.write_error = Some(PairingError::GattWriteFailed("BLE write failed".into()));

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(
                matches!(result, Err(PairingError::GattWriteFailed(_))),
                "expected GattWriteFailed, got {result:?}"
            );
            assert_eq!(
                transport.written.len(),
                1,
                "exactly one write attempt — the failed write must not be silently retried"
            );
        });
    }

    /// Validates: PT-1003
    ///
    /// Inject a GATT read failure (indication timeout) during Phase 1.
    /// Assert the error propagates immediately without a retry.
    #[test]
    fn t_pt_803_no_implicit_retries_phase1_read() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // No responses queued → IndicationTimeout on first read

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "", None).await;
            assert!(matches!(result, Err(PairingError::IndicationTimeout)));
            // Exactly one write (REQUEST_GW_INFO) before the read timeout.
            assert_eq!(
                transport.written.len(),
                1,
                "exactly one write before timeout — no retry of the read"
            );
            // And exactly one read_indication call — the timeout must not trigger a retry.
            assert_eq!(
                transport.read_call_count, 1,
                "exactly one read_indication call — no implicit retries on timeout"
            );
        });
    }

    // --- PT-0301: Challenge uniqueness across attempts ---

    /// Two pairing attempts with different RNG seeds must produce different
    /// challenges in the REQUEST_GW_INFO write.
    #[test]
    fn t_pt_208_challenge_uniqueness() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            // Attempt 1 with seed 0x42
            let rng1 = MockRng::new([0x42u8; 32]);
            let challenge1 = predicted_challenge(&rng1);
            let eph_public1 = predicted_eph_public(&rng1);
            let mut transport1 = MockBleTransport::new(247);
            transport1.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge1,
            )));
            transport1.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public1,
                &phone_psk,
                rf_channel,
            )));
            let mut store1 = MemoryPairingStore::new();
            pair_with_gateway(&mut transport1, &mut store1, &rng1, &[0xAA; 6], "", None)
                .await
                .unwrap();

            // Attempt 2 with seed 0x43
            let rng2 = MockRng::new([0x43u8; 32]);
            let challenge2 = predicted_challenge(&rng2);
            let eph_public2 = predicted_eph_public(&rng2);
            let mut transport2 = MockBleTransport::new(247);
            transport2.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge2,
            )));
            transport2.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public2,
                &phone_psk,
                rf_channel,
            )));
            let mut store2 = MemoryPairingStore::new();
            pair_with_gateway(&mut transport2, &mut store2, &rng2, &[0xAA; 6], "", None)
                .await
                .unwrap();

            // Extract challenges from REQUEST_GW_INFO writes
            assert!(
                !transport1.written.is_empty(),
                "transport1 must have at least one written frame"
            );
            assert!(
                !transport2.written.is_empty(),
                "transport2 must have at least one written frame"
            );
            let challenge_payload1 = transport1
                .written
                .iter()
                .find_map(|(_, _, data)| {
                    let (msg_type, payload) = parse_envelope(data).unwrap();
                    if msg_type == REQUEST_GW_INFO {
                        Some(payload.to_vec())
                    } else {
                        None
                    }
                })
                .expect("transport1 must contain a REQUEST_GW_INFO frame");
            let challenge_payload2 = transport2
                .written
                .iter()
                .find_map(|(_, _, data)| {
                    let (msg_type, payload) = parse_envelope(data).unwrap();
                    if msg_type == REQUEST_GW_INFO {
                        Some(payload.to_vec())
                    } else {
                        None
                    }
                })
                .expect("transport2 must contain a REQUEST_GW_INFO frame");

            assert_ne!(
                challenge_payload1, challenge_payload2,
                "challenges from different RNG seeds must differ"
            );
        });
    }

    // --- PT-0302: Same gw_public_key + different gateway_id ---

    /// TOFU check must reject a response with the same public key but a
    /// different `gateway_id` from what is stored.
    #[test]
    fn t_pt_209_tofu_gateway_id_mismatch() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let original_gw_id = [0x01u8; 16];
            let different_gw_id = [0x02u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            // Pre-store a gateway identity with original_gw_id.
            let mut store = MemoryPairingStore::new();
            let identity = GatewayIdentity {
                public_key: signing_key.verifying_key().to_bytes(),
                gateway_id: original_gw_id,
            };
            store.save_gateway_identity(&identity).unwrap();

            // Gateway responds with the same public key but different gateway_id.
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &different_gw_id,
                &challenge,
            )));

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &[0xAA; 6], "", None).await;
            assert!(
                matches!(result, Err(PairingError::GatewayIdMismatch)),
                "same public key + different gateway_id must be a TOFU violation, got {result:?}"
            );
        });
    }

    // --- PT-0303 / §4.1.1: ERROR(0x01) generic error code ---

    /// ERROR(0x01) at the GW_INFO_RESPONSE step should be mapped to
    /// `GatewayAuthFailed`.
    #[test]
    fn t_pt_210_error_0x01_at_gw_info() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // Gateway responds with ERROR(0x01) instead of GW_INFO_RESPONSE
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &[0x01]).unwrap()));

            let rng = MockRng::new([0x42u8; 32]);
            let mut store = MemoryPairingStore::new();

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &[0xAA; 6], "", None).await;
            match result {
                Err(PairingError::GatewayAuthFailed(reason)) => {
                    assert!(
                        reason.contains("0x01"),
                        "error message should include status code: {reason}"
                    );
                }
                other => panic!("expected GatewayAuthFailed, got {other:?}"),
            }
        });
    }

    /// §4.1.1: ERROR(0x01) at the PHONE_REGISTERED step should be mapped to
    /// `GatewayAuthFailed` (not a specific error like RegistrationWindowClosed).
    #[test]
    fn t_pt_211_error_0x01_at_phone_registered() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            // Second response: ERROR(0x01) generic
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &[0x01]).unwrap()));

            let mut store = MemoryPairingStore::new();

            let result =
                pair_with_gateway(&mut transport, &mut store, &rng, &[0xAA; 6], "", None).await;
            match result {
                Err(PairingError::GatewayAuthFailed(reason)) => {
                    assert!(
                        reason.contains("0x01"),
                        "error message should include status code: {reason}"
                    );
                }
                other => panic!("expected GatewayAuthFailed for generic error, got {other:?}"),
            }
        });
    }

    // --- PT-0405: Fresh ephemeral per attempt (Phase 1) ---

    /// Two Phase 1 attempts must use different ephemeral public keys in
    /// the REGISTER_PHONE write.
    #[test]
    fn t_pt_212_fresh_ephemeral_per_attempt() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            // Attempt 1
            let rng1 = MockRng::new([0x42u8; 32]);
            let challenge1 = predicted_challenge(&rng1);
            let eph_public1 = predicted_eph_public(&rng1);
            let mut transport1 = MockBleTransport::new(247);
            transport1.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge1,
            )));
            transport1.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public1,
                &phone_psk,
                rf_channel,
            )));
            let mut store1 = MemoryPairingStore::new();
            pair_with_gateway(&mut transport1, &mut store1, &rng1, &[0xAA; 6], "", None)
                .await
                .unwrap();

            // Attempt 2 with different seed
            let rng2 = MockRng::new([0x43u8; 32]);
            let challenge2 = predicted_challenge(&rng2);
            let eph_public2 = predicted_eph_public(&rng2);
            let mut transport2 = MockBleTransport::new(247);
            transport2.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge2,
            )));
            transport2.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public2,
                &phone_psk,
                rf_channel,
            )));
            let mut store2 = MemoryPairingStore::new();
            pair_with_gateway(&mut transport2, &mut store2, &rng2, &[0xAA; 6], "", None)
                .await
                .unwrap();

            // Extract ephemeral public keys from REGISTER_PHONE writes (2nd write)
            assert!(
                transport1.written.len() >= 2,
                "transport1 should have at least 2 writes"
            );
            assert!(
                transport2.written.len() >= 2,
                "transport2 should have at least 2 writes"
            );
            let (_, _, reg1) = &transport1.written[1];
            let (_, _, reg2) = &transport2.written[1];
            let (msg_type1, reg_payload1) = parse_envelope(reg1).unwrap();
            let (msg_type2, reg_payload2) = parse_envelope(reg2).unwrap();
            assert_eq!(
                msg_type1, REGISTER_PHONE,
                "second write must be REGISTER_PHONE"
            );
            assert_eq!(
                msg_type2, REGISTER_PHONE,
                "second write must be REGISTER_PHONE"
            );

            // First 32 bytes of REGISTER_PHONE body are the ephemeral public key
            assert!(
                reg_payload1.len() >= 32,
                "REGISTER_PHONE payload1 too short: len={}",
                reg_payload1.len()
            );
            assert!(
                reg_payload2.len() >= 32,
                "REGISTER_PHONE payload2 too short: len={}",
                reg_payload2.len()
            );
            assert_ne!(
                &reg_payload1[..32],
                &reg_payload2[..32],
                "ephemeral public keys must differ between attempts"
            );
        });
    }

    // --- §5.2: REQUEST_GW_INFO body verification ---

    /// The REQUEST_GW_INFO write must contain exactly 32 bytes of challenge data.
    #[test]
    fn t_pt_213_request_gw_info_body_is_32_bytes() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
            let gateway_id = [0x01u8; 16];
            let phone_psk = [0x55u8; 32];
            let rf_channel = 6u8;

            let rng = MockRng::new([0x42u8; 32]);
            let challenge = predicted_challenge(&rng);
            let eph_public = predicted_eph_public(&rng);

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &signing_key,
                &gateway_id,
                &challenge,
            )));
            transport.queue_response(Ok(build_phone_registered_response(
                &signing_key,
                &gateway_id,
                &eph_public,
                &phone_psk,
                rf_channel,
            )));

            let mut store = MemoryPairingStore::new();
            pair_with_gateway(&mut transport, &mut store, &rng, &[0xAA; 6], "", None)
                .await
                .unwrap();

            // First write is REQUEST_GW_INFO
            let (_, _, data) = &transport.written[0];
            let (msg_type, payload) = parse_envelope(data).unwrap();
            assert_eq!(msg_type, REQUEST_GW_INFO);
            assert_eq!(
                payload.len(),
                32,
                "REQUEST_GW_INFO body must be exactly 32 bytes (challenge), got {}",
                payload.len()
            );

            // Challenge bytes should be non-zero (from MockRng with 0x42 seed)
            assert!(
                payload.iter().any(|&b| b != 0),
                "challenge must not be all zeros"
            );
        });
    }
}
