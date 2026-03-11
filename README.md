<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde

**A programmable, verifiable runtime for distributed sensor nodes.**

Each node acts as a programmable sonde: a constrained probe that autonomously samples its environment and reports observations upstream.

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

## Further reading

- [Getting Started](docs/getting-started.md) — developer environment setup, toolchain installation, build and flash commands
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
