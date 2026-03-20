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

The gateway is composed of eight functional modules grouped in two tiers. The upper (data-path) tier contains: Transport (radio adapter, e.g., ESP-NOW over USB-CDC), Protocol Codec (frame serialization/deserialization), Session Manager (per-node lifecycle and sequence tracking), and Handler Router (forwarding application data to external handler processes). Each module in this tier connects to the next in series. The lower (infrastructure) tier contains: an ESP-NOW Adapter (concrete transport implementation), Node Registry (PSK and node metadata), Program Library (BPF program images and hash identity), and Handler Process (handler stdin/stdout management). Node Registry and Program Library share a common Storage trait abstraction at the bottom.

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
| **Transport** | Send/receive raw frames | GW-0100, GW-0104, GW-1100 |
| **Protocol Codec** | Serialize/deserialize frames (header, CBOR, HMAC) | GW-0101, GW-0102, GW-0103, GW-0600, GW-0603 |
| **Session Manager** | Per-node session lifecycle, sequence tracking, command dispatch | GW-0200–0204, GW-0602, GW-1002, GW-1003 |
| **Node Registry** | PSK lookup, node metadata, battery/ABI tracking | GW-0601, GW-0700, GW-0701, GW-0702, GW-0703 |
| **Program Library** | Program storage, verification, chunking, hash identity | GW-0300–0302, GW-0400–0403, GW-1004 |
| **Handler Router** | Route APP_DATA to handler processes by program_hash | GW-0500, GW-0501, GW-0504–0508 |
| **Handler Process** | Manage handler stdin/stdout lifecycle | GW-0502, GW-0503, GW-0506 |
| **Storage** | Persist node registry, program library, configuration | GW-0700, GW-1000, GW-1001 |
| **Admin API** | gRPC admin interface, CLI tool | GW-0800, GW-0801, GW-0802, GW-0803, GW-0804, GW-0805, GW-0806 |
| **BLE Pairing Handler** | BLE pairing protocol logic via modem relay | GW-1200–GW-1222 |

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

### 4.2  USB modem adapter (`UsbEspNowTransport`) (GW-1100)

When the gateway runs on a host without ESP-NOW hardware, a USB-attached ESP32-S3 radio modem provides the radio link. The `UsbEspNowTransport` implements the `Transport` trait by speaking the modem serial protocol defined in [modem-protocol.md](modem-protocol.md).

**Internal architecture:**

The adapter spawns a serial reader task that demultiplexes incoming modem messages:

- `RECV_FRAME` → pushed to an async channel consumed by `Transport::recv()`.
- `STATUS` / `SET_CHANNEL_ACK` / `SCAN_RESULT` → delivered to pending command futures.
- `ERROR` → logged; optionally triggers recovery.
- `MODEM_READY` → delivered to the startup/reset future.

```rust
pub struct UsbEspNowTransport {
    /// Async serial port (e.g., tokio_serial::SerialStream).
    port: Arc<Mutex<Box<dyn AsyncRead + AsyncWrite + Send + Unpin>>>,
    /// Channel for RECV_FRAME messages from the serial reader task.
    recv_rx: mpsc::Receiver<(Vec<u8>, PeerAddress)>,
    /// Modem's MAC address (from MODEM_READY).
    modem_mac: [u8; 6],
}
```

**Startup sequence (GW-1101):**

1. Open the serial port (device path from configuration).
2. Start the serial reader task (it demultiplexes all incoming frames, including `MODEM_READY` and `SET_CHANNEL_ACK`, and routes them to the appropriate pending futures).
3. Send `RESET`.
4. Wait for `MODEM_READY` (timeout: 5 seconds, up to 3 retries).
5. Extract `firmware_version` and `mac_address` from `MODEM_READY`; log both.
6. Send `SET_CHANNEL` with the configured channel.
7. Wait for `SET_CHANNEL_ACK` (timeout: 2 seconds).
8. Start the health monitor task.

