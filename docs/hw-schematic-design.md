<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Sonde Minimal Sensor Node — Schematic Design Document

> **Document status:** Draft
> **Board variant:** `minimal-qwiic`
> **Source:** Derived from [hw-requirements.md](hw-requirements.md) and [hw-design.md](hw-design.md).
> **Scope:** Component selection, schematic connectivity, and circuit design for the minimal ESP32-C3 sensor node with Qwiic I2C.

---

## 1. Overview

This document defines the schematic design for the **minimal-qwiic** board
variant of the sonde sensor node. The board pairs an ESP32-C3-MINI-1 module
with two Qwiic/STEMMA QT connectors, USB-C programming, single-cell LiPo
battery input, sensor power gating, and a GPIO breakout header. No sensors
are populated on-board — all sensing is via external Qwiic modules.

Key design goals:

1. **Ultra-low deep sleep current** (≤ 20 µA total board, per HW-0400).
2. **Plug-and-play I2C** via industry-standard Qwiic connectors (HW-0200).
3. **USB-C** for programming and power (HW-0101).
4. **Battery-powered field deployment** with LiPo input (HW-0103).
5. **JLCPCB assembly** using LCSC basic/extended parts (HW-0501).

---

## 2. Requirements Summary

This design addresses the following requirements from
[hw-requirements.md](hw-requirements.md):

| Requirement | Title | Coverage |
|-------------|-------|----------|
| HW-0100 | Microcontroller | ESP32-C3-MINI-1, 4 MB flash, all GPIOs routed |
| HW-0101 | USB-C connector | USB-C receptacle → native USB (GPIO18/19) |
| HW-0102 | Voltage regulator | MCP1700-3302E/TT, 1.6 µA Iq, 3.0–6 V input |
| HW-0103 | Battery input | JST-PH 2-pin, high-Z divider, Schottky protection |
| HW-0200 | I2C bus (Qwiic) | 2× Qwiic connectors, 4.7 kΩ pull-ups on gated rail |
| HW-0203 | GPIO breakout | 2.54 mm header with remaining GPIOs |
| HW-0204 | ADC input | GPIO0 for battery voltage, GPIO1 on header |
| HW-0400 | Deep sleep current | ≤ 20 µA budget, component-level breakdown |
| HW-0401 | Sensor power gating | P-FET high-side switch, solder jumper bypass |
| HW-0500 | Board dimensions | 50 mm × 30 mm standard variant |
| HW-0501 | Manufacturing | 2-layer, JLCPCB-compatible, 6 mil min trace/space |
| HW-0502 | Antenna keepout | No copper within module keepout zone |

**Not addressed** (out of scope for this minimal board):
HW-0201 (SPI bus), HW-0202 (1-Wire), HW-0300/HW-0301 (on-board sensors).

---

## 3. Architecture

### 3.1 High-Level Block Diagram

```
                          ┌──────────────────────────────────────────┐
                          │           BOARD: minimal-qwiic           │
                          │                                          │
  ┌─────────┐  VUSB 5V   │  ┌─────────┐   3V3    ┌──────────────┐ │
  │ USB-C   │────────────►│──│ Schottky│──────┐   │ ESP32-C3     │ │
  │ J4      │  D+/D-     │  │ D1      │      ▼   │ MINI-1  (U1) │ │
  │         │────────────►│──┤         │  ┌───────┐│              │ │
  └─────────┘             │  └─────────┘  │MCP1700││ GPIO18 ← D- │ │
                          │               │U2  LDO││ GPIO19 ← D+ │ │
  ┌─────────┐  VBAT       │  ┌─────────┐  │3.3V   ││ GPIO4  → SDA│─┼─►Qwiic J1,J2
  │ Battery │────────────►│──│ Schottky│──┘  │    ││ GPIO5  → SCL│─┼─►Qwiic J1,J2
  │ JST-PH  │             │  │ D2      │     │    ││ GPIO3  → PWR│─┤
  │ J3      │             │  └─────────┘     │    ││ GPIO0  ← ADC│─┼─►VBAT divider
  └─────────┘             │                  │    ││ GPIO9  ← BTN│─┤
                          │                  │    │└──────────────┘ │
                          │                  ▼    │                 │
                          │        ┌─────────────┐│ ┌────────────┐ │
                          │        │ P-FET Q1    ││ │ GPIO       │ │
                          │   3V3──│ Si2301      ││ │ Header J6  │ │
                          │        │→ SENSOR_3V3 ││ └────────────┘ │
                          │        └─────┬───────┘│                │
                          │              │        │                │
                          │         ┌────▼─────┐  │                │
                          │         │Qwiic VCC │  │                │
                          │         │+ Pull-ups│  │                │
                          │         └──────────┘  │                │
                          └──────────────────────────────────────────┘
```

### 3.2 Component Descriptions

#### U1 — ESP32-C3-MINI-1 Module

