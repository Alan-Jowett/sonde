# Sonde Sensor Node — Hardware Requirements

## 1  Purpose

This document defines requirements for a customizable ESP32-C3 sensor node
PCB that runs the sonde firmware. The design is parameterized: operators
select sensors, connectors, and peripherals from a menu, and the tooling
generates a board-specific schematic, layout, BOM, and Gerber files.

The goal is a single reference design that covers soil monitoring, air
quality, industrial sensing, and other applications — without requiring
a custom PCB for each deployment.

---

## 2  Scope

**In scope:**
- ESP32-C3 module (RISC-V, WiFi, BLE)
- Power regulation (battery + USB)
- Peripheral connectors (I2C/Qwiic, SPI, 1-Wire, GPIO)
- Direct sensor footprints (optional, parameterized)
- Programming/debug interface (USB-C)
- Antenna considerations (ESP-NOW range)

**Out of scope:**
- Modem hardware (separate ESP32-S3 board)
- Enclosure design
- Production testing fixtures (future)

---

## 3  Core components

### HW-0100  Microcontroller

**Priority:** Must

The board MUST use an ESP32-C3 module with integrated antenna, at least
4 MB flash, and all GPIO pins accessible.

**Acceptance criteria:**

1. ESP32-C3 module with integrated PCB antenna or U.FL connector.
2. Minimum 4 MB SPI flash.
3. All user-accessible GPIO pins routed to pads or connectors.

---

### HW-0101  USB-C connector

**Priority:** Must

The board MUST include a USB-C connector for programming, debugging,
and power input.

**Acceptance criteria:**

1. USB-C receptacle connected to the ESP32-C3 native USB peripheral.
2. USB provides 5V power input to the voltage regulator.
3. Compatible with `espflash` for firmware flashing without external tools.

---

### HW-0102  Voltage regulator

**Priority:** Must

The board MUST include a low-dropout (LDO) or switching regulator that
accepts a range of input voltages and provides 3.3V to the ESP32-C3.

**Acceptance criteria:**

1. Input voltage range: 3.5V–6V (USB 5V + battery headroom).
2. Output: 3.3V ± 5%, minimum 500 mA continuous.
3. Quiescent current ≤ 10 µA (for battery-powered deep sleep).
4. Reverse polarity protection on battery input.

---

### HW-0103  Battery input

**Priority:** Should

The board SHOULD support direct battery power for untethered deployment.

**Acceptance criteria:**

1. JST-PH 2-pin connector for single-cell LiPo (3.7V nominal).
2. Battery voltage readable via ADC for firmware battery monitoring.
3. USB and battery inputs safely coexist (USB preferred when present).

---

## 4  Peripheral interfaces

### HW-0200  I2C bus (Qwiic/STEMMA QT)

**Priority:** Must

The board MUST provide at least one I2C bus with standardized connectors
for plug-and-play sensor modules.

**Acceptance criteria:**

1. I2C0 SDA and SCL routed to at least one 4-pin Qwiic/STEMMA QT
   connector (JST-SH 1.0mm pitch).
2. 4.7 kΩ pull-up resistors on SDA and SCL.
3. Second Qwiic connector for daisy-chaining (Should priority).
4. GPIO pin assignments configurable via NVS (ND-0608) — the PCB
   default pin mapping is documented and matches the firmware defaults.

---

### HW-0201  SPI bus

**Priority:** Should

The board SHOULD expose an SPI bus for high-speed peripherals (displays,
SD cards, LoRa modules).

**Acceptance criteria:**

1. SPI MOSI, MISO, CLK, and CS routed to a pin header or connector.
2. At least one CS line; additional CS lines as GPIO breakout.
3. Compatible with 3.3V SPI peripherals.

---

### HW-0202  1-Wire bus

**Priority:** Should

The board SHOULD expose a 1-Wire bus for temperature sensors (DS18B20).

**Acceptance criteria:**

1. One GPIO pin routed to a 3-pin header (DATA, VCC, GND).
2. 4.7 kΩ pull-up resistor on the DATA line.
3. Screw terminal or JST connector option.

---

### HW-0203  GPIO breakout

**Priority:** Must

The board MUST expose unused GPIO pins for application-specific wiring.

**Acceptance criteria:**

1. All GPIO pins not consumed by I2C, SPI, USB, or boot strapping
   are routed to 0.1" (2.54mm) pin headers.
2. Pin header silkscreen labels match GPIO numbers.
3. At least 4 GPIO pins available after I2C + USB allocation.

---

### HW-0204  ADC input

**Priority:** Should

The board SHOULD expose at least one ADC-capable pin for analog sensors.

**Acceptance criteria:**

1. At least one ADC channel routed to a pin header with GND reference.
2. Optional voltage divider footprint for sensors above 3.3V range.

---

## 5  Direct sensor footprints (parameterized)

### HW-0300  Sensor footprint slots

**Priority:** Should

The board SHOULD include optional footprints for common sensors that
can be populated or left empty based on the application.

**Acceptance criteria:**

