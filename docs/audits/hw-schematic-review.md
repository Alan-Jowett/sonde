<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->

# Sonde Minimal Sensor Node — Schematic Design Review

## 1. Executive Summary

A systematic 7-phase schematic review of the `minimal-qwiic` board design
(hw-schematic-design.md) identified **15 findings**: 2 High, 6 Medium,
4 Low, and 3 Informational. The two High-severity findings are (1) the
MCP1700 LDO provides only 250 mA continuous, violating the HW-0102
requirement of ≥ 500 mA, and (2) a resistor voltage-divider on the P-FET
gate (R9/R10 both 10 kΩ) limits Vgs to −1.65 V instead of −3.3 V,
preventing full MOSFET enhancement. Both are correctable before schematic
capture. The remaining findings cover missing ESD protection on external
Qwiic/GPIO connectors, an absent SENSOR\_3V3 decoupling capacitor, a
missing battery fuse, and documentation discrepancies. Overall the design
is sound; the power architecture, deep-sleep budget, USB section, and I2C
bus are well-considered. Addressing the High findings before KiCad entry
is recommended.

---

## 2. Problem Statement

**Objective:** Verify the textual schematic design document
(`hw-schematic-design.md`) for electrical correctness, datasheet
compliance, and requirements coverage before schematic capture in KiCad.

**Expected outcome:** Every ESP32-C3 pin is accounted for, every power
path is traced and verified, every external interface has appropriate
protection, and all applicable HW-xxxx requirements are met.

**Impact:** Catching errors at the schematic design stage avoids costly
PCB re-spins and prototype failures.

---

## 3. Investigation Scope

- **Documents examined:**
  - `docs/hw-schematic-design.md` (v0.1, 2026-03-29) — schematic under
    review
  - `docs/hw-requirements.md` (Draft) — requirements baseline
  - `prompts/hardware/01-review-schematic.md` — review protocol

- **Datasheets consulted (via web search):**
  - ESP32-C3-MINI-1 datasheet (Espressif) — strapping pins, power pins,
    abs max ratings, USB pull-up behavior
  - MCP1700-3302E/TT datasheet (Microchip) — dropout, Iq, capacitor
    requirements
  - USBLC6-2SC6 datasheet (STMicroelectronics) — clamping voltage,
    pinout, leakage
  - Si2301CDS datasheet (Vishay) — Vgs(th), Rds(on) at multiple Vgs,
    gate charge

- **Tools used:** Manual review, datasheet cross-reference, arithmetic
  verification of power budgets and voltage dividers.

- **Limitations:**
  - No physical datasheet PDFs were opened; specifications were obtained
    from web-search summaries of official datasheets. Values are
    cross-checked across multiple sources for consistency.
  - No circuit simulation was performed (static analysis only).
  - PCB layout was not reviewed (schematic-level only). Layout
    carry-forward items are identified in Finding F-015.

### Phase Coverage

| Phase | Title | Findings? |
|-------|-------|-----------|
| 1 | Power Architecture Review | Yes (F-001, F-006, F-007, F-008, F-012) |
| 2 | Pin-Level Audit | Yes (F-002, F-011, F-013) |
| 3 | Bus Integrity | Yes (F-009, F-014) |
| 4 | Protection Circuit Review | Yes (F-003, F-004, F-012) |
| 5 | Power Sequencing and Reset | No findings — design is correct |
| 6 | Passive Component Verification | Yes (F-005, F-007) |
| 7 | Completeness Check | Yes (F-010, F-015) |

---

## 4. Findings

Findings are ordered by severity (High → Medium → Low → Informational).

---

### Finding F-001: MCP1700 Output Current Below HW-0102 Requirement

- **Severity:** High
- **Category:** Phase 1 — Power Architecture Review
- **Location:** U2 (MCP1700-3302E/TT); requirement HW-0102
- **Description:**
  HW-0102 requires: *"Output: 3.3 V ± 5%, minimum 500 mA continuous."*
  The selected MCP1700-3302E/TT is rated for **250 mA continuous** per
  the Microchip datasheet. This is 50% of the required value.

  The design document acknowledges that ESP32-C3 radio TX bursts can
  reach 340 mA peak and relies on the 10 µF bulk capacitor (C5) to
  supply the transient difference. While this bulk-cap strategy is
  common practice for short bursts, the continuous rating still falls
  below the stated requirement.

- **Evidence:**
  - hw-requirements.md, HW-0102, criterion 2: "minimum 500 mA
    continuous"
  - hw-schematic-design.md, §4.1.2: "250 mA max output"
  - MCP1700 datasheet: "Maximum Output Current: 250 mA"