- **Responsibility:** MCU, WiFi/BLE radio, program execution.
- **Interfaces:** USB (GPIO18/19), I2C (GPIO4/5), ADC (GPIO0), power gate
  control (GPIO3), BOOT button (GPIO9), all remaining GPIOs to header.
- **Dependencies:** 3V3 rail, GND.
- **Constraints:** Antenna keepout zone per datasheet (HW-0502).
  Operating voltage 3.0–3.6 V. Deep sleep ≈ 5 µA typical.

#### U2 — MCP1700-3302E/TT (LDO Regulator)

- **Responsibility:** Regulate input (3.0–6.0 V) to 3.3 V.
- **Interfaces:** VIN ← VBUS (after OR-ing diodes), VOUT → 3V3 rail, GND.
- **Constraints:** 250 mA max output, 1.6 µA quiescent current (typ).
  Dropout 178 mV typ at 250 mA, 500 mV max at 250 mA.

#### U3 — USBLC6-2SC6 (ESD Protection)

- **Responsibility:** ESD protection on USB D+/D- lines.
- **Interfaces:** I/O1, I/O2 ← USB data lines; VBUS ← USB 5 V.
- **Constraints:** Low capacitance (3.5 pF max), SOT-23-6.

#### Q1 — Si2301 (P-Channel MOSFET)

- **Responsibility:** High-side switch for sensor power gating.
- **Interfaces:** Source ← 3V3, Drain → SENSOR_3V3, Gate ← GPIO3
  (via 10 kΩ pull-up resistor R9 to 3V3 and series resistor R10 from GPIO3).
- **Constraints:** Rds(on) ≈ 110 mΩ at Vgs = −4.5 V.

#### J1, J2 — Qwiic/STEMMA QT Connectors

- **Responsibility:** I2C sensor connection (daisy-chain).
- **Interfaces:** Pin 1 = GND, Pin 2 = SENSOR_3V3, Pin 3 = SDA, Pin 4 = SCL.
- **Constraints:** JST-SH 4-pin, 1.0 mm pitch, horizontal SMD.

#### J3 — Battery Connector (JST-PH 2-pin)

- **Responsibility:** Single-cell LiPo battery input.
- **Interfaces:** Pin 1 = VBAT+, Pin 2 = GND.

#### J4 — USB-C Connector

- **Responsibility:** Programming, debug, USB power input.
- **Interfaces:** VBUS, D+, D−, CC1, CC2, GND, SHIELD.

#### J6 — GPIO Breakout Header

- **Responsibility:** Expose remaining GPIOs for user wiring.
- **Interfaces:** 2.54 mm pin header, labeled with GPIO numbers.

### 3.3 Voltage Domains

| Net Name | Voltage | Source | Always-On? |
|----------|---------|--------|------------|
| `VUSB` | 5.0 V nom | USB-C VBUS | When USB connected |
| `VBAT` | 3.0–4.2 V | Battery | When battery connected |
| `VIN` | 3.0–5.5 V | OR of VUSB/VBAT via Schottky | When any source present |
| `3V3` | 3.3 V ± 3% | MCP1700 output | Always (when VIN present) |
| `SENSOR_3V3` | 3.3 V | Gated via Q1 | Only when GPIO3 active |
| `GND` | 0 V | Common ground | Always |

### 3.4 Data Flow

```
External Sensors ←──I2C──→ Qwiic J1/J2 ←──SDA/SCL──→ ESP32-C3 (GPIO4/5)
                                                          │
Host PC ←──USB──→ USB-C J4 ←──D+/D-──→ ESP32-C3 (GPIO18/19)
                                                          │
Battery ←──VBAT──→ ADC Divider ──→ ESP32-C3 ADC (GPIO0)
```

---

## 4. Detailed Design

### 4.1 Power Supply

#### 4.1.1 Power Input OR-ing (USB + Battery)

USB 5 V and battery voltage are OR-ed using Schottky diodes to prevent
back-feeding between sources. USB is preferred when present due to its
higher voltage (5 V vs 3.0–4.2 V LiPo), which naturally wins through the
diode OR network.

```
VUSB (5V) ──►|── D1 (SS14, Schottky) ──┐
                                        ├──► VIN ──► MCP1700 IN
VBAT (3.7V) ─►|── D2 (SS14, Schottky) ─┘
```

- **D1, D2:** SS14 (1N5819 equivalent), SMA package.
  Vf = 0.45 V typ at 1 A. LCSC C2480.
- **Diode drop impact:** At USB (5 V − 0.45 V = 4.55 V into LDO) —
  well above 3.3 V + 0.5 V dropout = 3.8 V minimum. At low battery
  (3.0 V − 0.45 V = 2.55 V) — below LDO dropout. This defines the
  effective battery cutoff at ~3.5 V (3.3 V + 0.5 V max dropout −
  0.45 V diode ≈ 3.35 V minimum VBAT).

