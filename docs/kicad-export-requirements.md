<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# KiCad Export Tool Requirements Specification (`sonde-kicad`)

> **Document status:** Draft
> **Scope:** A Rust crate that converts sonde-hw-design IR files into
>   KiCad 8 schematics, PCB layouts, Specctra DSN files, and
>   manufacturing artifacts.
> **Audience:** Implementers (human or LLM agent) building the crate.
> **Related:** [kicad-export-design.md](kicad-export-design.md),
>   [kicad-export-validation.md](kicad-export-validation.md),
>   [protocol-crate-design.md](protocol-crate-design.md),
>   [gateway-requirements.md](gateway-requirements.md) (format reference)

---

## 1  Definitions

| Term | Definition |
|---|---|
| **IR** | Intermediate Representation — a YAML file produced by the sonde-hw-design pipeline. Each IR captures one pass of the design process. |
| **IR-0** | Structured requirements (functional, electrical, mechanical, environmental). |
| **IR-1** | Component bill — EDA-agnostic component list with generic footprints. |
| **IR-1e** | Enriched component bill — IR-1 augmented with KiCad symbol/footprint mappings and courtyard geometry. |
| **IR-1g** | Generic geometry — JEDEC-standard body dimensions for rough feasibility. |
| **IR-PB** | Power budget — power tree, consumers, operating modes, margin analysis. |
| **IR-2** | Logical circuit description — complete YAML netlist with named nets, functional groups, and pin-level connectivity. |
| **IR-3** | Physical placement constraints — board outline, connector positions, component zones, routing constraints, keep-outs. |
| **S-expression** | The parenthesized text format used by KiCad 8 for `.kicad_sch` and `.kicad_pcb` files. |
| **Specctra DSN** | A text-based PCB data exchange format used by Freerouter for autorouting. |
| **Specctra SES** | A session file produced by Freerouter containing routed traces and vias. |
| **Freerouter** | An open-source PCB autorouter that accepts DSN files and produces SES files. |
| **Ratsnest** | Unrouted connections shown as straight lines between pads that should be electrically connected. |
| **ERC** | Electrical Rules Check — validates schematic connectivity and pin types. |
| **DRC** | Design Rules Check — validates PCB layout against manufacturing constraints. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`KE-XXXX`).
- **Title** — Short name.
- **Description** — What the tool must do.
- **Acceptance criteria** — Observable, testable conditions.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — Motivation for the requirement.

---

## 3  IR loading and validation

### KE-0100  IR file loading

**Priority:** Must
**Source:** sonde-hw-design pipeline — IR files are the sole input.

**Description:**
The tool MUST load IR files from a directory containing YAML files
named `IR-0.yaml`, `IR-1.yaml`, `IR-1e.yaml`, `IR-1g.yaml`,
`IR-PB.yaml`, `IR-2.yaml`, and `IR-3.yaml`.

**Acceptance criteria:**

1. Given a directory path, the tool loads all IR YAML files present.
2. Each IR file is parsed into a strongly-typed Rust data structure.
3. The tool reports which IR files were loaded and which are missing.

---

### KE-0101  Required IR files for each output

**Priority:** Must
**Source:** Pipeline dependency graph.

**Description:**
Each output format requires a specific subset of IR files. The tool
MUST fail with a clear error if any required IR file is missing.

| Output | Required IR files |
|---|---|
| Schematic (`.kicad_sch`) | IR-1e, IR-2 |
| PCB (`.kicad_pcb`) | IR-1e, IR-2, IR-3 |
| Specctra DSN (`.dsn`) | IR-1e, IR-2, IR-3 |
| BOM CSV | IR-1, IR-1e |
| Pick-and-place CSV | IR-1e, IR-3 |
| SES import | IR-1e, IR-2, IR-3, plus existing `.kicad_pcb` and `.ses` file |

**Acceptance criteria:**

1. If a required IR file is missing, the tool exits with a non-zero
   status and names the missing file(s).
2. If all required IR files are present, loading succeeds.

