<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design of the Sonde gateway service.  
> **Audience:** Implementers (human or LLM agent) building the gateway.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [protocol.md](protocol.md), [protocol-crate-design.md](protocol-crate-design.md), [security.md](security.md), [gateway-api.md](gateway-api.md)

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
| Protocol crate | `sonde-protocol` (shared with node) | `no_std`-compatible; frame codec, CBOR messages, constants |
| Async runtime | tokio | Industry-standard async runtime; per-node task spawning |
| BPF verification | [prevail-rust](https://github.com/elazarg/prevail-rust) | Native Rust, feature-parity with C++ Prevail, no FFI |
| CBOR | Via `sonde-protocol` (`ciborium`) | Well-maintained, serde-compatible |
| HMAC | `hmac` + `sha2` crates (RustCrypto, implements `sonde-protocol::HmacProvider` trait) | Pure Rust, audited, no OpenSSL dependency |
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

The transport returns the sender's address alongside the frame. After the protocol layer authenticates the frame (using `key_hint` → candidate PSK lookup → HMAC verification) and identifies the node, the session manager stores the peer address in the session. Responses are sent to the address from the session.

The peer address is **session-scoped** — it is never persisted. Each WAKE re-establishes the address. This is correct because:
- A node's MAC address may change (hardware replacement after factory reset + re-pair).
- The `key_hint` → PSK lookup is the durable identity mechanism; the MAC is just a transient delivery address.

### 4.1  ESP-NOW adapter

The ESP-NOW adapter wraps the platform's ESP-NOW API:

- `recv()` returns one complete ESP-NOW frame (max 250 bytes) and the sender's MAC address (6 bytes).
- `send()` transmits one ESP-NOW frame to the specified MAC address. The ESP-NOW peer is registered on first use.
- The 250-byte frame size constraint (GW-0104) is enforced by ESP-NOW itself.

### 4.2  USB modem adapter (`UsbEspNowTransport`)

When the gateway runs on a host without ESP-NOW hardware, a USB-attached ESP32-S3 radio modem provides the radio link. The `UsbEspNowTransport` implements the `Transport` trait by speaking the modem serial protocol defined in [modem-protocol.md](modem-protocol.md).

**Internal architecture:**

The adapter spawns a serial reader task that demultiplexes incoming modem messages:

- `RECV_FRAME` → pushed to an async channel consumed by `Transport::recv()`.
- `STATUS` / `SET_CHANNEL_ACK` / `SCAN_RESULT` → delivered to pending command futures.
- `ERROR` → logged; optionally triggers recovery.
- `MODEM_READY` → delivered to the startup/reset future.

```rust
pub struct UsbEspNowTransport {
    /// Async serial port (e.g., tokio-serial).
    port: Arc<Mutex<AsyncSerialPort>>,
    /// Channel for RECV_FRAME messages from the serial reader task.
    recv_rx: mpsc::Receiver<(Vec<u8>, PeerAddress)>,
    /// Modem's MAC address (from MODEM_READY).
    modem_mac: [u8; 6],
}
```

**Startup sequence (GW-1101):**

1. Open the serial port (device path from configuration).
2. Send `RESET`.
3. Wait for `MODEM_READY` (timeout: 5 seconds, up to 3 retries).
4. Extract `firmware_version` and `mac_address` from `MODEM_READY`; log both.
5. Send `SET_CHANNEL` with the configured channel.
6. Wait for `SET_CHANNEL_ACK` (timeout: 2 seconds).
7. Start the serial reader task.
8. Start the health monitor task.

**Health monitor (GW-1102):**

A background task polls `GET_STATUS` every 30 seconds and logs:
- `tx_fail_count` delta since last poll (warns on rising failures).
- `uptime_s` decrease (indicates unexpected modem reboot).

**Error handling (GW-1103):**

On `ERROR` from the modem, the adapter logs the error code and message. If the error is unrecoverable, it sends `RESET` and re-executes the startup sequence.

**`send()` implementation:**

Constructs a `SEND_FRAME` envelope (`peer_mac || frame_data`) and writes it to the serial port. Returns immediately — fire-and-forget. The 250-byte ESP-NOW frame limit is enforced by the modem.

**`recv()` implementation:**

Awaits the next `RECV_FRAME` from the async channel. Returns `(frame_data, peer_mac.to_vec())`. RSSI is available but not surfaced through the `Transport` trait (logged internally for diagnostics).

---

## 5  Protocol codec

The protocol codec is provided by the shared `sonde-protocol` crate (see [protocol-crate-design.md](protocol-crate-design.md) for the full crate specification). The gateway uses the same frame format, CBOR message types, and constants as the node. The gateway provides a software `HmacProvider` implementation using the `hmac` + `sha2` RustCrypto crates.

### 5.1  Frame layout

All types, constants, and functions in this section are provided by the `sonde-protocol` crate (see [protocol-crate-design.md](protocol-crate-design.md) for the full API). The gateway-specific code only provides the `HmacProvider` implementation.

```
Offset 0:  key_hint    (2 bytes, big-endian)
Offset 2:  msg_type    (1 byte)
Offset 3:  nonce       (8 bytes, big-endian)
Offset 11: payload     (variable, CBOR-encoded)
Offset -32: hmac       (32 bytes, HMAC-SHA256 over bytes 0..len-32)
```

### 5.2  Inbound decoding

Uses `sonde_protocol::decode_frame()` → returns `DecodedFrame { header, payload, hmac }`. CBOR payload is **not** decoded until after HMAC verification.

### 5.3  HMAC verification

The gateway implements `sonde_protocol::HmacProvider` using the `hmac` + `sha2` RustCrypto crates:

```rust
struct RustCryptoHmac;

impl sonde_protocol::HmacProvider for RustCryptoHmac {
    fn compute(&self, key: &[u8], data: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    fn verify(&self, key: &[u8], data: &[u8], expected: &[u8; 32]) -> bool {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(data);
        mac.verify_slice(expected).is_ok() // constant-time comparison
    }
}
```

The session manager calls `sonde_protocol::verify_frame()` with candidate keys from the node registry.

### 5.4  Outbound encoding

Uses `sonde_protocol::encode_frame()` with the gateway's `RustCryptoHmac` provider.

### 5.5  CBOR message types

All message types (`NodeMessage`, `GatewayMessage`, `CommandPayload`) and their CBOR encode/decode logic are defined in the `sonde-protocol` crate. The gateway uses `NodeMessage::decode()` for inbound messages and `GatewayMessage::encode()` for outbound.

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
    pub hash: Vec<u8>,         // SHA-256 of CBOR-encoded program image
    pub image: Vec<u8>,        // CBOR-encoded program image (bytecode + map definitions)
    pub size: u32,             // byte length of the CBOR image
    pub verification_profile: VerificationProfile,
}

