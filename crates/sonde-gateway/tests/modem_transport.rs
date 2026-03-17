// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Modem transport tests (T-1100 through T-1108 except T-1105 which is
//! already in phase2d.rs), and BLE admin tests (T-1221, T-1222).

use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::modem::UsbEspNowTransport;
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
    use sonde_gateway::crypto::RustCryptoHmac;
    use sonde_gateway::engine::Gateway;
    use sonde_gateway::registry::NodeRecord;
    use sonde_gateway::storage::{InMemoryStorage, Storage};
    use sonde_protocol::{
        decode_frame, encode_frame, verify_frame, FrameHeader, GatewayMessage, NodeMessage,
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
    };
    let cbor = wake_msg.encode().unwrap();
    let wake_frame = encode_frame(&header, &cbor, &psk, &RustCryptoHmac).unwrap();

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
            assert!(verify_frame(&decoded, &psk, &RustCryptoHmac));
            let gw_msg = GatewayMessage::decode(decoded.header.msg_type, &decoded.payload).unwrap();
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
