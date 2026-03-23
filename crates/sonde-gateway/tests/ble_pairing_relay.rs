// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BLE pairing relay and failover identity tests (T-1223 through T-1226,
//! T-1107a).
//!
//! Covers the five validation gaps identified in issue #344:
//!
//! - T-1223: Ed25519 seed replication (GW-1203)
//! - T-1224: BLE GATT server via modem relay (GW-1204)
//! - T-1225: ATT MTU and fragmentation via modem relay (GW-1205)
//! - T-1226: BLE_ENABLE/BLE_DISABLE signals on window open/close (GW-1208)
//! - T-1107a: Modem RESET recovery re-executes startup (GW-1103)

mod common;

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signature, VerifyingKey};
use tokio::io::{duplex, AsyncWriteExt};
use tokio_stream::StreamExt;
use tonic::Request;
use zeroize::Zeroizing;

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use sonde_gateway::ble_pairing::{handle_ble_recv, RegistrationWindow};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::Transport;

use sonde_protocol::modem::{encode_modem_frame, BleRecv, FrameDecoder, ModemMessage};
use sonde_protocol::{encode_ble_envelope, parse_ble_envelope};

use common::{build_admin_with_modem, create_transport_and_server, read_modem_msg};

use tokio::io::DuplexStream;
use tonic::Status;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Collect a window event and a modem signal in either order, with a 5 s
/// timeout so tests fail fast instead of hanging CI.
async fn await_window_event_and_modem_signal(
    stream: &mut tokio_stream::wrappers::ReceiverStream<Result<BlePairingEvent, Status>>,
    server: &mut DuplexStream,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
    event_matches: impl Fn(&BlePairingEvent) -> bool,
    expected_msg: ModemMessage,
    label: &str,
) {
    let mut saw_event = false;
    let mut saw_modem = false;

    tokio::time::timeout(Duration::from_secs(5), async {
        while !(saw_event && saw_modem) {
            tokio::select! {
                maybe_event = stream.next(), if !saw_event => {
                    let event = maybe_event
                        .unwrap_or_else(|| panic!("{label}: stream ended"))
                        .unwrap_or_else(|e| panic!("{label}: stream error: {e}"));
                    assert!(event_matches(&event), "{label}: unexpected event {event:?}");
                    saw_event = true;
                }
                msg = async { read_modem_msg(server, decoder, buf).await }, if !saw_modem => {
                    assert_eq!(
                        expected_msg, msg,
                        "{label}: expected {expected_msg:?}, got {msg:?}"
                    );
                    saw_modem = true;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label}: timed out after 5 s"));
}

// ── Constants ───────────────────────────────────────────────────────────────

const BLE_MSG_REQUEST_GW_INFO: u8 = 0x01;
const BLE_MSG_GW_INFO_RESPONSE: u8 = 0x81;

/// GW_INFO_RESPONSE body length: 32 (public key) + 16 (gateway_id) + 64 (signature).
const GW_INFO_RESPONSE_LEN: usize = 32 + 16 + 64;

/// Maximum ATT characteristic value size for MTU=247 used in these tests:
/// 247 (ATT_MTU) − 3 (ATT header) = 244.
const ATT_VALUE_MAX_FOR_MTU_247: usize = 244;

/// Backwards-compatible alias; some tests still refer to this older name.
const DEFAULT_ATT_VALUE_MAX: usize = ATT_VALUE_MAX_FOR_MTU_247;

// ── T-1223: Ed25519 seed replication ────────────────────────────────────────

/// T-1223  Ed25519 seed replication (GW-1203).
///
/// 1. Generate gateway A identity; record public key and `gateway_id`.
/// 2. Export seed and `gateway_id`.
/// 3. Create gateway B by importing seed and `gateway_id`.
/// 4. Assert: B's public key matches A's.
/// 5. Assert: B's `gateway_id` matches A's.
/// 6. Send `REQUEST_GW_INFO` to both with the same challenge.
/// 7. Assert: both produce identical signatures.
#[tokio::test]
async fn t1223_ed25519_seed_replication() {
    // 1. Gateway A — generate identity.
    let identity_a = GatewayIdentity::generate().unwrap();
    let pub_key_a = *identity_a.public_key();
    let gateway_id_a = *identity_a.gateway_id();

    // 2. Export seed and gateway_id from A.
    let exported_seed = Zeroizing::new(*identity_a.seed());
    let exported_gateway_id = *identity_a.gateway_id();

    // 3. Gateway B — import seed and gateway_id.
    let identity_b = GatewayIdentity::from_parts(exported_seed, exported_gateway_id);

    // 4. Assert: B's public key matches A's.
    assert_eq!(
        identity_b.public_key(),
        &pub_key_a,
        "imported identity must have same public key"
    );

    // 5. Assert: B's gateway_id matches A's.
    assert_eq!(
        identity_b.gateway_id(),
        &gateway_id_a,
        "imported identity must have same gateway_id"
    );

    // 6. Send REQUEST_GW_INFO to both with the same challenge.
    let mut challenge = [0u8; 32];
    getrandom::fill(&mut challenge).unwrap();
    let envelope = encode_ble_envelope(BLE_MSG_REQUEST_GW_INFO, &challenge).unwrap();

    let storage_a: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    let storage_b: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    let mut window_a = RegistrationWindow::new();
    let mut window_b = RegistrationWindow::new();

    let resp_a = handle_ble_recv(&envelope, &identity_a, &storage_a, &mut window_a, 7, None)
        .await
        .expect("gateway A must respond");
    let resp_b = handle_ble_recv(&envelope, &identity_b, &storage_b, &mut window_b, 7, None)
        .await
        .expect("gateway B must respond");

    // Parse responses.
    let (type_a, body_a) = parse_ble_envelope(&resp_a).unwrap();
    let (type_b, body_b) = parse_ble_envelope(&resp_b).unwrap();
    assert_eq!(type_a, BLE_MSG_GW_INFO_RESPONSE);
    assert_eq!(type_b, BLE_MSG_GW_INFO_RESPONSE);

    // Verify response lengths before slicing.
    assert_eq!(
        body_a.len(),
        GW_INFO_RESPONSE_LEN,
        "GW_INFO_RESPONSE from A must be {GW_INFO_RESPONSE_LEN} bytes"
    );
    assert_eq!(
        body_b.len(),
        GW_INFO_RESPONSE_LEN,
        "GW_INFO_RESPONSE from B must be {GW_INFO_RESPONSE_LEN} bytes"
    );

    // Extract fields.
    let gw_pub_a: [u8; 32] = body_a[..32].try_into().unwrap();
    let gw_id_a: [u8; 16] = body_a[32..48].try_into().unwrap();
    let sig_a: [u8; 64] = body_a[48..GW_INFO_RESPONSE_LEN].try_into().unwrap();

    let gw_pub_b: [u8; 32] = body_b[..32].try_into().unwrap();
    let gw_id_b: [u8; 16] = body_b[32..48].try_into().unwrap();
    let sig_b: [u8; 64] = body_b[48..GW_INFO_RESPONSE_LEN].try_into().unwrap();

    // Verify response fields match the expected identity values (not just each other).
    assert_eq!(
        gw_pub_a, pub_key_a,
        "gateway A response must contain its known public key"
    );
    assert_eq!(
        gw_id_a, gateway_id_a,
        "gateway A response must contain its known gateway_id"
    );

    // 7. Assert: both produce identical public keys, gateway_ids, and signatures.
    assert_eq!(gw_pub_a, gw_pub_b, "public keys must match");
    assert_eq!(gw_id_a, gw_id_b, "gateway_ids must match");
    assert_eq!(
        sig_a, sig_b,
        "signatures must be identical for same challenge"
    );

    // Verify the signature is valid.
    let verifying_key = VerifyingKey::from_bytes(&gw_pub_a).unwrap();
    let mut sign_input = Vec::with_capacity(48);
    sign_input.extend_from_slice(&challenge);
    sign_input.extend_from_slice(&gw_id_a);
    let signature = Signature::from_bytes(&sig_a);
    assert!(
        verifying_key.verify_strict(&sign_input, &signature).is_ok(),
        "signature must verify"
    );
}

/// T-1223 also verifies persistence round-trip: export from storage, import
/// into another storage instance, and confirm identity consistency.
#[tokio::test]
async fn t1223_seed_replication_via_storage() {
    let storage_a = Arc::new(InMemoryStorage::new());
    let storage_b = Arc::new(InMemoryStorage::new());

    // Generate and persist on gateway A.
    let identity_a = GatewayIdentity::generate().unwrap();
    storage_a.store_gateway_identity(&identity_a).await.unwrap();

    // "Export" by loading from A's storage.
    let loaded = storage_a
        .load_gateway_identity()
        .await
        .unwrap()
        .expect("identity must be persisted");

    // "Import" into B's storage.
    let identity_b =
        GatewayIdentity::from_parts(Zeroizing::new(*loaded.seed()), *loaded.gateway_id());
    storage_b.store_gateway_identity(&identity_b).await.unwrap();

    // Reload from B's storage.
    let reloaded = storage_b
        .load_gateway_identity()
        .await
        .unwrap()
        .expect("identity must be persisted in B");

    assert_eq!(
        identity_a.public_key(),
        reloaded.public_key(),
        "public key must survive export/import"
    );
    assert_eq!(
        identity_a.gateway_id(),
        reloaded.gateway_id(),
        "gateway_id must survive export/import"
    );
}

// ── T-1224: BLE GATT server via modem relay ─────────────────────────────────

/// T-1224  BLE GATT server via modem relay (GW-1204).
///
/// Test flow:
/// 1. Initialize mock modem transport and in-memory gateway storage with a generated identity.
/// 2. Build a `REQUEST_GW_INFO` BLE envelope.
/// 3. Inject the envelope into the mock modem as a `BLE_RECV` frame.
/// 4. Allow the gateway's BLE handling logic to process the frame and emit a `BLE_INDICATE` response over the modem relay.
/// 5. Decode and verify the response contains `gw_public_key`, `gateway_id`, and a valid `signature`.
#[tokio::test]
async fn t1224_ble_gatt_server_via_modem_relay() {
    let (transport, mut server) = create_transport_and_server(6).await;
    let transport = Arc::new(transport);

    let identity = GatewayIdentity::generate().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    storage.store_gateway_identity(&identity).await.unwrap();

    // 3. Build a REQUEST_GW_INFO BLE envelope and inject as BLE_RECV.
    let mut challenge = [0u8; 32];
    getrandom::fill(&mut challenge).unwrap();
    let ble_envelope = encode_ble_envelope(BLE_MSG_REQUEST_GW_INFO, &challenge).unwrap();

    let ble_recv = ModemMessage::BleRecv(BleRecv {
        ble_data: ble_envelope,
    });
    server
        .write_all(&encode_modem_frame(&ble_recv).unwrap())
        .await
        .unwrap();

    // The gateway's BLE loop processes the message. We simulate the BLE
    // loop inline since integration tests don't start the gateway binary.
    let ble_event = tokio::time::timeout(Duration::from_secs(5), transport.recv_ble_event())
        .await
        .expect("timed out waiting for BLE event")
        .expect("expected BLE event");
    let recv_data = match ble_event {
        sonde_gateway::modem::BleEvent::Recv(br) => br.ble_data,
        other => panic!("expected BleEvent::Recv, got {other:?}"),
    };

    // Process through handle_ble_recv (same as gateway BLE loop).
    let mut window = RegistrationWindow::new();
    window.open(60);
    let response = handle_ble_recv(&recv_data, &identity, &storage, &mut window, 6, None)
        .await
        .expect("gateway must produce a BLE_INDICATE response");

    // 4. Send response back via BLE_INDICATE.
    transport.send_ble_indicate(&response).await.unwrap();

    // 5. Read BLE_INDICATE from mock modem side.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 4096];
    let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
    let indicate_data = match msg {
        ModemMessage::BleIndicate(bi) => bi.ble_data,
        other => panic!("expected BleIndicate, got {other:?}"),
    };

    // Decode the BLE envelope.
    let (msg_type, body) = parse_ble_envelope(&indicate_data).unwrap();
    assert_eq!(
        msg_type, BLE_MSG_GW_INFO_RESPONSE,
        "response must be GW_INFO_RESPONSE"
    );
    assert_eq!(
        body.len(),
        GW_INFO_RESPONSE_LEN,
        "GW_INFO_RESPONSE must be {GW_INFO_RESPONSE_LEN} bytes"
    );

    // Extract and verify fields.
    let gw_public_key: [u8; 32] = body[..32].try_into().unwrap();
    let gateway_id: [u8; 16] = body[32..48].try_into().unwrap();
    let sig_bytes: [u8; 64] = body[48..GW_INFO_RESPONSE_LEN].try_into().unwrap();

    assert_eq!(gw_public_key, *identity.public_key());
    assert_eq!(gateway_id, *identity.gateway_id());

    // Verify signature over (challenge ‖ gateway_id).
    let mut sign_input = Vec::with_capacity(48);
    sign_input.extend_from_slice(&challenge);
    sign_input.extend_from_slice(&gateway_id);

    let verifying_key = VerifyingKey::from_bytes(&gw_public_key).unwrap();
    let signature = Signature::from_bytes(&sig_bytes);
    assert!(
        verifying_key.verify_strict(&sign_input, &signature).is_ok(),
        "BLE_INDICATE response signature must verify"
    );
}

// ── T-1225: ATT MTU and fragmentation via modem relay ───────────────────────

/// T-1225  ATT MTU and fragmentation via modem relay (GW-1205).
///
/// Verifies the gateway sends complete BLE envelopes in a single
/// `BLE_INDICATE` message, even when the envelope exceeds the default
/// (ATT_MTU−3) = 244 byte characteristic value limit. The modem handles
/// ATT-level fragmentation per MD-0403 (delegation model).
///
/// Constructs a 300-byte BLE envelope (exceeding 244) and confirms it
/// is carried in a single `BLE_INDICATE` without gateway-side splitting.
#[tokio::test]
async fn t1225_att_mtu_fragmentation_via_modem_relay() {
    let (transport, mut server) = create_transport_and_server(6).await;
    let transport = Arc::new(transport);

    // Build an oversized BLE envelope: 300-byte body + 3-byte header = 303 bytes.
    // This exceeds the default (MTU−3)=244 limit, exercising the delegation model.
    let oversized_body = vec![0x42u8; DEFAULT_ATT_VALUE_MAX + 56];
    let oversized_envelope = encode_ble_envelope(BLE_MSG_GW_INFO_RESPONSE, &oversized_body)
        .expect("encoding must succeed");
    assert!(
        oversized_envelope.len() > DEFAULT_ATT_VALUE_MAX,
        "envelope must exceed (MTU-3)={DEFAULT_ATT_VALUE_MAX} bytes: actual {} bytes",
        oversized_envelope.len()
    );

    // Gateway sends the entire envelope in a single BLE_INDICATE.
    transport
        .send_ble_indicate(&oversized_envelope)
        .await
        .unwrap();

    // Read exactly one BLE_INDICATE from the mock modem.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 4096];
    let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
    let indicate_data = match msg {
        ModemMessage::BleIndicate(bi) => bi.ble_data,
        other => panic!("expected BleIndicate, got {other:?}"),
    };

    // Assert: the entire BLE envelope was sent in one message (no gateway-side
    // fragmentation). The modem is responsible for ATT-level fragmentation.
    assert_eq!(
        indicate_data, oversized_envelope,
        "BLE_INDICATE must contain the complete, unfragmented envelope"
    );

    // Verify the envelope is decodable.
    let (msg_type, body) = parse_ble_envelope(&indicate_data).unwrap();
    assert_eq!(msg_type, BLE_MSG_GW_INFO_RESPONSE);
    assert_eq!(body.len(), DEFAULT_ATT_VALUE_MAX + 56);

    // Also verify a real response (GW_INFO_RESPONSE = 112+3 = 115 bytes)
    // is sent in a single message as well.
    let identity = GatewayIdentity::generate().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    let mut challenge = [0u8; 32];
    getrandom::fill(&mut challenge).unwrap();
    let ble_envelope = encode_ble_envelope(BLE_MSG_REQUEST_GW_INFO, &challenge).unwrap();
    let mut window = RegistrationWindow::new();
    window.open(60);
    let response = handle_ble_recv(&ble_envelope, &identity, &storage, &mut window, 6, None)
        .await
        .expect("must produce response");

    transport.send_ble_indicate(&response).await.unwrap();
    let msg = read_modem_msg(&mut server, &mut decoder, &mut buf).await;
    let real_indicate = match msg {
        ModemMessage::BleIndicate(bi) => bi.ble_data,
        other => panic!("expected BleIndicate, got {other:?}"),
    };
    assert_eq!(
        real_indicate, response,
        "real GW_INFO_RESPONSE must also be sent unfragmented"
    );
}

// ── T-1226: BLE_ENABLE/BLE_DISABLE signals on window open/close ────────────

/// T-1226  BLE_ENABLE/BLE_DISABLE signals on window open/close (GW-1208).
///
/// 1. Open registration window via admin API.
/// 2. Assert: mock modem receives BLE_ENABLE.
/// 3. Close registration window explicitly.
/// 4. Assert: mock modem receives BLE_DISABLE.
/// 5. Open window again with 2s timeout.
/// 6. Wait for auto-close.
/// 7. Assert: mock modem receives BLE_ENABLE then BLE_DISABLE in order.
#[tokio::test]
async fn t1226_ble_enable_disable_signals() {
    let (admin, mut server, controller, _storage) = build_admin_with_modem(6).await;
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Open registration window.
    let request = Request::new(OpenBlePairingRequest { duration_s: 60 });
    let resp = admin.open_ble_pairing(request).await.unwrap();
    let mut stream = resp.into_inner();

    // Collect WindowOpened AND BLE_ENABLE in either order.
    await_window_event_and_modem_signal(
        &mut stream,
        &mut server,
        &mut decoder,
        &mut buf,
        |e| matches!(e.event, Some(ble_pairing_event::Event::WindowOpened(_))),
        ModemMessage::BleEnable,
        "open: WindowOpened + BLE_ENABLE",
    )
    .await;

    // 3. Close registration window explicitly.
    let close_resp = admin.close_ble_pairing(Request::new(Empty {})).await;
    assert!(close_resp.is_ok(), "CloseBlePairing must succeed");

    // Collect BLE_DISABLE AND WindowClosed in either order.
    await_window_event_and_modem_signal(
        &mut stream,
        &mut server,
        &mut decoder,
        &mut buf,
        |e| matches!(e.event, Some(ble_pairing_event::Event::WindowClosed(_))),
        ModemMessage::BleDisable,
        "close: WindowClosed + BLE_DISABLE",
    )
    .await;

    // Confirm window is actually closed.
    assert!(
        !controller.is_window_open().await,
        "window must be closed after explicit close"
    );

    // 5. Open again with a 1s timeout for auto-close.
    let request = Request::new(OpenBlePairingRequest { duration_s: 1 });
    let resp = admin.open_ble_pairing(request).await.unwrap();
    let mut stream2 = resp.into_inner();

    // Collect WindowOpened AND BLE_ENABLE in either order.
    await_window_event_and_modem_signal(
        &mut stream2,
        &mut server,
        &mut decoder,
        &mut buf,
        |e| matches!(e.event, Some(ble_pairing_event::Event::WindowOpened(_))),
        ModemMessage::BleEnable,
        "second open: WindowOpened + BLE_ENABLE",
    )
    .await;

    // 6. Wait for auto-close: collect BLE_DISABLE AND WindowClosed in either order.
    await_window_event_and_modem_signal(
        &mut stream2,
        &mut server,
        &mut decoder,
        &mut buf,
        |e| matches!(e.event, Some(ble_pairing_event::Event::WindowClosed(_))),
        ModemMessage::BleDisable,
        "auto-close: WindowClosed + BLE_DISABLE",
    )
    .await;
}

// ── T-1107a: Modem RESET recovery re-executes startup ───────────────────────

/// T-1107a  Modem RESET recovery re-executes startup (GW-1103).
///
/// After a modem ERROR, dropping and re-creating the transport (the
/// documented recovery pattern) re-executes the full RESET → MODEM_READY
/// → SET_CHANNEL startup sequence, restoring operational state.
#[tokio::test]
async fn t1107a_modem_reset_recovery() {
    // 1. Create initial transport and complete startup.
    let (transport, mut server) = create_transport_and_server(6).await;

    // 2. Inject an ERROR message.
    let error_msg = ModemMessage::Error(sonde_protocol::modem::ModemError {
        error_code: 0x01,
        message: b"ESPNOW_INIT_FAILED".to_vec(),
    });
    server
        .write_all(&encode_modem_frame(&error_msg).unwrap())
        .await
        .unwrap();

    // Verify transport still works after ERROR (reads next frame).
    let recv = ModemMessage::RecvFrame(sonde_protocol::modem::RecvFrame {
        peer_mac: [0x11; 6],
        rssi: -40,
        frame_data: vec![0xAA],
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    let (data, _) = transport.recv().await.unwrap();
    assert_eq!(data, vec![0xAA], "transport must still work after ERROR");

    // 3. Drop the transport to simulate the recovery pattern described in
    //    GW-1103: "drop + rebuild" the transport.
    drop(transport);
    drop(server);

    // 4. Re-create transport — this re-runs the full startup sequence.
    let (client2, mut server2) = duplex(4096);
    let transport_handle2 =
        tokio::spawn(async move { UsbEspNowTransport::new(client2, 6).await.unwrap() });

    // 5. Assert: startup sequence is re-executed (RESET → MODEM_READY → SET_CHANNEL).
    // Reuse shared modem startup handshake to avoid drift if the protocol changes.
    common::modem_startup(&mut server2, 6).await;

    // 6. Startup completes — transport is operational.
    let transport2 = transport_handle2.await.unwrap();

    // Verify the recovered transport works.
    let recv = ModemMessage::RecvFrame(sonde_protocol::modem::RecvFrame {
        peer_mac: [0x22; 6],
        rssi: -30,
        frame_data: vec![0xBB, 0xCC],
    });
    server2
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    let (data, peer) = transport2.recv().await.unwrap();
    assert_eq!(data, vec![0xBB, 0xCC], "recovered transport must recv");
    assert_eq!(peer, [0x22; 6].to_vec());
}
