// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Phase 2C-i integration tests: handler router wiring, APP_DATA dispatch,
//! APP_DATA_REPLY framing, and handler lifecycle.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use sonde_gateway::crypto::RustCryptoSha256;
use sonde_gateway::engine::Gateway;
use sonde_gateway::handler::{HandlerConfig, HandlerRouter, ProgramMatcher};
use sonde_gateway::registry::NodeRecord;
use sonde_gateway::storage::{InMemoryStorage, Storage};
use sonde_gateway::transport::PeerAddress;
use sonde_gateway::GatewayAead;

use sonde_protocol::{
    decode_frame, encode_frame, open_frame, CommandPayload, FrameHeader, GatewayMessage,
    NodeMessage, MSG_APP_DATA, MSG_APP_DATA_REPLY, MSG_WAKE,
};

// ─── Test helpers ──────────────────────────────────────────────────────

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

fn decode_response(raw: &[u8], psk: &[u8; 32]) -> (FrameHeader, GatewayMessage) {
    let decoded = decode_frame(raw).unwrap();
    let plaintext = open_frame(&decoded, psk, &GatewayAead, &RustCryptoSha256).unwrap();
    let msg = GatewayMessage::decode(decoded.header.msg_type, &plaintext).unwrap();
    (decoded.header, msg)
}

/// Send a WAKE and return `(starting_seq, timestamp_ms, CommandPayload)`.
async fn do_wake(
    gw: &Gateway,
    node: &TestNode,
    nonce: u64,
    program_hash: &[u8],
) -> (u64, u64, CommandPayload) {
    let frame = node.build_wake(nonce, 1, program_hash, 3300);
    let resp = gw
        .process_frame(&frame, node.peer_address())
        .await
        .expect("expected COMMAND response");
    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::Command {
            starting_seq,
            timestamp_ms,
            payload,
        } => (starting_seq, timestamp_ms, payload),
        other => panic!("expected Command, got {:?}", other),
    }
}

fn make_gateway_with_handler(
    storage: Arc<InMemoryStorage>,
    handler_router: Arc<tokio::sync::RwLock<HandlerRouter>>,
) -> Gateway {
    Gateway::new_with_handler(storage, Duration::from_secs(30), handler_router)
}

/// Register a node, assign + confirm a program so `current_program_hash` is set.
async fn setup_node_with_program(storage: &InMemoryStorage, node: &TestNode, program_hash: &[u8]) {
    let mut record = node.to_record();
    record.assigned_program_hash = Some(program_hash.to_vec());
    record.current_program_hash = Some(program_hash.to_vec());
    storage.upsert_node(&record).await.unwrap();
}

// ─── Python handler scripts ───────────────────────────────────────────

/// Write a Python script to a temp directory. Returns the path as a String.
fn write_handler_script(dir: &std::path::Path, name: &str, script: &str) -> String {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    path.to_string_lossy().into_owned()
}

/// Echo handler: reads one DATA message, replies with DATA_REPLY containing
/// the same data. Uses the raw CBOR integer-key map format.
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
    data = read_exact(length)
    return data

def write_msg(payload):
    sys.stdout.buffer.write(struct.pack('>I', len(payload)))
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()

def decode_cbor_map(data):
    """Minimal CBOR map decoder for handler protocol messages."""
    result = {}
    idx = 0
    if data[idx] & 0xe0 != 0xa0 and data[idx] != 0xbf:
        raise ValueError(f"expected map, got {data[idx]:#x}")
    if data[idx] == 0xbf:
        # Indefinite-length map
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
    if major == 0:  # unsigned int
        val, idx = decode_uint(info, data, idx)
        return val, idx
    elif major == 2:  # byte string
        length, idx = decode_uint(info, data, idx)
        return data[idx:idx+length], idx + length
    elif major == 3:  # text string
        length, idx = decode_uint(info, data, idx)
        return data[idx:idx+length].decode('utf-8'), idx + length
    elif major == 5:  # map
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
    """Encode list of (key, value) pairs as a CBOR map."""
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

