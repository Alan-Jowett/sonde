<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# KiCad Export Tool Design Specification (`sonde-kicad`)

> **Document status:** Draft
> **Scope:** Architecture and implementation of the `sonde-kicad` Rust crate.
> **Audience:** Implementers (human or LLM agent) building the crate.
> **Related:** [kicad-export-requirements.md](kicad-export-requirements.md),
>   [kicad-export-validation.md](kicad-export-validation.md)

---

## 1  Overview

`sonde-kicad` is a Rust crate that reads IR (Intermediate Representation)
YAML files produced by the sonde-hw-design pipeline and emits KiCad 8
schematics, PCB layouts, Specctra DSN files for autorouting, and
manufacturing artifacts. It also imports Freerouter session files to
produce fully routed PCBs.

The crate provides both a library API and a CLI binary. It has **no
runtime dependency on KiCad** (except for optional Gerber export via
`kicad-cli`). All KiCad file formats are generated directly using a
built-in S-expression serializer.

---

## 2  Crate metadata

```toml
[package]
name = "sonde-kicad"
version.workspace = true
edition = "2021"
license = "MIT"

[[bin]]
name = "sonde-kicad"
path = "src/bin/main.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
sha2 = "0.10"
uuid = { version = "1", features = ["v4"] }
clap = { version = "4", features = ["derive"] }
thiserror = "2"
```

---

## 3  Module structure

```
crates/sonde-kicad/
├── Cargo.toml
├── src/
│   ├── lib.rs                # Public API re-exports
│   ├── bin/
│   │   └── main.rs           # CLI entry point (clap)
│   ├── ir/
│   │   ├── mod.rs            # IR loading and IrBundle
│   │   ├── ir0.rs            # IR-0 structured requirements
│   │   ├── ir1.rs            # IR-1 component bill
│   │   ├── ir1e.rs           # IR-1e enriched component bill
│   │   ├── ir1g.rs           # IR-1g generic geometry
│   │   ├── ir_pb.rs          # IR-PB power budget
│   │   ├── ir2.rs            # IR-2 logical circuit
│   │   └── ir3.rs            # IR-3 physical placement
│   ├── sexpr/
│   │   ├── mod.rs            # S-expression AST and serializer
│   │   └── parser.rs         # S-expression parser (for .kicad_mod, .ses)
│   ├── schematic/
│   │   ├── mod.rs            # Schematic generation entry point
│   │   ├── symbols.rs        # Vendored symbol definitions
│   │   ├── layout.rs         # Functional group placement algorithm
│   │   └── wiring.rs         # Wire routing between pins and labels
│   ├── pcb/
│   │   ├── mod.rs            # PCB generation entry point
│   │   ├── placement.rs      # Component placement from IR-3
│   │   ├── footprints.rs     # Footprint file parser + pad data
│   │   ├── zones.rs          # Copper pour zones
│   │   └── silkscreen.rs     # Silkscreen text generation
│   ├── dsn/
│   │   ├── mod.rs            # DSN generation entry point
│   │   └── structure.rs      # DSN structure/network/placement builders
│   ├── ses/
│   │   ├── mod.rs            # SES import entry point
│   │   └── parser.rs         # SES file parser
│   ├── manufacturing/
│   │   ├── mod.rs            # Manufacturing exports
│   │   ├── bom.rs            # BOM CSV generation
│   │   ├── cpl.rs            # Pick-and-place CSV generation
│   │   └── gerber.rs         # Gerber export via kicad-cli
│   ├── uuid_gen.rs           # Deterministic UUID generator
│   ├── error.rs              # Error types
│   └── validate.rs           # IR cross-validation
└── tests/
    ├── ir_loading.rs         # IR parse tests
    ├── schematic.rs          # Schematic generation tests
    ├── pcb.rs                # PCB generation tests
    ├── dsn.rs                # DSN generation tests
    ├── ses.rs                # SES import tests
    ├── manufacturing.rs      # BOM/CPL tests
    ├── determinism.rs        # Determinism verification tests
    └── golden/               # Golden file snapshots
        └── carrier-board/
```

---

## 4  IR data structures

### 4.1  `IrBundle`

The top-level container holding all loaded IR data:

