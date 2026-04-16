// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! ESP-NOW channel persistence tests (T-0815a through T-0815e).
//!
//! Validates GW-0808: the gateway persists the ESP-NOW channel in the database
//! so that the value survives gateway and modem restarts.

mod common;

use std::sync::Arc;

use sonde_gateway::ble_pairing::{handle_ble_recv, RegistrationWindow};
use sonde_gateway::engine::resolve_espnow_channel;
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::storage::{InMemoryStorage, Storage};

use sonde_protocol::modem::{encode_modem_frame, FrameDecoder, ModemMessage};
use sonde_protocol::{encode_ble_envelope, parse_ble_envelope};

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use tonic::Request;

use common::{build_admin_with_modem, read_modem_msg};

const BLE_MSG_REGISTER_PHONE: u8 = 0x02;
const BLE_MSG_PHONE_REGISTERED: u8 = 0x82;

/// Handle a mock modem SetChannel request: read SetChannel, reply with
/// SetChannelAck, and return the channel value.
async fn handle_mock_set_channel(
    server: &mut tokio::io::DuplexStream,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> u8 {
    let msg = read_modem_msg(server, decoder, buf).await;
    let channel = match msg {
        ModemMessage::SetChannel(ch) => ch,
        other => panic!("expected SetChannel, got {other:?}"),
    };
    let ack = ModemMessage::SetChannelAck(channel);
    tokio::io::AsyncWriteExt::write_all(server, &encode_modem_frame(&ack).unwrap())
        .await
        .unwrap();
    channel
}

/// T-0815a: After `SetModemChannel(7)`, the database contains `espnow_channel = "7"`.
#[tokio::test]
async fn t0815a_channel_persisted_after_set_modem_channel() {
    let (admin, mut server, _controller, storage) = build_admin_with_modem(1).await;

    // Call SetModemChannel(7) — the admin service sends SET_CHANNEL to the modem
    // and persists the new value after receiving SET_CHANNEL_ACK.
    let set_channel_fut = admin.set_modem_channel(Request::new(SetModemChannelRequest {
        channel: 7,
    }));

    // Handle the mock modem's SET_CHANNEL / SET_CHANNEL_ACK exchange.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let (result, acked_channel) = tokio::join!(
        set_channel_fut,
        handle_mock_set_channel(&mut server, &mut decoder, &mut buf)
    );

    result.expect("SetModemChannel RPC must succeed");
    assert_eq!(acked_channel, 7, "modem must receive channel 7");

    // Assert: the persisted value is "7".
    let persisted = storage
        .get_config("espnow_channel")
        .await
        .expect("get_config must succeed");
    assert_eq!(
        persisted,
        Some("7".to_string()),
        "espnow_channel must be persisted as \"7\" after SetModemChannel(7)"
    );
}

/// T-0815b: After `SetModemChannel(7)`, a modem reconnect sends `SET_CHANNEL(7)`.
///
/// This test verifies the component behavior: after persisting channel 7, the
/// production `resolve_espnow_channel` function returns 7 (not the CLI default 1),
/// and a transport created with that value sends `SET_CHANNEL(7)` during startup.
#[tokio::test]
async fn t0815b_modem_reconnect_restores_persisted_channel() {
    let (admin, mut server, _controller, storage) = build_admin_with_modem(1).await;

    // Persist channel 7 via SetModemChannel.
    let set_channel_fut = admin.set_modem_channel(Request::new(SetModemChannelRequest {
        channel: 7,
    }));
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let (result, _) = tokio::join!(
        set_channel_fut,
        handle_mock_set_channel(&mut server, &mut decoder, &mut buf)
    );
    result.expect("SetModemChannel RPC must succeed");

    // Simulate reconnect: use the production resolve function with CLI channel 1.
    // It must return 7 (from DB), not 1 (from CLI).
    let reconnect_channel = resolve_espnow_channel(&*storage, 1)
        .await
        .expect("resolve must succeed");
    assert_eq!(
        reconnect_channel, 7,
        "reconnect must use persisted channel 7, not CLI channel 1"
    );

    // Create a new transport using the resolved channel (simulating reconnect).
    // The startup handshake must send SET_CHANNEL(7), not SET_CHANNEL(1).
    let (client, mut reconnect_server) = tokio::io::duplex(4096);
    let transport_handle = tokio::spawn(async move {
        sonde_gateway::modem::UsbEspNowTransport::new(client, reconnect_channel)
            .await
            .unwrap()
    });

    // Perform the startup handshake on the mock side, asserting the channel.
    common::modem_startup(&mut reconnect_server, 7).await;
    let _transport = transport_handle.await.unwrap();
}

