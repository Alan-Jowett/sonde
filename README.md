<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->

<p align="center">
  <img src="docs/sonde_logo.png" alt="Sonde logo showing a stylized bee labeled 'BPF' with circuit-board wings, sensor waveforms on the left, a radio tower emitting waves on the right, and the word 'SONDE' below." width="256">
</p>

# Sonde

[![CI](https://github.com/Alan-Jowett/sonde/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Alan-Jowett/sonde/actions/workflows/ci.yml)
[![ESP32-C3 Node](https://github.com/Alan-Jowett/sonde/actions/workflows/esp32.yml/badge.svg?branch=main)](https://github.com/Alan-Jowett/sonde/actions/workflows/esp32.yml)
[![ESP32-S3 Modem](https://github.com/Alan-Jowett/sonde/actions/workflows/esp32-modem.yml/badge.svg?branch=main)](https://github.com/Alan-Jowett/sonde/actions/workflows/esp32-modem.yml)
[![Tauri Desktop](https://github.com/Alan-Jowett/sonde/actions/workflows/tauri-desktop.yml/badge.svg?branch=main)](https://github.com/Alan-Jowett/sonde/actions/workflows/tauri-desktop.yml)
[![Tauri Android](https://github.com/Alan-Jowett/sonde/actions/workflows/tauri-android.yml/badge.svg?branch=main)](https://github.com/Alan-Jowett/sonde/actions/workflows/tauri-android.yml)
[![Nightly Release](https://github.com/Alan-Jowett/sonde/actions/workflows/nightly-release.yml/badge.svg?branch=main)](https://github.com/Alan-Jowett/sonde/actions/workflows/nightly-release.yml)
[![Coverage Status](https://coveralls.io/repos/github/Alan-Jowett/sonde/badge.svg?branch=main)](https://coveralls.io/github/Alan-Jowett/sonde?branch=main)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**A programmable sensor platform with desired-state convergence from cloud to node.**

Sonde turns battery-powered sensor nodes into programmable probes that keep static firmware but execute dynamic behavior defined by verified BPF programs ([RFC 9669](https://www.rfc-editor.org/rfc/rfc9669.html)). The gateway owns radio-facing orchestration; an external control plane owns fleet intent. Together they converge node state by flowing **desired state** down to the gateway and **actual state / application data** back upstream.

Nodes never need feature-driven firmware updates for normal application changes. New sampling logic, thresholds, diagnostics, and routing behavior ship as verified BPF plus gateway-managed state changes. The reference implementation targets ESP32-C3/S3 nodes and modems, with a cloud-agnostic connector model and an Azure reference deployment in this repository.

> **Status:** Active development, pre-1.0. The workspace includes the core protocol/gateway/node/modem stack, admin tooling, pairing surfaces, example handlers, and Azure companion/handler components. See [Project status](#project-status) below.

> ⚠️ **Spec-first project** — edit the [specification documents](docs/) before changing code. Code modules may be regenerated from specs. See [Contributing](docs/contributing.md#spec-first-development-model).

---

## Project status

**Lifecycle:** Active development — pre-1.0. APIs and wire formats may change between commits. Not yet recommended for production deployments.

**Maintenance:** The project is actively maintained. Bug reports, pull requests, and feature discussions are welcome. See [Contributing](docs/contributing.md) for guidelines.

**Versioning:** The project has not yet reached v1.0. Breaking changes to the wire protocol, connector schema, or crate APIs may still occur.

**Current repository shape:** the workspace already spans the device runtime, the gateway runtime, local admin/pairing tools, and cloud-facing integration surfaces.

| Component | Purpose |
|---|---|
| [`sonde-protocol`](crates/sonde-protocol) | Shared `no_std` wire format, CBOR message types, program-image encoding, and crypto abstractions |
| [`sonde-gateway`](crates/sonde-gateway) | Async gateway service: node sessions, program distribution, reconciliation hooks, connector I/O, and gRPC admin |
| [`sonde-node`](crates/sonde-node) | ESP32-C3/S3 firmware: wake cycle, BPF execution, persistent state, and radio protocol |
| [`sonde-modem`](crates/sonde-modem) | ESP32-S3 bridge between ESP-NOW radio and the gateway |
| [`sonde-admin`](crates/sonde-admin) | CLI for gateway administration and pairing flows |
| [`sonde-bpf`](crates/sonde-bpf) | Safe BPF interpreter used by the node runtime |
| [`sonde-pair`](crates/sonde-pair) + [`sonde-pair-ui`](crates/sonde-pair-ui/src-tauri) | BLE pairing core plus Tauri desktop/Android UI |
| [`sonde-azure-companion`](crates/sonde-azure-companion) | Reference connector/runtime bridge for Azure-backed control-plane sync |
| [`sonde-azure-handler`](crates/sonde-azure-handler) | Azure cloud-side reconciliation and data-ingest component |
| [`sonde-tmp102-handler`](crates/sonde-tmp102-handler) / [`sonde-sht40-handler`](crates/sonde-sht40-handler) | Example application handlers |
| [`sonde-e2e`](crates/sonde-e2e) | End-to-end validation harness |

CI runs on every push and PR and covers workspace build/test flows plus target-specific firmware and UI pipelines.

---

## How it works

```
┌───────────────────────────┐
│   Control plane / cloud   │
│ desired state, analytics, │
│ fleet policy, app ingest  │
└─────────────┬─────────────┘
              │ DESIRED_STATE / ACTUAL_STATE / APP_DATA
┌─────────────▼─────────────┐     local IPC      ┌──────────┐     USB      ┌──────────┐   ESP-NOW   ┌──────────┐
│ Connector / companion     │◄──────────────────►│ Gateway  │◄────────────►│  Modem   │◄───────────►│   Node   │
│ transport adapter         │                    │          │               │          │              │  BPF VM  │
└───────────────────────────┘                    └──────────┘               └──────────┘              └──────────┘
```

1. **The control plane declares intent** as complete desired state for one gateway or node at a time.
2. **A connector/companion transports that intent** to the gateway over the local connector API. The connector is cloud-agnostic; it adapts some external store-and-forward transport to Sonde's framed connector messages.
3. **Nodes wake on their own schedule** and send `WAKE` over ESP-NOW. Communication is always node-initiated.
4. **The gateway reconciles actual vs. desired state** at wake time, then responds with the next action: continue, update resident program, queue an ephemeral diagnostic, change schedule, rotate state, or reboot.
5. **The node executes its resident BPF program**, updates persistent maps, and emits application data when needed.
6. **The gateway publishes upstream actual state and app data** so the control plane can observe convergence and decide the next desired state.

The firmware never embeds application-specific policy. It provides the execution and transport substrate; BPF plus desired-state convergence define behavior.

---

## Architecture

The design separates five concerns:

| Layer | Lifetime | Location |
|---|---|---|
| **Firmware** | Static, uniform across all nodes | Flash |
| **Program logic** | Dynamic, delivered as BPF bytecode | Flash (resident) or RAM (ephemeral) |
| **Persistent state** | Survives deep sleep | Sleep-persistent memory |
| **Gateway runtime** | Always-on edge service | Gateway host |
| **Control plane** | External desired-state authority | Cloud or other upstream system |

This gives Sonde OTA-like flexibility without turning firmware rollout into the main application-delivery mechanism. New sensors, thresholds, schedules, diagnostics, and cloud-side routing changes are expressed as BPF programs plus desired state.

### Desired-state convergence

The connector protocol is organized around four message families:

- **`DESIRED_STATE`** — control plane to gateway, carrying complete desired state for one gateway or node.
- **`ACTUAL_STATE`** — gateway to control plane, carrying the latest observed gateway or node state.
- **`APP_DATA`** — gateway to control plane, carrying node-originated application payloads.
- **`CONNECTOR_HEALTH`** — gateway to control plane, carrying connector session health.

This makes the gateway the convergence engine at the edge: it accepts desired state from upstream, waits for node-initiated contact, applies the necessary changes, and reports the resulting actual state back upstream.

### Reference cloud deployment

This repository's reference cloud path is Azure-based:

- `sonde-azure-companion` bridges the gateway's local admin/connector surfaces to Azure runtime services.
- `sonde-azure-handler` consumes upstream connector traffic, stores actual state and sensor data, and emits downstream desired state.

The connector model itself is not Azure-specific, so other control-plane transports can implement the same gateway-facing contract.

---

## BPF programs

Nodes execute [BPF programs](docs/bpf-environment.md) that define all application behavior. Two classes exist:

- **Resident** — stored in flash, runs every wake cycle, full map read/write access.
- **Ephemeral** — one-shot diagnostic, stored in RAM, read-only maps, discarded after execution.

Programs are compiled to BPF ELF, verified by [Prevail](https://github.com/vbpf/ebpf-verifier) on the gateway, and distributed over the air. See the [BPF environment](docs/bpf-environment.md) doc for the full helper API, memory model, verification profiles, and development workflow.

---

## Radio protocol and security

Communication is always **node-initiated**. The gateway never wakes a node. Each radio frame uses:

- a fixed **11-byte binary header** (`key_hint`, `msg_type`, `nonce`),
- a CBOR payload encrypted with **AES-256-GCM**, and
- a 16-byte GCM authentication tag.

The header is authenticated as AEAD additional data; the payload is both encrypted and authenticated. Nodes and gateways use unique per-node 256-bit pre-shared keys. `WAKE` uses a random nonce; follow-on traffic uses gateway-assigned sequence numbers for replay protection.

The basic edge loop is still: node sends `WAKE` → gateway responds with `COMMAND` → node executes BPF → node sleeps. Programs are distributed via a node-driven chunked transfer. Application data flows as `APP_DATA`, with `APP_DATA_REPLY` used for request/response helper calls.

See [protocol.md](docs/protocol.md) for the full wire format and [security.md](docs/security.md) for the trust model.

### Security model

- Each node has a unique 256-bit pre-shared key stored in a dedicated flash partition.
- Keys are provisioned via USB-mediated pairing; no over-the-air key exchange.
- The gateway stores the key database and authenticates/decrypts all radio traffic with AES-256-GCM.
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
| **BPF execution** | Interpreter only (`sonde-bpf`, RFC 9669 compliant; no JIT for RISC-V/Xtensa). The `BpfInterpreter` trait allows alternative backends (e.g., rbpf, uBPF) to be plugged in. |
| **Max program size** | 4 KB resident, 2 KB ephemeral (recommended) |
| **Chunked transfer** | 4 KB program ≈ 20 round-trips over ESP-NOW |

### Canonical contributor hardware

Contributor-facing hardware docs and bring-up notes should assume these baseline builds unless a document explicitly says otherwise:

| Build | Base hardware | Notes |
|---|---|---|
| **Node** | [`hw/carrier-board`](hw/carrier-board) + Seeed Studio XIAO ESP32-C3 | Canonical battery-powered node build. The carrier board provides the Qwiic/I2C sensor connector and battery input. |
| **Modem** | [`hw/carrier-board`](hw/carrier-board) + Seeed Studio XIAO ESP32-S3 | Canonical USB modem build. Add a 128×64 SSD1306-compatible OLED on I2C0 (`GPIO5` SDA, `GPIO6` SCL, address `0x3C`) and an active-low button on `GPIO2`. |

---

## Building

```sh
# Fast protocol-only test
cargo test -p sonde-protocol

# Validate the workspace
cargo build --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
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
- [Node BOM](docs/node-bom.md) — canonical carrier-board + XIAO ESP32-C3 node build
- [Modem BOM](docs/modem-bom.md) — canonical carrier-board + XIAO ESP32-S3 modem build with local display/button
- [Contributing](docs/contributing.md) — contribution guidelines, DCO, SPDX requirements
- [Why BPF?](docs/why-bpf.md) — rationale for using BPF + Prevail as the execution model
- [BPF Environment](docs/bpf-environment.md) — program API, memory model, verification, and development workflow
- [Application API](docs/gateway-api.md) — data-plane API for building applications on the Sonde platform
- [Connector API](docs/gateway-companion-api.md) — desired-state / actual-state integration API for external control planes
- [Protocol](docs/protocol.md) — node-gateway wire protocol specification
- [Gateway Requirements](docs/gateway-requirements.md) — formal gateway requirements
- [Node Requirements](docs/node-requirements.md) — formal node firmware requirements
- [Security Model](docs/security.md) — threat model, key provisioning, authentication, replay protection, and failure modes
- [Azure Companion Requirements](docs/azure-companion-requirements.md) — Azure reference deployment for control-plane sync
- [Azure Provisioning Requirements](docs/azure-provisioning-requirements.md) — Bicep/bootstrap contract for the Azure deployment

---

## Related Work

Prior work has explored the use of eBPF‑derived virtual machines on microcontroller‑class devices to enable safe, dynamically deployable software modules. **Femto‑Containers** and **rBPF** integrate a reduced eBPF virtual machine into [RIOT‑OS](https://www.riot-os.org/), allowing small sandboxed programs to be deployed and executed on low‑power IoT devices, primarily to support DevOps‑style updates and fault isolation without reflashing firmware. These systems demonstrate that an eBPF‑like instruction set can be executed efficiently on resource‑constrained hardware and safely isolated from the host OS. Key references include:
- [Femto‑Containers paper](https://arxiv.org/abs/2106.12553)
- [Femto‑Containers code](https://github.com/future-proof-iot/Femto-Container)
- [rBPF paper](https://arxiv.org/abs/2011.12047)

Subsequent work, including **μBPF**, extends this line of research with just‑in‑time (JIT) compilation, over‑the‑air deployment pipelines, and formal verification to improve performance and provide stronger correctness guarantees for eBPF execution on microcontrollers. Key references include:
- [μBPF paper](https://marioskogias.github.io/docs/microbpf.pdf)
- [μBPF code](https://github.com/SzymonKubica/micro-bpf)

**rbpf** is a pure‑Rust user‑space eBPF interpreter (and optional JIT) that demonstrates BPF execution outside the Linux kernel in a safe, portable runtime:
- [rbpf code](https://github.com/qmonnet/rbpf)

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
