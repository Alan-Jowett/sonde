<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->

<p align="center">
  <img src="docs/sonde_logo.png" alt="Sonde logo showing a stylized bee labeled 'BPF' with circuit-board wings, sensor waveforms on the left, a radio tower emitting waves on the right, and the word 'SONDE' below." width="256">
</p>

# Sonde

[![CI](https://github.com/Alan-Jowett/sonde/workflows/CI/badge.svg)](https://github.com/Alan-Jowett/sonde/actions/workflows/ci.yml)
[![ESP32-C3 Node](https://github.com/Alan-Jowett/sonde/workflows/ESP32-C3%20Node%20Firmware%20CI/badge.svg)](https://github.com/Alan-Jowett/sonde/actions/workflows/esp32.yml)
[![ESP32-S3 Modem](https://github.com/Alan-Jowett/sonde/workflows/ESP32-S3%20Modem%20Firmware%20CI/badge.svg)](https://github.com/Alan-Jowett/sonde/actions/workflows/esp32-modem.yml)
[![Tauri Desktop](https://github.com/Alan-Jowett/sonde/workflows/Tauri%20Desktop%20Build/badge.svg)](https://github.com/Alan-Jowett/sonde/actions/workflows/tauri-desktop.yml)
[![Tauri Android](https://github.com/Alan-Jowett/sonde/workflows/Tauri%20Android%20APK/badge.svg)](https://github.com/Alan-Jowett/sonde/actions/workflows/tauri-android.yml)
[![Nightly Release](https://github.com/Alan-Jowett/sonde/workflows/Nightly%20Release/badge.svg)](https://github.com/Alan-Jowett/sonde/actions/workflows/nightly-release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**A programmable, verifiable runtime for distributed sensor nodes.**

Each node acts as a programmable sonde: a constrained probe that autonomously samples its environment and reports observations upstream.

Nodes run uniform firmware and execute behavior defined by [uBPF](https://github.com/iovisor/ubpf) programs verified with [Prevail](https://github.com/vbpf/ebpf-verifier). A gateway distributes programs, schedules, and configuration over the air — no firmware updates required. The architecture is hardware-agnostic; the reference implementation targets ESP32-C3/S3.

> **Status:** Active development — protocol, gateway, modem, and node crates are implemented and tested. See [Project status](#project-status) below.

---

## Project status

**Lifecycle:** Active development — pre-1.0. APIs and wire formats may change between commits. Not yet recommended for production deployments.

**Maintenance:** The project is actively maintained. Bug reports, pull requests, and feature discussions are welcome. See [Contributing](docs/contributing.md) for guidelines.

**Versioning:** The project has not yet reached v1.0. Breaking changes to the wire protocol or crate APIs will be noted in commit messages and PR descriptions. A stable v1.0 release is planned after the full system (gateway + modem + node + admin) has been validated end-to-end.

**Roadmap:** Core protocol, gateway, modem, and node crates are complete. Remaining work includes the BLE pairing tool (`sonde-pair`), end-to-end validation, and hardening for production deployments. See [implementation-guide.md](docs/implementation-guide.md) for the phased build plan.

| Crate | Purpose | Status |
|---|---|---|
| [`sonde-protocol`](crates/sonde-protocol) | `no_std` wire format: frame codec, CBOR messages, program images | ✅ Complete — 41 validation tests, 4 fuzz targets |
| [`sonde-gateway`](crates/sonde-gateway) | Async gateway service (tokio): sessions, program distribution, handler routing, gRPC admin | ✅ Core complete — handler routing and admin stubs in place |
| [`sonde-modem`](crates/sonde-modem) | ESP32-S3 USB-to-ESP-NOW bridge firmware | ✅ Functional — bridge logic and ESP-IDF drivers working |
| [`sonde-node`](crates/sonde-node) | ESP32-C3/S3 node firmware: wake cycle, BPF dispatch, program store | ✅ Core complete — wake cycle engine, 16 BPF helpers, A/B program store |
| [`sonde-pair`](crates/sonde-pair) | BLE pairing tool — Tauri v2 (Android / Windows / Linux) | 🚧 In progress — see [issue #163](https://github.com/Alan-Jowett/sonde/issues/163) |

CI runs on every push and PR: formatting, clippy, build, workspace tests, fuzz (protocol), and an ESP32 QEMU smoke test.

---

## How it works

```
┌──────────┐              ┌──────────┐              ┌──────────┐
│   Node   │   ESP-NOW    │  Modem   │     USB      │ Gateway  │
│          │──────────────│          │──────────────│          │
│  ┌────┐  │  WAKE ─────► │          │              │  ┌────┐  │
│  │ BPF│  │ ◄── COMMAND  │  bridge  │  serial ◄──► │  │ App│  │
│  └────┘  │  APP_DATA ─► │          │              │  └────┘  │
│          │              │          │              │          │
│  sleep   │              │ ESP32-S3 │              │  verify  │
└──────────┘              └──────────┘              └──────────┘
```

1. **Node wakes** and sends a `WAKE` message containing its program hash over ESP-NOW.
2. **Modem bridges** the radio frame to the gateway over USB (protocol-unaware, forwards opaque frames in both directions).
3. **Gateway responds** with a command: proceed, update program, run a diagnostic, change schedule, or reboot.
4. **Node executes** its resident BPF program, which can read sensors, update persistent maps, and send application data.
5. **Node sleeps** until the next scheduled interval (or earlier if the BPF program requests it).

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

Data is **authenticated but not encrypted** (integrity, not confidentiality). All messages use HMAC-SHA256 with per-node pre-shared keys. Replay protection uses session-scoped sequence numbers — no persistent replay state is required on either the node or the gateway. See [protocol.md § Authentication](docs/protocol.md#7--authentication) for details.

---

## Security Model

- Each node has a unique 256-bit pre-shared key stored in a dedicated flash partition.
- Keys are provisioned via USB-mediated pairing; no over-the-air key exchange.
- The gateway stores the key database and authenticates all messages with HMAC-SHA256.
- Nonces provide replay protection for WAKE; gateway-assigned sequence numbers protect all subsequent messages.
- BPF programs are integrity-checked by content hash at every transfer.
- Nodes can be factory-reset (erasing key, maps, and program) and re-paired with a fresh identity.

See [security.md](docs/security.md) for the complete security model: threat model, key provisioning, authentication, replay protection, identity binding, failure modes, and gateway failover.

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

See [gateway-requirements.md](docs/gateway-requirements.md) and [node-requirements.md](docs/node-requirements.md) for formal requirements.

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
| **Key storage** | Dedicated flash partition (software-accessible; security depends on secure boot / flash encryption; key erased on factory reset) |
| **Hardware crypto** | SHA-256, HMAC-SHA256, AES-128/256, hardware RNG (~10x faster than software) |
| **RAM** | C3: 400 KB (16 KB cache). S3: 512 KB |
| **Flash endurance** | ~100K erase cycles per 4 KB sector (273+ years at 1 update/day) |
| **BPF execution** | Interpreter only (no uBPF JIT for RISC-V/Xtensa) |
| **Max program size** | 4 KB resident, 2 KB ephemeral (recommended) |
| **Chunked transfer** | 4 KB program ≈ 20 round-trips over ESP-NOW |

---

## Building

```sh
# Build and test all host crates
cargo build --workspace
cargo test --workspace

# Test protocol crate only (fast, no deps)
cargo test -p sonde-protocol
```

Building the modem firmware requires the ESP-IDF Xtensa toolchain:

```sh
# Linux / macOS
. "$HOME/export-esp.sh"
cargo +esp build -p sonde-modem --features esp --target xtensa-esp32s3-espidf -Zbuild-std=std,panic_abort
```

```powershell
# Windows (PowerShell)
. ~/export-esp.ps1
cargo +esp build -p sonde-modem --features esp --target xtensa-esp32s3-espidf -Zbuild-std=std,panic_abort
```

See [Getting Started](docs/getting-started.md) for full toolchain setup.

---

## Further reading

- [Overview](docs/overview.md) — project summary, status, and goals
- [Getting Started](docs/getting-started.md) — developer environment setup, toolchain installation, build and flash commands
- [Contributing](docs/contributing.md) — contribution guidelines, DCO, SPDX requirements
- [Why BPF?](docs/why-bpf.md) — rationale for using uBPF + Prevail as the execution model
- [BPF Environment](docs/bpf-environment.md) — program API, memory model, verification, and development workflow
- [Application API](docs/gateway-api.md) — data-plane API for building applications on the Sonde platform
- [Protocol](docs/protocol.md) — node-gateway wire protocol specification
- [Gateway Requirements](docs/gateway-requirements.md) — formal gateway requirements
- [Node Requirements](docs/node-requirements.md) — formal node firmware requirements
- [Security Model](docs/security.md) — threat model, key provisioning, authentication, replay protection, and failure modes

---

## Related Work

Prior work has explored the use of eBPF‑derived virtual machines on microcontroller‑class devices to enable safe, dynamically deployable software modules. **Femto‑Containers** and **rBPF** integrate a reduced eBPF virtual machine into [RIOT‑OS](https://www.riot-os.org/), allowing small sandboxed programs to be deployed and executed on low‑power IoT devices, primarily to support DevOps‑style updates and fault isolation without reflashing firmware. These systems demonstrate that an eBPF‑like instruction set can be executed efficiently on resource‑constrained hardware and safely isolated from the host OS. Key references include:
- [Femto‑Containers paper](https://arxiv.org/abs/2106.12553)
- [Femto‑Containers code](https://github.com/future-proof-iot/Femto-Container)
- [rBPF paper](https://arxiv.org/abs/2011.12047)

Subsequent work, including **μBPF**, extends this line of research with just‑in‑time (JIT) compilation, over‑the‑air deployment pipelines, and formal verification to improve performance and provide stronger correctness guarantees for eBPF execution on microcontrollers. Key references include:
- [μBPF paper](https://marioskogias.github.io/docs/microbpf.pdf)
- [μBPF code](https://github.com/SzymonKubica/micro-bpf)

Related efforts also focus on formally verified eBPF interpreters and JITs for RIOT‑based systems, emphasizing proof‑carrying safety and memory isolation rather than general application architecture. See also:
- [End‑to‑end mechanized proof of rBPF](https://link.springer.com/chapter/10.1007/978-3-031-65627-9_16)

In contrast to these systems, which primarily treat eBPF as a *mechanism* for hosting isolated application fragments within a general‑purpose embedded operating system, **Sonde** adopts BPF as the *primary application execution model*. Sonde intentionally freezes node firmware and delegates all application behavior—including sampling logic, thresholds, diagnostics, and scheduling—to verified BPF bytecode managed by a gateway‑driven control plane. This design emphasizes end‑to‑end behavioral control, predictable energy usage, and verification‑first safety guarantees, rather than OS extensibility, multi‑tenant execution, or embedded DevOps tooling.

---

## Contributing

See [docs/contributing.md](docs/contributing.md) for full guidelines.

All contributions must include:

1. **SPDX license headers** — every `.md` and `.rs` file must start with:

   *Markdown:*
   ```
   <!-- SPDX-License-Identifier: MIT
     Copyright (c) 2026 sonde contributors -->
   ```
   *Rust:*
   ```rust
   // SPDX-License-Identifier: MIT
   // Copyright (c) 2026 sonde contributors
   ```

2. **DCO sign-off** — every commit must include a `Signed-off-by:` trailer (use `git commit -s`).

Install the repository's git hooks so these rules are enforced locally:

```sh
git config core.hooksPath hooks
```

Alternatively, if you use the [pre-commit](https://pre-commit.com) framework:

```sh
pip install pre-commit
pre-commit install --hook-type pre-commit --hook-type commit-msg
```

---

## License

[MIT](LICENSE)
