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
│  sleep   │                    │  verify  │
│          │                    │          │
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

## BPF programs

Nodes execute [BPF programs](docs/bpf-environment.md) that define all application behavior. Two classes exist:

- **Resident** — stored in flash, runs every wake cycle, full map read/write access.
- **Ephemeral** — one-shot diagnostic, stored in RAM, read-only maps, discarded after execution.

Programs are compiled to BPF ELF, verified by [Prevail](https://github.com/vbpf/ebpf-verifier) on the gateway, and distributed over the air. See the [BPF environment](docs/bpf-environment.md) doc for the full helper API, memory model, verification profiles, and development workflow.

---

## Node-gateway protocol

Communication is always **node-initiated**. The gateway never wakes a node. Messages use a fixed binary header, CBOR-encoded payload, and HMAC-SHA256 authentication. See [protocol.md](docs/protocol.md) for the full wire specification.

The basic cycle: node sends `WAKE` → gateway responds with a `COMMAND` (proceed, update program, change schedule, reboot) → node executes BPF → node sleeps. Programs are distributed via a node-driven chunked transfer. Application data is sent as `APP_DATA`; for request/response flows, the gateway replies with `APP_DATA_REPLY` when using `send_recv()`, while `send()` is fire-and-forget.

---

## Authentication

Data is **authenticated but not encrypted** (integrity, not confidentiality). All messages use HMAC-SHA256 with per-node pre-shared keys. Replay protection uses per-message random nonces with a 64-entry sliding window. See [protocol.md § Authentication](docs/protocol.md#7--authentication) for details.

---

## BPF program environment

BPF programs have access to raw bus primitives (I2C, SPI, GPIO, ADC), communication helpers (`send`, `send_recv`), persistent maps, and system functions. The firmware provides bus access; sensor-specific protocols live in the BPF program. See [bpf-environment.md](docs/bpf-environment.md) for the full helper API, memory model, verification profiles, and development workflow.

---

## Application handlers

The gateway is a platform service — application logic runs in a separate **handler process**. When a BPF program calls `send()` or `send_recv()`, the gateway forwards the data to the handler via stdin (length-prefixed CBOR). The handler processes it and replies via stdout. Handlers are routed by `program_hash`, so different BPF programs can have different handlers.

The developer ships two artifacts: a **BPF ELF** (node-side) and a **handler** in any language (gateway-side). See [gateway-api.md](docs/gateway-api.md) for the full handler protocol, message format, and examples.

---

## Operational concerns

- **Gateway failover** — replace the gateway with another instance provisioned with the same key database. Nodes won't notice.
- **Development** — BPF programs are platform-agnostic. Compile, verify, and test locally with `libsonde_test` — no hardware needed.
- **Diagnostics** — push an ephemeral program to inspect node state without disturbing the resident program.
- **Firmware updates** — physical access only. By design, firmware changes are rare — new features ship as BPF programs.

See [gateway-requirements.md](docs/gateway-requirements.md) for formal operational requirements.

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
- [BPF Environment](docs/bpf-environment.md) — program API, memory model, verification, and development workflow
- [Application API](docs/gateway-api.md) — data-plane API for building applications on the Sonde platform
- [Protocol](docs/protocol.md) — node-gateway wire protocol specification
- [Gateway Requirements](docs/gateway-requirements.md) — formal gateway requirements

---

## License

[MIT](LICENSE)
