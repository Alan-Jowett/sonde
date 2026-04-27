// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Handler configuration management tests (T-1400 through T-1407).
//!
//! Validates GW-1401 (storage), GW-1402 (admin API), GW-1405 (bootstrap),
//! GW-1406 (state export/import), and input validation.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tonic::Request;
use tracing_test::traced_test;
use zeroize::Zeroizing;

use sonde_gateway::admin::pb::gateway_admin_server::GatewayAdmin;
use sonde_gateway::admin::pb::*;
use sonde_gateway::admin::AdminService;
use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::sqlite_storage::SqliteStorage;
use sonde_gateway::storage::{HandlerRecord, InMemoryStorage, Storage};
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, FrameHeader, GatewayMessage, NodeMessage, MSG_APP_DATA,
    MSG_APP_DATA_REPLY, MSG_WAKE,
};

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

// ─── Protocol test helpers (T-1403/T-1404) ─────────────────────────────

struct TestNode {
    node_id: String,
    key_hint: u16,
    psk: [u8; 32],
}

impl TestNode {
    fn new(node_id: &str, key_hint: u16, psk: [u8; 32]) -> Self {
        Self {
            node_id: node_id.to_string(),
            key_hint,
            psk,
        }
    }

    fn to_record(&self) -> NodeRecord {
        NodeRecord::new(self.node_id.clone(), self.key_hint, self.psk)
    }

    fn peer_address(&self) -> Vec<u8> {
        self.node_id.as_bytes().to_vec()
    }

    fn build_wake(
        &self,
        nonce: u64,
        firmware_abi_version: u32,
        program_hash: &[u8],
        battery_mv: u32,
    ) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_WAKE,
            nonce,
        };
        let msg = NodeMessage::Wake {
            firmware_abi_version,
            program_hash: program_hash.to_vec(),
            battery_mv,
            firmware_version: "0.5.0".into(),
            blob: None,
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }

    fn build_app_data(&self, seq: u64, blob: &[u8]) -> Vec<u8> {
        let header = FrameHeader {
            key_hint: self.key_hint,
            msg_type: MSG_APP_DATA,
            nonce: seq,
        };
        let msg = NodeMessage::AppData {
            blob: blob.to_vec(),
        };
        let cbor = msg.encode().unwrap();
        encode_frame(&header, &cbor, &self.psk, &GatewayAead, &RustCryptoSha256).unwrap()
    }
}

fn decode_response(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = decode_frame(raw).unwrap();
    let plaintext = open_frame(&decoded, psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
    (decoded.header, msg)
}

/// Send a WAKE and return the `starting_seq` from the COMMAND response.
async fn do_wake(gw: &Gateway, node: &TestNode, nonce: u64, program_hash: &[u8]) -> u64 {
    let frame = node.build_wake(nonce, 1, program_hash, 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");
    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::Command { starting_seq, .. } => starting_seq,
        other => panic!("expected Command, got {:?}", other),
    }
}

/// Register a node with both assigned and current program hash set.
async fn setup_node_with_program(storage: &InMemoryStorage, node: &TestNode, program_hash: &[u8]) {
    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.to_vec());
    record.current_program_hash = Some(program_hash.to_vec());
    storage.upsert_node(&record).await.unwrap();
}

// ─── Python handler helpers (T-1403/T-1404) ────────────────────────────

macro_rules! require_python {
    () => {
        if !python_available() {
            if cfg!(feature = "python-tests") {
                panic!("Python 3 not found but `python-tests` feature is enabled; failing instead of skipping tests");
            } else {
                eprintln!("SKIPPING: Python 3 not found");
                return;
            }
        }
    };
}

fn python_cmd() -> &'static str {
    if cfg!(windows) {
        "py"
    } else {
        "python3"
    }
}

fn python_args() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["-3"]
    } else {
        vec![]
    }
}

fn python_available() -> bool {
    let mut cmd = std::process::Command::new(python_cmd());
    for arg in python_args() {
        cmd.arg(arg);
    }
    match cmd
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(output) => {
            if !output.status.success() {
                return false;
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            stdout.starts_with("Python 3") || stderr.starts_with("Python 3")
        }
        Err(_) => false,
    }
}

