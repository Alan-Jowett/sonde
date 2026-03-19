<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Why BPF?

## Summary

This document describes the rationale for using BPF ([RFC 9669](https://www.rfc-editor.org/rfc/rfc9669.html), verified by [Prevail](https://github.com/vbpf/ebpf-verifier)) as the execution model for Sonde nodes. The goal is to make node firmware static and uniform, while all behavior — sampling logic, thresholds, batching, anomaly detection, diagnostics — is delivered dynamically as verified BPF bytecode. This enables a safe, flexible, low-power distributed system without OTA firmware updates.

---

## Motivation

Sonde nodes are battery-powered ESP32-class devices that wake briefly, read sensors, optionally transmit data, and return to deep sleep. Traditional firmware-centric designs require reflashing devices to change behavior, add sensors, or adjust sampling logic. This does not scale well across dozens of nodes deployed outdoors or in hard-to-reach locations.

We need a mechanism that allows:

- Dynamic behavior updates without reflashing
- Strong safety guarantees
- Predictable execution time and energy usage
- A stable ABI across all nodes
- A small, verifiable, sandboxed execution environment
- The ability to run both long-term logic and one-shot diagnostics

BPF provides exactly this.

---

## Why BPF is a good fit

### Deterministic and safe

BPF's instruction set and execution model are designed for safety:

- No arbitrary pointers
- No dynamic allocation
- Bounded loops
- Verifiable memory access
- Strict helper-call boundaries

This is ideal for correctness-critical, battery-powered nodes.

### Verifiable with Prevail

Prevail provides static guarantees:

- Bounded execution time
- Bounded memory access
- Safe helper usage
- No untrusted pointer arithmetic
- No unbounded loops

This ensures predictable wake-time energy usage and prevents runaway programs.

### Tiny, portable runtime (sonde-bpf)

`sonde-bpf` provides:

- A zero-allocation BPF interpreter (RFC 9669 compliant)
- Tagged registers for pointer provenance and memory safety
- `#![no_std]`-compatible — runs on ESP32-C3/S3
- No OS dependencies

This keeps node firmware minimal and uniform. The interpreter is injected via a `BpfInterpreter` trait, so alternative backends (e.g., rbpf, uBPF) can be substituted without changing the firmware.

### Dynamic behavior without firmware updates

Nodes run a single firmware image. All behavior is delivered as BPF bytecode:

- Sampling logic
- Thresholds and hysteresis
- Batching and compression
- Transmission rules
- Diagnostics and calibration

This eliminates the need for OTA firmware updates.

### Clean separation of logic and state

BPF maps are backed by RTC slow memory, giving:

- Persistent state across deep sleep
- No flash wear
- A simple, verifiable memory model

Programs become pure logic; maps hold state.

### Supports both resident and ephemeral programs

The architecture distinguishes:

- **Resident programs** — long-term behavior, stored in flash, full map access
- **Ephemeral programs** — one-shot diagnostics, stored in RAM, read-only map access

This gives powerful remote-ops capabilities without compromising safety.

---

## Architectural benefits

### Uniform firmware across all nodes

Nodes differ only in:

- Attached sensors
- Installed BPF program
- Persistent map contents

Everything else is identical.

### Gateway-driven control plane

The gateway:

- Compiles BPF programs
- Verifies them with Prevail
- Distributes programs and schedules
- Sends ephemeral commands
- Receives sensor data

Nodes remain simple and low-power.

### Predictable battery usage

Because BPF programs are statically verified and bounded:

- Wake time is predictable
- Energy usage is stable
- Nodes can run for months or years

### Scales cleanly

Adding new sensors or new logic does not require:

- Reflashing nodes
- Changing firmware
- Physical access

Only new BPF programs.

---

## Conclusion

BPF provides a safe, verifiable, low-power execution model that allows Sonde nodes to remain simple, uniform, and long-lived while still supporting dynamic behavior. The combination of `sonde-bpf` (a custom RFC 9669 interpreter) + Prevail gives us a correctness-critical runtime with strong safety guarantees and a clean operational story. This architecture is significantly more flexible and maintainable than traditional firmware-centric IoT designs.
