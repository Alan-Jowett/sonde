<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Carrier Board — Schematic Design

> **Document status:** Draft v0.1  
> **Parent:** [carrier-board-requirements.md](carrier-board-requirements.md)  
> **Scope:** Circuit design, component selection rationale, schematic topology,
> PCB layout guidelines, and power analysis.

---

## 1  Block Diagram

```
                  ┌──────────────────────────────────────┐
                  │            CARRIER BOARD             │
                  │                                      │
 ┌─────────┐      │      ┌──────────┐                    │
 │ 2×AA    ├─VBAT─┼─[FB1]┤ XIAO     │                    │
 │ Battery │      │ [C3] │ ESP32-C3 │                    │
 └────┬────┘      │ 10µF │ (3V3 pin)│                    │
      │           │      │          │                    │
      │           │      │ GPIO4────┼──►┐                │
      │           │      │ GPIO6────┼───┤                │
      │           │      │ GPIO7────┼───┤                │
      │           │      │ GPIO3────┼───┼───►┐           │
      │           │      │ GPIO2────┼─┐ │    │           │
      │           │      └──────────┘ │ │    │           │
      │           │                   │ │    │           │
      │           │   ┌──────────┐    │ │    │           │
      │           │   │ P-FET    │◄───┘ │    │           │
      │           │   │ Si2301   │      │    │           │
      │           │   └────┬─────┘      │    │           │
      │           │        │            │    │           │
      │           │     SENSOR_V        │    │           │
      │           │   ┌────┴──────┐     │    │           │
      │           │   │ │  │  │ │ │     │    │           │
      │           │  R1 R2 R3 R5 R6     │    │           │
      │           │ 4k7 4k7 4k7 100k    │    │           │
      │           │   │  │  │  └┬┘      │    │           │
      │           │  SDA SCL DQ ADC     │    │           │
      │           │   │  │  │          ─┘    │           │
      │           │   J1 │  J2               │           │
      │           │ Qwiic│ 1Wire             │           │
      │           │      │                   │           │
      │           └──────┴───────────────────┘           │
      │                                                  │
      └──────────────────── J3 (Battery) ────────────────┘
```

---

## 2  Circuit Description

### 2.1  Power Supply (Direct Battery with Inrush Protection)

```
VBAT ───[FB1 ferrite]───┬───[C3 10µF]───GND
                        │
                        └───► XIAO 3V3 pin
```

**Topology:** Battery voltage is fed through a series ferrite bead (FB1) to the
XIAO ESP32-C3 3V3 pin. There is no LDO regulator. The XIAO's onboard 5V
regulator is bypassed (the 5V pin is not connected).

**Ferrite bead (FB1):** 0402 package, 120Ω @ 100 MHz, DC resistance ~0.08Ω.
The ferrite bead dampens high-frequency transients during battery insertion
(lead inductance ringing, contact bounce). At DC and low frequencies, it
presents near-zero impedance — no voltage drop during steady-state operation
or TX bursts.

**Voltage range:** 2×AA lithium cells provide ~3.6 V (fresh) to ~2.4 V
(near-depleted). The ESP32-C3 operating range is 3.0–3.6 V. Below 3.0 V
the MCU will brown out — this is the correct end-of-battery behavior.

**Accepted risk:** Energizer L91 open-circuit voltage is ~1.80–1.82 V/cell =
3.60–3.64 V for 2 cells, which is at the ESP32-C3 absolute maximum rating.
Under any load the voltage drops below 3.6 V. The ferrite bead + 10 µF cap
absorb insertion transients.

**Decoupling:** 10 µF ceramic (C3) near the XIAO 3V3 pin. X5R dielectric,
6.3 V or 10 V rating. The larger capacitance (vs typical 1 µF) serves two
purposes: (1) absorbing inrush energy during battery insertion, and (2)
providing charge reservoir during brief ESP-NOW TX bursts (~130 mA, <5 ms).

### 2.2  Power Gating (P-FET Load Switch)