**[ASSUMPTION]** Battery cutoff at 3.5 V is acceptable for single-cell
LiPo (nominal 3.7 V, full charge 4.2 V, discharge cutoff typically
3.0–3.2 V). The 3.35 V minimum VBAT provides approximately 5–10%
remaining capacity as a shutdown margin.

#### 4.1.2 LDO Voltage Regulator (U2)

**Selected part:** MCP1700-3302E/TT (Microchip)

| Parameter | Value | Source |
|-----------|-------|--------|
| Output voltage | 3.3 V ± 3% max | Datasheet guaranteed |
| Input voltage | 2.3–6.0 V | Datasheet guaranteed |
| Max output current | 250 mA | Datasheet guaranteed |
| Quiescent current | 1.6 µA typ, 4.0 µA max | Datasheet guaranteed |
| Dropout @ 250 mA | 178 mV typ, 500 mV max | Datasheet guaranteed |
| Package | SOT-23-3 | — |
| LCSC | C54447 | — |
| Unit cost | ~$0.15 | LCSC estimate |

**Decoupling:**
- **Input:** 1 µF ceramic (MLCC, X5R/X7R, 10 V) close to VIN pin.
- **Output:** 1 µF ceramic (MLCC, X5R/X7R, 10 V) + 10 µF ceramic
  (X5R, 10 V) close to VOUT pin.
- 100 nF ceramic bypass on ESP32-C3 VDD pin (within 3 mm).

**[DESIGN-ONLY]** The 10 µF bulk capacitor on the output exceeds the
MCP1700 datasheet minimum (1 µF). This provides additional transient
response margin for ESP32-C3 radio TX bursts (up to 340 mA peak per
ESP32-C3 datasheet), which exceed the MCP1700's 250 mA continuous
rating but are very short duration (< 10 ms). The bulk capacitor
supplies the difference.

#### 4.1.3 Deep Sleep Current Budget

| Component | Current (µA) | Source |
|-----------|-------------|--------|
| ESP32-C3 deep sleep (RTC timer on) | 5.0 typ | ESP32-C3 datasheet |
| MCP1700 quiescent | 1.6 typ / 4.0 max | MCP1700 datasheet |
| VBAT divider leakage (2× 10 MΩ) | 0.19 | Calculated: 3.7 V / 20 MΩ |
| GPIO pull-ups (10 kΩ EN, 10 kΩ GPIO9) | 0.0 | No current path in sleep |
| I2C pull-ups (gated rail — OFF) | 0.0 | Power gated off |
| USBLC6-2SC6 leakage | 0.15 max | Datasheet |
| Si2301 gate leakage | < 0.01 | Datasheet |
| **Total (typical)** | **≈ 7.0** | — |
| **Total (worst-case)** | **≈ 9.4** | — |

**Result:** Well within the HW-0400 requirement of ≤ 20 µA.

### 4.2 USB Section

#### 4.2.1 USB-C Connector (J4)

**Selected part:** SHOU HAN TYPE-C 16PIN 2MD(073)

| Parameter | Value |
|-----------|-------|
| Type | USB-C receptacle, 16-pin, mid-mount SMD |
| Current rating | 3 A / 5 V |
| LCSC | C2765186 |
| Unit cost | ~$0.10 |

**CC Resistors (UFP/Device):**
- R_CC1 = 5.1 kΩ, CC1 pin → GND
- R_CC2 = 5.1 kΩ, CC2 pin → GND
- These identify the device as a USB sink (UFP) and request default
  USB power (5 V / 500 mA–900 mA depending on host).

#### 4.2.2 USB Data Lines

```
USB-C D+ ──► USBLC6-2SC6 (U3) I/O1 ──► 22Ω series (R1) ──► GPIO19 (D+)
USB-C D- ──► USBLC6-2SC6 (U3) I/O2 ──► 22Ω series (R2) ──► GPIO18 (D-)
```

- **Series resistors (R1, R2):** 22 Ω, 0402. Provide impedance matching
  and limit current during ESD events. LCSC C25092.
- **ESD protection (U3):** USBLC6-2SC6, SOT-23-6. LCSC C7519. Placed
  as close to J4 as possible for effective protection.
- **USBLC6-2SC6 VBUS pin:** Connected to VUSB (5 V) via the USB-C VBUS.

#### 4.2.3 USB Power Path

```
USB-C VBUS ──► D1 (Schottky) ──► VIN ──► MCP1700
                   └──► USBLC6-2SC6 VBUS pin
```

### 4.3 ESP32-C3 Module (U1)

#### 4.3.1 Pin Assignment Table

