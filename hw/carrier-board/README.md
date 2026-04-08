<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Carrier Board — Manufacturing Guide

## Overview

The sonde carrier board is a 2-layer PCB that hosts a Seeed Studio XIAO ESP32-C3
module and provides connectors for I2C (Qwiic), 1-Wire sensors, and a 2×AA lithium
battery pack. The board powers the MCU directly from the battery (no LDO) and
includes a P-FET load switch for gating sensor power during deep sleep.

**Carrier board deep sleep current: ~0.01 µA** (essentially zero).

## Prerequisites

- [KiCad 8](https://www.kicad.org/download/) (free, open-source EDA)
- A [JLCPCB](https://jlcpcb.com/) account (or compatible PCB fab)
- A Seeed Studio XIAO ESP32-C3 module (not included in this BOM)

## Directory Contents

| File | Description |
|------|-------------|
| `sonde-carrier.kicad_pro` | KiCad 8 project file |
| `sonde-carrier.kicad_sch` | KiCad 8 schematic (when created) |
| `sonde-carrier.kicad_pcb` | KiCad 8 PCB layout (when created) |
| `bom-jlcpcb.csv` | Bill of Materials in JLCPCB format |
| `cpl-jlcpcb.csv` | Component Placement List (coordinates TBD after layout) |
| `README.md` | This file |

## How to Order PCBs

### Step 1: Open the Project in KiCad

1. Open KiCad 8
2. File → Open Project → select `sonde-carrier.kicad_pro`
3. Review the schematic (`.kicad_sch`)
4. Open the PCB editor (`.kicad_pcb`)

### Step 2: Generate Gerber Files

1. In the PCB editor: File → Fabrication Outputs → Gerbers (.gbr)
2. Set output directory to `gerbers/`
3. Select layers: F.Cu, B.Cu, F.SilkS, B.SilkS, F.Mask, B.Mask, Edge.Cuts
4. Click "Plot"
5. Then: File → Fabrication Outputs → Drill Files (.drl)
6. Generate drill file to the same `gerbers/` directory

### Step 3: Upload to JLCPCB

1. Go to [jlcpcb.com](https://jlcpcb.com/) and click "Order Now"
2. Upload the Gerber ZIP file
3. Set PCB parameters:
   - Layers: 2
   - PCB Thickness: 1.6 mm
   - Surface Finish: HASL (lead-free)
   - Copper Weight: 1 oz
4. Enable "SMT Assembly" if you want JLCPCB to solder SMD parts
5. Upload `bom-jlcpcb.csv` as the BOM
6. Upload `cpl-jlcpcb.csv` as the CPL (coordinates filled in after layout)

### Step 4: Hand-Solder Through-Hole Parts

JLCPCB SMT assembly handles the SMD components (resistors, caps, MOSFET,
JST SH connectors). The following through-hole parts need hand soldering:

- **J3, J4** — JST XH 3-pin (1-Wire connectors)
- **J5** — JST PH 2-pin (battery connector)
- **J6, J7** — 1×7 female pin headers (XIAO socket)

Use a soldering iron at 350°C with lead-free solder.

### Step 5: Insert the XIAO Module

1. Solder pin headers onto the XIAO ESP32-C3 (if not pre-soldered)
2. Insert the XIAO into J6/J7 with the **USB-C port facing outward**
   and the **antenna toward the top edge** of the carrier board
3. The XIAO should plug in snugly — no soldering needed on the carrier side

## Schematic Summary

```
VBAT (2×AA lithium, JST PH) ──┬──[C3 1µF]── GND
                               │
                               ├──► XIAO 3V3 pin (direct, no regulator)
                               │
                               ├──[R4 100kΩ]──┬── Q1 Gate (Si2301 P-FET)
                               │              │
                               │         GPIO4 (SENSOR_EN)
                               │
                               └──► Q1 Source
                                    Q1 Drain ──► SENSOR_V rail
                                                    │
                                    ┌───────────────┼────────────┐
                                    │               │            │
                                 [R1,R2 4.7kΩ]  [R3 4.7kΩ]  [R5,R6 100kΩ]
                                    │               │            │
                                 SDA,SCL          1-Wire DQ    VBAT_SENSE
                                 (GPIO6,7)        (GPIO3)      (GPIO2 ADC)
                                    │               │
                                 J1,J2 Qwiic     J3,J4 JST XH
```

## BOM Cost (qty 100)

| Item | Cost |
|------|------|
| PCB fabrication | ~$0.25/board |
| SMD components | ~$0.86/board |
| JLCPCB assembly | ~$0.80/board |
| **Total (carrier board only)** | **~$1.91/board** |
| XIAO ESP32-C3 (user-supplied) | ~$5.00 |

## Design Documents

- [Requirements Specification](../../docs/carrier-board-requirements.md)
- [Schematic Design](../../docs/carrier-board-design.md)
