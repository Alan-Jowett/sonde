<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde — Project Overview

> **Document status:** Draft
> **Scope:** High-level introduction to the Sonde project: what it is, its current status, goals, and where to find more information.
> **Audience:** New contributors, evaluators, and users who want to understand the project before diving into technical details.
> **Related:** [README.md](../README.md), [getting-started.md](getting-started.md), [contributing.md](contributing.md)

---

## What is Sonde?

Sonde is a programmable, verifiable runtime for distributed sensor nodes. Each node acts as a constrained probe — an instrument that autonomously samples its environment and reports observations to a gateway upstream.

Nodes run **uniform firmware** and execute behavior defined by [uBPF](https://github.com/iovisor/ubpf) programs verified by [Prevail](https://github.com/vbpf/ebpf-verifier). A gateway distributes programs, schedules, and configuration over the air — no firmware updates required. The reference implementation targets ESP32-C3/S3.

---

## Repository status

**Lifecycle:** Active development — pre-1.0.

APIs and the wire protocol may change between commits. The project is not yet recommended for production deployments.

**Maintenance:** The project is actively maintained. Bug reports, pull requests, and feature discussions are welcome. See [contributing.md](contributing.md) for guidelines on how to participate.

**Stability:** Core crates are implemented and tested. The table below shows the current status of each crate:

| Crate | Purpose | Status |
|---|---|---|
| [`sonde-protocol`](../crates/sonde-protocol) | `no_std` wire format: frame codec, CBOR messages, program images | ✅ Complete — 41 validation tests, 4 fuzz targets |
| [`sonde-gateway`](../crates/sonde-gateway) | Async gateway service (tokio): sessions, program distribution, handler routing, gRPC admin | ✅ Core complete |
| [`sonde-modem`](../crates/sonde-modem) | ESP32-S3 USB-to-ESP-NOW bridge firmware | ✅ Functional |
| [`sonde-node`](../crates/sonde-node) | ESP32-C3/S3 node firmware: wake cycle, BPF dispatch, program store | ✅ Core complete |
| [`sonde-pair`](../crates/sonde-pair) | BLE pairing tool — Tauri v2 (Android / Windows / Linux) | 🚧 In progress |

**Versioning:** The project has not yet reached v1.0. A stable v1.0 release is planned after the full system (gateway + modem + node + admin) has been validated end-to-end.

**Roadmap:** See [implementation-guide.md](implementation-guide.md) for the phased build plan and [README.md § Project status](../README.md#project-status) for the latest crate-level status.

---

## Goals

1. **Programmable behavior without firmware updates** — deploy new sensor logic, thresholds, and diagnostics as verified BPF bytecode.
2. **Verifiable safety** — all BPF programs are formally verified by Prevail before distribution; nodes will not execute unverified programs.
3. **Low-power, radio-based operation** — nodes use ESP-NOW for connectionless communication and deep sleep between wake cycles.
4. **Minimal trusted computing base** — firmware is intentionally frozen; all application behavior lives in the BPF program.
5. **Hardware-agnostic design** — platform-specific behavior is injected via traits; the core protocol and gateway are hardware-independent.

---

## Architecture summary

The design separates four concerns:

| Layer | Lifetime | Location |
|---|---|---|
| **Firmware** | Static, uniform across all nodes | Flash |
| **Program logic** | Dynamic, delivered as BPF bytecode | Flash (resident) or RAM (ephemeral) |
| **Persistent state** | Survives deep sleep | Sleep-persistent memory |
| **Control plane** | Gateway-driven | Gateway |

The workspace contains:

| Crate | Role |
|---|---|
| `sonde-protocol` | Shared `no_std` protocol: frame codec, CBOR messages, program image format |
| `sonde-gateway` | Async gateway: authenticates nodes, distributes programs, routes app data |
| `sonde-node` | ESP32 firmware: wake/sleep cycle, BPF execution, NVS storage |
| `sonde-modem` | ESP32-S3 bridge: relays ESP-NOW frames between radio and USB |
| `sonde-admin` | CLI wrapping the gateway gRPC admin API |
| `sonde-pair` | BLE pairing tool (planned — Tauri v2, Android + desktop) |
| `sonde-bpf` | Safe BPF interpreter with tagged register tracking |
| `sonde-e2e` | End-to-end test harness |

---

## Further reading

- [README.md](../README.md) — project introduction, architecture, building, and usage
- [Getting Started](getting-started.md) — developer environment setup and build instructions
- [Contributing](contributing.md) — how to contribute, code style, and requirements
- [Implementation Guide](implementation-guide.md) — phased build plan and module ordering
- [Protocol](protocol.md) — node-gateway wire protocol specification
- [Security Model](security.md) — threat model, key provisioning, authentication, and replay protection
- [BPF Environment](bpf-environment.md) — BPF program API, memory model, and development workflow
- [Why BPF?](why-bpf.md) — rationale for using uBPF + Prevail as the execution model