**Health monitor (GW-1102):**

A background task polls `GET_STATUS` every 30 seconds and logs:
- `tx_fail_count` delta since last poll (warns on rising failures).
- `uptime_s` decrease (indicates unexpected modem reboot).

**Error handling (GW-1103):**

On `ERROR` from the modem, the adapter logs the error code and message. If the error is unrecoverable, it sends `RESET` and re-executes the startup sequence.

**`send()` implementation:**

Constructs a `SEND_FRAME` envelope (`peer_mac || frame_data`) and writes it to the serial port. Does not wait for any modem or radio delivery acknowledgement — fire-and-forget at the radio layer, while still awaiting the serial write as needed. The 250-byte ESP-NOW frame limit is enforced by the modem.

**`recv()` implementation:**

Awaits the next `RECV_FRAME` from the async channel. Returns `(frame_data, peer_mac)`, where `peer_mac` is the `PeerAddress` obtained by converting the modem's 6-byte MAC address at the adapter boundary. RSSI is available but not surfaced through the `Transport` trait (logged internally for diagnostics).

---

## 5  Protocol codec

The protocol codec is provided by the shared `sonde-protocol` crate (see [protocol-crate-design.md](protocol-crate-design.md) for the full crate specification). The gateway uses the same frame format, CBOR message types, and constants as the node. The gateway provides a software `HmacProvider` implementation using the `hmac` + `sha2` RustCrypto crates.

### 5.1  Frame layout

All types, constants, and functions in this section are provided by the `sonde-protocol` crate (see [protocol-crate-design.md](protocol-crate-design.md) for the full API). The gateway-specific code only provides the `HmacProvider` implementation.

The frame is a flat byte array with fields at fixed offsets. The first 11 bytes form the binary header: `key_hint` occupies bytes 0–1 (big-endian u16), `msg_type` is byte 2, and `nonce` occupies bytes 3–10 (big-endian u64). Following the header is the variable-length CBOR-encoded payload. The final 32 bytes of the frame are the HMAC-SHA256 authentication tag, computed over all preceding bytes (header + payload).

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

Every inbound frame goes through a sequential pipeline. First the binary header is parsed (extracting `key_hint`, `msg_type`, and `nonce`). The `key_hint` is used to look up candidate node keys from the registry; if none are found the frame is silently discarded. The gateway tries each candidate key for HMAC verification; if none match, the frame is discarded. Once the node is identified by its matching key, the frame is dispatched based on `msg_type`. A `WAKE` frame causes a new session to be created (or an existing one replaced), a `COMMAND` response to be encoded and sent back, and a `node_online` event to be emitted. Post-WAKE frames (`GET_CHUNK`, `PROGRAM_ACK`, `APP_DATA`) require an active session and a matching sequence number; they are then routed to the program library, node registry, or handler process as appropriate. Any error at any step results in a silent discard — no error response is ever sent to the node.

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

### 7.3  Node registration

The registry supports adding and removing nodes (GW-0601, GW-0705). Registration is an admin operation, not part of the radio protocol.

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

When an `APP_DATA` frame arrives from a node it is routed to the matching handler process by `program_hash`. The gateway constructs a DATA message (containing `request_id`, `node_id`, `program_hash`, the opaque data blob, and a Unix timestamp) and writes it as a length-prefixed CBOR message to the handler's stdin. The gateway then reads the handler's stdout for a `DATA_REPLY` message whose `request_id` matches the request. If the reply contains a non-empty data field, the gateway sends an `APP_DATA_REPLY` back to the node; an empty data field means no reply is sent. The handler may also write `LOG` messages at any time, which the gateway routes to its own log.

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

## 10a  Master key provider

The 32-byte master key (used by `SqliteStorage` to encrypt PSKs, phone PSKs,
and the Ed25519 seed at rest) is loaded at startup via a `KeyProvider`
implementation selected by the `--key-provider` CLI flag (GW-0601b).

