<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Hardware — SPICE Simulation Pipeline Specification

> **Document status:** Draft
> **Source:** Issue #583
> **Relates to:** HW-1003, `prompts/hardware/02-validate-simulation.md`

---

## 1  Overview

This document specifies an automated SPICE simulation pipeline for the
sonde-hw tool. The pipeline generates SPICE decks from the internal net
graph produced during schematic generation, runs them through ngspice,
and evaluates assertions against the results.

The goal is to catch electrical design errors (rail voltage violations,
excessive sleep current, insufficient dropout margin) before PCB
fabrication — turning `prompts/hardware/02-validate-simulation.md` from
a manual review step into an automated CI check.

```
sonde-hw build configs/minimal-qwiic.yaml
   │
   ├── .kicad_sch          (schematic file)
   ├── bom.csv             (bill of materials)
   └── internal net graph  (Python data structure)
          │
          ▼
   sonde-hw simulate configs/minimal-qwiic.yaml
          │
          ├── Component → SPICE model mapping
          ├── SPICE deck generation (.cir files)
          ├── ngspice execution (subprocess)
          └── Assertion evaluation (pass/fail)
```

---

## 2  Requirements

### HW-SIM-001  Netlist export from build pipeline

**Priority:** Must

The `sonde-hw build` command MUST produce an internal netlist data
structure (in addition to `.kicad_sch` and BOM) that captures:

1. Every component instance with reference designator, value, and
   component type (resistor, capacitor, MOSFET, LDO, diode, connector).
2. Every net with a name and a list of connected pins (component ref +
   pin number/name).
3. The netlist MUST be serializable to JSON for debugging and
   cross-tool consumption.

**Acceptance criteria:**

1. After `sonde-hw build`, a `netlist.json` file is written to the
   output directory alongside the schematic and BOM.
2. The JSON contains `components` (array of objects) and `nets` (array
   of objects with connected pins).
3. Every component in the schematic appears in the netlist.
4. Every labeled net (`3V3`, `GND`, `I2C0_SDA`, etc.) appears by name.

---

### HW-SIM-002  SPICE model library

**Priority:** Must

The tool MUST include a SPICE model library that maps schematic
components to simulation models.

**Acceptance criteria:**

1. Each component type has a model definition in
   `hw/sonde_hw/spice/models/`.
2. The following models are included:

   | Component | Model file | Model type |
   |-----------|-----------|------------|
   | MCP1700 LDO | `mcp1700.sub` | Behavioral subcircuit (Vin, Vout, GND; Iq, dropout, current limit) |
   | Si2301 P-FET | `si2301.mod` | Level 1 MOSFET (Vth=-1.2V, Rds_on=115mΩ) |
   | Schottky diode | `schottky.mod` | Standard diode (Vf=0.3V, Is=1µA) |
   | ESP32-C3 | `esp32c3.sub` | Current source per operating state (5µA sleep, 80mA active, 150mA TX) |
   | USBLC6-2SC6 | `usblc6.sub` | Leakage model (0.15µA per line) |
   | Passives | Built-in | Ideal R, C (ngspice native) |

3. Each model file includes a header comment with the datasheet source
   and key parameters used.
4. Models are parameterized where practical (e.g., LDO dropout voltage
   as a parameter) for reuse with alternative components.

---

### HW-SIM-003  SPICE deck generation

**Priority:** Must

The tool MUST generate ngspice-compatible SPICE decks (`.cir` files)
from the netlist and model library.

**Acceptance criteria:**

1. Each simulation test case produces a self-contained `.cir` file in
   `hw/output/<config>/spice/`.
2. The deck includes:
   - `.include` directives for required model files
   - Component instantiation from the netlist
   - Source definitions (VBAT, VUSB) appropriate for the test
   - Analysis commands (`.op`, `.dc`, `.tran`)
   - `.measure` statements for assertion values
3. Net names in the SPICE deck match the schematic net names.
4. The deck is human-readable and can be run standalone with
   `ngspice -b <file>.cir`.

---

### HW-SIM-004  Simulation test suite

**Priority:** Must

The tool MUST include a predefined set of simulation tests that
validate the board design against the hardware requirements.

**Acceptance criteria:**

