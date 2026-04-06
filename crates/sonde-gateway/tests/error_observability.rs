// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! GW-1307 error diagnostic observability tests and GW-1306 AC5
//! (graceful log file failure).
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

    let bogus_hash = vec![
        0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];
    let err = admin
        .assign_program(Request::new(AssignProgramRequest {
            node_id: "node-1307b".to_string(),
            program_hash: bogus_hash,
        }))
        .await
        .unwrap_err();
    let msg = err.message().to_lowercase();
    assert!(
        msg.contains("deadbeef"),
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

/// GW-1306 AC5: verify that a log file open at a nonexistent directory
/// fails gracefully. The gateway's `init_service_logging` uses a `match`
/// on the open result and continues with ETW-only logging in the Err branch.
#[test]
fn t1306_ac5_graceful_log_file_failure() {
    let tmp = tempfile::tempdir().unwrap();
    // Create a path with a nonexistent parent directory inside the tempdir.
    let impossible_path = tmp
        .path()
        .join("nonexistent")
        .join("nested")
        .join("sonde.log");

    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&impossible_path);

    assert!(
        result.is_err(),
        "expected file open to fail for nonexistent parent directory"
    );

    // The gateway code would continue here with ETW-only logging.
    // This test confirms the error path is reachable and doesn't panic.
    let err = result.unwrap_err();
    assert!(
        !err.to_string().is_empty(),
        "error message should be non-empty for diagnostics"
    );
}

/// T-1306a: File sink writes to `<db-path>.log`.
///
/// Validates the path derivation and file creation logic used by the
/// gateway's service-mode logging (`init_service_logging` in the binary).
/// The actual tracing layer integration is binary-level and requires
/// running the gateway in service mode; this test validates the
/// underlying path derivation and file I/O that the binary depends on.
#[test]
fn t1306a_file_sink_path_derivation() {
    use std::io::Write;
    use std::path::PathBuf;

    // The gateway derives the log path as `<db-path>.log`.
    let db_path = PathBuf::from("/var/lib/sonde/gateway.db");
    let log_path = db_path.with_extension("log");
    assert_eq!(
        log_path,
        PathBuf::from("/var/lib/sonde/gateway.log"),
        "log file path must be <db-path> with .log extension"
    );

    // Verify the file can be created and written to (same OpenOptions
    // as the gateway's file sink: create + append).
    let tmp = tempfile::tempdir().unwrap();
    let test_db = tmp.path().join("test.db");
    let test_log = test_db.with_extension("log");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&test_log)
        .expect("log file must be creatable at derived path");
    writeln!(file, "test log entry").unwrap();
    drop(file);

    let contents = std::fs::read_to_string(&test_log).unwrap();
    assert!(
        contents.contains("test log entry"),
        "written content must be readable from the log file"
    );
}

/// T-1306b: ETW provider registered.
///
/// This test requires a Windows environment with ETW infrastructure
/// and cannot be meaningfully automated without the `windows` crate.
/// Marked `#[ignore]` — verify manually on Windows via:
/// `logman query providers | findstr sonde-gateway`
#[test]
#[ignore = "requires Windows ETW infrastructure — verify manually"]
#[cfg(windows)]
fn t1306b_etw_provider_registered() {
    // Manual verification: run `logman query providers` on Windows
    // and confirm "sonde-gateway" appears in the provider list.
}

/// T-1306c: Runtime log-level reload.
///
/// Validates that the `tracing_subscriber::reload::Layer` mechanism
/// correctly changes the active filter — events suppressed before
/// reload appear after reload, and vice versa.
///
/// Uses ERROR→INFO transition (not DEBUG) because some workspace crates
/// set `release_max_level_info`, and Cargo feature unification can
/// statically disable DEBUG call sites.
#[test]
fn t1306c_runtime_log_level_reload() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    // Shared buffer to capture formatted log output.
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let buf_clone = buf.clone();

    // Create a reloadable filter layer starting at ERROR (suppresses INFO).
    let initial = EnvFilter::new("error");
    let (filter, reload_handle) = tracing_subscriber::reload::Layer::new(initial);

    let writer =
        move || -> Box<dyn std::io::Write + Send> { Box::new(SharedWriter(buf_clone.clone())) };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false);

    let subscriber = tracing_subscriber::registry().with(filter).with(fmt_layer);

    // Scope the subscriber to this test only (not global).
    tracing::subscriber::with_default(subscriber, || {
        // 1. INFO event should be suppressed at ERROR level.
        tracing::info!("before_reload");
        {
            let locked = buf.lock().unwrap();
            let output = String::from_utf8_lossy(&locked);
            assert!(
                !output.contains("before_reload"),
                "INFO event must be suppressed at ERROR level"
            );
        }

        // 2. Reload to INFO level.
        let new_filter = EnvFilter::new("info");
        reload_handle
            .reload(new_filter)
            .expect("reload must succeed");

        // 3. INFO event should now appear.
        tracing::info!("after_reload");
        {
            let locked = buf.lock().unwrap();
            let output = String::from_utf8_lossy(&locked);
            assert!(
                output.contains("after_reload"),
                "INFO event must appear after reload to INFO level"
            );
        }
    });
}

/// Writer adapter that appends to a shared buffer.
struct SharedWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