### 10a.1  Trait

```rust
/// Abstracts how the 32-byte gateway master key is obtained.
pub trait KeyProvider: Send + Sync {
    fn load_master_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError>;
}
```

The trait is intentionally synchronous; key loading is a one-time startup
operation.  For async backends (e.g. Secret Service over D-Bus), the
implementation uses `tokio::task::block_in_place` when called from within a
tokio runtime, or a temporary `current_thread` runtime otherwise.

### 10a.2  Backends

| Struct | CLI value | Platform | Source |
|--------|-----------|----------|--------|
| `FileKeyProvider` | `file` *(default)* | All | 64-hex-char key file at `--master-key-file` |
| `EnvKeyProvider` | `env` | All | `SONDE_MASTER_KEY` environment variable |
| `DpapiKeyProvider` | `dpapi` | Windows | DPAPI-encrypted blob at `--master-key-file`; decrypted via `CryptUnprotectData` |
| `SecretServiceKeyProvider` | `secret-service` | Linux | D-Bus Secret Service keyring item identified by `--key-label` |

### 10a.3  Backend selection

The `build_key_provider()` function in `gateway.rs` instantiates the
appropriate backend from the parsed CLI.  Platform-specific backends compile
only on their target OS (`#[cfg(windows)]` / `#[cfg(target_os = "linux")]`).
Requesting an unavailable backend on the wrong platform returns
`KeyProviderError::NotAvailable` before any database is opened.

### 10a.4  DPAPI integration (Windows)

`DpapiKeyProvider` reads a binary DPAPI blob from the file system and calls
`CryptUnprotectData` (via `windows-sys`) to decrypt it.  The blob is created
with `protect_with_dpapi()` during initial provisioning.  Decryption is tied
to the Windows user or machine account — moving the blob to another machine or
user silently fails.

### 10a.5  Secret Service integration (Linux)

`SecretServiceKeyProvider` connects to the D-Bus `org.freedesktop.secrets`
endpoint (via `secret-service` crate, which uses `zbus`), unlocks the default
collection, and retrieves a 32-byte binary secret stored under the attributes
`service = "sonde-gateway"` and `account = <label>`.  The label defaults to
`"sonde-gateway-master-key"` and is configurable via `--key-label`.

The provisioning helper `store_in_secret_service()` writes a key into the
keyring during initial deployment.

### 10a.6  Error type

`KeyProviderError` has four variants:

| Variant | Meaning |
|---------|---------|
| `Io(String)` | File or environment I/O failed (file not found, variable not set) |
| `Format(String)` | Key material is present but malformed (wrong length, non-hex characters, wrong byte count) |
| `NotAvailable(String)` | The requested backend is not supported on this platform |
| `Backend(String)` | The backend itself returned an error (DPAPI failure, D-Bus error, keyring locked) |

---

## 11  Concurrency model

The gateway runs a single tokio async runtime:

The runtime has four categories of concurrent tasks. The transport receive loop (main task) accepts inbound frames and spawns a short-lived per-node processing task for each. Two periodic background tasks run on a timer: the session reaper (removes expired sessions from the session map) and the node timeout detector (emits `node_timeout` events for nodes that have missed their expected wake window). Finally, one long-lived handler I/O task exists per active handler process; each reads that handler's stdout and routes `LOG` and `DATA_REPLY` messages.

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

> **Requirements:** GW-0800 (gRPC API), GW-0801 (node management), GW-0802 (program management), GW-0803 (operational commands), GW-0804 (node status), GW-0805 (state export/import), GW-0806 (CLI tool).

