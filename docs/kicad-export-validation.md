<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# KiCad Export Tool Validation Specification (`sonde-kicad`)

> **Document status:** Draft
> **Scope:** Test plan for the `sonde-kicad` crate.
> **Audience:** Implementers (human or LLM agent) writing tests.
> **Related:** [kicad-export-requirements.md](kicad-export-requirements.md),
>   [kicad-export-design.md](kicad-export-design.md)

---

## 1  Overview

All tests in this document are pure Rust `#[test]` tests, including
file-fixture-based integration tests. No KiCad installation is required for unit or
integration tests. KiCad-dependent tests (ERC, DRC, Gerber export)
are gated behind a `kicad` feature flag or `#[ignore]` attribute.

There are 63 test cases organized into 9 sections.

### Test data

Tests use the carrier board IR files at
`hw/carrier-board/ir/` as the primary test fixture. Minimal synthetic
IR files are constructed inline for edge-case unit tests.

---

## 2  IR loading tests

### T-KE-001  Load complete IR directory

**Validates:** KE-0100, KE-0101

**Procedure:**
1. Call `load_ir("hw/carrier-board/ir/")`.
2. Assert: `IrBundle` is returned with all fields populated.
3. Assert: `ir1e.components.len() == 15`.
4. Assert: `ir2.nets.len() == 9` (VBAT, VBAT_FILT, SENSOR_V,
   SENSOR_EN, SDA, SCL, 1W_DATA, VBAT_SENSE, GND).
5. Assert: `ir2.netlist.len() == 15`.
6. Assert: `ir3.is_some()` and `ir3.board.width_mm == 25.0`.

---

### T-KE-002  Missing required IR file

**Validates:** KE-0101, KE-0104

**Procedure:**
1. Create a temp directory with only `IR-2.yaml` (missing IR-1e).
2. Call `load_ir(temp_dir)`.
3. Assert: returns `Error::MissingIrFile("IR-1e.yaml")`.

---

### T-KE-003  Schema version validation

**Validates:** KE-0102

**Procedure:**
1. Create an IR-1e YAML with `schema_version: "2.0.0"`.
2. Call `load_ir`.
3. Assert: returns `Error::SchemaVersion { found: "2.0.0", expected: "1.0.0" }`.

---

### T-KE-004  Malformed YAML

**Validates:** KE-0104

**Procedure:**
1. Create an IR-1e YAML with invalid syntax (unclosed quote).
2. Call `load_ir`.
3. Assert: returns `Error::YamlParse` with file name in the message.

---

### T-KE-005  Cross-validation: ref_des mismatch

**Validates:** KE-0103

**Procedure:**
1. Create IR-1e with components [R1, R2].
2. Create IR-2 with netlist referencing [R1, R2, R3].
3. Call `load_ir` and then `validate_cross_references`.
4. Assert: returns error mentioning R3 is in IR-2 but not IR-1e.

---

### T-KE-006  Cross-validation: MISSING library status

**Validates:** KE-0103

**Procedure:**
1. Create IR-1e with one component having `library_status: "MISSING"`.
2. Call `validate_cross_references`.
3. Assert: returns error naming the component with MISSING status.

---

### T-KE-007  IR-1e component deserialization

**Validates:** KE-0100

**Procedure:**
1. Load carrier board IR-1e.
2. Assert: `Q1.kicad_symbol == "Device:Q_PMOS_GSD"`.
3. Assert: `Q1.kicad_footprint == "Package_TO_SOT_SMD:SOT-23"`.
4. Assert: `Q1.courtyard_mm.width == 3.84`.
5. Assert: `R1.kicad_symbol == "Device:R"`.
6. Assert: `J1.kicad_footprint` contains `"JST_SH"`.

---

### T-KE-008  IR-2 net deserialization

**Validates:** KE-0100

