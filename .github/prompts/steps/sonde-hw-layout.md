# Phases 6–7: PCB Layout & Audit — Methodology

This file contains the pcb-layout-design and layout-design-review
protocols for Phases 6–7 of the Sonde hardware design workflow. The
agent should read this file when entering Phase 6.

---

## Protocol: PCB Layout Design

Apply this protocol when designing a PCB layout from a completed
schematic. The goal is to produce a routed, DRC-clean PCB design file
by generating a Python script that uses KiCad's `pcbnew` API for board
setup and component placement, FreeRouting for automated trace routing,
and `kicad-cli` for design rule validation. Execute all phases in order.

**Tool dependencies**:
- **KiCad** 7.0+ with `pcbnew` Python API
- **FreeRouting** (`freerouting.jar`) for automated routing
- **Java runtime** (for FreeRouting)
- **kicad-cli** (included with KiCad 7.0+) for DRC

If any tool is unavailable, the protocol can still produce the layout
spec and placement script, but routing and DRC phases cannot execute.

### Phase 1: Input Validation

1. **Schematic completeness**: All components have footprints, all nets
   named, ERC passes, netlist exportable.

2. **Component footprint inventory**: For each component — KiCad
   footprint, physical dimensions, mounting type (SMD/TH/module),
   special placement requirements.

3. **Design constraints from upstream**: Power dissipation, high-speed
   signals, RF clearance zones, current-carrying traces.

4. **Target fab service** and design rule minimums:

   | Parameter | JLCPCB (standard) | PCBWay (standard) |
   |-----------|-------------------|-------------------|
   | Min trace width | 0.127mm (5mil) | 0.1mm (4mil) |
   | Min spacing | 0.127mm (5mil) | 0.1mm (4mil) |
   | Min via drill | 0.3mm | 0.3mm |
   | Min annular ring | 0.13mm | 0.1mm |

   Confirm against fab's current specs before use.

### Phase 2: Layout Requirements Gathering (Interactive)

Do NOT proceed to board definition until the user confirms.

1. **Board form factor**: Target dimensions, shape, mounting method,
   mounting hole locations.

2. **Connector placement**: For each connector — which board edge,
   position along edge, orientation.

3. **Component placement preferences**: MCU socket position, battery
   connector, antenna keepout, LED positions, programming header
   accessibility, top vs. bottom side requirements.

4. **Mechanical constraints**: Enclosure fit, height clearance,
   keep-out zones, cable routing.

5. **Produce a layout requirements summary** for user confirmation.

### Phase 3: Board Definition

1. **Board outline**: Closed polygon on `Edge.Cuts`. Rectangular with
   optional corner radii.

2. **Layer stackup**:

   **2-layer**:
   | Layer | Purpose |
   |-------|---------|
   | F.Cu | Components + signal routing |
   | B.Cu | Ground plane + routing overflow |

   **4-layer**:
   | Layer | Purpose |
   |-------|---------|
   | F.Cu | Components + signal routing |
   | In1.Cu | Ground plane (continuous) |
   | In2.Cu | Power plane |
   | B.Cu | Signal routing + components |

3. **Mounting holes**: Place at user-specified locations using
   `MountingHole:MountingHole_M2.5` or equivalent.

4. **Board zones**: Ground pour on back/inner layer. Antenna keepout
   zones (no copper).

### Phase 4: Design Rule Configuration

1. **Default design rules**: Conservative defaults per fab house.
   Default trace 0.25mm, clearance 0.2mm, via 0.8/0.4mm.

2. **Net classes**:

   | Net Class | Trace Width | Clearance | Via Size | Applies To |
   |-----------|-------------|-----------|----------|------------|
   | Default | 0.25mm | 0.2mm | 0.8/0.4mm | All signals |
   | Power | 0.5mm+ | 0.3mm | 1.0/0.5mm | VBAT, 3V3, 3V3_SW |
   | High-Speed | per impedance | 0.2mm | 0.6/0.3mm | USB D+/D- |

   Power trace width = current_A / (0.048 × temp_rise_C^0.44)
   for 1oz copper outer layer. Present the calculation.

3. **Impedance-controlled traces**: Calculate trace width/spacing for
   target impedance using stackup geometry.

4. **Thermal relief**: Configure spoke width and gap for zone connections.

### Phase 5: Component Placement Strategy

1. **Placement priority order**:
   - **First**: Fixed-position (connectors, mounting holes, antenna)
   - **Second**: DIP socket for MCU module (central, with clear access)
   - **Third**: Power section (regulator, power gate MOSFET, bulk caps —
     grouped, input near power connector)
   - **Fourth**: High-speed / sensitive (crystal near MCU, USB near
     USB connector)
   - **Fifth**: Peripheral ICs (sensors near MCU I2C/SPI pins)
   - **Sixth**: Decoupling caps (within 3mm of each IC power pin)
   - **Seventh**: Remaining passives (near their IC)

2. **Placement rules**:
   - 1.27mm placement grid
   - ≥ 1mm courtyard clearance
   - Decoupling caps within 3mm of IC power pin
   - Crystal within 5mm of MCU oscillator pins
   - No components in antenna keepout
   - Top side preferred (bottom costs more for assembly)

3. **Thermal placement**: Regulators with thermal vias. Heat sources
   away from temperature sensors. Exposed pads with ≥ 4 thermal vias.

4. **Group placement verification**: Signal flow logical, power flows
   source to loads, high-speed paths short, connectors accessible.

### Phase 6: Routing Strategy

1. **Pre-route critical signals**:
   - USB differential pairs: tightly coupled, length-matched
   - Crystal traces: short, guarded with ground
   - Analog signals: away from switching noise
   - Power traces: wide, from regulator to loads