# Read messages, skipping EVENTs, until a DATA message arrives
while True:
    cbor_data = read_msg()
    msg = decode_cbor_map(cbor_data)
    if msg[1] == 2:  # EVENT — no reply expected
        continue
    break

# msg_type=1 is DATA, extract request_id (key 2) and data (key 5)
request_id = msg[2]
payload_data = msg[5]

# Build DATA_REPLY: {1: 0x81, 2: request_id, 3: payload_data}
reply = encode_cbor_map([
    (1, 0x81),       # msg_type = DATA_REPLY
    (2, request_id), # request_id
    (3, payload_data),  # data (echo back)
])
write_msg(reply)
"#;

/// Empty-reply handler: reads one DATA, replies with DATA_REPLY with empty data.
const EMPTY_REPLY_HANDLER_PY: &str = r#"
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
    if msg[1] == 2:  # EVENT — no reply expected
        continue
    break
request_id = msg[2]

# Reply with empty data
reply = encode_cbor_map([
    (1, 0x81),
    (2, request_id),
    (3, b""),  # empty data
])
write_msg(reply)
"#;

/// Crash handler: exits immediately with code 1.
const CRASH_HANDLER_PY: &str = r#"
import sys
sys.exit(1)
"#;

/// Multi-echo handler: reads messages in a loop, echoes each one.
const MULTI_ECHO_HANDLER_PY: &str = r#"
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
    msg_type = msg[1]
    if msg_type == 1:  # DATA
        request_id = msg[2]
        payload_data = msg[5]
        reply = encode_cbor_map([
            (1, 0x81),
            (2, request_id),
            (3, payload_data),
        ])
        write_msg(reply)
    elif msg_type == 2:  # EVENT — no reply expected
        pass
"#;

/// Fixed-reply handler: reads one DATA, replies with fixed bytes [0xAA, 0xBB].
const FIXED_REPLY_HANDLER_PY: &str = r#"
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
    if msg[1] == 2:  # EVENT — no reply expected
        continue
    break
request_id = msg[2]

reply = encode_cbor_map([
    (1, 0x81),
    (2, request_id),
    (3, b"\xaa\xbb"),
])
write_msg(reply)
"#;

/// Log-then-reply handler: reads DATA, writes LOG, then DATA_REPLY.
const LOG_HANDLER_PY: &str = r#"
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
    if msg[1] == 2:  # EVENT — no reply expected
        continue
    break
request_id = msg[2]
payload_data = msg[5]

# Write LOG first: {1: 0x82, 2: "debug", 3: "processing data"}
log_msg = encode_cbor_map([
    (1, 0x82),
    (2, "debug"),
    (3, "processing data"),
])
write_msg(log_msg)

# Then DATA_REPLY
reply = encode_cbor_map([
    (1, 0x81),
    (2, request_id),
    (3, payload_data),
])
write_msg(reply)
"#;

/// Wrong-request-id handler: reads DATA, replies with DATA_REPLY but with
/// a different `request_id`.
const WRONG_REQUEST_ID_HANDLER_PY: &str = r#"
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
    if msg[1] == 2:  # EVENT — no reply expected
        continue
    break
request_id = msg[2]
payload_data = msg[5]

# Reply with wrong request_id (original + 999)
reply = encode_cbor_map([
    (1, 0x81),
    (2, request_id + 999),
    (3, payload_data),
])
write_msg(reply)
"#;

/// Stateful counter handler: maintains a counter across messages, incrementing
/// with each DATA received. The counter value is the first byte of the reply.
/// If the handler is respawned, the counter resets to 0, proving state loss.
const STATEFUL_COUNTER_HANDLER_PY: &str = r#"
import sys, struct

counter = 0

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
    if msg[1] == 2:  # EVENT — skip
        continue
    break
    # Found a DATA message
request_id = msg[2]

counter += 1

reply = encode_cbor_map([
    (1, 0x81),
    (2, request_id),
    (3, bytes([counter])),
])
write_msg(reply)