---

### KE-0102  Schema version validation

**Priority:** Must
**Source:** IR format stability.

**Description:**
Each IR file contains a `schema_version` field. The tool MUST validate
that the schema version is compatible (currently `"1.0.0"`).

**Acceptance criteria:**

1. If `schema_version` is `"1.0.0"`, loading succeeds.
2. If `schema_version` is missing or unrecognized, the tool fails
   with a clear error naming the file and the unsupported version.

---

### KE-0103  IR cross-validation

**Priority:** Should
**Source:** Pipeline integrity.

**Description:**
The tool SHOULD validate cross-references between IR files:

- Every `ref_des` in IR-2's netlist must exist in IR-1e's components.
- Every `kicad_symbol` and `kicad_footprint` in IR-1e must be non-empty
  and have `library_status: "FOUND"`.
- Every power net in IR-2 that references an IR-PB node must name a
  node that exists in IR-PB's `power_tree`.
- Every component in IR-3's `component_zones` must exist in IR-1e.

**Acceptance criteria:**

1. Cross-validation errors are reported with file names, field paths,
   and the mismatched values.
2. Cross-validation failures are hard errors (prevent output generation).

---

### KE-0104  Fail-stop on malformed YAML

**Priority:** Must
**Source:** Data integrity.

**Description:**
If any IR file contains invalid YAML (syntax errors, unexpected types,
missing required fields), the tool MUST fail immediately with a
diagnostic pointing to the file and the error location.

**Acceptance criteria:**

1. A YAML syntax error produces an error message with file name and
   line number.
2. A missing required field produces an error naming the field and
   the IR file.
3. No partial output is generated when IR loading fails.

---

## 4  Schematic generation

### KE-0200  Valid KiCad 8 schematic output

**Priority:** Must
**Source:** KiCad 8 file format specification.

**Description:**
The tool MUST generate a `.kicad_sch` file in KiCad 8 S-expression
format (schema version `20231120`) that opens without errors in
KiCad 8.

**Acceptance criteria:**

1. The output file starts with `(kicad_sch (version 20231120) ...)`.
2. The `generator` field is `"sonde-kicad"`.
3. KiCad 8 opens the file without parse errors.

---

### KE-0201  Symbol library declarations

**Priority:** Must
**Source:** KiCad format — every referenced symbol must be declared.

**Description:**
The schematic MUST contain a `(lib_symbols ...)` block declaring every
symbol referenced by component instances. Symbol declarations must
include pin definitions with correct pin numbers, names, and
electrical types.

**Acceptance criteria:**

1. Every `lib_id` used by a component instance has a matching
   `(symbol ...)` entry in `lib_symbols`.
2. Pin numbers in the library symbol match the pin numbers used in
   IR-2's netlist for that component.
3. No ERC errors result from missing or mismatched symbol declarations.

---

### KE-0202  Component instances from IR-1e and IR-2

**Priority:** Must
**Source:** IR-1e provides symbol/footprint mappings; IR-2 provides
  component values and properties.

**Description:**
For each component in IR-1e, the tool MUST generate a `(symbol ...)`
instance node with:

- `lib_id` from IR-1e's `kicad_symbol` field.
- `Reference` property from IR-1e's `ref_des`.
- `Value` property from IR-2's netlist entry `value` field (for
  passives) or `component` description (for others).
- `Footprint` property from IR-1e's `kicad_footprint`.
- A stable, deterministic UUID.
- Pin nodes with UUIDs for net connectivity.

**Acceptance criteria:**

1. Component count in the schematic equals the component count in IR-1e.
2. Every component's `Reference`, `Value`, and `Footprint` properties
   match the IR data.
3. Every component has `(in_bom yes)` and `(on_board yes)`.

---

### KE-0203  Net connectivity via labels

**Priority:** Must
**Source:** IR-2 netlist defines all connections.

**Description:**
The tool MUST represent net connectivity using KiCad net labels. For
each named net in IR-2, a `(label ...)` node is placed at every pin
that connects to that net.

