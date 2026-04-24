// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Companion transient display tests (T-0826).

mod common;

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::RwLock;
use tonic::{Code, Request};

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::ShowModemDisplayMessageRequest;
use sonde_gateway::admin::AdminService;
use sonde_gateway::ble_pairing::{BlePairingController, PairingOrigin};
use sonde_gateway::companion::pb::gateway_companion_server::GatewayCompanion;
use sonde_gateway::companion::pb::*;
use sonde_gateway::companion::{CompanionEventHub, CompanionService};
use sonde_gateway::display_banner::{render_display_message, render_gateway_version_banner};
use sonde_gateway::display_control::{StatusPageCycle, StatusPageScrollTask};
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transient_display::{ActiveDisplayState, DisplayStateHandle};

use sonde_protocol::modem::{
    encode_modem_frame, DisplayFrameAck, FrameDecoder, ModemMessage, DISPLAY_FRAME_BODY_SIZE,
    DISPLAY_FRAME_CHUNK_COUNT, DISPLAY_FRAME_CHUNK_SIZE,
};

use common::{create_transport_and_server, read_modem_msg};

async fn receive_display_transfer(
    server: &mut DuplexStream,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
    let mut framebuffer = [0u8; DISPLAY_FRAME_BODY_SIZE];

    let begin = read_modem_msg(server, decoder, buf).await;
    let transfer_id = match begin {
        ModemMessage::DisplayFrameBegin(begin) => begin.transfer_id,
        other => panic!("expected DisplayFrameBegin, got {other:?}"),
    };
    server
        .write_all(
            &encode_modem_frame(&ModemMessage::DisplayFrameAck(DisplayFrameAck {
                transfer_id,
                next_chunk_index: 0,
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    for expected_chunk_index in 0..DISPLAY_FRAME_CHUNK_COUNT {
        let msg = read_modem_msg(server, decoder, buf).await;
        match msg {
            ModemMessage::DisplayFrameChunk(chunk) => {
                assert_eq!(chunk.transfer_id, transfer_id);
                assert_eq!(chunk.chunk_index, expected_chunk_index);
                let start = usize::from(expected_chunk_index) * DISPLAY_FRAME_CHUNK_SIZE;
                let end = start + DISPLAY_FRAME_CHUNK_SIZE;
                framebuffer[start..end].copy_from_slice(&chunk.chunk_data);
            }
            other => panic!("expected DisplayFrameChunk, got {other:?}"),
        }
        server
            .write_all(
                &encode_modem_frame(&ModemMessage::DisplayFrameAck(DisplayFrameAck {
                    transfer_id,
                    next_chunk_index: expected_chunk_index + 1,
                }))
                .unwrap(),
            )
            .await
            .unwrap();
    }

    framebuffer
}

async fn assert_no_stream_data_while_time_paused(
    server: &mut DuplexStream,
    buf: &mut [u8],
    duration: Duration,
    message: &str,
) {
    let no_data = tokio::time::timeout(duration, server.read(buf));
    tokio::pin!(no_data);
    tokio::time::advance(duration).await;
    assert!(no_data.await.is_err(), "{message}");
}

async fn build_services_with_modem(
    channel: u8,
) -> (
    AdminService,
    CompanionService,
    DuplexStream,
    Arc<BlePairingController>,
    Arc<dyn Storage>,
) {
    let (transport, server) = create_transport_and_server(channel).await;
    let transport = Arc::new(transport);
    let controller = Arc::new(BlePairingController::new());
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let display_generation = Arc::new(AtomicU64::new(0));
    let status_page_cycle = Arc::new(tokio::sync::Mutex::new(StatusPageCycle::default()));
    let status_page_scroll_task: StatusPageScrollTask = Arc::new(tokio::sync::Mutex::new(None));

    let display_handle = DisplayStateHandle::new();
    display_handle
        .set(ActiveDisplayState::new(
            Arc::clone(&transport),
            Arc::clone(&controller),
            Arc::clone(&display_generation),
            Arc::clone(&status_page_cycle),
            Arc::clone(&status_page_scroll_task),
        ))
        .await;

    let admin = AdminService::new(storage.clone(), pending.clone(), session_manager.clone())
        .with_ble(Arc::clone(&controller), Arc::clone(&transport))
        .with_display_state(
            display_generation,
            status_page_cycle,
            status_page_scroll_task,
        );
    let companion = CompanionService::new(
        storage.clone(),
        pending,
        session_manager,
        Arc::new(CompanionEventHub::default()),
        display_handle,
    );

    (admin, companion, server, controller, storage)
}

#[tokio::test]
async fn t0826_companion_transient_display_reuses_gateway_display_semantics() {
    tokio::time::pause();

    let (admin, companion, mut server, _controller, _storage) = build_services_with_modem(6).await;
    let admin = Arc::new(admin);
    let companion = Arc::new(companion);
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 2048];

    let first = tokio::spawn({
        let companion = Arc::clone(&companion);
        async move {
            companion
                .show_modem_display_message(Request::new(CompanionShowModemDisplayMessageRequest {
                    lines: vec!["Azure login".to_string(), "ABCD-EFGH".to_string()],
                }))
                .await
        }
    });
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(
        framebuffer,
        render_display_message(&["Azure login", "ABCD-EFGH"])
    );
    first.await.unwrap().unwrap();

    tokio::time::advance(Duration::from_secs(30)).await;

    let second = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
                    lines: vec!["Admin override".to_string()],
                }))
                .await
        }
    });
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(framebuffer, render_display_message(&["Admin override"]));
    second.await.unwrap().unwrap();

    assert_no_stream_data_while_time_paused(
        &mut server,
        &mut buf,
        Duration::from_secs(30) + Duration::from_millis(100),
        "the earlier companion timer must not restore the banner after admin replacement",
    )
    .await;

    tokio::time::advance(Duration::from_secs(30) + Duration::from_millis(100)).await;
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(
        framebuffer,
        render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test]