```
VBAT (direct) ──┬──[R4 100kΩ]──┬──► Q1 GATE
                │               │
                │          GPIO4 (D2)
                │
                ├──► Q1 SOURCE (Si2301CDS)
                │
                │    Q1 DRAIN ──┬──[C4 1µF]──GND
                │               │
                │               └──► SENSOR_V rail
                │
                └── (pull-up ensures OFF by default)
```

**Component:** Si2301CDS-T1-GE3 (SOT-23, P-channel MOSFET)
- Vds(max): −20 V
- Id(max): −3.1 A
- Rds(on): 142 mΩ @ Vgs = −2.5 V; 112 mΩ @ Vgs = −4.5 V
- Vgs(th): −0.4 V to −1.0 V (full enhancement at Vgs = −2.5 V)
- Gate leakage: ~10 nA (negligible)

**Operation:**
- **P-FET OFF** (sensors unpowered): GPIO4 = HIGH (VBAT) or floating.
  R4 (100 kΩ) pulls gate to VBAT = source. Vgs = 0 V. MOSFET is OFF.
- **P-FET ON** (sensors powered): GPIO4 = LOW (0 V).
  Vgs = 0 − VBAT ≈ −3.0 to −3.6 V. Full enhancement. Rds(on) ≈ 112 mΩ.
  Voltage drop at 10 mA load: 1.1 mV (negligible).
  SENSOR_V ≈ VBAT.

**Deep sleep:** GPIO4 is driven HIGH and held via `gpio_hold_en(GPIO_NUM_4)`.
The 100 kΩ pull-up provides a fail-safe: during boot or reset when GPIO is
floating (high-impedance), the pull-up ensures the MOSFET stays OFF, preventing
uncontrolled sensor power-on.

**SENSOR_V decoupling:** 1 µF ceramic (C4) stabilizes the gated rail during
sensor power-on transients (inrush current to Qwiic devices).

### 2.3  I2C Bus (Qwiic Connectors)

```
SENSOR_V ────┬──[R1 4.7kΩ]──┬── SDA (GPIO6) ──► J1.3
             │               │
             ├──[R2 4.7kΩ]──┬── SCL (GPIO7) ──► J1.4
             │
             ├──► J1.2 (VCC)
             │
GND ─────────┴──► J1.1 (GND)
```

**Pull-up resistors:** 4.7 kΩ is the standard value for I2C at 3.3 V / 100–400 kHz.
Both pull-ups are powered from SENSOR_V, so they draw zero current when gated OFF.

**Bus topology:** The Qwiic connector provides a single I2C port. Multiple sensors
are supported via Qwiic daisy-chaining (each Qwiic board has two connectors for
pass-through) and 7-bit I2C addressing.

### 2.4  1-Wire Bus

```
SENSOR_V ────┬──[R3 4.7kΩ]──┬── DQ (GPIO3) ──► J2.2
             │               │
             ├──► J2.3 (VCC)
             │
GND ─────────┴──► J2.1 (GND)
```

**Pull-up:** 4.7 kΩ is the standard 1-Wire pull-up value at 3.3 V.
Powered from SENSOR_V for zero sleep leakage.

**Bus topology:** The JST XH connector provides a single 1-Wire port. Multiple
sensors are supported via multi-drop (1-Wire uses 64-bit ROM addresses for
device differentiation). DS18B20 and similar sensors are natively multi-drop
compatible.

### 2.5  Battery Voltage Sensing

```
SENSOR_V ──[R5 100kΩ]──┬──[R6 100kΩ]── GND
                        │
                        ├──[C5 100nF]── GND
                        │
                        └──► GPIO2 (ADC, D0/A0)
```

**Divider ratio:** 1:2 (equal resistors). V_adc = SENSOR_V / 2 ≈ VBAT / 2.

Since SENSOR_V ≈ VBAT (only ~1 mV drop across the P-FET), the divider accurately
reads battery voltage when the P-FET is ON.

| Battery state | VBAT | V_adc |
|---------------|------|-------|
| Fresh (2×AA lithium) | 3.6 V | 1.80 V |
| Nominal | 3.0 V | 1.50 V |
| Near-depleted | 2.4 V | 1.20 V |

**ADC range:** The ESP32-C3 ADC with 11 dB attenuation reads 0–2.5 V, so all
values are within range.