| Module Pin | GPIO | Function | Net Name | Notes |
|------------|------|----------|----------|-------|
| 2 | GND | Ground | GND | Multiple GND pins |
| 3 | 3V3 | Power | 3V3 | Regulated supply |
| 4 | — | EN | EN | Reset with RC + button |
| 5 | GPIO0 | ADC1_CH0 | VBAT_SENSE | Battery voltage divider |
| 6 | GPIO1 | ADC1_CH1 | GPIO1 | Breakout header |
| 7 | GPIO2 | Strap (HIGH) | GPIO2 | Breakout header, 10 kΩ pull-up |
| 8 | GPIO3 | Output | SENSOR_PWR_EN | Power gate control |
| 9 | GPIO4 | I2C SDA | I2C0_SDA | Qwiic J1/J2 pin 3 |
| 10 | GPIO5 | I2C SCL | I2C0_SCL | Qwiic J1/J2 pin 4 |
| 11 | GPIO6 | GPIO | GPIO6 | Breakout header |
| 12 | GPIO7 | GPIO | GPIO7 | Breakout header |
| 13 | GPIO8 | Strap (HIGH) | GPIO8 | Breakout header, 10 kΩ pull-up |
| 14 | GPIO9 | Strap/BOOT | BOOT | BOOT button to GND |
| 15 | GPIO10 | GPIO | GPIO10 | Breakout header |
| 16 | GPIO18 | USB D− | USB_DN | USB-C data |
| 17 | GPIO19 | USB D+ | USB_DP | USB-C data |
| 18 | GPIO20 | UART0 RX | GPIO20/RX | Breakout header |
| 19 | GPIO21 | UART0 TX | GPIO21/TX | Breakout header |

#### 4.3.2 Strapping Pins

Per the ESP32-C3 datasheet, strapping pin states are sampled at reset:

| Pin | Required State | Implementation |
|-----|---------------|----------------|
| GPIO2 | HIGH (SPI boot) | 10 kΩ pull-up to 3V3 (R3) |
| GPIO8 | HIGH (default) | 10 kΩ pull-up to 3V3 (R4) |
| GPIO9 | HIGH = SPI boot, LOW = download | 10 kΩ pull-up to 3V3 (R5) + BOOT button to GND |

**BOOT button (SW1):** Pressing SW1 pulls GPIO9 LOW during reset,
entering USB/UART download mode for firmware flashing via `espflash`.
Per audit finding F-001.

**[INFERRED]** GPIO2 and GPIO8 must be HIGH at boot for normal SPI flash
boot. GPIO9 LOW at reset enters download mode (per ESP32-C3 TRM;
see audit finding F-001 for confidence notes).

#### 4.3.3 Reset Circuit (EN Pin)

```
3V3 ──── R6 (10kΩ) ──┬── EN pin (U1)
                      │
                      ├── C1 (100nF) ── GND     (debounce)
                      │
                      └── SW2 (RESET) ── GND    (manual reset)
```

- **R6:** 10 kΩ pull-up to 3V3. Ensures EN is HIGH during normal operation.
- **C1:** 100 nF ceramic. Debounces the reset line and provides a
  controlled rise time (~1 ms time constant with 10 kΩ).
- **SW2:** Tactile pushbutton to GND. Per audit finding F-001.

#### 4.3.4 Antenna Keepout

Per HW-0502 and the ESP32-C3-MINI-1 datasheet:
- No copper (traces, pours, vias) within the antenna area on
  **both layers** of the PCB.
- The antenna extends beyond the module footprint edge — a keepout
  zone of at least 5 mm from the board edge must have no ground plane.
- The module should be placed at the board edge with the antenna
  extending to or beyond the PCB boundary.

### 4.4 I2C / Qwiic Section

#### 4.4.1 Qwiic Connector Wiring (J1, J2)

Both connectors are wired in parallel (daisy-chain):

| Qwiic Pin | Signal | Net Name |
|-----------|--------|----------|
| 1 (Black) | GND | GND |
| 2 (Red) | VCC | SENSOR_3V3 |
| 3 (Blue) | SDA | I2C0_SDA |
| 4 (Yellow) | SCL | I2C0_SCL |

**Selected part:** JST SM04B-SRSS-TB (horizontal SMD)

| Parameter | Value |
|-----------|-------|
| Type | JST-SH 4-pin, 1.0 mm pitch, horizontal SMD |
| LCSC | C145956 |
| Unit cost | ~$0.12 |

#### 4.4.2 I2C Pull-Ups (Per Audit F-002)

```
SENSOR_3V3 ──── R7 (4.7kΩ) ──── I2C0_SDA (GPIO4)
SENSOR_3V3 ──── R8 (4.7kΩ) ──── I2C0_SCL (GPIO5)
```

**Critical design decision:** Pull-ups connect to `SENSOR_3V3` (gated
rail), **not** to the always-on `3V3` rail. This ensures:

1. **No backpower** — when sensor power is gated off, pull-ups are
   de-energized. No current path from SDA/SCL into unpowered sensor
   modules via their internal ESD diodes (audit finding F-002).
2. **Zero sleep leakage** — pull-up current is zero when sensors are off.
3. **Bus integrity** — when sensors are powered, 4.7 kΩ pull-ups provide
   standard I2C rise time for 100/400 kHz operation at the short trace
   lengths expected on this board (< 30 mm).

### 4.5 Sensor Power Gating