fn write_handler_script(dir: &std::path::Path, name: &str, content: &str) -> String {
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(content.as_bytes()).unwrap();
    path.to_str().unwrap().to_string()
}

/// Convert a [`HandlerRecord`] from storage into a [`HandlerConfig`] that
/// launches the given Python script. Used by live-reload tests to derive
/// the handler router from the shared storage rather than manually
/// constructing configs from test data.
fn handler_record_to_test_config(r: HandlerRecord, script: &str) -> Option<HandlerConfig> {
    let matcher = if r.program_hash == "*" {
        ProgramMatcher::Any
    } else {
        if r.program_hash.len() != 64
            || !r.program_hash.is_ascii()
            || !r.program_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        let bytes: Vec<u8> = (0..r.program_hash.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&r.program_hash[i..i + 2], 16).unwrap())
            .collect();
        ProgramMatcher::Hash(bytes)
    };
    // Use the Python wrapper command/args so the handler actually runs.
    let mut args: Vec<String> = python_args().iter().map(|s| s.to_string()).collect();
    args.push(script.to_string());
    Some(HandlerConfig {
        matchers: vec![matcher],
        command: python_cmd().to_string(),
        args,
        reply_timeout: r
            .reply_timeout_ms
            .filter(|&ms| ms > 0)
            .map(Duration::from_millis),
        working_dir: r.working_dir,
    })
}

/// Minimal Python echo handler that loops, reading DATA messages (skipping
/// EVENTs) and sending back a DATA_REPLY with identical payload. The loop
/// keeps the process alive until stdin is closed, which is required for
/// T-1404 to meaningfully test handler process lifetime on removal.
const ECHO_HANDLER_PY: &str = r#"
import sys, struct

def read_exact(n):
    buf = bytearray()
    while len(buf) < n:
        chunk = sys.stdin.buffer.read(n - len(buf))
        if not chunk:
            sys.exit(0)
        buf.extend(chunk)
    return bytes(buf)

def read_msg():
    raw = read_exact(4)
    length = struct.unpack('>I', raw)[0]
    return read_exact(length)

def write_msg(payload):
    sys.stdout.buffer.write(struct.pack('>I', len(payload)))
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()

def decode_cbor_map(data):
    result = {}
    idx = 0
    if data[idx] & 0xe0 != 0xa0 and data[idx] != 0xbf:
        raise ValueError(f"expected map, got {data[idx]:#x}")
    if data[idx] == 0xbf:
        idx += 1
        while data[idx] != 0xff:
            k, idx = decode_item(data, idx)
            v, idx = decode_item(data, idx)
            result[k] = v
        idx += 1
    else:
        count, idx = decode_uint(data[idx] & 0x1f, data, idx + 1)
        for _ in range(count):
            k, idx = decode_item(data, idx)
            v, idx = decode_item(data, idx)
            result[k] = v
    return result

def decode_item(data, idx):
    major = (data[idx] >> 5) & 0x07
    info = data[idx] & 0x1f
    idx += 1
    if major == 0:
        val, idx = decode_uint(info, data, idx)
        return val, idx
    elif major == 2:
        length, idx = decode_uint(info, data, idx)
        return data[idx:idx+length], idx + length
    elif major == 3:
        length, idx = decode_uint(info, data, idx)
        return data[idx:idx+length].decode('utf-8'), idx + length
    elif major == 5:
        count, idx = decode_uint(info, data, idx)
        m = {}
        for _ in range(count):
            k, idx = decode_item(data, idx)
            v, idx = decode_item(data, idx)
            m[k] = v
        return m, idx
    else:
        raise ValueError(f"unsupported major type {major}")

def decode_uint(info, data, idx):
    if info < 24:
        return info, idx
    elif info == 24:
        return data[idx], idx + 1
    elif info == 25:
        return struct.unpack('>H', data[idx:idx+2])[0], idx + 2
    elif info == 26:
        return struct.unpack('>I', data[idx:idx+4])[0], idx + 4
    elif info == 27:
        return struct.unpack('>Q', data[idx:idx+8])[0], idx + 8
    else:
        raise ValueError(f"unsupported additional info {info}")