1. The following tests are implemented:

   | Test ID | Name | Analysis | Assertion |
   |---------|------|----------|-----------|
   | `dc-operating-point` | DC operating point | `.op` | `V(3V3)` = 3.3V ± 5% when VBAT=3.7V |
   | `sleep-current` | Deep sleep current | `.op` | `I(VBAT)` ≤ 20µA (HW-0400) |
   | `dropout-margin` | LDO dropout sweep | `.dc VBAT 2.5 4.2 0.01` | `V(3V3)` ≥ 3.135V when VBAT ≥ 3.35V |
   | `power-on-transient` | Power-on settling | `.tran 1u 10m` | `V(3V3)` settles to 3.3V ± 5% within 5ms, no overshoot > 3.6V |
   | `power-gate-on` | Sensor rail enable | `.tran 1u 5m` | `V(SENSOR_3V3)` reaches 3.3V ± 5% within 1ms after GPIO enable |
   | `battery-divider` | ADC voltage check | `.op` | `V(VBAT_SENSE)` = VBAT × R12/(R11+R12) ± 1% |

2. Each test has a YAML definition in `hw/sonde_hw/spice/tests/` with:
   - Test ID and human-readable name
   - Required sources and their values
   - Operating state (which components are active)
   - Analysis type and parameters
   - Assertions (net name, operator, threshold)
3. Custom tests can be added by creating new YAML files.

---

### HW-SIM-005  ngspice runner

**Priority:** Must

The tool MUST execute ngspice in batch mode and parse the results.

**Acceptance criteria:**

