<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Gateway Design Specification

> **Document status:** Draft  
> **Scope:** Architecture and internal design of the Sonde gateway service.  
> **Audience:** Implementers (human or LLM agent) building the gateway.  
> **Related:** [gateway-requirements.md](gateway-requirements.md), [protocol.md](protocol.md), [protocol-crate-design.md](protocol-crate-design.md), [security.md](security.md), [gateway-api.md](gateway-api.md), [gateway-companion-api.md](gateway-companion-api.md)

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
| AEAD | `aes-gcm` crate (RustCrypto) | Pure Rust, audited, no OpenSSL dependency |
| Transport | Abstract trait (ESP-NOW as first adapter) | Decouples protocol logic from radio hardware |
| Storage | Abstract trait | Decouples persistence from storage engine |

---

## 3  Module architecture

The gateway is composed of eleven functional modules grouped in two tiers. The upper (data-path) tier contains: Transport (radio adapter, e.g., ESP-NOW over USB-CDC), Protocol Codec (frame serialization/deserialization), Session Manager (per-node session lifecycle, desired-vs-actual reconciliation, and command dispatch), and Handler Router (forwarding application data to external handler processes). Each module in this tier connects to the next in series. The lower (infrastructure) tier contains: an ESP-NOW Adapter (concrete transport implementation), Node Registry (PSK and node metadata), Program Library (BPF program images and hash identity), Handler Process (handler stdin/stdout management), Admin API (gRPC interface and CLI tool), Connector API (local framed interface for a single control-plane connector app), and BLE Pairing Handler (pairing protocol logic via modem relay). Node Registry and Program Library share a common Storage trait abstraction at the bottom. The architecture diagram below shows the nine core data-path and infrastructure modules; the Admin API, Connector API, and BLE Pairing Handler are described in §13, §13A, and §17 respectively.

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
| **Protocol Codec** | Serialize/deserialize frames (header, CBOR, AES-256-GCM AEAD) | GW-0101, GW-0102, GW-0103, GW-0600, GW-0603 |
| **Session Manager** | Per-node session lifecycle, sequence tracking, command dispatch | GW-0200–0204, GW-0602, GW-1002, GW-1003 |
| **Node Registry** | PSK lookup, node metadata, battery/ABI tracking | GW-0601, GW-0700, GW-0701, GW-0702, GW-0703 |
| **Program Library** | Program storage, verification, chunking, hash identity | GW-0300–0302, GW-0400–0403, GW-1004 |
| **Handler Router** | Route APP_DATA to handler processes by program_hash | GW-0500, GW-0501, GW-0504–0508 |
| **Handler Process** | Manage handler stdin/stdout lifecycle | GW-0502, GW-0503, GW-0506 |
| **Storage** | Persist node registry, program library, configuration | GW-0700, GW-1000, GW-1001 |
| **Admin API** | gRPC admin interface, CLI tool | GW-0800, GW-0801, GW-0802, GW-0803, GW-0804, GW-0805, GW-0806 |
| **Connector API** | Local framed control-plane bridge for a single connector app | GW-0810, GW-0811, GW-0812, GW-0813, GW-0814, GW-0815 |
| **BLE Pairing Handler** | BLE pairing protocol logic via modem relay | GW-1200–GW-1222a |

---

## 4  Transport trait

```rust
/// Opaque address type for the transport layer (e.g., MAC address for ESP-NOW).
pub type PeerAddress = Vec<u8>;

#[async_trait]
pub trait Transport: Send + Sync {
    /// Receive the next inbound frame (blocking until available).
    /// Returns the raw bytes (AEAD-encrypted frame) and the
    /// sender's transport-layer address.
    async fn recv(&self) -> Result<(Vec<u8>, PeerAddress), TransportError>;

    /// Send a frame to a specific peer by transport-layer address.
    async fn send(&self, frame: &[u8], peer: &PeerAddress) -> Result<(), TransportError>;
}
```

The transport returns the sender's address alongside the frame. After the protocol layer authenticates the frame (using `key_hint` → candidate PSK lookup → AES-256-GCM decryption) and identifies the node, the session manager stores the peer address in the session. Responses are sent to the address from the session.

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
- `EVENT_ERROR` → logged as a recoverable display-path warning.
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
6. Send `SET_CHANNEL` with the persisted channel from the database (GW-0808). If no channel is persisted yet, seed the database with the CLI `--channel` value and use that.
7. Wait for `SET_CHANNEL_ACK` (timeout: 2 seconds).
8. Start the health monitor task.

The transport adapter stops at the modem handshake boundary. Gateway-owned display policy lives above the transport: once the handshake completes, the gateway runtime renders `Sonde Gateway v<semver>` into a 128×64 1-bit framebuffer and sends it using the reliable display-transfer subprotocol (`DISPLAY_FRAME_BEGIN` plus eight acknowledged `DISPLAY_FRAME_CHUNK` messages) (GW-1101a). The same gateway-owned rendering path also produces the short-press status pages, the scrolling oversized `Nodes` page, and the timeout-driven return to the default banner (GW-1101b, GW-1101c, GW-1101d). This keeps text layout, page selection, and banner policy in the gateway while preserving the modem's role as a dumb framebuffer sink with local panel power management only.

**Health monitor (GW-1102):**

A background task polls `GET_STATUS` every 30 seconds and logs:
- `tx_fail_count` delta since last poll (warns on rising failures).
- `uptime_s` decrease (indicates unexpected modem reboot).

The health monitor tracks consecutive poll failures. When `poll_status()` returns an error, the counter increments; on success, it resets to zero. After `max_consecutive_failures` (default `DEFAULT_MAX_HEALTH_POLL_FAILURES = 3`) consecutive failures, the monitor logs at `ERROR` level and exits with `true`, signalling the caller that a modem reconnect is needed. The caller (gateway main loop) should then drop the transport and re-execute the startup sequence.

**Error handling (GW-1103):**

On `ERROR` from the modem, the adapter logs the error code and message. If the error is unrecoverable, it sends `RESET` and re-executes the startup sequence.

**Serial disconnect recovery (GW-1103 criteria 3–5):**

When the serial reader task encounters an OS I/O error (e.g. USB-CDC disconnect, Windows error 995), it does not exit permanently. Instead:

1. The reader task logs a warning and enters a reconnection loop.
2. The loop attempts to reopen the serial port with exponential backoff (1 s → 2 s → 4 s → … → 30 s cap).
3. Once the port reopens, the adapter re-executes the startup sequence (`RESET` → `MODEM_READY` → `SET_CHANNEL`), reading the channel from the database (GW-0808) rather than the CLI startup value.
4. After the handshake completes, the gateway re-sends the version banner via the reliable display-transfer subprotocol (GW-1101a).
5. The `recv()` and BLE event channels remain open during reconnection — callers block until the transport recovers.
6. If the port cannot be reopened (e.g. device permanently removed), the backoff loop continues indefinitely; the operator can shut down the gateway via Ctrl-C or service stop.

**Warm reboot recovery (GW-1103 criteria 7–8):**

When the modem firmware reboots without dropping the USB-CDC serial connection (a "warm reboot"), the modem sends an unsolicited `MODEM_READY`. The gateway detects this via an unexpected `MODEM_READY` arriving outside the startup handshake.

The `UsbEspNowTransport` exposes:
- `warm_reboot_notify: Arc<tokio::sync::Notify>` — fired by the reader task when it receives an unexpected `MODEM_READY`, after cancelling all pending operation waiters (channel-ack, status, and scan slots).
- `abort_reader_and_wait()` — aborts the reader task's `JoinHandle` and waits for it to finish so the serial port is released before reconnect.

The gateway main loop `select!`s on `warm_reboot_notify.notified()` alongside the normal shutdown and frame-loop exit paths. On warm reboot notification:

1. The gateway aborts all spawned consumer tasks (`frame_loop`, `ble_loop`, `grpc_handle`) and cancels the health monitor. This releases all `Arc<UsbEspNowTransport>` clones held by those tasks.
2. The gateway calls `abort_reader_and_wait()`, which aborts the transport reader task and waits for it to release the serial port.
3. The local `transport` Arc is dropped; once the remaining clones are gone, the transport's remaining resources are freed.
4. The gateway immediately (no backoff) reopens the serial port and calls `UsbEspNowTransport::new()` with the persisted channel read from the database (GW-0808). All consumer tasks are re-spawned as on first startup.
5. After the handshake completes, the gateway re-sends the version banner via the reliable display-transfer subprotocol (GW-1101a).
6. After successful warm reboot recovery, the exponential backoff counter is reset to its initial value (1 s), so any subsequent serial disconnect starts fresh.

Note: If multiple `MODEM_READY` messages arrive in rapid succession (overlapping warm reboots), `tokio::sync::Notify` coalesces them — at most one notification is delivered. Notifications that arrive while recovery is already in progress are absorbed by the already-pending notify permit and handled in the next reconnect iteration if recovery fails, or discarded if recovery succeeds (the reader task is no longer running and cannot fire further notifications).

**`send()` implementation:**

Constructs a `SEND_FRAME` envelope (`peer_mac || frame_data`) and writes it to the serial port. Does not wait for any modem or radio delivery acknowledgement — fire-and-forget at the radio layer, while still awaiting the serial write as needed. The 250-byte ESP-NOW frame limit is enforced by the modem.

**`recv()` implementation:**

Awaits the next `RECV_FRAME` from the async channel. Returns `(frame_data, peer_mac)`, where `peer_mac` is the `PeerAddress` obtained by converting the modem's 6-byte MAC address at the adapter boundary. RSSI is available but not surfaced through the `Transport` trait (logged internally for diagnostics).

---

## 5  Protocol codec

The protocol codec is provided by the shared `sonde-protocol` crate (see [protocol-crate-design.md](protocol-crate-design.md) for the full crate specification). The gateway uses the same frame format, CBOR message types, and constants as the node. The gateway provides a software AES-256-GCM implementation using the `aes-gcm` RustCrypto crate.

### 5.1  Frame layout

All types, constants, and functions in this section are provided by the `sonde-protocol` crate (see [protocol-crate-design.md](protocol-crate-design.md) for the full API). The gateway-specific code only provides the AES-256-GCM implementation.

The frame is a flat byte array with fields at fixed offsets. The first 11 bytes form the binary header: `key_hint` occupies bytes 0–1 (big-endian u16), `msg_type` is byte 2, and `nonce` occupies bytes 3–10 (big-endian u64). Following the header is the AES-256-GCM ciphertext (encrypted CBOR payload). The final 16 bytes of the frame are the AES-256-GCM authentication tag. The 11-byte header is used as Additional Authenticated Data (AAD). The GCM nonce is constructed as `SHA-256(psk)[0..3] ‖ msg_type ‖ frame_nonce` (12 bytes total).

```
Offset 0:  key_hint    (2 bytes, big-endian)
Offset 2:  msg_type    (1 byte)
Offset 3:  nonce       (8 bytes, big-endian)
Offset 11: ciphertext  (variable, AES-256-GCM encrypted CBOR payload)
Offset -16: tag        (16 bytes, AES-256-GCM authentication tag)
```

### 5.2  Inbound decoding

Uses `sonde_protocol::decode_frame()` → returns `DecodedFrame { header, ciphertext, tag }`. The CBOR payload is **not** available until after AES-256-GCM decryption succeeds.

### 5.3  AES-256-GCM decryption

The gateway performs AEAD decryption for each inbound frame:

1. Parse the 11-byte header to extract `key_hint`, `msg_type`, and `frame_nonce`.
2. Look up candidate PSKs by `key_hint` from the node registry (or phone PSK registry for `PEER_REQUEST`).
3. For each candidate PSK, reconstruct the GCM nonce as `SHA-256(psk)[0..3] ‖ msg_type ‖ frame_nonce` (12 bytes) and attempt AES-256-GCM-Open with the 11-byte header as AAD.
4. If decryption succeeds (GCM tag verifies), the node is identified by the matching key and the plaintext CBOR payload is returned.
5. If no candidate key produces a valid decryption, the frame is silently discarded.

### 5.4  Outbound encoding

Uses `sonde_protocol::encode_frame()` with AES-256-GCM encryption. The 11-byte header is used as AAD.

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

