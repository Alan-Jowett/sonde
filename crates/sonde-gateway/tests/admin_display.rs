// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Admin transient display tests (T-0815f through T-0815j).

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::RwLock;
use tonic::{Code, Request};

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use sonde_gateway::admin::AdminService;
use sonde_gateway::ble_pairing::PairingOrigin;
use sonde_gateway::display_banner::{render_display_message, render_gateway_version_banner};
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::InMemoryStorage;

use sonde_protocol::modem::{
    encode_modem_frame, DisplayFrameAck, FrameDecoder, ModemMessage, DISPLAY_FRAME_BODY_SIZE,
    DISPLAY_FRAME_CHUNK_COUNT, DISPLAY_FRAME_CHUNK_SIZE,
};

use common::{build_admin_with_modem, read_modem_msg};

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

async fn receive_display_begin_transfer_id(
    server: &mut DuplexStream,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> u8 {
    match read_modem_msg(server, decoder, buf).await {
        ModemMessage::DisplayFrameBegin(begin) => begin.transfer_id,
        other => panic!("expected DisplayFrameBegin, got {other:?}"),
    }
}

#[tokio::test]
async fn t0815f_transient_modem_display_via_admin_api() {
    tokio::time::pause();

    let (admin, mut server, _controller, _storage) = build_admin_with_modem(6).await;
    let admin = Arc::new(admin);
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 2048];

    let single_line = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
                    lines: vec!["Device login".to_string()],
                }))
                .await
        }
    });
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(framebuffer, render_display_message(&["Device login"]));
    single_line.await.unwrap().unwrap();

    let four_line = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
                    lines: vec![
                        "Device login".to_string(),
                        "Use browser".to_string(),
                        "Code".to_string(),
                        "ABCD-EFGH".to_string(),
                    ],
                }))
                .await
        }
    });
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(
        framebuffer,
        render_display_message(&["Device login", "Use browser", "Code", "ABCD-EFGH"])
    );
    four_line.await.unwrap().unwrap();

    tokio::time::advance(Duration::from_secs(60) + Duration::from_millis(100)).await;
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(
        framebuffer,
        render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test]
async fn t0815g_new_transient_display_request_replaces_older_one() {
    let (admin, mut server, _controller, _storage) = build_admin_with_modem(6).await;
    let admin = Arc::new(admin);
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 2048];

    tokio::time::pause();

    let first = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
                    lines: vec!["First".to_string()],
                }))
                .await
        }
    });
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(framebuffer, render_display_message(&["First"]));
    first.await.unwrap().unwrap();

    tokio::time::advance(Duration::from_secs(30)).await;

    let second = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
                    lines: vec!["Second".to_string()],
                }))
                .await
        }
    });
    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(framebuffer, render_display_message(&["Second"]));
    second.await.unwrap().unwrap();

    assert_no_stream_data_while_time_paused(
        &mut server,
        &mut buf,
        Duration::from_secs(30) + Duration::from_millis(100),
        "first timer must not restore the banner after replacement",
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
async fn t0815h_transient_display_rejected_during_ble_pairing() {
    let (admin, mut server, controller, _storage) = build_admin_with_modem(6).await;
    assert!(controller.open_window(120, PairingOrigin::Admin).await);

    let err = admin
        .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
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
async fn t0815i_transient_display_rejects_invalid_line_count() {
    let (admin, mut server, _controller, _storage) = build_admin_with_modem(6).await;

    let err = admin
        .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
            lines: vec![],
        }))
        .await
        .expect_err("empty line set must be rejected");
    assert_eq!(err.code(), Code::InvalidArgument);

    let err = admin
        .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
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
async fn t0815j_transient_display_without_modem_transport_is_unavailable() {
    let storage = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage, pending, session_manager);

    let err = admin
        .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
            lines: vec!["Unavailable".to_string()],
        }))
        .await
        .expect_err("missing modem transport must produce UNAVAILABLE");
    assert_eq!(err.code(), Code::Unavailable);
}

#[tokio::test]
async fn admin_display_failure_restores_gateway_banner() {
    let (admin, mut server, _controller, _storage) = build_admin_with_modem(6).await;
    let admin = Arc::new(admin);
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 2048];

    let request = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .show_modem_display_message(Request::new(ShowModemDisplayMessageRequest {
                    lines: vec!["Device login".to_string()],
                }))
                .await
        }
    });

    let transfer_id = receive_display_begin_transfer_id(&mut server, &mut decoder, &mut buf).await;
    server
        .write_all(
            &encode_modem_frame(&ModemMessage::DisplayFrameAck(DisplayFrameAck {
                transfer_id: transfer_id.wrapping_add(1),
                next_chunk_index: 0,
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    let framebuffer = receive_display_transfer(&mut server, &mut decoder, &mut buf).await;
    assert_eq!(
        framebuffer,
        render_gateway_version_banner(env!("CARGO_PKG_VERSION"))
    );

    let err = request
        .await
        .unwrap()
        .expect_err("mismatched ACK must fail the admin display request");
    assert_eq!(err.code(), Code::Internal);
}
