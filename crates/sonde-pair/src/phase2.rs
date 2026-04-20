// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::cbor::encode_pairing_request;
use crate::crypto;
use crate::envelope::{build_envelope, parse_envelope, parse_error_body, parse_node_ack};
use crate::error::{format_device_address, PairingError};
use crate::rng::RngProvider;
use crate::transport::BleTransport;
use crate::types::*;
use crate::validation::{compute_key_hint, validate_node_id};
use tracing::{debug, info, trace};
use zeroize::Zeroizing;

/// NODE_ACK indication timeout in milliseconds (PT-1002).
const NODE_ACK_TIMEOUT_MS: u64 = 5_000;

/// DIAG_RELAY_RESPONSE indication timeout in milliseconds (PT-1303).
const DIAG_RELAY_TIMEOUT_MS: u64 = 10_000;

/// Map a BLE provisioning message type byte to its spec name (PT-0702).
fn msg_type_name(t: u8) -> &'static str {
    match t {
        NODE_PROVISION => "NODE_PROVISION",
        NODE_ACK => "NODE_ACK",
        MSG_ERROR => "ERROR",
        _ => "UNKNOWN",
    }
}

/// Phase 2 (AEAD): Provision a node via BLE using simplified AEAD flow.
///
/// The phone generates the node PSK, builds a PairingRequest CBOR, encrypts
/// it with `phone_psk` via AES-256-GCM, and wraps it in a complete ESP-NOW
/// PEER_REQUEST frame using [`crypto::encrypt_pairing_request`].
///
/// The node stores the frame verbatim and relays it to the gateway on its
/// next wake cycle.
pub async fn provision_node(
    transport: &mut dyn BleTransport,
    artifacts: &crate::phase1::PairingArtifacts,
    rng: &dyn RngProvider,
    device_address: &[u8; 6],
    node_id: &str,
    sensors: &[crate::types::SensorDescriptor],
    pin_config: Option<PinConfig>,
) -> Result<NodeProvisionResult, PairingError> {
    // Step 1: Validate node_id
    validate_node_id(node_id)?;

    // Step 1a: Validate pin config (PT-1214 AC 5, AC 6)
    if let Some(ref pc) = pin_config {
        const MAX_GPIO: u8 = 21;
        if pc.i2c0_sda > MAX_GPIO || pc.i2c0_scl > MAX_GPIO {
            return Err(PairingError::InvalidPinConfig(format!(
                "GPIO pin number out of range (0–{}), got sda={}, scl={}",
                MAX_GPIO, pc.i2c0_sda, pc.i2c0_scl
            )));
        }
        if pc.i2c0_sda == pc.i2c0_scl {
            return Err(PairingError::InvalidPinConfig(format!(
                "i2c0_sda and i2c0_scl must be different GPIO pins, both are {}",
                pc.i2c0_sda
            )));
        }
    }

    // Step 2: Generate node PSK
    let mut node_psk = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut *node_psk)?;
    trace!("generated 32-byte node PSK");

    // Step 3: Compute node_key_hint
    let node_key_hint = compute_key_hint(&node_psk);

    // Step 4: Build PairingRequest CBOR
    let timestamp = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| PairingError::TimestampUnavailable)?
            .as_secs(),
    )
    .map_err(|_| PairingError::TimestampUnavailable)?;
    let cbor =
        encode_pairing_request(node_id, &node_psk, artifacts.rf_channel, sensors, timestamp)?;

    // Step 5: Encrypt with phone_psk and wrap in ESP-NOW AEAD PEER_REQUEST frame.
    let encrypted_frame = crypto::encrypt_pairing_request(&artifacts.phone_psk, &cbor)?;

    // Step 6: Connect to node
    // Skip client-initiated bonding — the node calls
    // ble_gap_security_initiate() in its on_connect callback to drive
    // LESC Just Works pairing.  Having both sides initiate simultaneously
    // confuses NimBLE's SMP state machine ("No open connection").
    transport.set_skip_bonding(true);
    debug!(address = ?device_address, "connecting to node (AEAD provision)");
    let mtu_result = transport.connect(device_address).await;
    // Reset skip-bonding hint immediately (one-shot) so any subsequent
    // connection on the same transport uses the default bonding flow.
    transport.set_skip_bonding(false);
    let mtu = mtu_result?;
    if mtu < BLE_MTU_MIN {
        transport.disconnect().await.ok();
        return Err(PairingError::MtuTooLow {
            device: format_device_address(device_address),
            negotiated: mtu,
            required: BLE_MTU_MIN,
        });
    }
    debug!(address = ?device_address, mtu, "connected to node");

    // Note: enforce_lesc() is intentionally NOT called for node connections.
    // The node uses LESC Just Works (ND-0904) because it has no display or
    // input for Numeric Comparison.  PT-0904 (LESC Numeric Comparison
    // enforcement) applies only to the modem connection in Phase 1.
    // LESC Just Works still provides link-layer encryption but does not
    // protect against active MITM — this residual risk is accepted for
    // headless nodes per the protocol spec (ble-pairing-protocol.md §8.2).

    // Step 7: Build NODE_PROVISION payload (AEAD format per spec §6.6):
    // node_key_hint(2) || node_psk(32) || rf_channel(1) || payload_len(2) || encrypted_payload
    let result = do_provision_node(
        transport,
        node_key_hint,
        &node_psk,
        artifacts.rf_channel,
        &encrypted_frame,
        pin_config,
    )
    .await;

    transport.disconnect().await.ok();
    result
}

