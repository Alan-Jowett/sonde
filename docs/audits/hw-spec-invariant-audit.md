# Sonde Sensor Node Hardware Specification Invariant Audit — Investigation Report

> **Audit date:** 2026-03-29
> **Specification audited:** `docs/hw-requirements.md` (35 requirements, HW-0100–HW-1104)
> **Audit prompt:** `prompts/hardware/09-audit-spec-invariants.md`

## 1. Executive Summary

Adversarial audit of `hw-requirements.md` against three invariants (remote
recoverability, update atomicity, deep sleep current). Found **7 findings** —
1 Critical, 2 High, 2 Medium, 2 Informational. The most significant gap: the
spec has **no requirement for a physical reset/bootloader-entry mechanism**,
meaning a firmware bug can permanently brick the device with no hardware
recovery path. The deep sleep invariant has multiple compliance gaps where
valid configurations can exceed 20 µA through always-on pull-up leakage.

## 2. Problem Statement

The hardware spec defines 35 requirements for a parameterized ESP32-C3 sensor
node PCB. Three invariants were supplied for audit:

1. **INV-1 — Remote Recoverability**: For every reachable state S and every
   failure mode F, there exists a recovery path to a state that accepts remote
   commands (USB programming or ESP-NOW radio).
2. **INV-2 — Update Atomicity**: Power loss at any point during firmware update
   must not brick the device.
3. **INV-3 — Deep Sleep Current**: Total board current in deep sleep ≤ 20 µA
   under any valid configuration.

## 3. Investigation Scope

- **Document examined**: `docs/hw-requirements.md` (35 requirements)
- **Tools**: Manual adversarial analysis per `prompts/hardware/09-audit-spec-invariants.md`
- **Limitations**: Firmware spec (`node-requirements.md`) was NOT audited — only
  the hardware spec. ESP32-C3 datasheet was referenced for [INFERRED] claims
  about GPIO9 bootstrap behavior.

## 4. Findings

### Finding F-001: No reset button or bootloader-entry mechanism

- **Severity**: Critical
- **Category**: Gap — Incompleteness
- **Invariant violated**: INV-1 (Remote Recoverability)
- **Spec sections**: HW-0101, HW-0203
- **Description**: The spec requires a USB-C connector (HW-0101) compatible
  with `espflash`, and GPIO breakout for pins "not consumed by bootstrapping"
  (HW-0203). But there is **no requirement for a physical mechanism to enter
  the ESP32-C3 download mode** (GPIO9 held low at boot). The ROM bootloader is
  always present in silicon, but entering it requires GPIO9 low at reset —
  without a button, test pad, or jumper, this is inaccessible.
- **Violating interpretation**: A compliant board routes GPIO9 to a pull-up for
  normal boot and leaves it inaccessible on the PCB. Firmware has a bug that
  crashes before USB initialization. The device cannot accept `espflash`
  commands because the application firmware never starts. The ROM bootloader
  cannot be entered because GPIO9 cannot be pulled low. The device is bricked.
- **Disproof attempt**: HW-0203 says pins "not consumed by... bootstrapping"
  go to headers — implying GPIO9 IS consumed by bootstrapping and might NOT be
  on a header. HW-0101 says "Compatible with `espflash`" — but `espflash`
  requires either a running stub loader (needs working firmware) or download
  mode (needs GPIO9). No other requirement closes this gap.
- **Confidence**: High
- **Remediation**: Add requirement: "The board MUST include a tactile reset
  button and a mechanism to enter ESP32-C3 download mode (GPIO9 pulled low at
  boot), either via a BOOT button or a test pad with documented procedure."

---

### Finding F-002: I2C pull-ups not required to be on gated power rail

- **Severity**: High
- **Category**: Ambiguity
- **Invariant violated**: INV-3 (Deep Sleep Current ≤ 20 µA)
- **Spec sections**: HW-0200, HW-0400, HW-0401
- **Description**: HW-0200 AC2 requires "4.7 kΩ pull-up resistors on SDA and
  SCL" but does not specify which power rail they connect to. HW-0401 (sensor
  power gating) is "Should" not "Must". A compliant board can have 4.7 kΩ
  pull-ups to the **main 3.3V rail** (always on) with an I2C sensor that holds
  SDA or SCL low during sleep. Leakage per line: 3.3V / 4.7 kΩ = **0.7 mA** —
  35× over the 20 µA budget, violating HW-0400.
- **Violating interpretation**: Board has 4.7 kΩ pull-ups to VCC_3V3 (main
  rail). A TMP102 sensor (HW-0301) is connected. During deep sleep, TMP102
  enters shutdown but its I2C lines may float or be driven — pull-ups leak
  continuously. Board draws > 100 µA in deep sleep.
