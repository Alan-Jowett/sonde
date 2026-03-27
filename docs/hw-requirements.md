<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Sensor Node — Hardware Requirements

> **Document status:** Draft
> **Source:** Original specification for the sonde hardware design pipeline.
> **Scope:** This document covers the sonde **hardware** (sensor node PCB) component only.
> **Related:** [hw-design.md](hw-design.md), [node-requirements.md](node-requirements.md)

---

## 1  Purpose

This document defines requirements for a customizable ESP32-C3 sensor node
PCB that runs the sonde firmware. The design is parameterized: operators
select sensors, connectors, and peripherals from a menu, and the tooling
generates a board-specific schematic, layout, BOM, and Gerber files.

The goal is a single reference design that covers soil monitoring, air
quality, industrial sensing, and other applications — without requiring
a custom PCB for each deployment.

---

## 2  Scope

**In scope:**
- ESP32-C3 module (RISC-V, WiFi, BLE)
- Power regulation (battery + USB)
- Peripheral connectors (I2C/Qwiic, SPI, 1-Wire, GPIO)
- Direct sensor footprints (optional, parameterized)
- Programming/debug interface (USB-C)
- Antenna considerations (ESP-NOW range)

**Out of scope:**
- Modem hardware (separate ESP32-S3 board)
- Enclosure design
- Production testing fixtures (future)

---

## 3  Core components

### HW-0100  Microcontroller

**Priority:** Must

The board MUST use an ESP32-C3 module with integrated antenna, at least
4 MB flash, and all GPIO pins accessible.

**Acceptance criteria:**

1. ESP32-C3 module with integrated PCB antenna or U.FL connector.
2. Minimum 4 MB SPI flash.
3. All user-accessible GPIO pins routed to pads or connectors.

---

### HW-0101  USB-C connector

**Priority:** Must

The board MUST include a USB-C connector for programming, debugging,
and power input.

**Acceptance criteria:**

1. USB-C receptacle connected to the ESP32-C3 native USB peripheral.
2. USB provides 5V power input to the voltage regulator.
3. Compatible with `espflash` for firmware flashing without external tools.

---

### HW-0102  Voltage regulator

**Priority:** Must

The board MUST include a low-dropout (LDO) or switching regulator that
accepts a range of input voltages and provides 3.3V to the ESP32-C3.

**Acceptance criteria:**

1. Input voltage range: 3.0V–6V (covers single-cell LiPo discharge down to 3.0V cutoff and USB 5V).
2. Output: 3.3V ± 5%, minimum 500 mA continuous.
3. Quiescent current ≤ 10 µA (for battery-powered deep sleep).
4. Reverse polarity protection on battery input.

---

### HW-0103  Battery input

**Priority:** Should

The board SHOULD support direct battery power for untethered deployment.

**Acceptance criteria:**

1. JST-PH 2-pin connector for single-cell LiPo (3.7V nominal).
2. Battery voltage readable via ADC for firmware battery monitoring.
3. USB and battery inputs safely coexist (USB preferred when present).
4. Battery sense path (divider, buffer, or switch) MUST be high-impedance
   or switchable in deep sleep so that total deep-sleep current, including
   sensing leakage, complies with HW-0400 (≤ 20 µA).

---

## 4  Peripheral interfaces

### HW-0200  I2C bus (Qwiic/STEMMA QT)

**Priority:** Must

The board MUST provide at least one I2C bus with standardized connectors
for plug-and-play sensor modules.

**Acceptance criteria:**

1. I2C0 SDA and SCL routed to at least one 4-pin Qwiic/STEMMA QT
   connector (JST-SH 1.0mm pitch).
2. 4.7 kΩ pull-up resistors on SDA and SCL.
3. *Recommendation:* provide a second Qwiic connector for daisy-chaining;
   this is encouraged but not required for HW-0200 compliance.
4. GPIO pin assignments configurable via NVS (ND-0608) — the PCB
   default pin mapping is documented and matches the firmware defaults.

---

### HW-0201  SPI bus

**Priority:** Should

The board SHOULD expose an SPI bus for high-speed peripherals (displays,
SD cards, LoRa modules).

**Acceptance criteria:**

1. SPI MOSI, MISO, CLK, and CS routed to a pin header or connector.
2. At least one CS line; additional CS lines as GPIO breakout.
3. Compatible with 3.3V SPI peripherals.

---

### HW-0202  1-Wire bus

**Priority:** Should

The board SHOULD expose a 1-Wire bus for temperature sensors (DS18B20).

**Acceptance criteria:**

