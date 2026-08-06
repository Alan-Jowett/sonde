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
use std::time::{Duration, SystemTime};

use tokio::sync::RwLock;

use sha2::Digest;
use sonde_admin::grpc_client::AdminClient;
use sonde_gateway::admin::AdminService;
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::program::{ProgramRecord, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use zeroize::Zeroizing;

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

/// Start an admin gRPC server backed by a caller-owned storage handle.
async fn start_server_with_storage(test_name: &str) -> (String, Arc<InMemoryStorage>) {
    let storage = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage.clone(), pending, session_manager);

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
            Ok(_) => return (endpoint, storage),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("failed to connect to admin server: {e}"),
        }
    }
}

/// Start an admin gRPC server backed by caller-owned storage and runtime state.
async fn start_server_with_runtime(
    test_name: &str,
) -> (String, Arc<InMemoryStorage>, Arc<SessionManager>) {
    let storage = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage.clone(), pending, Arc::clone(&session_manager));

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
            Ok(_) => return (endpoint, storage, session_manager),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("failed to connect to admin server: {e}"),
        }
    }
}

async fn seed_node_with_programs(
    storage: &Arc<InMemoryStorage>,
    node_id: &str,
    key_hint: u16,
    assigned_program_hash: Option<Vec<u8>>,
    current_program_hash: Option<Vec<u8>>,
) {
    let mut node = NodeRecord::new(node_id.to_string(), key_hint, [0xAA; 32]);
    node.assigned_program_hash = assigned_program_hash;
    node.current_program_hash = current_program_hash;
    storage.upsert_node(&node).await.unwrap();
}

async fn seed_program(
    storage: &Arc<InMemoryStorage>,
    hash: Vec<u8>,
    source_filename: Option<&str>,
) {
    storage
        .store_program(&ProgramRecord {
            size: 1,
            image: vec![0x00],
            hash,
            verification_profile: VerificationProfile::Resident,
            abi_version: None,
            source_filename: source_filename.map(str::to_string),
            decoder_image: None,
        })
        .await
        .unwrap();
}

async fn seed_phone_psk(storage: &Arc<InMemoryStorage>, key_hint: u16, label: &str) -> u32 {
    storage
        .store_phone_psk(&PhonePskRecord {
            phone_id: 0,
            phone_key_hint: key_hint,
            psk: Zeroizing::new([0x33; 32]),
            label: label.to_string(),
            issued_at: SystemTime::UNIX_EPOCH,
            status: PhonePskStatus::Active,
            key_version: 1,
        })
        .await
        .unwrap()
}

#[cfg(debug_assertions)]
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
        .ingest_program(
            cbor.clone(),
            1,
            None,
            Some(r"C:\captures\temp-reader.o".to_string()),
        )
        .await
        .unwrap();
    assert!(!hash.is_empty(), "program hash must not be empty");
    assert!(size > 0, "program size must be non-zero");

    let programs = client.list_programs().await.unwrap();
    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0].hash, hash);
    assert_eq!(
        programs[0].source_filename.as_deref(),
        Some("temp-reader.o")
    );
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

/// Test: list/get/status surface runtime battery observations without durable persistence.
#[tokio::test]
async fn grpc_runtime_battery_is_visible_in_status_surfaces() {
    let (endpoint, _storage, session_manager) =
        start_server_with_runtime("runtime_battery_status").await;
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    client
        .register_node("status-node", 0x5555, vec![0xBC; 32])
        .await
        .unwrap();

    session_manager.record_battery_mv("status-node", 3300).await;

    let nodes = client.list_nodes().await.unwrap();
    assert_eq!(nodes[0].last_battery_mv, Some(3300));

    let node = client.get_node("status-node").await.unwrap();
    assert_eq!(node.last_battery_mv, Some(3300));

    let status = client.get_node_status("status-node").await.unwrap();
    assert_eq!(status.battery_mv, Some(3300));
}

// ── BLE pairing tests ───────────────────────────────────────────────────────

/// Test: list phones on empty gateway returns empty list.
#[tokio::test]
async fn grpc_list_phones_empty() {
    let mut client = start_server_and_connect("list_phones_empty").await;
    let phones = client.list_phones().await.unwrap();
    assert!(phones.is_empty(), "empty gateway should have no phones");
}

