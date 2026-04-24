// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for sonde-admin gRPC client and BLE pairing operations.
//!
//! These tests spin up a real `AdminService` (backed by `InMemoryStorage`)
//! on a platform-native transport (UDS on Unix, named pipe on Windows)
//! and exercise the `AdminClient` wrapper.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use sonde_admin::grpc_client::AdminClient;
use sonde_gateway::admin::AdminService;
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::InMemoryStorage;

// ── Test harness ────────────────────────────────────────────────────────────

/// Generate a unique pipe/socket name for each test to avoid collisions.
fn unique_endpoint(test_name: &str) -> String {
    let pid = std::process::id();
    if cfg!(windows) {
        format!(r"\\.\pipe\sonde-admin-test-{test_name}-{pid}")
    } else {
        // Use /tmp directly with a unique filename. No tempdir needed since
        // serve_admin handles cleanup of stale socket files.
        format!("/tmp/sonde-admin-test-{test_name}-{pid}.sock")
    }
}

/// Start an admin gRPC server and return a connected `AdminClient`.
async fn start_server_and_connect(test_name: &str) -> AdminClient {
    let storage = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage, pending, session_manager);

    let endpoint = unique_endpoint(test_name);
    let server_endpoint = endpoint.clone();

    // Start server in background.
    tokio::spawn(async move {
        if let Err(e) = sonde_gateway::admin::serve_admin(admin, &server_endpoint).await {
            eprintln!("admin server ended: {e}");
        }
    });

    // Retry connecting until the server is ready.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match AdminClient::connect(&endpoint).await {
            Ok(client) => return client,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("failed to connect to admin server: {e}"),
        }
    }
}

// ── gRPC client tests ───────────────────────────────────────────────────────

/// Test: list nodes on empty gateway returns empty list.
#[tokio::test]
async fn grpc_list_nodes_empty() {
    let mut client = start_server_and_connect("list_nodes_empty").await;
    let nodes = client.list_nodes().await.unwrap();
    assert!(nodes.is_empty(), "empty gateway should have no nodes");
}

/// Test: register a node, then list and get it back.
#[tokio::test]
async fn grpc_register_list_get_node() {
    let mut client = start_server_and_connect("register_list_get").await;

    let node_id = client
        .register_node("test-node", 0x1234, vec![0xAA; 32])
        .await
        .unwrap();
    assert_eq!(node_id, "test-node");

    let nodes = client.list_nodes().await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "test-node");
    assert_eq!(nodes[0].key_hint, 0x1234);

    let node = client.get_node("test-node").await.unwrap();
    assert_eq!(node.node_id, "test-node");
}

/// Test: register then remove a node.
#[tokio::test]
async fn grpc_register_remove_node() {
    let mut client = start_server_and_connect("register_remove").await;

    client
        .register_node("ephemeral-node", 0x5678, vec![0xBB; 32])
        .await
        .unwrap();
    assert_eq!(client.list_nodes().await.unwrap().len(), 1);

    client.remove_node("ephemeral-node").await.unwrap();
    assert_eq!(client.list_nodes().await.unwrap().len(), 0);
}

/// Test: ingest a program and list it.
///
/// Skipped in release builds — the gateway rejects raw CBOR program images
/// in release mode (only ELF binaries verified by Prevail are accepted).
#[cfg(debug_assertions)]
#[tokio::test]
async fn grpc_ingest_list_program() {
    let mut client = start_server_and_connect("ingest_list_program").await;

    // Build a minimal CBOR program image: {1: <bytecode>, 2: []}.
    // Key 1 = bytecode, Key 2 = maps (empty array).
    let bytecode: Vec<u8> = vec![
        0xB7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
    ];
    // Deterministic CBOR: map(2) { 1: bytes(bytecode), 2: array(0) }
    let mut cbor = Vec::new();
    cbor.push(0xA2); // map(2)
    cbor.push(0x01); // key: 1
    cbor.push(0x50); // bytes(16)
    cbor.extend_from_slice(&bytecode);
    cbor.push(0x02); // key: 2
    cbor.push(0x80); // array(0)

    // Profile 1 = Resident (sonde_admin::pb::VerificationProfile::Resident).
    let (hash, size) = client
        .ingest_program(cbor.clone(), 1, None, None)
        .await
        .unwrap();
    assert!(!hash.is_empty(), "program hash must not be empty");
    assert!(size > 0, "program size must be non-zero");

    let programs = client.list_programs().await.unwrap();
    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0].hash, hash);
}

/// Test: set schedule on a node.
#[tokio::test]
async fn grpc_set_schedule() {
    let mut client = start_server_and_connect("set_schedule").await;

    client
        .register_node("sched-node", 0x0001, vec![0xCC; 32])
        .await
        .unwrap();

    // set_schedule should succeed without error.
    client.set_schedule("sched-node", 120).await.unwrap();
}

/// Test: queue reboot on a node.
#[tokio::test]
async fn grpc_queue_reboot() {
    let mut client = start_server_and_connect("queue_reboot").await;

    client
        .register_node("reboot-node", 0x0002, vec![0xDD; 32])
        .await
        .unwrap();

    // queue_reboot should succeed without error.
    client.queue_reboot("reboot-node").await.unwrap();
}

// ── BLE pairing tests ───────────────────────────────────────────────────────

/// Test: list phones on empty gateway returns empty list.
#[tokio::test]
async fn grpc_list_phones_empty() {
    let mut client = start_server_and_connect("list_phones_empty").await;
    let phones = client.list_phones().await.unwrap();
    assert!(phones.is_empty(), "empty gateway should have no phones");
}

/// Test: close BLE pairing when not open (may error — not an operational concern).
#[tokio::test]
async fn grpc_close_ble_pairing_when_not_open() {
    let mut client = start_server_and_connect("close_ble_noop").await;
    // Closing when not open may succeed or return an error; either is acceptable.
    // The important thing is it doesn't panic.
    let _ = client.close_ble_pairing().await;
}

/// Test: revoke a non-existent phone returns an error.
#[tokio::test]
async fn grpc_revoke_nonexistent_phone() {
    let mut client = start_server_and_connect("revoke_nonexistent").await;
    let result = client.revoke_phone(999).await;
    assert!(result.is_err(), "revoking non-existent phone should fail");
}

/// Test: transient modem display fails cleanly when no modem transport exists.
#[tokio::test]
async fn grpc_show_modem_display_message_no_modem() {
    let mut client = start_server_and_connect("show_modem_display_message_no_modem").await;
    let result = client
        .show_modem_display_message(vec!["Device login".to_string()])
        .await;
    let status = result.expect_err("missing modem transport should fail");
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

// ── State export/import ─────────────────────────────────────────────────────

/// Test: export empty state and import it back.
#[tokio::test]
async fn grpc_export_import_state() {
    let mut client = start_server_and_connect("export_import").await;

    // Register a node so there's data to export.
    client
        .register_node("export-node", 0x9999, vec![0xEE; 32])
        .await
        .unwrap();

    let exported = client.export_state("test-passphrase").await.unwrap();
    assert!(!exported.is_empty(), "exported state must not be empty");

    // Import back into the same gateway (idempotent for nodes).
    client
        .import_state(exported, "test-passphrase")
        .await
        .unwrap();

    let nodes = client.list_nodes().await.unwrap();
    assert!(
        nodes.iter().any(|n| n.node_id == "export-node"),
        "imported state must contain the original node"
    );
}
