// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::cbor::encode_pairing_request;
use crate::crypto;
use crate::envelope::{build_envelope, parse_envelope, parse_error_body, parse_node_ack};
use crate::error::{format_device_address, PairingError};
use crate::rng::RngProvider;
use crate::store::PairingStore;
use crate::transport::BleTransport;
use crate::types::*;
use crate::validation::{compute_key_hint, validate_node_id};
use sonde_protocol::encode_board_layout_cbor;
use tracing::{debug, info, trace};
use zeroize::Zeroizing;

/// NODE_ACK indication timeout in milliseconds (PT-1002).
const NODE_ACK_TIMEOUT_MS: u64 = 5_000;

/// `RUN_TEST_ACK` indication timeout in milliseconds.
const RUN_TEST_ACK_TIMEOUT_MS: u64 = 5_000;

/// `TEST_RESULT` indication timeout in milliseconds.
const TEST_RESULT_TIMEOUT_MS: u64 = 5_000;

/// Map a BLE provisioning message type byte to its spec name (PT-0702).
fn msg_type_name(t: u8) -> &'static str {
    match t {
        NODE_PROVISION => "NODE_PROVISION",
        NODE_ACK => "NODE_ACK",
        MSG_ERROR => "ERROR",
        _ => "UNKNOWN",
    }
}

fn validate_supported_battery_adc(layout: &BoardLayout) -> Result<(), PairingError> {
    match layout.battery_adc {
        Some(0..=4) | None => Ok(()),
        Some(pin) => Err(PairingError::InvalidBoardLayout(format!(
            "battery_adc GPIO {pin} is not ADC-capable on ESP32-C3; use GPIO 0-4 or leave it unassigned"
        ))),
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
        validate_supported_battery_adc(layout)?;
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

async fn connect_to_node(
    transport: &mut dyn BleTransport,
    device_address: &[u8; 6],
) -> Result<(), PairingError> {
    transport.set_defer_bonding(true);
    let mtu_result = transport.connect(device_address).await;
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
    Ok(())
}

fn parse_ble_response<'a>(
    response: &'a [u8],
    expected_msg_type: u8,
    expected_name: &str,
) -> Result<&'a [u8], PairingError> {
    let (msg_type, body) = sonde_protocol::parse_ble_envelope(response).ok_or_else(|| {
        PairingError::InvalidResponse {
            msg_type: 0,
            reason: "malformed BLE envelope".into(),
        }
    })?;
    if msg_type != expected_msg_type {
        return Err(PairingError::InvalidResponse {
            msg_type,
            reason: format!(
                "expected {expected_name} (0x{expected_msg_type:02x}), got 0x{msg_type:02x}"
            ),
        });
    }
    Ok(body)
}

