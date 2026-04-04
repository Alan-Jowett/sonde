// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::envelope::{build_envelope, parse_envelope, parse_error_body};
use crate::error::PairingError;
use crate::rng::RngProvider;
use crate::transport::{enforce_lesc, BleTransport};
use crate::types::*;
use crate::validation::validate_rf_channel;
use tracing::{debug, info, trace};
use zeroize::Zeroizing;

/// Callback for reporting Phase 1 sub-phase progress (PT-0701).
///
/// ## AEAD flow (`pair_with_gateway_aead`)
///
/// Transitions through these sub-phases in order:
/// - `"Connecting"` — BLE connection and MTU negotiation
/// - `"Registering"` — phone PSK generation and gateway registration
pub trait PairingProgress: Send + Sync {
    /// Called when the pairing state machine enters a new sub-phase.
    fn on_phase(&self, phase: &str);
}

/// Simplified Phase 1: Register phone with gateway via BLE (AES-GCM codec).
///
/// In this flow, the phone generates the PSK locally and sends it to
/// the gateway over BLE LESC. No ECDH, HKDF, or gateway keypair is
/// required. The gateway responds with `status`, `rf_channel`, and
/// `phone_key_hint`.
///
/// # Wire format
///
/// REGISTER_PHONE body: `phone_psk(32) ‖ label_len(1) ‖ label(0–64)`
///
/// PHONE_REGISTERED body: `status(1) ‖ rf_channel(1) ‖ phone_key_hint(2 BE)`
pub async fn pair_with_gateway_aead(
    transport: &mut dyn BleTransport,
    rng: &dyn RngProvider,
    device_address: &[u8; 6],
    phone_label: &str,
    progress: Option<&dyn PairingProgress>,
) -> Result<PairingArtifactsAead, PairingError> {
    if phone_label.len() > 64 {
        return Err(PairingError::InvalidPhoneLabel(format!(
            "phone label must be at most 64 bytes, got {}",
            phone_label.len()
        )));
    }

    // Step 1: Connect and check MTU
    if let Some(cb) = progress {
        cb.on_phase("Connecting");
    }
    debug!(address = ?device_address, "connecting to gateway (AEAD)");
    let mtu = transport.connect(device_address).await?;
    if mtu < BLE_MTU_MIN {
        transport.disconnect().await.ok();
        return Err(PairingError::MtuTooLow {
            negotiated: mtu,
            required: BLE_MTU_MIN,
        });
    }
    debug!(mtu, "connected to gateway");

    enforce_lesc(transport).await?;

    let result = do_pair_with_gateway_aead(transport, rng, phone_label, progress).await;

    transport.disconnect().await.ok();
    result
}

/// Inner implementation for the AEAD Phase 1 flow.
async fn do_pair_with_gateway_aead(
    transport: &mut dyn BleTransport,
    rng: &dyn RngProvider,
    phone_label: &str,
    progress: Option<&dyn PairingProgress>,
) -> Result<PairingArtifactsAead, PairingError> {
    // Step 2: Generate phone PSK
    if let Some(cb) = progress {
        cb.on_phase("Registering");
    }
    let mut phone_psk = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut *phone_psk)?;
    trace!("generated 32-byte phone PSK");

    // Step 3: Write REGISTER_PHONE (phone_psk || label_len || label)
    let mut register_body = Vec::with_capacity(32 + 1 + phone_label.len());
    register_body.extend_from_slice(&*phone_psk);
    register_body.push(phone_label.len() as u8);
    register_body.extend_from_slice(phone_label.as_bytes());
    let register =
        build_envelope(REGISTER_PHONE, &register_body).ok_or(PairingError::PayloadTooLarge {
            size: register_body.len(),
            max: u16::MAX as usize,
        })?;
    trace!(
        msg = "REGISTER_PHONE",
        len = register.len(),
        "BLE write (AEAD)"
    );
    transport
        .write_characteristic(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, &register)
        .await?;

    // Step 4: Read indication (timeout 30s)
    trace!("waiting for PHONE_REGISTERED indication (30 s timeout)");
    let response = transport
        .read_indication(GATEWAY_SERVICE_UUID, GATEWAY_COMMAND_UUID, 30_000)
        .await?;
    let (msg_type, payload) = parse_envelope(&response)?;
    trace!(
        msg_type = format_args!("0x{msg_type:02x}"),
        len = payload.len(),
        "BLE indication received (AEAD step 4)"
    );

    if msg_type == MSG_ERROR {
        let (status, diagnostic) = parse_error_body(payload);
        return match status {
            0x02 => Err(PairingError::RegistrationWindowClosed),
            0x03 => Err(PairingError::GatewayAlreadyPaired),
            code => {
                let reason = if diagnostic.is_empty() {
                    format!("AEAD registration failed: gateway error code 0x{code:02x}")
                } else {
                    format!(
                        "AEAD registration failed: gateway error code 0x{code:02x}: {diagnostic}"
                    )
                };
                Err(PairingError::RegistrationFailed(reason))
            }
        };
    }
    if msg_type != PHONE_REGISTERED {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!("expected PHONE_REGISTERED (0x82), got 0x{msg_type:02x}"),
        });
    }

    // Step 5: Parse PHONE_REGISTERED: status(1) + rf_channel(1) + phone_key_hint(2 BE)
    if payload.len() != 4 {
        return Err(PairingError::InvalidResponse {
            msg_type: PHONE_REGISTERED,
            reason: format!(
                "expected 4 bytes in PHONE_REGISTERED (AEAD), got {}",
                payload.len()
            ),
        });
    }

    let status = payload[0];
    if status != 0x00 {
        return Err(PairingError::RegistrationFailed(format!(
            "PHONE_REGISTERED status: 0x{status:02x}"
        )));
    }

    let rf_channel = payload[1];
    validate_rf_channel(rf_channel)?;

    let phone_key_hint = u16::from_be_bytes([payload[2], payload[3]]);

    // Verify phone_key_hint matches the PSK we generated
    let expected_hint = crate::validation::compute_key_hint(&phone_psk);
    if phone_key_hint != expected_hint {
        return Err(PairingError::InvalidKeyHint);
    }

    let artifacts = PairingArtifactsAead {
        phone_psk,
        phone_key_hint,
        rf_channel,
        phone_label: phone_label.to_string(),
    };

    info!(
        phone_key_hint,
        rf_channel, "Phase 1 (AEAD) complete — registered with gateway"
    );

    Ok(artifacts)
}