- **Root Cause:** The MCP1700 was selected for its 1.6 µA quiescent
  current (best-in-class for deep sleep). No LDO in SOT-23-3 combines
  sub-2 µA Iq with ≥ 500 mA output.

- **Impact:** If the ESP32-C3 WiFi transmitter draws sustained current
  above 250 mA (e.g., long packet trains or scanning), the LDO output
  voltage will droop or the device will thermally shut down. For single
  short TX bursts (< 10 ms at 340 mA peak), the 10 µF cap likely
  provides adequate ride-through.

- **Remediation:** Either (a) change the requirement HW-0102 to
  "minimum 250 mA continuous" and document the bulk-cap transient
  strategy, with a note that peak TX is handled by capacitor energy,
  or (b) select a higher-current LDO such as ME6211C33 (500 mA, 40 µA
  Iq) or TLV75533PDBV (500 mA, 17 µA Iq) and accept the higher
  quiescent current impact on deep sleep. Option (a) is recommended
  since 250 mA covers all steady-state loads and the TX burst strategy
  is well-proven.

- **Confidence:** High — values from both the requirement and datasheet
  are unambiguous.

---

### Finding F-002: P-FET Gate Voltage Divider Prevents Full Enhancement

- **Severity:** High
- **Category:** Phase 2 — Pin-Level Audit
- **Location:** Q1 (Si2301), R9 (10 kΩ), R10 (10 kΩ); U1 GPIO3
- **Description:**
  R9 (10 kΩ, gate → 3V3) and R10 (10 kΩ, gate → GPIO3) form a
  resistive voltage divider on the Q1 gate. When GPIO3 drives LOW
  (0 V):

  ```
  V_gate = 3.3 V × R10 / (R9 + R10)
         = 3.3 V × 10 k / (10 k + 10 k)
         = 1.65 V

  Vgs = V_gate − V_source = 1.65 V − 3.3 V = −1.65 V
  ```

  The Si2301 datasheet specifies Rds(on) at Vgs = −2.5 V (142 mΩ max)
  and Vgs = −4.5 V (112 mΩ max). At Vgs = −1.65 V the FET is past
  threshold (Vgs(th) = −0.4 V to −1.0 V) but **not fully enhanced**.
  Rds(on) will be significantly higher than the 110 mΩ cited in the
  design document — likely 300–500+ mΩ depending on drain current.

- **Evidence:**
  - hw-schematic-design.md, §4.5.1, circuit diagram: R9 and R10 both
    connected to gate node, 10 kΩ each
  - hw-schematic-design.md, §10 Net List: `SENSOR_PWR_EN: R10, Q1 gate
    (via R9 network), U1 GPIO3`
  - Si2301 datasheet: Rds(on) = 142 mΩ max at Vgs = −2.5 V;
    Vgs(th) = −0.4 V to −1.0 V

- **Root Cause:** R10 was added as a "drive resistor" between GPIO3 and
  the gate, but with the same value as pull-up R9 it creates a 1:1
  divider that halves the gate drive voltage.

- **Impact:** At typical Qwiic sensor loads (10–50 mA), the voltage
  drop across Q1 is small (5–25 mV at 500 mΩ) and sensor operation is
  unlikely to be affected. At higher loads (multiple sensors, 100+ mA),
  the drop becomes significant and wastes power. The FET also dissipates
  more heat in the linear region.

- **Remediation:** Remove R10 entirely and connect GPIO3 directly to
  the gate node. The pull-up R9 already provides a defined HIGH state
  when GPIO3 is Hi-Z. If a series gate resistor is desired for slew-rate
  control, use 1 kΩ for R10 (yielding Vgs ≈ −3.0 V) and/or increase
  R9 to 100 kΩ.

  Corrected circuit (recommended):
  ```
  GPIO3 ──────────┬── Q1 Gate
                   │
                   R9 (10 kΩ) ── 3V3
  ```

- **Confidence:** High — arithmetic is straightforward and Si2301
  Rds(on) vs Vgs behavior is well-documented.

---

### Finding F-003: No ESD Protection on Qwiic Connectors

- **Severity:** Medium
- **Category:** Phase 4 — Protection Circuit Review
- **Location:** J1, J2 (Qwiic connectors); U1 GPIO4, GPIO5
- **Description:**
  Qwiic connectors J1 and J2 are external-facing connectors where
  sensor modules are plugged and unplugged in the field. The I2C data
  lines (I2C0\_SDA on GPIO4, I2C0\_SCL on GPIO5) and the SENSOR\_3V3
  power pin have no ESD protection components between the connectors and
  the ESP32-C3.

  The ESP32-C3 I/O pins have a maximum voltage rating of 3.6 V
  (absolute max) and limited internal ESD tolerance. Hotplugging a Qwiic
  cable generates ESD events, particularly in dry outdoor environments.