**Acceptance criteria:**

1. Every pin-to-net assignment in IR-2's netlist is represented by a
   label at the corresponding pin location in the schematic.
2. All pins connected to the same net share the same label name.
3. NC (not connected) pins are represented with KiCad
   `(no_connect ...)` markers.

---

### KE-0204  Power symbols

**Priority:** Must
**Source:** KiCad ERC requires power pins to be driven by power symbols.

**Description:**
Power nets (type `power` in IR-2) MUST be represented using KiCad
power symbol instances (e.g., `power:GND`, `power:+3V3`). Custom
power net names (e.g., `VBAT`, `VBAT_FILT`, `SENSOR_V`) that do not
have standard KiCad power symbols MUST use `(power_port ...)` nodes
or `(global_label ...)` nodes with the power flag.

**Acceptance criteria:**

1. `GND` net uses a `power:GND` symbol instance.
2. Custom power nets are represented with power-flagged labels or
   custom power symbols so ERC does not report "power pin not driven."
3. Power symbol instances appear at every location where a power net
   connects to a component pin.

---

### KE-0205  Functional group layout

**Priority:** Should
**Source:** IR-2's `functional_groups` — visual organization.

**Description:**
Components SHOULD be visually grouped in the schematic according to
IR-2's `functional_groups`. Each functional group occupies a distinct
region of the schematic sheet with components placed in proximity.

**Acceptance criteria:**

1. Components within the same functional group are placed closer to
   each other than to components in other groups.
2. Groups are arranged in a logical flow (power-input → power-gating →
   interfaces → sensing → MCU socket).

---

### KE-0206  Title block

**Priority:** Must
**Source:** Design documentation.

**Description:**
The schematic MUST include a `(title_block ...)` with:

- `title`: IR-0 or IR-1e `project` name.
- `date`: a deterministic date (not system clock).
- `rev`: schema version from IR files.
- `comment 1`: "Generated by sonde-kicad <version>".

**Acceptance criteria:**

1. Title block fields are populated from IR data.
2. Date does not change between runs with the same input.

---

### KE-0207  Wire segments

**Priority:** Must
**Source:** KiCad schematic visual clarity.

**Description:**
The tool MUST generate `(wire ...)` segments connecting component pins
to their net labels. Wires must be orthogonal (horizontal or vertical)
and must not overlap with other wires or components.

**Acceptance criteria:**

1. Every pin has a wire segment connecting it to its net label.
2. All wire segments are orthogonal (0° or 90° angles only).
3. Wire endpoints land exactly on pin endpoints or label positions.

---

### KE-0208  LCSC part number annotation

**Priority:** Should
**Source:** JLCPCB assembly ordering.

**Description:**
If IR-1 provides `sourcing.lcsc_pn` for a component, the tool SHOULD
add an `LCSC` custom property to the schematic symbol instance.

**Acceptance criteria:**

1. Components with LCSC part numbers in IR-1 have a hidden `LCSC`
   property in the schematic.
2. The property value matches the IR-1 `lcsc_pn` field.

---

## 5  PCB generation

### KE-0300  Valid KiCad 8 PCB output

**Priority:** Must
**Source:** KiCad 8 file format specification.

**Description:**
The tool MUST generate a `.kicad_pcb` file in KiCad 8 S-expression
format (schema version `20231120`) that opens without errors in
KiCad 8.

**Acceptance criteria:**

1. The output file starts with `(kicad_pcb (version 20231120) ...)`.
2. The `generator` field is `"sonde-kicad"`.
3. KiCad 8 opens the file without parse errors.

---

### KE-0301  Board outline from IR-3

**Priority:** Must
**Source:** IR-3 `board` section.

**Description:**
The PCB MUST contain a board outline on the `Edge.Cuts` layer
matching IR-3's board dimensions (`width_mm`, `height_mm`, `shape`).

**Acceptance criteria:**

1. Board outline is a closed rectangle on `Edge.Cuts`.
2. Dimensions match IR-3's `width_mm` × `height_mm` exactly.
3. Origin is at IR-3's specified origin (default: bottom-left at 0,0).

