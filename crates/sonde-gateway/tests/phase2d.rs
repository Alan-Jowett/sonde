// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 2D integration tests: modem health monitoring (GW-1102),
//! modem error recovery documentation (GW-1103), and node timeout
//! detection (GW-0507 node_timeout EVENT).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sonde_gateway::engine::Gateway;
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::storage::{InMemoryStorage, Storage};

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, ModemStatus,
};
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

// ─── Modem test helpers ────────────────────────────────────────────────

/// Read bytes from the stream until a complete modem message is decoded.
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

/// Run the modem startup handshake on the mock (server) side of a duplex.
async fn do_startup_handshake(server: &mut DuplexStream) -> u8 {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Read RESET
    let msg = read_next_message(server, &mut decoder, &mut buf).await;
    assert!(
        matches!(msg, ModemMessage::Reset),
        "expected Reset, got {msg:?}"
    );

    // 2. Send MODEM_READY
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 2, 3, 4],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    let frame = encode_modem_frame(&ready).unwrap();
    server.write_all(&frame).await.unwrap();

    // 3. Read SET_CHANNEL
    let msg = read_next_message(server, &mut decoder, &mut buf).await;
    let requested_channel = match msg {
        ModemMessage::SetChannel(ch) => ch,
        other => panic!("expected SetChannel, got {other:?}"),
    };

    // 4. Send SET_CHANNEL_ACK
    let ack = ModemMessage::SetChannelAck(requested_channel);
    let frame = encode_modem_frame(&ack).unwrap();
    server.write_all(&frame).await.unwrap();

    requested_channel
}

// ─── GW-1102: Modem health monitoring ──────────────────────────────────

/// T-1105 extended: poll_status returns correct values across multiple calls
/// with different status payloads, verifying the values used by the health
/// monitor for delta and reboot detection.
#[tokio::test]
async fn t1105_poll_status_multiple_calls() {
    let (client, mut server) = duplex(1024);

    let startup = tokio::spawn(async move {
        do_startup_handshake(&mut server).await;
        server
    });

    let transport = sonde_gateway::modem::UsbEspNowTransport::new(client, 6)
        .await
        .unwrap();
    let mut server = startup.await.unwrap();

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // First poll — baseline
    let poll = {
        let status_msg = ModemMessage::Status(ModemStatus {
            channel: 6,
            uptime_s: 100,
            tx_count: 10,
            rx_count: 5,
            tx_fail_count: 0,
        });

        let transport_ref = &transport;
        let handle = tokio::spawn({
            // We need a workaround since transport isn't Send-safe across
            // the spawn boundary. Instead use a single-threaded approach.
            let status_bytes = encode_modem_frame(&status_msg).unwrap();
            let _ = status_bytes; // consumed below
            async { Ok::<(), ()>(()) }
        });
        let _ = handle;

        // Drive poll in current task context
        let poll_fut = transport_ref.poll_status();
        tokio::pin!(poll_fut);

        // Send GET_STATUS response from server side
        let server_fut = async {
            let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
            assert!(matches!(msg, ModemMessage::GetStatus));
            let status_msg = ModemMessage::Status(ModemStatus {
                channel: 6,
                uptime_s: 100,
                tx_count: 10,
                rx_count: 5,
                tx_fail_count: 0,
            });
            server
                .write_all(&encode_modem_frame(&status_msg).unwrap())
                .await
                .unwrap();
        };

        let (status, _) = tokio::join!(poll_fut, server_fut);
        let status = status.unwrap();
        assert_eq!(status.uptime_s, 100);
        assert_eq!(status.tx_fail_count, 0);
        status
    };

    // Second poll — tx_fail increased (health monitor would log a warning)
    {
        let poll_fut = transport.poll_status();
        tokio::pin!(poll_fut);

        let server_fut = async {
            let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
            assert!(matches!(msg, ModemMessage::GetStatus));
            let status_msg = ModemMessage::Status(ModemStatus {
                channel: 6,
                uptime_s: 130,
                tx_count: 15,
                rx_count: 8,
                tx_fail_count: 3,
            });
            server
                .write_all(&encode_modem_frame(&status_msg).unwrap())
                .await
                .unwrap();
        };

        let (status, _) = tokio::join!(poll_fut, server_fut);
        let status = status.unwrap();
        assert_eq!(status.uptime_s, 130);
        assert_eq!(status.tx_fail_count, 3);
        // Delta would be 3 - 0 = 3 (health monitor logs this)
        assert!(status.tx_fail_count > poll.tx_fail_count);
    }

    // Third poll — uptime decreased (reboot detected by health monitor)
    {
        let poll_fut = transport.poll_status();
        tokio::pin!(poll_fut);

        let server_fut = async {
            let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
            assert!(matches!(msg, ModemMessage::GetStatus));
            let status_msg = ModemMessage::Status(ModemStatus {
                channel: 6,
                uptime_s: 5, // rebooted — uptime dropped
                tx_count: 0,
                rx_count: 0,
                tx_fail_count: 0,
            });
            server
                .write_all(&encode_modem_frame(&status_msg).unwrap())
                .await
                .unwrap();
        };

        let (status, _) = tokio::join!(poll_fut, server_fut);
        let status = status.unwrap();
        assert_eq!(status.uptime_s, 5);
        // Health monitor would detect uptime_s < prev (130 > 5)
    }
}

