<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# End-to-End Validation Specification

> **Document status:** Draft
> **Scope:** System-level integration tests that exercise the full sonde stack.
> **Audience:** Implementers (human or LLM agent) building the E2E test harness.
> **Related:** [gateway-validation.md](gateway-validation.md), [node-validation.md](node-validation.md), [protocol.md](protocol.md), [gateway-design.md](gateway-design.md)

---

## 1  Purpose

The per-crate validation tests (gateway-validation.md, node-validation.md) exercise each component through mock interfaces. This document specifies **end-to-end tests** that wire the real gateway engine, modem bridge, and node wake-cycle engine together and verify correct behavior across component boundaries.

These tests validate that:

- The protocol wire format is compatible between gateway and node.
- The chunked transfer protocol completes successfully across the full stack.
- Authentication (AEAD, nonce, sequence numbers) works end-to-end.
- Admin commands (schedule, reboot, ephemeral) flow from the engine to the node.
- Application data is routed from a node through the gateway to a handler and back.

---

## 2  Architecture

All core components (gateway engine, modem bridge, node mock) run **in a single process** within one tokio runtime. No serial ports, PTYs, or network sockets are required. The one exception is APP_DATA handler tests (T-E2E-030/031), which spawn a small stub executable via `HandlerRouter` to exercise the real handler stdio path. This stub is built as a `[[bin]]` target in the E2E crate and is self-contained. All tests are deterministic and portable (Linux, macOS, Windows CI).

```
┌──────────────┐        duplex()        ┌──────────────────┐
│              │◄──────────────────────►│                  │
│   Gateway    │     serial framing     │   Modem Bridge   │
│   Engine     │    (modem protocol)    │   (sonde-modem)  │
│              │                        │                  │
└──────┬───────┘                        └────────┬─────────┘
       │                                         │
       │  Storage: SqliteStorage(:memory:)       │  Radio: ChannelRadio
       │  Admin: direct fn calls                 │  (mpsc-backed)
       │  Handler: in-process mock               │
       │                                         ▼
       │                                ┌──────────────────┐
       │                                │                  │
       │                                │   Node Mock      │
       │                                │   (sonde-node)   │
       │                                │                  │
       │                                └──────────────────┘
       │                                  run_wake_cycle()
       │                                  MockHal, MockRng,
       │                                  MockClock, etc.
```

### 2.1  Gateway side

- **Engine:** Real `Gateway` from `sonde_gateway::engine`. Tests that don't need APP_DATA handling use `Gateway::new_with_pending()`. Tests that exercise APP_DATA routing (T-E2E-030, T-E2E-031) use `Gateway::new_with_handler()` with a `HandlerRouter` configured to spawn a small stub executable (see below).
- **Storage:** `SqliteStorage::in_memory()` for test isolation (no files).
- **Transport:** `UsbEspNowTransport::new(duplex_client, channel)` — the gateway's modem adapter connected to the in-memory duplex stream.
- **Admin:** Direct function calls on `Gateway` and `Storage` (no gRPC in E2E tests). Admin operations are exercised by calling storage/engine methods directly, avoiding the need for network sockets.
- **Handler:** For APP_DATA tests (T-E2E-030/031) only: `HandlerRouter` spawns a stub executable built as a `[[bin]]` target in the E2E crate. The stub uses the gateway's handler framing protocol: 4-byte big-endian length prefix followed by CBOR payload (matching `sonde_gateway::handler::write_message`/`read_message`). It reads DATA messages from stdin, writes DATA_REPLY to stdout. Protocol-only tests do not use a handler.

### 2.2  Modem bridge

- **Bridge:** Real `Bridge` from `sonde_modem::bridge`, connecting a `PipeSerial` adapter to a `ChannelRadio`.
- **Serial adapter:** `PipeSerial` — a test-only `SerialPort` trait implementation backed by `std::sync::mpsc` channels (or a ring buffer). Must implement all three `SerialPort` methods: `read(&mut self, buf: &mut [u8]) → (usize, bool)`, `write(&mut self, data: &[u8]) → bool`, and `is_connected(&self) → bool`. The `is_connected` method should return `true` (always connected in tests); the `reconnected` flag from `read` should return `true` once at startup to trigger `MODEM_READY`. One side feeds the gateway's `UsbEspNowTransport` (via `tokio::io::duplex`), the other side feeds the bridge. Since `Bridge` uses the sync `SerialPort` trait while `UsbEspNowTransport` uses `AsyncRead + AsyncWrite`, an adapter bridges the two worlds:
  - The `tokio::io::duplex` server half is driven by a background tokio task that reads bytes and pushes them into a ring buffer; the `PipeSerial::read()` drains from that buffer.
  - `PipeSerial::write()` pushes bytes into another ring buffer that the tokio task reads and writes to the duplex stream.
- **Radio:** `ChannelRadio` — routes ESP-NOW frames to/from node mocks via `std::sync::mpsc` channels.
- **Lifecycle:** A dedicated thread (not tokio task) runs `bridge.poll()` in a loop, since `Bridge::poll()` is synchronous.

### 2.3  Node mock