- **Evidence:**
  - hw-schematic-design.md, §4.4.1: J1/J2 wired directly to
    I2C0\_SDA/SCL
  - hw-schematic-design.md, §10 Net List: `I2C0_SDA: U1 GPIO4, J1 pin
    3, J2 pin 3, R7` — no ESD device in path

- **Root Cause:** ESD protection was implemented for USB (USBLC6-2SC6)
  but not extended to the other external connectors.

- **Impact:** ESD strike during Qwiic cable insertion could damage GPIO4
  or GPIO5, potentially disabling I2C. Risk is elevated in the target
  outdoor deployment environment with low humidity.

- **Remediation:** Add a dual-channel ESD protection IC (e.g., PRTR5V0U2X
  for 3.3 V I/O or PESDxS2UT) on I2C0\_SDA and I2C0\_SCL between J1/J2
  and U1. Place the ESD device as close to the connectors as possible.
  [INFERRED] Low-capacitance (< 5 pF) devices are preferred to avoid
  degrading I2C rise time at 400 kHz.

- **Confidence:** High — standard ESD design practice for external
  connectors.

---

### Finding F-004: No ESD Protection on GPIO Breakout Header

- **Severity:** Medium
- **Category:** Phase 4 — Protection Circuit Review
- **Location:** J6 (GPIO header); U1 GPIO1, GPIO2, GPIO6, GPIO7, GPIO8,
  GPIO10, GPIO20, GPIO21
- **Description:**
  The GPIO breakout header J6 exposes 8 GPIO pins to user wiring. Long
  wires connected to header pins act as antennas for ESD and EMI
  coupling. No TVS or ESD protection is present on any header pin.

- **Evidence:**
  - hw-schematic-design.md, §4.7: header pin table, no protection
    components listed
  - Net List: header signals connect directly to U1 GPIOs

- **Root Cause:** Same as F-003 — ESD protection scoped only to USB.

- **Impact:** GPIO damage from ESD when user wires are connected. Less
  likely than F-003 (fewer plug/unplug cycles) but possible during bench
  work or field wiring.

- **Remediation:** Add TVS diode arrays (e.g., PESD0402-140 or similar)
  on header signals. Alternatively, document that J6 is intended for
  short, bench-use wiring only and external protection is the user's
  responsibility. A practical middle ground: add pads for optional TVS
  arrays (DNP by default).

- **Confidence:** High

---

### Finding F-005: No Decoupling Capacitor on SENSOR\_3V3 Rail

- **Severity:** Medium
- **Category:** Phase 6 — Passive Component Verification
- **Location:** SENSOR\_3V3 net (Q1 drain, J1 pin 2, J2 pin 2, R7, R8)
- **Description:**
  The SENSOR\_3V3 rail has no local decoupling capacitor. When Q1 turns
  on, inrush current into the capacitive load of connected Qwiic sensor
  modules (which typically have 1–10 µF of on-board decoupling) could
  cause a voltage transient on both SENSOR\_3V3 and the parent 3V3 rail.

  Additionally, the I2C pull-up resistors (R7, R8) source switching
  current from SENSOR\_3V3, and the absence of local decoupling means
  this switching current must travel back to C4/C5 on the 3V3 rail
  through Q1.

- **Evidence:**
  - hw-schematic-design.md, §10 Net List: `SENSOR_3V3: Q1 drain, SJ1,
    J1 pin 2, J2 pin 2, R7, R8` — no capacitor
  - Compare to 3V3 rail which has C4, C5, C6

- **Root Cause:** Oversight — the 3V3 rail was properly decoupled but
  the gated rail was not.

- **Impact:** Possible voltage dip on SENSOR\_3V3 during Q1 turn-on
  (inrush) and marginal I2C signal quality from lack of local bypass.
  Impact depends on connected sensor module capacitance.

- **Remediation:** Add a 1–10 µF ceramic capacitor (X5R, 10 V, 0402 or
  0603) on SENSOR\_3V3 close to J1/J2. A 1 µF cap is adequate for
  most Qwiic modules.

- **Confidence:** High

---

### Finding F-006: No Overcurrent Protection on Battery Input

