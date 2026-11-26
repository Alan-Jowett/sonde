<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# IR Fiber Head Validation Specification

> **Document status:** Draft
> **Scope:** Inspection and bench-validation plan for the standalone 8-channel IR fiber head.
> **Audience:** Implementers generating and reviewing the hardware deliverables.
> **Related:** [ir-fiber-head-requirements.md](ir-fiber-head-requirements.md),
> [ir-fiber-head-design.md](ir-fiber-head-design.md)

---

## 1  Overview

This document defines the validation checks for the IR fiber head hardware
design package. The checks cover design-document traceability, schematic
correctness, PCB geometry, manufacturability, and first-order bench behavior.

---

## 2  Design-package checks

### V-IFH-100  Standalone artifact set

**Validates:** IFH-0100, IFH-0500, IFH-0501

**Procedure:**
1. Inspect the repository tree for the board-specific docs and hardware source directory.
2. Confirm the board has its own requirements, design, validation, schematic, PCB, and Gerber artifacts.
3. Assert: the board does not depend on editing the existing carrier-board source files.

---

### V-IFH-101  Host wire-pad interface

**Validates:** IFH-0101, IFH-0104, IFH-0105

**Procedure:**
1. Inspect the schematic host connector or pad block.
2. Inspect the PCB edge or pad field.
3. Assert: `3V3`, `GND`, `SCL`, and `SDA` each appear on solderable through-hole pads.
4. Assert: the schematic contains no fixed board-local pull-up resistors on `SCL` or `SDA`.
5. Assert: the schematic contains no dedicated hardware address-select strap or jumper.

---

### V-IFH-102  Bring-up access pads

**Validates:** IFH-0102, IFH-0103

**Procedure:**
1. Inspect the schematic for the normal `SCL` / `SDA` mapping.
2. Inspect the schematic and PCB for separate reset and boot pads.
3. Assert: exactly two host signal pins are reused for programming/debug access.
4. Assert: reset and boot controls remain externally accessible.

---

### V-IFH-103  I2C bring-up assumptions documented

**Validates:** IFH-0104, IFH-0105, IFH-0500

**Procedure:**
1. Inspect the design package notes for I2C host assumptions.
2. Assert: the host-pull-up expectation is documented, including support for at least standard-mode 100 kHz I2C.
3. Assert: first-article firmware bring-up notes document the intended slave address, even though address selection is outside hardware scope.

---

## 3  Electrical checks

### V-IFH-200  Channel count and mapping

**Validates:** IFH-0200, IFH-0201, IFH-0300

**Procedure:**
1. Count the transmitter channels in the schematic.
2. Count the receiver channels in the schematic.
3. Inspect the MCU net mapping table.
4. Assert: there are 8 transmit nets and 8 receive nets.
5. Assert: all 8 receive nets terminate on ADC-capable MCU pins.

---

### V-IFH-201  Passive receive topology

**Validates:** IFH-0202, IFH-0203

**Procedure:**
1. Inspect one representative receiver channel and then all 8 channels.
2. Assert: each channel uses a discrete photodiode and passive resistor network only.
3. Assert: no amplifier, comparator, or demodulating receiver IC exists in the receive path.

---

### V-IFH-202  Direct-drive transmitter topology

**Validates:** IFH-0301, IFH-0302

**Procedure:**
1. Inspect one representative LED channel and then all 8 channels.
2. Assert: each LED channel uses a 940 nm IR LED.
3. Assert: each LED channel includes a current-limiting resistor.
4. Assert: no transistor driver stage exists between the MCU pin and the LED.
5. Assert: the LED topology is direct active-low sinking or an equally direct documented source/sink topology.

---

### V-IFH-203  Receiver wavelength compatibility

**Validates:** IFH-0301

**Procedure:**
1. Inspect the receiver photodiode BOM entries.
2. Review the photodiode spectral-response documentation captured in the design package.
3. Assert: each receiver device is specified for sensitivity at 940 nm.

---

### V-IFH-204  Simultaneous-drive current budget

**Validates:** IFH-0303