**Accuracy considerations:** The Thévenin source impedance is 50 kΩ (100 kΩ ∥ 100 kΩ).
The 100 nF capacitor (C5) acts as a charge reservoir for the SAR ADC sampling.
Time constant: τ = 50 kΩ × 100 nF = 5 ms. Firmware should wait ≥ 50 ms
after enabling the P-FET before sampling for full accuracy.

**Sleep leakage:** Zero. The divider is powered from SENSOR_V which is gated OFF
during deep sleep. This is the key advantage of the no-LDO topology — the divider
can sit on the gated rail because SENSOR_V ≈ VBAT when the P-FET is ON.

### 2.6  XIAO Module Interface

```
                     XIAO ESP32-C3 (top view, USB-C at bottom)
                  ┌──────────────────────────────┐
                  │  [u.FL]          antenna end │
                  │                              │
          J6      │                              │      J7
  D0/GPIO2  ○─────┤ 1                         14 ├──────○ D10/GPIO10
  D1/GPIO3  ○─────┤ 2                         13 ├──────○ D9/GPIO9
  D2/GPIO4  ○─────┤ 3                         12 ├──────○ D8/GPIO8
  D3/GPIO5  ○─────┤ 4                         11 ├──────○ D7/GPIO20
  D4/GPIO6  ○─────┤ 5                         10 ├──────○ D6/GPIO21
  D5/GPIO7  ○─────┤ 6                          9 ├──────○ 5V (NC)
       3V3  ○─────┤ 7                          8 ├──────○ GND
                  │            USB-C             │
                  └──────────┐      ┌────────────┘
                             └──────┘
  J6: 1×7 female header (left side)
  J7: 1×7 female header (right side)
```

**Notes:**
- Pin 7 (3V3) receives VBAT through the ferrite bead (no LDO).
- Pin 9 (5V) is **not connected** — we bypass the XIAO's onboard regulator.
- Pin 8 (GND) is the primary ground connection.
- All signal pins (D0–D10) are directly routed to the carrier board circuits
  as specified in the GPIO pin mapping table (see requirements §3).

---

## 3  PCB Layout Guidelines

### 3.1  Board Dimensions

- **Target size:** 25 mm × 35 mm.
- **Layer stackup:** 2-layer, 1.6 mm FR-4, 1 oz copper, HASL finish.
- **Board outline:** Rectangular with rounded corners (r = 1 mm).
- **No mounting holes.** Board secured via enclosure features (snap-fit,
  adhesive, slot-in rails).

### 3.2  Component Placement Strategy

```
          USB-C accessible
          ┌──────┐
     ┌────┘      └────────────┐
     │                        │
J1 ◄─┤  ┌──────────────────┐  ├─► J2
Qwiic│  │ J6   XIAO    J7  │  │1Wire
     │  │                  │  │
J3 ◄─┤  └──────────────────┘  │
Batt │  FB1 Q1 R1-R6 C3-C5    │
     │       (u.FL end)       │
     └────────────────────────┘

  25mm tall × 35mm wide
  J1, J3: left edge, right-angle outward
  J2: right edge, right-angle outward
  USB-C extends beyond top edge
```

### 3.3  Placement Rules

1. **XIAO module:** Center-top of board, USB-C extending beyond (or flush
   with) the top edge for cable access. The u.FL antenna connector at the
   bottom end of the XIAO must not be obstructed.
2. **Ferrite bead (FB1):** Near the battery connector (J3) and XIAO 3V3 pin.
   Bulk decoupling cap (C3, 10 µF) within 3 mm of FB1 output / XIAO 3V3.
3. **P-FET (Q1):** Near the XIAO, close to the SENSOR_V distribution point.
4. **Qwiic connector (J1):** Left board edge, right-angle, housing outward.
   I2C pull-ups (R1, R2) nearby.
5. **1-Wire connector (J2):** Right board edge, right-angle, housing outward.
   1-Wire pull-up (R3) nearby.
6. **Battery connector (J3):** Left board edge, below J1, right-angle,
   housing outward.
7. **Battery divider (R5, R6, C5):** Near Q1 drain (SENSOR_V rail).
8. **SMD passives:** Bottom area of board (below XIAO), or on back side.

