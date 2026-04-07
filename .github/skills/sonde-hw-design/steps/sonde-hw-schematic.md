# Phases 4–5: Schematic Design & Audit — Methodology

This file contains the schematic-design and schematic-compliance-audit
protocols for Phases 4–5 of the Sonde hardware design workflow. The agent
should read this file when entering Phase 4.

---

## Protocol: Schematic Design

Apply this protocol when designing a circuit schematic from selected
components and requirements. The goal is to produce a complete, correct,
and visually readable KiCad schematic file (`.kicad_sch` S-expression
format) that includes all supporting circuitry derived from component
datasheets. Execute all phases in order.

**If datasheet access is unavailable**: Do not fabricate reference
circuit values. Ask the user to provide datasheet excerpts or mark
derived values as `[UNVERIFIED — datasheet not consulted]`. Typical
values given throughout this protocol are illustrative examples — they
must be confirmed against the actual datasheet before use.

### Phase 1: Input Validation

1. **Component inventory**: For each selected component, confirm:
   - Full part number and manufacturer
   - Package/footprint
   - Operating voltage range
   - Pin count and pinout
   - Required interfaces

2. **Inter-component connections**: From the compatibility matrix, extract
   which components connect to which, voltage domains, level shifting needs.

3. **Power source identification**: Identify the primary power source and
   voltage rails required by all components.

4. **Missing information**: Stop and request any missing data before
   proceeding.

### Phase 2: Power Architecture Design

Design the complete power delivery network.

1. **Power tree design**: Map source → regulation → rail → consumers.
   For each rail: nominal voltage, tolerance, maximum current.

   **Sonde-specific**: Design TWO power domains:
   - **Always-on rail** (VDD_AO): Powers MCU module, RTC, and any
     component that must retain state during deep sleep. Fed from
     battery through reverse polarity protection and LDO/regulator.
   - **Gated rail** (VDD_SW): Powers sensors, LEDs, and all
     peripherals that can be fully shut off. Controlled by a GPIO
     through a P-MOSFET or load switch. When off, this rail MUST
     draw ≤ 1 µA leakage.

2. **Regulator selection and passive calculation**: For each stage:
   - **LDO regulators**: Input/output caps per datasheet, enable pin.
     For Sonde, select LDOs with ≤ 1 µA quiescent current (e.g.,
     TLV733P, XC6220, ME6211).
   - **Switching regulators**: Only if dropout or efficiency requires
     it. Calculate inductor, caps, feedback divider per datasheet.
   - **Cite datasheet section** for every passive value.

3. **Power gating circuit** (Sonde-critical):
   - P-MOSFET gate driven by MCU GPIO (active-low enable)
   - Gate pull-up resistor to VDD_AO (ensures rail is OFF at boot
     before MCU configures GPIO)
   - Optional: dedicated load switch IC (e.g., TPS22918) for lower
     Ron and controlled slew rate
   - Series capacitor on gated rail output for inrush current limiting
     if needed
   - Verify: when gated rail is off and MCU I/O pins are in deep-sleep
     state, no current leaks through I2C/SPI pull-ups or ESD clamps
     into the unpowered rail

4. **Battery management** (if rechargeable):
   - Charge controller, battery protection, power path management.

5. **Power sequencing**: If any components require sequenced power-on,
   design enable pin chaining. Verify reset is held until rails are stable.

### Phase 3: Supporting Circuitry Design

For each selected component, derive support circuits from its datasheet.

1. **Decoupling capacitors**: 100nF ceramic per VDD pin, bulk cap per
   domain. Voltage rating ≥ 1.5× rail voltage.

2. **Crystal/oscillator circuits** (if required): Load caps calculated
   from crystal spec and PCB stray capacitance.

3. **Reset circuits**: RC delay or voltage supervisor. Reset release
   timing must meet IC minimum.

4. **Boot/strap pin configuration**: Pull-up/pull-down resistors for
   desired boot mode. Document selected configuration.

5. **Pull-up/pull-down resistors**: I2C pull-ups (2.2kΩ–10kΩ), GPIO
   pull-ups (10kΩ). **For Sonde**: I2C pull-ups on the gated rail MUST
   connect to VDD_SW (not VDD_AO), so they draw zero current when the
   sensor bus is powered off.

