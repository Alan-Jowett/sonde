// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Modem transport tests (T-1100 through T-1108 except T-1105 which is
//! already in phase2d.rs). Also includes T-1104d and T-1104e (warm reboot).

use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::modem::{UsbEspNowTransport, DEFAULT_MAX_HEALTH_POLL_FAILURES};
use sonde_gateway::transport::Transport;

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, ModemStatus, RecvFrame,
};
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

// ─── Helpers ────────────────────────────────────────────────────────────

async fn read_next_message(
    stream: &mut DuplexStream,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> ModemMessage {
    loop {
        match decoder.decode() {
            Ok(Some(msg)) => return msg,
            Ok(None) => {}
            Err(e) => panic!("decode error: {e}"),
        }
        let n = stream.read(buf).await.expect("read failed");
        assert!(n > 0, "stream closed unexpectedly");
        decoder.push(&buf[..n]);
    }
}

async fn do_startup_handshake(server: &mut DuplexStream, channel: u8) {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Read RESET
    let msg = read_next_message(server, &mut decoder, &mut buf).await;
    assert!(matches!(msg, ModemMessage::Reset), "expected Reset");

    // 2. Send MODEM_READY
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 0, 0, 0],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    server
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    // 3. Read SET_CHANNEL
    let msg = read_next_message(server, &mut decoder, &mut buf).await;
    let ch = match msg {
        ModemMessage::SetChannel(c) => c,
        other => panic!("expected SetChannel, got {other:?}"),
    };
    assert_eq!(ch, channel);

    // 4. Send SET_CHANNEL_ACK
    server
        .write_all(&encode_modem_frame(&ModemMessage::SetChannelAck(ch)).unwrap())
        .await
        .unwrap();
}

async fn create_transport_and_server(channel: u8) -> (UsbEspNowTransport, DuplexStream) {
    let (client, mut server) = duplex(4096);

    let transport_handle =
        tokio::spawn(async move { UsbEspNowTransport::new(client, channel).await.unwrap() });

    do_startup_handshake(&mut server, channel).await;

    let transport = transport_handle.await.unwrap();
    (transport, server)
}

// ── T-1100: recv delivers RECV_FRAME ────────────────────────────────────

/// T-1100  UsbEspNowTransport — recv delivers RECV_FRAME.
#[tokio::test]
async fn t1100_recv_delivers_recv_frame() {
    let (transport, mut server) = create_transport_and_server(6).await;

    // Inject RECV_FRAME from mock modem.
    let frame_data = vec![0x01, 0x02, 0x03, 0x04];
    let peer_mac = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac,
        rssi: -50,
        frame_data: frame_data.clone(),
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    // Read from transport.
    let (data, peer) = transport.recv().await.unwrap();
    assert_eq!(data, frame_data);
    assert_eq!(peer, peer_mac.to_vec());
}

// ── T-1101: send produces SEND_FRAME ────────────────────────────────────

/// T-1101  UsbEspNowTransport — send produces SEND_FRAME.
#[tokio::test]
async fn t1101_send_produces_send_frame() {
    let (transport, mut server) = create_transport_and_server(6).await;

    let frame_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let peer_mac = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    transport.send(&frame_data, &peer_mac).await.unwrap();

    // Read the SEND_FRAME from the mock modem side.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    match msg {
        ModemMessage::SendFrame(sf) => {
            assert_eq!(sf.frame_data, frame_data);
            assert_eq!(sf.peer_mac, [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        }
        other => panic!("expected SendFrame, got {other:?}"),
    }
}

// ── T-1102: internal message demux ──────────────────────────────────────

/// T-1102  UsbEspNowTransport — internal message demux.
///
/// STATUS messages are handled internally; only RECV_FRAME surfaces via recv().
#[tokio::test]
async fn t1102_internal_message_demux() {
    let (transport, mut server) = create_transport_and_server(6).await;

    // Inject a STATUS (internal — should not surface via recv).
    let status = ModemMessage::Status(ModemStatus {
        uptime_s: 100,
        tx_count: 5,
        rx_count: 3,
        tx_fail_count: 0,
        channel: 6,
    });
    server
        .write_all(&encode_modem_frame(&status).unwrap())
        .await
        .unwrap();

    // Inject a RECV_FRAME.
    let frame_data = vec![0xAA, 0xBB];
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac: [0x11; 6],
        rssi: -40,
        frame_data: frame_data.clone(),
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    // recv() should return the RECV_FRAME data, not STATUS.
    let (data, _) = transport.recv().await.unwrap();
    assert_eq!(data, frame_data);
}

// ── T-1103: Startup — RESET then MODEM_READY then SET_CHANNEL ──────────

/// T-1103  Startup — RESET then MODEM_READY then SET_CHANNEL.
#[tokio::test]
async fn t1103_startup_sequence() {
    let (client, mut server) = duplex(4096);

    let transport_handle =
        tokio::spawn(async move { UsbEspNowTransport::new(client, 6).await.unwrap() });

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Assert: RESET received first.
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    assert!(
        matches!(msg, ModemMessage::Reset),
        "first msg must be RESET"
    );

    // 2. Send MODEM_READY.
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [2, 0, 0, 1],
        mac_address: [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01],
    });
    server
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    // 3. Assert: SET_CHANNEL(6).
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    match msg {
        ModemMessage::SetChannel(ch) => assert_eq!(ch, 6),
        other => panic!("expected SetChannel, got {other:?}"),
    }

    // 4. Send SET_CHANNEL_ACK.
    server
        .write_all(&encode_modem_frame(&ModemMessage::SetChannelAck(6)).unwrap())
        .await
        .unwrap();

    // 5. Startup completes.
    let _transport = transport_handle.await.unwrap();
}

