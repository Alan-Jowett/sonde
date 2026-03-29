// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Handler configuration management tests (T-1400 through T-1407).
//!
//! Validates GW-1401 (storage), GW-1402 (admin API), GW-1405 (bootstrap),
//! GW-1406 (state export/import), and input validation.

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
use sonde_gateway::handler::{HandlerConfig, ProgramMatcher};
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::{HandlerRecord, InMemoryStorage, Storage};

// ─── Test helpers ──────────────────────────────────────────────────────

const TEST_MASTER_KEY: [u8; 32] = [0x42u8; 32];

fn test_key() -> Zeroizing<[u8; 32]> {
    Zeroizing::new(TEST_MASTER_KEY)
}

// 64-character hex hashes for testing.
const HASH_A: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const HASH_B: &str = "ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122";

fn make_record(program_hash: &str, command: &str) -> HandlerRecord {
    HandlerRecord {
        program_hash: program_hash.to_string(),
        command: command.to_string(),
        args: vec![],
        working_dir: None,
        reply_timeout_ms: None,
    }
}

struct AdminHarness {
    #[allow(dead_code)]
    storage: Arc<dyn Storage>,
    admin: AdminService,
}

impl AdminHarness {
    fn new_sqlite() -> Self {
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory(test_key()).unwrap());
        let pending_commands: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
        let admin = AdminService::new(storage.clone(), pending_commands, session_manager);
        Self { storage, admin }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1400: Handler storage CRUD
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1400_handler_storage_crud() {
    let store = SqliteStorage::in_memory(test_key()).unwrap();

    // 1. Initially empty.
    assert!(store.list_handlers().await.unwrap().is_empty());

    // 2. Add a catch-all handler.
    let record = HandlerRecord {
        program_hash: "*".to_string(),
        command: "python".to_string(),
        args: vec!["handler.py".to_string()],
        working_dir: None,
        reply_timeout_ms: None,
    };
    assert!(store.add_handler(&record).await.unwrap());

    // 3. list_handlers returns one record.
    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].program_hash, "*");
    assert_eq!(handlers[0].command, "python");
    assert_eq!(handlers[0].args, vec!["handler.py"]);
    assert!(handlers[0].working_dir.is_none());

    // 4. Duplicate insert returns false.
    assert!(!store.add_handler(&record).await.unwrap());

    // 5. Still one record.
    assert_eq!(store.list_handlers().await.unwrap().len(), 1);

    // 6. Add a hex-hash handler.
    let hex_record = HandlerRecord {
        program_hash: HASH_A.to_string(),
        command: "handler_a".to_string(),
        args: vec!["--verbose".to_string()],
        working_dir: Some("/opt/handlers".to_string()),
        reply_timeout_ms: Some(5000),
    };
    assert!(store.add_handler(&hex_record).await.unwrap());

    // 7. list_handlers returns two records.
    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 2);

    // 8. Remove the hex handler.
    assert!(store.remove_handler(HASH_A).await.unwrap());
    assert_eq!(store.list_handlers().await.unwrap().len(), 1);

    // 9. Removing a non-existent handler returns false.
    assert!(!store.remove_handler(HASH_A).await.unwrap());
}

#[tokio::test]
async fn t1400_handler_storage_crud_in_memory() {
    let store = InMemoryStorage::new();

    assert!(store.list_handlers().await.unwrap().is_empty());

    let record = make_record("*", "echo");
    assert!(store.add_handler(&record).await.unwrap());
    assert!(!store.add_handler(&record).await.unwrap());
    assert_eq!(store.list_handlers().await.unwrap().len(), 1);

    let record2 = make_record(HASH_A, "handler_a");
    assert!(store.add_handler(&record2).await.unwrap());
    assert_eq!(store.list_handlers().await.unwrap().len(), 2);

    assert!(store.remove_handler(HASH_A).await.unwrap());
    assert_eq!(store.list_handlers().await.unwrap().len(), 1);
    assert!(!store.remove_handler(HASH_A).await.unwrap());
}