# Continue reading more DATA messages
while True:
    cbor_data = read_msg()
    msg = decode_cbor_map(cbor_data)
    if msg[1] == 2:  # EVENT — skip
        continue
    request_id = msg[2]
    counter += 1
    reply = encode_cbor_map([
        (1, 0x81),
        (2, request_id),
        (3, bytes([counter])),
    ])
    write_msg(reply)
"#;

/// Hang handler: reads one DATA message, then sleeps forever without replying.
/// Used to test handler reply timeout (GW-0503 AC2/AC3).
const HANG_HANDLER_PY: &str = r#"
import sys, struct, time

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

# Read one DATA message (skip EVENTs)
while True:
    cbor_data = read_msg()
    msg = decode_cbor_map(cbor_data)
    if msg[1] == 2:  # EVENT — skip
        continue
    break

# Hang forever — never send a reply
time.sleep(3600)
"#;

/// Find Python 3 executable name.
fn python_cmd() -> &'static str {
    if cfg!(windows) {
        // `py -3` is the reliable way to invoke Python 3 on Windows
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
            // `python --version` may print to stdout or stderr depending on version/platform
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
        reply_timeout: None,
        working_dir: None,
    }
}

/// Skip the test if Python 3 is not available.
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

// ═══════════════════════════════════════════════════════════════════════
//  T-05xx: Phase 2C Handler Integration Tests
// ═══════════════════════════════════════════════════════════════════════