6. **Analog signal conditioning** (if applicable): Voltage dividers,
   anti-aliasing filters, reference voltage circuits.

### Phase 4: Signal Routing Design

1. **I2C bus design**: SDA/SCL with pull-ups to VDD_SW. Check for
   address conflicts. I2C mux if needed.

2. **SPI bus design**: MOSI/MISO/CLK shared, unique CS per device.
   Pull-ups on unused CS lines.

3. **UART connections**: TX→RX crossover. Programming/debug UART
   accessible via header.

4. **USB connections** (if applicable): D+/D- through ESD to
   transceiver. Series resistors, pull-ups, VBUS sensing.

5. **GPIO and control signals**:
   - LED indicators with current-limiting resistors. For Sonde:
     LEDs on VDD_SW (gated) unless a power indicator is needed.
   - Power gate enable signal from MCU GPIO.
   - Sensor interrupt lines (if applicable).

6. **Test points**: Every power rail (VDD_AO, VDD_SW, VBAT), reset,
   programming/debug UART, gate enable signal.

### Phase 5: Protection Circuit Design

1. **ESD protection**: TVS on every external connector (USB, sensor
   headers, debug port). Clamping voltage below IC absolute max.

2. **Reverse polarity protection** on battery input: P-MOSFET
   (preferred for low drop) or series Schottky.

3. **Overcurrent protection**: PTC fuse on battery input.

4. **Overvoltage protection** (if input voltage could exceed regulator
   max): TVS or Zener clamping.

### Phase 6: Net Naming and Annotation

1. **Power nets**: `+VBAT`, `+3V3` (always-on), `+3V3_SW` (gated),
   `GND`.

2. **Signal nets**: `I2C0_SDA`, `I2C0_SCL`, `SPI0_MOSI`, `UART0_TX`,
   `LED_STATUS`, `GATE_EN`, `SENSOR_INT`, etc.

3. **Reference designators**: U (ICs), R (resistors), C (caps),
   D (diodes), J (connectors), Q (transistors), F (fuses),
   TP (test points). Sequential within functional groups.

4. **Annotations**: Voltage values near power symbols, critical signal
   constraints, datasheet references for passive values.

### Phase 7: Schematic Organization

1. **Single-sheet vs hierarchical**: Single sheet for < 50 components.
   Hierarchical for larger designs (power, MCU, sensors, connectors).

2. **Functional grouping**: IC + decoupling + pull-ups together.
   Power section separate from signal section. Connectors at edges.

3. **Inter-sheet connections**: Hierarchical labels for cross-sheet
   signals. Power symbols for rails.

4. **Title block**: Project name, sheet title, revision, date.

5. **Page boundary and placement area**: All component origins MUST
   fall within the page drawing area with a minimum margin of 25mm
   from all page borders. For A4 (297×210mm): usable area is
   x: 25–272, y: 25–185. Verify the page size can accommodate all
   components at required spacing. If not, use a larger page or
   hierarchical sheets.

### Phase 8: KiCad Schematic Generation

Generate `.kicad_sch` S-expression file(s).

#### 8.0: Approach-Level Gate (BEFORE writing any code)

Before writing any schematic generation code or helper functions:

1. **Verify one symbol against the dimension table.** Write the
   simplest symbol (e.g., resistor) with graphical body and pins.
   Check: body ≥ 2.032mm, pin span ≥ 7.62mm, pin length ≥ 1.27mm,
   `_0_1` sub-symbol exists with a graphical primitive. Fix before
   proceeding — every subsequent symbol inherits the same patterns.

2. **Verify placement fits the page.** Compute bounding boxes per
   §8.2 rule 9. Sum extent + gaps + 25mm margins. Verify it fits
   the page. Choose a larger page or hierarchical sheets NOW.

3. **Check for existing code divergence.** If reusing any existing
   symbol code, compare its dimensions against the §8.3 table
   BEFORE using it. If any dimension is smaller, the existing code
   is non-conforming — do NOT use it as-is.

#### 8.1: S-Expression Structure

```
(kicad_sch
  (version 20231120)
  (generator "promptkit")
  (uuid "<random-uuid>")
  (paper "A4")
  (title_block ...)
  (lib_symbols ...)       ;; Full symbol definitions with graphical body
  (symbol ...)            ;; Component instances
  (wire ...)              ;; Wires
  (label ...)             ;; Net labels
  (global_label ...)      ;; Inter-sheet labels
  (no_connect ...)        ;; NC markers
  (junction ...)          ;; Wire junctions
)
```