// ── T-1104: Startup — MODEM_READY timeout ───────────────────────────────

/// T-1104  Startup — MODEM_READY timeout.
///
/// If the modem never sends MODEM_READY, startup returns an error.
#[tokio::test]
async fn t1104_startup_modem_ready_timeout() {
    let (client, _server) = duplex(4096);
    // Don't respond with MODEM_READY — transport should timeout.
    let result =
        tokio::time::timeout(Duration::from_secs(10), UsbEspNowTransport::new(client, 6)).await;

    match result {
        Ok(Err(_)) => {} // Transport returned error — expected
        Err(_) => {}     // Tokio timeout — also acceptable
        Ok(Ok(_)) => panic!("startup should not succeed without MODEM_READY"),
    }
}

// ── T-1106: Health monitoring — uptime reset detection ──────────────────

/// T-1106  Health monitoring — uptime reset detection.
///
/// Two sequential status polls where uptime decreases indicate a reboot.
#[tokio::test]
async fn t1106_uptime_reset_detection() {
    let (transport, mut server) = create_transport_and_server(6).await;
    let transport = Arc::new(transport);

    // First poll: spawn poll_status, respond with uptime=120.
    let t1 = Arc::clone(&transport);
    let poll1 = tokio::spawn(async move { t1.poll_status().await });

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    assert!(matches!(msg, ModemMessage::GetStatus));

    let status_resp = ModemMessage::Status(ModemStatus {
        uptime_s: 120,
        tx_count: 10,
        rx_count: 5,
        tx_fail_count: 0,
        channel: 6,
    });
    server
        .write_all(&encode_modem_frame(&status_resp).unwrap())
        .await
        .unwrap();

    let status1 = poll1.await.unwrap().unwrap();
    assert_eq!(status1.uptime_s, 120);

    // Small delay to ensure the transport is ready for next poll.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second poll: uptime=3 (reboot).
    let t2 = Arc::clone(&transport);
    let poll2 = tokio::spawn(async move { t2.poll_status().await });

    // Give the transport time to send GET_STATUS.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    assert!(matches!(msg, ModemMessage::GetStatus));

    let status_resp2 = ModemMessage::Status(ModemStatus {
        uptime_s: 3,
        tx_count: 0,
        rx_count: 0,
        tx_fail_count: 0,
        channel: 6,
    });
    server
        .write_all(&encode_modem_frame(&status_resp2).unwrap())
        .await
        .unwrap();

    let status2 = poll2.await.unwrap().unwrap();
    assert_eq!(status2.uptime_s, 3);

    // Caller would detect: uptime decreased 120 → 3 → modem rebooted.
    assert!(status2.uptime_s < status1.uptime_s);
}

// ── T-1107: Modem ERROR handling ────────────────────────────────────────

