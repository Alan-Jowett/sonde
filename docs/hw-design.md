<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Sensor Node — Hardware Design (Generation Tool)

> **Document status:** Draft
> **Source:** Derived from [hw-requirements.md](hw-requirements.md).
> **Scope:** This document covers the architecture of the sonde hardware generation tool (`sonde-hw`).
> **Related:** [hw-requirements.md](hw-requirements.md), [node-design.md](node-design.md)

---

## 1  Overview

This document describes the architecture of the sonde hardware
generation tool (`sonde-hw`), which transforms a YAML board
configuration into validated KiCad files and manufacturing outputs.

The tool is the bridge between the parameterized requirements
(HW-0600–HW-0601) and the physical outputs (HW-0800–HW-0802).
It enforces all electrical and mechanical constraints at generation
time, so that any board it produces is guaranteed to pass ERC, DRC,
and netlist checks.

```
                    ┌─────────────┐
                    │ config.yaml │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  Validate   │ HW-0700 (schema check)
                    └──────┬──────┘
                           │
              ┌────────────▼────────────┐
              │   Template Expansion    │
              │                         │
              │  Base schematic         │
              │  + parameterized blocks │
              │  + sensor footprints    │
              └────────────┬────────────┘
                           │
                ┌──────────▼──────────┐
                │  Schematic (.kicad_sch) │ HW-0800
                └──────────┬──────────┘
                           │
                    ┌──────▼──────┐
                    │     ERC     │ HW-1000
                    └──────┬──────┘
                           │
                ┌──────────▼──────────┐
                │  Component Placer   │
                │  + Trace Router     │
                └──────────┬──────────┘
                           │
                ┌──────────▼──────────┐
                │   PCB (.kicad_pcb)  │ HW-0801
                └──────────┬──────────┘
                           │
                    ┌──────▼──────┐
                    │     DRC     │ HW-1001
                    └──────┬──────┘
                           │
              ┌────────────▼────────────┐
              │  Manufacturing Export   │
              │  Gerber + BOM + CPL     │ HW-0802
              └────────────┬────────────┘
                           │
                    ┌──────▼──────┐
                    │   Netlist   │ HW-1002
                    │   Verify   │
                    └─────────────┘
```

---

## 2  Tool architecture

### 2.1  Language and dependencies

The tool is a Python CLI application (`sonde-hw`) using:

| Dependency | Purpose |
|---|---|
| `pyyaml` | Parse board configuration |
| `jsonschema` | Validate config against schema |
| `kicad-skip` or `kiutils` | Read/write KiCad file formats |
| `subprocess` (kicad-cli) | Run ERC, DRC, Gerber export |

Python is chosen over Rust because:
- KiCad's scripting API is Python-native
- PCB generation is a batch process, not performance-critical
- The KiCad ecosystem tooling is Python-first

### 2.2  CLI interface

```
sonde-hw validate <config.yaml>                     # Schema check only
sonde-hw validate <config.yaml> --allow <check-id>  # Schema + rules, allowing specific checks
sonde-hw build <config.yaml>                        # Full pipeline
sonde-hw build <config.yaml> --skip-drc             # Generate without DRC (for iteration)
sonde-hw export <config.yaml>                       # Gerber + BOM + CPL only (assumes .kicad_pcb exists)
sonde-hw budget <config.yaml>                       # Power budget calculator
sonde-hw check-firmware <contract.yaml> --nvs-config <firmware-pins.yaml>  # Check firmware against hardware contract
# Commands that perform checks accept repeated --allow <check-id> flags to waive specific findings.
```

### 2.3  Directory structure

```
hw/
├── configs/                    # Board configurations
│   ├── minimal.yaml
│   ├── soil-monitor.yaml
│   └── environmental.yaml
├── templates/                  # Schematic building blocks
│   ├── base.kicad_sch          # MCU + USB + regulator (always included)
│   ├── qwiic.kicad_sch         # Single Qwiic connector block
│   ├── spi-header.kicad_sch    # SPI pin header block
│   ├── one-wire.kicad_sch      # 1-Wire header block
│   ├── battery.kicad_sch       # JST-PH + voltage divider
│   ├── power-gate.kicad_sch    # MOSFET sensor power switch
│   └── sensors/
│       ├── tmp102.kicad_sch
│       ├── sht40.kicad_sch
│       ├── bme280.kicad_sch
│       ├── veml7700.kicad_sch
│       └── ds18b20.kicad_sch
├── footprints/                 # Custom footprints (if not in KiCad stdlib)
├── rules/                      # Fab-specific DRC rules
│   ├── jlcpcb.kicad_dru
│   ├── pcbway.kicad_dru
│   └── oshpark.kicad_dru
├── output/                     # Generated outputs (per config)
│   └── <config-name>/
│       ├── board.kicad_sch
│       ├── board.kicad_pcb
│       ├── gerber/
│       ├── bom.csv
│       └── cpl.csv
├── schema.json                 # YAML config schema
└── sonde-hw.py                 # Generation tool
```