def encode_uint(major, val):
    major_bits = major << 5
    if val < 24:
        return bytes([major_bits | val])
    elif val < 256:
        return bytes([major_bits | 24, val])
    elif val < 65536:
        return bytes([major_bits | 25]) + struct.pack('>H', val)
    elif val < 2**32:
        return bytes([major_bits | 26]) + struct.pack('>I', val)
    else:
        return bytes([major_bits | 27]) + struct.pack('>Q', val)

def encode_cbor_map(pairs):
    out = encode_uint(5, len(pairs))
    for k, v in pairs:
        out += encode_item(k)
        out += encode_item(v)
    return out

def encode_item(val):
    if isinstance(val, int):
        return encode_uint(0, val)
    elif isinstance(val, bytes):
        return encode_uint(2, len(val)) + val
    elif isinstance(val, str):
        encoded = val.encode('utf-8')
        return encode_uint(3, len(encoded)) + encoded
    else:
        raise ValueError(f"unsupported type {type(val)}")

while True:
    cbor_data = read_msg()
    msg = decode_cbor_map(cbor_data)
    if msg[1] == 2:
        continue
    request_id = msg[2]
    payload_data = msg[5]
    reply = encode_cbor_map([
        (1, 0x81),
        (2, request_id),
        (3, payload_data),
    ])
    write_msg(reply)
"#;

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
            store.add_handler(&record).await.unwrap();
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

// ═══════════════════════════════════════════════════════════════════════
//  T-1403: Handler live-reload — add
// ═══════════════════════════════════════════════════════════════════════

