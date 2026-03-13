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
- Authentication (HMAC, nonce, sequence numbers) works end-to-end.
- Admin commands (schedule, reboot, ephemeral) flow from the engine to the node.
- Application data is routed from a node through the gateway to a handler and back.

---

## 2  Architecture

All components run **in a single process** within one tokio runtime. No external processes, serial ports, PTYs, or network sockets are required. This makes the tests deterministic and portable (runs on Linux, macOS, and Windows CI).

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

- **Engine:** Real `Gateway::new_with_pending()` from `sonde-gateway::engine`.
- **Storage:** `SqliteStorage::in_memory()` for test isolation (no files).
- **Transport:** `UsbEspNowTransport::new(duplex_client, channel)` — the gateway's modem adapter connected to the in-memory duplex stream.
- **Admin:** Direct function calls on `Gateway` and `Storage` (no gRPC in E2E tests). Admin operations are exercised by calling storage/engine methods directly, avoiding the need for network sockets.
- **Handler:** In-process mock handler that reads DATA messages and writes DATA_REPLY (using the existing handler framing from `sonde-gateway::handler`).

### 2.2  Modem bridge

- **Bridge:** Real `Bridge` from `sonde-modem::bridge`, connecting a `PipeSerial` adapter to a `ChannelRadio`.
- **Serial adapter:** `PipeSerial` — a test-only `SerialPort` trait implementation backed by `std::sync::mpsc` channels (or a ring buffer). One side feeds the gateway's `UsbEspNowTransport` (via `tokio::io::duplex`), the other side feeds the bridge. Since `Bridge` uses the sync `SerialPort` trait (`read(&mut self, buf: &mut [u8]) → (usize, bool)`, `write(&mut self, data: &[u8]) → bool`) while `UsbEspNowTransport` uses `AsyncRead + AsyncWrite`, an adapter bridges the two worlds:
  - The `tokio::io::duplex` server half is driven by a background tokio task that reads bytes and pushes them into a ring buffer; the `PipeSerial::read()` drains from that buffer.
  - `PipeSerial::write()` pushes bytes into another ring buffer that the tokio task reads and writes to the duplex stream.
- **Radio:** `ChannelRadio` — routes ESP-NOW frames to/from node mocks via `std::sync::mpsc` channels.
- **Lifecycle:** A dedicated thread (not tokio task) runs `bridge.poll()` in a loop, since `Bridge::poll()` is synchronous.

### 2.3  Node mock

- **Engine:** Real `run_wake_cycle()` from `sonde-node::wake_cycle`.
- **Transport:** `ChannelTransport` — a test-only `Transport` implementation backed by the same `mpsc` channels as the `ChannelRadio`, simulating ESP-NOW send/recv.
- **Platform mocks:** The E2E crate provides its own mock implementations of the node platform traits (matching the signatures in `sonde-node::traits`). These are simple re-implementations since the `#[cfg(test)]` mocks in `sonde-node` are not exported:
  - `MockHal` — returns configurable I2C/SPI/GPIO/ADC data.
  - `MockStorage` (PlatformStorage) — in-memory key/program/schedule storage.
  - `MockBpfInterpreter` — records load/execute calls.
  - `MockRng`, `MockClock`, `MockBattery` — deterministic values.

### 2.4  Channel radio and channel transport

These are the glue components that simulate ESP-NOW radio:

```rust
/// Simulates ESP-NOW broadcast between a modem and one or more nodes.
///
/// Uses `std::sync::mpsc` (not tokio) because `Radio::drain_rx` takes
/// `&self` and `Radio::send` takes `&mut self` — both synchronous.
/// The receiver is wrapped in `Mutex` to satisfy `drain_rx(&self)`.
///
/// `drain_rx()` returns `Vec<RecvFrame>` which includes `rssi: i8`.
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
/// synchronous `sonde-node::traits::Transport::recv(timeout_ms)` contract.
struct ChannelTransport {
    /// Frames from the gateway (via ChannelRadio).
    rx: std::sync::mpsc::Receiver<(Vec<u8>, [u8; 6])>,
    /// Frames to the gateway (via ChannelRadio).
    tx: std::sync::mpsc::Sender<(Vec<u8>, [u8; 6])>,
    node_mac: [u8; 6],
}
```

The `ChannelRadio` implements `sonde-modem::bridge::Radio`:
- `send(&mut self, peer_mac, data)` → pushes to `to_nodes` sender.
- `drain_rx(&self)` → locks `from_nodes` mutex, drains all pending frames as `Vec<RecvFrame>`.

The `ChannelTransport` implements `sonde-node::traits::Transport` (synchronous):
- `send(&mut self, frame)` → pushes to `tx` sender.
- `recv(&mut self, timeout_ms)` → calls `rx.recv_timeout(Duration::from_millis(timeout_ms))`, returns `Ok(Some(data))` or `Ok(None)` on timeout.