2. **Autorouter configuration**: Power nets first, high-speed second,
   remaining last. Minimize vias. Prefer fewer layers.

3. **Ground plane strategy**:
   - 2-layer: ground pour on back copper, vias near every IC ground
     and decoupling cap ground
   - 4-layer: dedicate In1.Cu as continuous ground — no signal routing
   - Avoid slots/gaps under signal traces
   - Ground vias near every signal via

4. **Power distribution**:
   - 2-layer: wide traces or short copper pours
   - 4-layer: In2.Cu as power plane

5. **DFM rules**: 45° bends (no sharp corners), avoid routing under
   QFN pads, ≥ 0.5mm from board edges for traces, ≥ 1mm for components.

### Phase 7: Python Script Generation

Generate a Python script using `pcbnew` API.

```python
#!/usr/bin/env python3
"""PCB layout script generated by PromptKit.

Prerequisites:
  - KiCad 7.0+ with pcbnew Python API
  - .kicad_pcb with footprints/nets from schematic

Usage:
  python3 layout.py path/to/board.kicad_pcb
"""
import pcbnew
import sys

board = pcbnew.LoadBoard(sys.argv[1])
# ... board setup, placement, zone definitions, DSN export
board.Save(sys.argv[1])
```

The script must implement:
- Board outline on Edge.Cuts
- Layer stackup configuration
- Design rules and net classes
- Component placement from a `PLACEMENT` dictionary
- Mounting holes
- Copper zone definitions
- Specctra DSN export for FreeRouting
- Save board

Configuration at top of script (board dimensions, positions, rules).
Error handling for missing files, missing footprints, export failures.

### Phase 8: Autorouting Execution

1. **FreeRouting**:
   ```bash
   java -jar freerouting.jar -de board.dsn -do board.ses -mp 20
   ```

2. **Import result**: `pcbnew.ImportSpecctraSES(board, "board.ses")`

3. **Post-routing**: Fill zones, remove islands, save.

4. **Completeness check**: All nets routed? If not, report unrouted
   nets — may need placement adjustment.

### Phase 9: DRC Validation Loop

1. **Run DRC**:
   ```bash
   kicad-cli pcb drc -o drc-report.json --format json \
     --severity-all board.kicad_pcb
   ```

2. **Classify violations**: Clearance, unconnected net, track width,
   via, courtyard overlap, edge clearance, zone fill.

3. **Fix strategy**:
   - Clearance/width: adjust design rules, re-route (Phase 8)
   - Courtyard overlap: adjust placement, re-run from Phase 7
   - Unconnected: adjust placement for routing channels
   - Max 5 iterations — then report and request user intervention

4. **DRC clean**: Zero violations. Warnings documented and justified.

5. **DRC summary table**: Iteration, violations, warnings, action taken.

### Phase 10: Self-Audit Checklist

1. DRC clean, warnings documented.
2. Power trace widths verified (IPC-2221).
3. USB diff pairs length-matched, reference planes continuous.
4. Decoupling caps within 3mm, antenna keepout respected, connectors
   at edges, thermal vias under exposed pads.
5. Ground plane continuous, power distribution adequate.
6. All features meet fab minimums, silkscreen clear, fiducials if needed.

Fix Critical/High before presenting. Present Medium/Low as notes.

Present board dimensions, stackup, placement, routing, DRC summary,
and Python script. User MUST approve before manufacturing.

---

## Protocol: Layout Design Review

Apply when reviewing the PCB layout against schematic intent and
requirements. Execute all phases in order.

### Phase 1: DRC Report Review

1. **Classify** violations vs. warnings.
2. **Assess** each violation: real error or rule misconfiguration?
3. **DRC rule coverage**: Rules appropriate for target fab house?

### Phase 2: Trace Width and Current Capacity

1. **Power traces**: Calculate required width (IPC-2221) for worst-case
   current. Compare to actual. Flag undersized.
2. **Signal traces**: Meet fab minimum width.
3. **Ground connections**: Adequate for return current paths.

### Phase 3: Impedance and Signal Integrity

1. **USB differential pairs**: Tightly coupled, length-matched, ~90Ω
   differential, continuous reference plane.
2. **Other controlled-impedance**: Stackup and geometry support target.
3. **Return path continuity**: Via placement provides return current
   path. Flag gaps in ground plane under signals.

### Phase 4: Component Placement Review

1. **Decoupling caps**: Within 3mm of power pin, short ground via path.
2. **Antenna keepout**: No copper in keepout zone.
3. **Connectors**: Accessible from edges, correct orientation.
4. **Thermal**: Thermal vias under exposed pads, heat sources away from
   sensitive components.

### Phase 5: Ground Plane and Power Integrity

1. **Ground plane continuity**: Flag slots, cutouts, narrow necks.
2. **Power pour integrity**: Low-impedance paths, adequate via connections.
3. **Ground domains**: Separate analog/digital joined at single point
   (if applicable).

### Phase 6: Manufacturing Constraint Compliance

1. **Minimum features**: Trace width, spacing, via, solder mask dam,
   silkscreen line width — all meet fab minimums.
2. **Board outline**: Dimensions, corner radii within fab capability.
3. **Panelization**: Edge clearance for V-scoring/tab routing.
4. **Assembly**: Consistent orientation, spacing for pick-and-place,
   fiducials if required.

### Phase 7: Findings Summary

1. **Each finding**: Phase, affected area, severity, remediation.
2. **Coverage summary**: DRC items, power traces, impedance signals,
   manufacturing constraints — all checked.