Every inbound frame goes through a sequential pipeline. First the binary header is parsed (extracting `key_hint`, `msg_type`, and `nonce`). The `key_hint` is used to look up candidate node keys from the registry; if none are found the frame is silently discarded. The gateway tries AES-256-GCM-Open with each candidate key; if none succeed, the frame is discarded. Once the node is identified by its matching key, the frame is dispatched based on `msg_type`. A `WAKE` frame causes a new session to be created (or an existing one replaced), a `COMMAND` response to be encoded and sent back, and a `node_online` event to be emitted. If the WAKE contains a `blob` field (CBOR key 10), the gateway extracts it and routes it to the handler as a DATA message using the same flow as APP_DATA (§9.4); the handler's reply, if any, is always stored as deferred data for the next cycle (§6.3a) regardless of the `delivery` field in the reply. Post-WAKE frames (`GET_CHUNK`, `PROGRAM_ACK`, `APP_DATA`) require an active session and a matching sequence number; they are then routed to the program library, node registry, or handler process as appropriate. Any error at any step results in a silent discard — no error response is ever sent to the node.

```
recv frame
  │
  ├── parse header (key_hint, msg_type, nonce)
  │
  ├── lookup candidate keys by key_hint
  │     └── no keys → discard (GW-1002)
  │
  ├── try AES-256-GCM-Open with each candidate key
  │     └── none succeed → discard (GW-0600)
  │
  ├── identify node (bound to matching key)
  │
  ├── if WAKE:
  │     ├── decode CBOR payload → Wake fields
  │     ├── create/replace session for this node
  │     ├── generate random starting_seq
  │     ├── get current UTC timestamp_ms
  │     ├── determine command (check program_hash, pending actions)
  │     ├── if command is NOP and deferred data exists for node:
  │     │     └── include deferred data as `blob` (key 10) in COMMAND, clear store
   │     ├── encode COMMAND response
   │     ├── send response (echoing wake nonce)
   │     ├── update node registry (battery_mv, firmware_abi_version, firmware_version)
   │     ├── update runtime node observations (`last_seen`)
   │     ├── if WAKE contains `blob`:
  │     │     ├── route to handler as DATA message (§9.4)
  │     │     └── store handler reply as deferred data (§6.3a)
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

### 6.3a  Deferred reply storage

The gateway maintains a RAM-only map of `node_id → Vec<u8>` for deferred replies. At most one reply is stored per node — if a new reply arrives before the previous one is delivered, the latest reply wins. The stored data is cleared after successful delivery (inclusion in a COMMAND `blob`). Deferred data is only delivered on NOP commands; if the node receives an `UPDATE_PROGRAM`, `UPDATE_SCHEDULE`, or other non-NOP command, the deferred data remains stored until the next NOP cycle. The map is not persisted — gateway restarts discard all pending deferred replies.

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

The node registry persists durable node metadata through the storage trait. Runtime-only observation data is kept separately in memory.

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
    pub firmware_version: Option<String>,
    pub last_battery_mv: Option<u32>,
    pub admin_node_id: String,  // opaque human-readable ID for handler API
}
```

`NodeRecord` is used primarily for durable registry state. The Rust struct currently retains a `last_seen` field for in-memory compatibility, but it is not storage-backed, is initialized as `None` on reads/imports, and is not used as the source of truth for admin status or timeout detection. The runtime `last_seen` data lives in the separate in-memory observation map below.

### 7.1a  Runtime node observations

The gateway maintains a separate in-memory map for per-node runtime observations:

```rust
pub struct RuntimeNodeState {
    pub last_seen: Option<SystemTime>,
}
```

The runtime state is keyed by `NodeId` and updated only after a valid `WAKE` is processed. It is cleared on gateway restart and is excluded from SQLite persistence and state export/import. Admin read paths and timeout detection merge durable `NodeRecord` data with this runtime map.

### 7.2  Key lookup

```rust
pub fn lookup_by_key_hint(&self, key_hint: u16) -> Vec<&NodeRecord>
```

Returns all nodes matching the `key_hint`. The caller tries AES-256-GCM decryption with each candidate's PSK (GW-0601).

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
    pub abi_version: Option<u32>,        // ABI version (None = any ABI)
    pub source_filename: Option<String>, // original filename passed at ingestion time
}

pub enum VerificationProfile {
    Resident,
    Ephemeral,
}
```

The `source_filename` is operator-supplied metadata: the original source filename
(basename, not full path) passed to `IngestProgram`. It is stored in the `programs`
table as a nullable `TEXT` column (`source_filename TEXT`) and returned in
`ListPrograms` responses. It does NOT affect the program hash — the hash covers
only the CBOR image.

### 8.2  Program ingestion

1. Accept pre-compiled BPF ELF (GW-0400).
2. Reject ephemeral programs that declare maps — ephemeral programs are stateless and must not carry map definitions (GW-0401 criterion 5). Detected via a lightweight scan of ELF section headers for map-backed sections (`.maps`/`maps`, `.rodata`, `.data`, `.bss`) before invoking prevail. The scan also validates `e_machine == EM_BPF` to avoid false positives on non-BPF ELFs.
3. Extract programs from the `sonde` ELF section only — `elf.get_programs("sonde", "", &mut platform)` filters to the `SEC("sonde")` section, ignoring helper functions or other code in unrelated sections (GW-0401 criterion 6).
4. Verify with `prevail-rust` using `SondePlatform` — a custom Prevail platform that defines helper prototypes for sonde helpers 1–16 (GW-0401, GW-0404). Prevail's loader resolves ELF map relocations to `LDDW src=1, imm=<map_index>`. When verification fails, per-instruction diagnostic notes are collected and included in the `IngestProgram` gRPC error message (GW-1305). The first line after the summary is always the output of `find_first_error()` (so clients can reliably extract it), followed by any unmarshal-stage notes, then as much of the invariant state from `print_invariants()` as fits within the gRPC trailer size limit. When truncation is required, the gateway appends an explicit truncation marker to the diagnostics so clients can detect that the invariant dump was cut off.
5. Extract bytecode and map definitions from the matched program.
6. Extract initial data for global variable maps (GW-0405): scan ELF section headers for `.rodata`, `.data`, and `.bss` in section-header order.  For `SHT_PROGBITS` sections (`.rodata`, `.data`), copy the section content; for `SHT_NOBITS` sections (`.bss`), emit empty data (maps are zero-initialized by the node). The ordering matches the `map_type == 0` descriptors produced by Prevail's `add_global_variable_maps()`, so initial data entries correspond 1:1 to map definitions.
7. Encode as CBOR program image using `sonde_protocol::ProgramImage::encode_deterministic()`. Each map definition with non-empty initial data includes `initial_data` (key 5) in its CBOR map. See [protocol-crate-design.md §7](protocol-crate-design.md) and [protocol.md § Program image format](protocol.md#program-image-format).
8. Enforce size limits on the CBOR image: 4 KB resident, 2 KB ephemeral (GW-0403).
9. Compute `program_hash` using `sonde_protocol::program_hash()` with the gateway's SHA-256 provider (GW-0402).
10. Store in library. Verification and encoding complete at ingestion time — chunk serving is immediate (GW-0400).

### 8.2.1  Sonde verifier platform (`SondePlatform`)

The gateway uses a custom Prevail platform (`SondePlatform`) instead of `LinuxPlatform` for BPF program verification (GW-0404). Sonde assigns different semantics to helper IDs 1–16 than Linux BPF does, so using `LinuxPlatform` causes programs that call sonde helpers to fail verification or be verified under incorrect (Linux) helper semantics.

`SondePlatform` wraps `LinuxPlatform` via composition — it delegates ELF map parsing, map descriptor management, and conformance group handling to the inner `LinuxPlatform`, and overrides helper-related methods with sonde-specific prototypes.

**Module:** `crate::sonde_platform` (`crates/sonde-gateway/src/sonde_platform.rs`)

**Helper prototypes (IDs 1–16):** each prototype declares its name, return type, argument types (up to 5), and whether any argument is a pointer to readable or writable memory. The prototypes match the signatures defined in `test-programs/include/sonde_helpers.h`.

| ID | Name | Return | Args |
|----|------|--------|------|
| 1 | `i2c_read` | `Integer` | `(handle, *writable, size)` |
| 2 | `i2c_write` | `Integer` | `(handle, *readable, size)` |
| 3 | `i2c_write_read` | `Integer` | `(handle, *readable, size, *writable, size)` |
| 4 | `spi_transfer` | `Integer` | `(handle, *readable_or_null, *writable_or_null, size)` |
| 5 | `gpio_read` | `Integer` | `(pin)` |
| 6 | `gpio_write` | `Integer` | `(pin, value)` |
| 7 | `adc_read` | `Integer` | `(channel)` |
| 8 | `send` | `Integer` | `(*readable, size)` |
| 9 | `send_recv` | `Integer` | `(*readable, size, *writable, size, timeout)` |
| 10 | `map_lookup_elem` | `PtrToMapValueOrNull` | `(*map, *key)` |
| 11 | `map_update_elem` | `Integer` | `(*map, *key, *value)` |
| 12 | `get_time` | `Integer` | `()` |
| 13 | `get_battery_mv` | `Integer` | `()` |
| 14 | `delay_us` | `Integer` | `(microseconds)` |
| 15 | `set_next_wake` | `Integer` | `(seconds)` |
| 16 | `bpf_trace_printk` | `Integer` | `(*readable, size)` |

**Program type:** all ELF sections are treated as `"sonde"` program type with a 16-byte context descriptor matching `struct sonde_context`.

**Map type mapping:** sonde's `BPF_MAP_TYPE_ARRAY` (value 1) maps to an array map type. Map type 0 (global variable maps from `.rodata`/`.data`/`.bss` sections) maps to an array map type so that `LDDW` references produce `shared`-typed value pointers. Unknown map types are treated as generic maps.

#### 8.2.1.1  Global variable map descriptor sync (`sync_map_descriptors`)

`prevail-rust` promotes `.rodata`, `.data`, and `.bss` ELF sections to map descriptors (map_type 0) during `ElfObject::get_programs`. These descriptors are added to the ELF loader's internal state via `add_global_variable_maps()`, but are **not** propagated through `parse_maps_section` to the platform. As a result, `SondePlatform::get_map_descriptor()` returns `None` for global variable map FDs, causing the verifier to type `LDDW`-loaded registers as `ctx` instead of `shared` — which leads to spurious verification failures.

**Workaround:** after calling `get_programs`, the gateway copies the full set of map descriptors from `RawProgram.info.map_descriptors` into `SondePlatform` via `sync_map_descriptors(&[EbpfMapDescriptor])`. `SondePlatform` maintains a mirror `Vec<EbpfMapDescriptor>` that is checked first in `get_map_descriptor()`, falling back to the inner `LinuxPlatform` for maps parsed via `parse_maps_section`.

**Section name prefix matching:** the initial data extraction (step 5 above) uses prefix matching rather than exact equality for global data section names. Prevail promotes sections such as `.rodata.str1.1` and `.data.rel.ro` — not just `.rodata` and `.data` — so the scanner matches any section whose name equals or starts with a known global data prefix followed by `.`.

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
    pub reply_timeout: Option<Duration>,
    pub working_dir: Option<String>,
}
```

Multiple program hashes can map to the same handler (GW-0504).

> **Shared state (D-485):** The `Gateway` instance MUST be wired so
> that the admin API and the handler router share the same
> `pending_commands`/`SessionManager`/`HandlerRouter`. In production,
> construct the gateway via `new_with_pending`, passing the shared
> `Arc<tokio::sync::RwLock<HandlerRouter>>` built from the database at
> startup (GW-1407). The same `Arc` is given to the `AdminService` for
> live reload (GW-1404, §19.5). Do **not** create an independent
> `pending_commands` map or `HandlerRouter` for any code path. D-485
> occurred when a constructor created a separate `pending_commands`
> map, breaking the admin→engine path; that pattern is forbidden. The
> convenience constructor `new_with_handler` currently allocates its
> own internal `pending_commands`/`SessionManager` and is only safe to
> use in contexts that do not expose the admin API or otherwise require
> shared state.

### 9.2  Routing

On receiving APP_DATA from a node:

1. Look up the node's current `program_hash` in the handler config.
2. If no match and no catch-all → do not send APP_DATA_REPLY to the node (GW-0504).
3. If match → forward to handler as a DATA message (GW-0505).

### 9.3  Handler process lifecycle

Each handler config spawns a handler process (GW-0503):

- Process is started on first message.
- stdin/stdout communicate via 4-byte big-endian length prefix + CBOR (GW-0502).
- stderr is captured and forwarded to the gateway log at WARN level (one log entry per line) so that handler startup errors (e.g. missing dependencies, syntax errors) are visible to the operator.
- If the process stays alive → reuse for subsequent messages.
- If the process exits with code 0 → respawn on next message.
- If the process exits with non-zero → log error, no APP_DATA_REPLY to node.

