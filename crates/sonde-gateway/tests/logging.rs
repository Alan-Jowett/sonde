// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

// Some imports are only used by the debug-gated modem frame test.
#![allow(unused_imports, dead_code)]

//! Operational logging tests (GW-1300, GW-1302).
//!
//! Validates that the gateway emits the structured tracing events specified
//! by the operational logging requirements.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::{Gateway, PendingCommand};
use sonde_gateway::gateway_identity::GatewayIdentity;
use sonde_gateway::phone_trust::{PhonePskRecord, PhonePskStatus};
use sonde_gateway::program::{ProgramLibrary, VerificationProfile};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::session::SessionManager;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::{PeerAddress, Transport};

use sonde_gateway::GatewayAead;
#[cfg(debug_assertions)]
use sonde_protocol::modem::{encode_modem_frame, FrameDecoder, ModemMessage, RecvFrame};
use sonde_protocol::{
    encode_frame, FrameHeader, GatewayMessage, NodeMessage, Sha256Provider, MSG_APP_DATA,
    MSG_PEER_REQUEST, MSG_WAKE, PEER_REQ_KEY_PAYLOAD,
};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};

use tracing_test::traced_test;

// ── Test helpers ───────────────────────────────────────────────────────

const TEST_NODE_PSK: [u8; 32] = [0x42u8; 32];
const TEST_PHONE_PSK: [u8; 32] = [0x55u8; 32];

fn compute_key_hint(psk: &[u8; 32]) -> u16 {
    let crypto_sha = sonde_gateway::crypto::RustCryptoSha256;
    let h = crypto_sha.hash(psk);
    u16::from_be_bytes([h[30], h[31]])
}

fn make_gateway(storage: Arc<InMemoryStorage>) -> Gateway {
    Gateway::new(storage, Duration::from_secs(30))
}

async fn store_test_program(storage: &InMemoryStorage, bytecode: &[u8]) -> Vec<u8> {
    let lib = ProgramLibrary::new();
    let image = sonde_protocol::ProgramImage {
        bytecode: bytecode.to_vec(),
        maps: vec![],
        map_initial_data: vec![],
    };
    let cbor = image.encode_deterministic().unwrap();
    let record = lib
        .ingest_unverified(cbor, VerificationProfile::Resident)
        .unwrap();
    let hash = record.hash.clone();
    storage.store_program(&record).await.unwrap();
    hash
}

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

    fn peer_address(&self) -> PeerAddress {
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

// ── PEER_REQUEST helpers (adapted from peer_request.rs) ────────────────

/// Encrypt PairingRequest CBOR with phone_psk using AES-256-GCM.
/// Returns: inner_nonce(12) ‖ ciphertext ‖ tag(16)
/// AAD = "sonde-pairing-v2"
fn encrypt_inner_payload(pairing_cbor: &[u8], phone_psk: &[u8; 32]) -> Vec<u8> {
    const PAIRING_AAD: &[u8] = b"sonde-pairing-v2";

    let cipher = Aes256Gcm::new_from_slice(phone_psk).unwrap();
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).unwrap();
    let nonce = GcmNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: pairing_cbor,
                aad: PAIRING_AAD,
            },
        )
        .unwrap();

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    out
}

fn build_peer_request(
    _identity: &GatewayIdentity,
    node_id: &str,
    node_psk: &[u8; 32],
    rf_channel: u8,
    phone_psk: &[u8; 32],
) -> Vec<u8> {
    let node_key_hint = compute_key_hint(node_psk);
    let phone_key_hint = compute_key_hint(phone_psk);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Inner pairing CBOR (keys match build_pairing_cbor in peer_request.rs).
    let cbor = ciborium::Value::Map(vec![
        (
            ciborium::Value::Integer(1.into()),
            ciborium::Value::Text(node_id.to_string()),
        ),
        (
            ciborium::Value::Integer(2.into()),
            ciborium::Value::Integer(node_key_hint.into()),
        ),
        (
            ciborium::Value::Integer(3.into()),
            ciborium::Value::Bytes(node_psk.to_vec()),
        ),
        (
            ciborium::Value::Integer(4.into()),
            ciborium::Value::Integer(rf_channel.into()),
        ),
        (
            ciborium::Value::Integer(6.into()),
            ciborium::Value::Integer(ts.into()),
        ),
    ]);
    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(&cbor, &mut cbor_bytes).unwrap();

    // Encrypt inner payload with phone_psk.
    let encrypted_payload = encrypt_inner_payload(&cbor_bytes, phone_psk);

    // Outer CBOR: { 1: encrypted_payload }
    let outer = ciborium::Value::Map(vec![(
        ciborium::Value::Integer(PEER_REQ_KEY_PAYLOAD.into()),
        ciborium::Value::Bytes(encrypted_payload),
    )]);
    let mut outer_buf = Vec::new();
    ciborium::into_writer(&outer, &mut outer_buf).unwrap();

    let header = FrameHeader {
        key_hint: phone_key_hint,
        msg_type: MSG_PEER_REQUEST,
        nonce: 0x1234567890ABCDEF,
    };

    encode_frame(
        &header,
        &outer_buf,
        phone_psk,
        &GatewayAead,
        &RustCryptoSha256,
    )
    .unwrap()
}