/// T-1107  Modem ERROR handling.
///
/// Inject an ERROR message from the mock modem. The transport logs it.
#[tokio::test]
async fn t1107_modem_error_handling() {
    let (transport, mut server) = create_transport_and_server(6).await;

    // Inject an ERROR message.
    let error_msg = ModemMessage::Error(sonde_protocol::modem::ModemError {
        error_code: 0x01,
        message: b"test error".to_vec(),
    });
    server
        .write_all(&encode_modem_frame(&error_msg).unwrap())
        .await
        .unwrap();

    // The transport should not crash. Send a RECV_FRAME after the error
    // to verify the transport is still operational.
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac: [0x11; 6],
        rssi: -40,
        frame_data: vec![0xFF],
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    let (data, _) = transport.recv().await.unwrap();
    assert_eq!(data, vec![0xFF], "transport must still work after ERROR");
}

// ── T-1108: End-to-end wake cycle over PTY ──────────────────────────────

/// T-1108  End-to-end wake cycle over PTY.
///
/// Full gateway + modem transport: inject a WAKE via RECV_FRAME, verify
/// the gateway sends a COMMAND via SEND_FRAME.
#[tokio::test]
async fn t1108_e2e_wake_cycle_over_pty() {
    use sonde_gateway::crypto::RustCryptoSha256;
    use sonde_gateway::engine::Gateway;
    use sonde_gateway::registry::NodeRecord;
    use sonde_gateway::storage::{InMemoryStorage, Storage};
    use sonde_gateway::GatewayAead;
    use sonde_protocol::{
        decode_frame, encode_frame, open_frame, FrameHeader, GatewayMessage, NodeMessage,
        Sha256Provider, MSG_COMMAND, MSG_WAKE,
    };

    let psk = [0x42u8; 32];
    let sha = sonde_gateway::RustCryptoSha256;
    let hash = sha.hash(&psk);
    let key_hint = u16::from_be_bytes([hash[30], hash[31]]);

    let storage = Arc::new(InMemoryStorage::new());
    let node = NodeRecord::new("pty-node".into(), key_hint, psk);
    storage.upsert_node(&node).await.unwrap();

    let (transport, mut server) = create_transport_and_server(7).await;
    let gateway = Gateway::new(storage, Duration::from_secs(30));

    // Build a valid WAKE frame.
    let header = FrameHeader {
        key_hint,
        msg_type: MSG_WAKE,
        nonce: 42,
    };
    let wake_msg = NodeMessage::Wake {
        firmware_abi_version: 1,
        program_hash: vec![0u8; 32],
        battery_mv: 3300,
        firmware_version: "0.4.0".into(),
        blob: None,
    };
    let cbor = wake_msg.encode().unwrap();
    let wake_frame = encode_frame(&header, &cbor, &psk, &GatewayAead, &RustCryptoSha256).unwrap();

    // Inject RECV_FRAME carrying the WAKE.
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac: [0x11; 6],
        rssi: -30,
        frame_data: wake_frame,
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    // Read the WAKE frame from the transport.
    let (frame_data, peer) = transport.recv().await.unwrap();

    // Process through gateway.
    let response = gateway.process_frame(&frame_data, peer).await;
    assert!(response.is_some(), "gateway must respond to WAKE");

    // Send response back through transport.
    let resp_data = response.unwrap();
    transport
        .send(&resp_data, &[0x11; 6].to_vec())
        .await
        .unwrap();

    // Read SEND_FRAME from server side.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 512];
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    match msg {
        ModemMessage::SendFrame(sf) => {
            // Decode the COMMAND response.
            let decoded = decode_frame(&sf.frame_data).unwrap();
            assert_eq!(decoded.header.msg_type, MSG_COMMAND);
            let plaintext = open_frame(&decoded, &psk, &GatewayAead, &RustCryptoSha256).unwrap();
            let gw_msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
            match gw_msg {
                GatewayMessage::Command {
                    starting_seq,
                    timestamp_ms,
                    ..
                } => {
                    assert!(starting_seq > 0, "starting_seq must be non-zero");
                    assert!(timestamp_ms > 0, "timestamp_ms must be non-zero");
                }
                other => panic!("expected Command, got {other:?}"),
            }
        }
        other => panic!("expected SendFrame, got {other:?}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Issue #352 — Modem transport gap tests
// ═══════════════════════════════════════════════════════════════════════

// ── Gap 5: GW-1101 — SET_CHANNEL_ACK timeout ────────────────────────────

/// GW-1101: SET_CHANNEL_ACK timeout.
///
/// Modem sends MODEM_READY (ACKs RESET) but never sends SET_CHANNEL_ACK.
/// The gateway must detect this and return an error rather than operating
/// on the wrong RF channel.
#[tokio::test]
async fn gw1101_set_channel_ack_timeout() {
    let (client, mut server) = duplex(4096);

    let transport_handle = tokio::spawn(async move { UsbEspNowTransport::new(client, 6).await });

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Read RESET
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    assert!(matches!(msg, ModemMessage::Reset));

    // 2. Send MODEM_READY
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 0, 0, 0],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    server
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    // 3. Read SET_CHANNEL — but do NOT send SET_CHANNEL_ACK
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    assert!(
        matches!(msg, ModemMessage::SetChannel(6)),
        "expected SetChannel(6), got {msg:?}"
    );

    // 4. Transport must fail (e.g., due to a timeout) rather than succeeding
    let result = tokio::time::timeout(Duration::from_secs(10), transport_handle).await;
    match result {
        Ok(Ok(Err(_))) => {
            // Transport failed as expected when SET_CHANNEL_ACK was not received.
        }
        Ok(Err(e)) => panic!("spawn panicked: {e}"),
        Err(_) => panic!("test timed out — transport should fail within ~10s"),
        Ok(Ok(Ok(_))) => panic!("transport must not succeed without SET_CHANNEL_ACK"),
    }
}

// ── Gap 6: GW-1103 AC2 — Recovery after ERROR → RESET ──────────────────

/// GW-1103 AC2: After modem ERROR, drop + reconstruct transport re-executes
/// the full startup sequence (RESET → MODEM_READY → SET_CHANNEL → ACK).
#[tokio::test]
async fn gw1103_error_recovery_full_restart() {
    // Phase 1: Normal startup
    let (transport, mut server1) = create_transport_and_server(6).await;

    // Inject ERROR from modem
    let error_msg = ModemMessage::Error(sonde_protocol::modem::ModemError {
        error_code: 0x02,
        message: b"ESPNOW_INIT_FAILED".to_vec(),
    });
    server1
        .write_all(&encode_modem_frame(&error_msg).unwrap())
        .await
        .unwrap();

    // Verify transport still receives frames after ERROR (non-fatal)
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac: [0x11; 6],
        rssi: -40,
        frame_data: vec![0xAA],
    });
    server1
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();
    let (data, _) = tokio::time::timeout(Duration::from_secs(10), transport.recv())
        .await
        .expect("transport recv timed out in gw1103_error_recovery_full_restart")
        .unwrap();
    assert_eq!(data, vec![0xAA], "transport must survive ERROR");

    // Phase 2: Drop and reconstruct — simulating gateway recovery
    drop(transport);
    drop(server1);

    // Create new transport on a fresh connection — must re-run startup
    let (client2, mut server2) = duplex(4096);
    let transport_handle =
        tokio::spawn(async move { UsbEspNowTransport::new(client2, 6).await.unwrap() });

    // Verify the FULL startup sequence re-executes
    let mut decoder2 = FrameDecoder::new();
    let mut buf2 = [0u8; 256];

    let msg = read_next_message(&mut server2, &mut decoder2, &mut buf2).await;
    assert!(
        matches!(msg, ModemMessage::Reset),
        "recovery must start with RESET"
    );

    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 0, 0, 0],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    server2
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    let msg = read_next_message(&mut server2, &mut decoder2, &mut buf2).await;
    assert!(
        matches!(msg, ModemMessage::SetChannel(6)),
        "recovery must send SET_CHANNEL"
    );

    server2
        .write_all(&encode_modem_frame(&ModemMessage::SetChannelAck(6)).unwrap())
        .await
        .unwrap();

    let _transport2 = tokio::time::timeout(Duration::from_secs(10), transport_handle)
        .await
        .expect("transport startup timed out in gw1103_error_recovery_full_restart")
        .unwrap();
}