### 9.4  Message flow

When an `APP_DATA` frame arrives from a node it is routed to the matching handler process by `program_hash`. The gateway constructs a DATA message (containing `request_id`, `node_id`, `program_hash`, the opaque data blob, and a Unix timestamp) and writes it as a length-prefixed CBOR message to the handler's stdin. The gateway then reads the handler's stdout for a `DATA_REPLY` message whose `request_id` matches the request. If the reply contains a non-empty data field, the gateway sends an `APP_DATA_REPLY` back to the node; an empty data field means no reply is sent. The handler may also write `LOG` messages at any time, which the gateway routes to its own log.

The `DATA_REPLY` message supports an optional `delivery` field (CBOR key 4). When `delivery` = 1 and the data field is non-empty, the gateway stores the reply as deferred data (§6.3a) instead of sending an immediate `APP_DATA_REPLY`. The deferred data is delivered as a `blob` in the next NOP COMMAND. When `delivery` is absent or 0, the reply is sent immediately as before. For DATA messages originating from a WAKE `blob`, the gateway forces deferred delivery regardless of the `delivery` field in the reply — there is no active session to send an immediate reply to at WAKE time.

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
  │     ├── if data is zero-length → do not send reply
  │     ├── if delivery == 1 (or WAKE-originated) → store as deferred data (§6.3a)
  │     └── otherwise → send APP_DATA_REPLY to node
  │
  └── (handler may also write LOG messages at any time → route to gateway log)
```

### 9.5  Event messages

The session manager emits lifecycle events to handlers (GW-0507):

| Event | When | Details |
|---|---|---|
| `node_online` | WAKE processed | `battery_mv`, `firmware_abi_version`, `firmware_version` |
| `program_updated` | PROGRAM_ACK received | `program_hash` |
| `node_timeout` | Node missed expected wake | `last_seen`, `expected_interval_s` (`last_seen` comes from runtime node observations) |

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

    // Handler configuration (GW-1401)
    async fn add_handler(&self, record: &HandlerRecord) -> Result<bool, StorageError>;
    async fn remove_handler(&self, program_hash: &str) -> Result<bool, StorageError>;
    async fn list_handlers(&self) -> Result<Vec<HandlerRecord>, StorageError>;
}
```

The storage trait is async to support different backends (file, SQLite, network) without blocking the event loop.

Storage implementations SHOULD encrypt PSK material at rest (GW-0601a). The `NodeRecord.psk` field contains the raw 256-bit key — implementations are responsible for encrypting it before persisting and decrypting on read. The storage trait itself is agnostic to the encryption mechanism.

### 10.1  Gateway configuration storage (GW-0808)

The storage trait includes methods for persisting gateway-level configuration values such as the ESP-NOW radio channel:

```rust
// Gateway config (GW-0808)
async fn get_config(&self, key: &str) -> Result<Option<String>, StorageError>;
async fn set_config(&self, key: &str, value: &str) -> Result<(), StorageError>;
```

The `SqliteStorage` backend stores these in a `gateway_config` table:

```sql
CREATE TABLE IF NOT EXISTS gateway_config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

The `espnow_channel` key stores the current radio channel. The `--channel` CLI flag seeds this value on first startup only; subsequent changes via `SetModemChannel` update the database entry. On modem reconnect and BLE pairing, the persisted value is read from the database.

---

## 10a  Master key provider

The 32-byte master key (used by `SqliteStorage` to encrypt PSKs and phone PSKs
at rest) is loaded at startup via a `KeyProvider`
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

## 11A  Operational logging

> **Requirements:** GW-1300 (lifecycle events), GW-1301 (modem transport state), GW-1302 (frame-level debug traces), GW-1306 (service-mode logging), GW-1307 (error diagnostic observability), GW-1308 (handler pipeline logging).

The gateway uses the `tracing` crate for structured, levelled logging. All log entries use key=value fields for machine-parseability. Log levels follow a consistent policy:

| Level | Usage |
|---|---|
| `ERROR` | Unrecoverable failures: handler crash, storage write failure, serial disconnect |
| `WARN` | Recoverable anomalies: malformed CBOR, AEAD authentication failure, ABI mismatch, modem tx failures |
| `INFO` | Operator-relevant lifecycle events: node WAKE, COMMAND selected, session create/expire, PEER_REQUEST processed, PEER_ACK frame encoded, modem transport state transitions |
| `DEBUG` | Developer diagnostics: individual modem frames (send/recv), channel-full drops, health polls |

### 11A.0a  Build-type–aware log levels (GW-1304)

The gateway compiles in all log levels (up to TRACE) in both debug and release builds, enabling operators to enable debug logging on release binaries via `RUST_LOG` without recompilation.

**Compile-time filtering:**

| Build profile | Cargo feature | Effect |
|---|---|---|
| `dev` (debug) | `max_level_trace` | All levels compiled in |
| `release` | `max_level_trace` | All levels compiled in (no compile-time stripping) |

This differs from the node firmware, which uses `release_max_level_warn` to minimize code size on embedded targets. The gateway runs on a host with ample resources and benefits from runtime-configurable debug output.

**Runtime default:**

The `EnvFilter` fallback in the gateway binary varies by build type:

```rust
#[cfg(debug_assertions)]
const DEFAULT_FILTER: &str = "sonde_gateway=info";
#[cfg(not(debug_assertions))]
const DEFAULT_FILTER: &str = "sonde_gateway=warn";
```

`RUST_LOG` overrides the default in both builds. In release, `RUST_LOG=sonde_gateway=debug` enables DEBUG output (all levels up to TRACE are compiled in). The runtime default of `sonde_gateway=warn` keeps release output quiet unless the operator explicitly requests more detail.

### 11A.1  Engine lifecycle logging

The `Gateway::process_frame` / `handle_wake` / `handle_peer_request` methods emit structured `info!()` entries for:

- **PEER_REQUEST processed** — `node_id`, `key_hint`, `result` (`"registered"` or `"duplicate"`)
- **PEER_ACK frame encoded** — `node_id`
- **WAKE received** — `node_id`, `seq`, `battery_mv`
- **COMMAND selected** — `node_id`, `command_type`
- **Session created** — `node_id` (emitted in `handle_wake` after `create_session`)
- **Session expired** — `node_id` (emitted in `SessionManager::reap_expired`)

### 11A.2  Modem transport state logging

The gateway binary (`bin/gateway.rs`) logs modem transport state transitions at `INFO` level:

- `"modem serial connected"` — after the serial port is opened
- `"modem transport ready"` — after successful startup handshake
- `"modem disconnecting"` — when a transport subsystem exits unexpectedly
- `"modem disconnected — reconnecting"` — before each reconnection backoff sleep, with `backoff_s` indicating the delay in seconds

### 11A.3  Frame-level debug traces

The `UsbEspNowTransport` logs each frame at `DEBUG` level in `dispatch_message` (recv path) and `Transport::send` (send path), with fields `msg_type` (decoded from the protocol frame header, e.g., `"WAKE"`, `"COMMAND"`), `peer_mac`, and `len`. The send-path log is emitted only after a successful write to the modem.

### 11A.4  Service-mode logging and monitoring (GW-1306)

When the gateway runs as a Windows service, three logging sinks are active:

1. **File sink** — writes to `<db-path>.log` (e.g., `gateway.db.log` next to the database). The file sink uses the same build-type–aware `EnvFilter` default as console mode (§ 11A.0a): `sonde_gateway=warn` in release, `sonde_gateway=info` in debug. If the log file cannot be created, opened, or written to (at startup or during runtime), the gateway logs an `ERROR` to the ETW sink and continues without file logging.

2. **ETW sink** — registers provider name `sonde-gateway`. The sink is unfiltered; all events up to the compile-time maximum level are forwarded to any active ETW tracing session. Operators use standard ETW tooling (`logman`, `tracelog`, WPR) to capture and filter events.

3. **Runtime log-level reload** — operators can change the file sink filter without restarting the service. The gateway watches for a platform-appropriate reload signal (e.g., `SERVICE_CONTROL_PARAMCHANGE` on Windows) and re-reads `RUST_LOG` from the environment, applying the new `EnvFilter` within 5 seconds. This uses the `tracing-subscriber` `reload` layer.

### 11A.5  Error diagnostic observability (GW-1307)

When the gateway encounters an error at a user-facing or operator-visible boundary, the error log entry or gRPC error response includes four pieces of diagnostic context:

1. **Operation name** — the high-level action that failed (e.g., `"IngestProgram"`, `"serial port open"`, `"import_state"`).
2. **Triggering input / parameters** — non-sensitive metadata such as `program_hash`, file path, environment variable name, or port name. Secret key material and credentials are never included.
3. **Underlying error** — the specific error from the subsystem (OS error code, verifier instruction, SQLite status, CBOR parse error).
4. **Actionable guidance** — where a corrective action is known, a short human-readable hint (e.g., `"check COM port permissions"`, `"re-upload program"`, `"verify passphrase"`).

**Boundary coverage:**

| Boundary | Implementation | Diagnostic fields |
|---|---|---|
| Program verification | `admin.rs` — `IngestProgram` | program name, verifier instruction label + error description (GW-1305) |
| Program assignment | `admin.rs` — `AssignProgram` | `program_hash`, `"program not found"` guidance |
| Key provider (file) | `FileKeyProvider::load` | file path, OS error, `"create key file"` guidance |
| Key provider (env) | `EnvKeyProvider::load` | variable name, `"set environment variable"` guidance |
| Storage open | `SqliteStorage::open` | database path, SQLite error, `"check directory permissions"` guidance |
| State export/import | `export_state` / `import_state` | operation name, variant-specific guidance (empty passphrase, decryption failure, corrupt bundle) |
| Ephemeral dispatch | `QueueEphemeral` | `program_hash`, verification profile |

Errors that cross the gRPC boundary use `tonic::Status` with the diagnostic message in the status detail string. Errors that are internal (e.g., AEAD decryption failures on the radio protocol) are logged but never sent to the node — the silent-discard policy (§12) still applies to the radio interface.

### 11A.6  Handler pipeline logging (GW-1308)

The gateway logs the complete APP_DATA handler pipeline at INFO level so operators can trace data flow from node to handler process and back. Each log entry uses structured `tracing` fields.

| Event | Level | Module | Structured fields | AC |
|---|---|---|---|---|
| APP_DATA received | INFO | `engine.rs` | `node_id`, `program_hash`, `len` | AC1 |
| APP_DATA dropped (no program hash) | WARN | `engine.rs` | `node_id` | AC6 |
| APP_DATA dropped (no handler match) | WARN | `engine.rs` | `node_id`, `program_hash`, `handler_count` | AC6 |
| Handler matched | INFO | `engine.rs` | `program_hash`, `command` | AC2 |
| Handler invoked | INFO | `engine.rs` | `command` | AC3 |
| Handler replied | INFO | `engine.rs` | `len` | AC4 |
| Handler exited | INFO / ERROR | `handler.rs` | `code` (exit code) | AC5 |
| Handler stderr line | WARN | `handler.rs` | `handler` (command), line text | AC7 |

The handler-exited event is emitted when `ensure_running()` detects via `try_wait()` that a previously running handler process has terminated. Clean exits (code 0) are logged at INFO; non-zero exits at ERROR.

---

## 12  Error handling

All protocol errors result in **silent discard** — no error response is sent to the node (security.md §6). Errors are logged internally for operational monitoring.

| Error | Behavior |
|---|---|
| No key matches `key_hint` | Discard. Log at debug level. |
| AEAD authentication failure | Discard. Log at debug level. |
| Wrong sequence number / no active session | Discard. Log at info level. |
| Malformed CBOR (post-auth) | Discard. Log at warn level. |
| Unexpected `msg_type` | Discard. Log at warn level. |
| APP_DATA with no `current_program_hash` | Discard. Log at warn level. |
| APP_DATA with no matching handler | Discard. Log at warn level (includes `program_hash` and `handler_count`). |
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
    rpc ShowModemDisplayMessage(ShowModemDisplayMessageRequest) returns (Empty);

    // BLE phone pairing
    rpc OpenBlePairing(OpenBlePairingRequest) returns (stream BlePairingEvent);
    rpc CloseBlePairing(Empty) returns (Empty);
    rpc ConfirmBlePairing(ConfirmBlePairingRequest) returns (Empty);
    rpc ListPhones(Empty) returns (ListPhonesResponse);
    rpc RevokePhone(RevokePhoneRequest) returns (Empty);

    // Handler management (GW-1402)
    rpc AddHandler(AddHandlerRequest) returns (Empty);
    rpc RemoveHandler(RemoveHandlerRequest) returns (Empty);
    rpc ListHandlers(Empty) returns (ListHandlersResponse);
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
| Node status | `GetNodeStatus` | Returns latest known state for a node (program hash, battery, ABI version, runtime last seen, active session). Runtime `last_seen` is cleared on gateway restart. |
| Export state | `ExportState` | Serializes gateway state (node registry, program library, registered identity/phone PSKs, and handler configuration). Encrypted with AES-256-GCM using an operator-supplied passphrase. |
| Import state | `ImportState` | Restores node registry, program library, registered identity/phone PSKs, and handler configuration from a previously exported, encrypted bundle. If the bundle lacks handler records (older version), existing handlers are preserved. |
| Modem status | `GetModemStatus` | Returns modem status: radio channel, TX/RX/fail counters, uptime. |
| Set modem channel | `SetModemChannel` | Sets the ESP-NOW radio channel (1–14). Persists the new channel in the database (GW-0808). |
| Scan channels | `ScanModemChannels` | Scans all WiFi channels for AP activity, returns AP counts and RSSI per channel. |
| Show transient modem display text | `ShowModemDisplayMessage` | Renders 1–4 gateway-supplied text lines on the modem display for 60 s, then restores the normal gateway banner (GW-0809). |
| Open BLE pairing | `OpenBlePairing` | Opens an admin-initiated phone registration window and sends `BLE_ENABLE` to the modem. Server-streaming RPC returning pairing events (passkey, phone connected/disconnected/registered, window closed). |
| Close BLE pairing | `CloseBlePairing` | Closes the active BLE pairing session and sends `BLE_DISABLE` to the modem. |
| Confirm BLE pairing | `ConfirmBlePairing` | Accepts or rejects a Numeric Comparison passkey during an admin-initiated BLE pairing session. |
| List phones | `ListPhones` | Lists all registered phones with PSK metadata (ID, key hint, label, issue time, status). |
| Revoke phone | `RevokePhone` | Revokes a phone's PSK by phone ID. |
| Add handler | `AddHandler` | Registers a handler for a `program_hash` (GW-1402). Validates hash format. Returns `ALREADY_EXISTS` on duplicate. Triggers live reload. |
| Remove handler | `RemoveHandler` | Removes handler for given `program_hash` (GW-1402). Returns `NOT_FOUND` if no match. Terminates running process. Triggers live reload. |
| List handlers | `ListHandlers` | Returns all configured handlers (GW-1402). |

### 13.3  CLI tool (`sonde-admin`)

The CLI wraps the gRPC API:

```
sonde-admin node list
sonde-admin node get <node-id>
sonde-admin node register <node-id> <key-hint> <psk-hex>
sonde-admin node remove <node-id>

