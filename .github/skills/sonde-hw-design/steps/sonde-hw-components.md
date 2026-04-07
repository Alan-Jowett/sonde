# Phases 2–3: Component Selection & Audit — Methodology

This file contains the component-selection and component-selection-audit
protocols for Phases 2–3 of the Sonde hardware design workflow. The agent
should read this file when entering Phase 2.

---

## Protocol: Component Selection

Apply this protocol when selecting electronic components for a Sonde
carrier/sensor board based on requirements. The goal is to choose core
functional components (sensors, connectors, power management, protection)
that fulfil the user's feature requirements, are available for purchase,
and are mutually compatible. Execute all phases in order.

**Scope boundary**: This protocol selects components that deliver the
user's requested functionality. It does NOT select supporting circuitry
(decoupling capacitors, ESD protection, pull-up resistors, filtering
passives) — those are derived from the selected components' datasheets
during schematic design.

### Phase 1: Requirements Extraction

Extract the functional and environmental requirements that constrain
component selection.

1. **Feature requirements**: List every user-facing feature that
   requires a hardware component. For each feature, identify:
   - The physical phenomenon or interface involved
   - Performance targets (sample rate, data rate, resolution, range)
   - Interface to the host controller (I2C, SPI, UART, analog, GPIO)

2. **Environmental requirements**: Extract constraints:
   - Temperature range (commercial 0–70°C, industrial −40–85°C)
   - Supply voltage available or expected
   - Power budget (battery capacity and target life)
   - Physical size constraints
   - Regulatory requirements (FCC, CE)

3. **Project requirements**: Extract non-technical constraints:
   - Target unit cost at projected volume
   - Target fabrication service (JLCPCB, PCBWay)
   - Prototype vs. production
   - Timeline constraints

4. **Produce a requirements summary table**:

   | ID | Requirement | Drives Selection Of | Priority |
   |----|-------------|---------------------|----------|
   | CR-001 | Measure temperature ±0.5°C, −40–85°C | Temperature sensor | Must |
   | CR-002 | Deep-sleep current ≤ 20 µA | All components (shutdown/quiescent current) | Must |
   | ... | ... | ... | ... |

   Priority values: **Must** (non-negotiable), **Should** (strongly
   preferred), **May** (nice-to-have).

### Phase 2: Component Category Identification

1. **Enumerate categories**: For each requirement, identify the
   component category it demands:
   - **Sensor**: temperature, humidity, pressure, soil moisture, etc.
   - **Power management**: load switch / P-MOSFET (for power gating),
     LDO or switching regulator, battery protection
   - **Connector**: DIP socket for MCU module, sensor connectors,
     programming header, battery connector
   - **Protection**: ESD, reverse polarity, overcurrent
   - **Interface**: level shifter (if needed for voltage domain crossing)

2. **Consolidation check**: Can any component satisfy multiple
   requirements? (e.g., a sensor module with built-in ADC)

3. **Produce a category list** with consolidation notes.

### Phase 3: Candidate Search

For each component category, identify at least 2 viable candidates.

1. **Initial candidates from domain knowledge**: List well-known
   options. Include: part number, manufacturer, key spec, package.

   For Sonde designs, strongly prefer:
   - Components available in **JLCPCB basic parts** library
   - Components with **low quiescent/shutdown current** (≤ 1 µA for
     components on the gated rail)
   - Components in **hand-solderable packages** (0402 minimum for
     passives, SOIC/SOT-23 for ICs) unless assembly service handles
     smaller packages

2. **Real-time verification**: When search tools are available, verify:
   - Part number is current (not discontinued or NRND)
   - Key specs match the requirement
   - Part is available from major distributors (DigiKey, Mouser, LCSC)

   **If search is unavailable**: Mark unverified fields as `[UNVERIFIED]`.

3. **Expand search if needed**: Search distributor parametric filters.

4. **Produce a candidate list per category**:

   | Category | Candidate | Manufacturer | Key Spec | Package | Shutdown Current | Status |
   |----------|-----------|-------------|----------|---------|-----------------|--------|
   | Temp sensor | TMP117AIDRVR | TI | ±0.1°C, I2C | SOT-6 | 250 nA | Active |
   | ... | ... | ... | ... | ... | ... | ... |

### Phase 4: Technical Evaluation

Score each candidate against the requirements.

1. **Define evaluation criteria** from the requirements:
   - Feature coverage
   - Power consumption (active AND shutdown/quiescent — critical for Sonde)
   - Interface compatibility with ESP32-C3
   - Package suitability
   - Software ecosystem / driver availability
   - Cost

2. **Weight the criteria** — for Sonde, recommend these defaults:

   | Criterion | Weight |
   |-----------|--------|
   | Feature coverage | 20% |
   | Power consumption (sleep) | 25% |
   | Cost | 20% |
   | Package suitability | 15% |
   | Interface compatibility | 10% |
   | Software ecosystem | 10% |

   Ask the user to adjust weights before scoring.

3. **Score each candidate** on a 1–5 scale per criterion.

4. **Flag disqualifiers**: Any candidate scoring 1 on a Must-priority
   requirement is eliminated. Document the reason.

### Phase 5: Sourcing Evaluation

Verify procurement feasibility. The same search fallback applies.

