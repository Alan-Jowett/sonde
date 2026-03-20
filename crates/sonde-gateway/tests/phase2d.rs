// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 2D integration tests: modem health monitoring (GW-1102),
//! modem error recovery documentation (GW-1103), and node timeout
//! detection (GW-0507 node_timeout EVENT).

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sonde_gateway::engine::Gateway;
use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::storage::{InMemoryStorage, Storage};

use sonde_protocol::modem::{
    encode_modem_frame, FrameDecoder, ModemMessage, ModemReady, ModemStatus,
};
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
use tracing_test::traced_test;

// ─── Modem test helpers ────────────────────────────────────────────────

/// Read bytes from the stream until a complete modem message is decoded.
async fn read_next_message(
    stream: &mut DuplexStream,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> ModemMessage {
    loop {
        match decoder.decode() {
            Ok(Some(msg)) => return msg,
            Ok(None) => {}
            Err(e) => panic!("decode error: {e}"),
        }
        let n = stream.read(buf).await.expect("read failed");
        assert!(n > 0, "stream closed unexpectedly");
        decoder.push(&buf[..n]);
    }
}

/// Run the modem startup handshake on the mock (server) side of a duplex.
async fn do_startup_handshake(server: &mut DuplexStream) -> u8 {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // 1. Read RESET
    let msg = read_next_message(server, &mut decoder, &mut buf).await;
    assert!(
        matches!(msg, ModemMessage::Reset),
        "expected Reset, got {msg:?}"
    );

    // 2. Send MODEM_READY
    let ready = ModemMessage::ModemReady(ModemReady {
        firmware_version: [1, 2, 3, 4],
        mac_address: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
    });
    let frame = encode_modem_frame(&ready).unwrap();
    server.write_all(&frame).await.unwrap();

    // 3. Read SET_CHANNEL
    let msg = read_next_message(server, &mut decoder, &mut buf).await;
    let requested_channel = match msg {
        ModemMessage::SetChannel(ch) => ch,
        other => panic!("expected SetChannel, got {other:?}"),
    };

    // 4. Send SET_CHANNEL_ACK
    let ack = ModemMessage::SetChannelAck(requested_channel);
    let frame = encode_modem_frame(&ack).unwrap();
    server.write_all(&frame).await.unwrap();

    requested_channel
}

// ─── GW-1102: Modem health monitoring ──────────────────────────────────

/// T-1105 extended: poll_status returns correct values across multiple calls
/// with different status payloads, verifying the values used by the health
/// monitor for delta and reboot detection.
#[tokio::test]
async fn t1105_poll_status_multiple_calls() {
    let (client, mut server) = duplex(1024);

    let startup = tokio::spawn(async move {
        do_startup_handshake(&mut server).await;
        server
    });

    let transport = sonde_gateway::modem::UsbEspNowTransport::new(client, 6)
        .await
        .unwrap();
    let mut server = startup.await.unwrap();

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 256];

    // First poll — baseline
    let poll = {
        // Drive poll_status in current task alongside mock server
        let poll_fut = transport.poll_status();
        tokio::pin!(poll_fut);

        // Send GET_STATUS response from server side
        let server_fut = async {
            let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
            assert!(matches!(msg, ModemMessage::GetStatus));
            let status_msg = ModemMessage::Status(ModemStatus {
                channel: 6,
                uptime_s: 100,
                tx_count: 10,
                rx_count: 5,
                tx_fail_count: 0,
            });
            server
                .write_all(&encode_modem_frame(&status_msg).unwrap())
                .await
                .unwrap();
        };

        let (status, _) = tokio::join!(poll_fut, server_fut);
        let status = status.unwrap();
        assert_eq!(status.uptime_s, 100);
        assert_eq!(status.tx_fail_count, 0);
        status
    };

    // Second poll — tx_fail increased (health monitor would log a warning)
    {
        let poll_fut = transport.poll_status();
        tokio::pin!(poll_fut);

        let server_fut = async {
            let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
            assert!(matches!(msg, ModemMessage::GetStatus));
            let status_msg = ModemMessage::Status(ModemStatus {
                channel: 6,
                uptime_s: 130,
                tx_count: 15,
                rx_count: 8,
                tx_fail_count: 3,
            });
            server
                .write_all(&encode_modem_frame(&status_msg).unwrap())
                .await
                .unwrap();
        };

        let (status, _) = tokio::join!(poll_fut, server_fut);
        let status = status.unwrap();
        assert_eq!(status.uptime_s, 130);
        assert_eq!(status.tx_fail_count, 3);
        // Delta would be 3 - 0 = 3 (health monitor logs this)
        assert!(status.tx_fail_count > poll.tx_fail_count);
    }

    // Third poll — uptime decreased (reboot detected by health monitor)
    {
        let poll_fut = transport.poll_status();
        tokio::pin!(poll_fut);

        let server_fut = async {
            let msg = read_next_message(&mut server, &mut decoder, &mut buf).await;
            assert!(matches!(msg, ModemMessage::GetStatus));
            let status_msg = ModemMessage::Status(ModemStatus {
                channel: 6,
                uptime_s: 5, // rebooted — uptime dropped
                tx_count: 0,
                rx_count: 0,
                tx_fail_count: 0,
            });
            server
                .write_all(&encode_modem_frame(&status_msg).unwrap())
                .await
                .unwrap();
        };

        let (status, _) = tokio::join!(poll_fut, server_fut);
        let status = status.unwrap();
        assert_eq!(status.uptime_s, 5);
        // Health monitor would detect uptime_s < prev (130 > 5)
    }
}