/// Run one generic pre-provisioning test using existing Phase 1 artifacts.
pub async fn run_pre_provisioning_test_with_artifacts(
    transport: &mut dyn BleTransport,
    _artifacts: &crate::phase1::PairingArtifacts,
    device_address: &[u8; 6],
    command: &PreProvisioningTestCommand,
) -> Result<sonde_protocol::TestResult, PairingError> {
    connect_to_node(transport, device_address).await?;

    let run_test_body = sonde_protocol::encode_run_test_command(
        command.test_type,
        command.rf_channel,
        &command.payload,
    )
    .map_err(|e| PairingError::DiagnosticFailed(format!("RUN_TEST_COMMAND encode failed: {e}")))?;
    let run_test_envelope =
        sonde_protocol::encode_ble_envelope(sonde_protocol::BLE_RUN_TEST_COMMAND, &run_test_body)
            .ok_or_else(|| {
            PairingError::DiagnosticFailed("RUN_TEST_COMMAND envelope too large".into())
        })?;

    transport
        .write_characteristic(NODE_SERVICE_UUID, NODE_COMMAND_UUID, &run_test_envelope)
        .await?;

    let ack_response = transport
        .read_indication(
            NODE_SERVICE_UUID,
            NODE_COMMAND_UUID,
            RUN_TEST_ACK_TIMEOUT_MS,
        )
        .await?;
    let ack_body = parse_ble_response(
        &ack_response,
        sonde_protocol::BLE_RUN_TEST_ACK,
        "RUN_TEST_ACK",
    )?;
    let ack_status = sonde_protocol::decode_run_test_ack(ack_body).map_err(|e| {
        PairingError::InvalidResponse {
            msg_type: sonde_protocol::BLE_RUN_TEST_ACK,
            reason: format!("decode RUN_TEST_ACK: {e}"),
        }
    })?;
    if ack_status != sonde_protocol::RUN_TEST_ACK_OK {
        transport.disconnect().await.ok();
        let reason = match ack_status {
            sonde_protocol::RUN_TEST_ACK_INVALID => "node rejected test command as invalid",
            sonde_protocol::RUN_TEST_ACK_UNSUPPORTED => {
                "node does not support the requested test type"
            }
            other => {
                return Err(PairingError::DiagnosticFailed(format!(
                    "unknown RUN_TEST_ACK status: 0x{other:02x}"
                )))
            }
        };
        return Err(PairingError::DiagnosticFailed(reason.into()));
    }

    transport.disconnect().await.ok();
    connect_to_node(transport, device_address).await?;

    let read_result_envelope = sonde_protocol::encode_ble_envelope(
        sonde_protocol::BLE_READ_TEST_RESULT,
        &sonde_protocol::encode_read_test_result(),
    )
    .ok_or_else(|| PairingError::DiagnosticFailed("READ_TEST_RESULT envelope too large".into()))?;
    transport
        .write_characteristic(NODE_SERVICE_UUID, NODE_COMMAND_UUID, &read_result_envelope)
        .await?;

    let result_response = transport
        .read_indication(NODE_SERVICE_UUID, NODE_COMMAND_UUID, TEST_RESULT_TIMEOUT_MS)
        .await?;
    let result_body = parse_ble_response(
        &result_response,
        sonde_protocol::BLE_TEST_RESULT,
        "TEST_RESULT",
    )?;
    let result = sonde_protocol::decode_test_result(result_body).map_err(|e| {
        PairingError::InvalidResponse {
            msg_type: sonde_protocol::BLE_TEST_RESULT,
            reason: format!("decode TEST_RESULT: {e}"),
        }
    })?;

    transport.disconnect().await.ok();
    Ok(result)
}

/// Run one generic pre-provisioning test, loading Phase 1 artifacts from storage.
pub async fn run_pre_provisioning_test(
    transport: &mut dyn BleTransport,
    store: &dyn PairingStore,
    device_address: &[u8; 6],
    command: &PreProvisioningTestCommand,
) -> Result<sonde_protocol::TestResult, PairingError> {
    let artifacts = store.load_artifacts()?.ok_or_else(|| {
        PairingError::DiagnosticFailed(
            "complete Phase 1 gateway pairing first to obtain a phone PSK".into(),
        )
    })?;
    run_pre_provisioning_test_with_artifacts(transport, &artifacts, device_address, command).await
}

