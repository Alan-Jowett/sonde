<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# IR Fiber Head Requirements Specification

> **Document status:** Draft
> **Source:** User request on 2026-07-03.
> **Scope:** Standalone 8-channel infrared transmitter / receiver board.
> **Related:** [ir-fiber-head-design.md](ir-fiber-head-design.md),
> [ir-fiber-head-validation.md](ir-fiber-head-validation.md),
> [kicad-export-requirements.md](kicad-export-requirements.md),
> [kicad-export-design.md](kicad-export-design.md)

---

## 1  Definitions

| Term | Definition |
|---|---|
| **Tx row** | The row of 8 infrared LED transmitter channels. |
| **Rx row** | The row of 8 infrared photodiode receiver channels. |
| **Fiber via** | A plated or non-plated board hole used as a mechanical capture point for a 1 mm fiber inserted through the PCB toward an upside-down optical component. |
| **Dual-use pins** | Two MCU pins that serve as either the host I2C `SCL`/`SDA` interface in normal operation or as programming/debug signals during bring-up. |
| **Host interface** | The off-board electrical connection supplying `3V3`, `GND`, `SCL`, and `SDA` through solderable through-hole pads. |

---

## 2  Requirement format

Each requirement uses the following fields:

- **ID** — Unique identifier (`IFH-XXXX`).
- **Title** — Short name.
- **Description** — What the board must do.
- **Acceptance criteria** — Observable, testable conditions.
- **Priority** — MoSCoW: **Must**, **Should**, **May**.
- **Source** — User intent motivating the requirement.

---

## 3  System and host interface requirements

### IFH-0100  Standalone optical head board

**Priority:** Must  
**Source:** User request — “this should be a brand new spec, not connected to the existing one”

**Description:**  
The design MUST define a standalone PCB for an 8-channel IR transmitter / receiver pair. It MUST NOT depend on the existing Sonde node or carrier-board electrical design.

**Acceptance criteria:**

1. The specification describes a complete standalone board rather than an add-on to an existing board.
2. The implementation package contains its own schematic, PCB layout, and manufacturing outputs.

---

### IFH-0101  Host power and I2C interface

**Priority:** Must  
**Source:** User request — “the goal is for this to be a I2C slave device, powered directly via 3.3v I2C power lines”

**Description:**  
The board MUST accept a host connection providing `3V3`, `GND`, `SCL`, and `SDA`.

**Acceptance criteria:**

1. The schematic contains four host-interface nets: `3V3`, `GND`, `SCL`, and `SDA`.
2. The PCB exposes those four nets on solderable through-hole pads suitable for hand-soldered wires.
3. No additional power input connector is required for normal operation.

---

### IFH-0102  Dual-use host/programming pins

**Priority:** Must  
**Source:** User request — “I want it to have 2 pins mapped for either programming or for I2C clock/data”

**Description:**  
Two MCU pins MUST be assigned so that the normal host I2C clock/data connection can be repurposed during bring-up for programming or debug access.

**Acceptance criteria:**

1. The design assigns exactly two MCU pins to the `SCL` and `SDA` nets.
2. The design documentation states how those two nets are reused during programming/debug access.
3. No second, conflicting pair of I2C pins is required.

---

### IFH-0103  Separate reset and boot access

**Priority:** Must  
**Source:** User request — “Separate reset/boot pads are OK”

**Description:**  
The board MUST expose separate access points for reset and boot-mode control so that programming access does not depend exclusively on the two dual-use host pins.

**Acceptance criteria:**

1. The PCB provides accessible reset and boot test pads or equivalent bring-up pads.
2. The schematic labels those pads unambiguously.

---

### IFH-0104  Host-provided I2C pull-ups

**Priority:** Must  
**Source:** User selection — “Rely on host pull-ups”

**Description:**  
The board MUST rely on the host bus for `SCL` and `SDA` pull-up resistors. The board itself MUST NOT add mandatory onboard I2C pull-ups that could over-constrain the shared bus.

**Acceptance criteria:**

1. The schematic contains no fixed pull-up resistors from `SCL` or `SDA` to `3V3`.
2. The design documentation states that the host bus provides the I2C pull-ups.
3. The design documentation states that the host pull-ups must support at least standard-mode 100 kHz I2C operation.

---