---

### KE-0302  Layer stackup from IR-3

**Priority:** Must
**Source:** IR-3 `board.layers` and `board.stackup`.

**Description:**
The PCB layer configuration MUST match IR-3's stackup specification.

**Acceptance criteria:**

1. For a 2-layer board: F.Cu and B.Cu copper layers are defined.
2. Standard non-copper layers are present: F.SilkS, B.SilkS,
   F.Mask, B.Mask, F.Paste, B.Paste, F.CrtYd, B.CrtYd, Edge.Cuts.
3. For 4-layer boards: In1.Cu and In2.Cu are added.

---

### KE-0303  Component placement from IR-3

**Priority:** Must
**Source:** IR-3 `connector_placement` and `component_zones`.

**Description:**
Components with explicit positions in IR-3's `connector_placement`
MUST be placed at those exact coordinates. Components listed in
`component_zones` MUST be placed within their zone's
`anchor`+`extent_mm` bounding box, respecting
`proximity_constraint_mm`.

**Acceptance criteria:**

1. Connectors (J1, J2, J3, etc.) are placed at their IR-3 `position`
   coordinates.
2. Zone-assigned components are within their zone's bounding box.
3. Components within a zone are no further than
   `proximity_constraint_mm` from each other.
4. No component courtyards overlap (checked against IR-1e
   `courtyard_mm`).

---

### KE-0304  Footprint references from IR-1e

**Priority:** Must
**Source:** IR-1e `kicad_footprint` field.

**Description:**
Each component's footprint in the PCB MUST reference the footprint
specified in IR-1e's `kicad_footprint` field.

**Acceptance criteria:**

1. Every component's footprint library reference matches IR-1e.
2. Footprint pad count matches IR-2's pin count for that component.

---

### KE-0305  Net definitions from IR-2

**Priority:** Must
**Source:** IR-2 net list and netlist.

**Description:**
The PCB MUST define nets matching IR-2's named nets and assign pads
to nets according to IR-2's pin-to-net connectivity.

**Acceptance criteria:**

1. Every named net in IR-2 appears as a `(net ...)` definition in the PCB.
2. Component pads are assigned to the correct nets per IR-2's netlist.
3. A ratsnest is visible in KiCad for all unrouted connections.

---

### KE-0306  Keep-out zones from IR-3

**Priority:** Must
**Source:** IR-3 `keepout_zones`.

**Description:**
The PCB MUST include keep-out zones matching IR-3's specifications,
with correct layer assignments and restriction types.

**Acceptance criteria:**

1. Each IR-3 keep-out zone appears in the PCB with correct boundary
   coordinates.
2. Keep-out restrictions (no copper, no components, no traces) are
   applied on the specified layers.

---

### KE-0307  Design rules from IR-3

**Priority:** Should
**Source:** IR-3 `routing_constraints`.

**Description:**
The PCB SHOULD include net class definitions reflecting IR-3's
routing constraints (trace widths, clearances, via sizes).

**Acceptance criteria:**

1. Power nets have net classes with `min_width_mm` from IR-3's
   `power_traces`.
2. Signal nets have net classes with `width_mm` from IR-3's
   `signal_traces`.
3. Via constraints match IR-3's `via_constraints`.

---

### KE-0308  Ground plane copper pour

**Priority:** Should
**Source:** IR-3 `routing_constraints.power_traces` GND specification.

**Description:**
If IR-3 specifies a GND copper pour (type `"copper pour"`) on a
specific layer, the PCB SHOULD include a `(zone ...)` definition
for that layer covering the full board area, assigned to the GND net.

**Acceptance criteria:**

1. A copper zone on the specified layer (typically B.Cu) covers the
   board outline.
2. The zone is assigned to the GND net.
3. Zone clearance and fill settings are reasonable defaults.

---

### KE-0309  Silkscreen from IR-3

**Priority:** Should
**Source:** IR-3 `silkscreen` section.

