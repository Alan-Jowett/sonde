<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# IR Fiber Head Design Specification

> **Document status:** Draft
> **Scope:** Architecture and implementation design for the standalone 8-channel IR fiber head.
> **Audience:** Implementers building the board and its manufacturing package.
> **Related:** [ir-fiber-head-requirements.md](ir-fiber-head-requirements.md),
> [ir-fiber-head-validation.md](ir-fiber-head-validation.md),
> [kicad-export-design.md](kicad-export-design.md),
> [kicad-export-requirements.md](kicad-export-requirements.md)

---

## 1  Overview

The IR fiber head is a small standalone PCB that exposes 8 optical transmit
channels and 8 optical receive channels. The host supplies `3V3`, `GND`,
`SCL`, and `SDA` over wire-soldered through-hole pads. A CH32V203 performs
transmitter control and receiver sampling.

The board intentionally favors **simple, inspectable circuitry** over analog
complexity:

1. Each transmitter channel is a discrete 940 nm IR LED driven directly from an
   MCU GPIO through a resistor.
2. Each receiver channel is a discrete 940 nm photodiode plus passive resistor
   network tied directly to an ADC-capable MCU pin.
3. Optical alignment is performed mechanically by the PCB: upside-down optical
   parts sit above fiber-capture holes, and fibers insert from the opposite side.

---

## 2  Design constraints