async fn companion_display_rejected_during_ble_pairing() {
    let (_admin, companion, mut server, controller, _storage) = build_services_with_modem(6).await;
    assert!(controller.open_window(120, PairingOrigin::Admin).await);

    let err = companion
        .show_modem_display_message(Request::new(CompanionShowModemDisplayMessageRequest {
            lines: vec!["Blocked".to_string()],
        }))
        .await
        .expect_err("display request must fail during active BLE pairing");
    assert_eq!(err.code(), Code::FailedPrecondition);

    let mut buf = [0u8; 256];
    assert!(
        tokio::time::timeout(Duration::from_millis(200), server.read(&mut buf))
            .await
            .is_err(),
        "rejected display request must not emit modem traffic"
    );
}

#[tokio::test]
async fn companion_display_rejects_invalid_line_count() {
    let (_admin, companion, mut server, _controller, _storage) = build_services_with_modem(6).await;

    let err = companion
        .show_modem_display_message(Request::new(CompanionShowModemDisplayMessageRequest {
            lines: vec![],
        }))
        .await
        .expect_err("empty line set must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);

    let err = companion
        .show_modem_display_message(Request::new(CompanionShowModemDisplayMessageRequest {
            lines: vec![
                "1".to_string(),
                "2".to_string(),
                "3".to_string(),
                "4".to_string(),
                "5".to_string(),
            ],
        }))
        .await
        .expect_err("more than four lines must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);

    let mut buf = [0u8; 256];
    assert!(
        tokio::time::timeout(Duration::from_millis(200), server.read(&mut buf))
            .await
            .is_err(),
        "invalid requests must not emit modem traffic"
    );
}

#[tokio::test]
async fn companion_display_without_transport_is_unavailable() {
    let storage: Arc<dyn Storage> = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let companion = CompanionService::new(
        storage,
        pending,
        session_manager,
        Arc::new(CompanionEventHub::default()),
        DisplayStateHandle::new(),
    );

    let err = companion
        .show_modem_display_message(Request::new(CompanionShowModemDisplayMessageRequest {
            lines: vec!["Unavailable".to_string()],
        }))
        .await
        .expect_err("missing modem transport must produce UNAVAILABLE");
    assert_eq!(err.code(), Code::Unavailable);
}