/// Inner implementation for AEAD node provisioning.
async fn do_provision_node(
    transport: &mut dyn BleTransport,
    node_key_hint: u16,
    node_psk: &[u8; 32],
    rf_channel: u8,
    encrypted_frame: &[u8],
    pin_config: Option<PinConfig>,
) -> Result<NodeProvisionResult, PairingError> {
    if encrypted_frame.len() > PEER_PAYLOAD_MAX_LEN {
        return Err(PairingError::PayloadTooLarge {
            size: encrypted_frame.len(),
            max: PEER_PAYLOAD_MAX_LEN,
        });
    }
    let payload_len = encrypted_frame.len() as u16;

    // Pin config CBOR {1: u8, 2: u8} is at most 7 bytes (map(2) + 2×(uint,uint)).
    let pin_cbor_capacity = if pin_config.is_some() { 7 } else { 0 };

    let mut provision_payload = Zeroizing::new(Vec::with_capacity(
        2 + 32 + 1 + 2 + encrypted_frame.len() + pin_cbor_capacity,
    ));
    provision_payload.extend_from_slice(&node_key_hint.to_be_bytes());
    provision_payload.extend_from_slice(node_psk);
    provision_payload.push(rf_channel);
    provision_payload.extend_from_slice(&payload_len.to_be_bytes());
    provision_payload.extend_from_slice(encrypted_frame);

    // Append optional pin config CBOR (PT-1214, ND-0608)
    if let Some(pc) = pin_config {
        let pin_cbor = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(1.into()),
                ciborium::Value::Integer(pc.i2c0_sda.into()),
            ),
            (
                ciborium::Value::Integer(2.into()),
                ciborium::Value::Integer(pc.i2c0_scl.into()),
            ),
        ]);
        ciborium::into_writer(&pin_cbor, &mut *provision_payload)
            .map_err(|e| PairingError::CborEncodeFailed(format!("pin_config: {e}")))?;
        trace!(
            sda = pc.i2c0_sda,
            scl = pc.i2c0_scl,
            "appended pin config CBOR to NODE_PROVISION"
        );
    }

    let message = Zeroizing::new(build_envelope(NODE_PROVISION, &provision_payload).ok_or(
        PairingError::PayloadTooLarge {
            size: provision_payload.len(),
            max: u16::MAX as usize,
        },
    )?);

    trace!(
        msg = "NODE_PROVISION",
        len = message.len(),
        "BLE write (AEAD)"
    );
    transport
        .write_characteristic(NODE_SERVICE_UUID, NODE_COMMAND_UUID, &message)
        .await?;

    trace!(
        timeout_ms = NODE_ACK_TIMEOUT_MS,
        "waiting for NODE_ACK indication"
    );
    let response = transport
        .read_indication(NODE_SERVICE_UUID, NODE_COMMAND_UUID, NODE_ACK_TIMEOUT_MS)
        .await?;
    let (msg_type, payload) = parse_envelope(&response)?;
    trace!(
        msg_type = format_args!("0x{msg_type:02x}"),
        msg_name = msg_type_name(msg_type),
        len = payload.len(),
        "BLE indication received (AEAD provision)"
    );

    if msg_type == MSG_ERROR {
        let (status, message) = parse_error_body(payload);
        const MAX_DIAGNOSTIC_LEN: usize = 256;
        let diagnostic: String = message
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
            .take(MAX_DIAGNOSTIC_LEN)
            .collect();
        debug!(
            status = format_args!("0x{status:02x}"),
            diagnostic = %diagnostic,
            "node returned error response (AEAD provision)"
        );
        return Err(PairingError::NodeErrorResponse {
            status,
            message: diagnostic,
        });
    }
    if msg_type != NODE_ACK {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!(
                "expected NODE_ACK (0x{:02x}), got 0x{msg_type:02x}",
                NODE_ACK
            ),
        });
    }

    let status_byte = parse_node_ack(payload)?;
    let status = NodeAckStatus::from_byte(status_byte);

    match status {
        NodeAckStatus::Success => {
            info!("Phase 2 (AEAD) complete - node provisioned");
        }
        _ => {
            debug!(status = ?status, "node provision failed (AEAD)");
            return Err(PairingError::NodeProvisionFailed(status));
        }
    }

    Ok(NodeProvisionResult { status })
}