/// Perform the rebooted pre-provisioning RSSI diagnostic.
pub async fn check_rssi(
    transport: &mut dyn BleTransport,
    store: &dyn PairingStore,
    device_address: &[u8; 6],
) -> Result<DiagnosticResult, PairingError> {
    let artifacts = store.load_artifacts()?.ok_or_else(|| {
        PairingError::DiagnosticFailed(
            "complete Phase 1 gateway pairing first to obtain a phone PSK".into(),
        )
    })?;
    let (diag_frame, request_nonce) =
        crate::crypto::build_diag_request_frame(&artifacts.phone_psk)?;
    let command = PreProvisioningTestCommand {
        test_type: PRE_PROVISIONING_TEST_TYPE_DIAG_FRAME,
        rf_channel: Some(artifacts.rf_channel),
        payload: diag_frame,
    };

    let result =
        run_pre_provisioning_test_with_artifacts(transport, &artifacts, device_address, &command)
            .await?;

    match result.status {
        sonde_protocol::TEST_RESULT_OK => {
            let reply_frame = result.reply_frame.as_deref().ok_or_else(|| {
                PairingError::DiagnosticFailed("missing raw DIAG_REPLY frame".into())
            })?;
            let node_reply_rssi_dbm = result.reply_rssi_dbm.ok_or_else(|| {
                PairingError::DiagnosticFailed("missing node-observed reply RSSI".into())
            })?;
            let (gateway_rssi_dbm, signal_quality) = crate::crypto::decrypt_diag_reply(
                reply_frame,
                &artifacts.phone_psk,
                request_nonce,
            )?;
            Ok(DiagnosticResult {
                gateway_rssi_dbm,
                signal_quality,
                node_reply_rssi_dbm,
                attempt_count: result.attempt_count,
                elapsed_ms: result.elapsed_ms,
            })
        }
        sonde_protocol::TEST_RESULT_TIMEOUT => Err(PairingError::DiagnosticFailed(
            "diagnostic timed out — verify gateway is running and modem is connected".into(),
        )),
        sonde_protocol::TEST_RESULT_NO_RESULT => Err(PairingError::DiagnosticFailed(
            "node reported no retained test result; retry the diagnostic run".into(),
        )),
        sonde_protocol::TEST_RESULT_EXECUTION_ERROR => Err(PairingError::DiagnosticFailed(
            "node failed to execute the staged diagnostic command".into(),
        )),
        other => Err(PairingError::DiagnosticFailed(format!(
            "unknown TEST_RESULT status: 0x{other:02x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::MockRng;
    use crate::store::MockPairingStore;
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

    #[tokio::test]
    async fn provision_node_board_layout_battery_adc_not_supported() {
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
                battery_adc: Some(7),
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

    fn mock_store() -> MockPairingStore {
        MockPairingStore::with_artifacts(mock_artifacts())
    }

    fn encode_ack_response(status: u8) -> Vec<u8> {
        sonde_protocol::encode_ble_envelope(
            sonde_protocol::BLE_RUN_TEST_ACK,
            &sonde_protocol::encode_run_test_ack(status).unwrap(),
        )
        .unwrap()
    }

    fn encode_test_result_response(result: &sonde_protocol::TestResult) -> Vec<u8> {
        sonde_protocol::encode_ble_envelope(
            sonde_protocol::BLE_TEST_RESULT,
            &sonde_protocol::encode_test_result(result).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn check_rssi_requires_phase1_artifacts() {
        let mut transport = MockBleTransport::new(247);
        let store = MockPairingStore::new();

        let result = check_rssi(&mut transport, &store, &[0xAA; 6]).await;
        let message = result.unwrap_err().to_string();
        assert!(message.contains("Phase 1"), "unexpected error: {message}");
        assert!(transport.written.is_empty(), "must fail before BLE writes");
    }

    #[tokio::test]
    async fn check_rssi_happy_path_reconnects_and_reads_result() {
        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(encode_ack_response(sonde_protocol::RUN_TEST_ACK_OK)));
        transport.queue_response(Ok(encode_test_result_response(
            &sonde_protocol::TestResult {
                status: sonde_protocol::TEST_RESULT_TIMEOUT,
                test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
                reply_frame: None,
                reply_rssi_dbm: None,
                attempt_count: 4,
                elapsed_ms: 8_600,
            },
        )));
        let store = mock_store();

        let result = check_rssi(&mut transport, &store, &[0xAA; 6]).await;
        assert!(matches!(result, Err(PairingError::DiagnosticFailed(_))));
        assert_eq!(transport.written.len(), 2);
        assert_eq!(
            transport.written[0].2[0],
            sonde_protocol::BLE_RUN_TEST_COMMAND
        );
        assert_eq!(
            transport.written[1].2[0],
            sonde_protocol::BLE_READ_TEST_RESULT
        );
        assert_eq!(transport.disconnect_count, 2);
    }

    #[tokio::test]
    async fn check_rssi_ack_failure_surfaces_error() {
        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(encode_ack_response(
            sonde_protocol::RUN_TEST_ACK_INVALID,
        )));
        let store = mock_store();

        let result = check_rssi(&mut transport, &store, &[0xAA; 6]).await;
        assert!(matches!(result, Err(PairingError::DiagnosticFailed(_))));
        assert_eq!(transport.written.len(), 1);
        assert_eq!(
            transport.written[0].2[0],
            sonde_protocol::BLE_RUN_TEST_COMMAND
        );
    }

    #[tokio::test]
    async fn check_rssi_success_combines_gateway_and_node_metadata() {
        struct SuccessDiagnosticTransport {
            inner: MockBleTransport,
            psk: [u8; 32],
        }

        impl crate::transport::BleTransport for SuccessDiagnosticTransport {
            fn start_scan(
                &mut self,
                service_uuids: &[u128],
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PairingError>> + '_>>
            {
                self.inner.start_scan(service_uuids)
            }

            fn stop_scan(
                &mut self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PairingError>> + '_>>
            {
                self.inner.stop_scan()
            }

            fn get_discovered_devices(
                &self,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<Vec<ScannedDevice>, PairingError>> + '_,
                >,
            > {
                self.inner.get_discovered_devices()
            }

            fn connect(
                &mut self,
                address: &[u8; 6],
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u16, PairingError>> + '_>>
            {
                self.inner.connect(address)
            }

            fn disconnect(
                &mut self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PairingError>> + '_>>
            {
                self.inner.disconnect()
            }

            fn write_characteristic(
                &mut self,
                service: u128,
                characteristic: u128,
                data: &[u8],
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PairingError>> + '_>>
            {
                self.inner
                    .write_characteristic(service, characteristic, data)
            }

            fn read_indication(
                &mut self,
                service: u128,
                characteristic: u128,
                timeout_ms: u64,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<u8>, PairingError>> + '_>,
            > {
                if self.inner.read_call_count == 1 {
                    let written = self.inner.written[0].2.clone();
                    let psk = self.psk;
                    let sha = crate::crypto::PairSha256;
                    let aead = crate::crypto::PairAead;
                    let (_, body) = sonde_protocol::parse_ble_envelope(&written).unwrap();
                    let decoded = sonde_protocol::decode_run_test_command(body).unwrap();
                    let request = sonde_protocol::decode_frame(&decoded.payload).unwrap();
                    let reply_payload = sonde_protocol::GatewayMessage::DiagReply {
                        diagnostic_type: sonde_protocol::DIAG_TYPE_RSSI,
                        rssi_dbm: -58,
                        signal_quality: sonde_protocol::SIGNAL_QUALITY_GOOD,
                    }
                    .encode()
                    .unwrap();
                    let header = sonde_protocol::FrameHeader {
                        key_hint: sonde_protocol::key_hint_from_psk(&psk, &sha),
                        msg_type: sonde_protocol::MSG_DIAG_REPLY,
                        nonce: request.header.nonce,
                    };
                    let raw_reply =
                        sonde_protocol::encode_frame(&header, &reply_payload, &psk, &aead, &sha)
                            .unwrap();
                    self.inner.queue_response(Ok(encode_test_result_response(
                        &sonde_protocol::TestResult {
                            status: sonde_protocol::TEST_RESULT_OK,
                            test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
                            reply_frame: Some(raw_reply),
                            reply_rssi_dbm: Some(-67),
                            attempt_count: 2,
                            elapsed_ms: 4_300,
                        },
                    )));
                }
                self.inner
                    .read_indication(service, characteristic, timeout_ms)
            }

            fn pairing_method(&self) -> Option<PairingMethod> {
                self.inner.pairing_method()
            }

            fn set_defer_bonding(&mut self, defer: bool) {
                self.inner.set_defer_bonding(defer);
            }
        }

        let artifacts = mock_artifacts();
        let store = MockPairingStore::with_artifacts(artifacts.clone());
        let mut transport = SuccessDiagnosticTransport {
            inner: MockBleTransport::new(247),
            psk: *artifacts.phone_psk,
        };
        transport
            .inner
            .queue_response(Ok(encode_ack_response(sonde_protocol::RUN_TEST_ACK_OK)));

        let result = check_rssi(&mut transport, &store, &[0xAA; 6])
            .await
            .unwrap();
        assert_eq!(transport.inner.written.len(), 2);
        assert_eq!(
            transport.inner.written[0].2[0],
            sonde_protocol::BLE_RUN_TEST_COMMAND
        );
        assert_eq!(
            transport.inner.written[1].2[0],
            sonde_protocol::BLE_READ_TEST_RESULT
        );
        assert_eq!(transport.inner.disconnect_count, 2);
        assert_eq!(result.gateway_rssi_dbm, -58);
        assert_eq!(result.signal_quality, sonde_protocol::SIGNAL_QUALITY_GOOD);
        assert_eq!(result.node_reply_rssi_dbm, -67);
        assert_eq!(result.attempt_count, 2);
        assert_eq!(result.elapsed_ms, 4_300);
    }

    #[tokio::test]
    async fn repeated_sampling_requires_separate_runs() {
        let store = mock_store();
        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(encode_ack_response(sonde_protocol::RUN_TEST_ACK_OK)));
        transport.queue_response(Ok(encode_test_result_response(
            &sonde_protocol::TestResult {
                status: sonde_protocol::TEST_RESULT_TIMEOUT,
                test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
                reply_frame: None,
                reply_rssi_dbm: None,
                attempt_count: 4,
                elapsed_ms: 8_600,
            },
        )));
        let _ = check_rssi(&mut transport, &store, &[0xAA; 6]).await;
        assert_eq!(
            transport
                .written
                .iter()
                .filter(|(_, _, data)| data[0] == sonde_protocol::BLE_RUN_TEST_COMMAND)
                .count(),
            1
        );

        transport.queue_response(Ok(encode_ack_response(sonde_protocol::RUN_TEST_ACK_OK)));
        transport.queue_response(Ok(encode_test_result_response(
            &sonde_protocol::TestResult {
                status: sonde_protocol::TEST_RESULT_TIMEOUT,
                test_type: Some(sonde_protocol::TEST_TYPE_DIAG_FRAME),
                reply_frame: None,
                reply_rssi_dbm: None,
                attempt_count: 4,
                elapsed_ms: 8_600,
            },
        )));
        let _ = check_rssi(&mut transport, &store, &[0xAA; 6]).await;
        assert_eq!(
            transport
                .written
                .iter()
                .filter(|(_, _, data)| data[0] == sonde_protocol::BLE_RUN_TEST_COMMAND)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn run_pre_provisioning_test_uses_explicit_test_discriminator() {
        let store = mock_store();
        let command = PreProvisioningTestCommand {
            test_type: PRE_PROVISIONING_TEST_TYPE_DIAG_FRAME,
            rf_channel: Some(6),
            payload: vec![0xAA; 32],
        };
        let mut transport = MockBleTransport::new(247);
        transport.queue_response(Ok(encode_ack_response(sonde_protocol::RUN_TEST_ACK_OK)));
        transport.queue_response(Ok(encode_test_result_response(
            &sonde_protocol::TestResult {
                status: sonde_protocol::TEST_RESULT_TIMEOUT,
                test_type: Some(PRE_PROVISIONING_TEST_TYPE_DIAG_FRAME),
                reply_frame: None,
                reply_rssi_dbm: None,
                attempt_count: 4,
                elapsed_ms: 8_600,
            },
        )));

        let _ = run_pre_provisioning_test(&mut transport, &store, &[0xAA; 6], &command).await;
        let (_, _, written) = &transport.written[0];
        let (_, body) = sonde_protocol::parse_ble_envelope(written).unwrap();
        let decoded = sonde_protocol::decode_run_test_command(body).unwrap();
        assert_eq!(decoded.test_type, PRE_PROVISIONING_TEST_TYPE_DIAG_FRAME);
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
