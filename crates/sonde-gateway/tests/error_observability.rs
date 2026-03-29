// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! GW-1307 error diagnostic observability tests.
//!
//! Validates that error messages at user-facing boundaries include
//! operation name, input/parameters, subsystem error, and actionable
//! guidance where applicable.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tonic::Request;
use zeroize::Zeroizing;

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use sonde_gateway::admin::AdminService;
use sonde_gateway::engine::PendingCommand;
use sonde_gateway::key_provider::{EnvKeyProvider, FileKeyProvider, KeyProvider};
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::InMemoryStorage;

// ── Helpers ─────────────────────────────────────────────────────────────

fn make_admin() -> AdminService {
    let storage = Arc::new(InMemoryStorage::new());
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let sm = Arc::new(SessionManager::new(Duration::from_secs(30)));
    AdminService::new(storage, pending, sm)
}

// ── T-1307a: IngestProgram empty image includes operation + guidance ────

/// GW-1307 AC1: IngestProgram with empty image includes operation name
/// and actionable guidance in the error response.
#[tokio::test]
async fn t1307a_ingest_empty_image_includes_context() {
    let admin = make_admin();
    let err = admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: vec![],
            verification_profile: 1, // RESIDENT
            abi_version: None,
            source_filename: Some("test.o".to_string()),
        }))
        .await
        .unwrap_err();
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("program ingestion failed") || msg.contains("image is empty"),
        "error should include specific operation name, got: {msg}"
    );
    assert!(
        msg.contains("test.o") || msg.contains("source"),
        "error should include source filename, got: {msg}"
    );
}

// ── T-1307b: AssignProgram with missing program includes hash + guidance

/// GW-1307 AC1: AssignProgram with nonexistent program includes the
/// program hash and actionable guidance.
#[tokio::test]
async fn t1307b_assign_program_missing_includes_context() {
    let admin = make_admin();
    // Register a node first.
    let psk = vec![0x42u8; 32];
    admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "node-1307b".to_string(),
            key_hint: 0x1307,
            psk,
        }))
        .await
        .unwrap();

    let bogus_hash = vec![0xDE, 0xAD];
    let err = admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "node-1307b".to_string(),
            program_hash: bogus_hash,
        }))
        .await
        .unwrap_err();
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("dead"),
        "error should include the program hash, got: {msg}"
    );
    assert!(
        msg.contains("ingestprogram") || msg.contains("listprograms"),
        "error should include actionable guidance (IngestProgram/ListPrograms), got: {msg}"
    );
}

// ── T-1307c: Key provider missing file includes path + guidance ────────

/// GW-1307 AC4 (adapted): FileKeyProvider error includes the file path
/// and guidance for creating a key.
#[test]
fn t1307c_file_key_provider_missing_includes_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("nonexistent-subdir")
        .join("missing-key.hex");
    let provider = FileKeyProvider::new(path);
    let err = provider.load_master_key().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing-key.hex"),
        "error should include the file path, got: {msg}"
    );
    assert!(
        msg.contains("sonde-admin")
            || msg.contains("generate-key")
            || msg.contains("--master-key-file"),
        "error should include guidance for creating a key, got: {msg}"
    );
}

// ── T-1307d: Key provider wrong length includes expected vs actual ─────

/// GW-1307 AC1: parse_hex_key with wrong length includes expected and
/// actual character count.
#[test]
fn t1307d_key_wrong_length_includes_details() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short.key");
    std::fs::write(&path, "aabb").unwrap();
    let provider = FileKeyProvider::new(path);
    let err = provider.load_master_key().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("64") && msg.contains("4"),
        "error should include expected (64) and actual (4) length, got: {msg}"
    );
}

// ── T-1307e: EnvKeyProvider not set includes variable name + guidance ──

/// GW-1307 AC1: EnvKeyProvider includes the variable name and guidance.
#[test]
fn t1307e_env_key_not_set_includes_guidance() {
    let provider = EnvKeyProvider::new("SONDE_TEST_KEY_1307_NONEXISTENT");
    let err = provider.load_master_key().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("SONDE_TEST_KEY_1307_NONEXISTENT"),
        "error should include the env var name, got: {msg}"
    );
    assert!(
        msg.contains("--key-provider") || msg.contains("file"),
        "error should include guidance, got: {msg}"
    );
}