1. One GPIO pin routed to a 3-pin header (DATA, VCC, GND).
2. 4.7 kΩ pull-up resistor on the DATA line.
3. Screw terminal or JST connector option.

---

### HW-0203  GPIO breakout

**Priority:** Must

The board MUST expose unused GPIO pins for application-specific wiring.

**Acceptance criteria:**

1. All GPIO pins not consumed by I2C, SPI, USB, or bootstrapping
   are routed to 0.1" (2.54mm) pin headers.
2. Pin header silkscreen labels match GPIO numbers.
3. At least 4 GPIO pins available after I2C + USB allocation.

---

### HW-0204  ADC input

**Priority:** Should

The board SHOULD expose at least one ADC-capable pin for analog sensors.

**Acceptance criteria:**

1. At least one ADC channel routed to a pin header with GND reference.
2. Optional voltage divider footprint for sensors above 3.3V range.

---

## 5  Direct sensor footprints (parameterized)

### HW-0300  Sensor footprint slots

**Priority:** Should

The board SHOULD include optional footprints for common sensors that
can be populated or left empty based on the application.

**Acceptance criteria:**

1. Each sensor slot has a designated I2C address or GPIO pin.
2. Unpopulated slots do not interfere with other peripherals.
3. Sensor slots are selectable via the parameterized design tool.

---

### HW-0301  Supported sensor footprints

**Priority:** Should

The following sensor footprints SHOULD be available as options:

| Sensor | Interface | Package | Use case |
|--------|-----------|---------|----------|
| TMP102 | I2C (0x48) | SOT-563 | Temperature |
| SHT40 | I2C (0x44) | DFN-4 | Temperature + humidity |
| BME280 | I2C (0x76) | LGA-8 | Temp + humidity + pressure |
| VEML7700 | I2C (0x10) | — | Ambient light |
| DS18B20 | 1-Wire | TO-92 | Waterproof temperature probe |
| Soil moisture | ADC | 2-pin header | Capacitive soil sensor |

---

## 6  Power management

### HW-0400  Deep sleep current

**Priority:** Must

The board MUST achieve deep sleep current low enough for multi-month
battery operation.

**Acceptance criteria:**

1. Total board deep sleep current ≤ 20 µA (ESP32-C3 + regulator + pullups).
2. No active loads during deep sleep (LEDs off, peripherals power-gated
   or inherently low-power).

---

### HW-0401  Sensor power gating

**Priority:** Should

The board SHOULD support GPIO-controlled power to sensor connectors
so sensors can be fully powered off during deep sleep.

**Acceptance criteria:**

1. A MOSFET or load switch on the sensor power rail, controlled by a GPIO.
2. Firmware can enable/disable sensor power via `gpio_write()`.
3. Power gating is optional — can be bypassed with a solder jumper for
   always-on sensors.

---

## 7  Mechanical and manufacturing

### HW-0500  Board dimensions

**Priority:** Should

**Acceptance criteria:**

1. Maximum board size: 50mm × 30mm (fits standard project enclosures).
2. Mounting holes: 2× M2.5, positioned for standoff mounting.
3. All components on one side (single-side assembly for cost).

---

### HW-0501  Manufacturing constraints

**Priority:** Must

**Acceptance criteria:**

1. Minimum trace width: 0.15mm (6 mil).
2. Minimum clearance: 0.15mm (6 mil).
3. Minimum via diameter: 0.3mm drill, 0.6mm pad.
4. 2-layer PCB (cost-optimized; 4-layer option for RF performance).
5. Standard 1.6mm FR-4 substrate.
6. Compatible with JLCPCB / PCBWay / OSH Park manufacturing capabilities.

---

### HW-0502  Antenna keepout

**Priority:** Must

**Acceptance criteria:**

1. No copper (traces, ground plane, or components) within the antenna
   keepout zone specified by the ESP32-C3 module datasheet.
2. Board edge near antenna has no ground plane for at least 5mm.

---

## 8  Design parameterization

### HW-0600  Parameterized design tool

**Priority:** Must

The design MUST be parameterizable so that operators can select options
and generate a board-specific design without manual schematic editing.

**Acceptance criteria:**

1. A configuration file (YAML) specifies:
   - Number and type of Qwiic connectors (0, 1, or 2)
   - SPI header (yes/no)
   - 1-Wire header (yes/no)
   - Direct sensor footprints to populate
   - Battery connector (yes/no)
   - Sensor power gating (yes/no)
   - Board size variant (compact / standard)
2. The tool generates KiCad schematic (`.kicad_sch`) and PCB layout
   (`.kicad_pcb`) from the configuration.
