# Gateway Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design of the Sonde gateway service.  
> **Audience:** Implementers (human or LLM agent) building the gateway.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [protocol.md](protocol.md), [security.md](security.md), [gateway-api.md](gateway-api.md)

---

## 1  Overview

The gateway is a single long-running Rust service that:

1. Receives authenticated frames from nodes over a radio transport.
2. Manages node sessions (WAKE → COMMAND → post-WAKE exchanges).
3. Distributes BPF programs via chunked transfer.
4. Routes application data to external handler processes.
5. Persists node registry, key material, and program library.

The gateway is **stateless with respect to replay protection** — active sessions exist only in memory. All durable state (keys, programs, schedules) is managed through an abstract storage backend.

---

## 2  Technology choices

| Decision | Choice | Rationale |
|---|---|---|
| Language | Rust | Memory safety, async ecosystem, strong typing, no GC pauses |
| Async runtime | tokio | Industry-standard async runtime; per-node task spawning |
| BPF verification | [prevail-rust](https://github.com/elazarg/prevail-rust) | Native Rust, feature-parity with C++ Prevail, no FFI |
| CBOR | `ciborium` crate | Well-maintained, serde-compatible |
| HMAC | `hmac` + `sha2` crates (RustCrypto) | Pure Rust, audited, no OpenSSL dependency |
| Transport | Abstract trait (ESP-NOW as first adapter) | Decouples protocol logic from radio hardware |
| Storage | Abstract trait | Decouples persistence from storage engine |

---

## 3  Module architecture

```
┌──────────────────────────────────────────────────────────────┐
│                        gateway                               │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌────────────┐   │
│  │Transport │  │ Protocol │  │  Session  │  │  Handler   │   │
│  │ (trait)  │──│  Codec   │──│  Manager  │──│  Router    │   │
│  └──────────┘  └──────────┘  └───────────┘  └────────────┘   │
│       │                           │               │          │
│  ┌──────────┐  ┌──────────┐  ┌───────────┐  ┌────────────┐   │
│  │ ESP-NOW  │  │ Node     │  │ Program   │  │  Handler   │   │
│  │ Adapter  │  │ Registry │  │ Library   │  │  Process   │   │
│  └──────────┘  └──────────┘  └───────────┘  └────────────┘   │
│                     │              │                         │
│                ┌──────────┐                                  │
│                │ Storage  │                                  │
│                │ (trait)  │                                  │
│                └──────────┘                                  │
└──────────────────────────────────────────────────────────────┘
```

### 3.1  Module responsibilities

| Module | Responsibility | Requirements covered |
|---|---|---|
| **Transport** | Send/receive raw frames | GW-0100, GW-0104 |
| **Protocol Codec** | Serialize/deserialize frames (header, CBOR, HMAC) | GW-0101, GW-0102, GW-0103, GW-0600, GW-0603 |
| **Session Manager** | Per-node session lifecycle, sequence tracking, command dispatch | GW-0200–0204, GW-0602, GW-1002, GW-1003 |
| **Node Registry** | PSK lookup, node metadata, battery/ABI tracking | GW-0601, GW-0700, GW-0701, GW-0702, GW-0703 |
| **Program Library** | Program storage, verification, chunking, hash identity | GW-0300–0302, GW-0400–0403, GW-1004 |
| **Handler Router** | Route APP_DATA to handler processes by program_hash | GW-0500, GW-0501, GW-0504–0508 |
| **Handler Process** | Manage handler stdin/stdout lifecycle | GW-0502, GW-0503, GW-0506 |
| **Storage** | Persist node registry, program library, configuration | GW-0700, GW-1000, GW-1001 |

---

## 4  Transport trait

```rust
/// Opaque address type for the transport layer (e.g., MAC address for ESP-NOW).
pub type PeerAddress = Vec<u8>;

#[async_trait]
pub trait Transport: Send + Sync {
    /// Receive the next inbound frame (blocking until available).
    /// Returns the raw bytes (header + payload + HMAC) and the
    /// sender's transport-layer address.
    async fn recv(&self) -> Result<(Vec<u8>, PeerAddress), TransportError>;

    /// Send a frame to a specific peer by transport-layer address.
    async fn send(&self, frame: &[u8], peer: &PeerAddress) -> Result<(), TransportError>;
}
```

The transport returns the sender's address alongside the frame. After the protocol layer authenticates the frame and identifies the node, the session manager stores the peer address in the session. Responses are sent to the address from the session, not looked up by `key_hint`.

### 4.1  ESP-NOW adapter

The ESP-NOW adapter wraps the platform's ESP-NOW API:

- `recv()` returns one complete ESP-NOW frame (max 250 bytes) and the sender's MAC address (6 bytes).
- `send()` transmits one ESP-NOW frame to the specified MAC address. The ESP-NOW peer is registered on first use.
- The 250-byte frame size constraint (GW-0104) is enforced by ESP-NOW itself.

---

## 5  Protocol codec

The codec handles frame serialization and deserialization. It operates on raw byte buffers and produces/consumes typed messages.

### 5.1  Frame layout

```
Offset 0:  key_hint    (2 bytes, big-endian)
Offset 2:  msg_type    (1 byte)
Offset 3:  nonce       (8 bytes, big-endian)
Offset 11: payload     (variable, CBOR-encoded)
Offset -32: hmac       (32 bytes, HMAC-SHA256 over bytes 0..len-32)
```

### 5.2  Inbound decoding

```rust
pub struct InboundFrame {
    pub key_hint: u16,
    pub msg_type: u8,
    pub nonce: u64,
    pub payload: Vec<u8>,  // raw CBOR bytes (pre-auth, not yet decoded)
    pub hmac: [u8; 32],
}
```

Decoding steps:
1. Validate minimum frame size (11 header + 32 HMAC = 43 bytes).
2. Split frame into header (11 bytes), payload (middle), HMAC (last 32 bytes).
3. Parse header fields at fixed offsets.
4. Return `InboundFrame`. CBOR payload is **not** decoded until after HMAC verification.

### 5.3  HMAC verification

```rust
pub fn verify_hmac(key: &[u8; 32], frame: &InboundFrame) -> bool {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(&frame.header_and_payload_bytes());
    mac.verify_slice(&frame.hmac).is_ok()
}
```

The codec provides this as a pure function. The session manager calls it with candidate keys from the node registry.

### 5.4  Outbound encoding

```rust
pub fn encode_frame(
    key_hint: u16,
    msg_type: u8,
    nonce: u64,
    payload_cbor: &[u8],
    key: &[u8; 32],
) -> Vec<u8>
```

1. Write header (11 bytes) + payload.
2. Compute HMAC over header + payload.
3. Append HMAC (32 bytes).
4. Return complete frame.

### 5.5  CBOR message types

Each protocol message is a Rust enum variant with typed fields:

```rust
pub enum NodeMessage {
    Wake {
        firmware_abi_version: u32,
        program_hash: Vec<u8>,
        battery_mv: u32,
    },
    GetChunk {
        chunk_index: u32,
    },
    ProgramAck {
        program_hash: Vec<u8>,
    },
    AppData {
        blob: Vec<u8>,
    },
}

pub enum GatewayMessage {
    Command {
        command_type: u8,
        starting_seq: u64,
        timestamp_ms: u64,
        payload: Option<CommandPayload>,
    },
    Chunk {
        chunk_index: u32,
        chunk_data: Vec<u8>,
    },
    AppDataReply {
        blob: Vec<u8>,
    },
}
```

CBOR encoding/decoding uses integer keys as defined in protocol.md § CBOR key mapping.

---

## 6  Session manager

The session manager is the core orchestration module. It processes authenticated frames and drives the node through its wake cycle.

### 6.1  Session state

```rust
pub struct Session {
    pub node_id: NodeId,
    pub peer_address: PeerAddress,
    pub wake_nonce: u64,
    pub next_expected_seq: u64,
    pub created_at: Instant,
    pub state: SessionState,
}

pub enum SessionState {
    AwaitingPostWake,
    ChunkedTransfer {
        program_hash: Vec<u8>,
        program_size: u32,
        chunk_size: u32,
        chunk_count: u32,
        is_ephemeral: bool,
    },
    BpfExecuting,
}
```

Sessions are stored in a `HashMap<NodeId, Session>`. At most one session exists per node (GW-0602). A new WAKE replaces any existing session for that node.

### 6.2  Session timeout

Sessions are reaped after a configurable timeout (default: 30 seconds). A background task periodically scans the session map and removes expired entries.

### 6.3  Inbound frame processing

```
recv frame
  │
  ├── parse header (key_hint, msg_type, nonce)
  │
  ├── lookup candidate keys by key_hint
  │     └── no keys → discard (GW-1002)
  │
  ├── try HMAC with each candidate key
  │     └── none match → discard (GW-0600)
  │
  ├── identify node (bound to matching key)
  │
  ├── if WAKE:
  │     ├── decode CBOR payload → Wake fields
  │     ├── create/replace session for this node
  │     ├── generate random starting_seq
  │     ├── get current UTC timestamp_ms
  │     ├── determine command (check program_hash, pending actions)
  │     ├── encode COMMAND response
  │     ├── send response (echoing wake nonce)
  │     ├── update node registry (battery_mv, firmware_abi_version)
  │     └── emit EVENT to handler (node_online)
  │
  ├── if post-WAKE (GET_CHUNK, PROGRAM_ACK, APP_DATA):
  │     ├── lookup active session for this node
  │     │     └── no session → discard
  │     ├── verify nonce field == session.next_expected_seq
  │     │     └── mismatch → discard
  │     ├── advance session.next_expected_seq
  │     ├── decode CBOR payload
  │     └── dispatch to appropriate handler:
  │           ├── GET_CHUNK → serve chunk from program library
  │           ├── PROGRAM_ACK → update node's program_hash in registry
  │           └── APP_DATA → route to handler process
  │
  └── discard on any error (no error response sent)
```

### 6.4  Command selection logic

On receiving a valid WAKE, the session manager determines the command:

| Condition | Command | Priority |
|---|---|---|
| Pending ephemeral program for this node | `RUN_EPHEMERAL` | 1 (highest) |
| `program_hash` differs from intended program | `UPDATE_PROGRAM` | 2 |
| Pending schedule change for this node | `UPDATE_SCHEDULE` | 3 |
| Pending reboot request for this node | `REBOOT` | 4 |
| None of the above | `NOP` | 5 (lowest) |

Only one command is issued per WAKE (GW-0103).

---

## 7  Node registry

The node registry persists node metadata through the storage trait.

### 7.1  Node record

```rust
pub struct NodeRecord {
    pub node_id: NodeId,
    pub key_hint: u16,
    pub psk: [u8; 32],
    pub assigned_program_hash: Option<Vec<u8>>,
    pub current_program_hash: Option<Vec<u8>>,
    pub schedule_interval_s: u32,
    pub firmware_abi_version: Option<u32>,
    pub last_battery_mv: Option<u32>,
    pub last_seen: Option<SystemTime>,
    pub admin_node_id: String,  // opaque human-readable ID for handler API
}
```

### 7.2  Key lookup

```rust
pub fn lookup_by_key_hint(&self, key_hint: u16) -> Vec<&NodeRecord>
```

Returns all nodes matching the `key_hint`. The caller tries HMAC verification with each candidate's PSK (GW-0601).

### 7.3  Node registration (USB pairing)

The registry supports adding and removing nodes (GW-0601, GW-0704, GW-0705). Registration is an admin operation, not part of the radio protocol.

---

## 8  Program library

The program library stores verified BPF programs and serves chunks.

### 8.1  Program record

```rust
pub struct ProgramRecord {
    pub hash: Vec<u8>,         // SHA-256 of program bytes
    pub bytes: Vec<u8>,        // complete program binary
    pub size: u32,
    pub verification_profile: VerificationProfile,
}

pub enum VerificationProfile {
    Resident,
    Ephemeral,
}
```

### 8.2  Program ingestion

1. Accept pre-compiled BPF ELF (GW-0400).
2. Verify with `prevail-rust` against the appropriate profile (GW-0401).
3. Enforce size limits: 4 KB resident, 2 KB ephemeral (GW-0403).
4. Compute SHA-256 hash (GW-0402).
5. Store in library.

### 8.3  Chunk serving

```rust
pub fn get_chunk(
    &self,
    program_hash: &[u8],
    chunk_index: u32,
    chunk_size: u32,
) -> Option<Vec<u8>>
```

Returns the bytes for the requested chunk. Chunk boundaries are computed as:
- Start: `chunk_index * chunk_size`
- End: `min(start + chunk_size, program_size)`
- Last chunk may be smaller than `chunk_size`.

All gateway instances in a failover group serve identical bytes for the same hash (GW-1004).

---

## 9  Handler router

The handler router maps `program_hash` → handler process and manages the handler lifecycle.

### 9.1  Configuration

```rust
pub struct HandlerConfig {
    pub program_hashes: Vec<Vec<u8>>,  // or "*" for catch-all
    pub command: String,
    pub args: Vec<String>,
}
```

Multiple program hashes can map to the same handler (GW-0504).

### 9.2  Routing

On receiving APP_DATA from a node:

1. Look up the node's current `program_hash` in the handler config.
2. If no match and no catch-all → do not send APP_DATA_REPLY to the node (GW-0504).
3. If match → forward to handler as a DATA message (GW-0505).

### 9.3  Handler process lifecycle

Each handler config spawns a handler process (GW-0503):

- Process is started on first message.
- stdin/stdout communicate via 4-byte big-endian length prefix + CBOR (GW-0502).
- If the process stays alive → reuse for subsequent messages.
- If the process exits with code 0 → respawn on next message.
- If the process exits with non-zero → log error, no APP_DATA_REPLY to node.

### 9.4  Message flow

```
APP_DATA from node
  │
  ├── route by program_hash
  │
  ├── construct DATA message:
  │     msg_type: 0x01
  │     request_id: unique per in-flight request
  │     node_id: admin-assigned opaque string
  │     program_hash: current program hash
  │     data: blob from APP_DATA
  │     timestamp: current Unix time (seconds)
  │
  ├── write length-prefixed CBOR to handler stdin
  │
  ├── read DATA_REPLY from handler stdout
  │     ├── request_id must match
  │     ├── if data is non-zero-length → send APP_DATA_REPLY to node
  │     └── if data is zero-length → do not send APP_DATA_REPLY
  │
  └── (handler may also write LOG messages at any time → route to gateway log)
```

### 9.5  Event messages

The session manager emits lifecycle events to handlers (GW-0507):

| Event | When | Details |
|---|---|---|
| `node_online` | WAKE processed | `battery_mv`, `firmware_abi_version` |
| `program_updated` | PROGRAM_ACK received | `program_hash` |
| `node_timeout` | Node missed expected wake | `last_seen`, `expected_interval_s` |

Events are sent as EVENT messages (msg_type 0x02) to the handler's stdin. No reply is expected.

---

## 10  Storage trait

```rust
#[async_trait]
pub trait Storage: Send + Sync {
    // Node registry
    async fn list_nodes(&self) -> Result<Vec<NodeRecord>>;
    async fn get_node(&self, node_id: &NodeId) -> Result<Option<NodeRecord>>;
    async fn get_nodes_by_key_hint(&self, key_hint: u16) -> Result<Vec<NodeRecord>>;
    async fn upsert_node(&self, record: &NodeRecord) -> Result<()>;
    async fn delete_node(&self, node_id: &NodeId) -> Result<()>;

    // Program library
    async fn get_program(&self, hash: &[u8]) -> Result<Option<ProgramRecord>>;
    async fn store_program(&self, record: &ProgramRecord) -> Result<()>;
    async fn delete_program(&self, hash: &[u8]) -> Result<()>;
    async fn list_programs(&self) -> Result<Vec<ProgramRecord>>;

    // Export / import (GW-1001)
    async fn export_state(&self) -> Result<Vec<u8>>;
    async fn import_state(&self, data: &[u8]) -> Result<()>;
}
```

The storage trait is async to support different backends (file, SQLite, network) without blocking the event loop.

---

## 11  Concurrency model

The gateway runs a single tokio async runtime:

```
tokio runtime
  │
  ├── transport recv loop (main task)
  │     └── for each inbound frame:
  │           └── spawn per-node processing task
  │
  ├── session reaper (periodic background task)
  │     └── removes expired sessions
  │
  ├── node timeout detector (periodic background task)
  │     └── emits node_timeout events
  │
  └── handler I/O tasks (one per active handler process)
        └── reads stdout, routes LOG/DATA_REPLY
```

Per-node processing tasks are spawned for each inbound frame. The session map is shared via `Arc<RwLock<HashMap<NodeId, Session>>>`. Since sessions are short-lived and contention is low (each node has at most one concurrent frame), a simple `RwLock` is sufficient.

The node registry and program library are accessed through the storage trait behind an `Arc`. Storage implementations are responsible for their own internal synchronization.

---

## 12  Error handling

All protocol errors result in **silent discard** — no error response is sent to the node (security.md §6). Errors are logged internally for operational monitoring.

| Error | Behavior |
|---|---|
| No key matches `key_hint` | Discard. Log at debug level. |
| HMAC verification failure | Discard. Log at debug level. |
| Wrong sequence number / no active session | Discard. Log at info level. |
| Malformed CBOR (post-auth) | Discard. Log at warn level. |
| Unexpected `msg_type` | Discard. Log at warn level. |
| Handler crash mid-request | No APP_DATA_REPLY sent. Log at error level. |
| Storage failure | Log at error level. Retry or degrade gracefully. |

---

## 13  Configuration

The gateway is configured via a configuration file (format TBD — TOML recommended for Rust ecosystem). Configuration includes:

| Setting | Description | Default |
|---|---|---|
| `transport` | Transport backend and connection parameters | Required |
| `storage` | Storage backend and connection parameters | Required |
| `handlers` | Handler routing table (program_hash → command) | `[]` |
| `session_timeout_s` | Session inactivity timeout | `30` |
| `node_timeout_multiplier` | Multiple of node's schedule interval before `node_timeout` event | `3` |
| `max_message_size` | Maximum handler API message size | `1048576` (1 MB) |
| `log_level` | Logging verbosity | `info` |

---

## 14  Startup sequence

1. Load configuration.
2. Initialize storage backend.
3. Load node registry and program library from storage.
4. Initialize transport (e.g., open ESP-NOW interface).
5. Start handler processes for configured handlers.
6. Start session reaper background task.
7. Start node timeout detector background task.
8. Enter main recv loop.

---

## 15  Shutdown sequence

1. Stop accepting new frames.
2. Wait for in-flight sessions to complete (with timeout).
3. Terminate handler processes (SIGTERM, then SIGKILL after timeout).
4. Flush any pending storage writes.
5. Close transport.
6. Exit.
