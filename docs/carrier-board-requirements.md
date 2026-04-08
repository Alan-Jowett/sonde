<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Carrier Board — Requirements Specification

> **Document status:** Draft v0.1  
> **Scope:** Hardware requirements for the sonde carrier board (codename: *sonde-carrier*).  
> **Target:** A 2-layer PCB that hosts a Seeed Studio XIAO ESP32-C3 module and provides
> connectors for I2C sensors (Qwiic), 1-Wire sensors, and a 2×AA lithium battery pack.  
> **Design constraint:** Retail price target ≤ $5 (BOM + assembly ≤ ~$3 at qty 100).  
> **Power note:** The carrier board feeds battery voltage directly to the XIAO 3V3
> pin (no LDO). This means 2×AA lithium cells (up to 3.66 V open-circuit) are
> applied directly to the ESP32-C3 VDD, which is at the absolute maximum rating.
> This is an accepted trade-off for simplicity and cost. See CB-0301.

---

## 1  Overview

The sonde carrier board is a small, open-source PCB designed so that any
contributor can download the manufacturing files and order boards from JLCPCB
(or a compatible fab). The board accepts a XIAO ESP32-C3 module via
through-hole DIP-style headers and provides:

- Regulated 3.3 V power from 2×AA lithium cells.
- Power-gated sensor bus for ultra-low deep-sleep current.
- Two Qwiic (I2C) connectors and two 1-Wire connectors.
- Battery voltage sensing via ADC.

The XIAO module is **not** part of the carrier board BOM — the user supplies it
separately.

---

## 2  Requirements

### 2.1  MCU Interface

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0100 | The board SHALL accept a Seeed Studio XIAO ESP32-C3 module via two 1×7 female pin headers at 2.54 mm pitch, spaced 17.5 mm center-to-center (matching the XIAO form factor). | Must |
| CB-0101 | The XIAO module SHALL plug in with pins soldered to the module and inserted into the carrier board headers (DIP-socket style, no soldering to carrier). | Must |
| CB-0102 | The board SHALL route power to the XIAO 3V3 pin, bypassing the XIAO's onboard 5 V regulator entirely. The XIAO 5V/VIN pin SHALL NOT be connected to the carrier board power rail. | Must |
| CB-0103 | The board SHALL connect to the XIAO GND pin(s). | Must |

### 2.2  Connectors

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0200 | The board SHALL provide 1 Qwiic-compatible connector (JST SH 4-pin, 1.0 mm pitch, SM04B-SRSS-TB). Connector SHALL be placed at a board edge with housing facing outward for horizontal cable exit. Qwiic supports daisy-chaining for multiple sensors off one port. | Must |
| CB-0201 | Qwiic connector pinout SHALL follow the SparkFun Qwiic standard: pin 1 = GND, pin 2 = VCC (SENSOR_V), pin 3 = SDA, pin 4 = SCL. | Must |
| CB-0202 | I2C bus signals: SDA = GPIO6, SCL = GPIO7. | Must |
| CB-0203 | The board SHALL provide 1 connector for 1-Wire sensors using a JST XH 3-pin **right-angle** header (2.5 mm pitch, S3B-XH-A). Connector SHALL be at a board edge with housing facing outward. 1-Wire supports multi-drop for multiple sensors on one bus. | Must |
| CB-0204 | 1-Wire connector pinout SHALL be: pin 1 = GND, pin 2 = DATA, pin 3 = VCC (SENSOR_V). | Must |
| CB-0205 | 1-Wire data line: GPIO3. | Must |
| CB-0206 | The board SHALL provide a battery input connector using a JST PH 2-pin **right-angle** header (2.0 mm pitch, S2B-PH-K-S). Pin 1 = VCC (+), pin 2 = GND (−). Connector SHALL be at a board edge with housing facing outward. | Must |

### 2.3  Power

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0300 | The board SHALL accept 2×AA lithium batteries (nominal 3.0 V, max 3.6 V fresh, min ~1.8 V depleted) via the JST PH battery connector. | Must |
| CB-0301 | The board SHALL feed battery voltage (VBAT) through a series ferrite bead (FB1) to the XIAO 3V3 pin, bypassing the XIAO's onboard 5 V regulator. **No LDO is used.** This means VDD varies with battery state (2.4–3.6 V). **Accepted risk:** 2×AA lithium cells can produce up to 3.64 V open-circuit, which is at the ESP32-C3 absolute maximum rating of 3.6 V. Under any load this drops below 3.6 V. The ferrite bead dampens battery insertion transients (lead inductance ringing). | Must |
| CB-0302 | A 10 µF ceramic decoupling capacitor (C3) SHALL be placed near the XIAO 3V3 pin. The larger capacitance (vs 1 µF) absorbs inrush energy during battery insertion and provides charge reservoir during brief TX bursts. | Must |
| CB-0303 | No reverse polarity protection is required — the polarized JST PH connector provides mechanical keying. | Info |
| CB-0304 | No power switch is required — power is controlled by connecting/disconnecting the battery pack. | Info |

