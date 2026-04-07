# Phases 8–9: Manufacturing Artifacts — Methodology

This file contains the manufacturing-artifact-generation protocol for
Phases 8–9 of the Sonde hardware design workflow. The agent should
read this file when entering Phase 8.

---

## Protocol: Manufacturing Artifact Generation

Apply this protocol when generating manufacturing deliverables from
a completed, DRC-clean KiCad PCB design. The goal is to produce all
files needed to submit a board for fabrication and assembly at a
turnkey service (JLCPCB, PCBWay, etc.). Execute all phases in order.

**Tool dependencies**:
- **KiCad 7.0+** with `kicad-cli`
- **Python 3.8+** for the generation script

If `kicad-cli` is unavailable, document the required artifacts and
their specifications but note automated generation cannot proceed.

### Phase 1: Input Validation

1. **DRC status**: Confirm zero violations. If violations exist, return
   to the layout protocol's DRC validation loop.

2. **Board file completeness**: Verify `.kicad_pcb` has board outline
   on Edge.Cuts, all components placed with footprints, all nets routed,
   zones filled, silkscreen labels present.

3. **Target fab service**: Confirm with user:
   - Fab service (JLCPCB, PCBWay, OSH Park)
   - Assembly service (same fab, different, or hand-assembly)
   - Order quantity
   - Board parameters: layers, copper weight, surface finish (HASL,
     ENIG, OSP), thickness, solder mask color

4. **BOM data completeness**: Every component needs MPN, supplier PN
   (e.g., LCSC number for JLCPCB), value, footprint, designator.
   Flag missing supplier PNs for user input.

### Phase 2: Gerber Generation

1. **Layer mapping** (2-layer board):

   | Layer | KiCad Layer | Gerber Suffix | Purpose |
   |-------|-------------|---------------|---------|
   | Front copper | F.Cu | .GTL | Top traces |
   | Back copper | B.Cu | .GBL | Bottom traces |
   | Front mask | F.Mask | .GTS | Top mask openings |
   | Back mask | B.Mask | .GBS | Bottom mask openings |
   | Front silk | F.SilkS | .GTO | Top labels |
   | Back silk | B.SilkS | .GBO | Bottom labels |
   | Board outline | Edge.Cuts | .GKO | Perimeter |
   | Front paste | F.Paste | .GTP | Stencil (assembly) |
   | Back paste | B.Paste | .GBP | Stencil (if bottom SMD) |

   For 4-layer, add In1.Cu (.G2) and In2.Cu (.G3).

2. **Settings**: RS-274X (Gerber X2 preferred), 4.6 coordinate format,
   millimeters, Protel extensions.

3. **Command**:
   ```bash
   kicad-cli pcb export gerbers \
     --output manufacturing/gerbers/ \
     --layers F.Cu,B.Cu,F.Mask,B.Mask,F.SilkS,B.SilkS,Edge.Cuts,F.Paste,B.Paste \
     --use-protel-extensions \
     board.kicad_pcb
   ```

### Phase 3: Drill File Generation

1. **Types**: PTH (vias, through-hole pins) and NPTH (mounting holes)
   as separate files.

2. **Settings**: Excellon format, millimeters, absolute coordinates,
   no zero suppression.

3. **Command**:
   ```bash
   kicad-cli pcb export drill \
     --output manufacturing/gerbers/ \
     --format excellon \
     --drill-origin absolute \
     --excellon-units mm \
     --generate-map --map-format gerberx2 \
     board.kicad_pcb
   ```

### Phase 4: BOM Generation

1. **Extract BOM**:
   ```bash
   kicad-cli sch export bom \
     --output manufacturing/assembly/bom-raw.csv \
     --fields "Reference,Value,Footprint,MPN,Manufacturer,LCSC,DNP" \
     --group-by Value,Footprint \
     board.kicad_sch
   ```
   Derive schematic path from PCB path (same directory, same basename).

2. **Required fields**: Reference, Value, Footprint, Quantity, MPN,
   Manufacturer, Supplier PN, DNP flag.

3. **Fab-specific formatting**:

   **JLCPCB**: Columns: `Comment, Designator, Footprint, LCSC`.
   Comment = value. CSV, UTF-8.

   **PCBWay**: Columns: `Item, Qty, Designator, Package/Case,
   Manufacturer, MPN, Supplier, Supplier PN`. CSV or Excel.

4. **DNP handling**: DNP components appear with flag — never silently omit.

5. **BOM verification**: Unique line items match schematic, total
   quantity correct, no missing MPNs (flag for user).

### Phase 5: Pick-and-Place Generation

1. **Command**:
   ```bash
   kicad-cli pcb export pos \
     --output manufacturing/assembly/ \
     --format csv --units mm \
     --side both --use-drill-file-origin \
     board.kicad_pcb
   ```

2. **Fields**: Ref, Val, Package, PosX, PosY, Rot, Side.

3. **Rotation offsets**: JLCPCB requires corrections for many packages.
   Common: SOT-23 (+180°), QFP (+90°), some SOIC (+90°). User should
   verify in JLCPCB assembly preview.

4. **Origin consistency**: Same origin across Gerber, drill, and
   pick-and-place files.

5. **Verification**: Count matches BOM (minus TH and DNP), no
   duplicate designators.

### Phase 6: Assembly Drawing Generation

1. **Top/bottom assembly views**:
   ```bash
   kicad-cli pcb export pdf \
     --output manufacturing/assembly/assembly-top.pdf \
     --layers F.Fab,F.SilkS,Edge.Cuts \
     board.kicad_pcb

   kicad-cli pcb export pdf \
     --output manufacturing/assembly/assembly-bottom.pdf \
     --layers B.Fab,B.SilkS,Edge.Cuts --mirror \
     board.kicad_pcb
   ```

