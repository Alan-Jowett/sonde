<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Canonical Sonde Node BOM (Carrier Board + XIAO ESP32-C3)

> **Document status:** Draft
> **Scope:** Canonical contributor hardware for a battery-powered ESP-NOW sonde node.
> **Audience:** Contributors, field deployers, and anyone building the reference node hardware.
> **Related:** [node-design.md](node-design.md), [getting-started.md](getting-started.md), [../hw/carrier-board/README.md](../hw/carrier-board/README.md)

---

## Design philosophy

- **Canonical** — contributor docs should refer to one baseline node build.
- **Reproducible** — the base PCB lives in this repository under `hw/carrier-board/`.
- **Low power** — the carrier board is designed for battery-backed node operation.
- **Sensor-agnostic** — the board exposes a Qwiic/STEMMA QT I2C connector; sensor behavior lives in BPF programs, not firmware.

---

## 1  Core platform

| Item | Description | Notes |
|------|-------------|-------|
| `hw/carrier-board` PCB | Sonde carrier board from this repository | Canonical base PCB |
| Seeed Studio XIAO ESP32-C3 | ESP32-C3 module with USB-C | Socketed MCU module for the canonical node build |
| 2×AA lithium battery pack | Primary battery supply | Connects to the carrier board battery input |
| Qwiic/STEMMA QT cable | JST SH 4-pin I2C cable | Connects sensors to the carrier board |

> **Canonical build:** Use the carrier board with a Seeed Studio XIAO ESP32-C3. Other ESP32-C3 boards may still be useful for bench experiments, but contributor-facing docs should assume this combination.

---

## 2  Wiring summary

### Power

The canonical node build uses the carrier board's direct-battery design:

```
2×AA pack ──→ carrier-board battery connector ──→ XIAO ESP32-C3 3V3 / GND
```

See [`hw/carrier-board/README.md`](../hw/carrier-board/README.md) for the board-level power details and assembly notes.

### Sensor I2C

The carrier board routes the canonical node sensor bus to the Qwiic/STEMMA QT header:

| Signal | GPIO | Description |
|--------|------|-------------|
| SDA    | 6    | I2C data |
| SCL    | 7    | I2C clock |

Any 3.3 V I2C sensor with a Qwiic/STEMMA QT connector is supported. The node firmware reads `i2c0_sda` and `i2c0_scl` from NVS and falls back to compiled-in defaults of `GPIO0`/`GPIO1` if they are not configured, so provision or configure the canonical carrier-board build to use `GPIO6` for SDA and `GPIO7` for SCL.

---

## 3  Suggested add-ons

| Item | Purpose |
|------|---------|
| Enclosure | Protects the carrier board and battery pack in the field |
| Cable gland / strain relief | Protects the sensor cable exit |
| Silicone adhesive or standoffs | Secures the board inside the enclosure |
| Desiccant / conformal coating | Helps with moisture-prone deployments |

---

## 4  Block diagram

```
+----------------+     +----------------------+     +-------------------+     +--------------+
| 2×AA lithium   |---->| Sonde carrier board  |---->| XIAO ESP32-C3     |---->| Qwiic/STEMMA |
| battery pack   |     | battery + sensor I/O |     | sonde-node fw     |     | QT sensor(s) |
+----------------+     +----------------------+     +-------------------+     +--------------+
                                                           |
                                                           | ESP-NOW
                                                           v
                                                    +--------------+
                                                    |    Modem     |
                                                    |  + gateway   |
                                                    +--------------+
```

---

## 5  Notes

- **A custom PCB is part of the canonical build.** Use the repository's `hw/carrier-board` design as the base hardware.
- **Sensors are not included.** Sonde follows a BYOS (Bring Your Own Sensor) model.
- **The carrier board README is the assembly source of truth.** Use it for battery, connector, and mechanical details.
