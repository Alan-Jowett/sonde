// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::cbor::encode_pairing_request;
use crate::crypto;
use crate::envelope::{build_envelope, parse_envelope, parse_error_body, parse_node_ack};
use crate::error::PairingError;
use crate::rng::RngProvider;
use crate::store::PairingStore;
use crate::transport::BleTransport;
use crate::types::*;
use crate::validation::{compute_key_hint, validate_node_id};
use tracing::{debug, info};
use zeroize::Zeroizing;

/// Phase 2: Provision a node via BLE.
///
/// Requires a prior Phase 1 pairing (artifacts in store). Generates a node PSK,
/// builds an authenticated+encrypted provision message, and sends it to the node.
pub async fn provision_node(
    transport: &mut dyn BleTransport,
    store: &dyn PairingStore,
    rng: &dyn RngProvider,
    device_address: &[u8; 6],
    node_id: &str,
    sensors: &[SensorDescriptor],
) -> Result<NodeProvisionResult, PairingError> {
    // Step 1: Load PairingArtifacts from store
    let artifacts = store.load_artifacts()?.ok_or(PairingError::NotPaired)?;
    debug!("loaded pairing artifacts");

    // Step 2: Validate node_id
    validate_node_id(node_id)?;

    // Step 3: Generate node_psk
    let mut node_psk = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut *node_psk)?;

    // Step 4: Compute node_key_hint
    let node_key_hint = compute_key_hint(&node_psk);

    // Step 5: Build PairingRequest CBOR
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cbor =
        encode_pairing_request(node_id, &node_psk, artifacts.rf_channel, sensors, timestamp)?;

    // Step 6: HMAC-SHA256 the CBOR with phone_psk
    let hmac = crypto::hmac_sha256(&*artifacts.phone_psk, &cbor);
    let mut authenticated_request = Zeroizing::new(Vec::with_capacity(2 + cbor.len() + 32));
    authenticated_request.extend_from_slice(&artifacts.phone_key_hint.to_be_bytes());
    authenticated_request.extend_from_slice(&cbor);
    authenticated_request.extend_from_slice(&hmac);

    // Step 7: Convert gw Ed25519 public → X25519
    let gw_x25519 = crypto::ed25519_to_x25519_public(&artifacts.gateway_identity.public_key)?;

    // Step 8: Generate ephemeral X25519 keypair
    let (eph_secret, eph_public) = crypto::generate_x25519_keypair(rng)?;

    // Step 9: ECDH + HKDF → AES key
    let shared_secret = crypto::x25519_ecdh(&eph_secret, &gw_x25519);
    let aes_key = crypto::hkdf_sha256(
        &shared_secret,
        &artifacts.gateway_identity.gateway_id,
        b"sonde-node-pair-v1",
    );

    // Step 10: Generate nonce and encrypt
    let mut nonce = [0u8; 12];
    rng.fill_bytes(&mut nonce)?;
    let ciphertext = crypto::aes256gcm_encrypt(
        &aes_key,
        &nonce,
        &authenticated_request,
        &artifacts.gateway_identity.gateway_id,
    )?;

    // Step 11: Connect to node, check MTU
    info!("connecting to node");
    let mtu = transport.connect(device_address).await?;
    if mtu < BLE_MTU_MIN {
        transport.disconnect().await.ok();
        return Err(PairingError::MtuTooLow {
            negotiated: mtu,
            required: BLE_MTU_MIN,
        });
    }

    let result = do_provision_node(
        transport,
        node_key_hint,
        &node_psk,
        artifacts.rf_channel,
        &eph_public,
        &nonce,
        &ciphertext,
    )
    .await;

    // Step 15: Disconnect and zero ephemeral keys
    transport.disconnect().await.ok();
    // eph_secret, node_psk dropped via Zeroizing

    result
}