**Description:**
The PCB SHOULD include silkscreen text elements matching IR-3's
silkscreen plan (labels, reference designators, polarity markers).

**Acceptance criteria:**

1. Each label in IR-3's `silkscreen.labels` appears as a text element
   on `F.SilkS`.
2. Reference designators are visible on the silkscreen layer.

---

### KE-0310  No trace routing

**Priority:** Must
**Source:** Design decision — Freerouter handles routing.

**Description:**
The generated PCB MUST NOT contain routed traces or vias. Component
placement and net definitions produce a ratsnest; routing is deferred
to Freerouter via DSN export.

**Acceptance criteria:**

1. The PCB file contains zero `(segment ...)` nodes.
2. The PCB file contains zero `(via ...)` nodes (except keep-out vias).
3. All nets appear as ratsnest (unrouted).

---

## 6  Specctra DSN export

### KE-0400  Valid Specctra DSN output

**Priority:** Must
**Source:** Freerouter input format.

**Description:**
The tool MUST generate a `.dsn` file in Specctra DSN format that
Freerouter can load and autoroute.

**Acceptance criteria:**

1. Freerouter opens the DSN file without parse errors.
2. The file contains: `(pcb ...)` root with `(parser ...)`,
   `(resolution ...)`, `(unit ...)`, `(structure ...)`,
   `(placement ...)`, `(library ...)`, `(network ...)` sections.

---

### KE-0401  Board boundary in DSN

**Priority:** Must
**Source:** IR-3 board outline.

**Description:**
The DSN file MUST define the board boundary in the `(structure ...)`
section matching IR-3's board dimensions.

**Acceptance criteria:**

1. Board boundary polygon matches IR-3 dimensions.
2. Layer definitions match the PCB stackup.

---

### KE-0402  Component placement in DSN

**Priority:** Must
**Source:** IR-3 placement data.

**Description:**
The DSN `(placement ...)` section MUST list all components with their
positions and orientations matching the generated PCB.

**Acceptance criteria:**

1. Component count in DSN equals component count in the PCB.
2. Positions match the PCB placement.
3. Component sides (front/back) are correct.

---

### KE-0403  Net definitions in DSN

**Priority:** Must
**Source:** IR-2 netlist.

**Description:**
The DSN `(network ...)` section MUST define all nets with their
pin references matching IR-2's connectivity.

**Acceptance criteria:**

1. Every named net in IR-2 appears in the DSN network section.
2. Pin references use the format `<ref_des>-<pad_number>`.
3. Net classes with trace width and clearance rules are included.

---

### KE-0404  Design rules in DSN

**Priority:** Must
**Source:** IR-3 routing constraints.

**Description:**
The DSN file MUST include design rules (clearances, trace widths,
via dimensions) derived from IR-3's `routing_constraints`.

**Acceptance criteria:**

1. Default clearance rule is present.
2. Power net trace widths match IR-3 `power_traces.min_width_mm`.
3. Signal net trace widths match IR-3 `signal_traces.width_mm`.
4. Via dimensions match IR-3 `via_constraints`.

---

### KE-0405  Keep-out zones in DSN

**Priority:** Must
**Source:** IR-3 keepout zones.

**Description:**
Keep-out zones from IR-3 MUST be represented as wiring keep-outs in
the DSN structure section.

**Acceptance criteria:**

1. Each IR-3 keep-out appears as a keep-out region in the DSN file.
2. Keep-out boundaries match IR-3 coordinates.

---

## 7  Freerouter SES import

### KE-0500  Parse Specctra SES file

**Priority:** Must
**Source:** Freerouter output format.

**Description:**
The tool MUST parse a Freerouter-produced `.ses` (session) file and
extract routed wires and vias.

**Acceptance criteria:**

1. The tool successfully parses a valid SES file without errors.
2. Wire segments (layer, coordinates, width) are extracted.
3. Via instances (position, padstack) are extracted.

---

### KE-0501  Merge routing into PCB