- **Disproof attempt**: HW-1103 AC9 ("Sleep leakage accounting: pull-ups...
  do not violate the sleep-state current budget") is a contract invariant
  CHECK, not a design constraint. It would flag the violation, but HW-1104
  (CI validation) is "Should" — so the check may not run. Even if it runs,
  nothing requires the design to be changed before manufacturing.
- **Confidence**: High
- **Remediation**: Add to HW-0200: "Pull-up resistors MUST connect to the
  sensor power rail (HW-0401) when sensor power gating is enabled, so that
  pull-ups are de-energized during deep sleep."

---

### Finding F-003: Spec is silent on firmware update mechanism

- **Severity**: High
- **Category**: Gap — Spec silence
- **Invariant violated**: INV-2 (Update Atomicity)
- **Spec sections**: HW-0100, entire document
- **Description**: The spec has **zero requirements** related to firmware
  update: no A/B partitioning, no OTA, no rollback, no update atomicity.
  HW-0100 requires "minimum 4 MB SPI flash" but doesn't specify partition
  layout. [INFERRED] The spec delegates firmware update to
  `node-requirements.md`, but the hardware must provide sufficient flash for
  dual-partition layout. With a 4 MB flash and sonde firmware (~1.5 MB),
  dual-partition fits — but nothing in the hardware spec guarantees it.
- **Violating interpretation**: A compliant board uses exactly 4 MB flash.
  The firmware team implements single-partition OTA (no rollback). Power loss
  during flash erase leaves the device with corrupted firmware. No A/B
  fallback exists. The device is bricked (also violates INV-1).
- **Disproof attempt**: `node-requirements.md` may specify partition layout —
  but this audit covers only the hardware spec. The hardware spec alone does
  not guarantee INV-2.
- **Confidence**: Medium — depends on whether `node-requirements.md`
  constrains this. The hardware spec alone does not guarantee INV-2.
- **Remediation**: Add to HW-0100: "Flash capacity MUST be sufficient for
  dual-partition (A/B) firmware layout as specified in `node-requirements.md`.
  Minimum 4 MB with at least 2× the maximum firmware image size available
  for OTA partitions."

---

### Finding F-004: 1-Wire pull-up contributes to sleep current unaccounted

- **Severity**: Medium
- **Category**: Ambiguity
- **Invariant violated**: INV-3 (Deep Sleep Current ≤ 20 µA)
- **Spec sections**: HW-0202, HW-0400
- **Description**: HW-0202 AC2 requires "4.7 kΩ pull-up resistor on the DATA
  line" for 1-Wire. Same issue as F-002 — the pull-up rail is unspecified. If
  connected to the main 3.3V rail and a DS18B20 is attached, the pull-up leaks
  continuously during deep sleep.
- **Violating interpretation**: Board has 1-Wire pull-up to main 3.3V.
  DS18B20 probe connected. During deep sleep, pull-up leaks 0.7 mA through
  the probe's parasitic load.
- **Disproof attempt**: HW-0401 power gating would help if the 1-Wire pull-up
  is on the gated rail — but HW-0202 doesn't require this and HW-0401 is
  "Should."
- **Confidence**: High
- **Remediation**: Add to HW-0202: "The 1-Wire pull-up MUST connect to the
  gated sensor power rail when sensor power gating is enabled."

---

### Finding F-005: Multiple sensors without power gating can exceed 20 µA

- **Severity**: Medium
- **Category**: Implicit Assumption
- **Invariant violated**: INV-3 (Deep Sleep Current ≤ 20 µA)
- **Spec sections**: HW-0301, HW-0400, HW-0401
- **Description**: HW-0301 lists 6 sensors. A configuration with external
  sensors on always-on power (HW-0401 is "Should") may exceed the deep sleep
  budget. For example, a capacitive soil moisture sensor with an active
  oscillator circuit draws milliamps continuously. The spec says nothing about
  external sensor sleep current requirements.
- **Violating interpretation**: Board configured with soil moisture sensor on
  ADC. Sensor has an oscillator drawing 5 mA continuously. No power gating
  (HW-0401 is "Should"). Total sleep current exceeds 20 µA.
- **Disproof attempt**: HW-1103 AC1 checks "sum of known always-on loads ≤
  rail budget." But "known" loads may not include external sensors — only
  on-board components.
- **Confidence**: Medium — depends on what sensors are actually connected.
- **Remediation**: Elevate HW-0401 from "Should" to "Must" for configurations
  that include direct sensor footprints (HW-0300), OR add: "External sensors
  connected to always-on rails MUST have documented sleep current that, when
  summed with board quiescent current, does not exceed 20 µA."

---

### Finding F-006: Battery sense divider impedance not specified

- **Severity**: Informational
- **Category**: Positive — spec addresses this (mostly)
- **Invariant violated**: None (INV-3 addressed but could be tighter)
- **Spec sections**: HW-0103
- **Description**: HW-0103 AC4 explicitly requires the battery sense path to
  be "high-impedance or switchable in deep sleep so that total deep-sleep
  current... complies with HW-0400 (≤ 20 µA)." This is well-specified.
  However, no specific resistance value is given for "high-impedance."
  [INFERRED] A 10 MΩ divider would leak 0.33 µA at 3.3V — acceptable. But a
  100 kΩ divider would leak 33 µA — over budget.
- **Confidence**: High
- **Remediation**: Consider adding a minimum resistance: "high-impedance
  (≥ 1 MΩ total divider impedance)."

---

### Finding F-007: Contract invariant checks not mandatory in CI

- **Severity**: Informational
- **Category**: Gap — weakened enforcement
- **Invariant violated**: INV-3 (indirectly)
- **Spec sections**: HW-1103, HW-1104
- **Description**: HW-1103 (contract invariant checks) is "Must" — the tool
  MUST validate invariants. But HW-1104 (CI integration) is "Should" — running
  these checks in CI is optional. The build pipeline (HW-0901 AC5) lists
  `validate → generate → ERC → DRC → Gerber` but does not include contract
  invariant checks.
- **Confidence**: Medium
- **Remediation**: Either elevate HW-1104 to "Must" or add contract invariant
  checks to the HW-0901 pipeline command.

---

## 5. Coverage Matrix

| Spec Section | INV-1 (Recoverability) | INV-2 (Update Atomicity) | INV-3 (Sleep ≤ 20 µA) |
|---|---|---|---|
| §3 HW-0100 MCU | **F-001** | **F-003** | Clean |
| §3 HW-0101 USB-C | **F-001** | Clean | Clean |
| §3 HW-0102 Regulator | Clean | Clean | Clean (Iq ≤ 10 µA) |
| §3 HW-0103 Battery | Clean | Clean | F-006 (informational) |
| §4 HW-0200 I2C | Clean | Clean | **F-002** |
| §4 HW-0201 SPI | Clean | Clean | Clean |
| §4 HW-0202 1-Wire | Clean | Clean | **F-004** |
| §4 HW-0203 GPIO | **F-001** | Clean | Clean |
| §4 HW-0204 ADC | Clean | Clean | Clean |
| §5 HW-0300/0301 Sensors | Clean | Clean | **F-005** |
| §6 HW-0400 Sleep current | Clean | Clean | (defines invariant) |
| §6 HW-0401 Power gating | Clean | Clean | **F-005** |
| §7 Mechanical | Clean | Clean | Clean |
| §8–11 Tooling | Clean | Clean | Clean |
| §12 Verification | Clean | Clean | Clean |
| §13 Contract | Clean | Clean | **F-007** |

## 6. Remediation Plan

| Priority | Finding | Fix | Effort |
|----------|---------|-----|--------|
| 1 | F-001 | Add BOOT button + reset button requirement | S |
| 2 | F-002 | Specify pull-up rail must be gated sensor rail | S |
| 3 | F-004 | Same — 1-Wire pull-up to gated rail | S |
| 4 | F-003 | Add flash partitioning cross-reference | S |
| 5 | F-005 | Elevate HW-0401 to Must when sensors present | S |
| 6 | F-007 | Add contract checks to build pipeline | S |
| 7 | F-006 | Add min divider impedance | S |

## 7. Prevention

- Add a "Recovery Mechanisms" section to the hardware spec covering physical
  reset, download mode entry, and watchdog behavior
- Promote HW-1104 (CI invariant checks) from "Should" to "Must"
- Cross-reference `node-requirements.md` for partition layout constraints
- Require pull-up rails to be explicitly specified in all bus requirements

## 8. Open Questions

1. **GPIO9 accessibility**: Does the ESP32-C3 module's integrated antenna
   design leave GPIO9 accessible? The module datasheet would confirm whether
   GPIO9 is exposed on the module pins.
2. **ESP32-C3 USB-JTAG fallback**: The ESP32-C3 has a built-in USB-JTAG
   controller. Can it enter download mode via USB without GPIO9? If yes,
   F-001 severity could be reduced.
3. **Partition layout**: Does `node-requirements.md` require A/B partitioning?
   If yes, F-003 is mitigated.

## 9. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-03-29 | Copilot (spec audit agent) | Initial audit |