1. ngspice is invoked via `subprocess` in batch mode (`ngspice -b`).
2. If ngspice is not installed, the command exits with a clear error
   message and guidance ("install ngspice: apt install ngspice /
   brew install ngspice / choco install ngspice").
3. Simulation output is parsed from ngspice stdout/rawfile.
4. `.measure` results are extracted and compared against assertions.
5. Each test produces a pass/fail result with the measured value and
   the assertion threshold.
6. Simulation timeout: 30 seconds per test (configurable). Tests that
   exceed the timeout are reported as failures.

---

### HW-SIM-006  CLI integration

**Priority:** Must

**Acceptance criteria:**

1. `sonde-hw simulate <config.yaml>` runs all simulation tests and
   reports results.
2. `sonde-hw simulate <config.yaml> --test <test-id>` runs a single
   test.
3. `sonde-hw simulate <config.yaml> --list` lists available tests.
4. Exit code is 0 if all tests pass, non-zero if any fail.
5. Output format: one line per test with pass/fail, measured value,
   and threshold.
6. `--verbose` flag shows full ngspice output for debugging.
7. The `simulate` command requires a prior `build` (netlist.json must
   exist in the output directory).

---

### HW-SIM-007  CI integration

**Priority:** Should

**Acceptance criteria:**

1. The GitHub Actions workflow for `hw/` runs `sonde-hw simulate` after
   `sonde-hw build` on all example configurations.
2. Simulation failures fail the CI pipeline.
3. ngspice is installed in the CI environment via `apt install ngspice`.

---

## 3  Architecture

### 3.1  Module structure

```
hw/sonde_hw/
├── spice/
│   ├── __init__.py
│   ├── netlist.py        # Net graph → SPICE netlist conversion
│   ├── deck.py           # SPICE deck generator (.cir files)
│   ├── runner.py         # ngspice subprocess runner + output parser
│   ├── assertions.py     # Assertion evaluator (measured vs threshold)
│   ├── models/           # SPICE model files (.sub, .mod)
│   │   ├── mcp1700.sub
│   │   ├── si2301.mod
│   │   ├── schottky.mod
│   │   ├── esp32c3.sub
│   │   └── usblc6.sub
│   └── tests/            # Test definitions (YAML)
│       ├── dc-operating-point.yaml
│       ├── sleep-current.yaml
│       ├── dropout-margin.yaml
│       ├── power-on-transient.yaml
│       ├── power-gate-on.yaml
│       └── battery-divider.yaml
```

### 3.2  Data flow

```
config.yaml
    │
    ▼
sonde-hw build
    │
    ├── board.kicad_sch
    ├── bom.csv
    └── netlist.json  ◄── NEW: net graph export
            │
            ▼
sonde-hw simulate
    │
    ├── Load netlist.json
    ├── For each test YAML:
    │     ├── Map components → SPICE models
    │     ├── Generate .cir deck
    │     ├── Run ngspice -b
    │     ├── Parse .measure results
    │     └── Evaluate assertions
    └── Report pass/fail summary
```

### 3.3  Netlist JSON format

```json
{
  "config": "minimal-qwiic",
  "components": [
    {
      "ref": "U2",
      "type": "ldo",
      "value": "MCP1700-3302E",
      "model": "mcp1700",
      "pins": {
        "1": { "name": "VIN", "net": "VBAT_REG" },
        "2": { "name": "GND", "net": "GND" },
        "3": { "name": "VOUT", "net": "3V3" }
      }
    }
  ],
  "nets": [
    {
      "name": "3V3",
      "pins": ["U2:3", "U1:VDD", "C3:1", "R7:1", "R8:1"]
    },
    {
      "name": "GND",
      "pins": ["U2:2", "U1:GND", "C1:2", "C3:2"]
    }
  ]
}
```

### 3.4  Test definition format

```yaml
# hw/sonde_hw/spice/tests/sleep-current.yaml
id: sleep-current
name: Deep sleep current budget
description: >
  Verify total board current in deep sleep does not exceed 20 µA (HW-0400).

sources:
  VBAT: 3.7    # Nominal LiPo voltage
  VUSB: 0      # USB disconnected

state: deep_sleep   # ESP32-C3 in deep sleep, sensors off

analysis:
  type: op       # DC operating point

assertions:
  - net: VBAT
    measure: current
    operator: "<="
    threshold: 20e-6
    unit: A
    description: "Total sleep current must not exceed 20 µA"

  - net: 3V3
    measure: voltage
    operator: ">="
    threshold: 3.135
    unit: V
    description: "3V3 rail must be within spec during sleep"
```

### 3.5  SPICE model approach

Models are behavioral approximations, not transistor-level. The goal is
design validation, not silicon-accurate simulation.

**MCP1700 LDO** (`mcp1700.sub`):
```spice
* MCP1700-3302E behavioral LDO model
* Iq=1.6µA typ, Vdropout=178mV typ @ 150mA, Iout_max=250mA
.subckt mcp1700 vin gnd vout
  * Quiescent current
  Iq vin gnd 1.6u
  * Output regulation (behavioral voltage source)
  Breg vout gnd V = {
+   if(V(vin,gnd) > 3.3 + 0.178, 3.3,
+   if(V(vin,gnd) > 0.5, V(vin,gnd) - 0.178, 0))
+ }
  * Output impedance
  Rout vout_int vout 0.5
.ends mcp1700
```

**Si2301 P-FET** — use ngspice Level 1 MOSFET with datasheet Vth and
Rds(on).

**ESP32-C3** — modeled as a switchable current source:
- Deep sleep: 5µA from 3V3 to GND
- Active: 80mA
- Radio TX: 150mA

---

## 4  Tradeoff analysis

### ngspice vs PySpice vs Xyce

| | ngspice (subprocess) | PySpice | Xyce |
|---|---|---|---|
| Install | apt/brew/choco | pip + ngspice | Manual build |
| API | Batch mode, parse stdout | Python objects | Batch mode |
| Capability | Sufficient for analog | Same engine | Parallel, mixed-signal |
| Complexity | Low | Medium | High |
| **Decision** | **Selected** | Rejected (extra dep) | Rejected (install burden) |

### Internal netlist vs kicad-cli export

| | Internal | kicad-cli |
|---|---|---|
| Speed | Instant (data already in memory) | Requires KiCad installed |
| Accuracy | Matches what we generated | Matches what KiCad sees |
| **Decision** | **Selected for MVP** | Add as validation cross-check later |

---

## 5  MVP scope

The first implementation covers:

1. ✅ Netlist JSON export from `sonde-hw build`
2. ✅ SPICE model files for MCP1700, Si2301, Schottky, ESP32-C3
3. ✅ SPICE deck generation for 3 tests: `dc-operating-point`,
   `sleep-current`, `battery-divider`
4. ✅ ngspice batch runner with assertion evaluation
5. ✅ `sonde-hw simulate` CLI command
6. ⬜ Transient tests (`power-on-transient`, `power-gate-on`) — deferred
7. ⬜ CI integration — deferred
8. ⬜ kicad-cli netlist cross-check — deferred

---

## 6  Open questions

1. **ngspice output parsing**: ngspice can output binary rawfiles or
   ASCII. Which is easier to parse? Binary is more reliable but needs
   a parser. ASCII `.measure` output is simpler.
2. **Model accuracy**: Behavioral LDO models are approximations. Should
   we include a "model fidelity" warning in simulation results?
3. **Windows ngspice path**: ngspice on Windows may not be on PATH.
   Should we auto-detect from common install locations?

---

## 7  Revision history

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-30 | Copilot | Initial specification |
