// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::crypto;
use crate::envelope::{
    build_envelope, parse_envelope, parse_gw_info_response, parse_phone_registered,
};
use crate::error::PairingError;
use crate::rng::RngProvider;
use crate::store::PairingStore;
use crate::transport::BleTransport;
use crate::types::*;
use crate::validation::compute_key_hint;
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
) -> Result<PairingArtifacts, PairingError> {
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
    let result = do_pair_with_gateway(transport, store, rng).await;

    // Step 14: Always disconnect
    transport.disconnect().await.ok();

    result
}

async fn do_pair_with_gateway(
    transport: &mut dyn BleTransport,
    store: &mut dyn PairingStore,
    rng: &dyn RngProvider,
) -> Result<PairingArtifacts, PairingError> {
    // Step 2: Generate 32-byte challenge
    let mut challenge = [0u8; 32];
    rng.fill_bytes(&mut challenge)?;
    debug!("generated challenge");

    // Step 3: Write REQUEST_GW_INFO
    let request = build_envelope(REQUEST_GW_INFO, &challenge);
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
        return Err(PairingError::GatewayAuthFailed(
            "gateway returned error".into(),
        ));
    }
    if msg_type != GW_INFO_RESPONSE {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!("expected GW_INFO_RESPONSE (0x81), got 0x{msg_type:02x}"),
        });
    }

    let gw_info = parse_gw_info_response(payload)?;

    // Step 5: Verify Ed25519 signature over challenge
    crypto::verify_ed25519_signature(&gw_info.gw_public_key, &challenge, &gw_info.signature)?;
    info!("gateway signature verified");

    // Step 6: TOFU — check stored identity
    if let Some(stored) = store.load_gateway_identity()? {
        if stored.public_key != gw_info.gw_public_key {
            return Err(PairingError::PublicKeyMismatch);
        }
        debug!("gateway identity matches stored TOFU record");
    } else {
        debug!("first-time gateway identity — storing TOFU record");
    }

    // Step 7: Generate ephemeral X25519 keypair
    let (eph_secret, eph_public) = crypto::generate_x25519_keypair(rng)?;

    // Step 8: Write REGISTER_PHONE
    let register = build_envelope(REGISTER_PHONE, &eph_public);
    transport
        .write_characteristic(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, &register)
        .await?;

    // Step 9: Read indication (timeout 30s)
    let response2 = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 30_000)
        .await?;
    let (msg_type2, payload2) = parse_envelope(&response2)?;

    if msg_type2 == MSG_ERROR {
        let reason = if !payload2.is_empty() {
            match payload2[0] {
                0x02 => return Err(PairingError::RegistrationWindowClosed),
                code => format!("gateway error code 0x{code:02x}"),
            }
        } else {
            "gateway error (no details)".into()
        };
        return Err(PairingError::GatewayAuthFailed(reason));
    }
    if msg_type2 != PHONE_REGISTERED {
        return Err(PairingError::InvalidResponse {
            msg_type: msg_type2,
            reason: format!("expected PHONE_REGISTERED (0x82), got 0x{msg_type2:02x}"),
        });
    }

    let phone_reg = parse_phone_registered(payload2)?;

    // Step 10: Convert gw Ed25519 public → X25519, ECDH, HKDF, decrypt
    let gw_x25519 = crypto::ed25519_to_x25519_public(&gw_info.gw_public_key)?;
    // ECDH with the gateway's ephemeral X25519 public key
    let shared_secret = crypto::x25519_ecdh(&eph_secret, &phone_reg.gw_ephemeral_public_key);
    let aes_key = crypto::hkdf_sha256(&shared_secret, &gw_info.gateway_id, b"sonde-phone-pair-v1");

    let decrypted = crypto::aes256gcm_decrypt(
        &aes_key,
        &phone_reg.nonce,
        &phone_reg.ciphertext,
        &gw_x25519,
    )?;

    // Step 11: Parse decrypted: phone_psk [32] + rf_channel [1]
    if decrypted.len() != 33 {
        return Err(PairingError::InvalidResponse {
            msg_type: PHONE_REGISTERED,
            reason: format!(
                "expected 33 bytes in decrypted payload, got {}",
                decrypted.len()
            ),
        });
    }

    let mut phone_psk = Zeroizing::new([0u8; 32]);
    phone_psk.copy_from_slice(&decrypted[..32]);
    let rf_channel = decrypted[32];

    // Step 12: Compute phone_key_hint
    let phone_key_hint = compute_key_hint(&phone_psk);

    // Step 13: Build and save artifacts
    let artifacts = PairingArtifacts {
        gateway_identity: GatewayIdentity {
            public_key: gw_info.gw_public_key,
            gateway_id: gw_info.gateway_id,
        },
        phone_psk,
        phone_key_hint,
        rf_channel,
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
    use ed25519_dalek::{Signer, SigningKey};

    /// Build a valid mock GW_INFO_RESPONSE for testing.
    fn build_gw_info_response(
        signing_key: &SigningKey,
        gateway_id: &[u8; 16],
        challenge: &[u8; 32],
    ) -> Vec<u8> {
        let vk = signing_key.verifying_key();
        let sig = signing_key.sign(challenge);

        let mut response = Vec::new();
        response.push(GW_INFO_RESPONSE);
        response.extend_from_slice(&vk.to_bytes());
        response.extend_from_slice(gateway_id);
        response.extend_from_slice(&sig.to_bytes());
        response
    }

    /// Build a valid mock PHONE_REGISTERED response for testing.
    fn build_phone_registered_response(
        gw_signing_key: &SigningKey,
        gateway_id: &[u8; 16],
        phone_eph_public: &[u8; 32],
        phone_psk: &[u8; 32],
        rf_channel: u8,
    ) -> Vec<u8> {
        // Gateway generates its own ephemeral keypair
        let gw_eph_secret = x25519_dalek::StaticSecret::from([0x44u8; 32]);
        let gw_eph_public = x25519_dalek::PublicKey::from(&gw_eph_secret);

        // Derive the shared secret and AES key
        let shared =
            gw_eph_secret.diffie_hellman(&x25519_dalek::PublicKey::from(*phone_eph_public));

        let aes_key = crypto::hkdf_sha256(&shared.to_bytes(), gateway_id, b"sonde-phone-pair-v1");

        // AAD is the gateway's X25519 public key derived from Ed25519
        let gw_x25519 =
            crypto::ed25519_to_x25519_public(&gw_signing_key.verifying_key().to_bytes()).unwrap();

        // Plaintext: phone_psk + rf_channel
        let mut plaintext = Vec::with_capacity(33);
        plaintext.extend_from_slice(phone_psk);
        plaintext.push(rf_channel);

        let nonce = [0x01u8; 12];
        let ciphertext =
            crypto::aes256gcm_encrypt(&aes_key, &nonce, &plaintext, &gw_x25519).unwrap();

        let mut response = Vec::new();
        response.push(PHONE_REGISTERED);
        response.extend_from_slice(&gw_eph_public.to_bytes());
        response.extend_from_slice(&nonce);
        response.extend_from_slice(&ciphertext);
        response
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

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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

            // Sign with wrong key
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_gw_info_response(
                &wrong_key,
                &gateway_id,
                &challenge,
            )));

            // The response has wrong_key's public key, but we don't have
            // a valid signature from *that* key for our challenge either,
            // actually wrong_key.sign(challenge) IS valid for wrong_key's public key.
            // What we want is: sign with one key, present another key's public key.
            // Let's build a manual response.
            let vk_correct = signing_key.verifying_key();
            let sig_wrong = wrong_key.sign(&challenge);

            let mut response = Vec::new();
            response.push(GW_INFO_RESPONSE);
            response.extend_from_slice(&vk_correct.to_bytes()); // public key of correct
            response.extend_from_slice(&gateway_id);
            response.extend_from_slice(&sig_wrong.to_bytes()); // signature from wrong key

            // Reset transport with correct bad response
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(response));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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
            };
            store.save_artifacts(&different_artifacts).unwrap();

            let device_addr = [0xAA; 6];
            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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
            transport.queue_response(Ok(vec![MSG_ERROR, 0x02]));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
            assert!(matches!(
                result,
                Err(PairingError::RegistrationWindowClosed)
            ));
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

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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

            // Build a PHONE_REGISTERED with garbage ciphertext
            let gw_eph_secret = x25519_dalek::StaticSecret::from([0x44u8; 32]);
            let gw_eph_public = x25519_dalek::PublicKey::from(&gw_eph_secret);
            let mut bad_response = Vec::new();
            bad_response.push(PHONE_REGISTERED);
            bad_response.extend_from_slice(&gw_eph_public.to_bytes());
            bad_response.extend_from_slice(&[0x01u8; 12]); // nonce
            bad_response.extend_from_slice(&[0xFFu8; 49]); // garbage ciphertext
            transport.queue_response(Ok(bad_response));

            let mut store = MemoryPairingStore::new();
            let device_addr = [0xAA; 6];

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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

            let result = pair_with_gateway(&mut transport, &mut store, &rng, &device_addr).await;
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