- **Engine:** Real `run_wake_cycle()` from `sonde_node::wake_cycle`.
- **Transport:** `ChannelTransport` — a test-only `Transport` implementation backed by the same `mpsc` channels as the `ChannelRadio`, simulating ESP-NOW send/recv.
- **Platform mocks:** The E2E crate provides its own mock implementations of the node platform traits (matching the signatures in `sonde_node::traits`). These are simple re-implementations since the `#[cfg(test)]` mocks in `sonde-node` are not exported:
  - `MockHal` — returns configurable I2C/SPI/GPIO/ADC data.
  - `MockStorage` (PlatformStorage) — in-memory key/program/schedule storage.
  - `MockBpfInterpreter` — records load/execute calls.
  - `MockRng`, `MockClock`, `MockBattery` — deterministic values.

### 2.4  Channel radio and channel transport

These are the glue components that simulate ESP-NOW radio:

```rust
/// Simulates ESP-NOW broadcast between a modem and one or more nodes.
///
/// Uses `std::sync::mpsc` (not tokio) because `Radio::drain_one` takes
/// `&self` and `Radio::send` takes `&mut self` — both synchronous.
/// The receiver is wrapped in `Mutex` to satisfy `drain_one(&self)`.
///
/// `drain_one()` returns `Option<RecvFrame>` which includes `rssi: i8`.
/// The ChannelRadio uses a fixed RSSI value (e.g., -40) for all
/// simulated frames since RSSI is not relevant to protocol correctness.
///
/// Note: `std::sync::mpsc` is multi-producer / single-consumer.
/// Each test uses a single node. Multi-node tests would need a
/// per-node sender map or a broadcast channel; this is left as a
/// future extension.
struct ChannelRadio {
    /// Frames sent by the gateway (via modem bridge) arrive at the node.
    to_node: std::sync::mpsc::Sender<(Vec<u8>, [u8; 6])>,
    /// Frames sent by the node arrive here for the gateway.
    from_node: std::sync::Mutex<std::sync::mpsc::Receiver<(Vec<u8>, [u8; 6])>>,
}

/// Node-side transport backed by the same channels.
///
/// Uses `std::sync::mpsc` with `recv_timeout()` to implement the
/// synchronous `sonde_node::traits::Transport::recv(timeout_ms)` contract.
struct ChannelTransport {
    /// Frames from the gateway (via ChannelRadio).
    rx: std::sync::mpsc::Receiver<(Vec<u8>, [u8; 6])>,
    /// Frames to the gateway (via ChannelRadio).
    tx: std::sync::mpsc::Sender<(Vec<u8>, [u8; 6])>,
    node_mac: [u8; 6],
}
```

The `ChannelRadio` implements `sonde_modem::bridge::Radio` (all required methods):
- `send(&mut self, peer_mac, data)` → pushes to `to_node` sender.
- `drain_one(&self)` → locks `from_node` mutex, pops one pending frame as `Option<RecvFrame>`.
- `set_channel(&mut self, ch) → Result<(), &'static str>` → stores channel, returns `Ok(())`.
- `channel(&self) → u8` → returns stored channel.
- `scan_channels(&mut self) → Vec<(u8, u8, i8)>` → returns empty vec (no APs in simulation).
- `mac_address(&self) → [u8; 6]` → returns a fixed test MAC (e.g., `[0x00; 6]`).
- `reset_state(&mut self)` → no-op.

The `ChannelTransport` implements `sonde_node::traits::Transport` (synchronous):
- `send(&mut self, frame)` → pushes to `tx` sender.
- `recv(&mut self, timeout_ms)` → calls `rx.recv_timeout(Duration::from_millis(timeout_ms as u64))`, returns `Ok(Some(data))` or `Ok(None)` on timeout.

---

## 3  Test harness setup

Each test follows this sequence:

1. **Create channels:** `std::sync::mpsc` pairs for radio simulation (gateway↔nodes).
2. **Create duplex:** `tokio::io::duplex(4096)` for the serial link between gateway and bridge.
3. **Create PipeSerial adapter:** Bridges the sync `SerialPort` trait (used by `Bridge`) to the async duplex stream (used by `UsbEspNowTransport`). A background tokio task shuttles bytes between the duplex server half and the adapter's internal ring buffers.
4. **Start modem bridge:** Spawn a **std::thread** running a bridge poll loop. Construct `let mut bridge = Bridge::new(pipe_serial, channel_radio, ModemCounters::new())` (note: `ModemCounters::new()` already returns `Arc`). The loop checks an `AtomicBool` stop flag each iteration and sleeps unconditionally for 1ms after each `poll()` call (since `Bridge::poll()` returns `()` and does not report whether work was done). The thread is joined at test teardown.
5. **Start gateway transport:** `UsbEspNowTransport::new(duplex_client, channel)` — this runs the startup handshake (RESET → MODEM_READY → SET_CHANNEL → SET_CHANNEL_ACK) against the bridge.
6. **Create gateway engine:** `Gateway::new_with_pending(storage, pending_commands, session_manager)` for protocol tests, or `Gateway::new_with_handler(storage, session_timeout, handler_router)` for APP_DATA tests (T-E2E-030/031). Commands are queued via `Gateway::queue_command()` when using the handler variant.
7. **Register test nodes:** Insert `NodeRecord` into storage with known PSKs.
8. **Run test scenario:** Drive node wake cycles (via `spawn_blocking` since `run_wake_cycle` is sync) and assert on gateway behavior.
9. **Teardown:** Set the stop flag, join the bridge thread, drop the transport.