**Priority:** Must
**Source:** Post-routing PCB completion.

**Description:**
The tool MUST merge routing from a SES file into an existing
`.kicad_pcb` file, adding `(segment ...)` and `(via ...)` nodes for
each routed wire and via.

**Acceptance criteria:**

1. The output PCB contains trace segments for all routed wires.
2. The output PCB contains via nodes for all placed vias.
3. Trace widths match the SES wire widths.
4. Via drill and diameter match the SES via padstack dimensions.
5. Net assignments on traces and vias are correct.
6. Existing PCB content (components, zones, board outline) is preserved.

---

### KE-0502  Routing completeness check

**Priority:** Should
**Source:** Design verification.

**Description:**
After SES import, the tool SHOULD report how many nets were
successfully routed and whether any remain unrouted.

**Acceptance criteria:**

1. A summary line reports: "N of M nets routed."
2. Unrouted nets are listed by name.

---

## 8  Manufacturing exports

### KE-0600  BOM CSV generation

**Priority:** Must
**Source:** JLCPCB assembly ordering.

**Description:**
The tool MUST generate a Bill of Materials CSV file with columns
suitable for JLCPCB SMT assembly.

**Acceptance criteria:**

1. CSV columns include at minimum: `Designator`, `Value`, `Footprint`,
   `Manufacturer`, `Part Number`, `LCSC Part Number`, `Quantity`.
2. Every component from IR-1e appears in the BOM.
3. Component values are taken from IR-2's netlist entries.
4. LCSC part numbers are taken from IR-1's `sourcing.lcsc_pn`.

---

### KE-0601  Pick-and-place CSV generation

**Priority:** Must
**Source:** JLCPCB assembly — component placement data.

**Description:**
The tool MUST generate a pick-and-place (CPL) CSV file with component
positions suitable for SMT assembly machines.

**Acceptance criteria:**

1. CSV columns: `Designator`, `Mid X`, `Mid Y`, `Layer`, `Rotation`.
2. Coordinates are in millimeters.
3. Positions match the PCB component placement.
4. Layer values are `Top` or `Bottom`.

---

### KE-0602  Gerber export via kicad-cli

**Priority:** Should
**Source:** PCB fabrication.

**Description:**
The tool SHOULD support invoking `kicad-cli` to export Gerber files
and drill files from a routed `.kicad_pcb` file.

**Acceptance criteria:**

1. Gerber files are generated for all copper, mask, silkscreen, and
   edge layers.
2. Excellon drill file is generated.
3. If `kicad-cli` is not available, the tool reports an error
   suggesting manual export from KiCad.

---

## 9  Determinism

### KE-0700  Deterministic output

**Priority:** Must
**Source:** Reproducible builds — identical IR → identical output.

**Description:**
Given identical IR files, the tool MUST produce bit-identical output
files across multiple runs on any platform.

**Acceptance criteria:**

1. Running the tool twice on the same IR directory produces files
   with identical content (verified by byte comparison or SHA-256).
2. No output depends on system time, random number generators, or
   platform-specific behavior.

---

### KE-0701  Deterministic UUID generation

**Priority:** Must
**Source:** KiCad requires UUIDs on all elements.

**Description:**
UUIDs for KiCad elements (symbols, pins, wires, labels) MUST be
generated deterministically from a seed derived from the IR content.

**Acceptance criteria:**

1. The same IR content produces the same UUIDs.
2. Different IR content produces different UUIDs.
3. UUIDs are valid UUID v4 format strings.
4. The seed is derived from a hash of the project name and IR content.

---

### KE-0702  Sorted collections

**Priority:** Must
**Source:** Output stability.

**Description:**
All collections in the output (component instances, net definitions,
library symbols, wire segments) MUST be sorted in a deterministic
order before serialization.

**Acceptance criteria:**

1. Library symbols are sorted by library-qualified name.
2. Component instances are sorted by reference designator
   (alphanumeric: C1 < C2 < J1 < R1 < U1).
3. Net definitions are sorted by net name.
4. Wire segments are sorted by their coordinate tuples.