---

## 3  Test harness setup

Each test follows this sequence:

1. **Create channels:** `std::sync::mpsc` pairs for radio simulation (gateway↔nodes).
2. **Create duplex:** `tokio::io::duplex(4096)` for the serial link between gateway and bridge.
3. **Create PipeSerial adapter:** Bridges the sync `SerialPort` trait (used by `Bridge`) to the async duplex stream (used by `UsbEspNowTransport`). A background tokio task shuttles bytes between the duplex server half and the adapter's internal ring buffers.
4. **Start modem bridge:** Spawn a **std::thread** running a bridge poll loop. Construct `let mut bridge = Bridge::new(pipe_serial, channel_radio, ModemCounters::new())` (note: `ModemCounters::new()` already returns `Arc`). The loop checks an `AtomicBool` stop flag each iteration and sleeps unconditionally for 1ms after each `poll()` call (since `Bridge::poll()` returns `()` and does not report whether work was done). The thread is joined at test teardown.
5. **Start gateway transport:** `UsbEspNowTransport::new(duplex_client, channel)` — this runs the startup handshake (RESET → MODEM_READY → SET_CHANNEL → SET_CHANNEL_ACK) against the bridge.
6. **Create gateway engine:** `Gateway::new_with_pending(storage, pending_commands, session_manager)`.
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
4. Node receives COMMAND, verifies HMAC, proceeds to BPF execution (no program → skip).
5. Node sleeps.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }`.
- Gateway's `NodeRecord.last_battery_mv` updated to 3300.
- Gateway's `NodeRecord.firmware_abi_version` updated.
- Gateway's `NodeRecord.last_seen` updated.

---

#### T-E2E-002  HMAC authentication round-trip

**Validates:** HMAC-SHA256 authentication works across gateway (software crypto) and node (injected crypto).

**Preconditions:**
1. Node registered with PSK `[0xAA; 32]`.

**Procedure:**
1. Node sends WAKE authenticated with PSK.
2. Gateway verifies HMAC using its `RustCryptoHmac`.
3. Gateway responds with COMMAND authenticated with the same PSK.
4. Node verifies HMAC using its `TestHmac`.

**Assertions:**
- Wake cycle completes successfully (no `AuthFailure`).
- Both sides use the same PSK and produce compatible HMAC tags.

---

#### T-E2E-003  Wrong PSK rejected silently

**Validates:** GW-0601 (wrong key → silent discard), ND-0301.

**Preconditions:**
1. Node registered with PSK `[0xAA; 32]`.
2. Node configured with PSK `[0xBB; 32]` (mismatch).

**Procedure:**
1. Node sends WAKE authenticated with wrong PSK.
2. Gateway receives frame, HMAC verification fails.
3. Gateway does not respond.
4. Node sends WAKE 4 times total (1 initial + 3 retries), then sleeps.

**Assertions:**
- `run_wake_cycle()` returns `Sleep { seconds: 60 }` (retries exhausted).
- Gateway sent zero response frames.

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

## 5  Test-to-requirement traceability

| Test ID | Requirements |
|---------|-------------|
| T-E2E-001 | GW-0100, GW-0102, GW-0103, ND-0200, ND-0201 |
| T-E2E-002 | GW-0600, ND-0300, ND-0301 |
| T-E2E-003 | GW-0601, GW-1002, ND-0301 |
| T-E2E-010 | GW-0201, GW-0300, GW-0302, ND-0500, ND-0501, ND-0506 |
| T-E2E-011 | GW-0200 |
| T-E2E-020 | GW-0203, GW-0803, ND-0202 |
| T-E2E-021 | GW-0204, GW-0803, ND-0202 |
| T-E2E-022 | GW-0202, ND-0503 |
| T-E2E-030 | GW-0500, GW-0501, ND-0602 |
| T-E2E-031 | GW-0500, ND-0602 |
| T-E2E-040 | GW-1002, ND-0700 |
| T-E2E-041 | GW-0602, ND-0303 |
| T-E2E-050 | GW-1100, GW-1101 |
| T-E2E-051 | GW-1100, modem protocol |

---

## 6  Implementation notes

### 6.1  Crate structure

The E2E tests should live in a dedicated workspace crate:

```
crates/sonde-e2e/
├── Cargo.toml          # depends on sonde-gateway, sonde-node, sonde-modem, sonde-protocol
└── tests/
    ├── harness.rs      # shared test setup (ChannelRadio, ChannelTransport, E2eNode, etc.)
    └── e2e_tests.rs    # test cases T-E2E-001 through T-E2E-051
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