/// Test: `pairing list-phones` renders an empty table and empty JSON array.
#[tokio::test(flavor = "multi_thread")]
async fn cli_list_phones_empty() {
    let endpoint = start_server("cli_list_phones_empty").await;

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "pairing", "list-phones"])
        .output()
        .expect("failed to run sonde-admin pairing list-phones");

    assert!(output.status.success(), "CLI list-phones should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "empty list should only print the table header"
    );
    assert!(lines[0].contains("ID"));
    assert!(lines[0].contains("Key Hint"));
    assert!(lines[0].contains("Label"));
    assert!(lines[0].contains("Status"));

    let json_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args([
            "--socket",
            &endpoint,
            "--format",
            "json",
            "pairing",
            "list-phones",
        ])
        .output()
        .expect("failed to run sonde-admin pairing list-phones --format json");

    assert!(json_output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json_output.stdout))
            .expect("stdout must be valid JSON");
    assert_eq!(parsed, serde_json::json!([]));
}

/// Test: `pairing list-phones` renders seeded phone metadata.
#[tokio::test(flavor = "multi_thread")]
async fn cli_list_phones_shows_seeded_phone() {
    let (endpoint, storage) = start_server_with_storage("cli_list_phones_shows_seeded_phone").await;
    let phone_id = seed_phone_psk(&storage, 0x1234, "Alice phone").await;

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "pairing", "list-phones"])
        .output()
        .expect("failed to run sonde-admin pairing list-phones");

    assert!(output.status.success(), "CLI list-phones should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&phone_id.to_string()));
    assert!(stdout.contains("0x1234"));
    assert!(stdout.contains("Alice phone"));
    assert!(stdout.contains("active"));
}

/// Test: close BLE pairing when not open (may error — not an operational concern).
#[tokio::test]
async fn grpc_close_ble_pairing_when_not_open() {
    let mut client = start_server_and_connect("close_ble_noop").await;
    // Closing when not open may succeed or return an error; either is acceptable.
    // The important thing is it doesn't panic.
    let _ = client.close_ble_pairing().await;
}

/// Test: `pairing stop` refuses to run non-interactively without `--yes`.
#[tokio::test(flavor = "multi_thread")]
async fn cli_pairing_stop_requires_yes_noninteractive() {
    let endpoint = start_server("cli_pairing_stop_requires_yes_noninteractive").await;

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "pairing", "stop"])
        .output()
        .expect("failed to run sonde-admin pairing stop");

    assert!(
        !output.status.success(),
        "pairing stop should refuse non-interactive confirmation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("refusing to proceed without confirmation"));
    assert!(!stderr.contains("Close BLE pairing window? [y/N]:"));
}

/// Test: revoke a non-existent phone returns an error.
#[tokio::test]
async fn grpc_revoke_nonexistent_phone() {
    let mut client = start_server_and_connect("revoke_nonexistent").await;
    let result = client.revoke_phone(999).await;
    assert!(result.is_err(), "revoking non-existent phone should fail");
}

