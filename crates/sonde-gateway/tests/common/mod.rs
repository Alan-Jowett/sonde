// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Shared mock-modem helpers for gateway integration tests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::RwLock;

use sonde_gateway::admin::AdminService;
use sonde_gateway::ble_pairing::BlePairingController;
use sonde_gateway::display_control::{StatusPageCycle, StatusPageScrollTask};
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::modem::UsbEspNowTransport;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};

use sonde_protocol::modem::{encode_modem_frame, FrameDecoder, ModemMessage, ModemReady};

async fn read_with_wall_clock_timeout(
    stream: &mut DuplexStream,
    buf: &mut [u8],
    timeout: Duration,
) -> usize {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_bg = Arc::clone(&cancelled);
    tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + timeout;
        while !cancelled_bg.load(Ordering::SeqCst) {
            let now = std::time::Instant::now();
            if now >= deadline {
                let _ = tx.send(());
                return;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    });

    tokio::select! {
        read = stream.read(buf) => {
            cancelled.store(true, Ordering::SeqCst);
            read.expect("read failed")
        }
        _ = async {
            let _ = rx.await;
        } => panic!("timed out waiting for modem message"),
    }
}

/// Read the next decoded modem message from a mock server stream.
pub async fn read_modem_msg(
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
        // Use a wall-clock deadline so paused Tokio time does not turn a
        // missing modem frame into a hung test.
        let n = read_with_wall_clock_timeout(stream, buf, Duration::from_secs(10)).await;
        assert!(n > 0, "stream closed unexpectedly");
        decoder.push(&buf[..n]);
    }
}

/// Perform the full modem startup handshake on the mock server side.
pub async fn modem_startup(server: &mut DuplexStream, channel: u8) {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Read RESET
    let msg = read_modem_msg(server, &mut decoder, &mut buf).await;
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

    // 3. Read SET_CHANNEL
    let msg = read_modem_msg(server, &mut decoder, &mut buf).await;
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

/// Create a transport + mock modem server with completed startup handshake.
#[allow(dead_code)]
pub async fn create_transport_and_server(channel: u8) -> (UsbEspNowTransport, DuplexStream) {
    let (client, mut server) = duplex(4096);

    let transport_handle =
        tokio::spawn(async move { UsbEspNowTransport::new(client, channel).await.unwrap() });

    modem_startup(&mut server, channel).await;

    let transport = transport_handle.await.unwrap();
    (transport, server)
}

/// Build an `AdminService` wired to a mock modem transport.
#[allow(dead_code)]
pub async fn build_admin_with_modem(
    channel: u8,
) -> (
    AdminService,
    DuplexStream,
    Arc<BlePairingController>,
    Arc<dyn Storage>,
) {
    let (client, mut server) = duplex(4096);
    let transport_handle =
        tokio::spawn(async move { UsbEspNowTransport::new(client, channel).await.unwrap() });
    modem_startup(&mut server, channel).await;
    let transport = Arc::new(transport_handle.await.unwrap());

    let controller = Arc::new(BlePairingController::new());
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let display_generation = Arc::new(AtomicU64::new(0));
    let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
    let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));

    let admin = AdminService::new(storage.clone(), pending, session_manager)
        .with_ble(controller.clone(), transport)
        .with_display_state(
            display_generation,
            status_page_cycle,
            status_page_scroll_task,
        );

    (admin, server, controller, storage)
}