```rust
pub struct IrBundle {
    pub ir0: Option<Ir0>,     // requirements (optional for most outputs)
    pub ir1: Option<Ir1>,     // component bill (needed for BOM)
    pub ir1e: Ir1e,           // enriched components (always required)
    pub ir1g: Option<Ir1g>,   // generic geometry (optional)
    pub ir_pb: Option<IrPb>,  // power budget (optional)
    pub ir2: Ir2,             // circuit netlist (always required)
    pub ir3: Option<Ir3>,     // placement (required for PCB/DSN)
    pub project: String,      // project name (from IR-1e or IR-2)
}
```

### 4.2  IR-1e types

```rust
pub struct Ir1e {
    pub schema_version: String,
    pub project: String,
    pub backend: String,
    pub components: Vec<Ir1eComponent>,
}

pub struct Ir1eComponent {
    pub ref_des: String,
    pub ir1_generic_footprint: String,
    pub kicad_symbol: String,       // e.g. "Device:R"
    pub kicad_footprint: String,    // e.g. "Resistor_SMD:R_0402_1005Metric"
    pub library_status: String,     // "FOUND" or "MISSING"
    pub bbox_mm: Dimensions,
    pub courtyard_mm: Dimensions,
    pub courtyard_area_mm2: f64,
    pub notes: Option<String>,
}

pub struct Dimensions {
    pub width: f64,
    pub height: f64,
}
```

### 4.3  IR-2 types

```rust
pub struct Ir2 {
    pub schema_version: String,
    pub project: String,
    pub nets: Vec<Net>,
    pub functional_groups: Vec<FunctionalGroup>,
    pub netlist: Vec<NetlistEntry>,
}

pub struct Net {
    pub name: String,
    pub description: String,
    pub net_type: NetType,            // power or signal
    pub power_source: Option<String>, // cross-ref to IR-PB node
}

pub enum NetType { Power, Signal }

pub struct FunctionalGroup {
    pub name: String,
    pub description: String,
    pub components: Vec<String>,     // ref_des list
    pub signal_flow: String,
    pub requirements: Vec<String>,
}

pub struct NetlistEntry {
    pub ref_des: String,
    pub component: String,          // description
    pub group: String,              // functional group name
    pub pins: Vec<PinConnection>,
    pub value: Option<String>,
    pub value_rationale: Option<String>,
    pub value_citation: Option<String>,
}

pub struct PinConnection {
    pub pin: u32,
    pub name: String,
    pub net: String,
    pub label: Option<String>,       // KNOWN/INFERRED/ASSUMED
    pub status: Option<String>,      // for NC pins
}
```

### 4.4  IR-3 types

```rust
pub struct Ir3 {
    pub schema_version: String,
    pub project: String,
    pub backend: String,
    pub board: Board,
    pub edges: Edges,
    pub connector_placement: Vec<ConnectorPlacement>,
    pub component_zones: Vec<ComponentZone>,
    pub keepout_zones: Vec<KeepoutZone>,
    pub routing_constraints: RoutingConstraints,
    pub feasibility: Feasibility,
    pub silkscreen: Silkscreen,
}

pub struct Board {
    pub shape: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub area_mm2: f64,
    pub layers: u32,
    pub copper_weight_oz: u32,
    pub surface_finish: String,
    pub origin: String,
}

pub struct ConnectorPlacement {
    pub ref_des: String,
    pub edge: Option<String>,
    pub position: Position,
    pub orientation: String,
    pub courtyard_mm: Dimensions,
    pub mounting: String,
}

pub struct Position {
    pub x_mm: f64,
    pub y_mm: f64,
}

pub struct ComponentZone {
    pub group: String,
    pub components: Vec<String>,
    pub zone: ZoneSpec,
    pub proximity_constraint_mm: f64,
}

pub struct ZoneSpec {
    pub description: String,
    pub anchor: Position,
    pub extent_mm: Dimensions,
}

pub struct KeepoutZone {
    pub name: String,
    pub boundary: KeepoutBoundary,
    pub restriction: String,
    pub layer: String,
}

pub struct KeepoutBoundary {
    pub boundary_type: String,  // "rectangle"
    pub x_mm: f64,
    pub y_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
}

pub struct RoutingConstraints {
    pub power_traces: Vec<PowerTrace>,
    pub signal_traces: Vec<SignalTrace>,
    pub via_constraints: ViaConstraints,
}

pub struct PowerTrace {
    pub net: String,
    pub min_width_mm: f64,
    pub trace_type: Option<String>,  // "copper pour" for GND
    pub layer: Option<String>,
}

pub struct SignalTrace {
    pub nets: Vec<String>,
    pub width_mm: f64,
    pub max_length_mm: Option<f64>,
}

pub struct ViaConstraints {
    pub diameter_mm: f64,
    pub drill_mm: f64,
}
```