sonde-admin program ingest <elf-file> --profile resident|ephemeral
# <elf-file> is a BPF ELF object file (required in release/production builds).
# A pre-encoded CBOR image is only accepted in debug/development builds.
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
sonde-admin modem display <line> [<line> ...]

sonde-admin pairing start [--duration-s <seconds>]
sonde-admin pairing stop
sonde-admin pairing list-phones
sonde-admin pairing revoke-phone <phone-id>

sonde-admin handler add <program-hash> <command> [args...] [--working-dir <path>]
sonde-admin handler remove <program-hash>
sonde-admin handler list
```

All commands support `--format json` for machine-readable output.

---

## 13A  Control-plane connector

> **Requirements:** GW-0810 (connector API), GW-0811 (desired-state ingress), GW-0812 (upstream actual-state/status), GW-0813 (upstream application data), GW-0814 (transport abstraction), GW-0815 (loss observability).

The gateway exposes a second local integration surface for a single control-plane
connector application that bridges the gateway to an external control plane. The
connector API is specified in [gateway-companion-api.md](gateway-companion-api.md).
Unlike the operator-focused `GatewayAdmin` gRPC surface, the connector API is a
local framed-message interface whose downstream transport is intentionally left
outside the gateway core.

### 13A.1  Local connector transport and framing

The connector server uses the same platform-specific local transports as the
admin API:

- **Unix/macOS:** Unix domain socket (default `/var/run/sonde/connector.sock`)
- **Windows:** Named pipe (default `\\.\pipe\sonde-connector`)

No TCP listener is opened. The connector interface is not a second admin RPC
surface and it is not modeled as cloud-vendor-specific transport inside the
gateway. Each connector message is written as:

```text
+-------------------------------+
| Length (4 bytes, big-endian)  |
| Message bytes (Length bytes)  |
+-------------------------------+
```

The message bytes contain the Sonde-defined connector protocol payload. The
gateway and the external control plane interpret that payload; the connector
application only transports framed messages between the local socket and the
configured external transport.

### 13A.2  Desired-state ingestion and reconciliation path

Connector ingress messages target exactly one addressable entity: the gateway
itself or one registered node. Each message carries the complete desired state
for that entity. The gateway persists the new desired state, replaces any older
desired state for the same entity, and then reconciles desired state against:

1. the latest actual state already recorded for that entity,
2. the node registry and program library,
3. the gateway's normal `pending_commands` state, and
4. any gateway-scoped desired state that influences node command production.

The connector path therefore does not provide imperative cloud-originated
operations such as `QueueReboot` or `AssignProgram`. Those remain internal
effects that fall out of gateway reconciliation when desired state differs from
actual state.

### 13A.3  Upstream actual-state, status, and application-data path

When the gateway accepts a node `WAKE`, it updates the node's latest-known
actual state and emits an upstream connector message describing that state
change. When the gateway accepts node-originated application data, it emits a
separate upstream connector message containing the opaque payload bytes plus the
gateway metadata needed by the control plane to associate the data with the
originating node and program.

For a `WAKE` carrying a piggybacked `blob`, the gateway emits the actual-state
update first and the application-data message second. Application-data egress is
informational only; it does not replace the existing handler reply path used for
node `send_recv()` responses.

### 13A.4  External transport boundary and loss signaling

The gateway does not know whether the external control-plane transport is Azure
Service Bus, Kafka, NATS, or another asynchronous broker. The connector
application owns that transport-specific adaptation. The gateway design assumes
an asynchronous, store-and-forward connector path and requires detectable loss
or desynchronization to be surfaced to operators rather than silently ignored.

The gateway therefore treats connector health as gateway status. A connector
delivery failure that could make desired state or upstream status stale must be
reported through gateway-visible status, logging, or both so operators know that
reconciliation against the control plane may need manual review.

---

## 14  Configuration

The gateway is configured via a configuration file (format TBD — TOML recommended for Rust ecosystem). Configuration includes:

| Setting | Description | Default |
|---|---|---|
| `transport` | Transport backend and connection parameters | Required |
| `storage` | Storage backend and connection parameters | Required |
| `admin_socket` | gRPC admin API socket path | `/var/run/sonde/admin.sock` (Linux), `\\.\pipe\sonde-admin` (Windows) |
| `connector_socket` | Local connector API socket path | `/var/run/sonde/connector.sock` (Linux), `\\.\pipe\sonde-connector` (Windows) |
| `connector_max_message_size` | Maximum connector API frame size | `1048576` (1 MB) |
| `handlers` | Handler routing table (program_hash → command) | `[]` |
| `session_timeout_s` | Session inactivity timeout | `30` |
| `node_timeout_multiplier` | Multiple of node's schedule interval before `node_timeout` event | `3` |
| `max_message_size` | Maximum handler API message size | `1048576` (1 MB) |
| `log_level` | Logging verbosity | `info` |

---

## 14A  Build metadata

> **Requirement:** GW-1303

Both host binaries (`sonde-gateway` and `sonde-admin`) embed the git commit hash at build time using a `build.rs` script. This mirrors the approach already used by `sonde-node` firmware.

### 14A.1  Build script

Each crate's `build.rs` emits a `cargo:rustc-env=SONDE_GIT_COMMIT=<hash>` directive:

1. Check the `SONDE_GIT_COMMIT` environment variable (set by CI with the full SHA).
2. If unset, run `git rev-parse --short HEAD` to obtain the short hash.
3. Normalise to 7 characters for consistency between CI and local builds.
4. Fall back to `"unknown"` if git is unavailable.

Rebuild triggers watch `.git/HEAD`, the resolved branch ref, and `.git/packed-refs`.

### 14A.2  Version string

The clap `#[command]` attribute uses `concat!()` to build a compile-time version string:

```rust
#[command(version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("SONDE_GIT_COMMIT"), ")"))]
```

This produces output like `sonde-gateway 0.3.0 (a1b2c3d)`.

### 14A.3  Startup log

The gateway emits the version string in its first `info!()` log line so that operators can identify the running build from log output.

---

## 15  Startup sequence

1. Load configuration.
2. Initialize storage backend.
3. Load node registry and program library from storage.
4. Initialize transport (e.g., open ESP-NOW interface, or for USB modem: open serial port → `RESET` → `MODEM_READY` → `SET_CHANNEL`; see §4.2). The channel is read from the database (GW-0808); if no value is persisted, the CLI `--channel` flag seeds the database.
5. If a modem transport is active, render `Sonde Gateway v<semver>` using the gateway crate version string and send it to the modem using the reliable display-transfer subprotocol (GW-1101a). Recoverable modem `EVENT_ERROR` faults on this path are logged as warnings; however, exhausting the ACK retry budget for the reliable transfer is treated as a modem transport failure and enters the normal recovery path.
6. If `--handler-config` is provided, bootstrap handlers from YAML into the database (GW-1405, §19.6).
7. Load handler configuration from database and build `HandlerRouter` (GW-1401, GW-1407). The router is always built, even if no handlers exist. Wrap in `Arc<tokio::sync::RwLock<HandlerRouter>>` for shared access.
8. Start gRPC admin API server, passing the shared `HandlerRouter` reference to the `AdminService` for live reload (GW-1404).
9. Start the local connector API server, passing shared desired-state storage, actual-state publication path, and connector health state.
10. Start handler processes for configured handlers.
11. Start session reaper background task.
12. Start node timeout detector background task.
13. Enter main recv loop.

---

## 16  Shutdown sequence

> **Requirement:** GW-1400.

1. Stop accepting new frames.
2. Wait for in-flight sessions to complete (with timeout).
3. Terminate handler processes (graceful shutdown request, then forced kill after timeout).
4. Flush any pending storage writes.
5. Close transport.
6. Exit.

### 16.1  Shutdown timeout (GW-1400)

A 5-second shutdown deadline is enforced by a watchdog thread. The
implementation lives in `bin/gateway.rs` and works as follows:

1. The main thread executes the graceful shutdown sequence (steps 1–6
   above) inside `run_gateway`. This includes stopping new frames,
   waiting (with its own internal timeouts) for in-flight sessions to
   complete, terminating handler processes, flushing storage, closing
   transports, and returning.
2. After `run_gateway` returns (i.e., after "gateway stopped" is logged),
   the caller (`main` for console mode, `service_entry` for Windows
   service mode) starts a **force-exit watchdog** on a separate OS thread
   using `std::thread::spawn`.
3. The watchdog thread sleeps for 5 seconds. If the process has not
   exited normally by then — for example because a `Drop` impl is stuck
   (such as a serial port `Drop` blocked on pending I/O) or the tokio
   runtime teardown hangs — the watchdog logs a `WARN`-level message and
   then calls `std::process::exit(0)` to force-terminate the process.
4. If runtime teardown completes before the 5-second deadline, the
   process exits via the normal return path and the watchdog thread
   simply terminates when the process exits.

