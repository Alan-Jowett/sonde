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
use crate::transport::BleTransport;
use crate::types::*;
use crate::validation::validate_rf_channel;
use tracing::{debug, info};
use zeroize::Zeroizing;

/// Phase 1: Pair with a gateway via BLE.
///
/// Establishes trust-on-first-use (TOFU) identity, performs ECDH key exchange,
/// and receives the phone PSK and RF channel from the gateway.
pub async fn pair_with_gateway(
    transport: &mut dyn BleTransport,
    store: &mut dyn PairingStore,
    rng: &dyn RngProvider,
    device_address: &[u8; 6],
    phone_label: &str,
) -> Result<PairingArtifacts, PairingError> {
    // Validate phone label length (spec §5.4: 0–64 bytes)
    if phone_label.len() > 64 {
        return Err(PairingError::InvalidPhoneLabel(format!(
            "phone label must be at most 64 bytes, got {}",
            phone_label.len()
        )));
    }

    // Step 1: Connect and check MTU
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

    // Use a closure-like scope to ensure cleanup on error
    let result = do_pair_with_gateway(transport, store, rng, phone_label).await;

    // Step 14: Always disconnect
    transport.disconnect().await.ok();

    result
}

async fn do_pair_with_gateway(
    transport: &mut dyn BleTransport,
    store: &mut dyn PairingStore,
    rng: &dyn RngProvider,
    phone_label: &str,
) -> Result<PairingArtifacts, PairingError> {
    // Step 2: Generate 32-byte challenge
    let mut challenge = [0u8; 32];
    rng.fill_bytes(&mut challenge)?;
    debug!("generated challenge");

    // Step 3: Write REQUEST_GW_INFO
    let request =
        build_envelope(REQUEST_GW_INFO, &challenge).ok_or(PairingError::PayloadTooLarge {
            size: challenge.len(),
            max: u16::MAX as usize,
        })?;
    transport
        .write_characteristic(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, &request)
        .await?;

    // Step 4: Read indication (timeout 5s)
    let response = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 5000)
        .await?;
    let (msg_type, payload) = parse_envelope(&response)?;

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

    // Step 6: TOFU — check stored identity
    if let Some(stored) = store.load_gateway_identity()? {
        if stored.public_key != gw_info.gw_public_key {
            return Err(PairingError::PublicKeyMismatch);
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

    // Step 7: Generate ephemeral X25519 keypair
    let (eph_secret, eph_public) = crypto::generate_x25519_keypair(rng)?;

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
    transport
        .write_characteristic(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, &register)
        .await?;

    // Step 9: Read indication (timeout 30s)
    let response2 = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 30_000)
        .await?;
    let (msg_type2, payload2) = parse_envelope(&response2)?;

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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
                pair_with_gateway(&mut transport, &mut store, &rng, &device_addr, "").await;
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
}