---

## 3  Schematic generation

### 3.1  Template composition

The schematic is built by composing template blocks:

1. **Base template** (`base.kicad_sch`) is always included. Contains:
   - ESP32-C3 module with all pin connections
   - USB-C connector with ESD protection
   - 3.3V voltage regulator with input/output decoupling
   - Boot/reset button circuit
   - Power LED (optional, controlled by config)

2. **Peripheral blocks** are conditionally included based on config:
   - Each block is a self-contained sub-schematic with defined
     interface nets (e.g., `I2C0_SDA`, `I2C0_SCL`, `3V3`, `GND`)
   - The tool merges blocks by connecting matching net names

3. **Sensor blocks** are included per the `sensors[]` list:
   - Each sensor block connects to a bus (I2C address, 1-Wire, ADC channel)
   - The tool validates that bus addresses don't conflict
   - Decoupling capacitors are included per sensor datasheet

### 3.2  Net naming convention

| Net name | Description |
|---|---|
| `3V3` | 3.3V power rail |
| `VBAT` | Battery voltage (pre-regulator) |
| `VUSB` | USB 5V input |
| `GND` | Ground |
| `I2C0_SDA` | I2C bus 0 data |
| `I2C0_SCL` | I2C bus 0 clock |
| `SPI0_MOSI` | SPI bus 0 master-out |
| `SPI0_MISO` | SPI bus 0 master-in |
| `SPI0_CLK` | SPI bus 0 clock |
| `SPI0_CS0` | SPI bus 0 chip select 0 |
| `OW0_DATA` | 1-Wire bus 0 data |
| `SENSOR_PWR_EN` | Sensor power gate enable |
| `GPIO_N` | General-purpose I/O pin N |

### 3.3  Reference designator assignment

Designators are assigned deterministically based on block order:

| Block | Range | Example |
|---|---|---|
| Base (MCU) | U1 | U1 = ESP32-C3 module |
| Base (regulator) | U2 | U2 = LDO/switching regulator |
| Base (USB ESD) | U3 | U3 = ESD protection IC |
| Qwiic 1 | J1 | J1 = Qwiic connector |
| Qwiic 2 | J2 | J2 = Qwiic connector |
| Battery | J3 | J3 = JST-PH battery |
| USB-C | J4 | J4 = USB-C receptacle |
| SPI header | J5 | J5 = SPI pin header |
| GPIO header | J6 | J6 = GPIO pin header |
| Sensors | U10+ | U10 = first sensor, U11 = second, etc. |
| Capacitors | C1+ | Sequential across all blocks |
| Resistors | R1+ | Sequential across all blocks |

---

## 4  PCB layout generation

### 4.1  Board outline

The tool generates a board outline based on `board.size`:

| Variant | Dimensions | Mounting holes |
|---|---|---|
| `compact` | 35mm × 25mm | 2× M2.5 |
| `standard` | 50mm × 30mm | 2× M2.5 |

### 4.2  Component placement algorithm

Placement is rule-based, not optimization-based (deterministic):

1. **Antenna zone**: top edge of board, no copper keepout per HW-0502.
2. **USB-C**: bottom edge, centered.
3. **ESP32-C3 module**: center of board, oriented with antenna toward
   top edge.
4. **Regulator**: near USB-C connector (short power path).
5. **Decoupling caps**: within 3mm of the IC they serve.
6. **Connectors**: along left and right edges, ordered by config list.
7. **Sensors**: remaining space, grouped by bus (I2C sensors near
   Qwiic connector, 1-Wire sensor near 1-Wire header).
8. **GPIO header**: along bottom edge opposite USB-C.

### 4.3  Routing strategy

1. **Power traces**: 0.4mm minimum width for 3V3 and VBAT.
2. **Signal traces**: 0.15mm (6 mil) default.
3. **USB D+/D-**: differential pair, 90Ω impedance (2-layer approximation).
4. **I2C**: routed together, ≤ 30mm total trace length.
5. **Ground pour**: bottom layer, with vias to top-layer GND pads.
6. **Routing order**: power → USB → I2C → SPI → GPIO → 1-Wire.

### 4.4  Layer stackup

| Layer | Usage |
|---|---|
| F.Cu (top) | Signal routing, component pads |
| B.Cu (bottom) | Ground pour, overflow routing |
| F.SilkS | Component outlines, labels |
| B.SilkS | Board name, version, config hash |
| F.Mask | Solder mask (top) |
| B.Mask | Solder mask (bottom) |
| Edge.Cuts | Board outline |