---

## 5  S-expression serializer

### 5.1  AST

The S-expression AST is the core data structure for generating all
KiCad files and DSN files:

```rust
pub enum SExpr {
    Atom(String),
    Quoted(String),
    List(Vec<SExpr>),
}
```

### 5.2  Serialization rules

- `Atom` values are written as-is (e.g., `kicad_sch`, `42`, `0.5`).
- `Quoted` values are wrapped in double quotes (e.g., `"Device:R"`).
- `List` values are wrapped in parentheses with space-separated
  children.
- Indentation: top-level nodes at 1-indent, nested nodes at
  increasing indent levels, using 2-space indentation.
- Numeric values: floats use up to 6 decimal places, trailing zeros
  trimmed but at least one decimal place preserved.

### 5.3  Formatting

```
(kicad_sch
  (version 20231120)
  (generator "sonde-kicad")
  (generator_version "0.4.0")
  (uuid "a1b2c3d4-...")
  (paper "A4")
  (lib_symbols
    (symbol "Device:R"
      ...
    )
  )
  (symbol
    (lib_id "Device:R")
    ...
  )
)
```

### 5.4  S-expression parser

For reading `.kicad_mod` footprint files and `.ses` session files,
a parser converts S-expression text into the `SExpr` AST. The parser
handles:

- Nested parentheses to arbitrary depth.
- Quoted strings with escape sequences (`\"`, `\\`).
- Unquoted atoms (identifiers, numbers).
- Comments (lines starting with `#` in DSN/SES format).

---

## 6  Schematic generation

### 6.1  Pipeline

```
IR-1e + IR-2 → SchematicBuilder → SExpr tree → serialize → .kicad_sch
```

### 6.2  `SchematicBuilder`

```rust
pub struct SchematicBuilder<'a> {
    ir1e: &'a Ir1e,
    ir2: &'a Ir2,
    uuid_gen: UuidGenerator,
    symbols: SymbolRegistry,
}

impl SchematicBuilder<'_> {
    pub fn build(&mut self) -> Result<SExpr, Error> {
        let lib_symbols = self.build_lib_symbols()?;
        let instances = self.build_instances()?;
        let (wires, labels) = self.build_connectivity()?;
        let power_symbols = self.build_power_symbols()?;
        let title_block = self.build_title_block();
        // Assemble into root (kicad_sch ...) node
    }
}
```

### 6.3  Symbol registry

The `SymbolRegistry` provides vendored symbol definitions. Each
symbol definition is a complete KiCad `(symbol ...)` S-expression
including pin definitions.

**Strategy:** Symbol definitions are stored as Rust source constants
(S-expression strings) compiled into the binary. The registry provides
lookup by library-qualified name (e.g., `"Device:R"`).

**Initial vendored set** (covers the carrier board components):

| Library Symbol | Pins | Used by |
|---|---|---|
| `Device:R` | 2 (passive) | R1–R6 |
| `Device:C` | 2 (passive) | C1–C3 |
| `Device:FerriteBead` | 2 (passive) | FB1 |
| `Device:Q_PMOS_GSD` | 3 (G, S, D) | Q1 |
| `Connector_Generic:Conn_01x02` | 2 (passive) | J3 |
| `Connector_Generic:Conn_01x03` | 3 (passive) | J2 |
| `Connector_Generic:Conn_01x04` | 4 (passive) | J1 |
| `Connector_Generic:Conn_01x07` | 7 (passive) | J6, J7 |
| `power:GND` | 1 (power_in) | GND symbols |
| `power:+3V3` | 1 (power_in) | +3V3 symbols |

Additional symbols can be loaded from an external `.kicad_sym` file
(KE-1001).

### 6.4  Component instance generation