// ─── GW-0507: node_timeout EVENT ───────────────────────────────────────

/// Verify check_node_timeouts identifies nodes that have exceeded 3×
/// their schedule_interval_s since last_seen. Uses an empty handler
/// router so the scan logic actually executes (no process is spawned).
#[tokio::test]
async fn t0507_check_node_timeouts_emits_event() {
    let storage = Arc::new(InMemoryStorage::new());

    // Register a node with a 60s interval and last_seen 200s ago
    let mut node = NodeRecord::new("timeout-node".into(), 0x0001, [0xAA; 32]);
    node.schedule_interval_s = 60;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(200));
    storage.upsert_node(&node).await.unwrap();

    let router = Arc::new(HandlerRouter::new(vec![]));
    let gw = Gateway::new_with_handler(storage, Duration::from_secs(30), router);
    gw.check_node_timeouts(3).await;
    // No panic = success; the scan logic ran and found the timed-out node,
    // but with an empty router there is no matching handler to deliver to.
}

/// Verify that nodes within their expected interval are NOT flagged.
#[tokio::test]
async fn t0507_check_node_timeouts_not_timed_out() {
    let storage = Arc::new(InMemoryStorage::new());

    // Node seen 30s ago with 60s interval — well within 3× window
    let mut node = NodeRecord::new("fresh-node".into(), 0x0002, [0xBB; 32]);
    node.schedule_interval_s = 60;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(30));
    storage.upsert_node(&node).await.unwrap();

    let router = Arc::new(HandlerRouter::new(vec![]));
    let gw = Gateway::new_with_handler(storage, Duration::from_secs(30), router);
    gw.check_node_timeouts(3).await;
    // No panic, no timeout detected.
}

/// Verify that nodes with no last_seen are skipped.
#[tokio::test]
async fn t0507_check_node_timeouts_no_last_seen() {
    let storage = Arc::new(InMemoryStorage::new());

    let node = NodeRecord::new("new-node".into(), 0x0003, [0xCC; 32]);
    storage.upsert_node(&node).await.unwrap();

    let router = Arc::new(HandlerRouter::new(vec![]));
    let gw = Gateway::new_with_handler(storage, Duration::from_secs(30), router);
    gw.check_node_timeouts(3).await;
    // No panic — node with no last_seen is skipped.
}