**Procedure:**
1. Load carrier board IR-2.
2. Assert: net "VBAT" has type `Power`.
3. Assert: net "SDA" has type `Signal`.
4. Assert: net "VBAT" has `power_source` = `"IR-PB/VBAT"`.
5. Assert: `functional_groups[0].name == "power-input"`.
6. Assert: `functional_groups[0].components` contains "J3", "FB1", "C1".

---

### T-KE-009  IR-3 board deserialization

**Validates:** KE-0100

**Procedure:**
1. Load carrier board IR-3.
2. Assert: `board.width_mm == 25.0`.
3. Assert: `board.height_mm == 35.0`.
4. Assert: `board.layers == 2`.
5. Assert: `connector_placement.len() == 5` (J1, J2, J3, J6, J7).
6. Assert: J1 position is `(0.0, 22.0)`.

---

## 3  S-expression tests

### T-KE-010  Atom serialization

**Validates:** Design §5.2

**Procedure:**
1. Create `SExpr::Atom("version".into())`.
2. Serialize.
3. Assert: output is `version`.

---

### T-KE-011  Quoted string serialization

**Validates:** Design §5.2

**Procedure:**
1. Create `SExpr::Quoted("Device:R".into())`.
2. Serialize.
3. Assert: output is `"Device:R"`.

---

### T-KE-012  List serialization

**Validates:** Design §5.2

**Procedure:**
1. Create `SExpr::List(vec![Atom("version"), Atom("20231120")])`.
2. Serialize.
3. Assert: output is `(version 20231120)`.

---

### T-KE-013  Nested list indentation

**Validates:** Design §5.3

**Procedure:**
1. Create a nested structure: `(kicad_sch (version 20231120))`.
2. Serialize with indentation.
3. Assert: output has proper indentation with 2-space indent.

---

### T-KE-014  Float formatting

**Validates:** Design §5.2

**Procedure:**
1. Serialize `SExpr::Atom("100.330000")` and `SExpr::Atom("0.5")`.
2. Assert: trailing zeros are trimmed appropriately.
3. Assert: at least one decimal place is preserved.

---

### T-KE-015  S-expression parser round-trip

**Validates:** Design §5.4

**Procedure:**
1. Create a known S-expression string.
2. Parse it into `SExpr`.
3. Serialize back.
4. Assert: output matches the original (modulo whitespace normalization).

---

### T-KE-016  Parser handles quoted strings with escapes

**Validates:** Design §5.4

**Procedure:**
1. Parse `(property "Value" "4.7k\\u03A9")`.
2. Assert: the quoted string value is `4.7k\u03A9`.

---

## 4  Schematic generation tests

### T-KE-020  Schematic root structure

**Validates:** KE-0200

**Procedure:**
1. Generate schematic from carrier board IR.
2. Parse the output as S-expression.
3. Assert: root node is `kicad_sch`.
4. Assert: `(version 20231120)` is present.
5. Assert: `(generator "sonde-kicad")` is present.
6. Assert: `(lib_symbols ...)` block is present.

---

### T-KE-021  All components present

**Validates:** KE-0202

**Procedure:**
1. Generate schematic from carrier board IR.
2. Count top-level `(symbol ...)` nodes (excluding lib_symbols).
3. Assert: count == 15 (matching IR-1e component count).

---

### T-KE-022  Component properties

**Validates:** KE-0202

**Procedure:**
1. Generate schematic from carrier board IR.
2. Find the symbol instance for R1.
3. Assert: `lib_id` == `"Device:R"`.
4. Assert: `Reference` property == `"R1"`.
5. Assert: `Value` property == `"4.7kΩ"`.
6. Assert: `Footprint` property == `"Resistor_SMD:R_0402_1005Metric"`.

---

### T-KE-023  Library symbols declared

**Validates:** KE-0201

**Procedure:**
1. Generate schematic from carrier board IR.
2. Collect all `lib_id` values from symbol instances.
3. For each `lib_id`, assert: a matching `(symbol ...)` exists in
   `lib_symbols`.

---

### T-KE-024  Net labels for signal nets