### IFH-0105  No hardware address-select requirement

**Priority:** Must  
**Source:** User response — “This is firmware concern. No hardware address select”

**Description:**  
The board MUST NOT reserve pins, jumpers, or straps for hardware I2C address selection. Slave addressing is a firmware concern.

**Acceptance criteria:**

1. The schematic contains no dedicated address-select pins or straps.
2. The design documentation states that I2C slave address selection is outside the hardware scope.

---

## 4  MCU and receive-path requirements

### IFH-0200  CH32V203 controller

**Priority:** Must  
**Source:** User request — “I want a ch32v203 to do the data processing”

**Description:**  
The board MUST use a CH32V203 microcontroller for local control, transmitter drive, and receiver sampling.

**Acceptance criteria:**

1. The schematic BOM names a CH32V203 device.
2. The selected package exposes enough pins to implement all required Tx, Rx, host, and bring-up signals without external GPIO expanders.

---

### IFH-0201  ADC-capable receive-pin assignment

**Priority:** Must  
**Source:** User request — “can we also pick pins that can either be gpio or adc for the photodiodes?”

**Description:**  
Each of the 8 receiver channels MUST route to an MCU pin that firmware can use as a digital GPIO input and that is also ADC-capable for future analog sampling.

**Acceptance criteria:**

1. All 8 receiver nets terminate on ADC-capable CH32V203 pins.
2. The design documentation identifies the 8 assigned receive pins.

---

### IFH-0202  Simplest possible receive front end

**Priority:** Must  
**Source:** User request — “I want to use a resistor and feed the result directly into a gpio port. Simplest possible”

**Description:**  
Each receiver channel MUST use a simple photodiode-plus-resistor network connected directly to the MCU receive pin. No dedicated amplifier, comparator, or external ADC is required.

**Acceptance criteria:**

1. Each receiver channel contains exactly one photodiode sensing element and a passive resistor network sufficient to create a measurable node voltage.
2. No active analog front-end IC is present in the receiver path.

---

### IFH-0203  Software-modulated sensing compatibility

**Priority:** Must  
**Source:** User request — “will modulate IR output via software”

**Description:**  
The receive path MUST preserve raw timing and amplitude information well enough for firmware-driven transmit modulation and receive sampling. The hardware MUST NOT include demodulating receiver modules intended only for fixed-carrier remote-control protocols.

**Acceptance criteria:**

1. The BOM uses discrete photodiodes rather than integrated demodulating IR receiver cans.
2. No fixed-frequency demodulator stage is present.

---

## 5  Transmit-path requirements

### IFH-0300  Eight transmitter channels

**Priority:** Must  
**Source:** User request — “I want to design a 8 channel IR transmitter / receiver pair”

**Description:**  
The board MUST provide 8 independent infrared transmitter channels.

**Acceptance criteria:**

1. The schematic contains 8 discrete transmitter channels.
2. Each transmitter channel has its own MCU control net.

---

### IFH-0301  940 nm optical wavelength

**Priority:** Must  
**Source:** User selection — “940 nm”

**Description:**  
The transmitter LEDs and receiver photodiodes MUST be chosen for 940 nm operation.

**Acceptance criteria:**

1. The BOM specifies 940 nm IR LEDs.
2. The BOM specifies photodiodes whose spectral response includes 940 nm.

---

### IFH-0302  Direct-drive LED channels

**Priority:** Must  
**Source:** User selection — “Direct MCU GPIO drive through resistors (simplest)”

**Description:**  
Each transmitter channel MUST be driven directly from an MCU GPIO through a current-limiting resistor. No transistor driver stage is required.

**Acceptance criteria:**

1. Each LED channel includes a current-limiting resistor.
2. No per-channel transistor driver is present between the MCU GPIO and the LED.
3. The design documentation defines whether the GPIO drives the LED by sourcing or sinking current.

---

### IFH-0303  Simultaneous-drive power budget

**Priority:** Must  
**Source:** User selections — “Up to 100 mA total” and “Support all 8 simultaneously within budget”

**Description:**  
The transmitter resistor values and power distribution MUST support all 8 LEDs being enabled simultaneously while keeping total board current within a 100 mA host-supplied budget.

**Acceptance criteria:**