The gateway exposes a local gRPC API for administrative operations (GW-0800). A CLI tool (`sonde-admin`) wraps the API for operator use (GW-0806).

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
    rpc ExportState(ExportStateRequest) returns (ExportStateResponse);
    rpc ImportState(ImportStateRequest) returns (Empty);

    // Modem management
    rpc GetModemStatus(Empty) returns (ModemStatus);
    rpc SetModemChannel(SetModemChannelRequest) returns (Empty);
    rpc ScanModemChannels(Empty) returns (ScanModemChannelsResponse);

    // BLE phone pairing
    rpc OpenBlePairing(OpenBlePairingRequest) returns (stream BlePairingEvent);
    rpc CloseBlePairing(Empty) returns (Empty);
    rpc ConfirmBlePairing(ConfirmBlePairingRequest) returns (Empty);
    rpc ListPhones(Empty) returns (ListPhonesResponse);
    rpc RevokePhone(RevokePhoneRequest) returns (Empty);
}
```

The gRPC server runs on a local socket: a **Unix domain socket** on Linux/macOS (default: `/var/run/sonde/admin.sock`) or a **named pipe** on Windows (default: `\\.\pipe\sonde-admin`). No TCP port is exposed. It is implemented with the `tonic` crate.

### 13.2  Key operations

| Operation | gRPC method | Description |
|---|---|---|
| Pair node | `RegisterNode` | Admin/manual operation. Registers key_hint, PSK, and admin node_id. (In the BLE pairing flow, node registration happens automatically via `PEER_REQUEST` processing — see ble-pairing-protocol.md §7.3.) |
| Factory reset | `RemoveNode` | Removes node from registry. |
| Ingest program | `IngestProgram` | Accepts ELF binary + profile. Triggers verification, CBOR encoding, storage. Returns hash or error. |
| Assign program | `AssignProgram` | Sets a node's assigned program. Next WAKE triggers UPDATE_PROGRAM if hash differs. |
| Queue ephemeral | `QueueEphemeral` | Queues a one-shot diagnostic program for a node's next WAKE. |
| Set schedule | `SetSchedule` | Queues an UPDATE_SCHEDULE for a node's next WAKE. |
| Node status | `GetNodeStatus` | Returns latest known state for a node (program hash, battery, ABI version, last seen, active session). |
| Export state | `ExportState` | Serializes gateway state (node registry, program library, and registered identity/phone PSKs, if applicable). Does not include handler routing configuration (deferred). Encrypted with AES-256-GCM using an operator-supplied passphrase. |
| Import state | `ImportState` | Restores node registry, program library, and registered identity/phone PSKs from a previously exported, encrypted bundle. Handler routing configuration is not restored (deferred and not part of the bundle). |
| Modem status | `GetModemStatus` | Returns modem status: radio channel, TX/RX/fail counters, uptime. |
| Set modem channel | `SetModemChannel` | Sets the ESP-NOW radio channel (1–14). |
| Scan channels | `ScanModemChannels` | Scans all WiFi channels for AP activity, returns AP counts and RSSI per channel. |
| Open BLE pairing | `OpenBlePairing` | Opens the phone registration window and sends `BLE_ENABLE` to modem. Server-streaming RPC returning events (passkey, phone connected/disconnected/registered, window closed). |
| Close BLE pairing | `CloseBlePairing` | Closes the registration window and sends `BLE_DISABLE` to modem. |
| Confirm BLE pairing | `ConfirmBlePairing` | Accepts or rejects a Numeric Comparison passkey during BLE pairing. |
| List phones | `ListPhones` | Lists all registered phones with PSK metadata (ID, key hint, label, issue time, status). |
| Revoke phone | `RevokePhone` | Revokes a phone's PSK by phone ID. |

### 13.3  CLI tool (`sonde-admin`)

The CLI wraps the gRPC API:

```
sonde-admin node list
sonde-admin node get <node-id>
sonde-admin node register <node-id> <key-hint> <psk-hex>
sonde-admin node remove <node-id>

sonde-admin program ingest <elf-file> --profile resident|ephemeral
sonde-admin program list
sonde-admin program assign <node-id> <program-hash>
sonde-admin program remove <program-hash>