1. **Availability check**: In-stock quantity, unit price (prototype
   and production), MOQ, lead time.

2. **Lifecycle status**: Active, NRND, Obsolete, Preview.

3. **Second-source check**: Pin-compatible alternatives from other
   manufacturers.

4. **Assembly service compatibility**: Verify JLCPCB basic vs.
   extended parts classification.

5. **Produce a sourcing summary per candidate**.

### Phase 6: Compatibility Cross-Check

Verify selected components work together.

1. **Voltage domain compatibility**: Map each component's operating
   voltage. Verify bus-level compatibility.

2. **Interface compatibility**: Verify protocol modes, electrical
   levels, pin availability on ESP32-C3.

3. **Power budget roll-up**: For Sonde, calculate TWO budgets:
   - **Deep-sleep budget**: Sum quiescent/shutdown current of ALL
     components. Components on the gated rail contribute their
     leakage current (should be ~0). Components on the always-on
     rail contribute their quiescent current. The MCU module's
     deep-sleep current dominates. Target: ≤ 20 µA total.
   - **Active budget**: Sum worst-case active current. Compare to
     battery capacity for duty-cycle-adjusted battery life.

4. **Physical compatibility**: Footprint area, height clearance,
   thermal adjacency.

5. **Produce a compatibility matrix**.

### Phase 7: Selection Decision Matrix

Produce the final weighted decision matrix.

1. **Combine scores** into a table per category.
2. **State the recommendation** with justification.
3. **Document eliminated candidates** with reasons.

### Phase 8: Selection Summary

1. **Selected components table** with category, part, manufacturer,
   key spec, package, price, justification.

2. **Risk flags**: Single-source, long lead times, tight margins,
   lifecycle concerns.

3. **Downstream implications for schematic design**: Required voltage
   rails, bus interfaces, layout-sensitive signals, power gating
   requirements, reference circuit recommendations.

4. **Present for user approval**: The user MUST confirm before
   proceeding to schematic design.

---

## Protocol: Component Selection Audit

Apply this protocol when auditing the component selection report. The
auditor MUST NOT trust any claim — every assertion is re-verified
independently. Execute all phases in order.

### Phase 1: Part Number Verification

For every selected component, verify the part number is real and
currently orderable.

**If search is unavailable**: Ask the user to provide datasheet URLs
or distributor links. Mark unverifiable assertions as `[UNVERIFIED]`.
If a selected component's existence or orderable status cannot be
verified, treat it as a blocking finding — the audit MUST FAIL.

1. **Part number existence**: Search manufacturer's website or major
   distributor. Common hallucination patterns:
   - Plausible but non-existent suffixes
   - Outdated part numbers
   - Conflation of eval board and IC part numbers
   - Module vs. bare-chip confusion

2. **Manufacturer confirmation**: Verify via manufacturer's product page.

3. **Record verification results** in a table.

4. **Unverified part numbers** are Critical findings that force FAIL.

### Phase 2: Specification Cross-Check

For every selected component, verify claimed specs match the datasheet.

1. **Locate the current datasheet** from the manufacturer.

2. **Verify each claimed specification**: Operating voltage, temperature
   range, performance specs, interfaces, package, power consumption
   (especially shutdown/quiescent current for Sonde).

3. **Common errors to check**:
   - Confusing typical vs. maximum/minimum values
   - Confusing module vs. IC specs
   - Misattributing specs from one variant to another
   - Outdated specs from a previous revision

4. **Severity classification**:
   - Critical: Wrong spec, actual doesn't meet requirement
   - High: Wrong spec, actual still meets requirement
   - Medium: Ambiguous (typical vs. max)
   - Low: Minor discrepancy, no functional impact
   - Informational: Over-specification, cost opportunity

### Phase 3: Requirements Satisfaction Audit

1. **Requirements traceability**: For each requirement, verify a
   selected component is mapped and its verified spec satisfies it.

2. **Unsatisfied requirements**: Flag any unmapped or unsatisfied
   requirements.

3. **Over-specification check**: Flag expensive components where
   simpler alternatives would suffice (informational).

4. **Produce a traceability matrix**.

### Phase 4: Sourcing Data Verification

Independently verify sourcing claims.

1. **Current availability**: Search distributors, record stock and price.
2. **Compare against claims**: Flag price > 20% off, stock changes, etc.
3. **Assembly service verification**: Check JLCPCB/PCBWay parts database.
4. **Sourcing risk assessment**: Single-source, low stock, extended lead
   time, recent price increases.

### Phase 5: Compatibility Verification

1. **Voltage level verification**: Check output high/low vs. input
   thresholds from datasheets for each inter-component connection.
2. **Interface protocol verification**: Verify matching protocol modes.
3. **Power budget verification**: Recalculate using verified specs.
   Flag if verified budget differs > 10% from claimed.
4. **Pin count verification**: Verify ESP32-C3 has enough pins.

### Phase 6: Findings Summary

1. **Document each finding**: Phase, component, severity, evidence,
   recommended action.

2. **Produce an audit verdict**:
   - **PASS**: No Critical or High findings
   - **PASS WITH CONDITIONS**: No Critical, but High findings exist
   - **FAIL**: Critical findings — selection must be revised

3. **Audit coverage summary**: Part numbers verified, specs checked,
   requirements traced, sourcing verified, compatibility checked.