### 2.4  Power Gating (Deep Sleep)

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0400 | The board SHALL include a P-channel MOSFET high-side load switch (Si2301CDS-T1-GE3, SOT-23) to gate a SENSOR_V power rail. | Must |
| CB-0401 | The SENSOR_V rail SHALL power: Qwiic VCC pins, 1-Wire VCC pins, I2C pull-up resistors, the 1-Wire pull-up resistor, and the battery voltage divider. | Must |
| CB-0402 | The P-FET gate SHALL be controlled by GPIO4 (XIAO pin D2). GPIO4 has RTC hold capability for maintaining state through deep sleep. | Must |
| CB-0403 | A 100 kΩ pull-up resistor from the P-FET gate to VBAT SHALL ensure the MOSFET defaults to OFF (sensors unpowered) during boot, reset, and deep sleep. | Must |
| CB-0404 | Firmware drives GPIO4 LOW to turn on SENSOR_V (Vgs ≈ −VBAT, full enhancement) and HIGH to turn off. | Info |
| CB-0405 | The total carrier board deep sleep current (excluding the XIAO module) SHALL be ≤ 20 µA. Design target is < 1 µA. | Must |
| CB-0406 | The SENSOR_V rail SHALL include a 1 µF decoupling capacitor. | Must |

### 2.5  Sensor Interface

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0500 | The I2C bus (SDA, SCL) SHALL have 4.7 kΩ pull-up resistors to SENSOR_V (gated rail). | Must |
| CB-0501 | The 1-Wire data line SHALL have a 4.7 kΩ pull-up resistor to SENSOR_V (gated rail). | Must |
| CB-0502 | All pull-up resistors being on the gated SENSOR_V rail ensures zero leakage in deep sleep. | Info |

### 2.6  Battery Voltage Sensing

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0600 | The board SHALL include a resistor voltage divider on the SENSOR_V rail (gated) for battery voltage sensing. | Must |
| CB-0601 | The divider midpoint SHALL connect to GPIO2 (XIAO pin D0, ADC-capable). | Must |
| CB-0602 | The divider SHALL use 100 kΩ / 100 kΩ resistors (1:2 ratio). When SENSOR_V ≈ VBAT = 3.6 V, the ADC sees 1.8 V. At VBAT = 3.0 V, the ADC sees 1.5 V. | Must |
| CB-0603 | A 100 nF capacitor from the ADC pin to GND SHALL be included for sampling accuracy with the 50 kΩ Thévenin source impedance. Settling time: ~35 ms (7 × 5 ms). | Must |
| CB-0604 | The divider draws 0 µA in deep sleep because it is powered from the gated SENSOR_V rail. When active: I = VBAT / 200 kΩ ≈ 16.5 µA (negligible during wake). | Info |
| CB-0605 | Firmware must enable the P-FET (SENSOR_EN LOW), wait ≥ 50 ms for rail and divider to settle, then read GPIO2 ADC. The ADC value represents VBAT / 2. | Info |

### 2.7  Physical

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0700 | The PCB SHALL be 2-layer (front copper + back copper). | Must |
| CB-0701 | The board SHALL be as compact as feasible. Target: ≤ **25 mm × 35 mm**. | Should |
| CB-0702 | The board SHALL NOT include mounting holes. Board is secured in an enclosure via snap-fit clips, adhesive, friction fit, or slot-in rails. | Must |
| CB-0703 | The board SHALL NOT include any LEDs (power budget priority). | Must |
| CB-0704 | Silkscreen SHALL label all connectors (J1–J3, J6–J7), polarity markings, and the sonde project name. | Should |
| CB-0705 | The XIAO module uses the **u.FL external antenna variant** (no PCB antenna). The carrier board SHALL provide clearance for the u.FL connector and antenna cable routing at the antenna end of the XIAO. No copper keep-out zone is required. | Must |
| CB-0706 | The XIAO SHALL be oriented so that the USB-C port extends beyond or is flush with one board edge, allowing direct USB cable access without obstruction. | Must |
| CB-0707 | All sensor and battery connectors (J1–J3) SHALL be right-angle and placed at board edges with housings facing outward, so that cables exit horizontally from the board sides. No connectors shall be on the USB-access edge or the opposite (u.FL) edge. | Must |

### 2.8  Environmental

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0800 | Operating temperature range: −20 °C to +60 °C. | Must |
| CB-0801 | All components SHALL be rated for the operating temperature range. | Must |

### 2.9  Manufacturing

| ID | Requirement | Priority |
|----|------------|----------|
| CB-0900 | The design SHALL target JLCPCB for PCB fabrication and SMT assembly. | Must |
| CB-0901 | Deliverables SHALL include: KiCad 8 project files, Gerber files (or instructions to export from KiCad), JLCPCB-format BOM (CSV), and JLCPCB-format CPL (CSV). | Must |
| CB-0902 | All SMD components SHOULD be available in the JLCPCB/LCSC parts library (basic or extended). | Should |
| CB-0903 | Through-hole connectors (JST XH, JST PH, female headers) MAY require hand soldering if JLCPCB THT assembly is not used. | Info |
| CB-0904 | The EDA source format SHALL be KiCad 8. | Must |