**Critical: `lib_symbols` graphical body requirement.** KiCad schematic
files are self-contained — the `(lib_symbols ...)` section MUST embed
the **complete symbol definition** for every symbol used, including
graphical primitives that make the symbol visible. A symbol with only
pin definitions but no graphical body will produce a schematic that
passes ERC and has correct connectivity but **renders as completely
empty** when opened in KiCad.

Each symbol uses a **two-level sub-symbol structure**:

```
(symbol "<LibName>:<PartName>"
  (in_bom yes) (on_board yes)
  (property "Reference" "R" (at ...) (effects ...))
  (property "Value" "R" (at ...) (effects ...))

  ;; Sub-symbol _0_1: GRAPHICAL BODY (what makes it visible)
  (symbol "<LibName>:<PartName>_0_1"
    (rectangle (start <x1> <y1>) (end <x2> <y2>)
      (stroke (width 0) (type default))
      (fill (type none)))
  )

  ;; Sub-symbol _1_1: PINS (what defines connectivity)
  (symbol "<LibName>:<PartName>_1_1"
    (pin <elec_type> <gfx_style> (at <x> <y> <angle>) (length <len>)
      (name "<name>" (effects (font (size 1.27 1.27))))
      (number "<num>" (effects (font (size 1.27 1.27)))))
  )
)
```

**Both sub-symbols are required.** The `_0_1` sub-symbol contains the
visual body (at minimum one `(rectangle ...)` for ICs or one
`(polyline ...)` for passives). The `_1_1` sub-symbol contains pin
definitions. Omitting the `_0_1` sub-symbol produces an invisible
component.

**Pin coordinate rule:** Pin `(at ...)` coordinates are relative to
the symbol origin (0, 0). Pin angles: 0 = extends left (pin end on
right), 90 = extends down (pin end on top), 180 = extends right (pin
end on left), 270 = extends up (pin end on bottom).

#### 8.2: Visual Layout Rules (MANDATORY)

1. **Grid alignment**: ALL coordinates snap to 2.54mm grid.
2. **Component spacing**: Minimum 20.32mm between component bodies.
3. **Signal flow**: Left-to-right. Power top-to-bottom.
4. **Component orientation**: Pin 1 top-left for ICs. Conventional
   orientation for passives, diodes, connectors.
5. **Wire routing**:
   - **Every pin MUST have a wire segment** — net labels and power
     symbols alone create electrical connections but are visually
     unreadable without wire stubs. A pin with only a label placed
     directly on its endpoint is prohibited.
   - **Intra-block wires vs. inter-block labels**: Within a
     functional block (IC + its decoupling caps, pull-ups, passives),
     components sharing a net MUST be connected by **direct wires**.
     The reader should trace the circuit within a block without
     reading label text. **Net labels are for inter-block connections
     only** — signals crossing between functional blocks.

   **Negative example — what BAD connectivity looks like:**
   ```
   BAD: Labels on pins, no wires between nearby components
   ┌──────┐              ┌──────┐
   │  IC  ├─VDD          │  C1  ├─VDD    (label-only, no wire)
   └──────┘              └──────┘

   GOOD: Direct wire within block, label for inter-block
   ┌──────┐    wire     ┌──────┐
   │  IC  ├────────────┤  C1  │   (intra-block: direct wire)
   └──┬───┘             └──────┘
      ├──VDD_SENSOR               (inter-block: label on stub)
   ```
   If your schematic looks like BAD, you have violated this rule.

   - Orthogonal only. Endpoints MUST align with pin
     endpoints exactly. Junction dots where 3+ wires meet.
     Never route through component bodies.
6. **Label placement**: Net labels MUST be placed on short wire stubs
   (2.54mm–5.08mm) extending from pins — NEVER directly on pin
   endpoints without a wire. Labels overlapping pin endpoints are
   unreadable. Power symbols on vertical stubs above/below connection
   point.
7. **No-connect markers**: On every intentionally unconnected pin.
8. **Power flags**: PWR_FLAG on every power net driven by a regulator
   or connector pin.

