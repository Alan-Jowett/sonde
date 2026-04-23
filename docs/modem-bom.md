<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Canonical Sonde Modem BOM (Carrier Board + XIAO ESP32-S3)

> **Document status:** Draft
> **Scope:** Canonical contributor hardware for the USB-attached Sonde modem.
> **Audience:** Contributors, developers, and anyone building the reference modem hardware used by the gateway.
> **Related:** [gateway-design.md](gateway-design.md), [getting-started.md](getting-started.md), [../hw/carrier-board/README.md](../hw/carrier-board/README.md)

---

## Design philosophy

- **Canonical** — contributor docs should assume a single modem hardware build.
- **Shared base hardware** — the modem reuses the repository's `hw/carrier-board` PCB as its base.
- **USB attached** — the modem is powered by and connected to the host running `sonde-gateway`.
- **Local UI included** — the canonical modem build includes a display and a button for local status/input work.

---

## 1  Core platform

| Item | Description | Notes |
|------|-------------|-------|
| `hw/carrier-board` PCB | Sonde carrier board from this repository | Canonical base PCB |
| Seeed Studio XIAO ESP32-S3 | ESP32-S3 module with USB-C | Runs `sonde-modem` firmware |
| USB-C cable | Host connection for power, flashing, and runtime CDC | Connects the modem to the host gateway machine |

> **Canonical build:** Use the carrier board with a Seeed Studio XIAO ESP32-S3. Contributor-facing repro notes should assume this modem platform unless they explicitly call out another board.

---

## 2  Local display and button

The canonical modem build includes the following local peripherals:

| Item | Description | Notes |
|------|-------------|-------|
| SSD1306-compatible OLED | 128×64 monochrome I2C display | Firmware expects address `0x3C` |
| Momentary pushbutton | Normally-open button input | Wired active-low |

### Wiring expectations

| Peripheral | Signal | GPIO | Notes |
|------------|--------|------|-------|
| OLED | SDA | 5 | I2C0 data |
| OLED | SCL | 6 | I2C0 clock |
| OLED | Address | — | `0x3C` |
| Button | Input | 2 | Active-low, internal pull-up enabled by firmware |

If you want the full contributor-reference behavior, add both the display and the button when building the modem.

---

## 3  Optional add-ons

| Item | Purpose |
|------|---------|
| Enclosure | Protects the modem during desk or field use |
| USB extension cable | Lets you reposition the modem for better RF coverage |
| Mounting hardware / adhesive | Secures the board and display inside an enclosure |

---

## 4  Block diagram

```
+------------------+     +----------------------+     +-------------------+
| PC Host          |---->| Sonde carrier board  |---->| XIAO ESP32-S3     |
| (sonde-gateway)  |     | + OLED + button      |     | sonde-modem fw    |
+------------------+     +----------------------+     +-------------------+
                                                           |
                                                           | ESP-NOW
                                                           v
                                                    +--------------+
                                                    |  Sonde nodes |
                                                    +--------------+
```

---

## 5  Notes

- **The modem is USB-powered.** No battery is required for the canonical modem build.
- **A custom PCB is part of the canonical build.** Use the repository's `hw/carrier-board` design as the base hardware.
- **The display and button are part of the contributor-reference hardware.** Add them if you are building the modem for UI/input work.
- **The carrier board README is the assembly source of truth.** Use it for board fabrication and mechanical details.