### 3.1  Test node helper

```rust
struct E2eNode {
    node_id: String,
    mac: [u8; 6],
    key_hint: u16,
    psk: [u8; 32],
    transport: ChannelTransport,
    storage: MockStorage,
    // ... other mocks
}

impl E2eNode {
    /// Run one wake cycle and return the outcome.
    fn wake(&mut self) -> WakeCycleOutcome { ... }
}
```

---

## 4  Test cases

### 4.1  Protocol compatibility

#### T-E2E-001  NOP wake cycle (no pending command, no program)

**Validates:** Protocol wire format compatibility between gateway and node.

**Preconditions:**
1. Node registered in gateway storage with known PSK.
2. No program assigned to the node.
3. No pending commands.

**Procedure:**
1. Node runs `run_wake_cycle()`.
2. Node sends WAKE with `program_hash = []`, `battery_mv = 3300`.
3. Gateway receives WAKE, creates session, responds with COMMAND(NOP).
4. Node receives COMMAND, verifies AEAD authentication, proceeds to BPF execution (no program → skip).
5. Node sleeps.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Gateway's `NodeRecord.last_battery_mv` updated to 3300.
- Gateway's `NodeRecord.firmware_abi_version` updated.
- Gateway's `NodeRecord.last_seen` updated.

---

#### T-E2E-002  AEAD authentication round-trip

**Validates:** AES-256-GCM authentication works across gateway (software crypto) and node (injected crypto).

**Preconditions:**
1. Node registered with PSK `[0xAA; 32]`.

**Procedure:**
1. Node sends WAKE authenticated with PSK via AES-256-GCM.
2. Gateway decrypts and verifies using its `RustCryptoAead`.
3. Gateway responds with COMMAND encrypted with the same PSK.
4. Node decrypts using its `TestAead`.

**Assertions:**
- Wake cycle completes successfully (no `AuthFailure`).
- Both sides use the same PSK and produce compatible AEAD ciphertexts.

---

#### T-E2E-002b  Consecutive wake cycles

**Validates:** GW-0600, ND-0300, ND-0301, ND-0304.

**Preconditions:**
1. Node registered with PSK `[0x55; 32]`.

**Procedure:**
1. Run a full wake cycle on a `NodeProxy` — verify it completes with `Sleep { seconds: 60 }`.
2. Run a second wake cycle on the **same** `NodeProxy` (same storage, same RNG instance — internal state advances).
3. Collect the WAKE nonces from both cycles.

**Assertions:**
- Both cycles complete with `Sleep { seconds: 60 }` and receive gateway responses.
- WAKE nonces differ between the two cycles (RNG state advances).
- Gateway's `NodeRecord.last_seen` is updated after both cycles.

---

#### T-E2E-002c  AEAD wake cycle with BPF APP_DATA

**Validates:** GW-0600, ND-0300, ND-0602.

**Preconditions:**
1. Node registered with matching PSK.
2. BPF program assigned that calls `send()` (helper 8).

**Procedure:**
1. Node completes AEAD wake cycle (WAKE → COMMAND → program download).
2. BPF program executes and calls `send()` with a 2-byte blob.
3. Node sends an AEAD-authenticated APP_DATA frame.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Exactly one `MSG_APP_DATA` frame was sent by the node.

---

#### T-E2E-003  Wrong PSK rejected silently

**Validates:** GW-0601 (wrong key → silent discard), ND-0301.

**Preconditions:**
1. Node registered with PSK `[0xAA; 32]`.
2. Node configured with PSK `[0xBB; 32]` (mismatch).

**Procedure:**
1. Node sends WAKE authenticated with wrong PSK.
2. Gateway receives frame, AEAD decryption fails.
3. Gateway does not respond.
4. Node sends WAKE 4 times total (1 initial + 3 retries), then sleeps.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }` (retries exhausted).
- Gateway sent zero response frames.

---

#### T-E2E-004  Tampered AEAD frame (silent discard)

**Validates:** GW-0600 (GCM tag verification), ND-0300.

**Preconditions:**
1. Node registered with matching PSK.
2. Tamper mode enabled on transport (bit-flip in ciphertext region).

**Procedure:**
1. Node sends WAKE with valid PSK.
2. Transport flips a bit in the ciphertext before forwarding.
3. Gateway receives frame, GCM authentication fails.
4. Gateway silently discards the frame — no response.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }` (retries exhausted).
- Gateway sent zero response frames.
- Gateway did not update `last_seen` or battery telemetry for the node.

---

### 4.2  Program distribution

#### T-E2E-010  Full program update cycle

**Validates:** GW-0201, GW-0300, GW-0302, ND-0500, ND-0501, ND-0506.

**Preconditions:**
1. Node registered with no current program.
2. Program ingested into gateway library and assigned to node.