**Validates:** KE-0203

**Procedure:**
1. Generate schematic from carrier board IR.
2. Find all `(label ...)` nodes.
3. Assert: label "SDA" appears at least 3 times (R1, J1, J6).
4. Assert: label "SCL" appears at least 3 times (R2, J1, J6).

---

### T-KE-025  Power symbols for GND

**Validates:** KE-0204

**Procedure:**
1. Generate schematic from carrier board IR.
2. Find symbol instances with `lib_id` containing `"power:GND"` or
   equivalent power port nodes.
3. Assert: at least 8 GND connections exist (matching IR-2 GND pins).

---

### T-KE-026  NC pins marked

**Validates:** KE-0203

**Procedure:**
1. Generate schematic from carrier board IR.
2. Find `(no_connect ...)` nodes.
3. Assert: at least 7 no-connect markers (for J6.4, J7.1–J7.5, J7.7).

---

### T-KE-027  Title block

**Validates:** KE-0206

**Procedure:**
1. Generate schematic from carrier board IR.
2. Find `(title_block ...)` node.
3. Assert: `(title "sonde-carrier")`.
4. Assert: `(comment 1 ...)` contains "sonde-kicad".

---

### T-KE-028  Wire segments present

**Validates:** KE-0207

**Procedure:**
1. Generate schematic from carrier board IR.
2. Count `(wire ...)` nodes.
3. Assert: wire count > 0 (at least one wire per connected pin).

---

### T-KE-029  Functional group separation

**Validates:** KE-0205

**Procedure:**
1. Generate schematic from carrier board IR.
2. Collect positions of components in the "power-input" group
   (J3, FB1, C1) and the "i2c-interface" group (J1, R1, R2).
3. Compute the centroid of each group.
4. Assert: the centroids are separated by at least 20mm.

---

### T-KE-030  Custom power net labels

**Validates:** KE-0204

**Procedure:**
1. Generate schematic from carrier board IR.
2. Assert: net names "VBAT", "VBAT_FILT", "SENSOR_V" appear as
   global labels or power port instances with power flags.
3. Assert: these nets do not trigger "power pin not driven" in
   ERC-like validation.

---

## 5  PCB generation tests

### T-KE-031  PCB root structure

**Validates:** KE-0300

**Procedure:**
1. Generate PCB from carrier board IR.
2. Parse as S-expression.
3. Assert: root node is `kicad_pcb`.
4. Assert: `(version 20231120)` is present.
5. Assert: `(generator "sonde-kicad")` is present.

---

### T-KE-032  Board outline

**Validates:** KE-0301

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find `(gr_line ...)` nodes on `Edge.Cuts` layer.
3. Assert: 4 lines forming a closed rectangle.
4. Assert: rectangle dimensions are 25.0 × 35.0 mm.

---

### T-KE-033  Layer definitions

**Validates:** KE-0302

**Procedure:**
1. Generate PCB from carrier board IR (2-layer board).
2. Find `(layers ...)` node.
3. Assert: F.Cu (id 0) and B.Cu (id 31) are defined as `signal`.
4. Assert: F.SilkS, B.SilkS, F.Mask, B.Mask, Edge.Cuts are present.

---

### T-KE-034  Connector placement coordinates

**Validates:** KE-0303

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find footprint for J1 (Qwiic connector).
3. Assert: J1 position x == 0.0, y == 35.0 - 22.0 = 13.0 (after
   Y-axis transform).
4. Find footprint for J3 (battery connector).
5. Assert: J3 position x == 0.0, y == 35.0 - 10.0 = 25.0.

---

### T-KE-035  Zone-placed component bounds

**Validates:** KE-0303

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find footprints for power-gating zone (Q1, R4, C2).
3. Assert: all positions are within the zone's anchor + extent
   (anchor 5.0,14.0; extent 6.0×6.0 → after Y-transform).

---

### T-KE-036  Net definitions