9. **Layout composition algorithm**: To systematically place components:

   **Step 1 — Define functional blocks.** Group all components into
   blocks (e.g., "Power Input", "Regulation", "MCU", "Sensor 1").
   Each block = primary IC/connector + supporting passives.

   **Step 2 — Compute each block's bounding box.**
   - Width = max(IC width, horizontal passives × 10.16mm) +
     2 × stub (5.08mm) + 2 × label margin (7.62mm)
   - Height = (pin rows × 2.54mm) + (stacked passives × 10.16mm) +
     2 × stub (5.08mm)
   - Minimum: 25.4mm × 25.4mm

   **Step 3 — Arrange blocks in a grid.**
   - Left column: power input, battery, protection
   - Center column: regulation, MCU/controller
   - Right column: peripherals, sensors, output connectors
   - Inter-block gap: max(20.32mm, tallest label × 1.27mm + 5.08mm)

   **Step 4 — Determine page size.**

   | Components | Page |
   |------------|------|
   | ≤ 15       | A4   |
   | 16–40      | A3   |
   | 41–80      | A2   |

   If total extent exceeds the page, use hierarchical sheets.

   **Step 5 — Assign absolute coordinates.** Start first block at
   (25.4, 25.4). All coordinates snap to 2.54mm grid.

#### 8.3: Component Symbol References

> ⚠️ **EXISTING CODE IS ASSUMED NON-CONFORMING.** If the repository
> has existing schematic generation code, it predates these standards.
> Do NOT copy symbol dimensions or connectivity patterns from existing
> functions without verifying each value against the dimension table
> below. Common non-conformances: undersized bodies (e.g., 0.762mm
> instead of 2.032mm), missing `_0_1` sub-symbols, label-only
> connectivity, pin lengths below 1.27mm.

Use KiCad standard library symbols:
- `Device:R`, `Device:C`, `Device:C_Polarized`, `Device:L`
- `Device:LED`, `Device:D`, `Device:D_Schottky`, `Device:D_TVS`
- `Device:Q_PMOS_GDS` (for power gating MOSFET)
- `Connector_Generic:Conn_01xNN` (substitute pin count)
- `power:GND`, `power:+3V3`, `power:+5V`, `power:PWR_FLAG`
- `TestPoint:TestPoint`

**Every symbol MUST have a complete definition in `lib_symbols`**,
including the graphical body sub-symbol (`_0_1`). KiCad schematic
files are self-contained — they do NOT load symbols from external
libraries at render time.

#### Extracting Standard Symbols from KiCad (PREFERRED)

Standard components (resistors, capacitors, diodes, transistors)
have **well-known electrical schematic symbols** — zigzag for
resistors, parallel plates for capacitors, triangle-and-bar for
diodes, gate/drain/source arrow for MOSFETs. Using plain rectangles
for passives and transistors is non-conforming.

The **preferred method** is to extract real symbol definitions from
KiCad's installed library:

```python
import glob, os, re

def extract_kicad_symbol(lib_name: str, symbol_name: str) -> str:
    """Extract a symbol from KiCad's standard library files.
    
    KiCad library paths:
      Windows: C:\\Program Files\\KiCad\\<ver>\\share\\kicad\\symbols\\
      Linux:   /usr/share/kicad/symbols/
      macOS:   /Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/
    """
    search = ["C:/Program Files/KiCad/*/share/kicad/symbols",
              "/usr/share/kicad/symbols",
              "/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"]
    lib_file = None
    for pat in search:
        for p in glob.glob(pat):
            c = os.path.join(p, f"{lib_name}.kicad_sym")
            if os.path.exists(c):
                lib_file = c; break
        if lib_file: break
    if not lib_file:
        raise FileNotFoundError(f"Library '{lib_name}.kicad_sym' not found")
    with open(lib_file) as f:
        content = f.read()
    match = re.search(rf'\(symbol "{re.escape(symbol_name)}"', content)
    if not match:
        raise ValueError(f"Symbol '{symbol_name}' not found in {lib_file}")
    start, depth = match.start(), 0
    for i in range(start, len(content)):
        if content[i] == '(': depth += 1
        elif content[i] == ')':
            depth -= 1
            if depth == 0: return content[start:i+1]
    raise ValueError(f"Malformed symbol block for '{symbol_name}'")
```