3. The tool runs KiCad ERC and DRC checks and reports errors.
4. The tool generates Gerber files and BOM for manufacturing.

---

### HW-0601  Default configuration

**Priority:** Must

**Acceptance criteria:**

1. A default configuration exists that produces a minimal viable board:
   ESP32-C3 + USB-C + regulator + 1× Qwiic + GPIO breakout.
2. The default configuration passes all ERC and DRC checks.
3. The default BOM cost is ≤ $5 USD (excluding PCB fabrication).

---

## 9  Configuration format

### HW-0700  Board configuration schema

**Priority:** Must

The board configuration MUST be expressed as a YAML file with a
well-defined schema that captures all parameterizable options.

**Acceptance criteria:**

1. The schema is documented as a JSON Schema or equivalent formal definition.
2. The configuration file includes:
   - `mcu`: module variant (e.g., `esp32-c3-mini-1`)
   - `flash_size`: 4 / 8 / 16 MB
   - `connectors.qwiic`: count (0–2)
   - `connectors.spi`: boolean
   - `connectors.one_wire`: boolean
   - `connectors.gpio_header`: boolean
   - `connectors.battery`: boolean (JST-PH)
   - `sensors[]`: list of sensor IDs from the supported catalog (HW-0301)
   - `power.sensor_gating`: boolean
   - `power.regulator`: LDO or switching
   - `board.size`: `compact` | `standard`
   - `board.layers`: 2 | 4
   - `board.fab`: JLCPCB | PCBWay | oshpark | custom DRC rules
3. Unknown keys are rejected (strict parsing).
4. A `sonde-hw validate config.yaml` command checks the configuration
   against the schema and reports errors before generation.

---

### HW-0701  Configuration example

**Priority:** Must

**Acceptance criteria:**

1. The repository includes at least three example configurations:
   - `hw/configs/minimal.yaml` — ESP32-C3 + USB-C + 1× Qwiic
   - `hw/configs/soil-monitor.yaml` — battery + soil moisture ADC + DS18B20
   - `hw/configs/environmental.yaml` — BME280 + VEML7700 + 2× Qwiic
2. Each example passes schema validation and produces a valid board.

---

## 10  Output formats

### HW-0800  Schematic output

**Priority:** Must

**Acceptance criteria:**

1. The tool produces a KiCad 8 schematic file (`.kicad_sch`).
2. The schematic uses KiCad standard library symbols where available;
   custom symbols are included in the repository.
3. The schematic is human-readable and can be opened in KiCad for
   manual review or modification.
4. Net names match the firmware's GPIO naming (e.g., `I2C0_SDA`,
   `I2C0_SCL`, `SENSOR_PWR_EN`).

---

### HW-0801  PCB layout output

**Priority:** Must

**Acceptance criteria:**

1. The tool produces a KiCad 8 PCB file (`.kicad_pcb`).
2. Component placement follows a deterministic algorithm:
   - MCU centered on the board
   - USB-C connector on one edge
   - Antenna at the opposite edge with keepout enforced
   - Connectors along board edges
   - Decoupling capacitors adjacent to power pins
3. Trace routing is performed by either (a) a deterministic scripted
   routing algorithm (e.g., a KiCad Action Plugin or external autorouter
   invoked via script) or (b) a documented, replayable set of interactive
   routing steps. Manual routing adjustments are allowed post-generation.
4. The layout includes a ground pour on the bottom layer.
5. All footprints have 3D models for visual review.

---

### HW-0802  Manufacturing outputs

**Priority:** Must

**Acceptance criteria:**

1. The tool generates Gerber files (RS-274X) for all layers.
2. The tool generates an NC drill file (Excellon format).
3. The tool generates a BOM in CSV format with:
   - Component reference, value, footprint, manufacturer part number,
     supplier (LCSC / DigiKey / Mouser), unit cost, quantity.
4. The tool generates a component placement file (pick-and-place CPL)
   for SMT assembly.
5. All outputs are placed in a `hw/output/<config-name>/` directory.

---

## 11  Deterministic KiCad workflow

### HW-0900  Reproducible generation

**Priority:** Must

Given the same configuration file and tool version, the generation
pipeline MUST produce bit-identical outputs.

**Acceptance criteria:**

1. Running the tool twice with the same config produces identical
   `.kicad_sch`, `.kicad_pcb`, Gerber, and BOM files.
2. No timestamps, random UUIDs, or non-deterministic elements in
   the output files (or they are pinned to the config hash).
3. The tool version and config hash are embedded in the schematic
   title block and Gerber file comments for traceability.

---

### HW-0901  KiCad CLI integration

**Priority:** Must