pub enum VerificationProfile {
    Resident,
    Ephemeral,
}
```

### 8.2  Program ingestion

1. Accept pre-compiled BPF ELF (GW-0400).
2. Verify with `prevail-rust` against the appropriate profile (GW-0401). Prevail's loader resolves ELF map relocations to `LDDW src=1, imm=<map_index>`.
3. Extract bytecode (`.text` section) and map definitions from the ELF.
4. Encode as CBOR program image using `sonde_protocol::ProgramImage::encode_deterministic()`. See [protocol-crate-design.md §7](protocol-crate-design.md) and [protocol.md § Program image format](protocol.md#program-image-format).
5. Enforce size limits on the CBOR image: 4 KB resident, 2 KB ephemeral (GW-0403).
6. Compute `program_hash` using `sonde_protocol::program_hash()` with the gateway's SHA-256 provider (GW-0402).
7. Store in library. Verification and encoding complete at ingestion time — chunk serving is immediate (GW-0400).

### 8.3  Chunk serving

Uses `sonde_protocol::get_chunk()` on the stored CBOR image:

```rust
let chunk_data = sonde_protocol::get_chunk(&record.image, chunk_index, chunk_size);
```

Returns bytes from the stored CBOR program image for the requested chunk. Last chunk may be smaller than `chunk_size`.

All gateway instances in a failover group serve identical bytes for the same hash (GW-1004).

---

## 9  Handler router

The handler router maps `program_hash` → handler process and manages the handler lifecycle.

### 9.1  Configuration

```rust
pub enum ProgramMatcher {
    /// Match any program hash (catch-all).
    Any,
    /// Match a specific program hash.
    Hash(Vec<u8>),
}