Call `extract_kicad_symbol("Device", "R")` etc. for every standard
symbol and embed the result in `lib_symbols`. This gives correct
graphical shapes — not simplified rectangles.

**If KiCad is not installed**, fall back to the inline examples
below. But extracted library symbols are always preferred.

**Mandatory Symbol Dimensions** (normative — do NOT use smaller
values from existing code):

| Symbol Type | Body (W×H mm) | Pin Span (mm) | Pin Length (mm) | Min Spacing (mm) |
|-------------|---------------|---------------|-----------------|-----------------|
| 2-pin passive (R,C,L) | 2.032 × 5.08 | 7.62 | 1.27 | 10.16 |
| 3-pin (SOT-23) | 2.54 × 5.08 | 7.62 | 2.54 | 12.70 |
| IC (≤ 8 pins) | 10.16 × (pins/2 × 2.54) | — | 2.54 | 20.32 |
| IC (> 8 pins) | 10.16 × (pins/2 × 2.54) | — | 2.54 | 25.40 |
| Connector (N pins) | 2.54 × (N × 2.54) | — | 2.54 | 15.24 |

**Validation:** If any symbol body < 2.032mm in either dimension, or
any pin length < 1.27mm, the symbol is non-conforming — resize it.

**Fallback symbol examples** (use ONLY when KiCad library extraction
is unavailable — these use simplified IEC shapes):

```
;; Resistor — IEC rectangular body (simplified fallback)
;; For standard graphical symbols, extract from KiCad library instead.
(symbol "Device:R" (in_bom yes) (on_board yes)
  (property "Reference" "R" (at 2.032 0 90) (effects (font (size 1.27 1.27))))
  (property "Value" "R" (at -2.032 0 90) (effects (font (size 1.27 1.27))))
  (symbol "Device:R_0_1"
    (rectangle (start -1.016 -2.54) (end 1.016 2.54)
      (stroke (width 0) (type default)) (fill (type none))))
  (symbol "Device:R_1_1"
    (pin passive line (at 0 3.81 270) (length 1.27)
      (name "~" (effects (font (size 1.27 1.27))))
      (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 0 -3.81 90) (length 1.27)
      (name "~" (effects (font (size 1.27 1.27))))
      (number "2" (effects (font (size 1.27 1.27)))))))

;; Generic IC — rectangle body is CORRECT for ICs (standard symbol).
;; Do NOT use rectangles for passives or transistors.
(symbol "Custom:MyIC" (in_bom yes) (on_board yes)
  (property "Reference" "U" (at 0 6.35 0) (effects (font (size 1.27 1.27))))
  (property "Value" "MyIC" (at 0 -6.35 0) (effects (font (size 1.27 1.27))))
  (symbol "Custom:MyIC_0_1"
    (rectangle (start -5.08 5.08) (end 5.08 -5.08)
      (stroke (width 0.254) (type default)) (fill (type background))))
  (symbol "Custom:MyIC_1_1"
    (pin input line (at -7.62 2.54 0) (length 2.54)
      (name "VDD" ...) (number "1" ...))
    (pin passive line (at -7.62 0 0) (length 2.54)
      (name "GND" ...) (number "2" ...))
    (pin bidirectional line (at 7.62 2.54 180) (length 2.54)
      (name "SDA" ...) (number "3" ...))
    (pin bidirectional line (at 7.62 0 180) (length 2.54)
      (name "SCL" ...) (number "4" ...))))
```

For specific ICs, use manufacturer library or create inline symbol
definitions. Custom IC symbols MUST include:
- A `_0_1` sub-symbol with at least a `(rectangle ...)` body
- A `_1_1` sub-symbol with all pins and correct electrical types
- Pin endpoints outside the rectangle, length extending inward
- `(property ...)` entries for Reference, Value, and Footprint

#### 8.4: Generation Checklist

- [ ] Every symbol in `lib_symbols` has a `_0_1` sub-symbol with at
      least one graphical primitive (rectangle, polyline, arc, circle)
- [ ] Every symbol in `lib_symbols` has a `_1_1` sub-symbol with all
      pin definitions
- [ ] Every pin has at least one wire segment connecting it to a net
      label, another pin, or a power symbol — no label-only connections
- [ ] All component origins fall within the page drawing area (minimum
      25mm from all page borders)
