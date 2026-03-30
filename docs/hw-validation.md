<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Hardware Validation Specification

> **Document status:** Draft  
> **Scope:** Test plan for the sonde hardware generation tool (`sonde-hw`).  
> **Audience:** Implementers (human or LLM agent) writing hardware tool tests.  
> **Related:** [hw-requirements.md](hw-requirements.md), [hw-design.md](hw-design.md)

---

## 1  Overview

This document defines test cases for the hardware generation tool (`sonde-hw`). Tests validate configuration loading, schematic generation, ERC checks, BOM generation, and SPICE simulation against the requirements in [hw-requirements.md](hw-requirements.md).

**Test harness:** Tests are Python `pytest` cases located in `hw/`. They exercise the CLI and internal modules using example board configurations from `hw/configs/`.

---

## 2  Configuration tests

### T-H001  Valid configuration loads without error

**Validates:** HW-0700

**Procedure:**
1. Run `sonde-hw validate configs/minimal-qwiic.yaml`.
2. Assert: exit code 0 and output includes `"is valid"`.

### T-H002  Invalid configuration rejected

**Validates:** HW-0700

**Procedure:**
1. Create a YAML file missing required fields.
2. Run `sonde-hw validate` with the invalid file.
3. Assert: exit code non-zero with an error message referencing the missing field.

---

## 3  Schematic generation tests

### T-H010  Build produces schematic output

**Validates:** HW-0800

**Procedure:**
1. Run `sonde-hw build configs/minimal-qwiic.yaml --skip-erc`.
2. Assert: `output/minimal-qwiic/board.kicad_sch` exists and is non-empty.

### T-H011  BOM generated alongside schematic

**Validates:** HW-0802

**Procedure:**
1. Run `sonde-hw build configs/minimal-qwiic.yaml`.
2. Assert: `output/minimal-qwiic/bom.csv` exists and contains component entries.

---

## 4  ERC tests

### T-H020  ERC passes on valid schematic

**Validates:** HW-1000

**Procedure:**
1. Run `sonde-hw build configs/minimal-qwiic.yaml` (ERC enabled by default).
2. Assert: exit code 0.

---

## 5  Determinism tests

### T-H030  Reproducible generation

**Validates:** HW-0900

**Procedure:**
1. Run `sonde-hw build configs/minimal-qwiic.yaml` twice.
2. Assert: the generated `board.kicad_sch` and `bom.csv` are byte-identical between runs.

---

## 6  SPICE simulation tests

### T-H040  Simulate lists available tests

**Validates:** HW-1003

**Procedure:**
1. Run `sonde-hw simulate configs/minimal-qwiic.yaml --list`.
2. Assert: exit code 0 and output lists test IDs (e.g., `battery-divider`, `dc-operating-point`, `sleep-current`).

### T-H041  Simulate runs single test

**Validates:** HW-1003

**Procedure:**
1. Run `sonde-hw simulate configs/minimal-qwiic.yaml --test battery-divider`.
2. Assert: exit code 0 and output includes simulation results.

---

## Appendix A  Test-to-requirement traceability

| Requirement | Test(s) |
|---|---|
| HW-0700 | T-H001, T-H002 |
| HW-0800 | T-H010 |
| HW-0802 | T-H011 |
| HW-0900 | T-H030 |
| HW-1000 | T-H020 |
| HW-1003 | T-H040, T-H041 |