// ── Gap 7: GW-1205 — BLE indication sent to modem ──────────────────────

/// GW-1205: `send_ble_indicate` transmits a BLE_INDICATE message to the
/// modem, verifying the gateway doesn't fire-and-forget without actually
/// sending the data.
#[tokio::test]
async fn gw1205_ble_indicate_sent_to_modem() {
    let (transport, mut server) = create_transport_and_server(6).await;

    let payload = vec![0x01, 0x02, 0x03, 0x04, 0x05];
    transport.send_ble_indicate(&payload).await.unwrap();

    // Read the BLE_INDICATE from the mock modem side (with timeout to avoid
    // hanging CI if the transport regresses and stops emitting the frame).
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let msg = tokio::time::timeout(
        Duration::from_secs(5),
        read_next_message(&mut server, &mut decoder, &mut buf),
    )
    .await
    .expect("timed out waiting for BLE_INDICATE from modem transport");
    match msg {
        ModemMessage::BleIndicate(bi) => {
            assert_eq!(
                bi.ble_data, payload,
                "BLE_INDICATE must carry the exact payload"
            );
        }
        other => panic!("expected BleIndicate, got {other:?}"),
    }
}

// ── T-1104c: Health poll — sustained failures trigger reconnect ─────────

/// T-1104c  Sustained health poll failures trigger reconnect signal.
///
/// After `max_consecutive_failures` consecutive poll failures the health
/// monitor must return `true` (reconnect needed).
#[tokio::test]
async fn t1104c_health_poll_sustained_failures_trigger_reconnect() {
    let (transport, server) = create_transport_and_server(6).await;
    let transport = Arc::new(transport);

    let cancel = tokio_util::sync::CancellationToken::new();
    let weak = Arc::downgrade(&transport);

    // Drop the server so every poll_status call will fail (write succeeds
    // to the duplex but the STATUS response never arrives → timeout).
    drop(server);

    let handle = sonde_gateway::modem::spawn_health_monitor(
        weak,
        Duration::from_millis(10),
        cancel.clone(),
        DEFAULT_MAX_HEALTH_POLL_FAILURES,
    );

    let reconnect_needed = tokio::time::timeout(Duration::from_secs(30), handle)
        .await
        .expect("health monitor did not exit in time")
        .expect("health monitor task panicked");

    assert!(
        reconnect_needed,
        "health monitor must signal reconnect after sustained failures"
    );
}

