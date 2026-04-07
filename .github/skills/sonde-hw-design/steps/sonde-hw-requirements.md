# Phase 1: Requirements Discovery — Methodology

This file contains the detailed requirements-elicitation protocol for
Phase 1 of the Sonde hardware design workflow. The agent should read
this file when entering Phase 1.

---

## Protocol: Requirements Elicitation

Apply this protocol when converting a natural language description of a
Sonde carrier/sensor board into structured requirements. The goal is to
produce requirements that are **precise, testable, unambiguous, and traceable**.

### Phase 1: Scope Extraction

From the provided description:

1. Identify the **core objective**: what problem does this solve? For whom?
2. Identify **explicit constraints**: performance targets, compatibility
   requirements, regulatory requirements, deadlines.
3. Identify **implicit constraints**: assumptions about the environment,
   platform, or existing system that are not stated but required.
   Flag each with `[IMPLICIT]`.
4. Define **what is in scope** and **what is out of scope**. When the
   boundary is unclear, enumerate the ambiguity and ask for clarification.

### Phase 2: Requirement Decomposition

For each capability described:

1. Break it into **atomic requirements** — each requirement describes
   exactly one testable behavior or constraint.
2. Use **RFC 2119 keywords** precisely:
   - MUST / MUST NOT — absolute requirement or prohibition
   - SHALL / SHALL NOT — equivalent to MUST (used in some standards)
   - SHOULD / SHOULD NOT — recommended but not absolute
   - MAY — truly optional
3. Assign a **stable identifier**: `REQ-HW-<NNN>`
4. Write each requirement in the form:
   ```
   REQ-HW-<NNN>: The board MUST/SHALL/SHOULD/MAY <behavior>
   when <condition> so that <rationale>.
   ```

### Phase 3: Ambiguity Detection

Review each requirement for:

1. **Vague adjectives**: "fast," "responsive," "low-power" — replace
   with measurable criteria (e.g., "≤ 20 µA deep-sleep current").
2. **Unquantified quantities**: "long battery life," "many sensors" —
   replace with specific numbers or ranges.
3. **Implicit behavior**: "the board handles power" — what does that
   mean? Battery input? USB fallback? Both? Charging?
4. **Undefined terms**: if a term could mean different things, add it
   to a glossary with a precise definition.
5. **Missing negative requirements**: for every "MUST do X," consider
   "MUST NOT do Y" (e.g., "MUST NOT draw > 20 µA in deep sleep").

### Phase 4: Dependency and Conflict Analysis

1. Identify **dependencies** between requirements: which requirements
   must be satisfied before others can be implemented or tested?
2. Check for **conflicts**: requirements that contradict each other
   or create impossible constraints.
3. Check for **completeness**: are there scenarios or edge cases
   that no requirement covers? If so, draft candidate requirements
   and flag them as `[CANDIDATE]` for review.

### Phase 5: Acceptance Criteria

For each requirement:

1. Define at least one **acceptance criterion** — a concrete test that
   determines whether the requirement is met.
2. Acceptance criteria should be:
   - **Specific**: describes exact inputs, actions, and expected outputs.
   - **Measurable**: pass/fail is objective, not subjective.
   - **Independent**: testable without requiring other requirements to be met
     (where possible).

---

## Sonde-Specific Requirements Probes

When eliciting requirements for a Sonde carrier/sensor board, ask about:

1. **Sensors**: What physical phenomena? (temperature, humidity, soil
   moisture, light, gas, pressure, vibration, current) What accuracy
   and range? What interface (I2C, SPI, analog)?

2. **Power source**: Battery type and capacity? (CR123A, 2×AA, 18650,
   coin cell, USB). Target battery life? (months, years)

3. **Power gating**: Which peripherals go on the gated rail vs.
   always-on? Does anything need to retain state across sleep cycles?

4. **MCU module**: Which ESP32 module? (ESP32-C3-MINI-1,
   ESP32-C3-DevKitM-1, ESP32-S3-MINI-1, etc.) DIP socket pitch?
   (2.54mm standard)

5. **Connectivity**: ESP-NOW only (via module's built-in radio)?
   External antenna connector? Range requirements?

6. **Environment**: Indoor/outdoor? Temperature range? Moisture/IP
   rating? Vibration?

7. **Form factor**: Target board dimensions? Must fit an existing
   enclosure? Mounting method? (standoffs, adhesive, DIN rail)

8. **Connectors**: Sensor connectors (JST, screw terminal, header)?
   Programming header (USB-C, UART header, Tag-Connect)?

9. **Cost and volume**: Prototype or production? Target per-unit BOM
   cost? Fab service (JLCPCB, PCBWay)?

10. **Indicators**: Status LEDs? (power, activity, error) On gated
    or always-on rail?

11. **Implicit Sonde requirements** — surface these if the user
    doesn't mention them:
    - Deep-sleep current ≤ 20 µA (whole board, not just MCU)
    - Power gating for all non-essential peripherals
    - DIP socket for ESP32 module (not soldered down)
    - Test points on power rails, gated rail, reset, UART
    - UART or USB programming access
    - Reverse polarity protection on battery input
    - ESD protection on external connectors

### Output

A requirements summary table:

| ID | Requirement | Priority | Drives Selection Of | Acceptance Criterion |
|----|-------------|----------|---------------------|---------------------|
| REQ-HW-001 | Board MUST accept ESP32-C3-MINI-1 via 2.54mm DIP socket | Must | Connector, board layout | Module plugs in and makes electrical contact on all pins |
| REQ-HW-002 | Board MUST draw ≤ 20 µA in deep sleep | Must | Power architecture, component selection | Measured current at battery terminals with MCU in deep sleep |
| ... | ... | ... | ... | ... |

**Do NOT proceed to Phase 2 until the user explicitly confirms the
requirements are complete.**