#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t1403_handler_live_reload_add() {
    require_python!();

    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    let program_hash = vec![0x14u8; 32];
    let program_hash_hex: String = program_hash.iter().map(|b| format!("{b:02x}")).collect();

    // Shared infrastructure between admin and gateway.
    let mem_storage = Arc::new(InMemoryStorage::new());
    let storage: Arc<dyn Storage> = mem_storage.clone();
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let sm = Arc::new(SessionManager::new(Duration::from_secs(30)));

    // Register a node with a known program hash.
    let node = TestNode::new("node-1403", 0x1403, [0x43u8; 32]);
    setup_node_with_program(&mem_storage, &node, &program_hash).await;

    // Gateway sharing state with admin — initially empty handler router.
    let handler_router = Arc::new(RwLock::new(HandlerRouter::new(Vec::new())));
    let gw = Gateway::new_with_pending(
        storage.clone(),
        pending.clone(),
        sm.clone(),
        handler_router.clone(),
    );

    // 1. Empty handler router → APP_DATA produces no reply.
    let seq = do_wake(&gw, &node, 1000, &program_hash).await;
    let resp = gw
        .process_frame(
            &node.build_app_data(seq, &[0x01, 0x02, 0x03]),
            node.peer_address(),
        )
        .await;
    assert!(resp.is_none(), "no handler configured → no APP_DATA_REPLY");

    // 2. Add handler via admin API (shared storage).
    let admin = AdminService::new(storage.clone(), pending, sm);

    let mut cmd_args: Vec<String> = python_args().iter().map(|s| s.to_string()).collect();
    cmd_args.push(script.clone());

    admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: program_hash_hex.clone(),
            command: python_cmd().to_string(),
            args: cmd_args,
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // Verify the handler appears in storage.
    let list = admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.handlers.len(), 1);
    assert_eq!(list.handlers[0].program_hash, program_hash_hex);

    // 3. Build router from storage records and install on the *same* gateway.
    let records = storage.list_handlers().await.unwrap();
    let configs: Vec<HandlerConfig> = records
        .into_iter()
        .filter_map(|r| handler_record_to_test_config(r, &script))
        .collect();
    assert_eq!(configs.len(), 1, "storage should contain one handler");
    {
        let mut w = handler_router.write().await;
        *w = HandlerRouter::new(configs);
    }

    // 4. Same gateway now routes APP_DATA after reload.
    let seq2 = do_wake(&gw, &node, 2000, &program_hash).await;
    let blob = vec![0x01, 0x02, 0x03];
    let resp2 = gw
        .process_frame(&node.build_app_data(seq2, &blob), node.peer_address())
        .await
        .expect("handler added → APP_DATA_REPLY expected");

    let (hdr, msg) = decode_response(&resp2, &node.psk);
    assert_eq!(hdr.msg_type, MSG_APP_DATA_REPLY);
    match msg {
        GatewayMessage::AppDataReply { blob: reply } => {
            assert_eq!(reply, blob, "handler must echo the data back");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1404: Handler live-reload — remove
// ═══════════════════════════════════════════════════════════════════════

#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t1404_handler_live_reload_remove() {
    require_python!();

    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    let program_hash = vec![0x15u8; 32];

    // Shared infrastructure between admin and gateway.
    let mem_storage = Arc::new(InMemoryStorage::new());
    let storage: Arc<dyn Storage> = mem_storage.clone();
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let sm = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage.clone(), pending.clone(), sm.clone());

    // Pre-add a catch-all handler via admin API.
    let mut cmd_args: Vec<String> = python_args().iter().map(|s| s.to_string()).collect();
    cmd_args.push(script.clone());

    admin
        .add_handler(Request::new(AddHandlerRequest {
            program_hash: "*".to_string(),
            command: python_cmd().to_string(),
            args: cmd_args,
            working_dir: String::new(),
            reply_timeout_ms: None,
        }))
        .await
        .unwrap();

    // Register a node.
    let node = TestNode::new("node-1404", 0x1404, [0x44u8; 32]);
    setup_node_with_program(&mem_storage, &node, &program_hash).await;

    // 1. Build router from storage records, install on gateway.
    let records = storage.list_handlers().await.unwrap();
    let configs: Vec<HandlerConfig> = records
        .into_iter()
        .filter_map(|r| handler_record_to_test_config(r, &script))
        .collect();
    assert_eq!(configs.len(), 1, "storage should contain one handler");
    let router = Arc::new(RwLock::new(HandlerRouter::new(configs)));
    let gw = Gateway::new_with_pending(storage.clone(), pending, sm, router);

    let seq = do_wake(&gw, &node, 3000, &program_hash).await;
    let blob = vec![0xAA, 0xBB];
    let resp = gw
        .process_frame(&node.build_app_data(seq, &blob), node.peer_address())
        .await
        .expect("catch-all handler → APP_DATA_REPLY expected");

    let (hdr, msg) = decode_response(&resp, &node.psk);
    assert_eq!(hdr.msg_type, MSG_APP_DATA_REPLY);
    match msg {
        GatewayMessage::AppDataReply { blob: reply } => {
            assert_eq!(reply, blob, "handler must echo the data back");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }

    // Drop gateway to tear down handler processes before removal.
    drop(gw);

    // 2. Remove handler via admin API.
    admin
        .remove_handler(Request::new(RemoveHandlerRequest {
            program_hash: "*".to_string(),
        }))
        .await
        .unwrap();

    let list = admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert!(
        list.handlers.is_empty(),
        "handler removed → list should be empty"
    );

    // 3. Verify storage-driven reload yields no handlers.
    //    HandlerRouter is immutable, so a reload rebuilds the router from
    //    the current storage state (which is now empty).
    let post_records = storage.list_handlers().await.unwrap();
    assert!(
        post_records.is_empty(),
        "storage should be empty after remove"
    );
    let empty_router = Arc::new(RwLock::new(HandlerRouter::new(Vec::new())));
    let gw2 = Gateway::new_with_pending(
        storage,
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(SessionManager::new(Duration::from_secs(30))),
        empty_router,
    );
    let seq2 = do_wake(&gw2, &node, 4000, &program_hash).await;
    let resp2 = gw2
        .process_frame(&node.build_app_data(seq2, &[0xCC]), node.peer_address())
        .await;
    assert!(resp2.is_none(), "handler removed → no APP_DATA_REPLY");
}

// ═══════════════════════════════════════════════════════════════════════
//  T-1405a: Bootstrap with invalid YAML entry
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
#[traced_test]
async fn t1405a_bootstrap_invalid_yaml_entry() {
    use sonde_gateway::handler::load_handler_configs;

    // Create a YAML file with one valid entry and one invalid entry.
    let dir = tempfile::tempdir().unwrap();
    let yaml_path = dir.path().join("handlers.yaml");
    let yaml = format!(
        r#"
handlers:
  - program_hash: "{HASH_A}"
    command: "/usr/bin/valid_handler"
  - program_hash: "not-a-hex-string"
    command: "/usr/bin/invalid_handler"
"#
    );
    std::fs::write(&yaml_path, yaml).unwrap();

    // load_handler_configs succeeds, skipping the invalid entry.
    let configs = load_handler_configs(&yaml_path).unwrap();
    assert_eq!(configs.len(), 1, "only valid entry should be imported");
    assert_eq!(configs[0].command, "/usr/bin/valid_handler");

    // Warning must be logged for the invalid entry.
    assert!(logs_contain("skipping invalid handler entry"));
    assert!(logs_contain("not-a-hex-string"));

    // Import valid configs into storage (simulating gateway bootstrap).
    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory(test_key()).unwrap());
    for cfg in &configs {
        for matcher in &cfg.matchers {
            let program_hash = match matcher {
                ProgramMatcher::Any => "*".to_string(),
                ProgramMatcher::Hash(bytes) => {
                    use std::fmt::Write as _;
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
            storage.add_handler(&record).await.unwrap();
        }
    }

    // ListHandlers via admin API shows only the valid entry.
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let sm = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let admin = AdminService::new(storage, pending, sm);

    let list = admin
        .list_handlers(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.handlers.len(), 1);
    assert_eq!(list.handlers[0].program_hash, HASH_A);
    assert_eq!(list.handlers[0].command, "/usr/bin/valid_handler");
}

// ═══════════════════════════════════════════════════════════════════════
//  T-0514: Oversized handler message rejection
// ═══════════════════════════════════════════════════════════════════════

/// T-0514: `read_message` rejects frames whose 4-byte length header declares
/// a body larger than MAX_MESSAGE_SIZE (1 MiB), preventing memory exhaustion
/// from a misbehaving handler process.
#[tokio::test]
async fn t0514_oversized_handler_message_rejected() {
    use sonde_gateway::handler::read_message;

    const MAX_MESSAGE_SIZE: u32 = 1_048_576;
    // Length header one byte over the limit; no body needed — the reader
    // must reject before attempting to allocate or read the body.
    let data = (MAX_MESSAGE_SIZE + 1).to_be_bytes().to_vec();
    let mut reader = std::io::Cursor::new(data);

    let result = read_message(&mut reader).await;
    assert!(result.is_err(), "oversized message must be rejected");
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::InvalidData,
        "rejection must use InvalidData error kind"
    );
}

/// T-0514b: `read_message` accepts a well-formed message and rejects messages
/// with a length prefix exceeding MAX_MESSAGE_SIZE.
#[tokio::test]
async fn t0514b_handler_message_size_boundary() {
    use sonde_gateway::handler::{read_message, HandlerMessage};

    // Build a valid LOG message frame manually: 4-byte BE length + CBOR body.
    let msg = HandlerMessage::Log {
        level: "info".to_string(),
        message: "test".to_string(),
    };
    let cbor = msg.encode().unwrap();
    let mut framed = (cbor.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(&cbor);
    let mut reader = std::io::Cursor::new(framed);
    let decoded = read_message(&mut reader)
        .await
        .expect("valid message must decode");
    assert_eq!(decoded, msg);

    // A frame with length == MAX_MESSAGE_SIZE + 1 must be rejected.
    const MAX_MESSAGE_SIZE: u32 = 1_048_576;
    let data = (MAX_MESSAGE_SIZE + 1).to_be_bytes().to_vec();
    let mut reader2 = std::io::Cursor::new(data);
    let result = read_message(&mut reader2).await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
}