sonde-admin schedule set <node-id> <interval-seconds>
sonde-admin reboot <node-id>
sonde-admin ephemeral <node-id> <program-hash>

sonde-admin state export <file> [--passphrase <pass>]
sonde-admin state import <file> [--passphrase <pass>]

sonde-admin status <node-id>

sonde-admin modem status
sonde-admin modem set-channel <channel>
sonde-admin modem scan

sonde-admin pairing start [--duration-s <seconds>]
sonde-admin pairing stop
sonde-admin pairing list-phones
sonde-admin pairing revoke-phone <phone-id>
```

All commands support `--format json` for machine-readable output.

---

## 14  Configuration

The gateway is configured via a configuration file (format TBD — TOML recommended for Rust ecosystem). Configuration includes:

| Setting | Description | Default |
|---|---|---|
| `transport` | Transport backend and connection parameters | Required |
| `storage` | Storage backend and connection parameters | Required |
| `admin_socket` | gRPC admin API socket path | `/var/run/sonde/admin.sock` (Linux), `\\.\pipe\sonde-admin` (Windows) |
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

---

## 17  BLE pairing protocol handler

> **Requirements:** GW-1200–GW-1222.

The gateway implements the pairing protocol logic defined in [ble-pairing-protocol.md](ble-pairing-protocol.md). The physical BLE layer (GATT service, advertising, ATT MTU negotiation, indication fragmentation) is hosted by the USB modem (GW-1204, GW-1205); the gateway processes pairing messages relayed over the modem serial protocol: inbound messages arrive as `BLE_RECV` and responses are sent as `BLE_INDICATE` (see §4.2 `UsbEspNowTransport`). `PEER_REQUEST` / `PEER_ACK` frames travel over ESP-NOW, not BLE, and follow the standard transport path.

### 17.1  Gateway identity

On first startup, the gateway generates an Ed25519 keypair from OS CSPRNG and persists the 32-byte seed encrypted at rest using the master key (GW-1200, GW-0601a). A random 16-byte `gateway_id` is generated alongside the keypair and persisted with it (GW-1201). Both values are stable across restarts. The Ed25519 key is converted to X25519 via the standard birational map for ECDH key agreement; low-order points are rejected (GW-1202). The seed and `gateway_id` can be exported and imported via the admin API (`ExportState` / `ImportState`) so that all members of a failover group share the same identity (GW-1203).

### 17.2  BLE message relay

The modem hosts the Gateway Pairing Service GATT service (UUID `0000FE60-…`) and controls BLE advertising. The gateway controls advertising lifetime by sending `BLE_ENABLE` / `BLE_DISABLE` to the modem when the registration window opens or closes (GW-1208). When a phone writes to the Gateway Command characteristic, the modem forwards the raw bytes to the gateway as a `BLE_RECV` serial message. The gateway processes the command and sends any response back via `BLE_INDICATE`; the modem handles fragmentation to fit within the negotiated ATT MTU (GW-1205). Numeric Comparison passkeys are relayed from the modem via `BLE_PAIRING_CONFIRM` and surfaced to the operator through the admin API streaming RPC (GW-1222).

### 17.3  `REQUEST_GW_INFO` handling

On receiving a `REQUEST_GW_INFO` command (BLE command `0x01`) via `BLE_RECV`, the gateway signs (`challenge` ‖ `gateway_id`) with its Ed25519 private key and returns a `GW_INFO_RESPONSE` containing `gw_public_key`, `gateway_id`, and `signature` via `BLE_INDICATE` (GW-1206). This allows the phone to verify gateway identity before proceeding with registration.

### 17.4  Registration window and `REGISTER_PHONE`

The registration window is opened by a physical button hold (≥ 2 s) or by the admin API `OpenBlePairing` RPC. Opening sends `BLE_ENABLE` to the modem; closing (explicit or auto-close after a configurable duration, default 120 s) sends `BLE_DISABLE` (GW-1207, GW-1208). `REGISTER_PHONE` commands received while the window is closed are rejected with `ERROR(0x02)` (GW-1207).

When the window is open and a `REGISTER_PHONE` command arrives (BLE command `0x02`), the gateway: generates a 256-bit phone PSK from OS CSPRNG; derives `phone_key_hint` = `u16::from_be_bytes(SHA-256(psk)[30..32])` (big-endian u16 from the last two bytes of the hash); performs ECDH with the phone's ephemeral X25519 public key; derives an AES key via HKDF-SHA256 (salt = `gateway_id`, info = `"sonde-phone-reg-v1"`); encrypts the response (containing the phone PSK, `phone_key_hint`, and RF channel) with AES-256-GCM (AAD = `gateway_id`); and returns the encrypted `PHONE_REGISTERED` response via `BLE_INDICATE` (GW-1209). The phone PSK is stored with a label, issuance timestamp, and active status. Operators can revoke phone PSKs through the admin API; revoked PSKs are excluded from HMAC verification (GW-1210).

### 17.5  `PEER_REQUEST` processing

`PEER_REQUEST` frames (msg_type `0x05`) arrive over ESP-NOW through the standard transport path, not BLE. Processing follows a multi-stage verification pipeline; any failure at any stage results in silent discard with no `PEER_ACK` sent (GW-1220). The gateway does not apply sequence-number anti-replay checks to `PEER_REQUEST` / `PEER_ACK` frames — these use random nonces (GW-1221).

**Pipeline:**

1. **Key-hint bypass** — For msg_type `0x05`, the gateway bypasses the normal `key_hint` → PSK fast-path lookup and proceeds directly to CBOR parsing (GW-1211).
2. **Decryption** — The `encrypted_payload` is decrypted using ECDH (phone's ephemeral public key + gateway X25519 key) + HKDF-SHA256 (salt = `gateway_id`, info = `"sonde-node-pair-v1"`) + AES-256-GCM (AAD = `gateway_id`). GCM tag failure → discard (GW-1212).
3. **Phone HMAC verification** — The gateway looks up all non-revoked phone PSKs matching `phone_key_hint` and tries each until one produces a valid HMAC. No match → discard (GW-1213).
4. **Frame HMAC verification** — The frame HMAC is verified using the extracted `node_psk`. Mismatch → discard (GW-1214).
5. **Timestamp validation** — The `PairingRequest` timestamp must be within ± 86 400 s of current time. Out of range → discard (GW-1215).
6. **Node ID uniqueness** — The `node_id` must not already be registered. Duplicate → discard (GW-1216).
7. **Key-hint consistency** — The frame header `key_hint` must match the CBOR `node_key_hint`. Mismatch → discard (GW-1217).
8. **Node registration** — The node is registered with `node_id`, `node_key_hint`, `node_psk`, `rf_channel`, `sensors`, and `registered_by` = phone_id (GW-1218). The node registry (§7) stores the new record through the storage trait.

### 17.6  `PEER_ACK` generation

After successful registration, the gateway computes `registration_proof` = HMAC-SHA256(`node_psk`, `"sonde-peer-ack-v1"` ‖ `encrypted_payload`), builds a `PEER_ACK` CBOR message `{1: 0, 2: registration_proof}`, HMACs the frame with `node_psk`, and echoes the `nonce` from the `PEER_REQUEST` header (GW-1219).

### 17.7  Admin session

The admin API exposes `OpenBlePairing` (server-streaming RPC) to open the registration window and enable BLE advertising, `CloseBlePairing` to close it, `ConfirmBlePairing` for Numeric Comparison passkey confirmation, `ListPhones` to enumerate registered phones, and `RevokePhone` to revoke a phone PSK (GW-1222). See §13 for the full gRPC service definition.