---

### KE-0703  Fixed timestamps

**Priority:** Must
**Source:** Determinism.

**Description:**
Any timestamp embedded in output files (e.g., title block date) MUST
be a fixed value derived from IR content, not the system clock.

**Acceptance criteria:**

1. Title block date is a fixed deterministic value.
2. No `generator_version` or timestamp changes between runs with
   identical inputs.

---

## 10  CLI interface

### KE-0800  CLI binary

**Priority:** Must
**Source:** User interaction.

**Description:**
The crate MUST provide a CLI binary invokable as:
```
cargo run -p sonde-kicad -- <subcommand> [options]
```

**Acceptance criteria:**

1. The binary is defined in `crates/sonde-kicad/src/bin/`.
2. `--help` prints usage information.
3. Exit code 0 on success, non-zero on failure.

---

### KE-0801  Subcommands

**Priority:** Must
**Source:** Pipeline steps.

**Description:**
The CLI MUST support the following subcommands:

| Subcommand | Description |
|---|---|
| `schematic` | Generate `.kicad_sch` from IR files |
| `pcb` | Generate `.kicad_pcb` from IR files |
| `dsn` | Generate Specctra `.dsn` from IR files |
| `import-ses` | Merge Freerouter `.ses` routing into `.kicad_pcb` |
| `bom` | Generate BOM CSV from IR files |
| `cpl` | Generate pick-and-place CSV from IR files |
| `gerber` | Export Gerber via `kicad-cli` |
| `build` | Run full pipeline: schematic → PCB → DSN → BOM → CPL |

**Acceptance criteria:**

1. Each subcommand is functional and documented in `--help`.
2. Unknown subcommands produce an error message.

---

### KE-0802  Input directory argument

**Priority:** Must
**Source:** IR file location.

**Description:**
All subcommands that read IR files MUST accept an `--ir-dir <path>`
argument specifying the directory containing IR YAML files.

**Acceptance criteria:**

1. Default `--ir-dir` is the current working directory.
2. Non-existent directory produces a clear error.

---

### KE-0803  Output directory argument

**Priority:** Must
**Source:** Generated file location.

**Description:**
All subcommands that produce files MUST accept an `--output-dir <path>`
argument. The directory is created if it does not exist.

**Acceptance criteria:**

1. Output files are written to the specified directory.
2. Default is `./output/` relative to the IR directory.
3. The directory is created if missing.

---

## 11  Library API

### KE-0900  Public API

**Priority:** Must
**Source:** Composability — other tools and skills may call the API.

**Description:**
The crate MUST expose a public Rust API for each generation step,
independent of the CLI.

**Acceptance criteria:**

1. `load_ir(dir: &Path) -> Result<IrBundle, Error>` loads all IR files.
2. `emit_schematic(ir: &IrBundle) -> Result<String, Error>` returns
   the `.kicad_sch` content as a string.
3. `emit_pcb(ir: &IrBundle) -> Result<String, Error>` returns the
   `.kicad_pcb` content.
4. `emit_dsn(ir: &IrBundle) -> Result<String, Error>` returns DSN
   content.
5. `import_ses(pcb: &str, ses: &str) -> Result<String, Error>` merges
   routing.
6. `emit_bom_csv(ir: &IrBundle) -> Result<String, Error>` returns BOM.
7. `emit_cpl_csv(ir: &IrBundle) -> Result<String, Error>` returns CPL.

---

### KE-0901  Error types

**Priority:** Must
**Source:** Composability and debugging.

**Description:**
The library MUST use structured error types (not string errors) that
distinguish between:

- IR loading/parsing errors
- IR validation/cross-reference errors
- Generation errors (e.g., unresolvable symbol)
- SES parsing errors
- I/O errors

**Acceptance criteria:**

1. Error types implement `std::error::Error` and `Display`.
2. Each variant carries enough context to diagnose the problem
   (file name, field path, expected vs. actual value).

---

## 12  Embedded symbol definitions