The generation pipeline MUST be fully automatable via command-line
tools (no GUI interaction required).

**Acceptance criteria:**

1. Schematic generation uses KiCad's Python scripting API or
   direct file generation (no GUI).
2. ERC is run via `kicad-cli sch erc` or equivalent.
3. DRC is run via `kicad-cli pcb drc` or equivalent.
4. Gerber export is run via `kicad-cli pcb export gerbers` or equivalent.
5. The full pipeline (`validate → generate → ERC → DRC → Gerber`)
   is a single command: `sonde-hw build config.yaml`.

---

### HW-0902  CI integration

**Priority:** Should

**Acceptance criteria:**

1. A GitHub Actions workflow builds all example configurations on
   every push to `hw/` or `docs/hw-*.md`.
2. The workflow runs ERC + DRC and fails on violations.
3. The workflow uploads Gerber + BOM artifacts for download.

---

## 12  Verification and validation

### HW-1000  Electrical rule check (ERC)

**Priority:** Must

**Acceptance criteria:**

1. The generated schematic passes KiCad ERC with zero errors.
2. ERC warnings are documented and justified (e.g., intentional
   unconnected pins on the ESP32-C3 module).
3. ERC is run automatically as part of the build pipeline.

---

### HW-1001  Design rule check (DRC)

**Priority:** Must

**Acceptance criteria:**

1. The generated PCB passes KiCad DRC with zero errors using
   the design rules for the selected fabrication house.
2. DRC rules are stored as `.kicad_dru` files per fab house
   (e.g., `hw/rules/jlcpcb.kicad_dru`).
3. DRC is run automatically as part of the build pipeline.

---

### HW-1002  Netlist verification

**Priority:** Must

**Acceptance criteria:**

1. The tool verifies that the PCB netlist matches the schematic
   netlist (no unconnected nets, no extra nets).
2. Critical nets are explicitly checked:
   - 3.3V power rail connected to ESP32-C3 VDD
   - GND connected to ESP32-C3 GND
   - USB D+/D- connected to USB-C connector
   - I2C SDA/SCL connected to Qwiic connector pin 2/3
3. Netlist check is run automatically as part of the build pipeline.

---

### HW-1003  Simulation and emulation

**Priority:** Should

**Acceptance criteria:**

1. The tool can export a SPICE netlist for power rail simulation
   (regulator input/output, decoupling capacitor effectiveness).
2. An optional power budget calculator estimates battery life
   based on: deep sleep current, wake cycle duration, sensor power
   draw, and wake interval.
3. The power budget output is included in the build artifacts
   alongside the BOM.

---

## 13  Machine-checkable Power + I/O contract

A **machine-checkable Power + I/O contract** is a structured, versioned
specification that describes what the board guarantees (power rails,
electrical limits, I/O behavior) and what the firmware must assume — in
a way that can be validated automatically against the schematic, netlist,
BOM, and firmware pin configuration.

It is the hardware equivalent of an API contract: instead of function
signatures, it defines **rails, pins, states, and limits**. The goal is
to turn common integration failures into lintable violations (ERC-like
checks), rather than tribal knowledge discovered during bring-up.

### HW-1100  Contract format

**Priority:** Must

The board design MUST include a machine-checkable Power + I/O contract
expressed as a YAML file with a stable, versioned schema.

**Acceptance criteria:**

1. Each hardware configuration has a generated YAML contract file
   at `hw/output/<config>/contract.yaml`, validated against a stable
   JSON Schema at `hw/contract-schema.json`.
2. The contract includes normative fields (numbers, enums, pin names,
   states, thresholds) — not prose-only guidance.
3. Unknown or missing required fields are rejected by schema validation.
4. The contract version is included as a top-level field.

---

### HW-1101  Power contract (rails + states + budgets)

**Priority:** Must

The contract MUST define every power rail, operating state, and current
budget so that power integrity can be verified automatically.

**Acceptance criteria:**

1. Every power rail is defined with: name, type (source/regulated/switched),
   voltage operating window (min/typ/max), source description, and load
   envelope (peak/average current per operating state).
2. Operating states are enumerated: at minimum `shipping`, `deep_sleep`,
   `active`, and `radio_tx_burst`.
3. Each rail specifies a current budget per operating state (max mA).
4. Sequencing constraints are defined: which rails must be stable before
   others are enabled (e.g., "3V3 must settle before sensor power gate").
5. Brownout behavior is specified: below what VBAT threshold the firmware
   must enter a defined safe state.

---

### HW-1102  I/O contract (pins + electrical behavior + ownership)

**Priority:** Must

The contract MUST define every externally meaningful pin with its
electrical role, limits, and per-state behavior.