// ── T-1307f: SqliteStorage open failure includes path + guidance ───────

/// GW-1307 AC1: SqliteStorage::open with bad path includes path and guidance.
#[test]
fn t1307f_sqlite_open_failure_includes_path() {
    let master_key = Zeroizing::new([0x42u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let bad_path = dir
        .path()
        .join("nonexistent-dir")
        .join("does-not-exist")
        .join("gateway.db");
    let result = sonde_gateway::sqlite_storage::SqliteStorage::open(&bad_path, master_key);
    match result {
        Ok(_) => panic!("expected error for nonexistent path"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("nonexistent-dir")
                    || msg.contains("does-not-exist")
                    || msg.contains("gateway.db"),
                "error should include the database path, got: {msg}"
            );
            assert!(
                msg.contains("permission") || msg.contains("directory"),
                "error should include actionable guidance, got: {msg}"
            );
        }
    }
}

// ── T-1307g: import_state decryption failure includes guidance ─────────

/// GW-1307 AC2: import_state with garbage data includes guidance about
/// passphrase verification.
#[tokio::test]
async fn t1307g_import_state_decrypt_failure_includes_guidance() {
    let admin = make_admin();
    let err = admin
        .import_state(Request::new(ImportStateRequest {
            data: vec![0xFF; 64],
            passphrase: "wrong-passphrase".to_string(),
        }))
        .await
        .unwrap_err();
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("import state") || msg.contains("decrypt"),
        "error should include operation name, got: {msg}"
    );
    // Garbage data triggers InvalidMagic, so guidance is about invalid bundle
    // (not passphrase). The test validates that variant-specific guidance is
    // present per GW-1307 AC2.
    assert!(
        msg.contains("passphrase")
            || msg.contains("corrupt")
            || msg.contains("valid")
            || msg.contains("bundle"),
        "error should include variant-specific guidance, got: {msg}"
    );
}

// ── T-1307h: export_state empty passphrase includes guidance ───────────

/// GW-1307 AC2: export_state with empty passphrase includes operation
/// context and guidance.
#[tokio::test]
async fn t1307h_export_state_empty_passphrase_includes_guidance() {
    let admin = make_admin();
    let err = admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: String::new(),
        }))
        .await
        .unwrap_err();
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("export state") || msg.contains("passphrase"),
        "error should include operation name, got: {msg}"
    );
    assert!(
        msg.contains("passphrase") && msg.contains("not be empty"),
        "error should explain the problem, got: {msg}"
    );
}

// ── T-1307i: QueueEphemeral with wrong profile includes hash + profile ─

/// GW-1307 AC1: QueueEphemeral with non-ephemeral program includes
/// the program hash and profile in the error.
#[cfg(debug_assertions)]
#[tokio::test]
async fn t1307i_queue_ephemeral_wrong_profile_includes_details() {
    let admin = make_admin();

    // Register a node.
    admin
        .register_node(Request::new(RegisterNodeRequest {
            node_id: "node-1307i".to_string(),
            key_hint: 0x1307,
            psk: vec![0x42u8; 32],
        }))
        .await
        .unwrap();

    // Ingest a RESIDENT program (not ephemeral).
    let image = {
        let img = sonde_protocol::ProgramImage {
            bytecode: vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            maps: vec![],
            map_initial_data: vec![],
        };
        img.encode_deterministic().unwrap()
    };
    let resp = admin
        .ingest_program(Request::new(IngestProgramRequest {
            image_data: image,
            verification_profile: 1, // RESIDENT
            abi_version: None,
            source_filename: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let err = admin
        .queue_ephemeral(Request::new(QueueEphemeralRequest {
            node_id: "node-1307i".to_string(),
            program_hash: resp.program_hash.clone(),
        }))
        .await
        .unwrap_err();
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("queue ephemeral"),
        "error should include operation name, got: {msg}"
    );
    assert!(
        msg.contains("resident") || msg.contains("ephemeral"),
        "error should include the verification profile, got: {msg}"
    );
}