- **Severity:** Medium
- **Category:** Phase 4 — Protection Circuit Review
- **Location:** J3 (battery connector), D2 (SS14 Schottky)
- **Description:**
  There is no fuse, PTC, or current-limiting device between the battery
  connector J3 and the rest of the circuit. LiPo cells can deliver
  short-circuit currents exceeding 10 A. A board-level fault (solder
  bridge, component failure) that shorts VIN or 3V3 to GND would draw
  destructive current through D2 (rated 1 A continuous, 30 A peak
  surge) and could cause the battery to overheat or vent.

- **Evidence:**
  - hw-schematic-design.md, §4.1.1: `VBAT → D2 (SS14) → VIN` — no
    current limiting device
  - SS14 datasheet: 1 A continuous, 30 A peak surge

- **Root Cause:** Fuse/PTC not included in the minimal BOM.

- **Impact:** Safety risk — unprotected LiPo short circuit can cause
  thermal runaway. The MCP1700 has internal short-circuit protection on
  its output, but a fault on the VIN rail bypasses the LDO's
  protection.

- **Remediation:** Add a 500 mA or 1 A PTC resettable fuse (e.g.,
  Bourns MF-PSMF050X-2, 0805) in series with J3 pin 1 before D2.

- **Confidence:** High

---

### Finding F-007: MCP1700 Max Dropout Voltage Incorrectly Stated

- **Severity:** Medium
- **Category:** Phase 1 — Power Architecture Review
- **Location:** U2 (MCP1700-3302E/TT); hw-schematic-design.md §4.1.1,
  §4.1.2
- **Description:**
  The schematic design document states the MCP1700 maximum dropout at
  250 mA as **500 mV** (§4.1.2 table and §4.1.1 battery cutoff
  calculation). The Microchip datasheet specifies the maximum dropout at
  250 mA as **350 mV**.

  The battery cutoff calculation in §4.1.1 uses the 500 mV figure:
  *"3.3 V + 0.5 V max dropout"* — this should read *"3.3 V + 0.35 V
  max dropout"*.

  Corrected minimum VBAT for regulation:
  3.3 V + 0.35 V + 0.45 V (Schottky) = **4.1 V** at full 250 mA load.
  At lighter loads (deep sleep, ~10 µA), dropout is negligible and the
  LDO regulates down to VIN ≈ 3.3 V (VBAT ≈ 3.75 V).

- **Evidence:**
  - hw-schematic-design.md, §4.1.2 table: "500 mV max" (row: Dropout
    @ 250 mA)
  - MCP1700 datasheet (Microchip DS20001826F): "Dropout Voltage:
    350 mV max @ 250 mA, VOUT ≥ 2.5 V"

- **Root Cause:** Documentation error — likely confused with another
  LDO's spec or an older revision.

- **Impact:** The error is conservative (overestimates dropout), so the
  actual circuit has *more* headroom than documented. However,
  inaccurate datasheet citations undermine design review trust. The
  battery cutoff analysis yields a slightly wrong VBAT threshold.

- **Remediation:** Correct §4.1.2 table to "350 mV max" and update the
  battery cutoff calculation in §4.1.1 accordingly.

- **Confidence:** High — datasheet value confirmed across multiple
  distributor sources (DigiKey, Farnell, Microchip official).

---

### Finding F-008: Schottky Diode Reverse Leakage Not in Sleep Budget

- **Severity:** Medium
- **Category:** Phase 1 — Power Architecture Review
- **Location:** D1 (SS14), D2 (SS14); deep sleep current budget
  (§4.1.3)
- **Description:**
  The deep sleep current budget (§4.1.3) does not include reverse
  leakage from the Schottky diodes D1 and D2. In battery-only operation
  (USB disconnected), D1 is reverse-biased at approximately VIN ≈ 3.25 V
  (with a 3.7 V battery minus D2 forward drop). Reverse leakage through
  D1 from VIN toward the VUSB net (pulled toward 0 V by USBLC6-2SC6
  internal structures) constitutes a parasitic current drain.

  SS14 (1N5819 equivalent) reverse leakage is typically < 1 µA at 25 °C
  and low reverse voltages. However, Schottky diode leakage increases
  exponentially with temperature. At the operating maximum of +60 °C,
  leakage can increase 10–100× from the 25 °C value, potentially
  reaching 10–50 µA per diode.

- **Evidence:**
  - hw-schematic-design.md, §4.1.3: no line item for D1/D2 leakage
  - [INFERRED] SS14 reverse leakage behavior from general Schottky
    diode temperature coefficients (approximately 2× per 10 °C rise)

- **Root Cause:** Oversight — Schottky leakage is negligible at room
  temperature but the operating range extends to +60 °C.