---

## 5  Validation pipeline

### 5.1  Pre-generation checks

Before generating any files:

1. **Schema validation** (HW-0700): config against JSON schema.
2. **Address conflict check**: no two I2C sensors share an address.
3. **Pin conflict check**: no two peripherals use the same GPIO.
4. **Size feasibility**: estimated component count fits board variant.
5. **Power budget estimate**: total sensor draw within regulator capacity.

### 5.2  Post-generation checks

After generating schematic and PCB:

1. **ERC** (HW-1000): `kicad-cli sch erc board.kicad_sch --exit-code-violations`.
2. **DRC** (HW-1001): `kicad-cli pcb drc board.kicad_pcb --exit-code-violations`.
3. **Netlist match** (HW-1002): compare schematic and PCB netlists.
4. **Antenna keepout** (HW-0502): verify no copper in keepout zone.
5. **BOM cost check** (HW-0601): total BOM ≤ target cost.

### 5.3  Output verification

After Gerber export:

1. **Gerber viewer render**: generate PNG renders of each layer for
   visual inspection (automated via `gerbv` or KiCad).
2. **Drill file sanity**: verify drill count matches via count in BOM.
3. **Board dimensions**: verify Gerber outline matches config.

---

## 6  Determinism guarantees (HW-0900)

To ensure reproducible builds:

1. **No random UUIDs**: KiCad assigns UUIDs to components. The tool
   generates deterministic UUIDs from a hash of (config hash + component
   path), e.g., `UUID = SHA256(config_hash + "U1")[:32]`.

2. **Deterministic timestamps**: KiCad file timestamps are either
   omitted or set to a value derived from the config hash or a fixed
   epoch; filesystem modification times are never used.

3. **Sorted outputs**: component lists, net lists, and BOM rows are
   sorted alphabetically to avoid ordering differences.

4. **Pinned KiCad version**: the tool records the KiCad version in the
   output metadata. Different KiCad versions may produce different file
   formats — the tool enforces a minimum version.

5. **Config hash traceability**: the SHA-256 hash of the config file is
   embedded in:
   - Schematic title block
   - PCB silkscreen (back layer)
   - Gerber file comments
   - BOM header row

---

## 7  Power + I/O contract integration

The generation tool produces a machine-checkable Power + I/O contract
(`hw/output/<config>/contract.yaml`) alongside the schematic and PCB
files (HW-1100–HW-1104).

### 7.1  Contract generation

The contract is derived from:
- The board configuration (YAML) — which peripherals, sensors, and
  power options are selected
- The schematic template data — rail voltages, pull-up values, pin
  assignments
- The ESP32-C3 datasheet — pin voltage domains, max currents, strap pins

The tool generates the contract during schematic generation (step 3 in
the pipeline). Each template block contributes its power and I/O entries
to the contract.

### 7.2  Contract checks in the pipeline

Contract invariant checks (HW-1103) run after DRC, before Gerber export:

```
validate → generate schematic → ERC → generate PCB → DRC
→ contract invariant checks → Gerber export
```

If any invariant check fails, the pipeline stops and reports the
violation with a precise error message. The operator can override
specific checks with `--allow <check-id>` for known-acceptable
deviations (documented in the config).

### 7.3  Firmware binding validation

When the firmware's NVS pin configuration (ND-0608) is available, the
tool can cross-check it against the contract:

```
sonde-hw check-firmware contract.yaml --nvs-config firmware-pins.yaml
```

This validates that the firmware's I2C pin assignments, power gate
GPIO, and peripheral modes match the board's electrical contract.

---

## 8  Cross-references

| Requirement | Design section |
|---|---|
| HW-0600 | §2 Tool architecture, §3 Schematic generation |
| HW-0601 | §2.3 Directory structure (configs/) |
| HW-0700 | §5.1 Pre-generation checks |
| HW-0701 | §2.3 Directory structure (configs/) |
| HW-0800 | §3 Schematic generation |
| HW-0801 | §4 PCB layout generation |
| HW-0802 | §2.3 Directory structure (output/) |
| HW-0900 | §6 Determinism guarantees |
| HW-0901 | §2.2 CLI interface |
| HW-0902 | §5 Validation pipeline |
| HW-1000 | §5.2 Post-generation checks |
| HW-1001 | §5.2 Post-generation checks |
| HW-1002 | §5.2 Post-generation checks |
| HW-1003 | §5.3 Output verification |
| HW-1100 | §7.1 Contract generation |
| HW-1101 | §7.1 Contract generation |
| HW-1102 | §7.1 Contract generation |
| HW-1103 | §7.2 Contract checks in the pipeline |
| HW-1104 | §7.2 Contract checks in the pipeline |