---

## 3  GPIO Pin Mapping

| XIAO Pin | GPIO | Function | Assignment |
|----------|------|----------|------------|
| D0 (A0) | GPIO2 | ADC, RTC | Battery voltage ADC |
| D1 (A1) | GPIO3 | ADC, RTC | 1-Wire data |
| D2 (A2) | GPIO4 | ADC, RTC | SENSOR_EN (P-FET gate) |
| D3 (A3) | GPIO5 | ADC, RTC | Spare |
| D4 | GPIO6 | I2C | I2C SDA |
| D5 | GPIO7 | I2C | I2C SCL |
| D6 | GPIO21 | UART TX | Spare |
| D7 | GPIO20 | UART RX | Spare |
| D8 | GPIO8 | SPI SCK | Spare |
| D9 | GPIO9 | BOOT | Spare (boot strapping pin) |
| D10 | GPIO10 | SPI MOSI | Spare |

**Notes:**
- GPIO4 is chosen for SENSOR_EN because it has RTC hold capability, allowing
  the pin state to be maintained through deep sleep via `gpio_hold_en()`.
- GPIO2 is chosen for battery ADC because it is an ADC1 channel with no
  Wi-Fi conflict.
- GPIO9 is a boot strapping pin (LOW = download mode). Avoid using it for
  signals that may be LOW at boot.

---

## 4  Deep Sleep Power Budget (Carrier Board Only)

| Component | Deep Sleep Current | Notes |
|-----------|--------------------|-------|
| Si2301 P-FET (off-state leakage) | ~0.01 µA | Negligible |
| Gate pull-up resistor (100 kΩ) | 0 µA | No current path when gate = source |
| Battery divider (100 kΩ + 100 kΩ) | 0 µA | On gated SENSOR_V (OFF) |
| I2C pull-ups (4.7 kΩ × 2) | 0 µA | On gated SENSOR_V (OFF) |
| 1-Wire pull-up (4.7 kΩ) | 0 µA | On gated SENSOR_V (OFF) |
| SENSOR_V decoupling (1 µF) | 0 µA | No leakage path when gated |
| **Total carrier board** | **~0.01 µA** | **99.95% margin below 20 µA** |

**Note:** Without an LDO, the carrier board contributes essentially zero to the
system deep sleep current. The entire deep sleep budget is consumed by the XIAO
module itself (ESP32-C3 RTC domain + onboard components).

---

## 5  Estimated BOM Cost (qty 100, LCSC/JLCPCB pricing)

| Component | Qty | Unit Cost | Extended | LCSC P/N |
|-----------|-----|-----------|----------|----------|
| PCB (2-layer, ~25×35 mm) | 1 | $0.20 | $0.20 | — |
| Si2301CDS-T1-GE3 (P-FET, SOT-23) | 1 | $0.065 | $0.07 | C10487 |
| Ferrite bead 0402 (120Ω@100MHz) | 1 | $0.01 | $0.01 | C76712 |
| SM04B-SRSS-TB (Qwiic, JST SH 4-pin) | 1 | $0.15 | $0.15 | C160404 |
| S3B-XH-A (1-Wire, JST XH 3-pin RA) | 1 | $0.08 | $0.08 | C157928 |
| S2B-PH-K-S (Batt, JST PH 2-pin RA) | 1 | $0.06 | $0.06 | C157932 |
| Female header 1×7 (2.54 mm) | 2 | $0.10 | $0.20 | — |
| 4.7 kΩ 0402 (I2C + 1-Wire pull-ups) | 3 | $0.005 | $0.02 | C25900 |
| 100 kΩ 0402 (gate pull-up + batt divider) | 3 | $0.005 | $0.02 | C25741 |
| 10 µF 0805 ceramic (VBAT decoup) | 1 | $0.02 | $0.02 | C15850 |
| 1 µF 0402 ceramic (sensor decoup) | 1 | $0.01 | $0.01 | C52923 |
| 100 nF 0402 ceramic (ADC filter) | 1 | $0.005 | $0.01 | C307331 |
| **Total BOM** | **~16** | | **~$0.85** | |
| JLCPCB assembly (est. qty 100) | | | ~$0.80/board | |
| **Total per board** | | | **~$1.65** | |

The XIAO ESP32-C3 module (~$5) is supplied separately by the user.

---

## 6  Connector Reference Designators

| Ref | Connector | Type | Notes |
|-----|-----------|------|-------|
| J1 | Qwiic | JST SH 4-pin (SM04B-SRSS-TB) | I2C bus, right-angle, left edge |
| J2 | 1-Wire | JST XH 3-pin RA (S3B-XH-A) | 1-Wire bus, right-angle, right edge |
| J3 | Battery | JST PH 2-pin RA (S2B-PH-K-S) | VBAT input, right-angle, left edge |
| J6 | XIAO Left | Female header 1×7 (2.54 mm) | XIAO pins D0–D6+3V3 |
| J7 | XIAO Right | Female header 1×7 (2.54 mm) | XIAO pins D7–D10+GND+5V(NC) |