#[tokio::test]
async fn t1400_handler_args_stored_as_json() {
    let store = SqliteStorage::in_memory(test_key()).unwrap();

    let record = HandlerRecord {
        program_hash: "*".to_string(),
        command: "python".to_string(),
        args: vec![
            "--verbose".to_string(),
            "--port".to_string(),
            "8080".to_string(),
        ],
        working_dir: None,
        reply_timeout_ms: None,
    };
    store.add_handler(&record).await.unwrap();

    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers[0].args, vec!["--verbose", "--port", "8080"]);
}

#[tokio::test]
async fn t1400_handler_reply_timeout_round_trip() {
    let store = SqliteStorage::in_memory(test_key()).unwrap();

    let record = HandlerRecord {
        program_hash: "*".to_string(),
        command: "echo".to_string(),
        args: vec![],
        working_dir: None,
        reply_timeout_ms: Some(5000),
    };
    store.add_handler(&record).await.unwrap();

    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers[0].reply_timeout_ms, Some(5000));
}

#[tokio::test]
async fn t1400_handler_working_dir_round_trip() {
    let store = SqliteStorage::in_memory(test_key()).unwrap();

    let record = HandlerRecord {
        program_hash: "*".to_string(),
        command: "echo".to_string(),
        args: vec![],
        working_dir: Some("/opt/work".to_string()),
        reply_timeout_ms: None,
    };
    store.add_handler(&record).await.unwrap();

    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers[0].working_dir, Some("/opt/work".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1401: Handler CRUD via admin API
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1401_handler_crud_via_admin_api() {
    let h = AdminHarness::new_sqlite();

    // 1. ListHandlers returns empty.
    let list = h
        .admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert!(list.handlers.is_empty());

    // 2. AddHandler with reply_timeout_ms.
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "*".to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: Some(5000),
        }))
        .await
        .unwrap();

    // 3. ListHandlers returns one handler with matching fields.
    let list = h
        .admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.handlers.len(), 1);
    assert_eq!(list.handlers[0].program_hash, "*");
    assert_eq!(list.handlers[0].command, "echo");
    assert_eq!(list.handlers[0].reply_timeout_ms, Some(5000));

    // 4. AddHandler with same program_hash returns ALREADY_EXISTS.
    let err = h
        .admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "*".to_string(),
            command: "other".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::AlreadyExists);

    // 5. RemoveHandler succeeds.
    h.admin
        .remove_handler(Request::new(RemoveHandlerRequest {
            program_hash: "*".to_string(),
        }))
        .await
        .unwrap();

    let list = h
        .admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert!(list.handlers.is_empty());

    // 6. RemoveHandler on non-existent returns NOT_FOUND.
    let err = h
        .admin
        .remove_handler(Request::new(RemoveHandlerRequest {
            program_hash: "*".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1402: Handler persistence across restart
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1402_handler_persistence_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("gateway.db");

    // First "startup": add a handler.
    {
        let store = SqliteStorage::open(&db_path, test_key()).unwrap();
        let record = HandlerRecord {
            program_hash: "*".to_string(),
            command: "python".to_string(),
            args: vec!["handler.py".to_string()],
            working_dir: None,
            reply_timeout_ms: None,
        };
        assert!(store.add_handler(&record).await.unwrap());
    }

    // Second "startup": handler should still be there.
    {
        let store = SqliteStorage::open(&db_path, test_key()).unwrap();
        let handlers = store.list_handlers().await.unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].program_hash, "*");
        assert_eq!(handlers[0].command, "python");
        assert_eq!(handlers[0].args, vec!["handler.py"]);
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1405: Bootstrap from YAML file
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1405_bootstrap_from_yaml() {
    use sonde_gateway::handler::load_handler_configs;

    let store = SqliteStorage::in_memory(test_key()).unwrap();

    // Create a YAML file with two handlers.
    let dir = tempfile::tempdir().unwrap();
    let yaml_path = dir.path().join("handlers.yaml");
    let yaml = format!(
        r#"
handlers:
  - program_hash: "*"
    command: "/usr/bin/default"
  - program_hash: "{HASH_A}"
    command: "/usr/bin/handler_a"
    args: ["--verbose"]
"#
    );
    std::fs::write(&yaml_path, yaml).unwrap();

    // Parse and bootstrap.
    let configs = load_handler_configs(&yaml_path).unwrap();
    for cfg in &configs {
        for matcher in &cfg.matchers {
            let program_hash = match matcher {
                ProgramMatcher::Any => "*".to_string(),
                ProgramMatcher::Hash(bytes) => {
                    use std::fmt::Write;
                    let mut s = String::with_capacity(bytes.len() * 2);
                    for b in bytes {
                        let _ = write!(s, "{b:02x}");
                    }
                    s
                }
            };
            let record = HandlerRecord {
                program_hash,
                command: cfg.command.clone(),
                args: cfg.args.clone(),
                working_dir: None,
                reply_timeout_ms: None,
            };
            store.add_handler(&record).await.unwrap();
        }
    }

    // Both handlers should be in the database.
    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 2);

    // Remove one and re-bootstrap — it should be re-imported, and
    // the existing one should not be duplicated.
    store.remove_handler(HASH_A).await.unwrap();
    assert_eq!(store.list_handlers().await.unwrap().len(), 1);

    // Re-bootstrap.
    let configs2 = load_handler_configs(&yaml_path).unwrap();
    for cfg in &configs2 {
        for matcher in &cfg.matchers {
            let program_hash = match matcher {
                ProgramMatcher::Any => "*".to_string(),
                ProgramMatcher::Hash(bytes) => {
                    use std::fmt::Write;
                    let mut s = String::with_capacity(bytes.len() * 2);
                    for b in bytes {
                        let _ = write!(s, "{b:02x}");
                    }
                    s
                }
            };
            let record = HandlerRecord {
                program_hash,
                command: cfg.command.clone(),
                args: cfg.args.clone(),
                working_dir: None,
                reply_timeout_ms: None,
            };
            store.add_handler(&record).await.ok();
        }
    }

    // Both handlers should be present again (re-imported from YAML,
    // catch-all not duplicated).
    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1406: State export/import with handlers
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1406_state_export_import_with_handlers() {
    let h = AdminHarness::new_sqlite();

    // Add two handlers with distinct reply_timeout_ms.
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "*".to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: Some(5000),
        }))
        .await
        .unwrap();

    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: HASH_A.to_string(),
            command: "handler_a".to_string(),
            args: vec!["--mode".to_string(), "test".to_string()],
            working_dir: String::new(),
            reply_timeout_ms: Some(30000),
        }))
        .await
        .unwrap();

    // Export state.
    let exported = h
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "test-pass".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .data;

    // Create a second gateway with different handlers.
    let h2 = AdminHarness::new_sqlite();
    h2.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: HASH_B.to_string(),
            command: "old_handler".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // Import the bundle from gateway A.
    h2.admin
        .import_state(Request::new(ImportStateRequest {
            data: exported,
            passphrase: "test-pass".to_string(),
        }))
        .await
        .unwrap();

    // ListHandlers on gateway B should have exactly the two handlers from A.
    let list = h2
        .admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.handlers.len(), 2);

    // Verify program_hash values.
    let hashes: Vec<&str> = list
        .handlers
        .iter()
        .map(|h| h.program_hash.as_str())
        .collect();
    assert!(hashes.contains(&"*"));
    assert!(hashes.contains(&HASH_A));

    // Verify reply_timeout_ms round-tripped.
    let catch_all = list
        .handlers
        .iter()
        .find(|h| h.program_hash == "*")
        .unwrap();
    assert_eq!(catch_all.reply_timeout_ms, Some(5000));
    let hash_a = list
        .handlers
        .iter()
        .find(|h| h.program_hash == HASH_A)
        .unwrap();
    assert_eq!(hash_a.reply_timeout_ms, Some(30000));
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1406a: State import — backwards compatibility
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1406a_state_import_backwards_compatibility() {
    let h = AdminHarness::new_sqlite();

    // Add two handlers to the target gateway.
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "*".to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: HASH_A.to_string(),
            command: "handler".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // Export state from a "legacy" gateway with no handlers.
    let legacy = AdminHarness::new_sqlite();
    let exported = legacy
        .admin
        .export_state(Request::new(ExportStateRequest {
            passphrase: "test-pass".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .data;

    // Import the legacy bundle.
    h.admin
        .import_state(Request::new(ImportStateRequest {
            data: exported,
            passphrase: "test-pass".to_string(),
        }))
        .await
        .unwrap();

    // The two pre-existing handlers should be preserved.
    let list = h
        .admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.handlers.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1407: Handler add — program_hash validation
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1407_handler_add_program_hash_validation() {
    let h = AdminHarness::new_sqlite();

    // 1. Invalid string.
    let err = h
        .admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "invalid".to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // 2. Too short.
    let err = h
        .admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "AABB".to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // 3. Valid 64-char hex — succeeds.
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: HASH_A.to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // 4. Wildcard — succeeds.
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "*".to_string(),
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // 5. Empty command — rejected.
    let err = h
        .admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: HASH_B.to_string(),
            command: String::new(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1407b: program_hash case normalization
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1407_program_hash_case_normalization() {
    let h = AdminHarness::new_sqlite();

    // Add with uppercase hex.
    let upper = HASH_A.to_uppercase();
    h.admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: upper,
            command: "echo".to_string(),
            args: vec![],
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // ListHandlers returns lowercase.
    let list = h
        .admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.handlers.len(), 1);
    assert_eq!(list.handlers[0].program_hash, HASH_A);
}

// ═══════════════════════════════════════════════════════════════════════
//  Additional: replace_handlers atomicity
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_replace_handlers() {
    let store = SqliteStorage::in_memory(test_key()).unwrap();

    // Add initial handlers.
    store.add_handler(&make_record("*", "old")).await.unwrap();
    store
        .add_handler(&make_record(HASH_A, "old_a"))
        .await
        .unwrap();
    assert_eq!(store.list_handlers().await.unwrap().len(), 2);

    // Replace with a new set.
    let new_records = vec![make_record(HASH_B, "new_b")];
    store.replace_handlers(&new_records).await.unwrap();

    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].program_hash, HASH_B);
    assert_eq!(handlers[0].command, "new_b");
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1406: State bundle handler config round-trip through CBOR
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn t1406_handler_working_dir_in_bundle() {
    use sonde_gateway::state_bundle::{decrypt_state_full, encrypt_state_full};

    // Create handler configs with working_dir.
    let configs = vec![
        HandlerConfig {
            matchers: vec![ProgramMatcher::Any],
            command: "echo".to_string(),
            args: vec![],
            reply_timeout: Some(Duration::from_millis(5000)),
            working_dir: Some("/opt/handlers".to_string()),
        },
        HandlerConfig {
            matchers: vec![ProgramMatcher::Hash(vec![0x42u8; 32])],
            command: "handler".to_string(),
            args: vec!["--flag".to_string()],
            reply_timeout: None,
            working_dir: None,
        },
    ];

    let bundle = encrypt_state_full(&[], &[], None, &[], &configs, "test-pass").unwrap();
    let (_, _, _, _, decoded_configs) = decrypt_state_full(&bundle, "test-pass").unwrap();

    assert_eq!(decoded_configs.len(), 2);

    // Catch-all handler should have working_dir.
    let catch_all = decoded_configs
        .iter()
        .find(|c| matches!(c.matchers[0], ProgramMatcher::Any))
        .unwrap();
    assert_eq!(catch_all.working_dir, Some("/opt/handlers".to_string()));
    assert_eq!(catch_all.reply_timeout, Some(Duration::from_millis(5000)));

    // Hash handler should not have working_dir.
    let hash_handler = decoded_configs
        .iter()
        .find(|c| matches!(c.matchers[0], ProgramMatcher::Hash(_)))
        .unwrap();
    assert!(hash_handler.working_dir.is_none());
}

// ═══════════════════════════════════════════════════════════════════════
//  Handler list ordering
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_handler_list_ordered_by_program_hash() {
    let store = SqliteStorage::in_memory(test_key()).unwrap();

    // Insert in reverse order.
    store.add_handler(&make_record(HASH_B, "b")).await.unwrap();
    store.add_handler(&make_record("*", "star")).await.unwrap();
    store.add_handler(&make_record(HASH_A, "a")).await.unwrap();

    let handlers = store.list_handlers().await.unwrap();
    assert_eq!(handlers.len(), 3);
    // "*" < "a1..." < "cc..."
    assert_eq!(handlers[0].program_hash, "*");
    assert_eq!(handlers[1].program_hash, HASH_A);
    assert_eq!(handlers[2].program_hash, HASH_B);
}