#### 4.5.1 P-Channel MOSFET Switch (Q1)

**Selected part:** Si2301 (P-Channel MOSFET)

| Parameter | Value | Source |
|-----------|-------|--------|
| Vds | −20 V | Datasheet |
| Id | −3 A | Datasheet |
| Rds(on) @ Vgs = −4.5 V | 110 mΩ max | Datasheet |
| Vgs(th) | −0.45 to −1.0 V | Datasheet |
| Package | SOT-23 | — |
| LCSC | C306861 | — |
| Unit cost | ~$0.03 |

**Circuit:**

```
           3V3
            │
     ┌──────┤ (Source)
     │      │
     │  ┌───┴───┐
     │  │ Si2301│  Q1
     │  │ P-FET │
     │  └───┬───┘
     │      │ (Drain)
     │      │
     │  SENSOR_3V3 ──► Qwiic VCC, I2C pull-ups
     │
     ├── R9 (10kΩ) ── 3V3     (pull-up: OFF by default in sleep)
     │
     └── R10 (10kΩ) ── GPIO3  (drive LOW to turn ON)
```

**Operation:**
- **GPIO3 LOW:** Gate pulled LOW relative to source (3V3).
  Vgs ≈ −3.3 V, well below threshold. MOSFET turns ON.
  SENSOR_3V3 ≈ 3V3 − (I × Rds_on).
- **GPIO3 HIGH or Hi-Z:** Gate at 3V3 (via R9 pull-up).
  Vgs = 0 V. MOSFET OFF. SENSOR_3V3 floating/off.
- **Deep sleep:** GPIO3 defaults to Hi-Z. R9 pulls gate to 3V3.
  MOSFET OFF. Zero sensor current.

**[DESIGN-ONLY]** R10 provides current limiting for the GPIO and a
defined state if GPIO3 is reconfigured. The 10 kΩ pull-up R9 ensures
sensors are OFF by default at power-on and during deep sleep.

#### 4.5.2 Solder Jumper Bypass (SJ1)

A solder jumper connects 3V3 directly to SENSOR_3V3, bypassing Q1.
For always-on sensors that don't need power gating.

```
3V3 ──── SJ1 (normally open) ──── SENSOR_3V3
```

When SJ1 is bridged, sensors receive power directly from the 3V3 rail
regardless of GPIO3 state. **Note:** This adds I2C pull-up current
(~1.4 mA total) to the deep sleep budget if sensors are connected.

### 4.6 Battery Monitoring

#### 4.6.1 Voltage Divider

```
VBAT ──── R11 (10MΩ) ──┬── R12 (10MΩ) ──── GND
                        │
                        └── VBAT_SENSE (GPIO0, ADC1_CH0)
```

- **Divider ratio:** 1:2 (VBAT_SENSE = VBAT / 2).
- **Impedance:** 20 MΩ total. At VBAT = 4.2 V, leakage = 0.21 µA.
  At VBAT = 3.0 V, leakage = 0.15 µA. Well within HW-0400 budget.
- **ADC range:** ESP32-C3 ADC reads 0–2.5 V (with 11 dB attenuation).
  VBAT range 3.0–4.2 V → ADC reads 1.5–2.1 V. Within range.

**[ASSUMPTION]** 10 MΩ resistors and the ESP32-C3 ADC input impedance
(~13 MΩ per ESP-IDF documentation) form a loaded divider. The actual
reading will be slightly lower than VBAT/2 due to ADC input leakage.
Firmware calibration can compensate. For more precise readings,
consider a unity-gain buffer (future enhancement).

**Resistor selection:** R11, R12 = 10 MΩ, 0402, 1% tolerance.
LCSC C26083.

#### 4.6.2 Protection Capacitor

```
VBAT_SENSE ──── C2 (100pF) ──── GND
```

A 100 pF capacitor filters high-frequency noise on the ADC input
and provides charge storage for the SAR ADC sampling. The RC time
constant (20 MΩ × 100 pF = 2 ms) is adequate for the low-frequency
battery voltage measurement.

### 4.7 GPIO Breakout Header (J6)

The breakout header is a 2×5 pin, 2.54 mm pitch header exposing all
GPIOs not consumed by dedicated functions:

| Header Pin | Signal | Notes |
|------------|--------|-------|
| 1 | 3V3 | Power output |
| 2 | GND | Ground |
| 3 | GPIO1 | ADC1_CH1 capable |
| 4 | GPIO2 | Strapping pin (has 10 kΩ pull-up) |
| 5 | GPIO6 | General purpose |
| 6 | GPIO7 | General purpose |
| 7 | GPIO8 | Strapping pin (has 10 kΩ pull-up) |
| 8 | GPIO10 | General purpose |
| 9 | GPIO20 | UART0 RX |
| 10 | GPIO21 | UART0 TX |

**Silkscreen:** Each pin labeled with its GPIO number and function.
Power pins labeled `3V3` and `GND`.

### 4.8 Buttons (Per Audit F-001)