1. The design includes a current-budget calculation covering the 8-LED simultaneous-drive case.
2. The summed worst-case LED current plus MCU and passive loads does not exceed 100 mA from `3V3`.
3. The design includes a GPIO-drive calculation showing compliance with the selected CH32V203 package's per-pin and aggregate GPIO current limits.

---

## 6  Optical geometry requirements

### IFH-0400  Opposed 1×8 optical rows

**Priority:** Must  
**Source:** User response — “1x8 on one side of the board and 1x8 on the other side. LED / transmitter on one side and photodiode / receiver on the other” and “Opposite edges of the same PCB face”

**Description:**  
The board MUST place the 8 transmitter channels in a 1×8 row along one edge of the PCB face and the 8 receiver channels in a 1×8 row along the opposite edge of the same PCB face.

**Acceptance criteria:**

1. The PCB places exactly 8 transmitter optical ports on one edge-aligned row.
2. The PCB places exactly 8 receiver optical ports on the opposite edge-aligned row.
3. Both rows appear on the same PCB face.

---

### IFH-0401  5.08 mm channel pitch

**Priority:** Must  
**Source:** User selection — “5.08 mm / 0.2 in pitch”

**Description:**  
Within each optical row, adjacent channel centers MUST use 5.08 mm pitch.

**Acceptance criteria:**

1. Measured center-to-center spacing between adjacent channel ports is 5.08 mm ± 0.10 mm.

---

### IFH-0402  Upside-down optical mounting

**Priority:** Must  
**Source:** User request — “I want them mounted "upside down" over vias”

**Description:**  
The LEDs and photodiodes MUST be mounted upside down so that each optical package couples vertically into a board hole below it.

**Acceptance criteria:**

1. The footprint and assembly notes orient each optical component for downward emission or reception into the PCB hole.
2. The PCB footprint geometry aligns each optical package with its corresponding fiber via.

---

### IFH-0403  1 mm fiber interference-fit vias

**Priority:** Must  
**Source:** User request — “vias in the PCB that are big enough to inteference fit 1mm fiber optic cable”

**Description:**  
Each optical channel MUST include a board hole sized for interference-fit retention of nominal 1.00 mm PMMA fiber.

**Acceptance criteria:**

1. The layout defines a dedicated hole for each Tx and Rx optical channel.
2. The finished hole target is specified in the design documentation as suitable for nominal 1.00 mm PMMA fiber interference fit.
3. The Gerber / drill package contains those holes.

---

### IFH-0404  Opposite-side fiber insertion

**Priority:** Must  
**Source:** User selection — “Fibers insert from the opposite side of each optical component, through the board”

**Description:**  
Fibers MUST insert from the side opposite the optical components so that the board acts as the mechanical alignment plate between the fiber and the upside-down component.

**Acceptance criteria:**

1. The assembly notes specify fiber insertion from the opposite side of the board from the optical components.
2. No optical channel requires same-side fiber insertion.

---

### IFH-0405  Optical package compatibility with upside-down coupling

**Priority:** Must  
**Source:** Derived from the upside-down-over-via coupling requirement

**Description:**  
The selected LED and photodiode packages MUST have optical apertures and mechanical footprints compatible with upside-down coupling into a 1 mm fiber hole.

**Acceptance criteria:**

1. The optical axis of each selected package can be aligned to its matching hole center within ±0.25 mm.
2. The design documentation identifies the package orientation used for upside-down coupling.

---

## 7  Deliverable requirements

### IFH-0500  Hardware-source deliverables

**Priority:** Must  
**Source:** User request — “full requirements -> gerber file”

**Description:**  
The project MUST include hardware source artifacts sufficient to regenerate the board.

**Acceptance criteria:**

1. The repository contains requirements, design, and validation specifications for this board.
2. The repository contains IR and KiCad source artifacts sufficient to regenerate the board outputs.
3. The repository contains a documented I2C address assumption for first-article bring-up firmware, even though address selection is outside hardware scope.

---

### IFH-0501  Manufacturing deliverables

**Priority:** Must  
**Source:** User request — “full requirements -> gerber file”

**Description:**  
The implementation MUST produce a manufacturing package for PCB fabrication.

**Acceptance criteria:**

1. The repository contains generated schematic and PCB files.
2. The repository contains Gerber and drill outputs for the board.