// ── T-1104d: Warm reboot — unexpected MODEM_READY fires notify and flag ──

/// T-1104d  Unexpected MODEM_READY fires warm_reboot_notify and sets warm_reboot_flag.
///
/// After startup, an unsolicited MODEM_READY (modem warm reboot) must:
///   - set the warm_reboot_flag to true (GW-1103 AC7)
///   - fire warm_reboot_notify (GW-1103 AC7)
///   - cancel any pending waiters (tested via channel_ack_slot via change_channel)
#[tokio::test]
async fn t1104d_unexpected_modem_ready_fires_warm_reboot_notify() {
    let (transport, mut server) = create_transport_and_server(6).await;

    let warm_reboot_notify = transport.warm_reboot_notify();
    let warm_reboot_flag = transport.warm_reboot_flag();

    // Register a pending change_channel waiter. This exercises the
    // channel_ack_slot cancellation path inside dispatch_message.
    let transport_arc = Arc::new(transport);
    let ch_task = {
        let t = Arc::clone(&transport_arc);
        tokio::spawn(async move { t.change_channel(7).await })
    };

    // Consume the SET_CHANNEL command sent by change_channel.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
    assert!(
        matches!(msg, ModemMessage::SetChannel(7)),
        "expected SetChannel(7) from change_channel"
    );

    // Now inject an unsolicited MODEM_READY (simulates modem warm reboot).
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 0, 0, 0],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    server
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    // warm_reboot_notify must fire.
    tokio::time::timeout(Duration::from_secs(1), warm_reboot_notify.notified())
        .await
        .expect("warm_reboot_notify must fire on unexpected MODEM_READY");

    assert!(
        warm_reboot_flag.load(std::sync::atomic::Ordering::Acquire),
        "warm_reboot_flag must be set after unexpected MODEM_READY"
    );

    // The pending change_channel waiter must have been cancelled (channel
    // closed → Err returned).
    let ch_result = tokio::time::timeout(Duration::from_secs(1), ch_task)
        .await
        .expect("change_channel task did not complete in time")
        .expect("change_channel task panicked");
    assert!(
        ch_result.is_err(),
        "change_channel must return Err when cancelled by warm reboot"
    );
}

// ── T-1104e: Warm reboot — gateway re-runs startup with persisted channel ──