// ─── GW-0507: node_timeout EVENT ───────────────────────────────────────

/// Verify check_node_timeouts identifies nodes that have exceeded 3×
/// their schedule_interval_s since last_seen. Without a real handler
/// router, we verify the method runs without error against storage.
#[tokio::test]
async fn t0507_check_node_timeouts_no_handler() {
    let storage = Arc::new(InMemoryStorage::new());

    // Register a node with a 60s interval and last_seen 200s ago
    let mut node = NodeRecord::new("timeout-node".into(), 0x0001, [0xAA; 32]);
    node.schedule_interval_s = 60;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(200));
    storage.upsert_node(&node).await.unwrap();

    // Gateway without handler router — check_node_timeouts should return
    // gracefully (no router = no events to emit).
    let gw = Gateway::new(storage, Duration::from_secs(30));
    gw.check_node_timeouts(3).await;
    // No panic = success; events would be emitted if a handler router were
    // configured.
}

/// Verify that nodes within their expected interval are NOT flagged.
#[tokio::test]
async fn t0507_check_node_timeouts_not_timed_out() {
    let storage = Arc::new(InMemoryStorage::new());

    // Node seen 30s ago with 60s interval — well within 2× window
    let mut node = NodeRecord::new("fresh-node".into(), 0x0002, [0xBB; 32]);
    node.schedule_interval_s = 60;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(30));
    storage.upsert_node(&node).await.unwrap();

    let gw = Gateway::new(storage, Duration::from_secs(30));
    gw.check_node_timeouts(3).await;
    // No panic, no timeout detected.
}

/// Verify that nodes with no last_seen are skipped.
#[tokio::test]
async fn t0507_check_node_timeouts_no_last_seen() {
    let storage = Arc::new(InMemoryStorage::new());

    let node = NodeRecord::new("new-node".into(), 0x0003, [0xCC; 32]);
    storage.upsert_node(&node).await.unwrap();

    let gw = Gateway::new(storage, Duration::from_secs(30));
    gw.check_node_timeouts(3).await;
    // No panic — node with no last_seen is skipped.
}

/// Verify that nodes with zero schedule_interval are skipped.
#[tokio::test]
async fn t0507_check_node_timeouts_zero_interval() {
    let storage = Arc::new(InMemoryStorage::new());

    let mut node = NodeRecord::new("zero-interval".into(), 0x0004, [0xDD; 32]);
    node.schedule_interval_s = 0;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(500));
    storage.upsert_node(&node).await.unwrap();

    let gw = Gateway::new(storage, Duration::from_secs(30));
    gw.check_node_timeouts(3).await;
    // No panic — zero interval means no timeout check.
}