### KE-1000  Vendored KiCad symbol shapes

**Priority:** Must
**Source:** Determinism — no dependency on host KiCad installation.

**Description:**
The tool MUST embed (vendor) the KiCad symbol shape definitions
needed for schematic generation. Symbol shapes for standard library
components (`Device:R`, `Device:C`, `Device:Q_PMOS_GSD`,
`Device:FerriteBead`, `Connector_Generic:Conn_01xNN`, `power:GND`,
etc.) are compiled into the binary or loaded from a vendored file
shipped with the crate.

**Acceptance criteria:**

1. The tool generates valid schematics without a KiCad installation.
2. Symbol pin counts, names, numbers, and electrical types match the
   KiCad 8 standard library.
3. If a required symbol is not vendored, the tool fails with a clear
   error naming the missing symbol.

---

### KE-1001  Extensible symbol registry

**Priority:** Should
**Source:** Support for components beyond the initial vendored set.

**Description:**
The tool SHOULD allow loading additional symbol definitions from
an external `.kicad_sym` file via an `--extra-symbols <path>` option.

**Acceptance criteria:**

1. Symbols from the extra file are available for schematic generation.
2. Extra symbols override vendored symbols if names conflict.

---

## 13  Footprint data

### KE-1100  Footprint pad data for DSN

**Priority:** Must
**Source:** DSN format requires pad geometry for routing.

**Description:**
The tool MUST have access to footprint pad data (pad positions,
sizes, shapes, layers) for generating DSN files. This data may be:

- Embedded in the binary for common footprints, OR
- Loaded from KiCad `.kicad_mod` footprint files via a
  `--footprint-dir <path>` option.

**Acceptance criteria:**

1. Every footprint referenced by IR-1e has pad data available.
2. If pad data is missing for a footprint, the tool fails with a
   clear error naming the footprint.
3. Pad positions, sizes, and layers are correct.

---

### KE-1101  Footprint file parser

**Priority:** Must
**Source:** KiCad `.kicad_mod` files.

**Description:**
The tool MUST be able to parse KiCad 8 `.kicad_mod` footprint files
to extract pad geometry, courtyard, and reference data.

**Acceptance criteria:**

1. Parser handles SMD pads, THT pads, and NPTH holes.
2. Pad shape types: `rect`, `roundrect`, `circle`, `oval` are
   supported.
3. Parser extracts: pad number, position, size, shape, layers, drill.

---

## 14  Non-functional requirements

### KE-1200  Crate organization

**Priority:** Must
**Source:** Sonde workspace conventions.

**Description:**
The crate MUST be located at `crates/sonde-kicad/` and added to the
workspace `Cargo.toml`.

**Acceptance criteria:**

1. `crates/sonde-kicad/Cargo.toml` exists with `name = "sonde-kicad"`.
2. The workspace `Cargo.toml` includes `sonde-kicad` in `members`.
3. SPDX headers on all `.rs` files.

---

### KE-1201  Dependencies

**Priority:** Must
**Source:** Sonde crate conventions.

**Description:**
Dependencies MUST be minimal and appropriate:

- `serde` + `serde_yaml` for YAML parsing.
- `sha2` for deterministic hashing (UUID seed).
- `clap` for CLI argument parsing.
- `uuid` for UUID formatting.
- `thiserror` for error types.

No dependency on KiCad installation for generation (only for
optional Gerber export via `kicad-cli`).

**Acceptance criteria:**

1. `cargo build -p sonde-kicad` succeeds.
2. `cargo clippy -p sonde-kicad -- -D warnings` passes.
3. No dependency on Python, kiutils, or KiCad C++ libraries.

---

### KE-1202  Performance

**Priority:** May
**Source:** Developer experience.

**Description:**
The tool MAY complete generation for a typical board (15 components,
10 nets) in under 1 second.

**Acceptance criteria:**

1. `time cargo run -p sonde-kicad -- build --ir-dir hw/carrier-board/ir/`
   completes in under 2 seconds (including cargo overhead).