- **Impact:** At +60 °C worst case, the deep sleep budget could increase
  by 10–50 µA, potentially exceeding the 20 µA HW-0400 requirement.
  [ASSUMPTION] Exact SS14 reverse leakage at +60 °C and 3.25 V reverse
  bias is not available from the web-search data; verify from the SS14
  full datasheet reverse leakage vs temperature curves.

- **Remediation:** (a) Verify SS14 leakage at 60 °C from the full
  datasheet. If leakage exceeds 5 µA at 60 °C, consider replacing SS14
  with a lower-leakage Schottky (e.g., BAT54) or switching to a P-FET
  ideal diode circuit. (b) Add a D1/D2 leakage line item to the sleep
  current budget table with worst-case temperature values.

- **Confidence:** Medium — the concern is well-founded but exact
  leakage values at temperature require the full SS14 datasheet curves.

---

### Finding F-009: USB-C Shield Ground Connection Not Specified

- **Severity:** Low
- **Category:** Phase 3 — Bus Integrity
- **Location:** J4 (USB-C connector)
- **Description:**
  The USB-C connector J4 has a SHIELD pin listed in §4.2.1 but the
  schematic design does not explicitly describe how the shield is
  connected to GND. Best practice for USB-C is to connect the shield
  to chassis/board GND through a parallel combination of 100 nF + 1 MΩ
  to provide an ESD return path while avoiding ground loops in
  multi-board systems.

  The net list (§10) shows J4 connected to GND, which may include the
  shield, but the connection topology is ambiguous.

- **Evidence:**
  - hw-schematic-design.md, §4.2.1: SHIELD listed but no connection
    detail
  - §10 Net List: `GND: ... J4 ...` — ambiguous whether this includes
    SHIELD

- **Root Cause:** Incomplete specification of connector pin mapping.

- **Impact:** Direct shield-to-GND works and is commonly used on simple
  boards. The risk of ground loops is low for a battery-powered single-
  board device. Functional impact is minimal.

- **Remediation:** Explicitly document the shield connection. For this
  single-board design, a direct shield-to-GND connection is acceptable.
  Add a note to the schematic design document specifying the topology.

- **Confidence:** High

---

### Finding F-010: No Test Points Defined

- **Severity:** Low
- **Category:** Phase 7 — Completeness Check
- **Location:** Board-level; all critical nets
- **Description:**
  No test points are specified for any signal or power rail. For
  prototype bring-up and debug, test points on the following nets are
  strongly recommended:

  - `3V3` — verify regulator output
  - `VIN` — verify power OR-ing
  - `SENSOR_3V3` — verify power gate operation
  - `VBAT_SENSE` — verify ADC divider
  - `EN` — probe reset timing
  - `I2C0_SDA`, `I2C0_SCL` — verify I2C signal integrity
  - `GND` — oscilloscope ground clip point

- **Evidence:**
  - hw-schematic-design.md: no mention of test points in any section

- **Root Cause:** Not yet addressed in the draft schematic design.

- **Impact:** Increases debug difficulty during prototype bring-up. No
  impact on production boards if test points are omitted by choice.

- **Remediation:** Add test point pads (1.0 mm round SMD pads) for at
  minimum: 3V3, GND, SENSOR\_3V3, and VIN. Additional test points per
  the list above are recommended for the prototype revision.

- **Confidence:** High

---

### Finding F-011: Reversed Battery Applies Negative Voltage to GPIO0

- **Severity:** Low
- **Category:** Phase 2 — Pin-Level Audit
- **Location:** J3 (battery), R11 (10 MΩ), R12 (10 MΩ), U1 GPIO0
- **Description:**
  If the battery is connected with reversed polarity (despite the keyed
  JST-PH connector), J3 pin 1 goes to approximately −4.2 V. This
  voltage reaches GPIO0 (VBAT\_SENSE) through the voltage divider
  R11/R12, producing approximately −2.1 V at the ADC input pin.

  The ESP32-C3 GPIO absolute minimum voltage is typically −0.3 V.
  However, the 10 MΩ source impedance limits current through the
  internal ESD clamping diode to ~0.42 µA — well below any damage
  threshold.

- **Evidence:**
  - hw-schematic-design.md, §4.6.1: `VBAT → R11 (10MΩ) → VBAT_SENSE`
  - Calculation: 4.2 V / 20 MΩ = 0.21 µA through divider; GPIO0 sees
    −2.1 V through 10 MΩ impedance

- **Root Cause:** The Schottky diode D2 blocks reverse current to VIN
  but the voltage divider provides a separate path to GPIO0.

- **Impact:** Very low — the 10 MΩ impedance limits current to sub-µA
  levels. Internal ESD diode clamps the voltage safely. The JST-PH
  connector is mechanically keyed, making reverse insertion extremely
  unlikely.