#### SW1 — BOOT Button

```
GPIO9 ──── SW1 ──── GND
```

Momentary tactile switch. When pressed during reset, enters UART
download mode. During normal operation, available as a user button.
GPIO9 has a 10 kΩ pull-up (R5) for the strapping function.

#### SW2 — RESET Button

```
EN ──── SW2 ──── GND
```

Momentary tactile switch. Pulls EN LOW to reset the ESP32-C3.
EN has a 10 kΩ pull-up (R6) and 100 nF debounce capacitor (C1).

**Selected part (SW1, SW2):** 3×6×2.5 mm SMD tactile switch.
LCSC C318884.

### 4.9 Passive Component Summary

| Ref | Value | Package | Purpose | LCSC |
|-----|-------|---------|---------|------|
| R1, R2 | 22 Ω | 0402 | USB series resistors | C25092 |
| R3 | 10 kΩ | 0402 | GPIO2 strap pull-up | C25744 |
| R4 | 10 kΩ | 0402 | GPIO8 strap pull-up | C25744 |
| R5 | 10 kΩ | 0402 | GPIO9 strap pull-up | C25744 |
| R6 | 10 kΩ | 0402 | EN pull-up | C25744 |
| R7 | 4.7 kΩ | 0402 | I2C SDA pull-up | C25900 |
| R8 | 4.7 kΩ | 0402 | I2C SCL pull-up | C25900 |
| R9 | 10 kΩ | 0402 | Power gate pull-up | C25744 |
| R10 | 10 kΩ | 0402 | Power gate drive | C25744 |
| R11, R12 | 10 MΩ | 0402 | Battery divider | C26083 |
| R13, R14 | 5.1 kΩ | 0402 | USB CC1/CC2 to GND | C25905 |
| C1 | 100 nF | 0402 | EN debounce | C1525 |
| C2 | 100 pF | 0402 | ADC filter | C1546 |
| C3 | 1 µF | 0402 | LDO input | C52923 |
| C4 | 1 µF | 0402 | LDO output | C52923 |
| C5 | 10 µF | 0805 | LDO output bulk | C15850 |
| C6 | 100 nF | 0402 | ESP32 VDD bypass | C1525 |

---

## 5. Tradeoff Analysis

### Decision: LDO Regulator Selection

- **Options considered:**
  1. MCP1700-3302E/TT — 1.6 µA Iq, 250 mA, SOT-23-3
  2. ME6211C33M5G-N — 40 µA Iq, 500 mA, SOT-23-5
  3. AP2112K-3.3 — 55 µA Iq, 600 mA, SOT-23-5
  4. TLV75533PDBV — 17 µA Iq, 500 mA, SOT-23-5 (TI)
- **Decision:** MCP1700-3302E/TT
- **Rationale:** Lowest Iq (1.6 µA typ) by a large margin. Deep sleep
  current is the primary design driver (HW-0400). The 250 mA continuous
  rating is sufficient for ESP32-C3 (peak TX ≈ 340 mA is handled by
  bulk capacitor for the < 10 ms burst duration).
- **Tradeoffs:** Lower max current (250 mA vs 500–600 mA alternatives).
  Relies on bulk capacitor for peak TX current. No enable pin.
- **Reversibility:** Easy — SOT-23-3 footprint. The ME6211 (SOT-23-5)
  or AP2112 would require a footprint change but similar PCB area.

### Decision: Power Path OR-ing (Schottky vs P-FET)

- **Options considered:**
  1. Dual Schottky diodes — simple, proven, passive
  2. P-FET ideal diode controller — lower voltage drop
  3. Dedicated power mux IC (e.g., TPS2113) — automatic switchover
- **Decision:** Dual Schottky diodes (SS14)
- **Rationale:** Simplest implementation, no control logic needed.
  The ~0.45 V forward drop is acceptable given the 5 V USB input and
  the 3.5 V effective battery cutoff is within typical LiPo usage.
- **Tradeoffs:** Higher voltage drop than P-FET solution. Slightly
  reduces usable battery capacity (cutoff at ~3.5 V vs ~3.1 V).
- **Reversibility:** Easy — replace diodes with P-FET switch on
  same PCB area.

### Decision: I2C Pull-Up Rail

- **Options considered:**
  1. Pull-ups to always-on 3V3 rail
  2. Pull-ups to gated SENSOR_3V3 rail
- **Decision:** Pull-ups to SENSOR_3V3 (gated rail)
- **Rationale:** Eliminates backpower into unpowered sensor modules via
  their internal ESD diodes. Eliminates ~1.4 mA pull-up leakage during
  deep sleep. Per audit finding F-002.
- **Tradeoffs:** I2C bus only functional when sensor power is enabled.
  Firmware must enable power gate before I2C communication.
- **Reversibility:** Easy — reroute pull-ups to 3V3 with trace cut.

### Decision: Battery Voltage Divider Impedance

