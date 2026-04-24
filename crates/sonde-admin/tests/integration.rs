// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Integration tests for sonde-admin gRPC client and BLE pairing operations.
//!
//! These tests spin up a real `AdminService` (backed by `InMemoryStorage`)
//! on a platform-native transport (UDS on Unix, named pipe on Windows)
//! and exercise the `AdminClient` wrapper.

use std::collections::HashMap;
use std::process::Command;
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

/// Start an admin gRPC server and return its endpoint once it is accepting connections.
async fn start_server(test_name: &str) -> String {
    let storage = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage, pending, session_manager);

    let endpoint = unique_endpoint(test_name);
    let server_endpoint = endpoint.clone();

    tokio::spawn(async move {
        if let Err(e) = sonde_gateway::admin::serve_admin(admin, &server_endpoint).await {
            eprintln!("admin server ended: {e}");
        }
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match AdminClient::connect(&endpoint).await {
            Ok(_) => return endpoint,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("failed to connect to admin server: {e}"),
        }
    }
}

fn minimal_program_cbor() -> Vec<u8> {
    let bytecode: Vec<u8> = vec![
        0xB7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // mov r0, 0
        0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
    ];
    let mut cbor = Vec::new();
    cbor.push(0xA2); // map(2)
    cbor.push(0x01); // key: 1
    cbor.push(0x50); // bytes(16)
    cbor.extend_from_slice(&bytecode);
    cbor.push(0x02); // key: 2
    cbor.push(0x80); // array(0)
    cbor
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

/// Test: get_node on a fresh gateway returns not found.
#[tokio::test]
async fn grpc_get_node_missing_returns_error() {
    let mut client = start_server_and_connect("get_node_missing").await;
    let err = client
        .get_node("missing-node")
        .await
        .expect_err("missing node lookup should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// Test: remove_node on a fresh gateway returns not found.
#[tokio::test]
async fn grpc_remove_nonexistent_node_returns_error() {
    let mut client = start_server_and_connect("remove_node_missing").await;
    let err = client
        .remove_node("missing-node")
        .await
        .expect_err("removing a missing node should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// Test: factory_reset removes the node from the registry.
#[tokio::test]
async fn grpc_factory_reset_removes_node() {
    let mut client = start_server_and_connect("factory_reset_node").await;
    client
        .register_node("factory-node", 0x2222, vec![0xAB; 32])
        .await
        .unwrap();

    client.factory_reset("factory-node").await.unwrap();

    let err = client
        .get_node("factory-node")
        .await
        .expect_err("factory reset should remove the node");
    assert_eq!(err.code(), tonic::Code::NotFound);
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

/// Test: list_programs on a fresh gateway returns empty.
#[tokio::test]
async fn grpc_list_programs_empty() {
    let mut client = start_server_and_connect("list_programs_empty").await;
    let programs = client.list_programs().await.unwrap();
    assert!(programs.is_empty(), "fresh gateway should have no programs");
}

/// Test: assign a program to a node and observe it via get_node.
#[cfg(debug_assertions)]
#[tokio::test]
async fn grpc_assign_program_to_node() {
    let mut client = start_server_and_connect("assign_program_to_node").await;
    client
        .register_node("assign-node", 0x3333, vec![0xCD; 32])
        .await
        .unwrap();

    let (hash, _) = client
        .ingest_program(minimal_program_cbor(), 1, None, None)
        .await
        .unwrap();
    client
        .assign_program("assign-node", hash.clone())
        .await
        .unwrap();

    let node = client.get_node("assign-node").await.unwrap();
    assert_eq!(node.assigned_program_hash, hash);
}

/// Test: remove_program deletes the program from storage.
#[cfg(debug_assertions)]
#[tokio::test]
async fn grpc_remove_program() {
    let mut client = start_server_and_connect("remove_program").await;
    let (hash, _) = client
        .ingest_program(minimal_program_cbor(), 1, None, None)
        .await
        .unwrap();
    assert_eq!(client.list_programs().await.unwrap().len(), 1);

    client.remove_program(hash).await.unwrap();
    assert!(client.list_programs().await.unwrap().is_empty());
}

/// Test: remove_program on a missing hash returns not found.
#[tokio::test]
async fn grpc_remove_nonexistent_program_returns_error() {
    let mut client = start_server_and_connect("remove_program_missing").await;
    let err = client
        .remove_program(vec![0x11; 32])
        .await
        .expect_err("removing a missing program should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
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

/// Test: queue an ephemeral program for a node.
#[cfg(debug_assertions)]
#[tokio::test]
async fn grpc_queue_ephemeral() {
    let mut client = start_server_and_connect("queue_ephemeral").await;
    client
        .register_node("ephemeral-node", 0x4444, vec![0xEF; 32])
        .await
        .unwrap();

    let (hash, _) = client
        .ingest_program(minimal_program_cbor(), 2, None, None)
        .await
        .unwrap();
    client
        .queue_ephemeral("ephemeral-node", hash)
        .await
        .unwrap();
}

/// Test: get_node_status reports no active session before any WAKE.
#[tokio::test]
async fn grpc_get_node_status_without_session() {
    let mut client = start_server_and_connect("get_node_status_no_session").await;
    client
        .register_node("status-node", 0x5555, vec![0xBC; 32])
        .await
        .unwrap();

    let status = client.get_node_status("status-node").await.unwrap();
    assert_eq!(status.node_id, "status-node");
    assert!(!status.has_active_session);
    assert_eq!(status.battery_mv, None);
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

/// Test: get_modem_status fails cleanly when no modem transport exists.
#[tokio::test]
async fn grpc_get_modem_status_no_modem() {
    let mut client = start_server_and_connect("get_modem_status_no_modem").await;
    let err = client
        .get_modem_status()
        .await
        .expect_err("missing modem transport should fail");
    assert_eq!(err.code(), tonic::Code::Unavailable);
}

/// Test: set_modem_channel fails cleanly when no modem transport exists.
#[tokio::test]
async fn grpc_set_modem_channel_no_modem() {
    let mut client = start_server_and_connect("set_modem_channel_no_modem").await;
    let err = client
        .set_modem_channel(6)
        .await
        .expect_err("missing modem transport should fail");
    assert_eq!(err.code(), tonic::Code::Unavailable);
}

/// Test: scan_modem_channels fails cleanly when no modem transport exists.
#[tokio::test]
async fn grpc_scan_modem_channels_no_modem() {
    let mut client = start_server_and_connect("scan_modem_channels_no_modem").await;
    let err = client
        .scan_modem_channels()
        .await
        .expect_err("missing modem transport should fail");
    assert_eq!(err.code(), tonic::Code::Unavailable);
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

/// Test: add_handler, list_handlers, then remove_handler.
#[tokio::test]
async fn grpc_add_list_remove_handler() {
    let mut client = start_server_and_connect("add_list_remove_handler").await;

    client
        .add_handler("*", "echo", vec!["hello".into()], None, None)
        .await
        .unwrap();

    let handlers = client.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].program_hash, "*");
    assert_eq!(handlers[0].command, "echo");
    assert_eq!(handlers[0].args, vec!["hello"]);

    client.remove_handler("*").await.unwrap();
    assert!(client.list_handlers().await.unwrap().is_empty());
}

/// Test: `node list --format json` emits valid JSON.
#[tokio::test(flavor = "multi_thread")]
async fn cli_node_list_json_output() {
    let endpoint = start_server("cli_node_list_json_output").await;
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    client
        .register_node("json-node", 0x1234, vec![0xAA; 32])
        .await
        .unwrap();
    drop(client);

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "--format", "json", "node", "list"])
        .output()
        .expect("failed to run sonde-admin");

    assert!(output.status.success(), "CLI should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be JSON");
    let nodes = parsed
        .as_array()
        .expect("node list output must be an array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["node_id"], "json-node");
    assert_eq!(nodes[0]["key_hint"], 0x1234);
}

/// Test: non-interactive `node remove` refuses to proceed without `--yes`.
#[tokio::test(flavor = "multi_thread")]
async fn cli_node_remove_noninteractive_requires_yes() {
    let endpoint = start_server("cli_node_remove_noninteractive_requires_yes").await;
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    client
        .register_node("remove-me", 0x4321, vec![0xBB; 32])
        .await
        .unwrap();
    drop(client);

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "node", "remove", "remove-me"])
        .output()
        .expect("failed to run sonde-admin");

    assert!(
        !output.status.success(),
        "CLI should refuse non-interactive removal"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("non-interactive") || stderr.contains("--yes"),
        "stderr should explain the confirmation requirement: {stderr}"
    );

    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    let remaining = client.list_nodes().await.unwrap();
    assert_eq!(remaining.len(), 1, "node must not be removed");
}

/// Test: `node remove --yes` succeeds in non-interactive mode.
#[tokio::test(flavor = "multi_thread")]
async fn cli_node_remove_with_yes_succeeds() {
    let endpoint = start_server("cli_node_remove_with_yes_succeeds").await;
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    client
        .register_node("remove-me", 0x6789, vec![0xCC; 32])
        .await
        .unwrap();
    drop(client);

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args([
            "--socket",
            &endpoint,
            "--yes",
            "node",
            "remove",
            "remove-me",
        ])
        .output()
        .expect("failed to run sonde-admin");

    assert!(
        output.status.success(),
        "CLI should remove the node with --yes"
    );
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    assert!(client.list_nodes().await.unwrap().is_empty());
}