/// Phase 1 result for the simplified AEAD pairing flow.
///
/// Gateway authority derives solely from possession of the phone PSK.
#[derive(Clone)]
pub struct PairingArtifactsAead {
    pub phone_psk: Zeroizing<[u8; 32]>,
    pub phone_key_hint: u16,
    pub rf_channel: u8,
    pub phone_label: String,
}

impl std::fmt::Debug for PairingArtifactsAead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingArtifactsAead")
            .field("phone_key_hint", &self.phone_key_hint)
            .field("rf_channel", &self.rf_channel)
            .field("phone_label", &self.phone_label)
            .field("phone_psk", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod aead_phase1_tests {
    use super::*;
    use crate::envelope::build_envelope;
    use crate::rng::MockRng;
    use crate::transport::MockBleTransport;
    use crate::validation::compute_key_hint;

    /// Build a mock PHONE_REGISTERED (AEAD) response.
    ///
    /// Wire format: `status(1) ‖ rf_channel(1) ‖ phone_key_hint(2 BE)`
    fn build_phone_registered_aead(status: u8, rf_channel: u8, phone_psk: &[u8; 32]) -> Vec<u8> {
        let phone_key_hint = compute_key_hint(phone_psk);
        let mut body = Vec::with_capacity(4);
        body.push(status);
        body.push(rf_channel);
        body.extend_from_slice(&phone_key_hint.to_be_bytes());
        build_envelope(PHONE_REGISTERED, &body).unwrap()
    }

    /// Happy path: AEAD Phase 1 completes and returns correct artifacts.
    #[test]
    fn aead_phase1_happy_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let rng = MockRng::new([0x55u8; 32]);
            let mut predicted_psk = [0u8; 32];
            rng.fill_bytes(&mut predicted_psk).unwrap();
            let rf_channel = 6u8;

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_phone_registered_aead(
                0x00,
                rf_channel,
                &predicted_psk,
            )));

            let device_addr = [0xAA; 6];

            let result =
                pair_with_gateway_aead(&mut transport, &rng, &device_addr, "test-phone", None)
                    .await;
            let artifacts = result.unwrap();

            assert_eq!(*artifacts.phone_psk, predicted_psk);
            assert_eq!(artifacts.rf_channel, rf_channel);
            assert_eq!(artifacts.phone_key_hint, compute_key_hint(&predicted_psk));
            assert_eq!(artifacts.phone_label, "test-phone");
        });
    }

    /// AEAD Phase 1: registration window closed.
    #[test]
    fn aead_phase1_registration_window_closed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let rng = MockRng::new([0x55u8; 32]);
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &[0x02]).unwrap()));

            let result = pair_with_gateway_aead(&mut transport, &rng, &[0xAA; 6], "", None).await;
            assert!(matches!(
                result,
                Err(PairingError::RegistrationWindowClosed)
            ));
        });
    }

    /// AEAD Phase 1: REGISTER_PHONE body contains PSK + label.
    #[test]
    fn aead_phase1_register_phone_body_format() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let rng = MockRng::new([0x55u8; 32]);
            let mut predicted_psk = [0u8; 32];
            rng.fill_bytes(&mut predicted_psk).unwrap();
            let label = "my-phone";

            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_phone_registered_aead(0x00, 6, &predicted_psk)));

            pair_with_gateway_aead(&mut transport, &rng, &[0xAA; 6], label, None)
                .await
                .unwrap();

            // Verify REGISTER_PHONE was the first (and only) GATT write
            assert_eq!(transport.written.len(), 1);
            let (_, _, data) = &transport.written[0];
            let (msg_type, payload) = parse_envelope(data).unwrap();
            assert_eq!(msg_type, REGISTER_PHONE);

            // Body: phone_psk(32) + label_len(1) + label(N)
            assert_eq!(payload.len(), 32 + 1 + label.len());
            assert_eq!(&payload[..32], &predicted_psk);
            assert_eq!(payload[32], label.len() as u8);
            assert_eq!(&payload[33..], label.as_bytes());
        });
    }

    /// AEAD Phase 1: `PairingArtifactsAead` Debug redacts PSK.
    #[test]
    fn aead_artifacts_debug_redacts_psk() {
        let artifacts = PairingArtifactsAead {
            phone_psk: Zeroizing::new([0x42u8; 32]),
            phone_key_hint: 0x1234,
            rf_channel: 6,
            phone_label: "test".into(),
        };
        let debug = format!("{artifacts:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("42"));
    }
}