/// Test: `pairing revoke-phone --yes` updates status and prints the result.
#[tokio::test(flavor = "multi_thread")]
async fn cli_pairing_revoke_phone_with_yes() {
    let (endpoint, storage) = start_server_with_storage("cli_pairing_revoke_phone_with_yes").await;
    let phone_id = seed_phone_psk(&storage, 0x4567, "Revokable phone").await;

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args([
            "--socket",
            &endpoint,
            "--yes",
            "pairing",
            "revoke-phone",
            &phone_id.to_string(),
        ])
        .output()
        .expect("failed to run sonde-admin pairing revoke-phone");

    assert!(output.status.success(), "CLI revoke-phone should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("Phone {phone_id} revoked")));

    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    let phones = client.list_phones().await.unwrap();
    assert_eq!(phones.len(), 1);
    assert_eq!(phones[0].phone_id, phone_id);
    assert_eq!(phones[0].status, "revoked");
}

/// Test: transient modem display fails cleanly when no modem transport exists.
#[tokio::test]
async fn grpc_show_modem_display_message_no_modem() {
    let mut client = start_server_and_connect("show_modem_display_message_no_modem").await;
    let result = client
        .show_modem_display_message(vec!["Device login".to_string()], false)
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

/// Test: human-readable node status prefers source_filename and falls back to hash.
#[tokio::test(flavor = "multi_thread")]
async fn cli_node_status_prefers_source_filename() {
    let (endpoint, storage) =
        start_server_with_storage("cli_node_status_prefers_source_filename").await;
    let named_hash = vec![0x11; 32];
    let fallback_hash = vec![0x22; 32];
    let fallback_hash_hex = hex::encode(&fallback_hash);
    let named_hash_hex = hex::encode(&named_hash);
    let full_source_path = r"C:\captures\temp-reader.o";

    seed_program(&storage, named_hash.clone(), Some(full_source_path)).await;
    seed_program(&storage, fallback_hash.clone(), None).await;
    seed_node_with_programs(
        &storage,
        "named-node",
        0x1001,
        Some(named_hash.clone()),
        Some(named_hash.clone()),
    )
    .await;
    seed_node_with_programs(
        &storage,
        "fallback-node",
        0x1002,
        Some(fallback_hash.clone()),
        Some(fallback_hash.clone()),
    )
    .await;

    let list_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "node", "list"])
        .output()
        .expect("failed to run sonde-admin node list");
    assert!(list_output.status.success(), "node list should succeed");
    let list_stdout = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        list_stdout.contains("temp-reader.o"),
        "named program should use source_filename: {list_stdout}"
    );
    assert!(
        list_stdout.contains(&fallback_hash_hex),
        "missing source_filename should fall back to hash: {list_stdout}"
    );
    assert!(
        !list_stdout.contains(&named_hash_hex),
        "default node list should hide the hash when source_filename exists: {list_stdout}"
    );
    assert!(
        !list_stdout.contains(full_source_path),
        "node list should not render the full source path: {list_stdout}"
    );

    let get_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "node", "get", "named-node"])
        .output()
        .expect("failed to run sonde-admin node get");
    assert!(get_output.status.success(), "node get should succeed");
    let get_stdout = String::from_utf8_lossy(&get_output.stdout);
    assert!(
        get_stdout.contains("temp-reader.o"),
        "node get should use source_filename: {get_stdout}"
    );
    assert!(
        !get_stdout.contains(&named_hash_hex),
        "default node get should hide the hash when source_filename exists: {get_stdout}"
    );
    assert!(
        !get_stdout.contains(full_source_path),
        "node get should not render the full source path: {get_stdout}"
    );

    let status_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "status", "named-node"])
        .output()
        .expect("failed to run sonde-admin status");
    assert!(status_output.status.success(), "status should succeed");
    let status_stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        status_stdout.contains("temp-reader.o"),
        "status should use source_filename: {status_stdout}"
    );
    assert!(
        !status_stdout.contains(&named_hash_hex),
        "default status should hide the hash when source_filename exists: {status_stdout}"
    );
    assert!(
        !status_stdout.contains(full_source_path),
        "status should not render the full source path: {status_stdout}"
    );

    let json_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args([
            "--socket",
            &endpoint,
            "--format",
            "json",
            "status",
            "named-node",
        ])
        .output()
        .expect("failed to run sonde-admin status --format json");
    assert!(json_output.status.success(), "JSON status should succeed");
    let json_stdout = String::from_utf8_lossy(&json_output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&json_stdout).expect("stdout must be valid JSON");
    assert_eq!(parsed["current_program_hash"], named_hash_hex);
}