**Validates:** KE-0305

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find all `(net ...)` definitions.
3. Assert: net "VBAT" exists.
4. Assert: net "GND" exists.
5. Assert: net "SDA" exists.
6. Assert: total net count == 10 (9 named + 1 empty unconnected net).

---

### T-KE-037  Keep-out zone

**Validates:** KE-0306

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find zone with `(keepout ...)`.
3. Assert: antenna-clearance zone exists.
4. Assert: boundary matches IR-3 coordinates (after Y-transform).
5. Assert: layer is "F.Cu".

---

### T-KE-038  No routed traces

**Validates:** KE-0310

**Procedure:**
1. Generate PCB from carrier board IR.
2. Count `(segment ...)` nodes.
3. Assert: count == 0.
4. Count `(via ...)` nodes (excluding keep-out vias).
5. Assert: count == 0.

---

### T-KE-039  Ground plane zone

**Validates:** KE-0308

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find zone on "B.Cu" assigned to "GND" net.
3. Assert: zone polygon covers the full board outline.
4. Assert: `(fill yes ...)` is set.

---

### T-KE-040  Net class definitions

**Validates:** KE-0307

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find `(net_class ...)` definitions or `(setup ...)` section.
3. Assert: power net class has width ≥ 0.5mm.
4. Assert: default signal net class has width ≥ 0.25mm.

---

### T-KE-041  Silkscreen text

**Validates:** KE-0309

**Procedure:**
1. Generate PCB from carrier board IR.
2. Find `(gr_text ...)` nodes on "F.SilkS" layer.
3. Assert: text "sonde" is present.
4. Assert: text "J1 I2C" or similar connector labels are present.

---

### T-KE-042  Component footprint count

**Validates:** KE-0304

**Procedure:**
1. Generate PCB from carrier board IR.
2. Count `(footprint ...)` nodes.
3. Assert: count == 15 (matching IR-1e).

---

## 6  DSN generation tests

### T-KE-043  DSN root structure

**Validates:** KE-0400

**Procedure:**
1. Generate DSN from carrier board IR.
2. Assert: output starts with `(pcb "sonde-carrier.dsn"`.
3. Assert: contains `(parser ...)`, `(resolution ...)`, `(unit um)`.
4. Assert: contains `(structure ...)`, `(placement ...)`,
   `(library ...)`, `(network ...)`.

---

### T-KE-044  DSN board boundary

**Validates:** KE-0401

**Procedure:**
1. Generate DSN from carrier board IR.
2. Find `(boundary ...)` in `(structure ...)`.
3. Assert: boundary polygon defines a 25mm × 35mm rectangle
   (in µm: 25000 × 35000).

---

### T-KE-045  DSN component placement

**Validates:** KE-0402

**Procedure:**
1. Generate DSN from carrier board IR.
2. Find `(placement ...)` section.
3. Count total `(place ...)` nodes.
4. Assert: count == 15.
5. Assert: R1 position matches PCB placement (in µm).

---

### T-KE-046  DSN net definitions

**Validates:** KE-0403

**Procedure:**
1. Generate DSN from carrier board IR.
2. Find `(network ...)` section.
3. Assert: `(net VBAT (pins J3-1 FB1-1 Q1-2 R4-2))` exists
   (pin ordering may vary).
4. Assert: all 9 named nets are present.

---

### T-KE-047  DSN design rules

**Validates:** KE-0404

**Procedure:**
1. Generate DSN from carrier board IR.
2. Find `(rule ...)` in `(structure ...)`.
3. Assert: default width ≥ 250 (µm, 0.25mm).
4. Assert: default clearance ≥ 200 (µm, 0.2mm).
5. Find power net class.
6. Assert: power width ≥ 500 (µm, 0.5mm).

---

### T-KE-048  DSN keep-out

**Validates:** KE-0405

**Procedure:**
1. Generate DSN from carrier board IR.
2. Find `(keepout ...)` in `(structure ...)`.
3. Assert: antenna-clearance keep-out exists with correct boundary.

---