- [ ] Every coordinate is a multiple of 2.54
- [ ] Every wire endpoint matches a pin endpoint exactly
- [ ] Every junction placed where 3+ wires meet
- [ ] Every unconnected pin has no-connect marker
- [ ] Every power net has PWR_FLAG
- [ ] No components overlap
- [ ] All UUIDs unique
- [ ] Reference designators unique and sequential
- [ ] Title block populated

#### 8.5: Visual Verification Gate (MANDATORY)

After generating the `.kicad_sch` file, **render the schematic and
visually inspect it**. This step CANNOT be skipped. Open in KiCad,
export via `kicad-cli sch export pdf`, or ask the user to inspect.

**Verify all five:**

1. **All symbols visible at legible size** — every component has a
   visible body. If invisible or tiny, the `_0_1` sub-symbol is
   missing or dimensions are below the mandatory minimums.
2. **All wires visible** — every connection has wire segments. If
   labels exist but no wires, `(wire ...)` entries are missing.
   Verify intra-block connections use direct wires, not just labels.
3. **No components overlap or fall outside the page border**.
4. **Labels are readable** — no label-on-label or label-on-pin
   overlaps. Labels sit on wire stubs with clear separation.
5. **Functional blocks visually grouped** — related components
   clustered together with clear inter-block separation.

If any check fails, fix before presenting. Common fixes:
- Invisible/tiny symbols → add `_0_1` sub-symbol at mandatory sizes;
  do NOT reuse undersized dimensions from existing code
- Missing wires → add `(wire ...)` entries between pins and labels
- Off-page → recalculate via layout composition algorithm
- Overlapping labels → extend wire stubs, adjust positions
- No visual grouping → re-run layout composition algorithm

#### 8.6: Executable Validator

If generating schematics via Python, implement and run this validator
BEFORE declaring the schematic complete:

```python
def validate_kicad_sch(sch_path: str) -> list[str]:
    """Validate .kicad_sch against PromptKit schematic standards."""
    import re
    errors = []
    with open(sch_path, "r") as f:
        content = f.read()

    # 1. Every lib_symbol needs a _0_1 sub-symbol with graphics
    for name in re.findall(r'\(symbol "([^"]+)"\s+\(in_bom', content):
        escaped = re.escape(name)
        if not re.search(rf'\(symbol "{escaped}_0_1"', content):
            errors.append(f"MISSING GRAPHICAL BODY: {name}")
        else:
            m = re.search(rf'\(symbol "{escaped}_0_1"(.*?)\n  \)',
                          content, re.DOTALL)
            if m and not re.search(
                    r'\(rectangle|\(polyline|\(arc|\(circle', m.group(1)):
                errors.append(f"EMPTY GRAPHICAL BODY: {name}_0_1")

    # 2. Pin lengths >= 1.27mm
    for length in re.findall(r'\(pin [^)]+\(length ([0-9.]+)\)', content):
        if float(length) < 1.27:
            errors.append(f"PIN TOO SHORT: {length}mm < 1.27mm min")
            break

    # 3. Body rectangles >= 2.032mm
    for x1, y1, x2, y2 in re.findall(
            r'\(rectangle \(start ([0-9.-]+) ([0-9.-]+)\) '
            r'\(end ([0-9.-]+) ([0-9.-]+)\)', content):
        w, h = abs(float(x2)-float(x1)), abs(float(y2)-float(y1))
        if w < 2.032 and h < 2.032:
            errors.append(f"BODY TOO SMALL: {w:.1f}x{h:.1f}mm")

    # 4. At least one wire must exist
    if not re.findall(r'\(wire ', content):
        errors.append("NO WIRES: zero (wire ...) entries")

    # 5. Components within page bounds
    page = re.search(r'\(paper "([^"]+)"\)', content)
    if page:
        limits = {"A4": (297,210), "A3": (420,297)}.get(
            page.group(1), (297,210))
        for x, y in re.findall(
                r'\(symbol \(lib_id "[^"]+"\).*?\(at ([0-9.-]+) ([0-9.-]+)',
                content, re.DOTALL):
            if not (25 <= float(x) <= limits[0]-25):
                errors.append(f"OFF PAGE: x={x}")
            if not (25 <= float(y) <= limits[1]-25):
                errors.append(f"OFF PAGE: y={y}")
    return errors
```