**Procedure:**
1. Node sends WAKE with `program_hash = []`.
2. Gateway detects hash mismatch → responds with COMMAND(UPDATE_PROGRAM).
3. Node sends GET_CHUNK for each chunk index.
4. Gateway responds with CHUNK data for each.
5. Node reassembles image, verifies SHA-256 hash.
6. Node sends PROGRAM_ACK.
7. Gateway updates `NodeRecord.current_program_hash`.
8. Node executes program (MockBpfInterpreter records load+execute).

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Node's `MockBpfInterpreter.loaded == true`.
- Node's `MockBpfInterpreter.executed == true`.
- Gateway's `NodeRecord.current_program_hash` matches assigned hash.
- Correct number of GET_CHUNK / CHUNK exchanges occurred.

---

#### T-E2E-011  Program already current → NOP

**Validates:** GW-0200 (hash matches → NOP).

**Preconditions:**
1. Node has the assigned program installed (hashes match).

**Procedure:**
1. Node sends WAKE with correct `program_hash`.
2. Gateway detects hash match → responds with COMMAND(NOP).

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- No GET_CHUNK messages exchanged.

---

### 4.3  Command dispatch

#### T-E2E-020  UPDATE_SCHEDULE via admin

**Validates:** GW-0203, GW-0803.

**Preconditions:**
1. Node registered with `schedule_interval_s = 60`.