### T-KE-049  DSN via definition

**Validates:** KE-0404

**Procedure:**
1. Generate DSN from carrier board IR.
2. Find `(via ...)` in `(structure ...)`.
3. Assert: via padstack name includes "600:300" (0.6mm/0.3mm drill).

---

## 7  SES import tests

### T-KE-050  Parse minimal SES file

**Validates:** KE-0500

**Procedure:**
1. Create a minimal SES file with one routed wire and one via:
   ```
   (session "test.ses"
     (routes
       (resolution um 10)
       (network_out
         (net VBAT
           (wire (path F.Cu 5000 0 0 10000 0))
         )
         (net GND
           (via "Via[0-1]_600:300_um" 5000 5000)
         )
       )
     )
   )
   ```
2. Parse the SES file.
3. Assert: 1 wire extracted (net VBAT, F.Cu, width 5000µm,
   from (0,0) to (10000,0)).
4. Assert: 1 via extracted (net GND, at (5000,5000)).

---

### T-KE-051  Merge SES into PCB

**Validates:** KE-0501

**Procedure:**
1. Generate a PCB from carrier board IR (no traces).
2. Create a SES file with mock routing for 2 nets.
3. Call `import_ses(pcb_content, ses_content)`.
4. Parse the result.
5. Assert: original footprints are preserved.
6. Assert: new `(segment ...)` nodes are present.
7. Assert: new `(via ...)` nodes are present.

---

### T-KE-052  SES coordinate conversion

**Validates:** KE-0501

**Procedure:**
1. Create a SES wire with path `(F.Cu 5000 10000 20000 10000 30000)`.
2. Import into PCB.
3. Find the corresponding `(segment ...)`.
4. Assert: start == (10.0, 20.0) mm, end == (10.0, 30.0) mm.
5. Assert: width == 5.0 mm.

---

### T-KE-053  SES preserves existing PCB content

**Validates:** KE-0501

**Procedure:**
1. Generate a PCB with board outline, footprints, zones.
2. Import a SES file.
3. Assert: board outline `(gr_line ...)` nodes are unchanged.
4. Assert: `(footprint ...)` count is unchanged.
5. Assert: `(zone ...)` definitions are unchanged.

---

### T-KE-054  Routing completeness report

**Validates:** KE-0502

**Procedure:**
1. Generate a PCB with 9 named nets.
2. Create a SES with routing for only 5 nets.
3. Import and check the report.
4. Assert: report says "5 of 9 nets routed."
5. Assert: 4 unrouted net names are listed.

---

## 8  Manufacturing export tests

### T-KE-055  BOM CSV content

**Validates:** KE-0600

**Procedure:**
1. Generate BOM from carrier board IR.
2. Parse as CSV.
3. Assert: header row contains `Designator,Value,Footprint`.
4. Assert: 15 data rows (one per component).
5. Assert: R1 row has `Value` == `"4.7kΩ"`.
6. Assert: R1 row has `Footprint` ==
   `"Resistor_SMD:R_0402_1005Metric"`.

---

### T-KE-056  BOM sorted by designator

**Validates:** KE-0600, KE-0702

**Procedure:**
1. Generate BOM from carrier board IR.
2. Parse as CSV, extract Designator column.
3. Assert: designators are sorted: C1, C2, C3, FB1, J1, J2, J3,
   J6, J7, Q1, R1, R2, R3, R4, R5, R6.

---

### T-KE-057  CPL CSV content

**Validates:** KE-0601

**Procedure:**
1. Generate CPL from carrier board IR.
2. Parse as CSV.
3. Assert: header row contains `Designator,Mid X,Mid Y,Layer,Rotation`.
4. Assert: 15 data rows.
5. Assert: all `Layer` values are `"Top"` or `"Bottom"`.
6. Assert: `Mid X` and `Mid Y` are valid float numbers.

---

### T-KE-058  CPL coordinates match PCB

**Validates:** KE-0601