pub struct HandlerConfig {
    pub matchers: Vec<ProgramMatcher>,
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

Storage implementations SHOULD encrypt PSK material at rest (GW-0601a). The `NodeRecord.psk` field contains the raw 256-bit key — implementations are responsible for encrypting it before persisting and decrypting on read. The storage trait itself is agnostic to the encryption mechanism.

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

Per-node processing tasks are spawned for each inbound frame. The session map is shared via `Arc<tokio::sync::RwLock<HashMap<NodeId, Session>>>`. Since sessions are short-lived and contention is low (each node has at most one concurrent frame), a simple async-aware `RwLock` is sufficient, but implementations MUST NOT hold a lock guard across `.await` points. Implementers MAY instead use a concurrent map (e.g., `DashMap<NodeId, Session>`) to reduce lock contention and avoid common async locking pitfalls.

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

## 13  Admin API

The gateway exposes a local gRPC API for administrative operations. A CLI tool (`sonde-admin`) wraps the API for operator use.

### 13.1  gRPC service definition

```protobuf
service GatewayAdmin {
    // Node management
    rpc ListNodes(Empty) returns (ListNodesResponse);
    rpc GetNode(GetNodeRequest) returns (NodeInfo);
    rpc RegisterNode(RegisterNodeRequest) returns (RegisterNodeResponse);
    rpc RemoveNode(RemoveNodeRequest) returns (Empty);

    // Program management
    rpc IngestProgram(IngestProgramRequest) returns (IngestProgramResponse);
    rpc ListPrograms(Empty) returns (ListProgramsResponse);
    rpc AssignProgram(AssignProgramRequest) returns (Empty);
    rpc RemoveProgram(RemoveProgramRequest) returns (Empty);

    // Schedule and commands
    rpc SetSchedule(SetScheduleRequest) returns (Empty);
    rpc QueueReboot(QueueRebootRequest) returns (Empty);
    rpc QueueEphemeral(QueueEphemeralRequest) returns (Empty);

    // Status
    rpc GetNodeStatus(GetNodeStatusRequest) returns (NodeStatus);

    // State export/import
    rpc ExportState(Empty) returns (ExportStateResponse);
    rpc ImportState(ImportStateRequest) returns (Empty);
}
```

The gRPC server runs on a configurable local address (default: `localhost:50051`). It is implemented with the `tonic` crate.

### 13.2  Key operations

| Operation | gRPC method | Description |
|---|---|---|
| Pair node | `RegisterNode` | Called by CLI after USB key provisioning. Registers key_hint, PSK, and admin node_id. |
| Factory reset | `RemoveNode` | Called by CLI after USB factory reset. Removes node from registry. |
| Ingest program | `IngestProgram` | Accepts ELF binary + profile. Triggers verification, CBOR encoding, storage. Returns hash or error. |
| Assign program | `AssignProgram` | Sets a node's assigned program. Next WAKE triggers UPDATE_PROGRAM if hash differs. |
| Queue ephemeral | `QueueEphemeral` | Queues a one-shot diagnostic program for a node's next WAKE. |
| Set schedule | `SetSchedule` | Queues an UPDATE_SCHEDULE for a node's next WAKE. |
| Export state | `ExportState` | Serializes the full gateway state (node registry, program library, and handler routing configuration). |
| Import state | `ImportState` | Restores node registry, program library, and handler routing configuration from a previous export. |

### 13.3  CLI tool (`sonde-admin`)

The CLI wraps the gRPC API and handles USB communication for pairing/reset:

```
sonde-admin node list
sonde-admin node get <node-id>
sonde-admin node pair --usb <port>           # USB + gRPC
sonde-admin node reset --usb <port>          # USB + gRPC
sonde-admin node remove <node-id>

sonde-admin program ingest <elf-file> --profile resident|ephemeral
sonde-admin program list
sonde-admin program assign <node-id> <program-hash>
sonde-admin program remove <program-hash>

sonde-admin schedule set <node-id> <interval-seconds>
sonde-admin reboot <node-id>
sonde-admin ephemeral <node-id> <elf-file>

sonde-admin state export <file>
sonde-admin state import <file>

sonde-admin status <node-id>
```

All commands support `--format json` for machine-readable output.

**USB pairing flow:**
1. CLI connects to node via USB serial.
2. CLI generates a 256-bit PSK (from OS CSPRNG).
3. CLI writes PSK to node's flash key partition over USB.
4. CLI calls `RegisterNode` on the gateway gRPC API with the key_hint, PSK, and admin-assigned node_id.
5. If either step fails, both sides are rolled back.

**USB factory reset flow:**
1. CLI connects to node via USB serial.
2. CLI sends factory reset command to node (erases key, maps, program).
3. CLI calls `RemoveNode` on the gateway gRPC API.

---

## 14  Configuration

The gateway is configured via a configuration file (format TBD — TOML recommended for Rust ecosystem). Configuration includes:

| Setting | Description | Default |
|---|---|---|
| `transport` | Transport backend and connection parameters | Required |
| `storage` | Storage backend and connection parameters | Required |
| `admin_listen` | gRPC admin API listen address | `localhost:50051` |
| `handlers` | Handler routing table (program_hash → command) | `[]` |
| `session_timeout_s` | Session inactivity timeout | `30` |
| `node_timeout_multiplier` | Multiple of node's schedule interval before `node_timeout` event | `3` |
| `max_message_size` | Maximum handler API message size | `1048576` (1 MB) |
| `log_level` | Logging verbosity | `info` |

---

## 15  Startup sequence

1. Load configuration.
2. Initialize storage backend.
3. Load node registry and program library from storage.
4. Initialize transport (e.g., open ESP-NOW interface, or for USB modem: open serial port → `RESET` → `MODEM_READY` → `SET_CHANNEL`; see §4.2).
5. Start gRPC admin API server.
6. Start handler processes for configured handlers.
7. Start session reaper background task.
8. Start node timeout detector background task.
9. Enter main recv loop.

---

## 16  Shutdown sequence

1. Stop accepting new frames.
2. Wait for in-flight sessions to complete (with timeout).
3. Terminate handler processes (SIGTERM, then SIGKILL after timeout).
4. Flush any pending storage writes.
5. Close transport.
6. Exit.