For each component in IR-1e:

1. Look up the symbol in the registry by `kicad_symbol`.
2. Determine placement position from the layout algorithm (§6.5).
3. Generate a `(symbol ...)` node with:
   - `lib_id` = IR-1e `kicad_symbol`
   - `at` = placement position
   - `uuid` = deterministic UUID
   - Properties: `Reference` (ref_des), `Value` (from IR-2),
     `Footprint` (kicad_footprint), `Datasheet` ("~")
   - Optional `LCSC` property if available from IR-1
   - Pin nodes with UUIDs for each pin

### 6.5  Layout algorithm

Components are arranged by functional group in a column-based layout:

```
┌──────────────────────────────────────────────┐
│  Group 1: power-input     │  Group 4: 1-wire │
│  (J3, FB1, C1)           │  (J2, R3)        │
│                          │                   │
│  Group 2: power-gating   │  Group 5: sensing │
│  (Q1, R4, C2)            │  (R5, R6, C3)    │
│                          │                   │
│  Group 3: i2c-interface  │  Group 6: MCU     │
│  (J1, R1, R2)            │  (J6, J7)        │
└──────────────────────────────────────────────┘
```

**Algorithm:**

1. Sort functional groups in the order they appear in IR-2.
2. Divide groups into two columns.
3. Within each group, arrange components vertically with 10.16mm
   spacing (4 grid units).
4. Groups are separated by 20.32mm (8 grid units) vertically.
5. Columns are separated by 60.96mm (24 grid units) horizontally.
6. All positions snap to the KiCad 1.27mm grid.

### 6.6  Wire and label generation

For each net in IR-2:

