// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::cbor::encode_pairing_request;
use crate::crypto;
use crate::envelope::{build_envelope, parse_envelope, parse_error_body, parse_node_ack};
use crate::error::PairingError;
use crate::rng::RngProvider;
use crate::store::PairingStore;
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

/// Phase 2: Provision a node via BLE.
///
/// Requires a prior Phase 1 pairing (artifacts in store). Generates a node PSK,
/// builds an authenticated+encrypted provision message, and sends it to the node.
///
/// # Re-run safety (PT-0600)
///
/// This function takes `&mut dyn BleTransport`, providing compile-time mutual
/// exclusion via the Rust borrow checker.  The store is read-only (`&dyn`).
/// Callers using `Arc<Mutex<..>>` for async sharing get serialized access
/// through the mutex.  Re-provisioning an already-paired node (without
/// holding the pairing button) returns
/// `Err(NodeProvisionFailed(AlreadyPaired))`; callers should treat this
/// as a non-destructive outcome.
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
    trace!("generated 32-byte node PSK");

    // Step 4: Compute node_key_hint
    let node_key_hint = compute_key_hint(&node_psk);
    trace!("computed node key hint");

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
    trace!("generated ephemeral X25519 keypair");

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
    trace!("AES-256-GCM encryption succeeded");

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

    // LESC enforcement (PT-0904): reject insecure pairing methods.
    enforce_lesc(transport).await?;

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
        return Err(PairingError::PayloadTooLarge {
            size: total_encrypted_len,
            max: PEER_PAYLOAD_MAX_LEN,
        });
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

    let message = build_envelope(NODE_PROVISION, &provision_payload).ok_or(
        PairingError::PayloadTooLarge {
            size: provision_payload.len(),
            max: u16::MAX as usize,
        },
    )?;

    // Step 13: Write to NODE_COMMAND_UUID
    trace!(msg = "NODE_PROVISION", len = message.len(), "BLE write");
    transport
        .write_characteristic(NODE_SERVICE_UUID, NODE_COMMAND_UUID, &message)
        .await?;

    // Step 14: Read indication (timeout 5s)
    trace!("waiting for NODE_ACK indication (5 s timeout)");
    let response = transport
        .read_indication(NODE_SERVICE_UUID, NODE_COMMAND_UUID, 5000)
        .await?;
    let (msg_type, payload) = parse_envelope(&response)?;
    trace!(
        msg_type = format_args!("0x{msg_type:02x}"),
        msg_name = msg_type_name(msg_type),
        len = payload.len(),
        "BLE indication received"
    );

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
                matches!(result, Err(PairingError::PayloadTooLarge { .. })),
                "expected PayloadTooLarge, got {result:?}"
            );
            // No BLE write should have occurred
            assert!(
                transport.written.is_empty(),
                "no BLE write should occur when payload exceeds limit"
            );
        });
    }

    // --- PT-0502: BLE disconnect on Phase 2 error paths ---

    /// Validates: PT-0502 (BLE disconnect on error)
    ///
    /// After Phase 2 failure (NODE_ACK with StorageError), the BLE connection
    /// must be released.
    #[test]
    fn t_pt_402_disconnect_on_phase2_failure() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x02]).unwrap()));

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

            assert!(result.is_err());
            assert!(
                !transport.connected,
                "BLE connection must be released after Phase 2 failure"
            );
            assert!(
                transport.disconnect_count > 0,
                "disconnect() must be called on Phase 2 error path"
            );
        });
    }

    /// Validates: PT-0502 (BLE disconnect on success)
    ///
    /// After Phase 2 success, the BLE connection must also be released.
    #[test]
    fn t_pt_402_disconnect_on_phase2_success() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x00]).unwrap()));

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

            assert!(result.is_ok());
            assert!(
                !transport.connected,
                "BLE connection must be released after Phase 2 success"
            );
            assert!(
                transport.disconnect_count > 0,
                "disconnect() must be called after Phase 2 completes"
            );
        });
    }

    // --- PT-0904: LESC pairing method enforcement (Phase 2) ---

    /// T-PT-804 equivalent for Phase 2: Numeric Comparison enforced.
    ///
    /// When the transport reports Numeric Comparison as the pairing method,
    /// Phase 2 (node provisioning) must proceed normally and complete.
    #[test]
    fn t_pt_804_numeric_comparison_enforced_phase2() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.pairing_method = Some(PairingMethod::NumericComparison);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x00]).unwrap()));

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

            assert!(
                result.is_ok(),
                "provisioning should succeed with NumericComparison"
            );
        });
    }

    /// T-PT-805 equivalent for Phase 2: Just Works fallback rejected.
    ///
    /// When the transport reports Just Works as the pairing method, Phase 2
    /// must reject the connection before sending NODE_PROVISION and report
    /// an insecure pairing method error.
    #[test]
    fn t_pt_805_just_works_rejected_phase2() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.pairing_method = Some(PairingMethod::JustWorks);

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

    // --- PT-1000: Phase 2 disconnect mid-provision ---

    /// Validates: PT-1000
    ///
    /// Inject a BLE disconnect during Phase 2 (ConnectionDropped on
    /// NODE_ACK indication read).  Verify the tool returns an error
    /// cleanly and releases the BLE connection.
    #[test]
    fn t_pt_800_phase2_disconnect_mid_provision() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Err(PairingError::ConnectionDropped));

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

            assert!(
                matches!(result, Err(PairingError::ConnectionDropped)),
                "expected ConnectionDropped, got {result:?}"
            );
            assert!(
                !transport.connected,
                "transport must be disconnected after Phase 2 disconnect"
            );
        });
    }

    // --- PT-1001: No resource leaks on Phase 2 failure ---

    /// Validates: PT-1001
    ///
    /// Run multiple Phase 2 attempts that fail at different stages.
    /// After each failure, verify no open connections remain.
    #[test]
    fn t_pt_801_no_resource_leaks_phase2() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let device_addr = [0xBB; 6];
            let sensors = test_sensors();

            let failure_scenarios: Vec<Box<dyn Fn() -> MockBleTransport>> = vec![
                // 1. Connection failure
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.connect_error = Some(PairingError::ConnectionFailed("test".into()));
                    t
                }),
                // 2. MTU too low
                Box::new(|| MockBleTransport::new(100)),
                // 3. GATT write failure
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.write_error = Some(PairingError::GattWriteFailed("test".into()));
                    t
                }),
                // 4. Indication timeout (no response)
                Box::new(|| MockBleTransport::new(247)),
                // 5. ConnectionDropped during read
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Err(PairingError::ConnectionDropped));
                    t
                }),
                // 6. NODE_ACK(0x01) — already paired
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_envelope(NODE_ACK, &[0x01]).unwrap()));
                    t
                }),
                // 7. NODE_ACK(0x02) — storage error
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    t.queue_response(Ok(build_envelope(NODE_ACK, &[0x02]).unwrap()));
                    t
                }),
                // 8. Error response from node
                Box::new(|| {
                    let mut t = MockBleTransport::new(247);
                    let mut error_body = vec![0x01];
                    error_body.extend_from_slice(b"fail");
                    t.queue_response(Ok(build_envelope(MSG_ERROR, &error_body).unwrap()));
                    t
                }),
            ];

            for (i, make_transport) in failure_scenarios.iter().enumerate() {
                let mut transport = make_transport();
                let store = store_with_artifacts();
                let rng = MockRng::new([0x42u8; 32]);

                let result = provision_node(
                    &mut transport,
                    &store,
                    &rng,
                    &device_addr,
                    "sensor-1",
                    &sensors[..1],
                )
                .await;
                assert!(result.is_err(), "scenario {i} should fail");
                assert!(
                    !transport.connected,
                    "scenario {i}: transport must not be connected after Phase 2 failure"
                );
            }
        });
    }

    // --- PT-1002: Connection timeout exercised in Phase 2 ---

    /// Validates: PT-1002
    ///
    /// Exercise the BLE connection timeout path in Phase 2.
    #[test]
    fn t_pt_802_connection_timeout_phase2() {
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
                Err(PairingError::Timeout {
                    operation,
                    duration_secs,
                }) => {
                    assert_eq!(operation, "BLE connection");
                    assert_eq!(duration_secs, 10);
                }
                other => panic!("expected Timeout, got {other:?}"),
            }
            assert!(!transport.connected);
        });
    }

    // --- PT-1003: No implicit retries in Phase 2 ---

    /// Validates: PT-1003
    ///
    /// Inject a GATT write failure on the NODE_PROVISION write.
    /// Assert that exactly one write was recorded (the failed write is not retried).
    #[test]
    fn t_pt_803_no_implicit_retries_phase2_write() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.write_error = Some(PairingError::GattWriteFailed("BLE write failed".into()));

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
    /// Inject a GATT read failure (indication timeout) during Phase 2.
    /// Assert the error propagates without retry.
    #[test]
    fn t_pt_803_no_implicit_retries_phase2_read() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // No response → IndicationTimeout

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
            // Exactly one write (NODE_PROVISION) before the read timeout.
            assert_eq!(
                transport.written.len(),
                1,
                "exactly one write before timeout — no retry of the read"
            );
            // And exactly one read_indication() attempt — no implicit retries on read failure.
            assert_eq!(
                transport.read_call_count, 1,
                "read_indication() must be called exactly once — no implicit retries"
            );
        });
    }

    // --- PT-0408: Error path cleanup ---

    /// Verify that `provision_node` returns cleanly on error paths without
    /// panics. Zeroing of ephemeral keys and `node_psk` is handled by
    /// `Zeroizing` wrappers by construction and is not directly asserted here.
    #[test]
    fn t_pt_308_zeroing_on_error_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Error path: node returns an error response
            let mut transport = MockBleTransport::new(247);
            let mut error_body = vec![0x03];
            error_body.extend_from_slice(b"unexpected");
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &error_body).unwrap()));

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &[0xBB; 6],
                "sensor-1",
                &sensors[..1],
            )
            .await;

            // Function returned an error cleanly; zeroing of ephemeral keys
            // and node_psk is handled by `Zeroizing` wrappers by construction.
            assert!(
                matches!(result, Err(PairingError::NodeErrorResponse { .. })),
                "expected NodeErrorResponse, got {result:?}"
            );
        });
    }

    /// Verify error-path cleanup on indication timeout (no response from node).
    #[test]
    fn t_pt_308b_zeroing_on_timeout() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // No response queued → IndicationTimeout

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &[0xBB; 6],
                "sensor-1",
                &sensors[..1],
            )
            .await;

            assert!(matches!(result, Err(PairingError::IndicationTimeout)));
            // Zeroing is handled by `Zeroizing` wrappers by construction.
        });
    }

    // --- PT-0405: Fresh ephemeral X25519 per provisioning attempt ---

    /// Two provisioning attempts with different RNG seeds must use different
    /// ephemeral public keys in the NODE_PROVISION write.
    #[test]
    fn t_pt_309_fresh_ephemeral_per_attempt() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = store_with_artifacts();
            let sensors = test_sensors();

            // Attempt 1 with seed 0x42
            let rng1 = MockRng::new([0x42u8; 32]);
            let mut transport1 = MockBleTransport::new(247);
            transport1.queue_response(Ok(build_envelope(NODE_ACK, &[0x00]).unwrap()));
            provision_node(
                &mut transport1,
                &store,
                &rng1,
                &[0xBB; 6],
                "sensor-1",
                &sensors,
            )
            .await
            .unwrap();

            // Attempt 2 with seed 0x43
            let rng2 = MockRng::new([0x43u8; 32]);
            let mut transport2 = MockBleTransport::new(247);
            transport2.queue_response(Ok(build_envelope(NODE_ACK, &[0x00]).unwrap()));
            provision_node(
                &mut transport2,
                &store,
                &rng2,
                &[0xBB; 6],
                "sensor-2",
                &sensors,
            )
            .await
            .unwrap();

            // Extract NODE_PROVISION payloads and compare ephemeral public keys
            assert!(
                !transport1.written.is_empty(),
                "transport1 wrote no frames; expected at least one NODE_PROVISION frame"
            );
            assert!(
                !transport2.written.is_empty(),
                "transport2 wrote no frames; expected at least one NODE_PROVISION frame"
            );
            let payload1 = transport1
                .written
                .iter()
                .find_map(|(_, _, data)| {
                    let (msg_type, payload) = crate::envelope::parse_envelope(data).unwrap();
                    if msg_type == NODE_PROVISION {
                        Some(payload.to_vec())
                    } else {
                        None
                    }
                })
                .expect("expected at least one NODE_PROVISION frame in transport1.written");

            let payload2 = transport2
                .written
                .iter()
                .find_map(|(_, _, data)| {
                    let (msg_type, payload) = crate::envelope::parse_envelope(data).unwrap();
                    if msg_type == NODE_PROVISION {
                        Some(payload.to_vec())
                    } else {
                        None
                    }
                })
                .expect("expected at least one NODE_PROVISION frame in transport2.written");

            // NODE_PROVISION layout:
            // node_key_hint(2) + node_psk(32) + rf_channel(1) + payload_len(2) + eph_public(32) + ...
            const NODE_KEY_HINT_LEN: usize = 2;
            const NODE_PSK_LEN: usize = 32;
            const RF_CHANNEL_LEN: usize = 1;
            const PAYLOAD_LEN_FIELD_LEN: usize = 2;
            const EPH_PUBLIC_LEN: usize = 32;

            let eph_offset =
                NODE_KEY_HINT_LEN + NODE_PSK_LEN + RF_CHANNEL_LEN + PAYLOAD_LEN_FIELD_LEN;
            let eph_end = eph_offset + EPH_PUBLIC_LEN;
            assert!(
                payload1.len() >= eph_end,
                "NODE_PROVISION payload1 too short: len={} expected at least {eph_end}",
                payload1.len()
            );
            assert!(
                payload2.len() >= eph_end,
                "NODE_PROVISION payload2 too short: len={} expected at least {eph_end}",
                payload2.len()
            );
            let eph1 = &payload1[eph_offset..eph_end];
            let eph2 = &payload2[eph_offset..eph_end];

            assert_ne!(
                eph1, eph2,
                "ephemeral public keys must differ between provisioning attempts"
            );
        });
    }

    // --- PT-0500: Additional error subcategories ---

    /// Unknown NodeAckStatus byte (not 0x00, 0x01, or 0x02) should be reported
    /// as `NodeProvisionFailed(Unknown(byte))`.
    #[test]
    fn t_pt_500_unknown_node_ack_status() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(NODE_ACK, &[0x99]).unwrap()));

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &[0xBB; 6],
                "sensor-1",
                &sensors[..1],
            )
            .await;

            match result {
                Err(PairingError::NodeProvisionFailed(NodeAckStatus::Unknown(0x99))) => {}
                other => panic!("expected NodeProvisionFailed(Unknown(0x99)), got {other:?}"),
            }
        });
    }

    /// ERROR(0xFF) from node with status 0x00 and empty diagnostic.
    #[test]
    fn t_pt_500b_node_error_empty_diagnostic() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            transport.queue_response(Ok(build_envelope(MSG_ERROR, &[0x00]).unwrap()));

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &[0xBB; 6],
                "sensor-1",
                &sensors[..1],
            )
            .await;

            match result {
                Err(PairingError::NodeErrorResponse { status, message }) => {
                    assert_eq!(status, 0x00);
                    assert!(message.is_empty());
                }
                other => panic!("expected NodeErrorResponse with status 0x00, got {other:?}"),
            }
        });
    }

    /// Unexpected message type (not NODE_ACK or MSG_ERROR) should be
    /// reported as `InvalidResponse`.
    #[test]
    fn t_pt_500c_unexpected_msg_type() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(247);
            // Respond with an unknown type 0x42
            transport.queue_response(Ok(build_envelope(0x42, &[0x00]).unwrap()));

            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &[0xBB; 6],
                "sensor-1",
                &sensors[..1],
            )
            .await;

            match result {
                Err(PairingError::InvalidResponse { msg_type, .. }) => {
                    assert_eq!(msg_type, 0x42);
                }
                other => panic!("expected InvalidResponse, got {other:?}"),
            }
        });
    }

    /// MTU too low for Phase 2 should be reported before any writes.
    #[test]
    fn t_pt_500d_mtu_too_low() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut transport = MockBleTransport::new(100); // below minimum
            let store = store_with_artifacts();
            let rng = MockRng::new([0x42u8; 32]);
            let sensors = test_sensors();

            let result = provision_node(
                &mut transport,
                &store,
                &rng,
                &[0xBB; 6],
                "sensor-1",
                &sensors[..1],
            )
            .await;

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
            assert!(
                transport.written.is_empty(),
                "no writes should occur when MTU is too low"
            );
        });
    }
}