- **Options considered:**
  1. 2× 100 kΩ (200 kΩ total) — 18.5 µA at 3.7 V
  2. 2× 1 MΩ (2 MΩ total) — 1.85 µA at 3.7 V
  3. 2× 10 MΩ (20 MΩ total) — 0.19 µA at 3.7 V
- **Decision:** 2× 10 MΩ
- **Rationale:** Minimizes deep sleep current. The ESP32-C3 ADC has
  ~13 MΩ input impedance, so 10 MΩ divider resistors cause some
  loading error (~2.5%), but this is acceptable for battery voltage
  monitoring where ±100 mV accuracy is sufficient.
- **Tradeoffs:** Higher impedance means more susceptibility to noise
  and longer settling time. The 100 pF filter cap mitigates noise.
- **Reversibility:** Easy — swap resistor values.

### Decision: ESD Protection

- **Options considered:**
  1. USBLC6-2SC6 — proven, low capacitance, SOT-23-6
  2. TPD2E2U06 — TI, lower capacitance, SOT-23-6
  3. No ESD protection (rely on ESP32-C3 internal)
- **Decision:** USBLC6-2SC6
- **Rationale:** Industry-standard for USB 2.0 protection. IEC 61000-4-2
  level 4 rated. Low capacitance (3.5 pF) compatible with USB 2.0 full
  speed (12 Mbps). Widely available at LCSC (C7519).
- **Tradeoffs:** Adds cost (~$0.08) and board space. TPD2E2U06 has
  slightly lower capacitance but is less commonly stocked at LCSC.
- **Reversibility:** Easy — direct footprint swap with TPD2E2U06.

---

## 6. Security Considerations

- **USB ESD protection** (U3) prevents damage from electrostatic
  discharge events at the USB-C connector (external-facing interface).
- **Reverse polarity protection** via Schottky diodes prevents damage
  from incorrectly wired batteries (though JST-PH is keyed).
- **No JTAG exposed** — JTAG pins are not broken out to prevent
  unauthorized debug access in field-deployed nodes. USB serial
  boot can be disabled via eFuse if needed.
- **Sensor power isolation** — the power gating MOSFET provides
  electrical isolation of external sensor modules, preventing a
  shorted or malfunctioning sensor from drawing excessive current
  from the main rail.

---

## 7. Operational Considerations

### 7.1 Programming Workflow

1. Connect USB-C cable to host PC.
2. Hold BOOT button (SW1), press RESET (SW2), release BOOT.
3. ESP32-C3 enters USB/UART download mode.
4. Flash firmware using the merged CI image with `espflash write-bin -p PORT 0x0 ./firmware/flash_image.bin` (replace `PORT` with your serial port).
5. Press RESET to boot normally.

### 7.2 Battery Life Estimation

| Parameter | Value |
|-----------|-------|
| Battery capacity | 2000 mAh (typical 18650 Li‑ion) |
| Deep sleep current | 7 µA (typical) |
| Wake cycle current | 80 mA avg (sensor read + radio) |
| Wake cycle duration | 2 seconds |
| Wake interval | 15 minutes |
| Average current | 0.007 mA + (80 mA × 2 s) / (15 × 60 s) ≈ 0.185 mA (≈ 185 µA) |
| Estimated battery life | 2000 mAh / 0.185 mA ≈ **10,800 hours ≈ 450 days ≈ 1.2 years** |

**[ASSUMPTION]** The simple average-current estimate above ignores battery
self-discharge and capacity fade. For a 2000 mAh Li‑ion cell with
≈0.185 mA average load, realistic battery life is typically limited to
approximately **12–18 months**.

### 7.3 Thermal Considerations

The MCP1700 maximum power dissipation at worst case:
- VIN = 5.0 V (USB, after Schottky drop ≈ 4.55 V), VOUT = 3.3 V
- ΔV = 1.25 V, I = 250 mA → P = 0.31 W
- SOT-23-3 thermal resistance ≈ 220°C/W
- Temperature rise ≈ 68°C above ambient
- At 25°C ambient → junction ≈ 93°C (below 150°C max)

Continuous 250 mA from USB is the worst case. Battery operation
(VIN ≈ 3.5–4.2 V) produces less dissipation.

---

## 8. Open Questions

### Q1: MCP1700 Peak Current Capability

- **Question:** Can the MCP1700 supply the ESP32-C3 radio TX burst
  current (up to 340 mA for < 10 ms) without output voltage droop
  below 3.0 V?
- **Options:** (a) Rely on 10 µF bulk cap, (b) switch to ME6211
  (500 mA), (c) add a larger bulk cap (47 µF).
- **Information needed:** Actual transient response measurement on
  prototype board.
- **Impact of deferring:** Low — bulk capacitor strategy is common
  practice. Verify on first prototype.

### Q2: Battery Connector Orientation

- **Question:** Should the JST-PH connector face the same edge as
  USB-C or the opposite edge?
- **Options:** (a) Same edge — convenient for bench use, (b) opposite
  edge — cleaner cable routing in enclosure.
