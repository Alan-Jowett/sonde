<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Hardware Roadmap

> **Document status:** Draft
> **Scope:** Staged hardware roadmap for the Sonde node — from early prototyping with devboards to a fully custom, integrated design.
> **Audience:** Contributors, hardware designers, and anyone interested in the project's hardware trajectory.
> **Related:** [node-bom.md](node-bom.md), [node-design.md](node-design.md), [overview.md](overview.md), [contributing.md](contributing.md)

---

## Purpose

This document describes the planned evolution of Sonde node hardware. The goal is to reach a stable, low-cost, season-long deploy-and-forget sensor node with fully open-source PCB and enclosure designs.

A phased approach is used so that each stage produces a fully functional, validated artifact before the next stage begins. Hardware risk is front-loaded into the earliest, cheapest phase.

---

## Licensing

Both PCB and enclosure designs will be released under a permissive open hardware license (e.g., [CERN-OHL-P v2](https://ohwr.org/cern_ohl_p_v2.txt)), the hardware equivalent of MIT. All design files (schematics, KiCad projects, STL/STEP files) will be published in this repository.

---

## Stage 1 — Prototype: Devboard + Breakout Sensors + Off-the-Shelf Enclosure

**Status:** Current

**Goal:** Validate firmware architecture and field behavior with minimal hardware risk.

### Hardware

| Component | Role |
|-----------|------|
| ESP32 devboard (e.g., Adafruit QT Py ESP32-C3) | Compute + radio |
| Sensor breakout boards (SHT40, BME280, etc.) | Environmental sensing via I²C |
| Standalone power regulator + battery pack | Power supply |
| Off-the-shelf enclosure | Mechanical housing |

### What to validate

- Firmware architecture and BPF program execution
- Sampling cadence and data fidelity
- Deep-sleep power budget and wake reliability
- Environmental behavior (temperature, humidity, rain)
- ESP-NOW radio range and link reliability

### Outcome

A fully functional, field-deployable prototype using commodity parts. No custom hardware required. Serves as the reference for all firmware development.

See [node-bom.md](node-bom.md) for the exact parts list and sourcing information.

---

## Stage 2 — Integrated Sensor Board

**Status:** Planned

**Goal:** Replace individual breakout boards with a single custom sensor PCB to reduce wiring, connectors, and mechanical variability.

### Changes from Stage 1

- All I²C sensors consolidated onto one custom PCB
- Standardized airflow geometry and sensor placement
- Reduced connector count and point-to-point wiring
- MCU remains the same devboard as Stage 1

### What to validate

- Sensor placement and airflow geometry against Stage 1 baselines
- BOM reproducibility and global part availability
- Mechanical fit with the Stage 1 enclosure

### Outcome

Stable, reproducible sensing hardware with a small BOM. The sensor board becomes the stable interface between firmware and the physical world.

---

## Stage 3 — Custom 3D-Printed Enclosure

**Status:** Planned

**Goal:** Design a printable enclosure optimized for the Stage 2 sensor board geometry, long-term outdoor durability, and ease of assembly.

### Design objectives

- Airflow channels matched to the sensor board layout
- Membrane placement for moisture ingress protection
- Rain shedding and UV resistance (PETG or ASA material)
- Mounting options for pole, wall, or stake deployment
- Assembly without specialized tools

### What to validate

- Rain shedding under sustained simulated rainfall
- UV degradation over a full outdoor season
- Assembly time and ergonomics for field deployment
- Membrane effectiveness vs. sensor lag tradeoff

### Outcome

A complete, open-source mechanical design tailored to the sensor board. CAD files (STEP, STL) published in this repository under CERN-OHL-P.

---

## Stage 4 — Custom MCU + Power Regulator Board

**Status:** Planned

**Goal:** Replace the devboard with a custom ESP32-based MCU board to minimize BOM, optimize sleep current, and achieve final hardware integration.

### Changes from Stage 3

- Custom ESP32-C3 (or equivalent) circuit replacing the devboard
- Integrated power regulator and battery connector
- Integrated programming interface (USB or tag-connect)
- Optimized PCB layout for ultra-low sleep current
- Mechanical alignment with the Stage 2 sensor board and Stage 3 enclosure

### What to validate

- Sleep current against the Stage 1 devboard baseline (target: ≤ 10 µA)
- Full firmware compatibility without changes
- Field programming and over-the-air BPF program update flow (no firmware OTA)
- BOM cost and global sourcing

### Outcome

Final hardware architecture suitable for production-quality nodes. Combined with the Stage 2 sensor board and Stage 3 enclosure, this completes the fully custom, open-source sonde node stack.

---

## Summary

| Stage | Hardware | Status |
|-------|----------|--------|
| 1 | Devboard + breakout sensors + off-the-shelf enclosure | Current |
| 2 | Custom integrated sensor PCB | Planned |
| 3 | Custom 3D-printed enclosure | Planned |
| 4 | Custom MCU + power regulator board | Planned |

Hardware complexity increases gradually across stages. Each stage builds on a validated foundation, so contributors can focus effort on the current stage without needing visibility into future stages.