1. Each sensor slot has a designated I2C address or GPIO pin.
2. Unpopulated slots do not interfere with other peripherals.
3. Sensor slots are selectable via the parameterized design tool.

---

### HW-0301  Supported sensor footprints

**Priority:** Should

The following sensor footprints SHOULD be available as options:

| Sensor | Interface | Package | Use case |
|--------|-----------|---------|----------|
| TMP102 | I2C (0x48) | SOT-563 | Temperature |
| SHT40 | I2C (0x44) | DFN-4 | Temperature + humidity |
| BME280 | I2C (0x76) | LGA-8 | Temp + humidity + pressure |
| VEML7700 | I2C (0x10) | — | Ambient light |
| DS18B20 | 1-Wire | TO-92 | Waterproof temperature probe |
| Soil moisture | ADC | 2-pin header | Capacitive soil sensor |

---

## 6  Power management

### HW-0400  Deep sleep current

**Priority:** Must

The board MUST achieve deep sleep current low enough for multi-month
battery operation.

**Acceptance criteria:**

1. Total board deep sleep current ≤ 20 µA (ESP32-C3 + regulator + pullups).
2. No active loads during deep sleep (LEDs off, peripherals power-gated
   or inherently low-power).

---

### HW-0401  Sensor power gating

**Priority:** Should

The board SHOULD support GPIO-controlled power to sensor connectors
so sensors can be fully powered off during deep sleep.

**Acceptance criteria:**

1. A MOSFET or load switch on the sensor power rail, controlled by a GPIO.
2. Firmware can enable/disable sensor power via `gpio_write()`.
3. Power gating is optional — can be bypassed with a solder jumper for
   always-on sensors.

---

## 7  Mechanical and manufacturing

### HW-0500  Board dimensions

**Priority:** Should

**Acceptance criteria:**

1. Maximum board size: 50mm × 30mm (fits standard project enclosures).
2. Mounting holes: 2× M2.5, positioned for standoff mounting.
3. All components on one side (single-side assembly for cost).

---

### HW-0501  Manufacturing constraints

**Priority:** Must

**Acceptance criteria:**

1. Minimum trace width: 0.15mm (6 mil).
2. Minimum clearance: 0.15mm (6 mil).
3. Minimum via diameter: 0.3mm drill, 0.6mm pad.
4. 2-layer PCB (cost-optimized; 4-layer option for RF performance).
5. Standard 1.6mm FR-4 substrate.
6. Compatible with JLCPCB / PCBWay / OSH Park manufacturing capabilities.

---

### HW-0502  Antenna keepout

**Priority:** Must

**Acceptance criteria:**

1. No copper (traces, ground plane, or components) within the antenna
   keepout zone specified by the ESP32-C3 module datasheet.
2. Board edge near antenna has no ground plane for at least 5mm.

---

## 8  Design parameterization

### HW-0600  Parameterized design tool

**Priority:** Must

The design MUST be parameterizable so that operators can select options
and generate a board-specific design without manual schematic editing.

**Acceptance criteria:**

1. A configuration file (YAML or TOML) specifies:
   - Number and type of Qwiic connectors (0, 1, or 2)
   - SPI header (yes/no)
   - 1-Wire header (yes/no)
   - Direct sensor footprints to populate
   - Battery connector (yes/no)
   - Sensor power gating (yes/no)
   - Board size variant (compact / standard)
2. The tool generates KiCad schematic (`.kicad_sch`) and PCB layout
   (`.kicad_pcb`) from the configuration.
3. The tool runs KiCad ERC and DRC checks and reports errors.
4. The tool generates Gerber files and BOM for manufacturing.

---

### HW-0601  Default configuration

**Priority:** Must

**Acceptance criteria:**

1. A default configuration exists that produces a minimal viable board:
   ESP32-C3 + USB-C + regulator + 1× Qwiic + GPIO breakout.
2. The default configuration passes all ERC and DRC checks.
3. The default BOM cost is ≤ $5 USD (excluding PCB fabrication).

---

## Appendix A  Requirement index

| ID | Title | Priority |
|---|---|---|
| HW-0100 | Microcontroller | Must |
| HW-0101 | USB-C connector | Must |
| HW-0102 | Voltage regulator | Must |
| HW-0103 | Battery input | Should |
| HW-0200 | I2C bus (Qwiic/STEMMA QT) | Must |
| HW-0201 | SPI bus | Should |
| HW-0202 | 1-Wire bus | Should |
| HW-0203 | GPIO breakout | Must |
| HW-0204 | ADC input | Should |
| HW-0300 | Sensor footprint slots | Should |
| HW-0301 | Supported sensor footprints | Should |
| HW-0400 | Deep sleep current | Must |
| HW-0401 | Sensor power gating | Should |
| HW-0500 | Board dimensions | Should |
| HW-0501 | Manufacturing constraints | Must |
| HW-0502 | Antenna keepout | Must |
| HW-0600 | Parameterized design tool | Must |
| HW-0601 | Default configuration | Must |