/// T-0815c: After `SetModemChannel(7)`, a BLE `REGISTER_PHONE` response
/// contains `rf_channel = 7`, not the CLI startup value.
#[tokio::test]
async fn t0815c_ble_pairing_uses_persisted_channel() {
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());

    // Simulate: gateway started with --channel 1, then SetModemChannel(7) persisted.
    storage
        .set_config("espnow_channel", "7")
        .await
        .expect("set_config must succeed");

    // Use the production resolve function — it must return 7 (from DB).
    let rf_channel = resolve_espnow_channel(&*storage, 1)
        .await
        .expect("resolve must succeed");
    assert_eq!(rf_channel, 7, "resolve must return persisted channel");

    let identity = GatewayIdentity::generate().unwrap();
    let mut window = RegistrationWindow::new();
    window.open(60);

    // Build REGISTER_PHONE body: phone_psk(32) + label_len(1) + label.
    let phone_psk = [0x42u8; 32];
    let label = b"test-phone";
    let mut body = Vec::with_capacity(33 + label.len());
    body.extend_from_slice(&phone_psk);
    body.push(label.len() as u8);
    body.extend_from_slice(label);

    let envelope = encode_ble_envelope(BLE_MSG_REGISTER_PHONE, &body).unwrap();
    let resp = handle_ble_recv(&envelope, &identity, &storage, &mut window, rf_channel, None)
        .await
        .expect("REGISTER_PHONE must produce a response");

    let (msg_type, resp_body) = parse_ble_envelope(&resp).unwrap();
    assert_eq!(msg_type, BLE_MSG_PHONE_REGISTERED);
    assert_eq!(resp_body.len(), 4, "PHONE_REGISTERED body must be 4 bytes");
    assert_eq!(resp_body[0], 0x00, "status must be 0 (accepted)");
    assert_eq!(
        resp_body[1], 7,
        "rf_channel in PHONE_REGISTERED must be 7 (from DB), not 1 (CLI)"
    );
}

/// T-0815d: On a fresh database with `--channel 3`, the database is seeded
/// with channel 3 and the modem startup sends `SET_CHANNEL(3)`.
///
/// Uses the production `resolve_espnow_channel` function to validate the
/// seeding contract.
#[tokio::test]
async fn t0815d_cli_channel_seeds_database_on_first_startup() {
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());

    // Precondition: fresh database has no persisted channel.
    assert_eq!(
        storage.get_config("espnow_channel").await.unwrap(),
        None,
        "fresh database must have no espnow_channel"
    );

    // Use the production resolve function with CLI channel 3.
    let startup_channel = resolve_espnow_channel(&*storage, 3)
        .await
        .expect("resolve must succeed");

    assert_eq!(startup_channel, 3, "startup must use CLI channel on fresh DB");

    // Assert: the database now contains "3".
    assert_eq!(
        storage.get_config("espnow_channel").await.unwrap(),
        Some("3".to_string()),
        "database must be seeded with CLI channel 3"
    );

    // Verify: modem startup uses the seeded channel.
    let (client, mut server) = tokio::io::duplex(4096);
    let transport_handle = tokio::spawn(async move {
        sonde_gateway::modem::UsbEspNowTransport::new(client, startup_channel)
            .await
            .unwrap()
    });
    common::modem_startup(&mut server, 3).await;
    let _transport = transport_handle.await.unwrap();
}

/// T-0815e: With a persisted channel of 7, `--channel 3` is ignored — the
/// gateway starts on channel 7.
///
/// Uses the production `resolve_espnow_channel` function to validate the
/// override contract.
#[tokio::test]
async fn t0815e_persisted_channel_overrides_cli_channel() {
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());

    // Pre-populate database with channel 7.
    storage
        .set_config("espnow_channel", "7")
        .await
        .expect("set_config must succeed");

    // Use the production resolve function with CLI channel 3.
    let startup_channel = resolve_espnow_channel(&*storage, 3)
        .await
        .expect("resolve must succeed");

    assert_eq!(
        startup_channel, 7,
        "persisted channel 7 must override CLI channel 3"
    );

    // Verify: modem startup sends SET_CHANNEL(7), not SET_CHANNEL(3).
    let (client, mut server) = tokio::io::duplex(4096);
    let transport_handle = tokio::spawn(async move {
        sonde_gateway::modem::UsbEspNowTransport::new(client, startup_channel)
            .await
            .unwrap()
    });
    common::modem_startup(&mut server, 7).await;
    let _transport = transport_handle.await.unwrap();
}