- **Remediation:** No action required for the keyed connector variant.
  If a non-keyed battery connector is ever used, add a small-signal
  Schottky diode (e.g., BAT54) from GND to VBAT\_SENSE (cathode to
  VBAT\_SENSE) to clamp negative excursions.

- **Confidence:** High

---

### Finding F-012: No Overvoltage Protection on USB VBUS

- **Severity:** Low
- **Category:** Phase 4 — Protection Circuit Review
- **Location:** J4 (USB-C), D1 (SS14), U2 (MCP1700)
- **Description:**
  The MCP1700 maximum input voltage is 6.0 V. A compliant USB-C source
  provides 5.0 V ± 5% (max 5.25 V) to a device advertising default
  power via 5.1 kΩ CC resistors. After Schottky drop, VIN ≈ 4.8 V,
  well within limits.

  However, non-compliant USB chargers (Quick Charge, PD sources with
  firmware bugs) have been observed to deliver 9–20 V on VBUS. At 9 V:
  VIN = 9 V − 0.45 V = 8.55 V, exceeding the MCP1700 absolute maximum
  of 6.0 V.

- **Evidence:**
  - MCP1700 datasheet: VIN max = 6.0 V
  - hw-schematic-design.md, §4.2.1: CC resistors present (5.1 kΩ to
    GND), correctly identifying as UFP

- **Root Cause:** The design correctly requests only default USB power
  via CC resistors, but no hardware overvoltage protection exists if the
  source is non-compliant.

- **Impact:** Low probability (requires non-compliant source) but
  potentially destructive (permanent LDO and module damage). Risk is
  higher in field deployments where users may use arbitrary chargers.

- **Remediation:** [INFERRED] Add a VBUS TVS/Zener clamp (e.g., SMBJ6.0A
  or a 5.6 V Zener) on VUSB to clamp overvoltage. Alternatively,
  add a crowbar circuit using a TVS diode that triggers a PTC fuse.
  For the minimal BOM, document the risk and specify approved USB
  adapters.

- **Confidence:** Medium — failure mode is real but probability depends
  on deployment practices.

---

### Finding F-013: Strapping Pins Exposed on Breakout Header

- **Severity:** Informational
- **Category:** Phase 2 — Pin-Level Audit
- **Location:** J6 (GPIO header pins 4 and 7); U1 GPIO2, GPIO8
- **Description:**
  Strapping pins GPIO2 and GPIO8 are routed to the GPIO breakout header
  (J6). External devices connected to these pins could override the
  10 kΩ pull-up resistors (R3, R4) during power-on reset, causing
  incorrect boot mode selection. For example, an external sensor or
  actuator that pulls GPIO2 or GPIO8 LOW at power-on would prevent
  normal SPI flash boot.

- **Evidence:**
  - hw-schematic-design.md, §4.7: GPIO2 at J6 pin 4, GPIO8 at J6 pin 7
  - §4.3.2: GPIO2 and GPIO8 must be HIGH at boot

- **Root Cause:** Design choice to maximize GPIO availability on the
  header.

- **Impact:** Informational — this is a known tradeoff. The 10 kΩ
  pull-ups are strong enough that most typical loads (sensor inputs,
  LED indicators) won't override them. Users connecting low-impedance
  loads to these pins should be warned in the documentation.

- **Remediation:** Add a note to the GPIO header silkscreen or
  documentation: "GPIO2 and GPIO8 are boot strapping pins — do not pull
  LOW at power-on." No circuit change needed.

- **Confidence:** High

---

### Finding F-014: No Dedicated VBUS Detection Circuit

- **Severity:** Informational
- **Category:** Phase 3 — Bus Integrity
- **Location:** J4 (USB-C), U1 (ESP32-C3)
- **Description:**
  The USB specification recommends that a device detect VBUS presence
  before asserting the D+ pull-up for enumeration. The design has no
  GPIO connected to VBUS for firmware-level detection of USB connection.

  The ESP32-C3 internal USB Serial/JTAG peripheral handles D+ pull-up
  assertion automatically via hardware, and the SoC can detect USB
  connection through the USB peripheral's status registers. A dedicated
  VBUS sense GPIO is not strictly required for basic USB device
  operation.

- **Evidence:**
  - hw-schematic-design.md: no VBUS sense GPIO assigned
  - ESP32-C3 datasheet: internal USB D+ pull-up is software-controlled

- **Root Cause:** Design simplification — VBUS detection handled
  internally by ESP32-C3.