1. Collect all pins connected to this net (from IR-2's netlist).
2. For each pin:
   a. Compute the pin's endpoint from the component's position and
      the symbol's pin offset.
   b. Generate a short wire stub extending from the pin.
   c. Place a `(label ...)` at the wire endpoint with the net name.

KiCad merges all labels with the same name into one electrical net.

**Power nets** use power symbol instances instead of labels:
- `GND` → place `power:GND` symbol.
- Other power nets (VBAT, VBAT_FILT, SENSOR_V) → place
  `(global_label ...)` with `(shape input)` and power flag, or
  create custom power symbols.

**NC pins** get `(no_connect (at x y) (uuid ...))` markers.

---

## 7  PCB generation

### 7.1  Pipeline

```
IR-1e + IR-2 + IR-3 → PcbBuilder → SExpr tree → serialize → .kicad_pcb
```

### 7.2  `PcbBuilder`

```rust
pub struct PcbBuilder<'a> {
    ir1e: &'a Ir1e,
    ir2: &'a Ir2,
    ir3: &'a Ir3,
    uuid_gen: UuidGenerator,
    footprint_registry: FootprintRegistry,
}

impl PcbBuilder<'_> {
    pub fn build(&mut self) -> Result<SExpr, Error> {
        let layers = self.build_layers()?;
        let setup = self.build_setup()?;
        let nets = self.build_nets()?;
        let footprints = self.build_footprints()?;
        let zones = self.build_zones()?;
        let board_outline = self.build_outline()?;
        let keepouts = self.build_keepouts()?;
        // Assemble into root (kicad_pcb ...) node
    }
}
```

### 7.3  Board outline

Generate `(gr_line ...)` segments on the `Edge.Cuts` layer forming a
closed rectangle:

```sexpr
(gr_line (start 0 0) (end 25 0) (layer "Edge.Cuts") (stroke (width 0.05) (type default)))
(gr_line (start 25 0) (end 25 35) (layer "Edge.Cuts") (stroke (width 0.05) (type default)))
(gr_line (start 25 35) (end 0 35) (layer "Edge.Cuts") (stroke (width 0.05) (type default)))
(gr_line (start 0 35) (end 0 0) (layer "Edge.Cuts") (stroke (width 0.05) (type default)))
```

Coordinates from IR-3: origin at (0,0), width × height from
`board.width_mm` × `board.height_mm`.

**Coordinate system note:** IR-3 uses bottom-left origin with Y
increasing upward. KiCad PCB uses top-left origin with Y increasing
downward. The tool applies the transform: `kicad_y = board_height - ir3_y`.

### 7.4  Layer definitions

For a 2-layer board:

```sexpr
(layers
  (0 "F.Cu" signal)
  (31 "B.Cu" signal)
  (32 "B.Adhes" user "B.Adhesive")
  (33 "F.Adhes" user "F.Adhesive")
  (34 "B.Paste" user)
  (35 "F.Paste" user)
  (36 "B.SilkS" user "B.Silkscreen")
  (37 "F.SilkS" user "F.Silkscreen")
  (38 "B.Mask" user "B.Mask")
  (39 "F.Mask" user "F.Mask")
  (44 "Edge.Cuts" user)
  (46 "B.CrtYd" user "B.Courtyard")
  (47 "F.CrtYd" user "F.Courtyard")
  (48 "B.Fab" user "B.Fabrication")
  (49 "F.Fab" user "F.Fabrication")
)
```

### 7.5  Component placement

Two placement sources in IR-3:

**1. Explicit placement** (`connector_placement`):
Connectors have exact coordinates. Use directly after Y-axis
coordinate transform.

**2. Zone placement** (`component_zones`):
Components within a zone are placed using a simple packing algorithm:

1. Start at the zone's anchor position.
2. Place components left-to-right, wrapping to the next row when
   the zone's width is exceeded.
3. Spacing between components is derived from `proximity_constraint_mm`.
4. Verify no courtyard overlaps using IR-1e `courtyard_mm` data.

### 7.6  Footprint generation

Each placed component needs a `(footprint ...)` node containing:

- Footprint library reference (from IR-1e `kicad_footprint`).
- Position and rotation (`(at x y rotation)`).
- Layer (`"F.Cu"` for front, `"B.Cu"` for back).
- Pads with net assignments.
- Reference designator and value text.

**Pad data source:** Footprint pad geometry (positions, sizes, shapes,
drill) is loaded from `.kicad_mod` files in the footprint directory.
The `FootprintRegistry` loads and caches footprint definitions.

### 7.7  Net definitions

Each named net from IR-2 gets a `(net ...)` definition:

```sexpr
(net 0 "")       ; unconnected net (required by KiCad)
(net 1 "VBAT")
(net 2 "GND")
(net 3 "SENSOR_V")
...
```

Net IDs are assigned sequentially, sorted by net name. Pad-to-net
assignments are derived from IR-2's pin-to-net connectivity.

### 7.8  Keep-out zones

Each IR-3 keep-out zone generates a `(zone ...)` node with
`(keepout ...)` properties:

```sexpr
(zone
  (net 0) (net_name "")
  (layer "F.Cu")
  (uuid "...")
  (hatch edge 0.5)
  (connect_pads (clearance 0))
  (keepout (tracks not_allowed) (vias not_allowed) (pads not_allowed)
           (copperpour not_allowed) (footprints not_allowed))
  (fill (thermal_gap 0.5) (thermal_bridge_width 0.5))
  (polygon (pts
    (xy 8 30) (xy 17 30) (xy 17 35) (xy 8 35)
  ))
)
```

### 7.9  Ground plane copper pour

If IR-3 specifies a GND copper pour, generate a zone covering the
board outline on the specified layer (typically B.Cu), assigned to
the GND net:

```sexpr
(zone
  (net <gnd_net_id>) (net_name "GND")
  (layer "B.Cu")
  (uuid "...")
  (hatch edge 0.5)
  (connect_pads (clearance 0.25))
  (min_thickness 0.2)
  (fill yes (thermal_gap 0.3) (thermal_bridge_width 0.3))
  (polygon (pts
    (xy 0 0) (xy 25 0) (xy 25 35) (xy 0 35)
  ))
)
```

---

## 8  Specctra DSN generation

### 8.1  Pipeline

```
IR-1e + IR-2 + IR-3 + footprint pad data → DsnBuilder → DSN text → .dsn
```

### 8.2  DSN file structure

```
(pcb "<project>.dsn"
  (parser
    (string_quote ")
    (space_in_quoted_tokens on)
    (host_cad "sonde-kicad")
    (host_version "<version>")
  )
  (resolution um 10)
  (unit um)
  (structure ...)
  (placement ...)
  (library ...)
  (network ...)
  (wiring)
)
```

### 8.3  Structure section

Defines layers, boundaries, and design rules:

```
(structure
  (layer F.Cu (type signal) (property (index 0)))
  (layer B.Cu (type signal) (property (index 1)))
  (boundary
    (path signal 0
      0 0
      250000 0
      250000 350000
      0 350000
      0 0
    )
  )
  (keepout "<name>"
    (polygon signal 0
      <coordinates in um>
    )
  )
  (via "Via[0-1]_600:300_um")
  (rule
    (width 250)
    (clearance 200)
  )
)
```

Coordinates are in micrometers (µm). Conversion: `um = mm * 1000`.

### 8.4  Placement section

Each component with its position:

```
(placement
  (component "Resistor_SMD:R_0402_1005Metric"
    (place R1 5000 200000 front 0
      (PN "4.7kΩ")
    )
    (place R2 5000 190000 front 0
      (PN "4.7kΩ")
    )
  )
  (component "Package_TO_SOT_SMD:SOT-23"
    (place Q1 50000 210000 front 0
      (PN "Si2301CDS")
    )
  )
)
```

Components sharing the same footprint are grouped under one
`(component ...)` node.

### 8.5  Library section

Defines pad shapes (images) for each footprint:

```
(library
  (image "Resistor_SMD:R_0402_1005Metric"
    (outline (path signal 50 -930 -470 930 -470))
    (outline (path signal 50 930 -470 930 470))
    (outline (path signal 50 930 470 -930 470))
    (outline (path signal 50 -930 470 -930 -470))
    (pin Rect[T]Pad_560x620_um 1 -480 0)
    (pin Rect[T]Pad_560x620_um 2 480 0)
  )
  (padstack Rect[T]Pad_560x620_um
    (shape (rect F.Cu -280 -310 280 310))
    (attach off)
  )
  (padstack Via[0-1]_600:300_um
    (shape (circle F.Cu 600 0 0))
    (shape (circle B.Cu 600 0 0))
    (attach off)
  )
)
```

Pad data is extracted from parsed `.kicad_mod` footprint files.

### 8.6  Network section

Defines nets and their pin connections:

```
(network
  (net VBAT
    (pins J3-1 FB1-1 Q1-2 R4-2)
  )
  (net GND
    (pins J3-2 C1-2 C2-2 C3-2 R6-2 J1-1 J2-1 J7-6)
  )
  (net SDA
    (pins R1-1 J1-3 J6-5)
  )
  (class Default
    (circuit (use_via "Via[0-1]_600:300_um"))
    (rule (width 250) (clearance 200))
  )
  (class Power VBAT VBAT_FILT SENSOR_V
    (circuit (use_via "Via[0-1]_600:300_um"))
    (rule (width 500) (clearance 200))
  )
)
```

Net classes are derived from IR-3 `routing_constraints`:
- Power nets → use `power_traces.min_width_mm`.
- Signal nets → use `signal_traces.width_mm`.
- Default → 0.25mm width, 0.2mm clearance.

---

## 9  SES import

### 9.1  Pipeline

```
.kicad_pcb + .ses → SesImporter → merged .kicad_pcb
```

### 9.2  SES file parsing

A Specctra SES file contains:

```
(session "<project>.ses"
  (base_design "<project>.dsn")
  (placement ...)
  (was_is ...)
  (routes
    (resolution um 10)
    (parser ...)
    (network_out
      (net VBAT
        (wire
          (path F.Cu 5000
            50000 100000
            50000 200000
          )
        )
      )
      (net GND
        (wire
          (path B.Cu 2500
            30000 150000
            80000 150000
          )
        )
        (via "Via[0-1]_600:300_um" 60000 180000)
      )
    )
  )
)
```

### 9.3  SES to KiCad conversion

For each `(wire (path <layer> <width> <x1> <y1> <x2> <y2> ...))`:

1. Convert coordinates from DSN µm to KiCad mm (`mm = um / 1000`).
2. Convert width from µm to mm.
3. Look up the net ID from the net name in the existing PCB.
4. Generate a `(segment ...)` node:

```sexpr
(segment
  (start 5.0 10.0)
  (end 5.0 20.0)
  (width 0.5)
  (layer "F.Cu")
  (net 1)
  (uuid "...")
)
```

For each `(via "<padstack>" <x> <y>)`:

1. Convert coordinates from µm to mm.
2. Look up the net from the enclosing `(net ...)` node.
3. Generate a `(via ...)` node:

```sexpr
(via
  (at 6.0 18.0)
  (size 0.6)
  (drill 0.3)
  (layers "F.Cu" "B.Cu")
  (net 2)
  (uuid "...")
)
```

### 9.4  Merge strategy

1. Parse the existing `.kicad_pcb` as an S-expression tree.
2. Parse the `.ses` file and extract wires and vias.
3. Append new `(segment ...)` and `(via ...)` nodes to the PCB tree.
4. Re-serialize the modified tree.

The merge preserves all existing PCB content. Only routing elements
are added.

---

## 10  Manufacturing exports

### 10.1  BOM CSV

Format (JLCPCB compatible):

```csv
Designator,Value,Footprint,Manufacturer,Part Number,LCSC Part Number,Quantity
C1,10µF,Capacitor_SMD:C_0805_2012Metric,Samsung Electro-Mechanics,CL21A106KAYNNNE,C15850,1
R1,4.7kΩ,Resistor_SMD:R_0402_1005Metric,UNI-ROYAL,0402WGF4701TCE,C25900,1
```

Data sources:
- `Designator` → IR-1e `ref_des`
- `Value` → IR-2 netlist entry `value`
- `Footprint` → IR-1e `kicad_footprint`
- `Manufacturer` → IR-1 `manufacturer`
- `Part Number` → IR-1 `part_number`
- `LCSC Part Number` → IR-1 `sourcing.lcsc_pn`
- `Quantity` → always 1 per ref_des

Rows sorted by reference designator.

### 10.2  Pick-and-place CSV

Format (JLCPCB compatible):

```csv
Designator,Mid X,Mid Y,Layer,Rotation
C1,8.0,25.0,Top,0
R1,5.0,20.0,Top,90
```

Data sources:
- `Designator` → ref_des
- `Mid X`, `Mid Y` → component center from PCB placement (mm)
- `Layer` → "Top" or "Bottom"
- `Rotation` → component rotation in degrees

### 10.3  Gerber export

Delegates to `kicad-cli`:

```bash
kicad-cli pcb export gerbers <pcb_file> \
    --output <output_dir>/ \
    --layers "F.Cu,B.Cu,F.SilkS,B.SilkS,F.Mask,B.Mask,Edge.Cuts"

kicad-cli pcb export drill <pcb_file> \
    --output <output_dir>/ \
    --format excellon
```

If `kicad-cli` is not found in `PATH`, the tool prints a clear error
message suggesting manual export from KiCad GUI.

---

## 11  Deterministic UUID generator

### 11.1  Algorithm

```rust
pub struct UuidGenerator {
    seed: [u8; 32],
    counter: u64,
}

impl UuidGenerator {
    pub fn new(project: &str, ir_content_hash: &[u8; 32]) -> Self {
        // seed = SHA-256(project || ir_content_hash)
        let mut hasher = Sha256::new();
        hasher.update(project.as_bytes());
        hasher.update(ir_content_hash);
        Self {
            seed: hasher.finalize().into(),
            counter: 0,
        }
    }

    pub fn next(&mut self, path: &str) -> String {
        // hash = SHA-256(seed || path || counter)
        let mut hasher = Sha256::new();
        hasher.update(&self.seed);
        hasher.update(path.as_bytes());
        hasher.update(&self.counter.to_le_bytes());
        self.counter += 1;
        let hash = hasher.finalize();
        // Format as UUID v4
        format_uuid_v4(&hash[..16])
    }
}
```

The `path` argument provides uniqueness within a run (e.g.,
`"symbol:R1"`, `"pin:R1:1"`, `"wire:SDA:0"`). The counter provides
uniqueness even if `path` is accidentally reused.

### 11.2  IR content hash

The `ir_content_hash` is computed by hashing the raw bytes of all
input IR files in sorted filename order:

```rust
fn compute_ir_hash(ir_dir: &Path) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let mut files: Vec<_> = std::fs::read_dir(ir_dir)
        .filter(|e| e.path().extension() == Some("yaml"))
        .collect();
    files.sort_by_key(|e| e.file_name());
    for entry in files {
        hasher.update(&std::fs::read(entry.path()));
    }
    hasher.finalize().into()
}
```

---

## 12  CLI design

### 12.1  Argument parser (clap)

```rust
#[derive(Parser)]
#[command(name = "sonde-kicad", about = "Convert sonde-hw-design IR to KiCad files")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate KiCad schematic (.kicad_sch)
    Schematic {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
        #[arg(long)]
        extra_symbols: Option<PathBuf>,
    },
    /// Generate KiCad PCB layout (.kicad_pcb)
    Pcb {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
        #[arg(long)]
        footprint_dir: Option<PathBuf>,
    },
    /// Generate Specctra DSN file for Freerouter
    Dsn {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
        #[arg(long)]
        footprint_dir: Option<PathBuf>,
    },
    /// Import Freerouter session (.ses) into PCB
    ImportSes {
        #[arg(long)]
        pcb: PathBuf,
        #[arg(long)]
        ses: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Generate BOM CSV
    Bom {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Generate pick-and-place CSV
    Cpl {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
    },
    /// Export Gerber files via kicad-cli
    Gerber {
        #[arg(long)]
        pcb: PathBuf,
        #[arg(long, default_value = "./output/gerber")]
        output_dir: PathBuf,
    },
    /// Run full pipeline: schematic → PCB → DSN → BOM → CPL
    Build {
        #[arg(long, default_value = ".")]
        ir_dir: PathBuf,
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,
        #[arg(long)]
        footprint_dir: Option<PathBuf>,
        #[arg(long)]
        extra_symbols: Option<PathBuf>,
    },
}
```

### 12.2  Output file naming

| Subcommand | Output file |
|---|---|
| `schematic` | `<output_dir>/<project>.kicad_sch` |
| `pcb` | `<output_dir>/<project>.kicad_pcb` |
| `dsn` | `<output_dir>/<project>.dsn` |
| `import-ses` | specified by `--output` |
| `bom` | `<output_dir>/<project>-bom.csv` |
| `cpl` | `<output_dir>/<project>-cpl.csv` |
| `gerber` | `<output_dir>/gerber/` |
| `build` | all of the above |

---

## 13  Error handling

### 13.1  Error type hierarchy

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error in {file}: {source}")]
    YamlParse {
        file: String,
        source: serde_yaml::Error,
    },

    #[error("Missing required IR file: {0}")]
    MissingIrFile(String),

    #[error("Unsupported schema version in {file}: found {found}, expected {expected}")]
    SchemaVersion {
        file: String,
        found: String,
        expected: String,
    },

    #[error("IR cross-validation error: {0}")]
    CrossValidation(String),

    #[error("Missing symbol definition: {0}")]
    MissingSymbol(String),

    #[error("Missing footprint definition: {0}")]
    MissingFootprint(String),

    #[error("SES parse error: {0}")]
    SesParse(String),

    #[error("kicad-cli not found: {0}")]
    KicadCliNotFound(String),
}
```

### 13.2  Error reporting

CLI errors are printed to stderr with:
- File name and path where the error originated.
- For YAML errors: line and column numbers.
- For cross-validation: the two conflicting values and their sources.
- Suggestion for resolution when applicable.

---

## 14  Coordinate system conventions

### 14.1  IR coordinate system

IR-3 uses **bottom-left origin** with Y increasing upward. This
matches schematic conventions and physical board orientation.

### 14.2  KiCad PCB coordinate system

KiCad PCB uses **top-left origin** with Y increasing downward.

### 14.3  Transform

```rust
fn ir3_to_kicad_y(ir3_y: f64, board_height: f64) -> f64 {
    board_height - ir3_y
}
```

All IR-3 Y coordinates are transformed when generating PCB and DSN
output. The X coordinate is unchanged.

### 14.4  KiCad schematic coordinate system

KiCad schematics use screen coordinates: origin at top-left, Y
increasing downward. The schematic layout algorithm (§6.5) works
directly in schematic coordinates.

### 14.5  DSN coordinate system

Specctra DSN uses **bottom-left origin** with Y increasing upward
(same as IR-3). Coordinates are in micrometers. The transform is:

```rust
fn mm_to_um(mm: f64) -> i64 {
    (mm * 1000.0).round() as i64
}
```