async fn do_provision_node(
    transport: &mut dyn BleTransport,
    node_key_hint: u16,
    node_psk: &[u8; 32],
    rf_channel: u8,
    eph_public: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<NodeProvisionResult, PairingError> {
    // Step 12: Build NODE_PROVISION payload
    let total_encrypted_len = eph_public.len() + nonce.len() + ciphertext.len();
    if total_encrypted_len > PEER_PAYLOAD_MAX_LEN {
        return Err(PairingError::PayloadTooLarge(total_encrypted_len));
    }
    if total_encrypted_len > u16::MAX as usize {
        return Err(PairingError::PayloadTooLarge(total_encrypted_len));
    }
    let payload_len = total_encrypted_len as u16;
    let mut provision_payload = Vec::with_capacity(2 + 32 + 1 + 2 + 32 + 12 + ciphertext.len());
    provision_payload.extend_from_slice(&node_key_hint.to_be_bytes());
    provision_payload.extend_from_slice(node_psk);
    provision_payload.push(rf_channel);
    provision_payload.extend_from_slice(&payload_len.to_be_bytes());
    provision_payload.extend_from_slice(eph_public);
    provision_payload.extend_from_slice(nonce);
    provision_payload.extend_from_slice(ciphertext);

    let message = build_envelope(NODE_PROVISION, &provision_payload)
        .ok_or(PairingError::PayloadTooLarge(provision_payload.len()))?;

    // Step 13: Write to NODE_COMMAND_UUID
    transport
        .write_characteristic(NODE_SERVICE_UUID, NODE_COMMAND_UUID, &message)
        .await?;

    // Step 14: Read indication (timeout 5s)
    let response = transport
        .read_indication(NODE_SERVICE_UUID, NODE_COMMAND_UUID, 5000)
        .await?;
    let (msg_type, payload) = parse_envelope(&response)?;

    if msg_type == MSG_ERROR {
        let (status, message) = parse_error_body(payload);
        return Err(PairingError::NodeErrorResponse { status, message });
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
            info!("Phase 2 complete — node provisioned");
        }
        _ => {
            return Err(PairingError::NodeProvisionFailed(status));
        }
    }

    // Step 16: Return result
    Ok(NodeProvisionResult { status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::MockRng;
    use crate::store::MemoryPairingStore;
    use crate::transport::MockBleTransport;
    use ed25519_dalek::SigningKey;

    fn test_artifacts() -> PairingArtifacts {
        let signing_key = SigningKey::from_bytes(&[0x42u8; 32]);
        PairingArtifacts {
            gateway_identity: GatewayIdentity {
                public_key: signing_key.verifying_key().to_bytes(),
                gateway_id: [0x01u8; 16],
            },
            phone_psk: Zeroizing::new([0x55u8; 32]),
            phone_key_hint: compute_key_hint(&[0x55u8; 32]),
            rf_channel: 6,
            phone_label: String::new(),
        }
    }

    fn store_with_artifacts() -> MemoryPairingStore {
        let mut store = MemoryPairingStore::new();
        store.save_artifacts(&test_artifacts()).unwrap();
        store
    }

    fn test_sensors() -> Vec<SensorDescriptor> {
        vec![
            SensorDescriptor {
                sensor_type: 1,
                sensor_id: 0x48,
                label: Some("temp".into()),
            },
            SensorDescriptor {
                sensor_type: 2,
                sensor_id: 3,
                label: Some("humidity".into()),
            },
        ]
    }

    #[test]
    fn t_pt_300_happy_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x00]).unwrap())); // Success

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &sensors,
            )
            .await;

            let provision_result = result.unwrap();
            assert_eq!(provision_result.status, NodeAckStatus::Success);

            // Verify a write happened to NODE_COMMAND_UUID
            assert_eq!(transport.written.len(), 1);
            let (svc, chr, _data) = &transport.written[0];
            assert_eq!(*svc, NODE_SERVICE_UUID);
            assert_eq!(*chr, NODE_COMMAND_UUID);
        });
    }

    #[test]
    fn t_pt_301_not_paired() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            let store = MemoryPairingStore::new(); // empty — no artifacts
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &sensors[..1],
            )
            .await;

            assert!(matches!(result, Err(PairingError::NotPaired)));
        });
    }

    #[test]
    fn t_pt_302_invalid_node_id() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "", // empty node_id
                &sensors[..1],
            )
            .await;

            assert!(matches!(result, Err(PairingError::InvalidNodeId(_))));
        });
    }

    #[test]
    fn t_pt_303_already_paired() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x01]).unwrap())); // AlreadyPaired

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &sensors[..1],
            )
            .await;

            match result {
                Err(PairingError::NodeProvisionFailed(status)) => {
                    assert_eq!(status, NodeAckStatus::AlreadyPaired);
                }
                other => panic!("expected NodeProvisionFailed(AlreadyPaired), got {other:?}"),
            }
        });
    }

    #[test]
    fn t_pt_304_storage_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x02]).unwrap())); // StorageError

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &sensors[..1],
            )
            .await;

            match result {
                Err(PairingError::NodeProvisionFailed(status)) => {
                    assert_eq!(status, NodeAckStatus::StorageError);
                }
                other => panic!("expected NodeProvisionFailed(StorageError), got {other:?}"),
            }
        });
    }

    #[test]
    fn t_pt_305_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // No response queued → IndicationTimeout

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &sensors[..1],
            )
            .await;

            assert!(matches!(result, Err(PairingError::IndicationTimeout)));
        });
    }

    #[test]
    fn t_pt_306_node_error_response() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // Node responds with ERROR (0xFF) containing status 0x01 and diagnostic
            let mut error_body = vec![0x01];
            error_body.extend_from_slice(b"malformed");
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &error_body).unwrap()));

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &sensors[..1],
            )
            .await;

            match result {
                Err(PairingError::NodeErrorResponse { status, message }) => {
                    assert_eq!(status, 0x01);
                    assert_eq!(message, "malformed");
                }
                other => panic!("expected NodeErrorResponse, got {other:?}"),
            }
        });
    }

    #[test]
    fn t_pt_307_payload_too_large() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // Queue a response that should never be read
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x00]).unwrap()));

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let device_addr = [0xBB; 6];

            // Build a sensors list large enough to push encrypted payload > 202 bytes.
            // Each sensor with a 64-byte label adds ~70 bytes of CBOR.
            let big_sensors: Vec<SensorDescriptor> = (0..10)
                .map(|i| SensorDescriptor {
                    sensor_type: 1,
                    sensor_id: i,
                    label: Some("a]".repeat(32)),
                })
                .collect();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &device_addr,
                "sensor-1",
                &big_sensors,
            )
            .await;

            assert!(
                matches!(result, Err(PairingError::PayloadTooLarge(_))),
                "expected PayloadTooLarge, got {result:?}"
            );
            // No BLE write should have occurred
            assert!(
                transport.written.is_empty(),
                "no BLE write should occur when payload exceeds limit"
            );
        });
    }
}