/// Test: verbose node status includes hashes alongside source_filename.
#[tokio::test(flavor = "multi_thread")]
async fn cli_node_status_verbose_shows_hash() {
    let (endpoint, storage) = start_server_with_storage("cli_node_status_verbose_shows_hash").await;
    let named_hash = vec![0x33; 32];
    let named_hash_hex = hex::encode(&named_hash);

    seed_program(&storage, named_hash.clone(), Some("/tmp/temp-reader.o")).await;
    seed_node_with_programs(
        &storage,
        "named-node",
        0x2001,
        Some(named_hash.clone()),
        Some(named_hash.clone()),
    )
    .await;

    for args in [
        vec!["--socket", &endpoint, "--verbose", "node", "list"],
        vec![
            "--socket",
            &endpoint,
            "--verbose",
            "node",
            "get",
            "named-node",
        ],
        vec!["--socket", &endpoint, "--verbose", "status", "named-node"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
            .args(&args)
            .output()
            .expect("failed to run verbose sonde-admin command");
        assert!(output.status.success(), "verbose command should succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("temp-reader.o"),
            "verbose output should still show source_filename: {stdout}"
        );
        assert!(
            stdout.contains(&named_hash_hex),
            "verbose output should show the underlying hash: {stdout}"
        );
    }
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

// ── Key management integration tests (T-0900 through T-0905) ───────────────

/// Seed a deterministic gateway identity and master key metadata into storage.
///
/// Returns the identity so callers can derive expected fingerprint words, etc.
async fn seed_gateway_identity(storage: &Arc<InMemoryStorage>) -> GatewayIdentity {
    let seed = Zeroizing::new([0x42u8; 32]);
    let gateway_id = [0x01u8; 16];
    let identity = GatewayIdentity::from_parts(seed, gateway_id);
    storage.store_gateway_identity(&identity).await.unwrap();

    let master_key = [0x42u8; 32];
    let master_key_id = sha2::Sha256::digest(master_key);
    storage
        .set_config("master_key_id", &hex::encode(master_key_id))
        .await
        .unwrap();
    storage.set_config("master_key_epoch", "1").await.unwrap();

    identity
}

/// Start an admin gRPC server with a seeded gateway identity and a rotation
/// channel.  Returns the endpoint, storage, identity, and a receiver that
/// accepts rotation payloads (for use in T-0900/T-0901).
async fn start_server_with_rotation(
    test_name: &str,
) -> (
    String,
    Arc<InMemoryStorage>,
    GatewayIdentity,
    tokio::sync::mpsc::UnboundedReceiver<(
        Vec<u8>,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    )>,
) {
    let storage = Arc::new(InMemoryStorage::new());
    let identity = seed_gateway_identity(&storage).await;

    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let admin = AdminService::new(storage.clone(), pending, session_manager).with_rotation_tx(tx);

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
            Ok(_) => return (endpoint, storage, identity, rx),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("failed to connect to admin server: {e}"),
        }
    }
}

/// Build a rotation payload for testing. Mirrors the logic in `main.rs`
/// `build_rotation_payload()` without requiring it to be public.
fn build_test_rotation_payload(
    gw_x25519_public: &[u8; 32],
    gateway_id_raw: &[u8; 16],
    master_key_epoch: u64,
    new_master_key: &[u8; 32],
    rotation_code: &str,
) -> Vec<u8> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use hkdf::Hkdf;
    use sha2::Sha256;
    use x25519_dalek::{EphemeralSecret, PublicKey};

    let ephemeral_secret = EphemeralSecret::random();
    let ephemeral_public = PublicKey::from(&ephemeral_secret);

    let gw_public = PublicKey::from(*gw_x25519_public);
    let shared_secret = ephemeral_secret.diffie_hellman(&gw_public);

    let hkdf_salt = b"sonde-rotation-v1";
    let mut info = Vec::with_capacity(24);
    info.extend_from_slice(gateway_id_raw);
    info.extend_from_slice(&master_key_epoch.to_be_bytes());

    let hkdf = Hkdf::<Sha256>::new(Some(hkdf_salt), shared_secret.as_bytes());
    let mut aes_key = [0u8; 32];
    hkdf.expand(&info, &mut aes_key).unwrap();

    // Deterministic CBOR: 2-entry map with integer keys 1–2.
    let mut plaintext = Vec::with_capacity(64);
    plaintext.push(0xA2); // map(2)
                          // key 1: new_master_key (bstr 32)
    plaintext.push(0x01);
    plaintext.push(0x58);
    plaintext.push(32);
    plaintext.extend_from_slice(new_master_key);
    // key 2: rotation_code (tstr)
    plaintext.push(0x02);
    let code_bytes = rotation_code.as_bytes();
    if code_bytes.len() < 24 {
        plaintext.push(0x60 | code_bytes.len() as u8);
    } else {
        plaintext.push(0x78);
        plaintext.push(code_bytes.len() as u8);
    }
    plaintext.extend_from_slice(code_bytes);

    let cipher = Aes256Gcm::new((&aes_key).into());
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).unwrap();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext_and_tag = cipher
        .encrypt(
            nonce,
            aes_gcm::aead::Payload {
                msg: &plaintext,
                aad: &info,
            },
        )
        .unwrap();

    let mut payload = Vec::with_capacity(1 + 32 + 12 + ciphertext_and_tag.len());
    payload.push(0x01);
    payload.extend_from_slice(ephemeral_public.as_bytes());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&ciphertext_and_tag);
    payload
}