**Procedure:**
1. Generate PCB and CPL from carrier board IR.
2. For R1: extract position from PCB footprint and from CPL row.
3. Assert: positions match (within 0.01mm tolerance).

---

## 9  Determinism tests

### T-KE-059  Bit-identical schematic output

**Validates:** KE-0700

**Procedure:**
1. Generate schematic from carrier board IR.
2. Generate schematic again from the same IR.
3. Assert: the two outputs are byte-identical.

---

### T-KE-060  Bit-identical PCB output

**Validates:** KE-0700

**Procedure:**
1. Generate PCB from carrier board IR.
2. Generate PCB again from the same IR.
3. Assert: the two outputs are byte-identical.

---

### T-KE-061  UUID determinism

**Validates:** KE-0701

**Procedure:**
1. Create a `UuidGenerator` with a known project name and hash.
2. Call `next("symbol:R1")`.
3. Call `next("symbol:R1")` again on a fresh generator with the
   same seed.
4. Assert: both calls return the same UUID.
5. Call `next("symbol:R2")`.
6. Assert: R2's UUID differs from R1's UUID.

---

### T-KE-062  UUID format

**Validates:** KE-0701

**Procedure:**
1. Generate a UUID.
2. Assert: matches regex
   `^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`.
3. Assert: version nibble is 4.
4. Assert: variant bits are correct (10xx).

---

## 10  Integration tests (optional, requires KiCad 8)

These tests are gated behind `#[ignore]` and require KiCad 8
installed on the system.

### T-KE-I01  ERC validation

**Validates:** KE-0200, KE-0201, KE-0203, KE-0204

**Procedure:**
1. Generate schematic from carrier board IR.
2. Write to a temp file.
3. Run `kicad-cli sch erc <file> --exit-code-violations --format json`.
4. Assert: exit code 0 (no violations).

---

### T-KE-I02  DRC validation (post-routing)

**Validates:** KE-0300, KE-0501

**Procedure:**
1. Generate PCB from carrier board IR.
2. Import a mock SES with complete routing.
3. Write to a temp file.
4. Run `kicad-cli pcb drc <file> --exit-code-violations --format json`.
5. Assert: exit code 0 (no violations).

---

### T-KE-I03  Gerber export

**Validates:** KE-0602

**Procedure:**
1. Generate a routed PCB.
2. Call the `gerber` subcommand.
3. Assert: Gerber files exist in the output directory.
4. Assert: at least files for F.Cu, B.Cu, Edge.Cuts are present.

---

### T-KE-I04  Freerouter round-trip

**Validates:** KE-0400, KE-0500, KE-0501

**Procedure:**
1. Generate PCB and DSN from carrier board IR.
2. Run Freerouter on the DSN (requires Freerouter installed).
3. Import the resulting SES.
4. Assert: output PCB contains routed traces.
5. Assert: all nets are routed (0 unrouted reported).

---

## 11  Test matrix summary

| Section | Test IDs | Count | KiCad required? |
|---|---|---|---|
| IR loading | T-KE-001 – T-KE-009 | 9 | No |
| S-expression | T-KE-010 – T-KE-016 | 7 | No |
| Schematic | T-KE-020 – T-KE-030 | 11 | No |
| PCB | T-KE-031 – T-KE-042 | 12 | No |
| DSN | T-KE-043 – T-KE-049 | 7 | No |
| SES import | T-KE-050 – T-KE-054 | 5 | No |
| Manufacturing | T-KE-055 – T-KE-058 | 4 | No |
| Determinism | T-KE-059 – T-KE-062 | 4 | No |
| Integration | T-KE-I01 – T-KE-I04 | 4 | Yes |
| **Total** | | **63** | |

---

## 12  Running tests

```bash
# Run all non-KiCad tests
cargo test -p sonde-kicad

# Run a single test
cargo test -p sonde-kicad t_ke_001

# Run integration tests (requires KiCad 8 in PATH)
cargo test -p sonde-kicad -- --ignored
```
