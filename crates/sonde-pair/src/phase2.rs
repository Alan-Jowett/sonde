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
use sonde_protocol::encode_board_layout_cbor;
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
    board_layout: Option<BoardLayout>,
) -> Result<NodeProvisionResult, PairingError> {
    // Step 1: Validate node_id
    validate_node_id(node_id)?;

    // Step 1a: Validate board layout (PT-1214, PT-1216).
    if let Some(ref layout) = board_layout {
        if let Err(reason) = layout.validate() {
            return Err(PairingError::InvalidBoardLayout(reason.into()));
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
    // Defer createBond() until after the GATT connect latch.  The node
    // calls ble_gap_security_initiate() in its on_connect callback;
    // calling createBond() before the latch causes a dual-initiation race
    // that confuses NimBLE's SMP state machine.  Deferring createBond()
    // to after the latch is the standard Android BLE flow and works
    // correctly with the node's Just Works pairing.
    transport.set_defer_bonding(true);
    debug!(address = ?device_address, "connecting to node (AEAD provision)");
    let mtu_result = transport.connect(device_address).await;
    // Reset defer-bonding hint immediately (one-shot) so any subsequent
    // connection on the same transport uses the default bonding flow.
    transport.set_defer_bonding(false);
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
        board_layout,
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
    board_layout: Option<BoardLayout>,
) -> Result<NodeProvisionResult, PairingError> {
    if encrypted_frame.len() > PEER_PAYLOAD_MAX_LEN {
        return Err(PairingError::PayloadTooLarge {
            size: encrypted_frame.len(),
            max: PEER_PAYLOAD_MAX_LEN,
        });
    }
    let payload_len = encrypted_frame.len() as u16;

    let board_layout_cbor = match board_layout {
        Some(layout) => Some(
            encode_board_layout_cbor(&layout)
                .map_err(|e| PairingError::InvalidBoardLayout(e.to_string()))?,
        ),
        None => None,
    };

    let mut provision_payload = Zeroizing::new(Vec::with_capacity(
        2 + 32 + 1 + 2 + encrypted_frame.len() + board_layout_cbor.as_ref().map_or(0, Vec::len),
    ));
    provision_payload.extend_from_slice(&node_key_hint.to_be_bytes());
    provision_payload.extend_from_slice(node_psk);
    provision_payload.push(rf_channel);
    provision_payload.extend_from_slice(&payload_len.to_be_bytes());
    provision_payload.extend_from_slice(encrypted_frame);

    // Append optional board layout CBOR (PT-1214, ND-0608).
    if let Some(board_layout_cbor) = board_layout_cbor {
        provision_payload.extend_from_slice(&board_layout_cbor);
        trace!(
            board_layout_len = board_layout_cbor.len(),
            "appended board layout CBOR to NODE_PROVISION"
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
    async fn provision_node_board_layout_appended() {
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

        let board_layout = BoardLayout {
            i2c0_sda: Some(4),
            i2c0_scl: Some(5),
            one_wire_data: Some(3),
            battery_adc: Some(2),
            sensor_enable: Some(6),
        };

        let result = provision_node(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
            Some(board_layout),
        )
        .await;

        assert!(
            result.is_ok(),
            "provision with board_layout should succeed: {result:?}"
        );

        assert_eq!(transport.written.len(), 1);
        let (_svc, _chr, data) = &transport.written[0];
        let body = &data[3..];
        let payload_len = u16::from_be_bytes([body[35], body[36]]) as usize;
        let board_layout_cbor_start = 37 + payload_len;
        assert!(
            body.len() > board_layout_cbor_start,
            "body should have trailing board-layout CBOR"
        );

        let trailing = &body[board_layout_cbor_start..];
        let value: ciborium::Value = ciborium::from_reader(trailing).expect("valid CBOR");
        let map = value.as_map().expect("CBOR map");
        let expected = [
            (1, Some(4)),
            (2, Some(5)),
            (3, Some(3)),
            (4, Some(2)),
            (5, Some(6)),
        ];
        for (key, expected_value) in expected {
            let value = map
                .iter()
                .find(|(k, _)| *k == ciborium::Value::Integer(key.into()))
                .unwrap_or_else(|| panic!("missing key {key}"))
                .1
                .clone();
            match expected_value {
                Some(expected_value) => {
                    let actual = value.as_integer().expect("integer value");
                    assert_eq!(i128::from(actual), expected_value);
                }
                None => assert_eq!(value, ciborium::Value::Null),
            }
        }
    }

    #[tokio::test]
    async fn provision_node_board_layout_none_no_trailing() {
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
            "provision without board_layout should succeed: {result:?}"
        );

        let (_svc, _chr, data) = &transport.written[0];
        let body = &data[3..];
        let payload_len = u16::from_be_bytes([body[35], body[36]]) as usize;
        assert_eq!(
            body.len(),
            37 + payload_len,
            "no trailing bytes when board_layout is None"
        );
    }

    #[tokio::test]
    async fn provision_node_board_layout_out_of_range() {
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
            Some(BoardLayout {
                i2c0_sda: Some(6),
                i2c0_scl: Some(7),
                one_wire_data: None,
                battery_adc: Some(22),
                sensor_enable: None,
            }),
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::InvalidBoardLayout(_))),
            "expected InvalidBoardLayout, got {result:?}"
        );
        assert!(
            transport.written.is_empty(),
            "no BLE writes on validation failure"
        );
    }

    #[tokio::test]
    async fn provision_node_board_layout_sda_equals_scl() {
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
            Some(BoardLayout {
                i2c0_sda: Some(4),
                i2c0_scl: Some(4),
                one_wire_data: None,
                battery_adc: None,
                sensor_enable: None,
            }),
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::InvalidBoardLayout(_))),
            "expected InvalidBoardLayout, got {result:?}"
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

    /// Validates: PT-1214 AC2 — board layout CBOR deterministic encoding.
    #[test]
    fn board_layout_cbor_deterministic() {
        let buf = encode_board_layout_cbor(&BoardLayout::SONDE_SENSOR_NODE_REV_A).unwrap();
        assert_eq!(
            buf,
            [0xA5, 0x01, 0x06, 0x02, 0x07, 0x03, 0x03, 0x04, 0x02, 0x05, 0x04]
        );
    }
}