Run `validate_kicad_sch()` after generation. Fix all errors before
the visual gate (§8.5). This is not exhaustive — it does not replace
visual inspection.

### Phase 9: Self-Audit Checklist

Cross-check against the schematic-compliance-audit protocol before
presenting to the user:

1. **Power architecture**: Every rail traced source to load. Decoupling
   on every IC power pin. Current budget within source capacity.
2. **Pin-level**: Every IC pin verified against datasheet.
3. **Bus integrity**: I2C pull-ups, SPI CS lines, UART crossover.
4. **Protection**: ESD on external connectors, reverse polarity on power.
5. **Power sequencing**: Reset timing, boot pin configuration.
6. **Passive values**: Resistor power, capacitor voltage derating.
7. **Completeness**: No unconnected nets, no floating inputs, test points
   on critical signals.

Fix Critical and High findings before presenting. Present Medium/Low
as notes.

---

## Protocol: Schematic Compliance Audit

Apply this protocol when reviewing the schematic against requirements
and datasheets. Execute all phases in order.

### Phase 1: Power Architecture Review

1. **Enumerate every power rail** with source, voltage, tolerance, max current.
2. **Trace source to load** for each rail. Verify regulator input range,
   output voltage, current capacity, dropout.
3. **Verify decoupling** for each IC power pin.
4. **Check power rail isolation**: No backpower through I/O pins into
   unpowered gated rails. No current leakage through pull-ups on gated
   rails. Gate control signal in safe state at reset.
5. **Current budget**: Sum worst-case current per rail vs. source rating.
   Flag > 80% (marginal) or > 100% (violation).

### Phase 2: Pin-Level Audit

For every IC, verify every pin:
- Connected pins: correct voltage domain, compatible signal direction.
- Power pins: correct rail, ground connected, decoupling present.
- Unused pins: per datasheet recommendation.
- Bootstrap/strap pins: correct level, not overridden at power-on.
- Analog pins: reference connected, input in range.
- Reset pins: proper circuit, not floating.

Flag unconnected without justification, wrong voltage domain, output
contention, overridden strap pins.

### Phase 3: Bus Integrity

1. **I2C**: Pull-ups present on correct rail, appropriate value, no
   address conflicts, same voltage domain.
2. **SPI**: All signals routed, unique CS per device, unused CS pulled high.
3. **UART**: TX/RX crossed correctly, voltage compatible.
4. **USB**: D+/D- end-to-end, ESD at connector, termination, VBUS detect.
5. **Other buses**: Bus-specific termination and protection.

### Phase 4: Protection Circuit Review

1. **ESD**: Present on every external connector. Clamping below IC abs max.
2. **Reverse polarity**: On battery/DC input. Acceptable voltage drop.
3. **Overcurrent**: Fuse/PTC on power inputs.
4. **Overvoltage**: Clamping if input could exceed regulator max.

### Phase 5: Power Sequencing and Reset

1. **Power-on sequence**: If required, verify enforcement.
2. **Reset circuit**: Held until power stable. Release timing meets IC min.
3. **Bootstrap pins**: Correct value at sample time.

### Phase 6: Passive Component Verification

1. **Resistors**: Value, tolerance, power rating (P = V²/R).
2. **Capacitors**: Voltage rating ≥ 1.5× rail, DC bias derating for MLCCs.
3. **Inductors**: Saturation current exceeds worst-case DC + ripple.
4. **Ferrite beads**: Impedance at target frequency, DC current rating.

### Phase 7: Completeness Check

1. **Unconnected nets**: Any single-connection net is suspect.
2. **Missing ground**: Every IC and connector ground pin connected.
3. **Missing decoupling**: Cross-reference with Phase 1.
4. **Floating inputs**: Every IC input must be driven.
5. **Test points**: Power rails, reset, programming interface accessible.
6. **Designator/value completeness**: No placeholders remaining.

### Audit Verdict

- **PASS**: No blocking compliance issues.
- **FAIL**: Blocking issues found — fix and re-audit.

Present schematic, key decisions, audit results, and BOM. Ask:
"Do you approve this schematic?"

Transition rules:
- Approved → Phase 6 (PCB Layout)
- Revise schematic → Phase 4
- Revise components → Phase 2