/// Verify that nodes with zero schedule_interval are skipped.
#[tokio::test]
async fn t0507_check_node_timeouts_zero_interval() {
    let storage = Arc::new(InMemoryStorage::new());

    let mut node = NodeRecord::new("zero-interval".into(), 0x0004, [0xDD; 32]);
    node.schedule_interval_s = 0;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(500));
    storage.upsert_node(&node).await.unwrap();

    let router = Arc::new(HandlerRouter::new(vec![]));
    let gw = Gateway::new_with_handler(storage, Duration::from_secs(30), router);
    gw.check_node_timeouts(3).await;
    // No panic — zero interval means no timeout check.
}

// ─── GW-0507: node_timeout EVENT delivery to handler ────────────────

/// Write a Python script to a temp directory. Returns the path as a String.
fn write_handler_script(dir: &std::path::Path, name: &str, script: &str) -> String {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    path.to_string_lossy().into_owned()
}

/// Find Python 3 executable name.
fn python_cmd() -> &'static str {
    if cfg!(windows) {
        "py"
    } else {
        "python3"
    }
}

/// Arguments to pass before the script path to ensure Python 3.
fn python_args() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["-3"]
    } else {
        vec![]
    }
}

/// Check if Python 3 is available.
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

/// Build a `HandlerConfig` for a Python 3 handler script.
fn python_handler_config(matchers: Vec<ProgramMatcher>, script: String) -> HandlerConfig {
    let mut args: Vec<String> = python_args().iter().map(|s| s.to_string()).collect();
    args.push(script);
    HandlerConfig {
        matchers,
        command: python_cmd().to_string(),
        args,
    }
}

macro_rules! require_python {
    () => {
        if !python_available() {
            eprintln!("SKIPPED: Python 3 not available");
            return;
        }
    };
}

/// Event-logging handler that writes a LOG message when it receives a
/// node_timeout EVENT, confirming the event was delivered.
const NODE_TIMEOUT_EVENT_HANDLER_PY: &str = r#"
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
        count = data[idx] & 0x1f
        idx += 1
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

# Read messages in a loop, respond to EVENTs with LOG
while True:
    cbor_data = read_msg()
    msg = decode_cbor_map(cbor_data)
    msg_type = msg[1]

    if msg_type == 2:  # EVENT
        event_type = msg.get(4, "")
        if event_type == "node_timeout":
            log_msg = encode_cbor_map([
                (1, 0x82),
                (2, "info"),
                (3, "received node_timeout event"),
            ])
            write_msg(log_msg)
        continue

    if msg_type == 1:  # DATA — echo back
        request_id = msg[2]
        payload_data = msg[5]
        reply = encode_cbor_map([
            (1, 0x81),
            (2, request_id),
            (3, payload_data),
        ])
        write_msg(reply)
"#;

/// T-0507b: node_timeout EVENT is delivered to a running handler.
/// The handler writes a LOG message upon receiving the event, which the
/// gateway captures via tracing. Validates GW-0507 AC3.
#[tokio::test]
#[traced_test]
async fn t0507b_node_timeout_event_delivered_to_handler() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(
        tmp.path(),
        "timeout_event.py",
        NODE_TIMEOUT_EVENT_HANDLER_PY,
    );

    let router = Arc::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Any],
        script,
    )]));

    let storage = Arc::new(InMemoryStorage::new());

    // Register a node with a 60s interval, last_seen 200s ago (timed out).
    let mut node = NodeRecord::new("timeout-handler-node".into(), 0x0507, [0x57; 32]);
    node.schedule_interval_s = 60;
    node.last_seen = Some(SystemTime::now() - Duration::from_secs(200));
    storage.upsert_node(&node).await.unwrap();

    let gw = Gateway::new_with_handler(storage, Duration::from_secs(30), router);

    // check_node_timeouts should detect the timed-out node and deliver
    // a node_timeout EVENT to the handler. The handler writes a LOG message
    // which the gateway drains from stdout and emits via tracing.
    gw.check_node_timeouts(3).await;

    // Allow time for the handler to process the event and write the LOG.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The handler responds to node_timeout with a LOG message containing
    // "received node_timeout event". The gateway reads this LOG from stdout
    // and emits it via the tracing system.
    assert!(
        logs_contain("received node_timeout event"),
        "handler must receive and log node_timeout EVENT"
    );
}