/// T-0903: `key fingerprint` returns the BIP-39 fingerprint words.
#[tokio::test(flavor = "multi_thread")]
async fn key_fingerprint_returns_six_words() {
    let (endpoint, _storage, _identity, _rx) =
        start_server_with_rotation("key_fingerprint_returns_six_words").await;

    // Fetch state via AdminClient to get expected fingerprint.
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    let state = client.get_gateway_state().await.unwrap();
    assert_eq!(
        state.fingerprint_words.len(),
        6,
        "BIP-39 fingerprint should be 6 words"
    );

    drop(client);

    // Verify the CLI binary produces the same words.
    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "key", "fingerprint"])
        .output()
        .expect("failed to run sonde-admin key fingerprint");
    assert!(
        output.status.success(),
        "CLI key fingerprint should succeed"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = state.fingerprint_words.join(" ");
    assert_eq!(
        stdout.trim(),
        expected,
        "CLI output must match gRPC fingerprint"
    );

    // JSON format.
    let json_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args([
            "--socket",
            &endpoint,
            "--format",
            "json",
            "key",
            "fingerprint",
        ])
        .output()
        .expect("failed to run sonde-admin key fingerprint --format json");
    assert!(json_output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json_output.stdout))
            .expect("stdout must be valid JSON");
    assert_eq!(parsed["fingerprint_words"].as_array().unwrap().len(), 6);
}

/// T-0904: `key status` returns epoch, master_key_id, and rotation_in_progress.
#[tokio::test(flavor = "multi_thread")]
async fn key_status_shows_all_fields() {
    let (endpoint, _storage, _identity, _rx) =
        start_server_with_rotation("key_status_shows_all_fields").await;
    let expected_master_key_id = hex::encode(sha2::Sha256::digest([0x42u8; 32]));

    let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "key", "status"])
        .output()
        .expect("failed to run sonde-admin key status");
    assert!(output.status.success(), "CLI key status should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Master key epoch:      1"));
    assert!(stdout.contains(&expected_master_key_id));
    assert!(stdout.contains("Rotation in progress:  false"));

    let json_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "--format", "json", "key", "status"])
        .output()
        .expect("failed to run sonde-admin key status");
    assert!(
        json_output.status.success(),
        "CLI key status JSON should succeed"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json_output.stdout))
            .expect("stdout must be valid JSON");

    assert_eq!(parsed["master_key_epoch"], 1);
    assert_eq!(parsed["master_key_id"], expected_master_key_id);
    assert_eq!(parsed["rotation_in_progress"], false);
}

/// T-0904 supplement: `key status` exposes only current key metadata fields.
#[tokio::test(flavor = "multi_thread")]
async fn key_status_omits_legacy_fields() {
    let (endpoint, _storage, _identity, _rx) =
        start_server_with_rotation("key_status_omits_legacy_fields").await;

    let json_output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
        .args(["--socket", &endpoint, "--format", "json", "key", "status"])
        .output()
        .expect("failed to run sonde-admin key status");
    assert!(json_output.status.success());

    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json_output.stdout))
            .expect("stdout must be valid JSON");
    let object = parsed.as_object().expect("status JSON must be an object");
    assert_eq!(
        object.len(),
        3,
        "status JSON should expose only current fields"
    );
    assert!(object.contains_key("master_key_epoch"));
    assert!(object.contains_key("master_key_id"));
    assert!(object.contains_key("rotation_in_progress"));
}