/// T-0500: APP_DATA reception and echo forwarding via handler.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0500_app_data_echo_forwarding() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    let program_hash = vec![0x10; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-500", 0x0500, [0x50; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    // WAKE to establish session
    let (starting_seq, _, _) = do_wake(&gw, &node, 1000, &program_hash).await;

    // Send APP_DATA
    let blob = vec![0x01, 0x02, 0x03];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("expected APP_DATA_REPLY");

    let (hdr, msg) = decode_response(&resp, &node.psk);
    assert_eq!(hdr.msg_type, MSG_APP_DATA_REPLY);
    assert_eq!(hdr.nonce, starting_seq, "reply nonce must echo request seq");
    match msg {
        GatewayMessage::AppDataReply { blob: reply_blob } => {
            assert_eq!(reply_blob, blob, "handler must echo the data back");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0501: APP_DATA_REPLY with fixed non-zero data [0xAA, 0xBB].
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0501_app_data_reply_fixed_data() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "fixed.py", FIXED_REPLY_HANDLER_PY);

    let program_hash = vec![0x11; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-501", 0x0501, [0x51; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 2000, &program_hash).await;

    let app_frame = node.build_app_data(starting_seq, &[0xFF]);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("expected APP_DATA_REPLY");

    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::AppDataReply { blob } => {
            assert_eq!(blob, vec![0xAA, 0xBB]);
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0502: APP_DATA_REPLY suppressed on empty handler reply.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0502_empty_reply_suppressed() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "empty.py", EMPTY_REPLY_HANDLER_PY);

    let program_hash = vec![0x12; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-502", 0x0502, [0x52; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 3000, &program_hash).await;

    let app_frame = node.build_app_data(starting_seq, &[0x01]);
    let resp = gw.process_frame(&app_frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "empty handler reply must produce no response"
    );
}

/// T-0503: Multiple APP_DATA per wake cycle (persistent handler).
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0503_multiple_app_data_per_wake() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "multi.py", MULTI_ECHO_HANDLER_PY);

    let program_hash = vec![0x13; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-503", 0x0503, [0x53; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 4000, &program_hash).await;

    for i in 0u64..3 {
        let seq = starting_seq + i;
        let blob = vec![(i + 1) as u8; 4];
        let app_frame = node.build_app_data(seq, &blob);
        let resp = gw
            .process_frame(&app_frame, node.peer_address())
            .await
            .unwrap_or_else(|| panic!("expected reply for APP_DATA #{i}"));

        let (hdr, msg) = decode_response(&resp, &node.psk);
        assert_eq!(hdr.msg_type, MSG_APP_DATA_REPLY);
        match msg {
            GatewayMessage::AppDataReply { blob: reply_blob } => {
                assert_eq!(reply_blob, blob, "echo mismatch on message #{i}");
            }
            other => panic!("expected AppDataReply, got {:?}", other),
        }
    }
}

/// T-0504: Handler transport framing roundtrip (integration-level).
/// Verifies the gateway correctly encodes DATA with all fields and the handler
/// can decode+reply via the 4-byte length-prefix framing.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0504_handler_transport_framing() {
    require_python!();
    // Covered by T-0500 echo test — the echo handler decodes the full DATA
    // message and echoes the `data` field, proving framing works end-to-end.
    // This test exercises larger payloads.
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    let program_hash = vec![0x14; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-504", 0x0504, [0x54; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 5000, &program_hash).await;

    // Use a payload with enough variety to test CBOR encoding
    let blob = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("expected APP_DATA_REPLY");

    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::AppDataReply { blob: reply_blob } => {
            assert_eq!(reply_blob, blob, "framing roundtrip must preserve data");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0505: Handler respawn after clean exit.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0505_handler_respawn_on_clean_exit() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    // Use single-shot echo handler — it processes one message then exits(0).
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    let program_hash = vec![0x15; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-505", 0x0505, [0x55; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 6000, &program_hash).await;

    // First APP_DATA — handler processes then exits
    let blob1 = vec![0x01];
    let app1 = node.build_app_data(starting_seq, &blob1);
    let resp1 = gw
        .process_frame(&app1, node.peer_address())
        .await
        .expect("first APP_DATA must get reply");
    let (_, msg1) = decode_response(&resp1, &node.psk);
    match msg1 {
        GatewayMessage::AppDataReply { blob } => assert_eq!(blob, blob1),
        other => panic!("expected AppDataReply, got {:?}", other),
    }

    // Give the handler process time to exit so `try_wait()` detects it
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Second APP_DATA — handler must respawn
    let blob2 = vec![0x02];
    let app2 = node.build_app_data(starting_seq + 1, &blob2);
    let resp2 = gw
        .process_frame(&app2, node.peer_address())
        .await
        .expect("second APP_DATA must get reply (handler respawned)");
    let (_, msg2) = decode_response(&resp2, &node.psk);
    match msg2 {
        GatewayMessage::AppDataReply { blob } => assert_eq!(blob, blob2),
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0506: Handler crash — no reply sent to node.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0506_handler_crash_no_reply() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "crash.py", CRASH_HANDLER_PY);

    let program_hash = vec![0x16; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-506", 0x0506, [0x56; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 7000, &program_hash).await;

    let app_frame = node.build_app_data(starting_seq, &[0x01]);
    let resp = gw.process_frame(&app_frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "crashed handler must not produce a response"
    );
}

/// T-0507: Handler routing by program hash — two handlers, two programs.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0507_routing_by_program_hash() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let echo_script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);
    let fixed_script = write_handler_script(tmp.path(), "fixed.py", FIXED_REPLY_HANDLER_PY);

    let hash_a = vec![0xA0; 32];
    let hash_b = vec![0xB0; 32];

    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![
        python_handler_config(vec![ProgramMatcher::Hash(hash_a.clone())], echo_script),
        python_handler_config(vec![ProgramMatcher::Hash(hash_b.clone())], fixed_script),
    ])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    // Node A uses hash_a (echo handler)
    let node_a = TestNode::new("node-507a", 0x5070, [0xA1; 32]);
    setup_node_with_program(&storage, &node_a, &hash_a).await;
    let (seq_a, _, _) = do_wake(&gw, &node_a, 8000, &hash_a).await;

    let blob_a = vec![0xDE, 0xAD];
    let app_a = node_a.build_app_data(seq_a, &blob_a);
    let resp_a = gw
        .process_frame(&app_a, node_a.peer_address())
        .await
        .expect("node A must get echo reply");
    let (_, msg_a) = decode_response(&resp_a, &node_a.psk);
    match msg_a {
        GatewayMessage::AppDataReply { blob } => {
            assert_eq!(blob, blob_a, "echo handler must echo data");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }

    // Node B uses hash_b (fixed handler → [0xAA, 0xBB])
    let node_b = TestNode::new("node-507b", 0x5071, [0xB1; 32]);
    setup_node_with_program(&storage, &node_b, &hash_b).await;
    let (seq_b, _, _) = do_wake(&gw, &node_b, 9000, &hash_b).await;

    let app_b = node_b.build_app_data(seq_b, &[0xFF]);
    let resp_b = gw
        .process_frame(&app_b, node_b.peer_address())
        .await
        .expect("node B must get fixed reply");
    let (_, msg_b) = decode_response(&resp_b, &node_b.psk);
    match msg_b {
        GatewayMessage::AppDataReply { blob } => {
            assert_eq!(
                blob,
                vec![0xAA, 0xBB],
                "fixed handler must reply [0xAA, 0xBB]"
            );
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0508: No matching handler — no reply, no crash.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0508_no_handler_match_no_reply() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    // Handler only matches hash [0xAA; 32]
    let handler_hash = vec![0xAA; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(handler_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    // Node uses a different hash
    let node_hash = vec![0xBB; 32];
    let node = TestNode::new("node-508", 0x0508, [0x58; 32]);
    setup_node_with_program(&storage, &node, &node_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 10000, &node_hash).await;

    let app_frame = node.build_app_data(starting_seq, &[0x01]);
    let resp = gw.process_frame(&app_frame, node.peer_address()).await;
    assert!(resp.is_none(), "no matching handler must produce no reply");
}

/// T-0509: Catch-all handler (`ProgramMatcher::Any`).
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0509_catch_all_handler() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "echo.py", ECHO_HANDLER_PY);

    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Any],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let any_hash = vec![0xCC; 32];
    let node = TestNode::new("node-509", 0x0509, [0x59; 32]);
    setup_node_with_program(&storage, &node, &any_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 11000, &any_hash).await;

    let blob = vec![0xCA, 0xFE];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("catch-all handler must produce reply");

    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::AppDataReply { blob: reply_blob } => {
            assert_eq!(reply_blob, blob, "catch-all must echo data");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0510: `request_id` correlation — sequential APP_DATA use incrementing
/// nonces; each reply uses the correct nonce.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0510_request_id_correlation() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "multi.py", MULTI_ECHO_HANDLER_PY);

    let program_hash = vec![0x17; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-510", 0x0510, [0x5A; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 12000, &program_hash).await;

    // Send two APP_DATA with sequential seq numbers
    for i in 0u64..2 {
        let seq = starting_seq + i;
        let blob = vec![(0x10 + i) as u8];
        let app_frame = node.build_app_data(seq, &blob);
        let resp = gw
            .process_frame(&app_frame, node.peer_address())
            .await
            .expect("expected reply");

        let (hdr, _msg) = decode_response(&resp, &node.psk);
        assert_eq!(
            hdr.nonce, seq,
            "reply nonce must match request seq for message #{i}"
        );
    }
}

/// T-0511: Handler replies with wrong `request_id` — reply discarded.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0511_request_id_mismatch_discarded() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "wrong_id.py", WRONG_REQUEST_ID_HANDLER_PY);

    let program_hash = vec![0x18; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-511", 0x0511, [0x5B; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 13000, &program_hash).await;

    let app_frame = node.build_app_data(starting_seq, &[0x01]);
    let resp = gw.process_frame(&app_frame, node.peer_address()).await;
    assert!(resp.is_none(), "mismatched request_id must suppress reply");
}

/// T-0512: WAKE + APP_DATA with handler — smoke test for handler routing.
/// Verifies that the gateway can run WAKE and post-WAKE APP_DATA with a
/// configured handler without panicking, and that APP_DATA still works.
/// (EVENT forwarding from engine to handler is not wired in Phase 2C-i.)
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0512_handler_no_crash_on_wake() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "multi.py", MULTI_ECHO_HANDLER_PY);

    let program_hash = vec![0x19; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Any],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-512", 0x0512, [0x5C; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    // WAKE should succeed even with a handler configured
    let (starting_seq, _, payload) = do_wake(&gw, &node, 14000, &program_hash).await;
    assert!(matches!(payload, CommandPayload::Nop));

    // Follow-up APP_DATA still works
    let blob = vec![0x01];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("post-WAKE APP_DATA must still work");
    let (_hdr, msg) = decode_response(&resp, &node.psk);
    assert!(matches!(msg, GatewayMessage::AppDataReply { .. }));
}

/// T-0513: LOG messages from handler do not crash the gateway.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0513_log_messages_no_crash() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "log.py", LOG_HANDLER_PY);

    let program_hash = vec![0x1A; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-513", 0x0513, [0x5D; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 15000, &program_hash).await;

    let blob = vec![0xBE, 0xEF];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let resp = gw
        .process_frame(&app_frame, node.peer_address())
        .await
        .expect("handler with LOG then DATA_REPLY must produce reply");

    let (_hdr, msg) = decode_response(&resp, &node.psk);
    match msg {
        GatewayMessage::AppDataReply { blob: reply_blob } => {
            assert_eq!(reply_blob, blob, "data after LOG must still echo correctly");
        }
        other => panic!("expected AppDataReply, got {:?}", other),
    }
}

/// T-0503b: Long-running handler persists across multiple messages without
/// respawn. The handler maintains a counter that increments with each DATA
/// message. If the handler were respawned, the counter would reset.
/// Validates GW-0503 acceptance criteria 1 (handler persistence).
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0503b_handler_persistence_across_messages() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();

    // Stateful counter handler: maintains a counter across messages.
    // Each reply includes the counter value as the first byte.
    let script = write_handler_script(tmp.path(), "counter.py", STATEFUL_COUNTER_HANDLER_PY);

    let program_hash = vec![0x53; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-503b", 0x53B0, [0x5B; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 5300, &program_hash).await;

    // Send 5 messages — each reply should contain an incrementing counter.
    for i in 0u64..5 {
        let seq = starting_seq + i;
        let blob = vec![0xAA];
        let app_frame = node.build_app_data(seq, &blob);
        let resp = gw
            .process_frame(&app_frame, node.peer_address())
            .await
            .unwrap_or_else(|| panic!("expected reply for APP_DATA #{i}"));

        let (_hdr, msg) = decode_response(&resp, &node.psk);
        match msg {
            GatewayMessage::AppDataReply { blob: reply_blob } => {
                assert!(!reply_blob.is_empty(), "reply {i} must not be empty");
                assert_eq!(
                    reply_blob[0],
                    (i + 1) as u8,
                    "handler counter on message #{i} must be {} (handler was respawned if it reset)",
                    i + 1
                );
            }
            other => panic!("expected AppDataReply, got {:?}", other),
        }
    }
}

/// T-0503c: Handler that hangs indefinitely — gateway must time out and return
/// no reply. Validates GW-0503 AC2/AC3 (timeout kills handler, no reply sent).
/// Uses a 2 s `reply_timeout` to avoid blocking the test suite for 30 s.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn t0503c_handler_reply_timeout() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();

    let script = write_handler_script(tmp.path(), "hang.py", HANG_HANDLER_PY);

    let program_hash = vec![0x53; 32];
    let mut config =
        python_handler_config(vec![ProgramMatcher::Hash(program_hash.clone())], script);
    config.reply_timeout = Some(Duration::from_secs(2));
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![config])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-503c", 0x53C0, [0x5C; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 5301, &program_hash).await;

    // Send APP_DATA — handler will read it but never reply.
    // Gateway must time out (2s) and return None.
    let blob = vec![0x01];
    let app_frame = node.build_app_data(starting_seq, &blob);
    let peer = node.peer_address();

    let resp = gw.process_frame(&app_frame, peer).await;

    assert!(
        resp.is_none(),
        "hung handler must produce no reply after timeout"
    );
}

// ─── Regression: Gateway without handler still accepts APP_DATA silently ──

/// Verify the existing `Gateway::new()` (no handler) still silently accepts
/// APP_DATA without crashing (Phase 2B backward compatibility).
#[tokio::test]
async fn t05xx_no_handler_backward_compat() {
    let storage = Arc::new(InMemoryStorage::new());
    let gw = Gateway::new(storage.clone(), Duration::from_secs(30));

    let program_hash = vec![0xFF; 32];
    let node = TestNode::new("node-compat", 0x00FF, [0xEE; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 99000, &program_hash).await;

    let app_frame = node.build_app_data(starting_seq, &[0x01, 0x02]);
    let resp = gw.process_frame(&app_frame, node.peer_address()).await;
    assert!(
        resp.is_none(),
        "gateway without handler must silently accept APP_DATA"
    );
}

// ═══════════════════════════════════════════════════════════════════════
//  Issue #352 — Handler lifecycle and data path gap tests
// ═══════════════════════════════════════════════════════════════════════

// ─── Gap 1: GW-0503 AC3 — Persistent handler stays alive across messages ────

/// Persistent counter handler: stays alive across messages and includes an
/// incrementing counter in each reply. If the handler were respawned, the
/// counter would reset to 0.
const PERSISTENT_COUNTER_HANDLER_PY: &str = r#"
import sys, struct

counter = 0

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
    msg_type = msg[1]
    if msg_type == 1:  # DATA
        counter += 1
        request_id = msg[2]
        reply = encode_cbor_map([
            (1, 0x81),
            (2, request_id),
            (3, bytes([counter])),
        ])
        write_msg(reply)
    elif msg_type == 2:  # EVENT — no reply expected
        pass
"#;

/// GW-0503 AC3: Persistent handler stays alive across messages.
///
/// Uses a handler that maintains an in-process counter. Each DATA reply
/// includes the counter value. If the handler were respawned between
/// messages the counter would reset, causing assertion failures.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn gw0503_ac3_persistent_handler_stays_alive() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "counter.py", PERSISTENT_COUNTER_HANDLER_PY);

    let program_hash = vec![0x30; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-persist", 0x0530, [0x60; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 30000, &program_hash).await;

    // Send 3 APP_DATA messages — counter must increment 1, 2, 3
    for i in 0u64..3 {
        let seq = starting_seq + i;
        let blob = vec![(i + 1) as u8; 2];
        let app_frame = node.build_app_data(seq, &blob);
        let resp = gw
            .process_frame(&app_frame, node.peer_address())
            .await
            .unwrap_or_else(|| panic!("expected reply for APP_DATA #{i}"));

        let (_hdr, msg) = decode_response(&resp, &node.psk);
        match msg {
            GatewayMessage::AppDataReply { blob: reply_blob } => {
                assert_eq!(
                    reply_blob,
                    vec![(i + 1) as u8],
                    "counter must be {} on message #{i} (handler must not respawn)",
                    i + 1
                );
            }
            other => panic!("expected AppDataReply, got {:?}", other),
        }
    }
}

// ─── Gap 2: GW-0501 — Sequence-number correctness ──────────────────────────

/// GW-0501 AC3/AC4: Each APP_DATA_REPLY echoes the correct per-message
/// sequence number in the reply nonce.
///
/// Sends 3 APP_DATA messages with sequential seq numbers and verifies each
/// APP_DATA_REPLY's header nonce matches the request's sequence number.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn gw0501_sequence_number_correctness() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "multi.py", MULTI_ECHO_HANDLER_PY);

    let program_hash = vec![0x31; 32];
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![ProgramMatcher::Hash(program_hash.clone())],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    let node = TestNode::new("node-seq", 0x0531, [0x61; 32]);
    setup_node_with_program(&storage, &node, &program_hash).await;

    let (starting_seq, _, _) = do_wake(&gw, &node, 31000, &program_hash).await;

    for i in 0u64..3 {
        let seq = starting_seq + i;
        let blob = vec![(0x40 + i) as u8; 3];
        let app_frame = node.build_app_data(seq, &blob);
        let resp = gw
            .process_frame(&app_frame, node.peer_address())
            .await
            .unwrap_or_else(|| panic!("expected reply for APP_DATA #{i}"));

        let (hdr, msg) = decode_response(&resp, &node.psk);
        assert_eq!(hdr.msg_type, MSG_APP_DATA_REPLY);
        assert_eq!(
            hdr.nonce, seq,
            "APP_DATA_REPLY nonce must echo request seq {seq} for message #{i}"
        );
        match msg {
            GatewayMessage::AppDataReply { blob: reply_blob } => {
                assert_eq!(reply_blob, blob, "echo mismatch on message #{i}");
            }
            other => panic!("expected AppDataReply, got {:?}", other),
        }
    }
}

// ─── Gap 3: GW-0504 AC3 — Many-to-one routing ─────────────────────────────

/// GW-0504 AC3: Two program hashes routed to the same handler.
///
/// Configures a single echo handler with two hash matchers. Nodes running
/// different programs both get correct replies from the same handler.
#[cfg_attr(not(feature = "python-tests"), ignore = "requires Python runtime")]
#[tokio::test]
async fn gw0504_many_to_one_routing() {
    require_python!();
    let tmp = tempfile::tempdir().unwrap();
    let script = write_handler_script(tmp.path(), "multi.py", MULTI_ECHO_HANDLER_PY);

    let hash_x = vec![0xC0; 32];
    let hash_y = vec![0xD0; 32];

    // One handler configured for both program hashes
    let router = Arc::new(tokio::sync::RwLock::new(HandlerRouter::new(vec![python_handler_config(
        vec![
            ProgramMatcher::Hash(hash_x.clone()),
            ProgramMatcher::Hash(hash_y.clone()),
        ],
        script,
    )])));

    let storage = Arc::new(InMemoryStorage::new());
    let gw = make_gateway_with_handler(storage.clone(), router);

    // Node X with hash_x
    let node_x = TestNode::new("node-mto-x", 0x5040, [0xC1; 32]);
    setup_node_with_program(&storage, &node_x, &hash_x).await;
    let (seq_x, _, _) = do_wake(&gw, &node_x, 40000, &hash_x).await;

    let blob_x = vec![0xAA, 0xBB];
    let app_x = node_x.build_app_data(seq_x, &blob_x);
    let resp_x = gw
        .process_frame(&app_x, node_x.peer_address())
        .await
        .expect("node X must get reply from shared handler");
    let (_, msg_x) = decode_response(&resp_x, &node_x.psk);
    match msg_x {
        GatewayMessage::AppDataReply { blob } => {
            assert_eq!(blob, blob_x, "shared handler must echo node X data");
        }
        other => panic!("expected AppDataReply for X, got {:?}", other),
    }

    // Node Y with hash_y — same handler
    let node_y = TestNode::new("node-mto-y", 0x5041, [0xD1; 32]);
    setup_node_with_program(&storage, &node_y, &hash_y).await;
    let (seq_y, _, _) = do_wake(&gw, &node_y, 41000, &hash_y).await;

    let blob_y = vec![0xCC, 0xDD];
    let app_y = node_y.build_app_data(seq_y, &blob_y);
    let resp_y = gw
        .process_frame(&app_y, node_y.peer_address())
        .await
        .expect("node Y must get reply from shared handler");
    let (_, msg_y) = decode_response(&resp_y, &node_y.psk);
    match msg_y {
        GatewayMessage::AppDataReply { blob } => {
            assert_eq!(blob, blob_y, "shared handler must echo node Y data");
        }
        other => panic!("expected AppDataReply for Y, got {:?}", other),
    }
}