**Procedure:**
1. Admin queues `UpdateSchedule { interval_s: 120 }` for the node.
2. Node sends WAKE.
3. Gateway responds with COMMAND(UPDATE_SCHEDULE, interval_s=120).
4. Node stores new interval.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 120 }`.
- Node's `MockStorage.schedule_interval` == 120.
- Pending command consumed (queue empty after wake).

---

#### T-E2E-021  REBOOT via admin

**Validates:** GW-0204, GW-0803.

**Preconditions:**
1. Admin queues `Reboot` for the node.

**Procedure:**
1. Node sends WAKE.
2. Gateway responds with COMMAND(REBOOT).

**Assertions:**
- `run_wake_cycle()` returns `Reboot`.

---

#### T-E2E-022  RUN_EPHEMERAL via admin

**Validates:** GW-0202, GW-0803.

**Preconditions:**
1. Ephemeral program ingested into gateway library.
2. Admin queues `RunEphemeral { program_hash }` for the node.

**Procedure:**
1. Node sends WAKE.
2. Gateway responds with COMMAND(RUN_EPHEMERAL).
3. Node downloads program via chunked transfer.
4. Node executes ephemeral program (not persisted to flash).

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Node's `MockBpfInterpreter.executed == true`.
- Node's `MockStorage.programs` unchanged (ephemeral not stored).

---

### 4.4  Application data

#### T-E2E-030  APP_DATA round-trip with handler

**Validates:** GW-0500, GW-0501, ND-0602.

**Preconditions:**
1. Node registered with a program that calls `send_recv()`.
2. Gateway configured with an in-process handler that echoes data.

**Procedure:**
1. Node completes WAKE/COMMAND exchange.
2. Node sends APP_DATA with blob `[0xAA, 0xBB]`.
3. Gateway routes to handler.
4. Handler replies with `[0xCC, 0xDD]`.
5. Gateway sends APP_DATA_REPLY to node.

**Assertions:**
- Node receives reply blob `[0xCC, 0xDD]`.
- Handler received correct `node_id` and `program_hash` in DATA message.

---

#### T-E2E-031  APP_DATA fire-and-forget (send helper)

**Validates:** ND-0602 (send, no reply expected).

**Preconditions:**
1. Node's BPF program calls `send()` (fire-and-forget).

**Procedure:**
1. Node completes WAKE/COMMAND exchange.
2. Node sends APP_DATA with blob `[0x01, 0x02]`.
3. Gateway routes to handler.
4. Handler processes but returns empty reply.
5. Gateway does NOT send APP_DATA_REPLY.

**Assertions:**
- Handler received the data.
- No APP_DATA_REPLY frame sent to node.

---

#### T-E2E-032  APP_DATA AEAD end-to-end

**Validates:** GW-0500, GW-0600, ND-0300, ND-0602.

**Preconditions:**
1. Node registered with PSK.
2. BPF program calls `send([0xDE, 0xAD])`.
3. Gateway configured with AEAD wake cycle.
4. Handler configured for the program hash.

**Procedure:**
1. Node executes AEAD wake cycle: WAKE (AEAD) → COMMAND/NOP (AEAD).
2. BPF program executes and calls `send()`.
3. BPF helper produces an AEAD-authenticated APP_DATA frame.
4. Gateway receives the frame, decrypts with AES-256-GCM, validates sequence.
5. Gateway routes decrypted blob to handler via stdin.
6. Handler receives DATA message, processes it.

**Assertions:**
- Handler receives DATA message with blob `[0xDE, 0xAD]`.
- The APP_DATA frame on the wire uses AEAD format (11B header + ciphertext + 16B tag).
- The node/gateway exchange completes without the APP_DATA frame being silently discarded (e.g., the blob is delivered to the handler and processing continues normally).

---

#### T-E2E-033  Live reload — handler add end-to-end

**Validates:** GW-1404, GW-1407.

**Preconditions:**
1. Node registered with a PSK and a known `program_hash`.
2. Gateway started with no handlers configured.

**Procedure:**
1. Node completes AEAD WAKE/COMMAND exchange.
2. Node sends APP_DATA with blob `[0x01, 0x02]`.
3. Assert: no APP_DATA_REPLY is sent (no handler matched).
4. Call `AddHandler` via admin API with the node's `program_hash` and a test echo handler.
5. Node completes another AEAD WAKE/COMMAND exchange.
6. Node sends APP_DATA with blob `[0x03, 0x04]`.

**Assertions:**
- APP_DATA_REPLY is received after step 6 (handler was live-added and routed correctly).
- No gateway restart occurred between steps 3 and 6.

---

#### T-E2E-034  Live reload — handler remove end-to-end

**Validates:** GW-1404, GW-1407.

**Preconditions:**
1. Node registered with a PSK.
2. Gateway started with a catch-all handler (`program_hash` = `"*"`).

**Procedure:**
1. Node completes AEAD WAKE/COMMAND exchange.
2. Node sends APP_DATA with blob `[0xAA, 0xBB]`.
3. Assert: APP_DATA_REPLY is received (handler matched).
4. Call `RemoveHandler` via admin API with `program_hash` = `"*"`.
5. Node completes another AEAD WAKE/COMMAND exchange.
6. Node sends APP_DATA with blob `[0xCC, 0xDD]`.

**Assertions:**
- No APP_DATA_REPLY is sent after step 6 (handler was live-removed).
- The handler process from step 2 is no longer running.

---

### 4.5  Error handling

#### T-E2E-040  Unknown node silent discard

**Validates:** GW-1002, ND-0700.

**Preconditions:**
1. Node NOT registered in gateway storage.

**Procedure:**
1. Node sends WAKE.
2. Gateway cannot find a matching PSK.
3. Gateway does not respond.
4. Node retries and eventually sleeps.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Gateway sent zero response frames.

---

#### T-E2E-041  Sequence number enforcement

**Validates:** GW-0602, ND-0303.

**Preconditions:**
1. Node registered.
2. Program assigned (to trigger chunked transfer).

**Procedure:**
1. Node completes WAKE/COMMAND exchange.
2. Gateway assigns `starting_seq = S`.
3. Node sends GET_CHUNK with `nonce = S`.
4. Gateway responds (correct seq).
5. Node sends next GET_CHUNK with `nonce = S+1`.
6. Gateway responds (correct seq).

**Assertions:**
- All chunk responses echo the correct sequence numbers.
- Transfer completes successfully.

---

### 4.6  Modem bridge integration

#### T-E2E-050  Modem startup handshake

**Validates:** GW-1101 (RESET → MODEM_READY → SET_CHANNEL → ACK).

**Preconditions:**
1. Modem bridge running on duplex stream.

**Procedure:**
1. `UsbEspNowTransport::new(duplex_client, channel)` performs startup.
2. Bridge receives RESET, responds with MODEM_READY.
3. Bridge receives SET_CHANNEL, responds with SET_CHANNEL_ACK.

**Assertions:**
- Transport creation succeeds.
- Bridge is on the configured channel.
- Modem MAC address is reported correctly.

---

#### T-E2E-051  Frame round-trip through modem bridge

**Validates:** GW-1100, modem framing protocol.

**Preconditions:**
1. Gateway transport and modem bridge operational.

**Procedure:**
1. Node sends a frame via ChannelTransport (→ ChannelRadio → Bridge → serial → UsbEspNowTransport).
2. Gateway processes frame and sends response (→ UsbEspNowTransport → serial → Bridge → ChannelRadio → ChannelTransport).
3. Node receives response.

**Assertions:**
- Frame arrives at gateway intact (correct bytes).
- Response arrives at node intact.
- Modem framing (length prefix, type byte, body) correctly encoded/decoded at both ends.

---

#### T-E2E-052  Consecutive wake cycles through modem bridge

**Validates:** GW-1100, modem protocol, ND-0300, ND-0304.

**Preconditions:**
1. Gateway transport and modem bridge operational.
2. Node registered with PSK `[0x52; 32]`.

**Procedure:**
1. Run a full wake cycle on a `NodeProxy` through the modem bridge — verify it completes.
2. Run a second wake cycle on the **same** `NodeProxy` through the modem bridge.
3. Collect the WAKE nonces from both cycles.

**Assertions:**
- Both cycles complete with `Sleep { seconds: 60 }`.
- WAKE nonces are disjoint across cycles (no RNG collision).

---

#### T-E2E-053  Wrong PSK through modem bridge

**Validates:** GW-0601, GW-1002, ND-0301.

**Preconditions:**
1. Gateway transport and modem bridge operational.
2. Node registered with PSK `[0xAA; 32]`.
3. Node configured with PSK `[0xBB; 32]` (mismatch).

**Procedure:**
1. Node sends WAKE through the modem bridge with the wrong PSK.
2. Gateway silently discards the frame.
3. Node exhausts retries and sleeps.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Gateway's `NodeRecord.last_seen` is `None` (WAKE was never processed).

---

#### T-E2E-054  Program update through modem bridge

**Validates:** GW-0201, GW-0300, GW-1100, ND-0500, ND-0501.

**Preconditions:**
1. Gateway transport and modem bridge operational.
2. Node registered with no current program.
3. Program ingested and assigned to the node.

**Procedure:**
1. Node sends WAKE through the modem bridge with `program_hash = []`.
2. Gateway responds with COMMAND(UPDATE_PROGRAM) through the bridge.
3. Node sends GET_CHUNK for each chunk index through the bridge.
4. Gateway responds with CHUNK data through the bridge.
5. Node sends PROGRAM_ACK through the bridge.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Node sent at least one GET_CHUNK and exactly one PROGRAM_ACK through the bridge.
- Gateway's `NodeRecord.current_program_hash` matches the assigned hash.

---

### 4.7  BLE pairing / onboarding

#### T-E2E-060  Gateway Ed25519 identity persistence

**Validates:** GW-1200, GW-1201 — Ed25519 keypair and gateway_id generation and persistence.

**Preconditions:**
1. Fresh `SqliteStorage::in_memory(master_key)`.

**Procedure:**
1. Call `GatewayIdentity::generate()` and store via `Storage::store_gateway_identity`.
2. Reload identity via `Storage::load_gateway_identity`.

**Assertions:**
- Public key and gateway_id are identical after round-trip.

#### T-E2E-061  Phone registration (Phase 1)

**Validates:** GW-1206–GW-1210 — REGISTER_PHONE PSK exchange over BLE LESC, registration window enforcement.

**Preconditions:**
1. Gateway identity stored.
2. Registration window open.

**Procedure:**
1. Send REQUEST_GW_INFO envelope via `handle_ble_recv` with 32-byte challenge.
2. Parse GW_INFO_RESPONSE (verify Ed25519 public key, gateway_id, signature).
3. Send REGISTER_PHONE with phone-generated PSK + label over BLE LESC.
4. Parse PHONE_REGISTERED response (plaintext over BLE LESC — status, PSK, key_hint, channel).
5. Close registration window and attempt REGISTER_PHONE again.

**Assertions:**
- GW_INFO_RESPONSE contains valid 112-byte response.
- PHONE_REGISTERED decrypts to 36-byte inner (status + PSK + key_hint + channel).
- Phone PSK stored in gateway storage.
- Closed window returns ERROR envelope with status 0x02.

#### T-E2E-062  Node BLE provisioning (Phase 2)

**Validates:** ND-0905–ND-0908 — NODE_PROVISION handling, NVS state persistence.

**Preconditions:**
1. Phase 1 complete (phone PSK available).
2. Encrypted payload constructed from Phase 1 artifacts.

**Procedure:**
1. Build `encrypted_payload` (CBOR PairingRequest encrypted with AES-256-GCM using `phone_psk`).
2. Call `handle_node_provision` with key_hint, PSK, channel, and encrypted payload.

**Assertions:**
- `handle_node_provision` returns `NODE_ACK_SUCCESS`.
- Node storage contains: PSK, key_hint, channel, peer_payload.
- `reg_complete` is `false`.

#### T-E2E-063  PEER_REQUEST/PEER_ACK (Phase 3)

**Validates:** GW-1211–GW-1219, ND-0909–ND-0913 — Node relays encrypted payload to gateway, gateway decrypts and registers node, returns PEER_ACK with registration_proof.

**Preconditions:**
1. Gateway identity and phone PSK stored.
2. Node BLE-provisioned (PSK + peer_payload in storage, reg_complete = false).

**Procedure:**
1. Run wake cycle. Node detects `!reg_complete && has_peer_payload`.
2. Node builds PEER_REQUEST frame (msg_type 0x05) and sends via ESP-NOW.
3. Gateway decrypts encrypted_payload (AES-256-GCM with `phone_psk`), registers node.
4. Gateway returns PEER_ACK encrypted with `node_psk` via AES-256-GCM.
5. Node verifies PEER_ACK, sets `reg_complete`, proceeds to WAKE.

**Assertions:**
- PEER_REQUEST frame sent (msg_type 0x05).
- Node registered in gateway storage with correct key_hint.
- `reg_complete` is `true`.
- Wake cycle completes with `Sleep { seconds: 60 }`.

#### T-E2E-064  Complete onboarding → first WAKE

**Validates:** ND-0914 — Node transitions from bootstrap to steady-state; peer_payload erased after first WAKE.

**Preconditions:**
1. Same as T-E2E-063.

**Procedure:**
1. Run first wake cycle (PEER_REQUEST + WAKE).
2. Verify steady-state transition.
3. Run second wake cycle (pure WAKE, no PEER_REQUEST).

**Assertions:**
- After first cycle: `reg_complete` true, `peer_payload` erased (ND-0914).
- Second cycle succeeds without PEER_REQUEST.
- No msg_type 0x05 frames in second cycle.

#### T-E2E-065  Deferred erasure

**Validates:** ND-0913 (retain peer_payload on PEER_ACK), ND-0914 (erase after WAKE success).

**Preconditions:**
1. BLE-provisioned node with peer_payload present.

**Procedure:**
1. Verify peer_payload present before cycle.
2. Run wake cycle (PEER_REQUEST + WAKE).
3. Verify peer_payload absent after cycle.

**Assertions:**
- peer_payload present before cycle.
- `reg_complete` true and `peer_payload` None after WAKE success.

#### T-E2E-066  Self-healing

**Validates:** ND-0915 — WAKE failure after forged PEER_ACK reverts to PEER_REQUEST.

**Preconditions:**
1. BLE-provisioned node with `reg_complete = true` (forged) and peer_payload present.
2. Gateway does NOT have the node registered.

**Procedure:**
1. Run wake cycle: skip PEER_REQUEST (reg_complete=true) → WAKE fails (unknown node) → self-healing clears reg_complete.
2. Run second wake cycle: PEER_REQUEST → gateway registers → PEER_ACK → WAKE succeeds.

**Assertions:**
- After first cycle: `reg_complete` false, peer_payload retained.
- After second cycle: `reg_complete` true, peer_payload erased.

#### T-E2E-067  Agent revocation

**Validates:** GW-1213 — Revoked phone PSK causes PEER_REQUEST silent discard.

**Preconditions:**
1. Phone registered then revoked via `Storage::revoke_phone_psk`.
2. Encrypted payload built with the revoked phone's credentials.

**Procedure:**
1. Run wake cycle: PEER_REQUEST sent → gateway discards (revoked phone PSK, AEAD decryption fails) → PEER_ACK timeout.

**Assertions:**
- PEER_REQUEST frame sent.
- `reg_complete` remains false.
- Node NOT registered in gateway storage.

#### T-E2E-068  Factory reset and re-provisioning

**Validates:** ND-0917 — Factory reset clears all state; re-provisioning succeeds.

**Preconditions:**
1. Node previously registered (PEER_REQUEST + WAKE completed).

**Procedure:**
1. Factory reset via `run_pairing_mode` with `ResetRequest`.
2. Verify all state cleared.
3. Re-provision with new identity via `handle_node_provision`.
4. Run wake cycle (PEER_REQUEST + WAKE).

**Assertions:**
- After reset: key None, peer_payload None.
- Re-provisioned node completes PEER_REQUEST + WAKE successfully.
- New identity registered in gateway.

#### T-E2E-069  Multi-node concurrent

**Validates:** GW-1216 (node_id uniqueness), key isolation between nodes.

**Preconditions:**
1. Two nodes provisioned with distinct PSKs via the same phone.

**Procedure:**
1. Onboard node A (PEER_REQUEST + WAKE).
2. Onboard node B (PEER_REQUEST + WAKE).
3. Both run steady-state WAKE cycles.

**Assertions:**
- Both nodes registered in gateway storage.
- Both succeed in steady-state (no PEER_REQUEST in second cycle).

#### T-E2E-070  Full use case

**Validates:** All onboarding requirements + steady-state program execution.

**Preconditions:**
1. Fresh environment.

**Procedure:**
1. Generate gateway identity.
2. Phone registration (Phase 1).
3. Build encrypted payload and BLE-provision node (Phase 2).
4. PEER_REQUEST/PEER_ACK + first WAKE (Phase 3).
5. Deploy a BPF program that calls `send()` (helper 8).
6. Run wake cycle with real `SondeBpfInterpreter`.

**Assertions:**
- All onboarding steps succeed.
- BPF program executes and sends APP_DATA (msg_type 0x04).

---

### 4.8  BPF execution

#### T-E2E-080  E2E map access through full stack

**Validates:** bpf-environment.md §5, ND-0603.

**Preconditions:**
1. Node registered with a PSK.
2. A BPF program that calls `map_lookup_elem` and `map_update_elem` ingested and assigned.

**Procedure:**
1. Deploy the BPF program through the gateway→node chunked transfer.
2. Run a wake cycle with the real `SondeBpfInterpreter`.
3. Verify the program executes successfully and map side-effects are recorded.
4. Run a second wake cycle on the same node.
5. Verify map data persists across wake cycles.

**Assertions:**
- Both wake cycles complete with `Sleep`.
- Map writes from the first cycle are visible in the second cycle via `map_lookup_elem`.
- No interpreter errors or panics.

---

#### T-E2E-081  E2E ephemeral program restrictions

**Validates:** bpf-environment.md §2.2, ND-0603, ND-0604.

**Preconditions:**
1. Node registered with a PSK.
2. An ephemeral BPF program that attempts `map_update_elem` and `set_next_wake` ingested and deployed.

**Procedure:**
1. Deploy the ephemeral program via COMMAND(RUN_EPHEMERAL).
2. Run a wake cycle with the real `SondeBpfInterpreter`.

**Assertions:**
- `map_update_elem` is rejected (returns error code).
- `set_next_wake` is rejected (returns error code).
- The node does not crash and returns to sleep normally.

---

#### T-E2E-082  E2E chunked transfer corruption recovery

**Validates:** ND-0501, ND-0502.

**Preconditions:**
1. Node registered with no current program.
2. Program ingested and assigned to the node.

**Procedure:**
1. Node sends WAKE with `program_hash = []`.
2. Gateway responds with COMMAND(UPDATE_PROGRAM).
3. Node sends GET_CHUNK requests.
4. Inject a corrupted chunk (wrong data bytes) in one CHUNK response.
5. Node reassembles image and computes SHA-256 hash.

**Assertions:**
- Node detects hash mismatch and does not install the corrupted image.
- Node does not send PROGRAM_ACK.
- `run_wake_cycle()` returns `Sleep` (node recovers gracefully).

---

#### T-E2E-083  E2E instruction budget enforcement

**Validates:** bpf-environment.md §3.3, ND-0605.

**Preconditions:**
1. Node registered with a PSK.
2. A BPF program that exceeds the instruction budget (infinite loop or very long computation) ingested and assigned.

**Procedure:**
1. Deploy the program through the gateway→node chunked transfer.
2. Run a wake cycle with the real `SondeBpfInterpreter`.

**Assertions:**
- The interpreter terminates execution when the budget is exhausted.
- The node returns to sleep normally (`Sleep`).
- No crash, hang, or panic.

---

## 5  Test-to-requirement traceability

| Test ID | Requirements |
|---------|-------------|
| T-E2E-001 | GW-0100, GW-0102, GW-0103, ND-0200, ND-0201 |
| T-E2E-002 | GW-0600, ND-0300, ND-0301 |
| T-E2E-002b | GW-0600, ND-0300, ND-0301, ND-0304 |
| T-E2E-002c | GW-0600, ND-0300, ND-0602 |
| T-E2E-003 | GW-0601, GW-1002, ND-0301 |
| T-E2E-004 | GW-0600, ND-0300 |
| T-E2E-010 | GW-0201, GW-0300, GW-0302, ND-0500, ND-0501, ND-0506 |
| T-E2E-011 | GW-0200 |
| T-E2E-020 | GW-0203, GW-0803, ND-0202 |
| T-E2E-021 | GW-0204, GW-0803, ND-0202 |
| T-E2E-022 | GW-0202, ND-0503 |
| T-E2E-030 | GW-0500, GW-0501, ND-0602 |
| T-E2E-031 | GW-0500, ND-0602 |
| T-E2E-032 | GW-0500, GW-0600, ND-0300, ND-0602 |
| T-E2E-040 | GW-1002, ND-0700 |
| T-E2E-041 | GW-0602, ND-0303 |
| T-E2E-050 | GW-1100, GW-1101 |
| T-E2E-051 | GW-1100, modem protocol |
| T-E2E-052 | GW-1100, modem protocol, ND-0300, ND-0304 |
| T-E2E-053 | GW-0601, GW-1002, ND-0301 |
| T-E2E-054 | GW-0201, GW-0300, GW-1100, ND-0500, ND-0501 |
| T-E2E-060 | GW-1200, GW-1201 |
| T-E2E-061 | GW-1206, GW-1207, GW-1208, GW-1209, GW-1210 |
| T-E2E-062 | ND-0905, ND-0906, ND-0907, ND-0908 |
| T-E2E-063 | GW-1211, GW-1212, GW-1213, GW-1214, GW-1215, GW-1216, GW-1217, GW-1218, GW-1219, ND-0909, ND-0912, ND-0913 |
| T-E2E-064 | ND-0914 |
| T-E2E-065 | ND-0913, ND-0914 |
| T-E2E-066 | ND-0915 |
| T-E2E-067 | GW-1213 |
| T-E2E-068 | ND-0917 |
| T-E2E-069 | GW-1216 |
| T-E2E-070 | GW-1200, GW-1206, GW-1209, GW-1211, GW-1218, ND-0905, ND-0909, ND-0914, ND-0602 |
| T-E2E-080 | bpf-environment.md §5, ND-0603 |
| T-E2E-081 | bpf-environment.md §2.2, ND-0603, ND-0604 |
| T-E2E-082 | ND-0501, ND-0502 |
| T-E2E-083 | bpf-environment.md §3.3, ND-0605 |

---

## 6  Implementation notes

### 6.1  Crate structure

The E2E tests should live in a dedicated workspace crate:

```
crates/sonde-e2e/
├── Cargo.toml          # depends on sonde-gateway, sonde-node, sonde-modem, sonde-protocol, sonde-pair
└── tests/
    ├── harness.rs      # shared test setup (ChannelRadio, ChannelTransport, E2eNode, BLE pairing helpers, etc.)
    ├── e2e_tests.rs    # test cases T-E2E-060..070, T-E2E-081, T-E2E-083
    └── aead_e2e_tests.rs  # AEAD and handler routing tests: T-E2E-001..004, T-E2E-030..034
```

### 6.2  Async ↔ sync bridge

There are two async/sync boundaries:

1. **Bridge (sync) ↔ UsbEspNowTransport (async):** The bridge's `SerialPort` trait is synchronous, but `UsbEspNowTransport` expects `AsyncRead + AsyncWrite`. The `PipeSerial` adapter bridges this gap using internal ring buffers and a background tokio task that shuttles bytes between the duplex stream and the ring buffers.

2. **Node wake cycle (sync) ↔ test harness (async):** `run_wake_cycle()` is synchronous. The `ChannelTransport` uses `std::sync::mpsc::recv_timeout()` for blocking receive. Node wake cycles should run inside `tokio::task::spawn_blocking()` to avoid blocking the tokio runtime.

### 6.3  Timing

All timeouts should use short values (50-100ms) to keep tests fast. The `ChannelTransport` timeout for `recv()` should be configurable per test.

### 6.4  Test isolation

Each test creates its own:
- `SqliteStorage::in_memory()` (no shared state between tests).
- Fresh duplex pair and mpsc channels.
- Fresh modem bridge and gateway engine.

This allows tests to run in parallel with `cargo test`.