/// T-0900: `key rotate` happy path — submit a valid rotation payload to the
/// gateway and receive acceptance.
#[tokio::test(flavor = "multi_thread")]
async fn key_rotate_submit_accepted() {
    let (endpoint, storage, _identity, mut rx) =
        start_server_with_rotation("key_rotate_submit_accepted").await;

    // Spawn a mock rotation handler that accepts the first payload.
    let storage_clone = storage.clone();
    let handler = tokio::spawn(async move {
        let (payload, resp_tx) = rx.recv().await.expect("should receive rotation payload");
        assert!(!payload.is_empty(), "payload must not be empty");
        // Simulate successful rotation: bump epoch.
        storage_clone
            .set_config("master_key_epoch", "2")
            .await
            .unwrap();
        resp_tx.send(Ok(())).ok();
    });

    // Build a rotation payload using the AdminClient directly (not the CLI
    // binary, because the CLI prompts for interactive input).
    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    let state = client.get_gateway_state().await.unwrap();
    assert_eq!(state.master_key_epoch, 1);

    // Construct payload the same way the CLI does.
    let gw_x25519_pub: [u8; 32] = state.x25519_public_key.try_into().unwrap();
    let gateway_id_raw: [u8; 16] = state.gateway_id.try_into().unwrap();

    let payload = build_test_rotation_payload(
        &gw_x25519_pub,
        &gateway_id_raw,
        state.master_key_epoch,
        &[0x42u8; 32],
        "TESTCODE",
    );

    let resp = client.submit_rotation(payload).await.unwrap();
    assert!(resp.accepted, "rotation should be accepted: {}", resp.error);

    handler.await.unwrap();

    // Verify epoch bumped.
    let state2 = client.get_gateway_state().await.unwrap();
    assert_eq!(state2.master_key_epoch, 2, "epoch must have incremented");
}

/// T-0901: `key rotate` with wrong rotation code — gateway rejects.
#[tokio::test(flavor = "multi_thread")]
async fn key_rotate_rejected_by_engine() {
    let (endpoint, _storage, _identity, mut rx) =
        start_server_with_rotation("key_rotate_rejected_by_engine").await;

    // Spawn a mock rotation handler that rejects.
    tokio::spawn(async move {
        let (_payload, resp_tx) = rx.recv().await.expect("should receive rotation payload");
        resp_tx.send(Err("invalid rotation code".to_string())).ok();
    });

    let mut client = AdminClient::connect(&endpoint).await.unwrap();
    let state = client.get_gateway_state().await.unwrap();

    let gw_x25519_pub: [u8; 32] = state.x25519_public_key.try_into().unwrap();
    let gateway_id_raw: [u8; 16] = state.gateway_id.try_into().unwrap();

    let payload = build_test_rotation_payload(
        &gw_x25519_pub,
        &gateway_id_raw,
        state.master_key_epoch,
        &[0x42u8; 32],
        "WRONGCODE",
    );

    let resp = client.submit_rotation(payload).await.unwrap();
    assert!(!resp.accepted, "rotation should be rejected");
    assert!(
        resp.error.contains("invalid rotation code"),
        "error should mention the reason: {}",
        resp.error
    );
}

/// T-0905: Key material must not appear in CLI output.
#[tokio::test(flavor = "multi_thread")]
async fn key_material_not_in_cli_output() {
    let (endpoint, _storage, _identity, _rx) =
        start_server_with_rotation("key_material_not_in_cli_output").await;

    // Run fingerprint and status commands, capture all output.
    for subcmd in &["fingerprint", "status"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sonde-admin"))
            .args(["--socket", &endpoint, "key", subcmd])
            .output()
            .expect("failed to run sonde-admin");

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        // The seed [0x42; 32] and master key should never appear in output.
        let seed_hex = hex::encode([0x42u8; 32]);
        assert!(
            !combined.contains(&seed_hex),
            "key `{subcmd}` output must not contain the Ed25519 seed"
        );
    }
}