**Procedure:**
1. Review the design power-budget calculation.
2. Sum worst-case current for all 8 enabled LEDs, MCU operation, and static passive loads.
3. Review the GPIO-drive calculation for the selected CH32V203 package.
4. Assert: total worst-case current is less than or equal to 100 mA from `3V3`.
5. Assert: per-pin and aggregate GPIO current limits are not exceeded.

---

## 4  Optical and PCB geometry checks

### V-IFH-300  Opposed-row placement

**Validates:** IFH-0400, IFH-0401

**Procedure:**
1. Open the PCB layout.
2. Measure the center positions of adjacent Tx ports and adjacent Rx ports.
3. Assert: Tx ports form a 1×8 row on one board edge.
4. Assert: Rx ports form a 1×8 row on the opposite board edge on the same PCB face.
5. Assert: adjacent center spacing is 5.08 mm ± 0.10 mm.

---

### V-IFH-301  Upside-down optical alignment

**Validates:** IFH-0402, IFH-0404, IFH-0405

**Procedure:**
1. Inspect the optical footprints and assembly notes.
2. Inspect the PCB for hole alignment relative to the optical package centers.
3. Assert: each optical component is oriented to couple into the PCB hole beneath it.
4. Assert: assembly notes specify fiber insertion from the opposite side of the board.
5. Assert: the package geometry supports hole-center alignment within ±0.25 mm.

---

### V-IFH-302  Fiber-hole implementation

**Validates:** IFH-0403, IFH-0501

**Procedure:**
1. Inspect the drill data and PCB holes for all 16 optical ports.
2. Inspect the documented nominal finished hole size and tolerance.
3. Assert: each optical channel has a dedicated hole.
4. Assert: the Gerber / drill package contains those holes.
5. Assert: the documented hole target is a 0.96 mm finished NPTH hole with ±0.02 mm tolerance.
6. Assert: the documented hole target is intended for nominal 1.00 mm PMMA fiber interference fit.

---

## 5  Manufacturing-output checks

### V-IFH-400  Regenerable hardware outputs

**Validates:** IFH-0500, IFH-0501

**Procedure:**
1. Inspect the hardware source directory for IR YAML inputs and generated KiCad outputs.
2. Confirm the generated schematic and PCB files are present.
3. Confirm Gerber and drill files are present.
4. Assert: the artifact set is sufficient to regenerate the design package.

---

## 6  First article bench checks

### V-IFH-500  Receiver observability

**Validates:** IFH-0201, IFH-0202, IFH-0203

**Procedure:**
1. Power the assembled board at 3.3 V.
2. Modulate one Tx channel in firmware while observing the corresponding Rx node with ADC or logic capture.
3. Repeat for at least channels 0, 3, and 7.
4. While driving each tested channel, inspect at least one adjacent untargeted Rx channel for optical crosstalk.
5. Assert: the target Rx node shows a repeatable response correlated with the modulation pattern.
6. Assert: adjacent untargeted channels do not show a comparable above-noise response.

---

### V-IFH-501  Full-row simultaneous transmit

**Validates:** IFH-0303

**Procedure:**
1. Power the board from a current-limited 3.3 V supply.
2. Command firmware to enable all 8 Tx channels simultaneously.
3. Measure total board current.
4. Assert: measured current does not exceed 100 mA.

---

### V-IFH-502  I2C slave enumeration

**Validates:** IFH-0101, IFH-0102, IFH-0104, IFH-0105

**Procedure:**
1. Power the board at 3.3 V with host-provided I2C pull-ups.
2. Program or load bring-up firmware that exposes the intended test slave address.
3. Perform an I2C scan or directed transaction from a host controller.
4. Assert: the board ACKs on the documented bring-up address.

---

### V-IFH-503  Fiber retention sanity check

**Validates:** IFH-0403, IFH-0404

**Procedure:**
1. Insert representative nominal 1.00 mm PMMA fibers into at least one Tx hole and one Rx hole.
2. Assert: each fiber seats with firm hand pressure and remains retained without slipping out under its own weight.
3. Gently withdraw and reinsert each tested fiber once.
4. Assert: the hole does not crack, and the fiber remains insertable and retained after reinsertion.
