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

### Phase 8: KiCad Schematic Generation

Generate `.kicad_sch` S-expression file(s).

#### 8.1: S-Expression Structure

```
(kicad_sch
  (version 20231120)
  (generator "promptkit")
  (uuid "<random-uuid>")
  (paper "A4")
  (title_block ...)
  (lib_symbols ...)
  (symbol ...)       ;; Component instances
  (wire ...)         ;; Wires
  (label ...)        ;; Net labels
  (global_label ...) ;; Inter-sheet labels
  (no_connect ...)   ;; NC markers
  (junction ...)     ;; Wire junctions
)
```

#### 8.2: Visual Layout Rules (MANDATORY)

1. **Grid alignment**: ALL coordinates snap to 2.54mm grid.
2. **Component spacing**: Minimum 20.32mm between component bodies.
3. **Signal flow**: Left-to-right. Power top-to-bottom.
4. **Component orientation**: Pin 1 top-left for ICs. Conventional
   orientation for passives, diodes, connectors.
5. **Wire routing**: Orthogonal only. Endpoints MUST align with pin
   endpoints exactly. Use net labels for long connections. Junction
   dots where 3+ wires meet. Never route through component bodies.
6. **Label placement**: On short wire stubs from pins. Power symbols
   on vertical stubs above/below connection point.
7. **No-connect markers**: On every intentionally unconnected pin.
8. **Power flags**: PWR_FLAG on every power net driven by a regulator
   or connector pin.

#### 8.3: Component Symbol References

Use KiCad standard library symbols:
- `Device:R`, `Device:C`, `Device:C_Polarized`, `Device:L`
- `Device:LED`, `Device:D`, `Device:D_Schottky`, `Device:D_TVS`
- `Device:Q_PMOS_GDS` (for power gating MOSFET)
- `Connector_Generic:Conn_01xNN` (substitute pin count)
- `power:GND`, `power:+3V3`, `power:+5V`, `power:PWR_FLAG`
- `TestPoint:TestPoint`

For specific ICs, use manufacturer library or create inline symbol
definitions with correct pin positions, names, and electrical types.

#### 8.4: Generation Checklist

- [ ] Every coordinate is a multiple of 2.54
- [ ] Every wire endpoint matches a pin endpoint exactly
- [ ] Every junction placed where 3+ wires meet
- [ ] Every unconnected pin has no-connect marker
- [ ] Every power net has PWR_FLAG
- [ ] No components overlap
- [ ] All UUIDs unique
- [ ] Reference designators unique and sequential
- [ ] Title block populated

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