| Constraint | Design response | Requirements covered |
|---|---|---|
| Standalone board | Define a new spec and new `hw\ir-fiber-head\` source tree | IFH-0100, IFH-0500 |
| 3.3 V host-supplied power | No onboard regulator; all circuitry runs directly from host `3V3` | IFH-0101 |
| Shared I2C / programming pins | Reuse the two host signal pins as the normal-operating I2C pins and the default two-wire bring-up signals | IFH-0102 |
| Reset / boot access required | Provide dedicated bring-up pads | IFH-0103 |
| Host owns I2C pull-ups | Do not place fixed onboard `SCL` / `SDA` pull-ups; rely on the host bus | IFH-0104 |
| No hardware address straps | Leave I2C address selection to firmware; reserve no address-select straps or pins | IFH-0105 |
| 8 Tx + 8 Rx channels | Reserve 16 channel nets plus host and bring-up nets in MCU pin planning | IFH-0200, IFH-0201, IFH-0300, IFH-0400 |
| Simplicity over analog gain | Use direct LED drive and passive photodiode loads | IFH-0202, IFH-0302 |
| 100 mA all-LED budget | Size LED resistors from a simultaneous-drive power budget, not from one-channel-only assumptions | IFH-0303 |
| 1 mm fiber coupling | Make the PCB itself the alignment fixture | IFH-0402, IFH-0403, IFH-0404 |

---

## 3  Electrical architecture

### 3.1  Power distribution

- `3V3` enters on the host wire pad.
- `GND` is distributed as the common return.
- No separate power-input connector, battery path, or load switch is used.
- The design includes local MCU decoupling and any modest bulk capacitance
  needed for simultaneous LED switching.
- `SCL` and `SDA` rely on pull-ups provided by the host bus rather than fixed
  board-local pull-ups.
- The host pull-ups must support at least standard-mode 100 kHz I2C operation.

This is a pure 3.3 V logic-and-optics board; there is no secondary rail.

### 3.2  Host and bring-up interface

The board exposes six bring-up relevant external signals:

| External signal | Normal role | Bring-up role |
|---|---|---|
| `3V3` | Host power | Target power |
| `GND` | Ground | Ground |
| `SCL` | I2C clock | Shared debug/programming signal 1 |
| `SDA` | I2C data | Shared debug/programming signal 2 |
| `RST` pad | Not used by host | Reset control |
| `BOOT` pad | Not used by host | Boot-mode selection |

The host-facing connection remains a simple four-wire interface in normal use.
Bring-up uses the same powered board plus the two auxiliary pads.
The hardware does not reserve any separate address-select strap; the first-article
bring-up firmware must document the address it uses for enumeration.

### 3.3  MCU allocation strategy

The CH32V203 package must expose:

- 8 GPIO-class outputs for `TX0..TX7`
- 8 ADC-capable inputs for `RX0..RX7`
- 2 dual-use pins for `SCL` and `SDA`
- reset and boot access
- required power, clock, and decoupling pins

The reference implementation should start from a 48-pin CH32V203 package so the
design has comfortable room for 8 ADC-capable receive inputs, 8 transmitter
outputs, the shared I2C pair, and bring-up access without resorting to awkward
pin-mux compromises. Smaller packages may only be substituted if implementation
proves that they still satisfy the full channel and ADC pin budget cleanly.

### 3.4  Transmitter channels

Each transmitter channel uses the same topology:

`3V3 -> current-limiting resistor -> 940 nm IR LED -> MCU GPIO (active-low sink)`

Design rules:

1. One resistor per LED channel.
2. No shared resistor between multiple channels.
3. The LED drive topology is active-low sinking so the MCU sinks, rather than
   sources, the LED current.
4. LED resistor values are chosen from the simultaneous-drive budget.
5. The power budget calculation must include all 8 LEDs enabled at once.
6. The GPIO-drive calculation must verify both per-pin and aggregate current
   limits for the chosen CH32V203 package and pin grouping.

### 3.5  Receiver channels

Each receiver channel uses the same topology:

`3V3 or GND bias -> resistor network -> photodiode node -> ADC-capable MCU pin`

Design rules:

1. One photodiode per channel.
2. One passive resistor network per channel sufficient to create a directly
   sampled voltage.
3. No amplifier, comparator, or demodulator stage.
4. The node remains readable both as an ADC input and as a digital GPIO input.

The exact polarity may be selected during implementation to best match the
verified CH32V203 pinout and PCB routing, but all 8 channels must use a
consistent topology unless there is a documented reason to differ.

---

## 4  Optical and mechanical architecture

### 4.1  Row arrangement

- Tx channels occupy one 1×8 row on one edge of the PCB face.
- Rx channels occupy one 1×8 row on the opposite edge of the same PCB face.
- Both rows use 5.08 mm channel pitch.

This creates a board that behaves like an optical bridge or alignment bar with
transmission on one side and reception on the other.

### 4.2  Fiber coupling

Each channel consists of:

1. An upside-down optical component on the populated side.
2. A dedicated aligned board hole below that component.
3. A 1 mm fiber inserted from the opposite side into that hole.

The PCB therefore provides both axial alignment and retention for the fiber.
The reference design target is a **0.96 mm finished NPTH hole with ±0.02 mm
tolerance** for nominal 1.00 mm PMMA fiber. Validation must confirm that this
target still provides acceptable insertion force and retention with the chosen
fiber stock.

### 4.3  Optical package constraints

The selected IR LEDs and photodiodes must satisfy all of the following:

1. The optical aperture must face the board hole when the package is mounted
   upside down.
2. The package body and pads must allow the optical axis to align to the hole
   center within ±0.25 mm.
3. The package must be placeable on the populated side without mechanically
   blocking fiber insertion from the opposite side.
4. The package family must support 940 nm operation.

### 4.4  Board shape and routing intent

Because there is no strict board-size limit, routing should optimize for:

- straight optical rows
- short, regular channel routing
- easy access to host wire pads and bring-up pads
- uncomplicated assembly and inspection

The initial implementation target should be a simple 2-layer FR-4 board unless
layout evidence shows that more layers are required. The default assumption is
that 2 layers are sufficient for this pin-count and interface complexity.

---

## 5  Source-artifact pipeline

The implementation follows the repo’s existing hardware-source flow:

1. Requirements are captured in an IR-0 style hardware requirements artifact.
2. Component selection is captured in IR-1 / IR-1e style artifacts.
3. Net connectivity is captured in IR-2.
4. Board shape, hole placement, and routing constraints are captured in IR-3.
5. `sonde-kicad` generates KiCad schematic / PCB outputs and manufacturing files.

The implementation package for this board should therefore live in a dedicated
hardware directory and include:

- IR YAML source files
- generated `.kicad_sch`
- generated `.kicad_pcb`
- BOM / CPL outputs as applicable
- Gerber / drill outputs

---

## 6  Validation-driving design notes

The following points are intentionally called out because they are the highest
risk items for a simple optical design:

1. **Power budget risk** — simultaneous 8-channel LED drive must remain within
   100 mA including MCU current, and the chosen GPIO grouping must remain within
   package current limits.
2. **Signal-margin risk** — the passive photodiode network may have lower noise
   immunity than an amplified receiver, so validation must explicitly test that
   firmware-driven modulation remains observable.
3. **Mechanical-fit risk** — the 1 mm fiber interference-fit hole size is both
   a mechanical-retention feature and an optical-alignment feature; drill and
   finish assumptions must be checked explicitly.
4. **Bring-up risk** — the dual-use host/programming pins must remain usable
   both for I2C operation and for board programming/debug access.