- **Impact:** None for standard USB device operation. If firmware needs
  to distinguish "USB connected" vs "battery only" at the hardware
  level (e.g., for charge management or power mode selection), a VBUS
  sense pin would be needed.

- **Remediation:** No action needed for current requirements. If VBUS
  detection becomes a firmware requirement, route VBUS through a voltage
  divider (e.g., 100 kΩ / 100 kΩ) to a spare GPIO.

- **Confidence:** High

---

### Finding F-015: Layout Carry-Forward Items

- **Severity:** Informational
- **Category:** Phase 7 — Completeness Check
- **Location:** Board-level
- **Description:**
  The following items must be verified during PCB layout review (out of
  scope for schematic-level audit):

  1. **Decoupling placement:** C6 (100 nF) must be within 3 mm of U1
     VDD pin. C3 must be within 5 mm of U2 VIN pin. C4/C5 within 5 mm
     of U2 VOUT pin.
  2. **Antenna keepout:** No copper (traces, pours, vias) on either
     layer within the ESP32-C3-MINI-1 antenna keepout zone. Module at
     board edge with antenna extending to board boundary.
  3. **USB D+/D- routing:** Differential pair routing with 90 Ω
     impedance control. Matched trace lengths. USBLC6-2SC6 placed as
     close to J4 as possible.
  4. **ESD device placement:** USBLC6-2SC6 (U3) must be placed
     physically adjacent to J4, before the series resistors R1/R2 in
     the signal path.
  5. **Ground pour:** Continuous ground pour on bottom layer. No ground
     plane splits under the USB differential pair or I2C traces.
  6. **Thermal relief:** U2 (MCP1700) GND pad should have adequate
     thermal relief or direct copper connection for heat dissipation.

- **Evidence:** Standard PCB layout requirements from component
  datasheets and the hw-requirements.md manufacturing constraints.

- **Root Cause:** These are inherently layout-phase items, not schematic
  issues.

- **Impact:** Critical for board functionality — incorrect layout of
  any of these items can cause the board to fail despite a correct
  schematic.

- **Remediation:** Create a layout checklist from these items and
  verify each during the layout review phase.

- **Confidence:** High

---

## 5. Root Cause Analysis

No single root cause underlies all findings. The findings cluster into
three themes:

1. **Requirements-vs-design gap (F-001):** The LDO selection was driven
   by deep-sleep current optimization (HW-0400), creating a conflict
   with the HW-0102 current requirement. This is a classic requirements
   tradeoff that should be resolved by adjusting one requirement with an
   explicit engineering rationale.

2. **Incomplete protection coverage (F-003, F-004, F-006, F-008,
   F-012):** ESD protection was implemented for the USB interface but
   not extended to other external connectors (Qwiic, GPIO header).
   Battery overcurrent protection was omitted. These are common
   first-draft omissions in minimal BOM designs.

3. **Circuit design error (F-002):** The P-FET gate resistor network
   creates an unintended voltage divider. This appears to be a
   one-off design error rather than a systematic issue.

---

## 6. Remediation Plan

| Priority | Finding | Fix Description | Effort | Risk |
|----------|---------|-----------------|--------|------|
| 1 | F-002 | Remove R10 or change to 1 kΩ to restore full gate drive | S | Low |
| 2 | F-001 | Revise HW-0102 to 250 mA continuous with documented bulk-cap strategy, or select higher-current LDO | S | Low |
| 3 | F-005 | Add 1 µF cap on SENSOR\_3V3 rail | S | Low |
| 4 | F-007 | Correct MCP1700 dropout spec from 500 mV to 350 mV in doc | S | None |
| 5 | F-003 | Add ESD protection IC on I2C lines at Qwiic connectors | S | Low |
| 6 | F-006 | Add PTC fuse on battery input (500 mA–1 A) | S | Low |
| 7 | F-008 | Verify SS14 leakage at 60 °C; add line item to sleep budget | S | Low |
| 8 | F-004 | Add optional (DNP) TVS on GPIO header signals | M | Low |
| 9 | F-010 | Add test point pads on 3V3, GND, SENSOR\_3V3, VIN | S | None |
| 10 | F-009 | Document USB shield-to-GND connection topology | S | None |
| 11 | F-012 | Add VBUS TVS clamp or document approved USB adapters | S | Low |
| 12 | F-011 | No action needed (keyed connector, high-Z path) | — | — |
| 13 | F-013 | Add documentation note for strapping pins on header | S | None |
| 14 | F-014 | No action needed (ESP32-C3 handles internally) | — | — |
| 15 | F-015 | Create layout review checklist from carry-forward items | S | None |