2. **3D render** (optional, KiCad 8.0+):
   ```bash
   kicad-cli pcb render \
     --output manufacturing/assembly/board-3d.png \
     --width 1920 --height 1080 \
     board.kicad_pcb
   ```

### Phase 7: Fab-Specific Packaging

1. **Output directory**:
   ```
   manufacturing/
   ├── gerbers/          (all Gerber + drill files)
   ├── assembly/
   │   ├── bom.csv
   │   ├── pick-and-place.csv
   │   ├── assembly-top.pdf
   │   └── assembly-bottom.pdf
   └── README.md         (submission checklist)
   ```

2. **Gerber ZIP**: `cd manufacturing/gerbers && zip ../gerbers.zip *`

3. **Submission checklist** (README.md):
   ```markdown
   # Manufacturing Submission Checklist

   ## Board Specifications
   - Layers: [2 / 4]
   - Dimensions: [W]mm × [H]mm
   - Thickness: [1.6mm]
   - Copper weight: [1oz]
   - Surface finish: [HASL / ENIG / OSP]
   - Solder mask color: [green]
   - Silkscreen color: [white]

   ## Files Included
   - [ ] Gerbers (all layers) — gerbers.zip
   - [ ] Drill files (PTH + NPTH) — in gerbers.zip
   - [ ] BOM — assembly/bom.csv
   - [ ] Pick-and-place — assembly/pick-and-place.csv
   - [ ] Assembly drawings — assembly/*.pdf

   ## Pre-Submission Checks
   - [ ] Gerber viewer inspection
   - [ ] BOM supplier part numbers verified
   - [ ] Pick-and-place rotation verified in fab preview
   - [ ] Board dimensions confirmed
   - [ ] Layer count confirmed
   - [ ] Order quantity: [N]
   ```

### Phase 8: Pre-Submission Validation

1. **File presence**: All Gerbers, drill files, BOM, pick-and-place,
   assembly drawings present.

2. **Cross-artifact consistency**:
   - Gerber layer count matches stackup
   - Drill hole count matches PCB vias + TH pins
   - BOM component count matches schematic
   - Pick-and-place count matches BOM (minus TH and DNP)
   - Coordinate origin consistent

3. **Gerber inspection**: User MUST inspect in a viewer before
   submission (fab preview, gerbv, KiCad, Tracespace.io). Check:
   correct outline, no missing copper, mask aligns with pads,
   silkscreen readable.

4. **Known-issue checklist**:
   - Missing board outline (Edge.Cuts)
   - Solder mask openings too small
   - Silkscreen overlapping pads
   - Drill units mismatch (mm vs inches)
   - Pick-and-place origin mismatch

5. **Validation summary table**:

   | Check | Status | Details |
   |-------|--------|---------|
   | Gerber files present | ✅ | 9/9 layers |
   | Drill files present | ✅ | PTH + NPTH |
   | BOM complete | ⚠️ | 2 components missing LCSC PN |
   | Pick-and-place count | ✅ | 23 SMD components |
   | Coordinate origin | ✅ | Consistent |

### Phase 9: Python Script Generation

Generate a single Python script automating Phases 2–8.

```python
#!/usr/bin/env python3
"""Manufacturing artifact generation script.

Prerequisites: KiCad 7.0+ with kicad-cli, Python 3.8+

Usage: python3 generate_manufacturing.py board.kicad_pcb [fab_service]
Supported fab services: jlcpcb, pcbway (default: jlcpcb)
"""
import subprocess, sys, os, csv
from pathlib import Path

board_path = Path(sys.argv[1])
sch_path = board_path.with_suffix(".kicad_sch")
```

The script must implement:
- Argument parsing (board path, optional fab service)
- Input validation (files exist, kicad-cli available)
- Gerber, drill, BOM, pick-and-place, assembly drawing export
- Fab-specific BOM reformatting
- Output directory creation
- Gerber ZIP creation
- Cross-artifact validation
- Summary report to stdout

**Fab-specific configuration** at top of script:
```python
FAB_CONFIGS = {
    "jlcpcb": {
        "bom_columns": ["Comment", "Designator", "Footprint", "LCSC"],
        "supplier_field": "LCSC",
        "rotation_offsets": { ... },
    },
    "pcbway": {
        "bom_columns": ["Item", "Qty", "Designator", "Package",
                        "Manufacturer", "MPN", "Supplier", "Supplier PN"],
        "supplier_field": "MPN",
    },
}
```

Error handling for: kicad-cli not found, export failures, missing
input files, missing supplier PNs (warn, don't fail).

---

## Phase 9: Pre-Submission Review + Delivery

After generating manufacturing artifacts:

1. **Cross-artifact validation**: Gerber layers match stackup, drill
   count matches, BOM matches schematic, pick-and-place matches BOM,
   origins consistent.

2. **Gerber inspection gate**: User MUST inspect Gerbers in a viewer
   and confirm the board looks correct. Do NOT proceed without this.

3. **Present the complete design package**:
   - Requirements document
   - Component selection report with audit
   - KiCad schematic (`.kicad_sch`)
   - KiCad PCB (`.kicad_pcb`)
   - Python layout script
   - Manufacturing artifacts
   - All audit reports
   - Submission checklist

4. **Fab-specific submission instructions** (JLCPCB or PCBWay).

5. Ask: "Have you inspected the Gerbers and confirmed the board
   looks correct? Ready to submit?"

   - Confirmed → Design complete.
   - NOT reviewed → Do NOT proceed.
   - Issues found → Return to appropriate phase.