- **Information needed:** Enclosure design (out of scope).
- **Impact of deferring:** Low — layout change only.

### Q3: GPIO Header Pin Count

- **Question:** Should the GPIO header include 3V3/GND power pins
  or only signal pins?
- **Options:** (a) Include power (2×5 header), (b) signal only
  (1×8 header, smaller footprint).
- **Decision in this document:** Include power (2×5). Can be DNP
  if board space is constrained.
- **Impact of deferring:** Low — header size change only.

---

## 9. Preliminary BOM

| Ref | Qty | Description | Manufacturer | MPN | Package | LCSC | Unit Cost |
|-----|-----|-------------|-------------|-----|---------|------|-----------|
| U1 | 1 | ESP32-C3-MINI-1 (4MB) | Espressif | ESP32-C3-MINI-1-N4 | Module | C2838502 | $2.50 |
| U2 | 1 | LDO 3.3V 250mA 1.6µA Iq | Microchip | MCP1700-3302E/TT | SOT-23-3 | C54447 | $0.15 |
| U3 | 1 | USB ESD protection | ST | USBLC6-2SC6 | SOT-23-6 | C7519 | $0.08 |
| Q1 | 1 | P-FET −20V −3A | BORN | Si2301 | SOT-23 | C306861 | $0.03 |
| D1, D2 | 2 | Schottky 40V 1A | MDD | SS14 | SMA | C2480 | $0.02 |
| J1, J2 | 2 | Qwiic connector 4-pin | JST | SM04B-SRSS-TB | JST-SH horiz | C145956 | $0.12 |
| J3 | 1 | Battery connector 2-pin | JST | S2B-PH-SM4-TB | JST-PH horiz | C295747 | $0.08 |
| J4 | 1 | USB-C receptacle 16-pin | SHOU HAN | TYPE-C 16PIN 2MD | Mid-mount | C2765186 | $0.10 |
| J6 | 1 | Pin header 2×5 | — | — | 2.54mm | C124378 | $0.05 |
| SW1, SW2 | 2 | Tactile switch 3×6mm | — | — | SMD | C318884 | $0.02 |
| SJ1 | 1 | Solder jumper | — | — | 0805 pad | — | $0.00 |
| R1, R2 | 2 | 22 Ω 1% | — | — | 0402 | C25092 | $0.01 |
| R3–R6, R9, R10 | 6 | 10 kΩ 1% | — | — | 0402 | C25744 | $0.01 |
| R7, R8 | 2 | 4.7 kΩ 1% | — | — | 0402 | C25900 | $0.01 |
| R11, R12 | 2 | 10 MΩ 1% | — | — | 0402 | C26083 | $0.01 |
| R13, R14 | 2 | 5.1 kΩ 1% | — | — | 0402 | C25905 | $0.01 |
| C1, C6 | 2 | 100 nF X7R 16V | — | — | 0402 | C1525 | $0.01 |
| C2 | 1 | 100 pF C0G 50V | — | — | 0402 | C1546 | $0.01 |
| C3, C4 | 2 | 1 µF X5R 10V | — | — | 0402 | C52923 | $0.01 |
| C5 | 1 | 10 µF X5R 10V | — | — | 0805 | C15850 | $0.02 |

**Estimated BOM total: ≈ $3.65** (meets HW-0601 target of ≤ $5 USD)

Component count: 33 components (28 unique line items).

---

## 10. Net List Summary

| Net Name | Connected Components |
|----------|---------------------|
| `GND` | U1, U2, U3, J1-J4, J6, D1, D2, R12-R14, C1-C6, SW1, SW2 |
| `VUSB` | J4 VBUS, D1 anode, U3 VBUS |
| `VBAT` | J3 pin 1, D2 anode, R11 |
| `VIN` | D1 cathode, D2 cathode, U2 VIN, C3 |
| `3V3` | U2 VOUT, U1 VDD, C4, C5, C6, R3-R6, R9, Q1 source, SJ1, J6 pin 1 |
| `SENSOR_3V3` | Q1 drain, SJ1, J1 pin 2, J2 pin 2, R7, R8 |
| `I2C0_SDA` | U1 GPIO4, J1 pin 3, J2 pin 3, R7 |
| `I2C0_SCL` | U1 GPIO5, J1 pin 4, J2 pin 4, R8 |
| `USB_DP` | U1 GPIO19, R1, U3 I/O1 |
| `USB_DN` | U1 GPIO18, R2, U3 I/O2 |
| `VBAT_SENSE` | R11-R12 junction, C2, U1 GPIO0 |
| `SENSOR_PWR_EN` | R10, Q1 gate (via R9 network), U1 GPIO3 |
| `EN` | U1 EN, R6, C1, SW2 |
| `BOOT` | U1 GPIO9, R5, SW1 |

---

## 11. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 0.1 | 2026-03-29 | Copilot (AI-assisted) | Initial draft — component selection, schematic design |