**Effort key:** S = Small (< 1 hour), M = Medium (1–4 hours)

---

## 7. Prevention

- **Requirements review gate:** Before component selection, cross-check
  every selected component's key specifications against the quantitative
  acceptance criteria in the requirements document. A simple checklist
  ("does the LDO meet the current spec?") would have caught F-001.

- **Protection checklist:** For every connector in the design, ask:
  (a) Is it external-facing? (b) Does it have ESD protection?
  (c) Does it have overcurrent protection? This prevents the class of
  omissions in F-003, F-004, and F-006.

- **Gate drive verification:** For every MOSFET switch circuit, compute
  the actual Vgs at the gate under all drive conditions. Verify that
  Vgs provides adequate enhancement (Rds(on) at actual Vgs, not just
  the datasheet headline number).

- **Sleep budget audit at temperature:** Include temperature-dependent
  leakage terms (Schottky diodes, MOSFET gate leakage, ESD device
  leakage) in the deep sleep current budget. Evaluate at both room
  temperature and the maximum operating temperature.

- **Datasheet citation verification:** When citing datasheet values in
  design documents, record the exact datasheet document number and
  revision. This makes verification straightforward and prevents
  errors like F-007.

---

## 8. Open Questions

1. **SS14 reverse leakage at 60 °C:** The exact reverse leakage current
   of the SS14 at 60 °C and 3.25 V reverse bias is needed to close
   F-008. **Action:** Obtain the full SS14 datasheet and read the
   reverse current vs. temperature curve. If leakage exceeds 5 µA at
   60 °C, evaluate alternative diodes (BAT54, PMEG3010) or a P-FET
   ideal diode topology.

2. **MCP1700 transient response:** Open Question Q1 from the schematic
   design document remains open — can the MCP1700 + 10 µF bulk cap
   sustain ESP32-C3 TX bursts without output droop below 3.0 V?
   **Action:** Measure on first prototype with oscilloscope on 3V3 rail
   during WiFi TX burst.

3. **Requirement HW-0102 resolution:** The 500 mA continuous
   requirement conflicts with the 250 mA MCP1700 selection. This must
   be resolved as either a requirements change or a component change
   before schematic capture proceeds. **Action:** Stakeholder decision
   needed.

4. **ESP32-C3-MINI-1 pin mapping verification:** The pin assignment
   table in §4.3.1 uses module pin numbers that should be verified
   against the specific ESP32-C3-MINI-1-N4 datasheet revision. Multiple
   datasheet revisions exist with minor pin numbering differences.
   **Action:** Cross-reference against the current Espressif datasheet
   revision during KiCad symbol creation.

---

## 9. Requirements Cross-Reference

| Requirement | Priority | Status | Notes |
|-------------|----------|--------|-------|
| HW-0100 | Must | **MET** | ESP32-C3-MINI-1-N4, 4 MB, all GPIOs routed |
| HW-0101 | Must | **MET** | USB-C with BOOT/RESET buttons, espflash compatible |
| HW-0102 | Must | **PARTIAL** | 3.3 V ✓, Iq ✓, reverse protection ✓, **250 mA < 500 mA required** (F-001) |
| HW-0103 | Should | **MET** | JST-PH, ADC monitoring, safe coexistence, high-Z sense |
| HW-0200 | Must | **MET** | 2× Qwiic, 4.7 kΩ pull-ups, pin mapping documented |
| HW-0201 | Should | N/A | Out of scope for minimal-qwiic variant |
| HW-0202 | Should | N/A | Out of scope for minimal-qwiic variant |
| HW-0203 | Must | **MET** | 2×5 header, 8 GPIOs, silkscreen labels |
| HW-0204 | Should | **MET** | GPIO0 (battery ADC), GPIO1 (header ADC) |
| HW-0300 | Should | N/A | Out of scope for minimal-qwiic variant |
| HW-0301 | Should | N/A | Out of scope for minimal-qwiic variant |
| HW-0400 | Must | **MET**\* | 9.4 µA worst-case at 25 °C (\*see F-008 re: temperature) |
| HW-0401 | Should | **MET** | Si2301 P-FET gate, solder jumper bypass |
| HW-0500 | Should | **MET** | 50 × 30 mm target (layout verification needed) |
| HW-0501 | Must | **MET** | 2-layer, JLCPCB-compatible LCSC parts |
| HW-0502 | Must | **MET** | Antenna keepout documented (layout carry-forward) |
| HW-0601 | Must | **MET** | BOM ≈ $3.65 ≤ $5 target |

---

## 10. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-29 | Copilot (AI-assisted review) | Initial 7-phase schematic review |
