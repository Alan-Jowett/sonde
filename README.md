# Sonde

**A programmable, verifiable runtime for distributed sensor nodes.**

Nodes run uniform firmware and execute behavior defined by [uBPF](https://github.com/iovisor/ubpf) programs verified with [Prevail](https://github.com/vbpf/ebpf-verifier). A gateway distributes programs, schedules, and configuration over the air — no firmware updates required. The architecture is hardware-agnostic; the reference implementation targets ESP32-C3/S3.

> **Status:** Design phase. This document is the specification.

---

## How it works

```
┌──────────┐                    ┌──────────┐
│   Node   │  ── WAKE ───────►  │ Gateway  │
│          │  ◄── COMMAND ────  │          │
│  ┌────┐  │                    │  ┌────┐  │
│  │ BPF│──│── APP_DATA ─────►  │  │ App│  │
│  └────┘  │  ◄─APP_DATA_REPLY  │  └────┘  │
│          │                    │          │
│  sleep   │                    │  compile │
│          │                    │  verify  │
└──────────┘                    └──────────┘
```

1. **Node wakes** and sends a `WAKE` message containing its program hash.
2. **Gateway responds** with a command: proceed, update program, run a diagnostic, change schedule, or reboot.
3. **Node executes** its resident BPF program, which can read sensors, update persistent maps, and send application data.
4. **Node sleeps** until the next scheduled interval (or earlier if the BPF program requests it).

The firmware never interprets application data — it just transports opaque blobs between the BPF program and the gateway.

---

## Architecture

The design cleanly separates four concerns:

| Layer | Lifetime | Location |
|---|---|---|
| **Firmware** | Static, uniform across all nodes | Flash |
| **Program logic** | Dynamic, delivered as BPF bytecode | Flash (resident) or RAM (ephemeral) |
| **Persistent state** | Survives deep sleep | Sleep-persistent memory |
| **Control plane** | Gateway-driven | Gateway |

This gives you OTA-like flexibility without OTA complexity. New sensors, new logic, new thresholds — all delivered as BPF programs.

---

## Two classes of BPF programs

### Resident (long-term)
- Installed by the gateway, stored in flash.
- Runs on a schedule with full map read/write access.
- Implements steady-state behavior: sampling, thresholds, batching, transmission.

### Ephemeral (one-shot)
- Sent for immediate execution, stored in RAM, discarded after.
- Read-only access to maps. Stricter verifier profile (no map writes, small instruction budget).
- Used for diagnostics and remote introspection.

---

## Node-gateway protocol

Communication is always **node-initiated**. The gateway never wakes a node. Control plane messages use a fixed binary header followed by a CBOR-encoded payload (see [protocol.md](docs/protocol.md) for the full wire specification).

### Wake handshake

```
Node → Gateway:  WAKE  [header: key_hint, nonce]  { firmware_abi_version, program_hash, battery_mv }
Gateway → Node:  COMMAND  [header: key_hint, nonce]  { command_type, ... }
```

The program hash lets the gateway detect stale programs without version numbering — the program's identity is its content.

### Commands

| Command | Description |
|---|---|
| `NOP` | Proceed to BPF execution |
| `UPDATE_PROGRAM` | New resident program available (chunked transfer) |
| `RUN_EPHEMERAL` | One-shot program available (chunked transfer, same as UPDATE_PROGRAM) |
| `UPDATE_SCHEDULE` | New base wake interval |
| `REBOOT` | Restart firmware |

### Schedule model

The gateway sets a base interval. The BPF program can request an **earlier** wake via `set_next_wake(seconds)` but cannot extend beyond the gateway's interval.

### Chunked program transfer

```
Node → Gateway:  GET_CHUNK  [header: key_hint, nonce]  { chunk_index }
Gateway → Node:  CHUNK  [header: key_hint, nonce]  { chunk_index, chunk_data }
   ... repeat ...
Node:            Verify hash over complete program → store to flash
Node → Gateway:  PROGRAM_ACK  [header: key_hint, nonce]  { program_hash }
```

Node-driven, stop-and-wait. If power is lost mid-transfer, the node retries from chunk 0 on the next wake. After `PROGRAM_ACK`, the node executes the new program immediately in the same wake cycle.

### Application data

```
Node → Gateway:  APP_DATA  [header: key_hint, nonce]  { blob }
Gateway → Node:  APP_DATA_REPLY  [header: key_hint, nonce]  { blob }
```

Firmware wraps `send(ptr, len)` output as `APP_DATA`. The gateway replies with `APP_DATA_REPLY`, creating a bidirectional application channel. The BPF program and gateway application define their own request/response semantics on top — the protocol treats all blobs as opaque. Multiple round-trips per wake cycle are supported.

---

## Memory model

| Region | Persistence | Purpose |
|---|---|---|
| **Context** | Per-wake (read-only) | Sensor values, battery, timestamp, metadata |
| **Scratch** | Volatile | BPF working memory, lost on sleep |
| **Maps** | Sleep-persistent | BPF maps backed by sleep-persistent memory |
| **Flash** | Permanent | Resident program, schedule, A/B partitions |

Map layout is defined in the BPF program ELF using standard BPF map definitions and delivered alongside the program. Firmware allocates maps and enforces the platform's memory budget.

---

## Helper API

```c
read_sensor(id, buf_ptr, buf_len)   // returns 0 on success, nonzero on failure
send(ptr, len)                       // emit opaque APP_DATA blob
map_lookup_elem(map_id, key_ptr)
map_update_elem(map_id, key_ptr, value_ptr)  // resident only
get_time()
get_battery_mv()
set_next_wake(seconds)               // request earlier wake
bpf_trace_printk(fmt, fmt_len, ...)  // debug trace output
```

The ABI remains stable across firmware versions.

---

## Verification (Prevail)

All programs are verified before loading. Two profiles enforce different safety guarantees:

| | Resident | Ephemeral |
|---|---|---|
| **Loops** | Bounded | None or tightly bounded |
| **Map access** | Read/write | Read-only |
| **Instruction budget** | Larger | Small |
| **Helper set** | Full | Limited |
| **Side effects** | Allowed | None |

---

## Authentication

Data is **authenticated but not encrypted** (integrity, not confidentiality). All messages use HMAC-SHA256 with pre-shared keys.

```
┌──────────────────────────────────────────────────┐
│ Header (fixed binary, big-endian):               │
│   key_hint (2B) | msg_type (1B) | nonce (8B)     │
│ Payload: CBOR-encoded message body               │
│ HMAC-SHA256(header + payload, node_key) (32B)    │
└──────────────────────────────────────────────────┘
```

### Key provisioning
Each node receives a unique 256-bit key in secure storage during initial firmware flash. The gateway maintains the node-to-key mapping. No runtime key exchange.

### Replay protection
The node generates a fresh **64-bit random nonce** for every outbound message using a hardware RNG. The gateway tracks a per-node sliding window of seen nonces (64 entries). Gateway responses include the node's nonce, binding them to the request. **No persistent counter storage is needed on the node** — no flash wear, survives power loss.

### Overhead
43 bytes per frame (11-byte header + 32-byte HMAC). Negligible computation with hardware acceleration.

---

## Operational concerns

### Gateway failover
The gateway is identified only by its knowledge of node keys. Replace it with another gateway provisioned with the same key database. Nodes won't notice.

### Gateway unavailable
Bounded retries, then sleep until next interval. No gateway means no point running.

### Development and testing
BPF programs are platform-agnostic. Compile, verify, and run locally with uBPF — no hardware needed. Use `bpf_trace_printk` for debug output.

### Diagnostics
Push an ephemeral program to inspect map contents, read sensors, or report node state — without disturbing the resident program.

### Firmware updates
Physical access, same as initial provisioning. By design, firmware changes are rare — new features ship as BPF programs.

---

## Example use cases

All implemented as BPF programs, not firmware changes:

- *"Increase sampling frequency for the next 10 minutes."*
- *"Dump all persistent map contents for diagnostics."*
- *"Recalibrate soil sensor thresholds."*
- *"Send an immediate alert if temperature exceeds 35°C."*
- *"Run anomaly detection locally and only transmit deltas."*

---

## Reference implementation: ESP32-C3/S3

The reference implementation targets ESP32-C3 (RISC-V) and ESP32-S3 (Xtensa) running ESP-IDF.

| Aspect | Detail |
|---|---|
| **Radio transport** | ESP-NOW — connectionless 802.11, 250-byte frames (~207 bytes payload after auth overhead) |
| **Sleep-persistent memory** | RTC slow SRAM: 8 KB on C3, 8+8 KB on S3 (~4–6 KB usable for maps) |
| **Secure key storage** | eFuse blocks (up to 6, HMAC-purpose-only, inaccessible to software) |
| **Hardware crypto** | SHA-256, HMAC-SHA256, AES-128/256, hardware RNG (~10x faster than software) |
| **RAM** | C3: 400 KB (16 KB cache). S3: 512 KB |
| **Flash endurance** | ~100K erase cycles per 4 KB sector (273+ years at 1 update/day) |
| **BPF execution** | Interpreter only (no uBPF JIT for RISC-V/Xtensa) |
| **Max program size** | 4 KB resident, 2 KB ephemeral (recommended) |
| **Chunked transfer** | 4 KB program ≈ 20 round-trips over ESP-NOW |

---

## Further reading

- [Why BPF?](docs/why-bpf.md) — rationale for using uBPF + Prevail as the execution model
- [Application API](docs/gateway-api.md) — data-plane API for building applications on the Sonde platform
- [Protocol](docs/protocol.md) — node-gateway wire protocol specification
- [Gateway Requirements](docs/gateway-requirements.md) — formal gateway requirements

---

## License

[MIT](LICENSE)