**Acceptance criteria:**

1. Every pin entry includes: MCU pin name, board net name, connector pin
   (if applicable), electrical role (input/output/open-drain/analog/strap/
   interrupt/wake), and peripheral binding (I2C0_SCL, SPI_MISO, etc.).
2. Electrical limits are specified per pin: voltage domain, tolerance,
   required pull-ups/pull-downs (value + rail), max sink/source current.
3. Default and reset states are specified per pin: state at reset
   (Hi-Z/pulled/pushed) and state in each operating mode.
4. Firmware ownership rules are specified where relevant (e.g., "must not
   drive high while sensor rail is off", "input-only during shipping mode").

---

### HW-1103  Contract invariant checks

**Priority:** Must

The generation tool MUST validate the following invariants against the
contract, schematic, and firmware pin configuration.

**Acceptance criteria:**

**Power checks:**

1. State budgets satisfied: sum of known always-on loads + pull resistor
   leakage ≤ rail budget for each operating state.
2. Peak current envelope: worst-case state (radio TX burst) must not
   exceed source capability.
3. Sequencing: any pin that can source current into a rail must be Hi-Z
   when that rail is off.
4. Brownout safety: below VBAT_min, firmware must enter the defined safe
   state.

**I/O checks:**

5. Voltage domain compatibility: no pin connected to a net that exceeds
   the pin's tolerance in any operating state.
6. Backpower prevention: no path from a driven-high I/O into an
   unpowered peripheral rail via protection diodes.
7. Bootstrap pin safety: MCU strap pins are not overridden by external
   pull networks or peripherals at reset.
8. Bus integrity: I2C pull-ups exist, connect to the correct rail, and
   are within the acceptable range for the bus speed.
9. Sleep leakage accounting: pull-ups, pull-downs, and voltage dividers
   do not violate the sleep-state current budget.

**Firmware binding checks:**

10. Pin mux matches contract: firmware NVS pin config sets the correct
    mode (open-drain for I2C, push-pull for power gate, etc.).
11. Power-state pin behavior: firmware drives or tri-states pins as
    specified per operating state.
12. Forbidden combinations: e.g., "radio TX when VBAT below threshold"
    or "sensor enabled in shipping mode".

---

### HW-1104  Contract validation in CI

**Priority:** Should

**Acceptance criteria:**

1. The CI pipeline validates the contract schema on every push.
2. When both a contract and a generated schematic exist, invariant
   checks (HW-1103) run automatically and fail on violations.
3. Invariant check failures produce precise error messages (e.g.,
   "GPIO4 pull-up to 3V3 leaks 0.7 mA in deep_sleep, exceeding rail
   budget of 0.05 mA").

---

## Appendix A  Requirement index

| ID | Title | Priority |
|---|---|---|
| HW-0100 | Microcontroller | Must |
| HW-0101 | USB-C connector | Must |
| HW-0102 | Voltage regulator | Must |
| HW-0103 | Battery input | Should |
| HW-0200 | I2C bus (Qwiic/STEMMA QT) | Must |
| HW-0201 | SPI bus | Should |
| HW-0202 | 1-Wire bus | Should |
| HW-0203 | GPIO breakout | Must |
| HW-0204 | ADC input | Should |
| HW-0300 | Sensor footprint slots | Should |
| HW-0301 | Supported sensor footprints | Should |
| HW-0400 | Deep sleep current | Must |
| HW-0401 | Sensor power gating | Should |
| HW-0500 | Board dimensions | Should |
| HW-0501 | Manufacturing constraints | Must |
| HW-0502 | Antenna keepout | Must |
| HW-0600 | Parameterized design tool | Must |
| HW-0601 | Default configuration | Must |
| HW-0700 | Board configuration schema | Must |
| HW-0701 | Configuration examples | Must |
| HW-0800 | Schematic output | Must |
| HW-0801 | PCB layout output | Must |
| HW-0802 | Manufacturing outputs | Must |
| HW-0900 | Reproducible generation | Must |
| HW-0901 | KiCad CLI integration | Must |
| HW-0902 | CI integration | Should |
| HW-1000 | Electrical rule check (ERC) | Must |
| HW-1001 | Design rule check (DRC) | Must |
| HW-1002 | Netlist verification | Must |
| HW-1003 | Simulation and emulation | Should |
| HW-1100 | Contract format | Must |
| HW-1101 | Power contract (rails + states + budgets) | Must |
| HW-1102 | I/O contract (pins + electrical behavior) | Must |
| HW-1103 | Contract invariant checks | Must |
| HW-1104 | Contract validation in CI | Should |