/// Result of an RSSI diagnostic check.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticResult {
    /// Measured RSSI in dBm (typically −30 to −90).
    pub rssi_dbm: i8,
    /// Signal quality: 0=good, 1=marginal, 2=bad.
    pub signal_quality: u8,
}

/// Perform an RSSI diagnostic using the node as a radio relay (PT-1300).
///
/// The node must already be connected via BLE. This function can be called
/// multiple times (diagnostic is repeatable, PT-1306) and does not modify
/// any pairing state (PT-1308).
pub async fn check_rssi(
    transport: &mut dyn BleTransport,
    artifacts: &crate::phase1::PairingArtifacts,
) -> Result<DiagnosticResult, PairingError> {
    // 1. Build DIAG_REQUEST frame (PT-1301).
    let (diag_frame, request_nonce) =
        crate::crypto::build_diag_request_frame(&artifacts.phone_psk)?;

    // 2. Wrap in DIAG_RELAY_REQUEST BLE envelope (PT-1302).
    let relay_body = sonde_protocol::encode_diag_relay_request(artifacts.rf_channel, &diag_frame)
        .map_err(|e| PairingError::DiagnosticFailed(format!("relay encode: {}", e)))?;
    let envelope =
        sonde_protocol::encode_ble_envelope(sonde_protocol::BLE_DIAG_RELAY_REQUEST, &relay_body)
            .ok_or_else(|| PairingError::DiagnosticFailed("BLE envelope too large".into()))?;

    // 3. Write to node BLE characteristic.
    transport
        .write_characteristic(NODE_SERVICE_UUID, NODE_COMMAND_UUID, &envelope)
        .await?;

    // 4. Wait for DIAG_RELAY_RESPONSE (timeout per PT-1303).
    let response = transport
        .read_indication(NODE_SERVICE_UUID, NODE_COMMAND_UUID, DIAG_RELAY_TIMEOUT_MS)
        .await?;

    // 5. Parse BLE envelope.
    let (msg_type, body) = sonde_protocol::parse_ble_envelope(&response).ok_or_else(|| {
        PairingError::InvalidResponse {
            msg_type: 0,
            reason: "malformed BLE envelope".into(),
        }
    })?;
    if msg_type != sonde_protocol::BLE_DIAG_RELAY_RESPONSE {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!(
                "expected DIAG_RELAY_RESPONSE (0x82), got 0x{:02x}",
                msg_type
            ),
        });
    }

    // 6. Parse relay response status.
    let (status, payload) = sonde_protocol::decode_diag_relay_response(body).map_err(|e| {
        PairingError::InvalidResponse {
            msg_type,
            reason: format!("decode relay response: {}", e),
        }
    })?;

    match status {
        sonde_protocol::DIAG_RELAY_STATUS_OK => {
            // 7. Decrypt DIAG_REPLY (PT-1303 AC-2).
            let (rssi_dbm, signal_quality) =
                crate::crypto::decrypt_diag_reply(payload, &artifacts.phone_psk, request_nonce)?;
            Ok(DiagnosticResult {
                rssi_dbm,
                signal_quality,
            })
        }
        sonde_protocol::DIAG_RELAY_STATUS_TIMEOUT => Err(PairingError::DiagnosticFailed(
            "no response from gateway — verify gateway is running and modem is connected".into(),
        )),
        sonde_protocol::DIAG_RELAY_STATUS_CHANNEL_ERROR => Err(PairingError::DiagnosticFailed(
            "channel error — verify RF channel configuration".into(),
        )),
        other => Err(PairingError::DiagnosticFailed(format!(
            "unknown relay status: 0x{:02x}",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::MockRng;
    use crate::transport::MockBleTransport;

    #[tokio::test]
    async fn provision_node_happy_path() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);

        // NODE_ACK(0x00 = success) wrapped in envelope
        let ack_body = [0x00u8];
        let mut ack_envelope = Vec::new();
        ack_envelope.push(NODE_ACK);
        ack_envelope.extend_from_slice(&(ack_body.len() as u16).to_be_bytes());
        ack_envelope.extend_from_slice(&ack_body);

        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(ack_envelope));

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            None,
        )
        .await;

        assert!(result.is_ok(), "provision_node should succeed: {result:?}");
        assert_eq!(result.unwrap().status, NodeAckStatus::Success);

        // Verify NODE_PROVISION was written
        assert_eq!(transport.written.len(), 1);
        let (_svc, _chr, data) = &transport.written[0];
        // First byte of envelope is NODE_PROVISION msg type
        assert_eq!(data[0], NODE_PROVISION);

        // T-PT-311: Verify NODE_PROVISION wire format:
        //   envelope header (TYPE[1] + LEN[2]) + body
        //   body = node_key_hint[2] ‖ node_psk[32] ‖ rf_channel[1] ‖ payload_len[2] ‖ encrypted_payload
        let body = &data[3..]; // skip envelope header
        assert!(
            body.len() >= 37,
            "body must be at least 37 bytes (2+32+1+2 prefix), got {}",
            body.len()
        );

        // bytes 0..2: node_key_hint (BE u16) — derived from MockRng seed [0x42; 32]
        let written_key_hint = u16::from_be_bytes([body[0], body[1]]);
        let expected_key_hint = compute_key_hint(&[0x42u8; 32]);
        assert_eq!(
            written_key_hint, expected_key_hint,
            "node_key_hint mismatch"
        );

        // bytes 2..34: node_psk (32 bytes from MockRng)
        assert_eq!(&body[2..34], &[0x42u8; 32], "node_psk mismatch");

        // byte 34: rf_channel
        assert_eq!(body[34], 6, "rf_channel mismatch");

        // bytes 35..37: payload_len (BE u16)
        let payload_len = u16::from_be_bytes([body[35], body[36]]) as usize;
        assert!(payload_len > 0, "encrypted payload must be non-empty");

        // body length = 37 + payload_len (no pin config)
        assert_eq!(
            body.len(),
            37 + payload_len,
            "body length must be exactly 37 + payload_len when no pin config"
        );
    }

    #[tokio::test]
    async fn provision_node_mtu_too_low() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);
        let mut transport = MockBleTransport::new(100); // below BLE_MTU_MIN

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            None,
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::MtuTooLow { .. })),
            "expected MtuTooLow, got {result:?}"
        );
        assert!(
            transport.written.is_empty(),
            "no writes should occur when MTU is too low"
        );
    }

    #[tokio::test]
    async fn provision_node_invalid_node_id() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);
        let mut transport = MockBleTransport::new(247);

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "", // empty node_id
            &[],
            None,
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::InvalidNodeId(_))),
            "expected InvalidNodeId, got {result:?}"
        );
    }

    #[tokio::test]
    async fn provision_node_pin_config_appended() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);

        let ack_body = [0x00u8];
        let mut ack_envelope = Vec::new();
        ack_envelope.push(NODE_ACK);
        ack_envelope.extend_from_slice(&(ack_body.len() as u16).to_be_bytes());
        ack_envelope.extend_from_slice(&ack_body);

        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(ack_envelope));

        let pc = PinConfig {
            i2c0_sda: 4,
            i2c0_scl: 5,
        };

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            Some(pc),
        )
        .await;

        assert!(
            result.is_ok(),
            "provision with pin_config should succeed: {result:?}"
        );

        assert_eq!(transport.written.len(), 1);
        let (_svc, _chr, data) = &transport.written[0];
        let body = &data[3..];
        let payload_len = u16::from_be_bytes([body[35], body[36]]) as usize;
        let pin_cbor_start = 37 + payload_len;
        assert!(
            body.len() > pin_cbor_start,
            "body should have trailing CBOR"
        );

        let trailing = &body[pin_cbor_start..];
        let value: ciborium::Value = ciborium::from_reader(trailing).expect("valid CBOR");
        let map = value.as_map().expect("CBOR map");
        let sda = map
            .iter()
            .find(|(k, _)| *k == ciborium::Value::Integer(1.into()))
            .expect("key 1")
            .1
            .as_integer()
            .unwrap();
        let scl = map
            .iter()
            .find(|(k, _)| *k == ciborium::Value::Integer(2.into()))
            .expect("key 2")
            .1
            .as_integer()
            .unwrap();
        assert_eq!(i128::from(sda), 4);
        assert_eq!(i128::from(scl), 5);
    }

    #[tokio::test]
    async fn provision_node_pin_config_none_no_trailing() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);

        let ack_body = [0x00u8];
        let mut ack_envelope = Vec::new();
        ack_envelope.push(NODE_ACK);
        ack_envelope.extend_from_slice(&(ack_body.len() as u16).to_be_bytes());
        ack_envelope.extend_from_slice(&ack_body);

        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(ack_envelope));

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            None,
        )
        .await;

        assert!(
            result.is_ok(),
            "provision without pin_config should succeed: {result:?}"
        );

        let (_svc, _chr, data) = &transport.written[0];
        let body = &data[3..];
        let payload_len = u16::from_be_bytes([body[35], body[36]]) as usize;
        assert_eq!(
            body.len(),
            37 + payload_len,
            "no trailing bytes when pin_config is None"
        );
    }

    #[tokio::test]
    async fn provision_node_pin_config_out_of_range() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);
        let mut transport = MockBleTransport::new(247);

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            Some(PinConfig {
                i2c0_sda: 22,
                i2c0_scl: 5,
            }),
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::InvalidPinConfig(_))),
            "expected InvalidPinConfig, got {result:?}"
        );
        assert!(
            transport.written.is_empty(),
            "no BLE writes on validation failure"
        );
    }

    #[tokio::test]
    async fn provision_node_pin_config_sda_equals_scl() {
        use crate::phase1::PairingArtifacts;

        let artifacts = PairingArtifacts {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);
        let mut transport = MockBleTransport::new(247);

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            Some(PinConfig {
                i2c0_sda: 4,
                i2c0_scl: 4,
            }),
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::InvalidPinConfig(_))),
            "expected InvalidPinConfig, got {result:?}"
        );
        assert!(
            transport.written.is_empty(),
            "no BLE writes on validation failure"
        );
    }

    // ── RSSI diagnostic tests ─────────────────────────────────────

    fn mock_artifacts() -> crate::phase1::PairingArtifacts {
        use crate::crypto::PairSha256;
        let psk = [0x55u8; 32];
        crate::phase1::PairingArtifacts {
            phone_psk: zeroize::Zeroizing::new(psk),
            phone_key_hint: sonde_protocol::key_hint_from_psk(&psk, &PairSha256),
            rf_channel: 6,
            phone_label: "test".into(),
        }
    }

    #[tokio::test]
    async fn check_rssi_timeout_status() {
        let artifacts = mock_artifacts();

        let relay_body = sonde_protocol::encode_diag_relay_response(
            sonde_protocol::DIAG_RELAY_STATUS_TIMEOUT,
            &[],
        )
        .unwrap();
        let ble_response = sonde_protocol::encode_ble_envelope(
            sonde_protocol::BLE_DIAG_RELAY_RESPONSE,
            &relay_body,
        )
        .unwrap();

        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(ble_response));

        let result = check_rssi(&mut transport, &artifacts).await;
        assert!(matches!(result, Err(PairingError::DiagnosticFailed(_))));
        assert_eq!(transport.written.len(), 1);
    }

    #[tokio::test]
    async fn check_rssi_channel_error_status() {
        let artifacts = mock_artifacts();

        let relay_body = sonde_protocol::encode_diag_relay_response(
            sonde_protocol::DIAG_RELAY_STATUS_CHANNEL_ERROR,
            &[],
        )
        .unwrap();
        let ble_response = sonde_protocol::encode_ble_envelope(
            sonde_protocol::BLE_DIAG_RELAY_RESPONSE,
            &relay_body,
        )
        .unwrap();

        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(ble_response));

        let result = check_rssi(&mut transport, &artifacts).await;
        assert!(matches!(result, Err(PairingError::DiagnosticFailed(_))));
    }

    /// Validates: PT-1214 AC2 — pin config CBOR deterministic encoding.
    ///
    /// The pin config CBOR map must use integer keys in ascending order
    /// (key 1 = i2c0_sda, key 2 = i2c0_scl) with minimal-length encoding.
    #[test]
    fn pin_config_cbor_deterministic() {
        let pc = PinConfig {
            i2c0_sda: 5,
            i2c0_scl: 6,
        };
        let pin_cbor = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(1.into()),
                ciborium::Value::Integer(pc.i2c0_sda.into()),
            ),
            (
                ciborium::Value::Integer(2.into()),
                ciborium::Value::Integer(pc.i2c0_scl.into()),
            ),
        ]);
        let mut buf = Vec::new();
        ciborium::into_writer(&pin_cbor, &mut buf).unwrap();

        // Expected: A2 01 05 02 06
        // A2 = map(2), 01 = key 1, 05 = value 5, 02 = key 2, 06 = value 6
        assert_eq!(buf, [0xA2, 0x01, 0x05, 0x02, 0x06]);

        // Verify keys are in ascending order (deterministic CBOR §4.2).
        let decoded: ciborium::Value = ciborium::from_reader(buf.as_slice()).unwrap();
        if let ciborium::Value::Map(pairs) = decoded {
            let keys: Vec<u64> = pairs
                .iter()
                .map(|(k, _)| u64::try_from(k.as_integer().unwrap()).unwrap())
                .collect();
            assert_eq!(keys, vec![1, 2], "keys must be in ascending order");
        } else {
            panic!("expected CBOR map");
        }
    }
}