// ── T-1300  WAKE lifecycle logging ─────────────────────────────────────

/// T-1300: Validates GW-1300 AC3 (WAKE received), AC5 (session created),
/// and AC4 (COMMAND selected).
#[tokio::test]
#[traced_test]
async fn t1300_wake_lifecycle_logging() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway(storage.clone());

    let node = TestNode::new("node-log-1300", 0x1300, [0x13u8; 32]);
    let program_hash = store_test_program(&storage, b"test-bytecode").await;
    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    let frame = node.build_wake(100, 1, &program_hash, 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_some(), "expected COMMAND response");

    // GW-1300 AC3: WAKE received with node_id, seq, battery_mv.
    assert!(logs_contain("WAKE received"));
    assert!(logs_contain("node_id=node-log-1300"));
    assert!(logs_contain("seq="));
    assert!(logs_contain("battery_mv=3300"));

    // GW-1300 AC5: session created with node_id.
    assert!(logs_contain("session created"));
    assert!(logs_contain("node_id=node-log-1300"));

    // GW-1300 AC4: COMMAND selected with node_id and command_type.
    assert!(logs_contain("COMMAND selected"));
    assert!(logs_contain("node_id=node-log-1300"));
    assert!(logs_contain(r#"command_type="Nop""#));
}

// ── T-1301  Session expiry logging ─────────────────────────────────────

/// T-1301: Validates GW-1300 AC6 (session expired).
#[tokio::test(flavor = "current_thread", start_paused = true)]
#[traced_test]
async fn t1301_session_expiry_logging() {
    let session_manager = Arc::new(SessionManager::new(Duration::from_millis(1)));

    // Create a session (clock is already paused via start_paused = true).
    session_manager
        .create_session("node-log-1301".to_string(), b"peer".to_vec(), 1, 100)
        .await;

    // Advance past the session timeout so it expires deterministically.
    tokio::time::advance(Duration::from_millis(10)).await;

    let expired = session_manager.reap_expired().await;
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0], "node-log-1301");

    // GW-1300 AC6: session expired with node_id.
    assert!(logs_contain("session expired"));
    assert!(logs_contain("node-log-1301"));
}

// ── T-1302  PEER_REQUEST logging ───────────────────────────────────────

