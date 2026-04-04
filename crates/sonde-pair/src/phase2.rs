// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::cbor::encode_pairing_request;
use crate::crypto;
use crate::envelope::{build_envelope, parse_envelope, parse_error_body, parse_node_ack};
use crate::error::PairingError;
use crate::rng::RngProvider;
use crate::transport::{enforce_lesc, BleTransport};
use crate::types::*;
use crate::validation::{compute_key_hint, validate_node_id};
use tracing::{debug, info, trace};
use zeroize::Zeroizing;

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
/// PEER_REQUEST frame using [`crypto::encrypt_pairing_request_aead`].
///
/// The node stores the frame verbatim and relays it to the gateway on its
/// next wake cycle.
pub async fn provision_node_aead(
    transport: &mut dyn BleTransport,
    artifacts: &crate::phase1::PairingArtifactsAead,
    rng: &dyn RngProvider,
    device_address: &[u8; 6],
    node_id: &str,
    sensors: &[crate::types::SensorDescriptor],
) -> Result<NodeProvisionResult, PairingError> {
    // Step 1: Validate node_id
    validate_node_id(node_id)?;

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
    let encrypted_frame = crypto::encrypt_pairing_request_aead(&artifacts.phone_psk, &cbor)?;

    // Step 6: Connect to node
    debug!(address = ?device_address, "connecting to node (AEAD provision)");
    let mtu = transport.connect(device_address).await?;
    if mtu < BLE_MTU_MIN {
        transport.disconnect().await.ok();
        return Err(PairingError::MtuTooLow {
            negotiated: mtu,
            required: BLE_MTU_MIN,
        });
    }
    debug!(address = ?device_address, mtu, "connected to node");

    enforce_lesc(transport).await?;

    // Step 7: Build NODE_PROVISION payload (AEAD format per spec §6.6):
    // node_key_hint(2) || node_psk(32) || rf_channel(1) || payload_len(2) || encrypted_payload
    let result = do_provision_node_aead(
        transport,
        node_key_hint,
        &node_psk,
        artifacts.rf_channel,
        &encrypted_frame,
    )
    .await;

    transport.disconnect().await.ok();
    result
}

/// Inner implementation for AEAD node provisioning.
async fn do_provision_node_aead(
    transport: &mut dyn BleTransport,
    node_key_hint: u16,
    node_psk: &[u8; 32],
    rf_channel: u8,
    encrypted_frame: &[u8],
) -> Result<NodeProvisionResult, PairingError> {
    if encrypted_frame.len() > PEER_PAYLOAD_MAX_LEN {
        return Err(PairingError::PayloadTooLarge {
            size: encrypted_frame.len(),
            max: PEER_PAYLOAD_MAX_LEN,
        });
    }
    let payload_len = encrypted_frame.len() as u16;

    let mut provision_payload =
        Zeroizing::new(Vec::with_capacity(2 + 32 + 1 + 2 + encrypted_frame.len()));
    provision_payload.extend_from_slice(&node_key_hint.to_be_bytes());
    provision_payload.extend_from_slice(node_psk);
    provision_payload.push(rf_channel);
    provision_payload.extend_from_slice(&payload_len.to_be_bytes());
    provision_payload.extend_from_slice(encrypted_frame);

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

    trace!("waiting for NODE_ACK indication (5 s timeout)");
    let response = transport
        .read_indication(NODE_SERVICE_UUID, NODE_COMMAND_UUID, 5000)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::MockRng;
    use crate::transport::MockBleTransport;

    #[tokio::test]
    async fn provision_node_aead_happy_path() {
        use crate::phase1::PairingArtifactsAead;

        let artifacts = PairingArtifactsAead {
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

        let result = provision_node_aead(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
        )
        .await;

        assert!(
            result.is_ok(),
            "provision_node_aead should succeed: {result:?}"
        );
        assert_eq!(result.unwrap().status, NodeAckStatus::Success);

        // Verify NODE_PROVISION was written
        assert_eq!(transport.written.len(), 1);
        let (_svc, _chr, data) = &transport.written[0];
        // First byte of envelope is NODE_PROVISION msg type
        assert_eq!(data[0], NODE_PROVISION);
    }

    #[tokio::test]
    async fn provision_node_aead_mtu_too_low() {
        use crate::phase1::PairingArtifactsAead;

        let artifacts = PairingArtifactsAead {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);
        let mut transport = MockBleTransport::new(100); // below BLE_MTU_MIN

        let result = provision_node_aead(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "test-node",
            &[],
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
    async fn provision_node_aead_invalid_node_id() {
        use crate::phase1::PairingArtifactsAead;

        let artifacts = PairingArtifactsAead {
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: "test".into(),
        };

        let rng = MockRng::new([0x42u8; 32]);
        let mut transport = MockBleTransport::new(247);

        let result = provision_node_aead(
            &mut transport,
            &artifacts,
            &rng,
            &[0xAA; 6],
            "", // empty node_id
            &[],
        )
        .await;

        assert!(
            matches!(result, Err(PairingError::InvalidNodeId(_))),
            "expected InvalidNodeId, got {result:?}"
        );
    }
}