### 3.4  Routing Guidelines

- **Ground plane:** Continuous copper pour on the back layer (GND).
  This provides low-impedance return path and EMI shielding.
- **VBAT rail:** Route as a wide trace (0.5 mm minimum) from FB1 output to
  the XIAO 3V3 pin and to Q1 source.
- **SENSOR_V:** Route from Q1 drain to the pull-ups and connector
  VCC pins. Wide trace (0.5 mm) for low resistance.
- **Signal traces:** 0.25 mm minimum width for GPIO signals.
- **Trace clearance:** 0.2 mm minimum (JLCPCB standard capability).
- **Via size:** 0.3 mm drill, 0.6 mm annular ring.

### 3.5  Antenna and USB Access

The XIAO ESP32-C3 (u.FL variant) has an I-PEX/u.FL connector at the top end
of the module (opposite from USB-C). The carrier board must:

- **Not obstruct the u.FL connector.** The carrier board can extend past the
  XIAO's antenna end, but must have a cutout or clearance for the u.FL
  connector and cable.
- **Not obstruct USB-C.** The USB-C port at the bottom of the XIAO must be
  accessible for programming and power. The carrier board should either
  have a notch/cutout or the XIAO's USB-C end should extend beyond the
  carrier board edge.
- **No PCB antenna keep-out is required** since the u.FL variant has no
  on-board antenna. Copper pour and traces can extend under the XIAO's
  antenna end without concern.

---

## 4  Schematic Netlist Summary

| Net Name | Connected To |
|----------|-------------|
| VBAT | J5.1 (+), FB1 input |
| VBAT_F | FB1 output, C3, R4 (top), Q1 Source, J6/J7 3V3 pin |
| GND | J5.2 (−), C3, C4, C5, R6 (bottom), J1.1, J2.1, J3.1, J4.1, J6/J7 GND pin |
| SENSOR_V | Q1 Drain, C4, R1 (top), R2 (top), R3 (top), R5 (top), J1.2, J2.3 |
| SENSOR_EN | R4 (bottom), Q1 Gate, J6 pin 3 (GPIO4) |
| SDA | R1 (bottom), J1.3, J6 pin 5 (GPIO6) |
| SCL | R2 (bottom), J1.4, J6 pin 6 (GPIO7) |
| OW_DQ | R3 (bottom), J2.2, J6 pin 2 (GPIO3) |
| VBAT_SENSE | R5 (bottom), R6 (top), C5, J6 pin 1 (GPIO2) |
| NC_5V | J7 pin 6 (XIAO 5V pin) — not connected |

---

## 5  Component Reference Table

| Ref | Component | Value | Package | LCSC P/N |
|-----|-----------|-------|---------|----------|
| FB1 | Ferrite bead | 120Ω@100MHz | 0402 | C76712 |
| Q1 | Si2301CDS-T1-GE3 | P-FET | SOT-23 | C10487 |
| R1 | Resistor | 4.7 kΩ | 0402 | C25900 |
| R2 | Resistor | 4.7 kΩ | 0402 | C25900 |
| R3 | Resistor | 4.7 kΩ | 0402 | C25900 |
| R4 | Resistor | 100 kΩ | 0402 | C25741 |
| R5 | Resistor | 100 kΩ | 0402 | C25741 |
| R6 | Resistor | 100 kΩ | 0402 | C25741 |
| C3 | Capacitor | 10 µF / 10V | 0805 X5R | C15850 |
| C4 | Capacitor | 1 µF / 10V | 0402 X5R | C52923 |
| C5 | Capacitor | 100 nF / 10V | 0402 X7R | C307331 |
| J1 | Qwiic connector | JST SH 4-pin | SM04B-SRSS-TB | C160404 |
| J2 | 1-Wire connector | JST XH 3-pin RA | S3B-XH-A | C157928 |
| J3 | Battery connector | JST PH 2-pin RA | S2B-PH-K-S | C157932 |
| J6 | XIAO left header | 1×7 female 2.54mm | — | — |
| J7 | XIAO right header | 1×7 female 2.54mm | — | — |