This ensures the process never hangs after logging "gateway stopped",
regardless of serial port state (Issue #551).

---

## 17  BLE pairing protocol handler

> **Requirements:** GW-1200–GW-1222a.

The gateway implements the pairing protocol logic defined in [ble-pairing-protocol.md](ble-pairing-protocol.md). The physical BLE layer (GATT service, advertising, ATT MTU negotiation, indication fragmentation) is hosted by the USB modem (GW-1204, GW-1205); the gateway processes pairing messages relayed over the modem serial protocol: inbound messages arrive as `BLE_RECV` and responses are sent as `BLE_INDICATE` (see §4.2 `UsbEspNowTransport`). `PEER_REQUEST` / `PEER_ACK` frames travel over ESP-NOW, not BLE, and follow the standard transport path.

### 17.1  Gateway identity — RETIRED

> **RETIRED (issue #495).** The gateway no longer generates or persists a `gateway_id` (GW-1201, GW-1203 — both RETIRED). Gateway authority derives solely from possession of the node PSK database and phone PSK store. Failover requires replicating these databases (see GW-1000 `ExportState` / `ImportState`). No asymmetric keys are needed.

### 17.2  BLE message relay

The modem hosts the Gateway Pairing Service GATT service (UUID `0000FE60-…`) and controls BLE advertising. The gateway controls advertising lifetime by sending `BLE_ENABLE` / `BLE_DISABLE` to the modem when the registration window opens or closes (GW-1208). When a phone writes to the Gateway Command characteristic, the modem forwards the raw bytes to the gateway as a `BLE_RECV` serial message. The gateway processes the command and sends any response back via `BLE_INDICATE`; the modem handles fragmentation to fit within the negotiated ATT MTU (GW-1205). Numeric Comparison passkeys are relayed from the modem via `BLE_PAIRING_CONFIRM`. Modem button events are relayed as `EVENT_BUTTON` and interpreted only by the gateway; the modem remains a dumb button classifier and framebuffer sink.

### 17.3  `REQUEST_GW_INFO` handling — RETIRED

> **RETIRED (issue #495).** `REQUEST_GW_INFO` (BLE command `0x01`) and `GW_INFO_RESPONSE` are eliminated along with GW-1206. The simplified pairing pipeline uses `REGISTER_PHONE` / `PHONE_REGISTERED` only. No challenge–response or gateway identity exchange is needed — BLE LESC Numeric Comparison provides mutual authentication.

### 17.4  Registration window and `REGISTER_PHONE`

The gateway maintains a single BLE pairing session controller with:

- `window_open: bool`
- `deadline: Option<Instant>`
- `origin: Option<PairingOrigin>` where `PairingOrigin = Admin | Button`
- `successful_registration: bool`

The registration window opens either when the admin API calls `OpenBlePairing` or when the modem relays `EVENT_BUTTON(BUTTON_LONG)` while no BLE pairing session is active. In both cases the gateway sends `BLE_ENABLE` to the modem and records the session origin. A `BUTTON_LONG` received while a session is already active is ignored. A `BUTTON_SHORT` closes the session only when `origin == Button`; `BUTTON_SHORT` does not close an admin-initiated session. When no BLE pairing session is active, `BUTTON_SHORT` is routed instead to the display-page navigation policy described below. Auto-close uses the same timeout machinery for both origins (default 120 s). `REGISTER_PHONE` commands received while the window is closed are rejected with `ERROR(0x02)` (GW-1207).

When the window is open and a `REGISTER_PHONE` command arrives (BLE command `0x02`), the gateway: receives a phone-generated 256-bit PSK from the phone; derives `phone_key_hint = u16::from_be_bytes(SHA-256(psk)[30..32])`; stores the PSK with its label, issuance timestamp, and active status; and responds with a plaintext `PHONE_REGISTERED` indication containing `status`, `rf_channel`, and `phone_key_hint` via `BLE_INDICATE` (GW-1209). No additional encryption of the BLE response is needed — the BLE LESC link provides confidentiality. Operators can revoke phone PSKs through the admin API; revoked PSKs are excluded from AES-256-GCM decryption (GW-1210).

### 17.4a  Button-initiated display lifecycle

Gateway-owned display policy lives above the modem transport. During a button-initiated BLE pairing session, the gateway renders pairing status into a 128×64 framebuffer and sends it using the same reliable display-transfer protocol as the modem-ready banner (§4.2, GW-1101a). The display state machine is:

1. **Window opened by `BUTTON_LONG`** → display `Pairing`.
2. **`BLE_CONNECTED`** → display `Phone connected`.
3. **`BLE_PAIRING_CONFIRM(passkey)`** → display `Pin` plus the actual passkey.
4. **`PHONE_REGISTERED` / successful `REGISTER_PHONE`** → display `Provisioned`.
5. **Normal post-success close** → display `Done`.
6. **`BUTTON_SHORT` cancellation** → display `Cancelled`.
7. **Timeout close** → display `Timed out`.

The passkey screen has priority over the generic connected state until Numeric Comparison is resolved. `Done`, `Cancelled`, and `Timed out` are terminal status screens: the gateway keeps each one visible for 2 seconds, then restores the normal Sonde Gateway version banner if no newer pairing session has claimed the display. Display updates are driven by state transitions; repeated identical events do not need to re-send the same framebuffer.

### 17.4b  Short-press status-page navigation

Outside an active BLE pairing session, `EVENT_BUTTON(BUTTON_SHORT)` advances the modem display through a gateway-owned sequence of status pages. The gateway chooses the sequence contents and renders each page into the same 128×64 framebuffer format used for the default banner and pairing screens.

The default page sequence remains `[Channel, Nodes]`. The `Channel` page uses the existing centered two-line renderer. The `Nodes` page uses a node-status renderer that shows the operational node details most useful on the display: node ID, assigned/current program hashes, battery, last seen, and schedule, with nodes ordered by `node_id`, `key_hint` intentionally omitted, and `No nodes registered.` shown when the registry is empty. `last seen` is converted to the host's local timezone and formatted with locale-style date/time output rather than fixed UTC text. On the display, each field is formatted as a left-aligned property line followed by a left-aligned `- value` line so property names and values remain distinguishable on the small screen.

The gateway tracks the status-page cycle as a cursor over the configured page sequence (`StatusPageCycle { next_page_index }` in `bin/gateway.rs`) together with a monotonically increasing `display_generation` used to invalidate older timeout tasks. When the active page is `Nodes`, the gateway also tracks node-page-local scroll state: the current vertical window offset and whether an autonomous 50 ms scroll ticker is active for the current display generation.

Each `BUTTON_SHORT` while no pairing session is active:

1. Selects the page indicated by the current cycle cursor and sends the corresponding framebuffer via the reliable display-transfer path.
2. Advances `next_page_index` so the next short press shows the following configured status page, wrapping at the end of the sequence.
3. Increments `display_generation` and spawns a 60 s timeout task associated with that generation.

If the selected page is `Nodes`, the gateway first renders the complete page into an off-screen 1-bit framebuffer sized to the rendered text height. For oversized content, the gateway prepends one full display height of blank rows before the first rendered text row, so the initial transfer uses a blank 128×64 window and the text enters from the bottom edge as scrolling begins. If the rendered height exceeds 64 pixels, the gateway starts a 50 ms ticker that advances the visible window downward by 3 pixels per update, producing upward text motion on the OLED. The ticker reuses the same reliable display-transfer path as other display updates. It continues advancing past the last fully populated 128×64 window so the bottom of the rendered content scrolls completely off the top of the display before the next tick restarts from offset 0 at the start of the blank lead-in region. If the operator leaves the `Nodes` page and later returns, the page restarts from offset 0 rather than resuming from the previous offset. If the rendered height is 64 pixels or less, the gateway sends a single static page and does not start the ticker.

If another `BUTTON_SHORT` arrives before the timeout fires, the gateway advances again, increments `display_generation`, and leaves the older timeout task stale. Any existing `Nodes` page scroll ticker observes the generation change or an explicit stop request and exits without sending further updates. When a timeout task fires, it first stops any active `Nodes` page scroll task, then restores the default `Sonde Gateway v<semver>` banner only if its captured generation still matches the current `display_generation` and no BLE pairing session is active; otherwise it does nothing because a newer display update or pairing session has already claimed the screen. Autonomous `Nodes` page scroll updates do not increment `display_generation` and therefore do not extend the 60 s idle timeout.

### 17.4c  Admin-triggered transient display override

The admin API exposes a gateway-owned transient display override for operator
prompts such as headless device-login codes. This remains an operator/admin
workflow; the control-plane connector model in §13A does not introduce a second
display-control path. The shared helper accepts 1 to 4 text lines and rejects
the request with `FAILED_PRECONDITION` if a BLE pairing session currently owns
the display, so pairing passkeys and terminal status screens remain visible.

On success, the gateway renders the supplied lines with the same centered
128×64 text renderer used for the startup banner and button-pairing status
screens, then sends the resulting framebuffer through the existing reliable
display-transfer path. The RPC returns after that initial display update
completes; the caller does not remain connected for the 60-second dwell time.

The transient-display path reuses the same display-ownership machinery as
button-driven status pages: it cancels any active `Nodes` scroll task, resets
the status-page cycle to the normal starting position, increments
`display_generation`, and spawns a 60-second restore task tied to the captured
generation. When that task fires, it restores the default
`Sonde Gateway v<semver>` banner only if its generation is still current and no
BLE pairing session owns the display. A newer transient-display request, a
button-driven status-page update, or a pairing display transition invalidates
the older generation, causing the stale restore task to exit without
overwriting the newer screen.

### 17.5  `PEER_REQUEST` processing

`PEER_REQUEST` frames (msg_type `0x05`) arrive over ESP-NOW through the standard transport path, not BLE. Processing follows a multi-stage verification pipeline; any failure at any stage results in silent discard with no `PEER_ACK` sent (GW-1220). The gateway does not apply sequence-number anti-replay checks to `PEER_REQUEST` / `PEER_ACK` frames — these use random nonces (GW-1221).

**Pipeline:**

1. **Outer frame decryption** — The `key_hint` identifies a phone PSK. The gateway looks up all non-revoked phone PSK candidates matching the `key_hint` and tries AES-256-GCM-Open with each (GCM nonce = `SHA-256(phone_psk)[0..3] ‖ msg_type ‖ frame_nonce`, AAD = 11-byte header). No match → discard (GW-1211).
2. **Inner payload decryption** — The `encrypted_payload` field from the outer CBOR is decrypted with AES-256-GCM using the same `phone_psk` (AAD = `"sonde-pairing-v2"`). GCM tag failure → discard (GW-1212).
3. **Timestamp validation** — The `PairingRequest` timestamp must be within ± 86 400 s of current time. Out of range → discard (GW-1215).
4. **Node ID duplicate handling** — If the `node_id` is already registered **and** the `node_psk` matches the existing record, the gateway skips registration but still proceeds to PEER_ACK generation (GW-1218 AC4). If the `node_id` is registered with a **different** PSK, the frame is silently discarded (potential replay or conflict).
5. **Key-hint consistency** — The gateway computes `expected_node_key_hint = u16::from_be_bytes(SHA-256(node_psk)[30..32])` and verifies it matches the CBOR `node_key_hint`. The frame header `key_hint` identifies the *phone* PSK (used for the outer AES-GCM layer) and is expected to differ from `node_key_hint`. Mismatch between the CBOR `node_key_hint` and the derived value → discard (GW-1217).
6. **Node registration** — The node is registered with `node_id`, `node_key_hint`, `node_psk`, `rf_channel`, `sensors`, and `registered_by` = phone_id (GW-1218). The node registry (§7) stores the new record through the storage trait.

### 17.6  `PEER_ACK` generation

After successful registration **or** duplicate detection with matching PSK, the gateway builds a `PEER_ACK` CBOR message `{1: 0}` (status = success), encrypts the frame with AES-256-GCM using `node_psk` (GCM nonce = `SHA-256(node_psk)[0..3] ‖ msg_type ‖ frame_nonce`, AAD = 11-byte header), and echoes the `nonce` from the `PEER_REQUEST` header (GW-1219).

### 17.7  Admin and button sessions

The admin API exposes `OpenBlePairing` (server-streaming RPC) to open an admin-initiated registration window and enable BLE advertising, `CloseBlePairing` to close the active pairing session, `ConfirmBlePairing` for Numeric Comparison confirmation during admin-initiated pairing, `ListPhones` to enumerate registered phones, and `RevokePhone` to revoke a phone PSK (GW-1222).

When Numeric Comparison is requested during an admin-initiated session, the gateway broadcasts the passkey to admin-stream subscribers and waits up to 30 seconds for `ConfirmBlePairing`. When Numeric Comparison is requested during a button-initiated session, the gateway skips the admin confirmation wait, updates the display with the passkey screen, and immediately sends `BLE_PAIRING_CONFIRM_REPLY(accept=true)` (GW-1222a). The existing admin-stream events (`PhoneConnected`, `PhoneDisconnected`, `PasskeyRequest`, `PhoneRegistered`) remain available for both origins; button-initiated mode adds automatic confirmation and display transitions, not a different pairing protocol.

---

## 18  Installer and service management

This section covers platform packaging (MSI and `.deb`), system PATH registration, COM port auto-detection, service registration, and the `install` / `uninstall` CLI subcommands as a fallback.

### 18.1  WiX MSI ΓÇö PATH registration and service setup (GW-1500, GW-1501)

The MSI installer handles two tasks: PATH registration and Windows service setup.

**PATH registration:**

The existing `sonde.wxs` contains a WiX `Environment` element that appends the `bin` directory to the system `PATH`:

```xml
<Environment Id="PATH"
             Name="PATH"
             Value="[BinDir]"
             Separator=";"
             Action="set"
             Part="last"
             System="yes" />
```

Because `Part="last"` appends rather than replaces, existing PATH entries are preserved.

**Service registration with COM port auto-detect:**

The MSI includes a custom dialog page ("Modem Configuration") that collects the COM port:

1. A WiX custom action (C# or Rust DLL) runs during dialog initialization:
   - Enumerates USB devices via `SetupDiGetClassDevs` / `SetupDiEnumDeviceInfo`.
   - Filters for VID `303A`, PID `1001` (ESP32-S3 TinyUSB CDC ACM).
   - If found, reads the `PortName` registry value under the device's `Device Parameters` key.
   - Pre-populates the `MODEM_PORT` MSI property with the detected COM port.
2. The dialog displays a text field bound to `MODEM_PORT` (editable, in case the operator wants a different port).
3. On install complete, a deferred custom action creates `%ProgramData%\sonde\` and sets ACLs.
4. A WiX `ServiceInstall` element registers the service:
   ```xml
   <ServiceInstall Id="SondeGatewayService"
                   Name="sonde-gateway"
                   DisplayName="Sonde Gateway"
                   Start="auto"
                   Type="ownProcess"
                   ErrorControl="normal"
                   Arguments="--service --port [MODEM_PORT] --db [CommonAppDataFolder]sonde\gateway.db --master-key-file [CommonAppDataFolder]sonde\master-key.hex" />
   <ServiceControl Id="SondeGatewayControl"
                   Name="sonde-gateway"
                   Start="install"
                   Stop="both"
                   Remove="uninstall" />
   ```
5. On uninstall, `ServiceControl Remove="uninstall"` stops and removes the service. Data files in `%ProgramData%\sonde\` are preserved (not included in `RemoveFile` elements).
6. On upgrade, the service is stopped before file replacement and restarted after.

### 18.2  `sonde-gateway install` subcommand — CLI fallback (GW-1501)

The gateway binary exposes an `install` subcommand as a fallback for headless, scripted, or non-Windows deployments. The implementation is platform-specific:

**Windows (SCM):**

```
sonde-gateway install --port COM5 [--db C:\ProgramData\sonde\gateway.db] \
    [--master-key-file C:\ProgramData\sonde\master-key.hex] [--channel 1]
```

1. Validate that the process is running as Administrator (check via `OpenProcessToken` + `TokenElevation`). Exit with error code 1 and a clear message if not elevated.
2. Validate that `--port` is provided. Exit with error code 1 if omitted.
3. Build the `ImagePath` string: `"<exe_path>" --service --port <PORT> --db <DB> --master-key-file <KEY> [--channel <CH>]`. The `--service` flag is the normal gateway entry point used when launched by SCM.
4. Call `OpenSCManagerW` with `SC_MANAGER_CREATE_SERVICE` access.
5. Call `CreateServiceW` with:
   - `lpServiceName` = `"sonde-gateway"`
   - `lpDisplayName` = `"Sonde Gateway"`
   - `dwStartType` = `SERVICE_AUTO_START`
   - `lpBinaryPathName` = the `ImagePath` from step 3
   - `lpServiceStartName` = `NULL` (LocalSystem)
6. If the service already exists (`ERROR_SERVICE_EXISTS`), call `ChangeServiceConfigW` to update the `ImagePath` (idempotent update).
7. Set the service description via `ChangeServiceConfig2W` with `SERVICE_CONFIG_DESCRIPTION`.
8. Print success message and exit with code 0.

**Linux (systemd):**

```
sudo sonde-gateway install --port /dev/ttyACM0 [--db /var/lib/sonde/gateway.db] \
    [--key-provider file] [--channel 1]
```

1. Validate that the effective UID is 0 (root). Exit with error code 1 if not.
2. Validate that `--port` is provided.
3. Write (or update) all parameters in `/etc/sonde/environment`:
   ```
   SERIAL_PORT=/dev/ttyACM0
   DB_PATH=/var/lib/sonde/gateway.db
   KEY_PROVIDER=file
   # MASTER_KEY_FILE and CHANNEL are written only if provided
   ```
   The systemd unit reads all runtime parameters from this environment file via `EnvironmentFile=`; no parameters are hard-coded in the unit.
4. Verify that `/lib/systemd/system/sonde-gateway.service` exists (shipped by the `.deb` package or manually installed).
5. Run `systemctl daemon-reload`.
6. Run `systemctl enable sonde-gateway.service`.
7. Print success message. The operator must run `systemctl start sonde-gateway` separately (or reboot).

### 18.3  `sonde-gateway uninstall` subcommand (GW-1502)

The `uninstall` subcommand reverses the service registration performed by `install`. It does **not** delete the database, master key, or configuration files.

**Windows:**

1. Validate Administrator privileges.
2. Open the SCM and attempt to open the `sonde-gateway` service.
3. If the service does not exist, print an informational message and exit with code 0.
4. If the service is running, send `SERVICE_CONTROL_STOP` and wait up to 30 seconds for `SERVICE_STOPPED`.
5. Call `DeleteService`.
6. Print success message and exit with code 0.

**Linux:**

1. Validate root privileges.
2. Run `systemctl stop sonde-gateway.service` (ignore errors if not running).
3. Run `systemctl disable sonde-gateway.service` (ignore errors if not enabled).
4. Print success message. The environment file at `/etc/sonde/environment` and the systemd unit file are preserved (the `.deb` package owns the unit file; `dpkg -r` removes it).

### 18.4  Linux `.deb` package integration (GW-1503)

The `.deb` package (built by `installer/linux/build-deb.sh`) ships:

| Path | Contents |
|---|---|
| `/usr/local/bin/sonde-gateway` | Gateway binary |
| `/usr/local/bin/sonde-admin` | Admin CLI binary |
| `/lib/systemd/system/sonde-gateway.service` | systemd unit file |
| `/etc/sonde/environment` | Default environment (conffile; defaults `SERIAL_PORT=/dev/ttyUSB0`) |

**`postinst` script:**

1. Create `sonde` system group and user (if absent).
2. Add `sonde` to the `dialout` group for serial port access.
3. Create `/etc/sonde` (root:sonde, mode 750) and `/var/lib/sonde` (sonde:sonde, mode 750).
4. Run `systemctl daemon-reload && systemctl enable --now sonde-gateway.service` to enable and start the service.

**`prerm` script:**

1. Stop the service (`systemctl stop sonde-gateway.service`).
2. On `remove` or `purge` (but not `upgrade`), disable the service.

**systemd unit file** (`sonde-gateway.service`):

The unit runs as the `sonde` user with `SupplementaryGroups=dialout`, reads `SERIAL_PORT` from `EnvironmentFile=/etc/sonde/environment`, and includes `--master-key-file /var/lib/sonde/master-key.hex --generate-master-key` so the master key is auto-generated on first start (same pattern as the Windows MSI). Security hardening: `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `ReadWritePaths=/var/lib/sonde`, `ReadOnlyPaths=/etc/sonde`. See `installer/linux/sonde-gateway.service` for the full unit definition.

### 18.5  Configuration file locations (GW-1500, GW-1501, GW-1503)

| Platform | Configuration | Database | Master key |
|---|---|---|---|
| Windows | `%ProgramData%\sonde\` | `%ProgramData%\sonde\gateway.db` | `%ProgramData%\sonde\master-key.hex` |
| Linux (`.deb`) | `/etc/sonde/` | `/var/lib/sonde/gateway.db` | `/var/lib/sonde/master-key.hex` |
| Linux (manual) | Operator-chosen | Operator-chosen | Operator-chosen |

The MSI creates `%ProgramData%\sonde\` at install time (via the `ConfigGroup` component). The `.deb` `postinst` creates `/etc/sonde/` and `/var/lib/sonde/`. In both cases the directories are preserved on uninstall to avoid data loss.

---


## 19  Handler configuration management

> **Requirements:** GW-1401 (handler storage), GW-1402 (admin API), GW-1403 (CLI), GW-1404 (live reload), GW-1405 (bootstrap from file), GW-1406 (state export/import).

Handler routing is currently loaded from a YAML file (`--handler-config`). This section specifies database-backed handler configuration with admin API management, live reload, and state bundle integration.

### 19.1  Database schema

```sql
CREATE TABLE IF NOT EXISTS handlers (
    program_hash     TEXT PRIMARY KEY,   -- 64-char hex SHA-256 or "*" for catch-all
    command          TEXT NOT NULL,      -- executable path
    args             TEXT NOT NULL DEFAULT '[]',  -- JSON-encoded string array
    working_dir      TEXT,              -- optional working directory
    reply_timeout_ms INTEGER            -- optional per-handler timeout (NULL = gateway default)
);
```

`program_hash` is the primary key. The wildcard value `"*"` represents a catch-all handler (maps to `ProgramMatcher::Any`). All other values are 64-character hex strings representing SHA-256 hashes (maps to `ProgramMatcher::Hash`). Hex input is accepted case-insensitively and normalized to lowercase on storage (consistent with the existing YAML parser, which uses `from_str_radix(..., 16)`).

`args` is stored as a JSON array of strings (e.g., `["--verbose", "--port", "8080"]`). An empty array is stored as `"[]"`.

### 19.2  Handler record

```rust
pub struct HandlerRecord {
    pub program_hash: String,      // "*" or 64-char hex
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub reply_timeout_ms: Option<u64>,
}
```

Conversion to `HandlerConfig` (§9.1):
- `"*"` → `ProgramMatcher::Any`
- 64-char hex → `ProgramMatcher::Hash(decoded_bytes)`

### 19.3  Storage trait additions

```rust
#[async_trait]
pub trait Storage: Send + Sync {
    // ... existing methods ...

    // Handler configuration (GW-1401)
    async fn add_handler(&self, record: &HandlerRecord) -> Result<bool, StorageError>;
    async fn remove_handler(&self, program_hash: &str) -> Result<bool, StorageError>;
    async fn list_handlers(&self) -> Result<Vec<HandlerRecord>, StorageError>;
}
```

- `add_handler` — Inserts a handler record. Returns `Ok(true)` if the row was inserted, `Ok(false)` if a handler with the same `program_hash` already exists (consistent with the `insert_node_if_not_exists` pattern used elsewhere in the `Storage` trait).
- `remove_handler` — Deletes the handler with the given `program_hash`. Returns `true` if a row was deleted, `false` if no matching row existed.
- `list_handlers` — Returns all handler records ordered by `program_hash`.

The `SqliteStorage` implementation uses `INSERT OR IGNORE INTO handlers ...` for `add_handler` (checking `changes() > 0` to determine whether insertion occurred, consistent with `insert_node_if_not_exists`) and `DELETE FROM handlers WHERE program_hash = ?` for `remove_handler`.

### 19.4  Admin API additions

The gRPC service definition (§13.1) gains three new RPCs:

```protobuf
service GatewayAdmin {
    // ... existing RPCs ...

    // Handler management (GW-1402)
    rpc AddHandler(AddHandlerRequest) returns (Empty);
    rpc RemoveHandler(RemoveHandlerRequest) returns (Empty);
    rpc ListHandlers(Empty) returns (ListHandlersResponse);
}

message AddHandlerRequest {
    string program_hash = 1;          // "*" or 64-char hex
    string command = 2;
    repeated string args = 3;
    string working_dir = 4;           // empty string = not set
    optional uint64 reply_timeout_ms = 5;
}

message RemoveHandlerRequest {
    string program_hash = 1;
}

message HandlerInfo {
    string program_hash = 1;
    string command = 2;
    repeated string args = 3;
    string working_dir = 4;
    optional uint64 reply_timeout_ms = 5;
}

message ListHandlersResponse {
    repeated HandlerInfo handlers = 1;
}
```

The operations table (§13.2) gains:

| Operation | gRPC method | Description |
|---|---|---|
| Add handler | `AddHandler` | Registers a handler for a `program_hash`. Validates the hash format (64-char hex or `"*"`). Returns `ALREADY_EXISTS` if a handler with the same hash is already registered. Triggers live reload (GW-1404). |
| Remove handler | `RemoveHandler` | Removes the handler for the given `program_hash`. Returns `NOT_FOUND` if no match. Terminates the handler process if running. Triggers live reload (GW-1404). |
| List handlers | `ListHandlers` | Returns all configured handlers with their full configuration. |

The CLI tool (§13.3) gains:

```
sonde-admin handler add <program-hash> <command> [args...] [--working-dir <path>] [--reply-timeout <ms>]
sonde-admin handler remove <program-hash>
sonde-admin handler list
```

### 19.5  Handler live reload

The `AdminService` holds a reference to the `HandlerRouter` (wrapped in `Arc<tokio::sync::RwLock<HandlerRouter>>`). Since handler routing and reload occur inside async tasks, `tokio::sync::RwLock` is required to avoid blocking the Tokio runtime. When `AddHandler` or `RemoveHandler` succeeds at the storage layer, or when `ImportState` replaces handler records (GW-1404 AC5):

1. The `AdminService` calls `list_handlers()` on the storage trait to get the current handler set.
2. It converts each `HandlerRecord` to a `HandlerConfig` (§19.2).
3. It acquires a write lock on the `HandlerRouter` and calls `reload(new_configs)`.
4. `HandlerRouter::reload` diffs the old and new config sets:
   - **Added handlers** are inserted into the routing table (process spawned lazily on first message).
   - **Removed handlers** have their running process gracefully terminated and, if the process does not exit within 5 seconds, forcibly killed (e.g., `SIGTERM` then `SIGKILL` on POSIX, or `TerminateProcess` on Windows). The handler is then removed from the routing table.
   - **Unchanged handlers** (same `program_hash`, `command`, `args`, `working_dir`) retain their existing `HandlerProcess` instance (no restart).

This approach avoids disrupting in-flight requests to unaffected handlers.

> **Shared state note (D-485 extension):** The `HandlerRouter` reference shared between the admin API and the engine frame loop MUST be the same `Arc<tokio::sync::RwLock<HandlerRouter>>` instance. The frame loop acquires a read lock for routing; the admin API acquires a write lock only during reload.

### 19.6  Bootstrap from file

On startup, if `--handler-config <path>` is provided (GW-1405):

1. Parse the YAML file using the existing `load_handler_configs()` function (§9.1).
2. For each parsed `HandlerConfig`, expand it into one or more `HandlerRecord` values and call `storage.add_handler()` for each:
   - If the YAML `program_hash` field is a single matcher (e.g., `"*"` or one hex hash), emit exactly one `HandlerRecord` with that `program_hash`.
   - If the YAML `program_hash` field is a list of matchers, emit one `HandlerRecord` per matcher in the list. Each record uses the same `command`, `args`, and `working_dir` from the source `HandlerConfig`, differing only in `program_hash`.
3. If `add_handler` returns `Ok(false)` (duplicate), skip that record silently (database takes precedence).
4. If a YAML entry is invalid (e.g., malformed hex hash in `program_hash`), log a warning and continue processing the remaining entries.
5. After bootstrap, load all handlers from the database via `list_handlers()` and build the `HandlerRouter`.

This merge-on-startup strategy means the YAML file acts as a seed — it populates the database on first run but does not overwrite subsequent admin API changes.

### 19.7  State export/import

The state bundle (§13.2, `ExportState` / `ImportState`) is extended to include handler records (GW-1406).

**Export:** `export_state()` serializes handler records alongside nodes, programs, and phone PSKs. The existing state bundle already reserves root key `ROOT_KEY_HANDLERS = 6` for handlers and defines handler-level CBOR integer keys (see `state_bundle.rs`):

| Key | Name | Type | Description |
|-----|------|------|-------------|
| 1 | `HANDLER_KEY_MATCHERS` | array of text | `"*"` or hex hash strings |
| 2 | `HANDLER_KEY_COMMAND` | text | Executable path |
| 3 | `HANDLER_KEY_ARGS` | array of text | Command arguments (omitted if empty) |
| 4 | `HANDLER_KEY_REPLY_TIMEOUT_MS` | integer | Reply timeout in ms (omitted if unset) |

The existing format encodes `HandlerConfig` objects (with multiple matchers per entry). The new database-backed `HandlerRecord` model uses one record per `program_hash`, so the export layer converts `HandlerRecord` values back to the existing CBOR format: each record becomes a handler entry with a single-element `matchers` array. A new optional key is added for `working_dir`:

| Key | Name | Type | Description |
|-----|------|------|-------------|
| 5 | `HANDLER_KEY_WORKING_DIR` | text | Working directory (omitted if unset) |

On import, the existing `handler_config_from_cbor` decoder is extended to read key 5 (`working_dir`). Multi-matcher entries in older bundles are expanded to one `HandlerRecord` per matcher. This preserves full backward compatibility with bundles exported before `working_dir` support.

**Import:** `import_state()` restores handler records atomically within the same transaction that replaces nodes and programs. If the incoming bundle contains a handlers array, all existing handlers are deleted and replaced. If the handlers key is absent (bundle from older gateway version), existing handlers are preserved (no-op for backwards compatibility).

After import, the `AdminService` triggers a handler live reload (§19.5) so the `HandlerRouter` reflects the imported configuration.

---

## 20  App bundle deployment orchestration

This section defines how `sonde-admin` orchestrates bundle deployment by
composing existing gRPC operations.  The gateway itself is unaware of bundles —
all orchestration is client-side in the admin CLI.

### 20.1  Dependency

The `sonde-admin` binary depends on the `sonde-bundle` crate for manifest
parsing and validation.  It does NOT depend on `sonde-bundle`'s archive
creation functionality — only the parsing and validation modules.

### 20.2  Deploy command (GW-1600)

`sonde-admin deploy <bundle-path>` executes the following steps:

1. **Extract and validate** — call `sonde_bundle::archive::extract_bundle()`
   to extract the `.sondeapp` to a temporary directory, then call
   `sonde_bundle::validate::validate_manifest()` on the returned manifest
   and extracted directory.  Abort if `!result.is_valid()`.  This performs
   a single extraction pass; `validate_bundle()` is not used here because
   it would extract a second time internally.
2. **Deploy handler files** — if the bundle contains handler files:
   a. Determine the permanent handler directory:
      `<gateway-data-dir>/handlers/<app-name>-<version>/`.
      `sonde-admin` SHOULD compute a stable content hash for the extracted
      `handler/` directory (e.g., over relative paths and file bytes) and
      persist this hash in gateway- or admin-managed metadata alongside the
      app version.
   b. If the permanent handler directory already exists and the previously
      recorded content hash for this app/version matches the newly computed
      hash, `sonde-admin` MUST treat the handler deployment as idempotent:
      it SHOULD NOT delete or recopy files, and it SHOULD log
      "skipped (already deployed)" for the handler files step.
   c. If the directory does not exist, or the stored hash differs from the
      newly computed hash, create the permanent handler directory (creating
      parent directories as needed).  If the directory already exists from
      a previous, different deploy, remove its contents first
      (overwrite-on-conflict), then copy all files from the extracted
      `handler/` directory to the permanent handler directory.  Update the
      stored content hash to the newly computed value.
   d. Rewrite handler `working_dir` and file arguments to reference the
      permanent path.
3. **Ingest programs** — for each program in the manifest:
   a. Read the ELF binary from the extracted directory and compute its
      content hash using the same algorithm the gateway uses to key stored
      programs.
   b. Before sending the ELF, query the gateway's program state (e.g., via
      `ListPrograms`, `GetProgram`, or an equivalent lookup) to determine
      whether a program with this content hash already exists.  If it
      exists, record the mapping `program_name → program_hash`, log
      "skipped (already ingested)", and MUST NOT call `IngestProgram` for
      this program.
   c. If no existing program with the same hash is found, call the
      `IngestProgram` gRPC with the ELF and profile, and record the mapping:
      `program_name → program_hash` from the response.
   d. For gateways that implement `IngestProgram` as an upsert
      (`INSERT ... ON CONFLICT DO UPDATE`) and always return `Ok`,
      `sonde-admin` MUST still treat repeated ingest of identical ELF
      content as success.  If the gateway instead returns `ALREADY_EXISTS`
      for identical content, `sonde-admin` MAY rely on that signal rather
      than a prior lookup and SHOULD log "skipped (already ingested)" in
      that case.
4. **Configure handlers** — for each handler in the manifest:
   a. Resolve `handler.program` name to the program hash from step 3.
   b. Call the `AddHandler` gRPC with the resolved hash, command, args
      (rewritten to permanent paths in step 2d), working directory
      (permanent path), and reply timeout.
   c. If the gateway returns `ALREADY_EXISTS`, query the existing handler
      via `ListHandlers` and compare configuration.  If identical, log
      "skipped (already configured)".  If different, warn per §20.3 and
      continue.
5. **Assign programs to nodes** — for each node in the manifest:
   a. Resolve `node.program` name to the program hash from step 3.
   b. Call the `AssignProgram` gRPC with `node.name` and the resolved hash.
   c. If the node is already assigned the same hash (check via
      `GetNode` first), log "skipped (already assigned)" and continue.
6. **Clean up** — remove the temporary extraction directory (handler files
   have already been copied to the permanent location in step 2).
7. **Report** — print a summary table:
   ```
   Deploy complete: temperature-monitor v0.1.0
     Programs:  1 ingested, 0 skipped
     Handlers:  1 configured, 0 skipped
     Nodes:     2 assigned, 0 skipped
   ```

### 20.3  Idempotency (GW-1601)

Each step checks for existing state before acting:

- **Handler files:** `sonde-admin` computes a content hash over the
  extracted `handler/` directory and compares it to the hash stored from
  a previous deploy of the same app/version.  If the hashes match, the
  handler files step is skipped entirely (no file I/O) and logs
  "skipped (already deployed)".  If the hashes differ, the permanent
  handler directory is replaced with the new contents.
- **IngestProgram:** `sonde-admin` computes the ELF content hash locally
  and checks the gateway's stored programs (via `ListPrograms` or
  `GetProgram`) before calling `IngestProgram`.  If a program with
  the same hash already exists, the call is skipped and
  "skipped (already ingested)" is logged.  The current gateway
  implementation performs an upsert (`INSERT ... ON CONFLICT DO UPDATE`)
  and always returns `Ok` — `sonde-admin` SHOULD still pre-check to
  avoid redundant I/O and to emit the correct "skipped" report.
- **AddHandler:** The gateway returns `ALREADY_EXISTS` if a handler for
  the same program hash is already configured.  The deploy command then
  queries existing handler configuration via `ListHandlers` and compares
  fields.  If identical, skip and log "skipped (already configured)".
  If the existing handler has DIFFERENT configuration (different
  command/args), the deploy command warns the user and does NOT overwrite
  (preserving the user's manual changes).
- **AssignProgram:** The deploy command calls `GetNode` to check the node's
  current `assigned_program_hash`.  If it matches, skip and log
  "skipped (already assigned)".  If the node does not exist (not
  registered), warn and continue with the next node.

### 20.4  Undeploy command (GW-1602)

`sonde-admin undeploy <bundle-path> [--remove-programs] [--force]` executes:

1. **Parse manifest** — extract and parse the manifest (validation not
   strictly required, but schema version is checked).
2. **Compute program hashes** — for each program in the manifest:
   a. Read the ELF binary.
   b. Run the same ELF → CBOR → SHA-256 pipeline that `IngestProgram`
      uses to compute the content hash, without storing.
   c. Record the mapping: `program_name → program_hash`.
3. **Remove handlers** — for each handler in the manifest:
   a. If `handler.program` is `"*"` (catch-all), pass `"*"` directly to
      `RemoveHandler` without hash resolution.
   b. Otherwise, resolve the program name to hash.
   c. Call `RemoveHandler` with the hash (or `"*"`).
   d. If the handler does not exist, log "skipped (not found)".
4. **Remove handler files** — if the bundle defined handlers, remove the
   permanent handler directory at
   `<gateway-data-dir>/handlers/<app-name>-<version>/` if it exists.
5. **Warn about node assignments** — for each node in the manifest:
   a. Call `GetNode` to check current assignment.
   b. If the node is assigned to a bundle program, warn:
      "Node `<name>` is still assigned to program `<hash>`. Use
      `sonde-admin program assign` to reassign."
6. **Remove programs** (if `--remove-programs`):
   a. For each program, call `ListNodes` on the gateway to get ALL
      registered nodes, filter to those whose `assigned_program_hash`
      matches the program hash.
   b. If any nodes are assigned and `--force` is not set, skip with warning.
   c. If `--force`, call `AssignProgram` with empty hash for each assigned
      node first to unassign, then `RemoveProgram`.
   d. If no nodes are assigned, call `RemoveProgram`.
7. **Report** — print a summary.

### 20.5  Validate command (GW-1603)

`sonde-admin validate <bundle-path>` delegates to
`sonde_bundle::archive::validate_bundle()` and prints results.  This command does NOT
contact the gateway — it is fully offline.

### 20.6  Dry-run mode (GW-1604)

`sonde-admin deploy --dry-run <bundle-path>` runs the deploy algorithm but
replaces all **mutating** gRPC calls with no-ops.  It still contacts the gateway
for **read-only** state (via `GetNode`, `ListPrograms`, `ListHandlers`) to
determine which steps would be skipped vs. executed, and prints the plan:

```
Dry-run: temperature-monitor v0.1.0
  Would ingest:  bpf/temp_reader.elf (resident)
  Would add handler: python3 handler/ingest.py → temp-reader
  Would assign: greenhouse-1 → temp-reader
  Would assign: greenhouse-2 → temp-reader
```

### 20.7  Program hash computation

To support undeploy and dry-run (which need program hashes without ingesting),
the admin CLI must be able to compute the program hash locally.  This requires
the same ELF → CBOR → SHA-256 pipeline used by the gateway:

1. Parse ELF with `prevail-rust` to extract bytecode + maps + initial data.
2. Encode as `ProgramImage` (CBOR, deterministic encoding).
3. SHA-256 the CBOR bytes.

The `sonde-protocol` crate already provides `ProgramImage::encode_deterministic()`.
The ELF parsing is in `sonde-gateway`.  For the admin CLI, there are two options:

**Option A (recommended):** Factor the ELF → `ProgramImage` conversion out of
`sonde-gateway` into a shared library (e.g., a new module in `sonde-protocol`
or a thin `sonde-program` crate).

**Option B:** The admin CLI depends on `sonde-gateway` as a library for this
function only.

For V1, **Option B** is acceptable — the admin CLI already depends on
`sonde-gateway`'s proto definitions.  Option A can be a follow-up refactor.

### 20.8  CLI subcommand structure

The existing `Commands` enum in `sonde-admin` is extended with:

```rust
#[derive(Subcommand)]
enum Commands {
    // ... existing commands ...

    /// Deploy a Sonde App Bundle
    Deploy {
        /// Path to .sondeapp bundle
        bundle: PathBuf,
        /// Show what would be done without doing it
        #[arg(long)]
        dry_run: bool,
    },

    /// Undeploy a previously deployed bundle
    Undeploy {
        /// Path to .sondeapp bundle
        bundle: PathBuf,
        /// Remove programs from the library
        #[arg(long)]
        remove_programs: bool,
        /// Force removal even if programs are assigned
        #[arg(long)]
        force: bool,
    },

    /// Validate a Sonde App Bundle (offline)
    Validate {
        /// Path to .sondeapp bundle
        bundle: PathBuf,
    },
}
```

---

## 21  Pairing-time diagnostic handler

> **Requirements:** GW-1700 (DIAG_REQUEST reception), GW-1701 (DIAG_REQUEST session bypass), GW-1702 (RSSI measurement), GW-1703 (signal quality assessment), GW-1704 (DIAG_REPLY construction), GW-1705 (configurable RSSI thresholds), GW-1706 (diagnostic logging).

### 21.1  Overview

The gateway handles `DIAG_REQUEST` frames (`msg_type` 0x06) from pre-provisioning nodes acting as radio relays for the pairing tool. This provides the installer with RF link quality feedback before committing to node placement.

### 21.2  Frame reception and authentication

`DIAG_REQUEST` frames are processed in the main frame dispatch loop alongside `WAKE`, `APP_DATA`, and `PEER_REQUEST`. Authentication uses the same phone PSK lookup path as `PEER_REQUEST`:

1. Extract `key_hint` from the frame header.
2. Look up non-revoked phone PSK candidates matching `key_hint` (reuses `PhonePskStore::lookup_by_hint()`).
3. Attempt AES-256-GCM-Open with each candidate.
4. On successful decryption: decode the CBOR payload and extract `diagnostic_type`.
5. On decryption failure: silently discard (consistent with protocol error handling).

**Session bypass (GW-1701):** Unlike `WAKE` or post-WAKE messages, `DIAG_REQUEST` does not create or require an active session. The frame is processed statelessly — the gateway does not track the sender MAC or maintain any diagnostic session state.

### 21.3  RSSI measurement pipeline

The RSSI for the diagnostic reply comes from the modem's `RECV_FRAME` message. The existing `UsbModemTransport` already parses RSSI from each received frame (see §4.2). The diagnostic handler captures the RSSI that was associated with the `RECV_FRAME` carrying the `DIAG_REQUEST`.

Implementation approach:
- The `UsbModemTransport::recv_with_rssi()` method returns `(Vec<u8>, PeerAddress, i8)` — the raw frame, sender address, and RSSI. The base `Transport::recv()` trait method returns `(Vec<u8>, PeerAddress)` without RSSI; `Gateway::process_frame()` delegates to `process_frame_with_rssi(raw, peer, None)` for transports without RSSI support.
- The diagnostic handler reads `rssi` from the `process_frame_with_rssi` parameter. If `None` (e.g., loopback transport), uses a sentinel value of `0` dBm and logs a warning **(GW-1702)**.

### 21.4  Signal quality assessment

The gateway evaluates the RSSI against two configurable thresholds **(GW-1703, GW-1705)**:

```rust
pub struct RssiThresholds {
    pub good_threshold: i8,  // default: -60
    pub bad_threshold: i8,   // default: -75
}

impl RssiThresholds {
    pub fn assess(&self, rssi_dbm: i8) -> u8 {
        if rssi_dbm >= self.good_threshold {
            SIGNAL_QUALITY_GOOD      // 0
        } else if rssi_dbm >= self.bad_threshold {
            SIGNAL_QUALITY_MARGINAL  // 1
        } else {
            SIGNAL_QUALITY_BAD       // 2
        }
    }
}
```

Thresholds are configured at gateway startup via the CLI flags `--rssi-good-threshold` and `--rssi-bad-threshold`. The gateway validates `good_threshold > bad_threshold` and logs an error if violated.

### 21.5  DIAG_REPLY construction

The gateway constructs and transmits a `DIAG_REPLY` frame **(GW-1704)**:

1. Build CBOR payload: `{ 1: diagnostic_type, 2: rssi_dbm, 3: signal_quality }` using deterministic encoding (RFC 8949 §4.2).
2. Frame header:
   - `key_hint` = phone's `key_hint` (same as the request).
   - `msg_type` = `0x85`.
   - `nonce` = echo the request nonce (binds reply to request).
3. Encrypt with the same `phone_psk` that decrypted the request. AAD = 11-byte header.
4. Transmit via modem `SEND_FRAME`, addressed to the sender MAC from the `RECV_FRAME` metadata.

### 21.6  Logging

All diagnostic events are logged at `INFO` level **(GW-1706)**:
- `DIAG_REQUEST` received: sender MAC, phone key_hint, diagnostic_type.
- `DIAG_REPLY` sent: RSSI value, signal quality assessment, target MAC.
- Decryption failures are logged at `DEBUG` level (consistent with GW-1302).
- PSK values are never logged (consistent with GW-1307).

---

## 22  Container image

> **Requirements:** GW-1800 (multi-arch image), GW-1801 (tagging), GW-1802 (runtime configuration), GW-1803 (optional secret-service), GW-1804 (bundled modem flashing assets).

### 22.1  Overview

The gateway is distributed as a multi-architecture Docker container image alongside the traditional bare-metal binaries and `.deb` packages. The image targets Alpine Linux (musl libc) for minimal size and contains `sonde-gateway`, `sonde-admin`, `sonde-sht40-handler`, `sonde-tmp102-handler`, `espflash`, and two bundled modem merged flash images (default and verbose).

### 22.2  Build strategy

Each architecture (`linux/amd64`, `linux/arm64`) is built natively on a per-arch GitHub Actions runner — no QEMU cross-compilation. The per-arch images are combined into a single multi-arch manifest using `docker buildx imagetools create`.

**Multi-stage Dockerfile** (`.github/docker/Dockerfile.gateway`):

1. **Builder stage** (`rust:alpine`): installs `musl-dev`, `protobuf`, and the additional native dependencies required to build `espflash`; installs the pinned `espflash` CLI; builds all four Sonde binaries; and uses `--no-default-features` for `sonde-gateway` to exclude the `keyring` feature and its `secret-service`/`zbus` dependency tree.
2. **Runtime stage** (`alpine:3.21`): copies only the compiled runtime binaries, the `espflash` executable from the builder stage, and the two merged modem flash images (`modem-firmware` and `modem-firmware-verbose`) supplied in the Docker build context by the same workflow run. It then creates a non-root `sonde` user and declares `VOLUME /var/lib/sonde`.

### 22.3  Bundled flashing assets (GW-1804)

The runtime image exposes the bundled modem flashing assets at fixed paths:

| Asset | Path |
|-------|------|
| `espflash` | `/usr/local/bin/espflash` |
| Default modem image | `/usr/local/share/sonde/firmware/modem/default/flash_image.bin` |
| Verbose modem image | `/usr/local/share/sonde/firmware/modem/verbose/flash_image.bin` |

The container continues to start with `sonde-gateway` as its default entrypoint. Operators who need to reflash a modem run the container with an entrypoint override (for example `--entrypoint espflash` or `--entrypoint sh`) and invoke `espflash write-bin -p PORT 0x0 <image-path>` manually. The gateway process does not invoke `espflash` on startup or during routine operation.

To keep "latest" unambiguous, the bundled modem images are defined as the modem artifacts produced from the same git revision and workflow run as the container image build. The container workflow therefore consumes the `modem-firmware` and `modem-firmware-verbose` artifacts from the same CI execution rather than downloading an arbitrary previously published release.

### 22.4  Feature flag: `keyring` (GW-1803)

The `secret-service` dependency (D-Bus keyring via `zbus`) is gated behind a `keyring` cargo feature, enabled by default. Container builds pass `--no-default-features` to exclude it, since containers use `--key-provider file` or `--key-provider env` instead. All `#[cfg(target_os = "linux")]` gates on secret-service code are extended to `#[cfg(all(target_os = "linux", feature = "keyring"))]`.

### 22.5  Tagging strategy (GW-1801)

| Trigger | Tags |
|---------|------|
| Release tag (`v*`) | `latest`, semver (e.g., `0.5.0`), `sha-<short>` |
| Nightly / schedule / dispatch | `nightly`, `nightly-YYYYMMDD`, `sha-<short>` |

Public tags are created only after both architectures pass smoke tests.

### 22.6  Runtime configuration (GW-1802)

| Property | Value |
|----------|-------|
| `ENTRYPOINT` | `sonde-gateway` |
| `CMD` | `--db /var/lib/sonde/sonde.db --port /dev/ttyACM0 --key-provider env` |
| `VOLUME` | `/var/lib/sonde` |
| `USER` | `sonde` (non-root) |

Serial device access requires the operator to pass `--device=/dev/ttyACM0` and `--group-add <host-dialout-gid>` at `docker run` time. The container defaults assume `/dev/ttyACM0`; operators using a different modem path must override `--port`. The bundled modem images remain readable by the non-root `sonde` user so operators can invoke manual flashing commands without switching users inside the container.

### 22.7  CI integration

The `gateway-container.yml` workflow is called by `nightly-release.yml` as a parallel job. The nightly release job waits for the container build to complete before publishing the GitHub release. Release-tag container builds are therefore driven indirectly via `nightly-release.yml`, while `gateway-container.yml` itself is also available via `workflow_dispatch`.

To satisfy GW-1804's provenance rule, every execution path that publishes or smoke-tests the gateway container must make the modem artifacts available in the same workflow run before the image-build step. In the nightly/release path, those artifacts come from the sibling modem-firmware job in the caller workflow. In the standalone `workflow_dispatch` path, the workflow must first run the modem build (or an equivalent reusable workflow) so the image still consumes same-run `modem-firmware` and `modem-firmware-verbose` artifacts rather than downloading files from a previous run.