/// T-1302: Validates GW-1300 AC1 (PEER_REQUEST processed) and AC2 (PEER_ACK
/// frame encoded).
#[tokio::test]
#[traced_test]
async fn t1302_peer_request_logging() {
    let storage = Arc::new(InMemoryStorage::new());
    let identity = GatewayIdentity::generate().unwrap();
    storage.store_gateway_identity(&identity).await.unwrap();

    let crypto_sha = sonde_gateway::crypto::RustCryptoSha256;
    let phone_psk_hash = crypto_sha.hash(&TEST_PHONE_PSK);
    let phone_key_hint = u16::from_be_bytes([phone_psk_hash[30], phone_psk_hash[31]]);
    let phone_record = PhonePskRecord {
        phone_id: 0,
        phone_key_hint,
        psk: Zeroizing::new(TEST_PHONE_PSK),
        label: "test-phone".into(),
        issued_at: std::time::SystemTime::now(),
        status: PhonePskStatus::Active,
    };
    storage.store_phone_psk(&phone_record).await.unwrap();

    let session_manager = Arc::new(SessionManager::new(Duration::from_secs(30)));
    let pending: Arc<RwLock<HashMap<String, Vec<PendingCommand>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let gw = Gateway::new_with_pending(storage.clone(), pending, session_manager);

    let frame = build_peer_request(
        &identity,
        "node-peer-log",
        &TEST_NODE_PSK,
        7,
        &TEST_PHONE_PSK,
    );
    let peer: PeerAddress = b"peer-addr".to_vec();

    let resp = gw.process_frame(&frame, peer).await;
    assert!(resp.is_some(), "expected PEER_ACK response");

    // GW-1300 AC1: PEER_REQUEST processed with result "registered" and key_hint.
    assert!(logs_contain("PEER_REQUEST (AEAD) processed"));
    assert!(logs_contain(r#"result="registered""#));
    assert!(logs_contain("node_id=node-peer-log"));
    let node_key_hint = compute_key_hint(&TEST_NODE_PSK);
    let expected_key_hint_field = format!("key_hint={node_key_hint}");
    assert!(logs_contain(&expected_key_hint_field));

    // GW-1300 AC2: PEER_ACK frame encoded with node_id.
    assert!(logs_contain("PEER_ACK (AEAD) frame encoded"));
    assert!(logs_contain("node_id=node-peer-log"));
}

// ── T-1303  Modem frame debug logging ──────────────────────────────────

/// T-1303: Validates GW-1302 AC1 (recv frame debug log) and AC2 (send
/// frame debug log).
///
/// Skipped in release builds — `debug!()` call-sites are stripped at compile
/// time by `release_max_level_info`, so the log assertions cannot pass.
#[cfg(debug_assertions)]
#[tokio::test]
#[traced_test]
async fn t1303_modem_frame_debug_logging() {
    let (transport, mut server) = common::create_transport_and_server(6).await;

    // AC1: Inject RECV_FRAME and assert debug log.
    let frame_data = vec![
        0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let peer_mac = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let recv = ModemMessage::RecvFrame(RecvFrame {
        peer_mac,
        rssi: -50,
        frame_data: frame_data.clone(),
    });
    server
        .write_all(&encode_modem_frame(&recv).unwrap())
        .await
        .unwrap();

    let (_data, _peer) = transport.recv().await.unwrap();
    assert!(
        logs_contain("frame received from modem"),
        "expected RECV debug log"
    );
    assert!(logs_contain(r#"msg_type="WAKE""#));
    assert!(logs_contain("peer_mac=[17, 34, 51, 68, 85, 102]"));
    assert!(logs_contain("len=11"));

    // AC2: Send a frame and assert debug log.
    let send_frame = vec![
        0x00, 0x01, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let send_peer = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    transport.send(&send_frame, &send_peer).await.unwrap();

    // Read the sent message from the mock server side to avoid blocking.
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];
    let _msg = common::read_modem_msg(&mut server, &mut decoder, &mut buf).await;

    assert!(
        logs_contain("frame sent to modem"),
        "expected SEND debug log"
    );
    assert!(logs_contain(r#"msg_type="COMMAND""#));
    assert!(logs_contain("peer_mac=[170, 187, 204, 221, 238, 255]"));
    assert!(logs_contain("len=11"));
}

// ── T-1308  APP_DATA handler pipeline logging ──────────────────────────

/// Find Python 3 executable name.
fn python_cmd() -> &'static str {
    static CACHED: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    CACHED.get_or_init(|| {
        if cfg!(windows) {
            for cmd in &["py", "python3", "python"] {
                if let Ok(output) = std::process::Command::new(cmd)
                    .args(if *cmd == "py" {
                        vec!["-3", "--version"]
                    } else {
                        vec!["--version"]
                    })
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if stdout.starts_with("Python 3") {
                            return cmd;
                        }
                    }
                }
            }
            "py"
        } else {
            "python3"
        }
    })
}

/// Arguments to pass before the script path to ensure Python 3.
fn python_args() -> Vec<&'static str> {
    if cfg!(windows) && python_cmd() == "py" {
        vec!["-3"]
    } else {
        vec![]
    }
}

/// Check if Python 3 is available. Returns false if not installed or not Python 3.
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

macro_rules! require_python {
    () => {
        if !python_available() {
            panic!(
                "Python 3 not available: required for this integration test. \
                 Install Python 3, run tests without the `python-tests` feature \
                 (omit `--features python-tests`), or skip this test via \
                 `cargo test -- --skip <test-name>`."
            );
        }
    };
}

/// Echo handler script for pipeline logging test.
const ECHO_HANDLER_LOGGING_PY: &str = r#"
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
    data = read_exact(length)
    return data

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
    break

request_id = msg[2]
payload_data = msg[5]

reply = encode_cbor_map([
    (1, 0x81),
    (2, request_id),
    (3, payload_data),
])
write_msg(reply)
"#;

/// T-1308: Validates GW-1308 — APP_DATA handler pipeline logging.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
#[traced_test]
async fn t1308_app_data_handler_pipeline_logging() {
    use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
    use std::io::Write;

    require_python!();

    // Write echo handler script.
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("echo_log.py");
    {
        let mut f = std::fs::File::create(&script_path).unwrap();
        f.write_all(ECHO_HANDLER_LOGGING_PY.as_bytes()).unwrap();
        f.flush().unwrap();
    }

    let cmd = python_cmd();
    let mut args: Vec<String> = python_args().iter().map(|s| s.to_string()).collect();
    args.push("-u".to_string());
    args.push(script_path.to_string_lossy().into_owned());

    let program_hash = vec![
        0xAEu8, 0xC3, 0x69, 0xB1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];

    let config = HandlerConfig {
        matchers: vec![ProgramMatcher::Hash(program_hash.clone())],
        command: cmd.to_string(),
        args,
        reply_timeout: None,
        working_dir: None,
    };
    let router = Arc::new(HandlerRouter::new(vec![config]));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = Gateway::new_with_handler(storage.clone(), Duration::from_secs(30), router);

    let node = TestNode::new("node-log-1308", 0x1308, [0x37u8; 32]);
    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.clone());
    record.current_program_hash = Some(program_hash.clone());
    storage.upsert_node(&record).await.unwrap();

    // WAKE to establish session.
    let frame = node.build_wake(5000, 1, &program_hash, 3300);
    let resp = gw.process_frame(&frame, node.peer_address()).await;
    assert!(resp.is_some(), "expected COMMAND response");

    let (_hdr, msg) = decode_command(&resp.unwrap(), &node.psk);
    let starting_seq = match msg {
        GatewayMessage::Command { starting_seq, .. } => starting_seq,
        other => panic!("expected Command, got {:?}", other),
    };

    // Send APP_DATA.
    let blob = vec![0x01, 0x02, 0x03];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("expected APP_DATA_REPLY");

    // Poll for the "handler exited" log instead of fixed sleep, which is
    // flaky on slow CI runners. Each iteration sends a second APP_DATA to
    // trigger ensure_running() → try_wait(), which detects that the prior
    // single-shot handler has exited and emits the "handler exited" log
    // (GW-1308 AC5).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let app_frame2 = node.build_app_data(starting_seq + 1, &blob);
        let _ = gw.process_frame(&app_frame2, node.peer_address()).await;
        if logs_contain("handler exited") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "expected 'handler exited' log within 5 seconds — \
                 handler process may not have exited in time"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // GW-1308 AC1: APP_DATA received with node_id, program_hash, len.
    assert!(
        logs_contain("APP_DATA received"),
        "expected APP_DATA received log"
    );
    assert!(logs_contain("node_id=node-log-1308"));
    assert!(logs_contain("aec369b1"));
    assert!(logs_contain("len=3"));

    // GW-1308 AC2: handler matched with program_hash and command (structured fields).
    assert!(
        logs_contain("handler matched"),
        "expected handler matched log"
    );
    assert!(
        logs_contain("program_hash="),
        "expected handler matched log to include program_hash field"
    );
    assert!(
        logs_contain("command="),
        "expected handler matched log to include command field"
    );

    // GW-1308 AC3: handler invoked with command (structured field).
    assert!(
        logs_contain("handler invoked"),
        "expected handler invoked log"
    );
    assert!(
        logs_contain(&format!("command={}", cmd)),
        "expected handler invoked log with structured command field"
    );

    // GW-1308 AC4: handler replied with len (structured field).
    assert!(
        logs_contain("handler replied"),
        "expected handler replied log"
    );
    assert!(
        logs_contain(&format!("len={}", blob.len())),
        "expected handler replied log with structured len field"
    );

    // GW-1308 AC5: handler exited with code (structured field).
    assert!(
        logs_contain("handler exited"),
        "expected handler exited log"
    );
    assert!(
        logs_contain("code="),
        "expected handler exited log to include code field"
    );

    // Verify the reply was correct.
    let decoded = sonde_protocol::decode_frame(&resp).unwrap();
    let plaintext =
        sonde_protocol::open_frame(&decoded, &node.psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let reply_msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
    match reply_msg {
        GatewayMessage::AppDataReply { blob: reply_blob } => {
            assert_eq!(reply_blob, blob);
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

fn decode_command(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = sonde_protocol::decode_frame(raw).unwrap();
    let plaintext =
        sonde_protocol::open_frame(&decoded, psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
    (decoded.header, msg)
}