/// T-1104e  After modem warm reboot, gateway recovers with persisted channel.
///
/// Validates GW-1103 (criteria 7–8) and GW-0808 (AC6) at the transport level:
///   - warm reboot detection: flag set, notify fires (transport precondition)
///   - after abort_reader_and_wait() + drop, a new transport created with the
///     persisted channel (7) sends RESET → SET_CHANNEL(7), not SET_CHANNEL(1)
///
/// Note: The no-backoff and backoff-reset requirements (GW-1103 AC8–AC9) live
/// in `run_gateway`'s reconnect loop and cannot be asserted at this transport
/// level. They require a gateway-level harness to exercise `run_gateway` with
/// a mock serial port, which is tracked as a future test improvement.
#[tokio::test]
async fn t1104e_warm_reboot_recovery_uses_persisted_channel() {
    use std::sync::atomic::Ordering;

    // 1. Create initial transport (channel 1) and simulate steady-state operation.
    let (client1, mut server1) = duplex(4096);
    let transport_handle =
        tokio::spawn(async move { UsbEspNowTransport::new(client1, 1).await.unwrap() });
    {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 256];
        // RESET
        let msg = read_next_message(&mut server1, &mut decoder, &mut buf).await;
        assert!(matches!(msg, ModemMessage::Reset));
        // MODEM_READY
        let ready = ModemMessage::ModemReady(ModemReady {
            firmware_version: [1, 0, 0, 0],
            mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
        });
        server1
            .write_all(&encode_modem_frame(&ready).unwrap())
            .await
            .unwrap();
        // SET_CHANNEL(1)
        let msg = read_next_message(&mut server1, &mut decoder, &mut buf).await;
        assert!(
            matches!(msg, ModemMessage::SetChannel(1)),
            "expected SetChannel(1) during startup"
        );
        // SET_CHANNEL_ACK(1)
        server1
            .write_all(&encode_modem_frame(&ModemMessage::SetChannelAck(1)).unwrap())
            .await
            .unwrap();
    }
    let transport = Arc::new(transport_handle.await.unwrap());

    let warm_reboot_notify = transport.warm_reboot_notify();
    let warm_reboot_flag = transport.warm_reboot_flag();

    // 2. Simulate modem warm reboot — inject unsolicited MODEM_READY.
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 0, 0, 0],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    server1
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    // 3. Assert warm_reboot_notify fires and flag is latched.
    tokio::time::timeout(Duration::from_secs(1), warm_reboot_notify.notified())
        .await
        .expect("warm_reboot_notify must fire on unexpected MODEM_READY");
    assert!(
        warm_reboot_flag.load(Ordering::Acquire),
        "warm_reboot_flag must be set after unexpected MODEM_READY"
    );

    // 4. Recovery: abort reader task and drop the transport (mimics gateway recovery path).
    transport.abort_reader_and_wait().await;
    drop(transport);

    // 5. Simulate the reconnect loop reading the persisted channel from DB (channel 7).
    let persisted_channel: u8 = 7;

    // 6. Create new transport with persisted channel — the mock stream must receive
    //    RESET → SET_CHANNEL(7), not SET_CHANNEL(1).
    let (client2, mut server2) = duplex(4096);
    let transport2_handle = tokio::spawn(async move {
        UsbEspNowTransport::new(client2, persisted_channel)
            .await
            .unwrap()
    });

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // RESET
    let msg = read_next_message(&mut server2, &mut decoder, &mut buf).await;
    assert!(
        matches!(msg, ModemMessage::Reset),
        "expected Reset after warm reboot recovery"
    );

    // MODEM_READY
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 0, 0, 0],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    server2
        .write_all(&encode_modem_frame(&ready).unwrap())
        .await
        .unwrap();

    // SET_CHANNEL(7) — must be persisted channel, not channel 1
    let msg = read_next_message(&mut server2, &mut decoder, &mut buf).await;
    match msg {
        ModemMessage::SetChannel(ch) => {
            assert_eq!(
                ch, 7,
                "gateway must use persisted channel 7 after warm reboot, not default 1"
            );
            // Complete the handshake so transport2 is fully initialized.
            server2
                .write_all(&encode_modem_frame(&ModemMessage::SetChannelAck(ch)).unwrap())
                .await
                .unwrap();
        }
        other => panic!("expected SetChannel after warm reboot recovery, got {other:?}"),
    }

    let _transport2 = transport2_handle.await.unwrap();
}
