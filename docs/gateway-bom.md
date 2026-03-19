<!-- SPDX-License-Identifier: MIT
   Copyright (c) 2026 sonde contributors -->
# Sonde Gateway BOM (Rev A)

> **Document status:** Draft
> **Scope:** Minimal, off-the-shelf hardware platform for a USB-attached ESP-NOW gateway.
> **Audience:** Contributors, developers, and anyone building a sonde gateway from commodity parts.
> **Related:** [gateway-design.md](gateway-design.md), [getting-started.md](getting-started.md), [node-bom.md](node-bom.md)

---

## Design philosophy

- **Simple** — no custom PCBs, no soldering required.
- **Stable** — USB-powered, always-on, designed for long-running gateway service.
- **Vendor-agnostic** — at least two sources for every part; no lock-in.
- **Reproducible** — a contributor anywhere in the world can order the same parts and get a working gateway.
- **Zero RF risk** — proven on-board antenna designs; no external RF components.

---

## 1  Core compute (choose one)

Both options are fully supported. Contributors can pick either board.

### Option A: ESP32-S3 DevKitC-1 (recommended)

| Item | Description | Notes |
|------|-------------|-------|
| ESP32-S3-DevKitC-1 | ESP32-S3 dev board with USB-C | Native USB OTG, plenty of RAM/flash |

**Why this board:**
- Native USB OTG — reliable CDC device on the host.
- Generous RAM and flash headroom for gateway logic.
- Stable, official Espressif board with proven antenna design.
- Globally available from multiple distributors.

**Where to buy:**

| Vendor | Link |
|--------|------|
| Espressif (official) | <https://www.espressif.com/en/products/devkits/esp32-s3-devkitc-1> |
| DigiKey | <https://www.digikey.com/en/products/filter/rf-evaluation-and-development-kits-boards/859?s=N4IgjCBcpgDAdFAZhQMYBsDOBTANCAPZQDaIALGGABxwDsIAuoQA4AuUIAyuQE4CWAOwDmIAL4EAtKhCYcBEGUo0GLNl14ChYiQHpxQA> |
| Mouser | <https://www.mouser.com/ProductDetail/Espressif-Systems/ESP32-S3-DevKitC-1-N8R8> |

### Option B: ESP32-C3 DevKitM-1

| Item | Description | Notes |
|------|-------------|-------|
| ESP32-C3-DevKitM-1 | ESP32-C3 dev board with USB-C | Low-power RISC-V, native USB, simpler |

**Why this board:**
- Lower power draw, simpler architecture.
- Native USB support.
- Adequate for gateway workloads (ESP-NOW RX + USB framing).
- Slightly less headroom than S3 but fully sufficient.

**Where to buy:**

| Vendor | Link |
|--------|------|
| Espressif (official) | <https://www.espressif.com/en/products/devkits/esp32-c3-devkitm-1> |
| DigiKey | <https://www.digikey.com/en/products/filter/rf-evaluation-and-development-kits-boards/859?s=N4IgjCBcpgDAdFAZhQMYBsDOBTANCAPZQDaIALGGABxwDsIAuoQA4AuUIAyuQE4CWAOwDmIAL4EAtKhCYcBEGUo0GLNl14ChYiQHpxQA> |
| Mouser | <https://www.mouser.com/ProductDetail/Espressif-Systems/ESP32-C3-DevKitM-1> |

### Which to choose

Your gateway workload is light — ESP-NOW receive plus USB CDC framing. The S3 gives more margin; the C3 gives simplicity. Either works.

---

## 2  USB connectivity

The gateway needs two physical USB ports:

1. **Runtime USB (CDC)** — stays plugged into the PC host running the gateway service.
2. **Flashing USB** — used only when updating firmware.

Because dev boards expose a single USB port, the simplest solution is a USB data switch. This provides a second physical USB port without modifying the board.

| Item | Description | Notes |
|------|-------------|-------|
| USB-C Data Switch (2-port) | Manual toggle switch | Provides dedicated runtime and flashing ports |
| USB-C Cable (1–2 ft) | Runtime connection | Stays plugged into the PC |
| USB-C Cable (short) | Flashing connection | Only used during firmware updates |

### How it works

```
Board USB-C  ──→  USB Data Switch Input
                        ├── Output A  ──→  PC host (runtime CDC)
                        └── Output B  ──→  Flashing cable
```

- The board's single USB port connects to the switch input.
- Output A goes to the PC host for the always-on gateway service.
- Output B is used only when flashing firmware.
- Toggle the switch to flash. The gateway service will briefly lose the CDC device during the switch, but you avoid physically replugging cables or changing ports.

### Where to buy — USB data switch

| Vendor | Link |
|--------|------|
| Amazon | <https://www.amazon.com/usb-c-switch-2-port/s?k=usb+c+switch+2+port> |
| AliExpress | <https://www.aliexpress.com/w/wholesale-usb-c-data-switch-2-port.html> |

> **Note:** Any manual USB-C 2-to-1 selector/sharing switch works. Look for
> "USB-C sharing switch" or "USB-C selector switch" — these are generic
> commodity parts. Ensure it supports data (not power-only).

### Where to buy — USB-C cables

| Vendor | Link |
|--------|------|
| Amazon | <https://www.amazon.com/usb-c-cable-short/s?k=usb+c+cable+short> |
| Monoprice | <https://www.monoprice.com/category/cables/usb-cables/usb-c-cables> |
| DigiKey | <https://www.digikey.com/en/products/filter/usb-cables/458?s=N4IgTCBcDaIGYFMC2BjALiAugXyA> |

---

## 3  Power

The gateway is USB-powered only. No regulator, no battery, no charging circuitry.

| Item | Description | Notes |
|------|-------------|-------|
| USB-C 5 V from PC | Power supplied by the host USB port | Board draws < 200 mA |
| USB Power Meter (optional) | Inline USB-C power monitor | Useful for debugging power draw |

### Where to buy — power meter (optional)

| Vendor | Link |
|--------|------|
| Amazon | <https://www.amazon.com/usb-c-power-meter/s?k=usb+c+power+meter> |
| AliExpress | <https://www.aliexpress.com/w/wholesale-usb-c-power-meter.html> |

---

## 4  RF

| Item | Description | Notes |
|------|-------------|-------|
| Onboard antenna (ESP32-S3 or C3) | PCB antenna on the dev board | Adequate for desk or under-desk placement |
| USB Extension Cable (optional) | USB-C or USB-A extension | Lets you reposition the gateway for better RF coverage |

No external RF components required. The onboard antenna is sufficient for typical indoor deployments (desk, shelf, or under-desk).

### Where to buy — USB extension cable (optional)

| Vendor | Link |
|--------|------|
| Amazon | <https://www.amazon.com/usb-c-extension-cable/s?k=usb+c+extension+cable> |
| Monoprice | <https://www.monoprice.com/category/cables/usb-cables/usb-c-cables> |

---

## 5  Enclosure (optional)

If you want a tidy, publishable enclosure:

| Item | Description | Notes |
|------|-------------|-------|
| Hammond 1591XXS or 1591XS | Small ABS project box | Fits the dev board comfortably |
| Panel-mount USB-C passthrough (optional) | Bulkhead USB-C connector | Clean front-panel connection |
| Silicone Adhesive (RTV) | General-purpose RTV sealant | Internal mounting — no screws or standoffs needed |

The enclosure is entirely optional. Many contributors will leave the board bare on their desk.

### Where to buy — enclosure

| Vendor | Link |
|--------|------|
| DigiKey | <https://www.digikey.com/en/product-highlight/h/hammond/1591-series-multipurpose-enclosures> |
| Mouser | <https://www.mouser.com/c/enclosures/enclosures-boxes-cases/?m=Hammond&series=1591> |
| Newark | <https://www.newark.com/hammond/1591asbk/enclosure-multipurpose-abs-black/dp/87F2522> |

### Where to buy — USB-C passthrough (optional)

| Vendor | Link |
|--------|------|
| Amazon | <https://www.amazon.com/usb-c-panel-mount/s?k=usb+c+panel+mount> |
| DigiKey | <https://www.digikey.com/en/products/filter/usb-connectors/312?s=N4IgTCBcDaIGYFMC2BjALiAugXyA> |

---

## 6  Block diagram

```
+------------------+     +-------------------+     +---------------------+
| PC Host          |     | USB Data Switch   |     | ESP32-S3/C3 DevKit  |
| (gateway service)|---->| (2-port toggle)   |---->| sonde-modem fw      |
+------------------+     +-------------------+     +---------------------+
                                ↑                           |
                         [Flashing Port]                    | ESP-NOW
                                                            v
                                                     +--------------+
                                                     |  Sonde Nodes |
                                                     +--------------+
```

**Runtime mode:** PC host ↔ USB CDC ↔ ESP32 dev board ↔ ESP-NOW radio ↔ sonde nodes.

---

## 7  Estimated per-gateway cost

| Category | Approximate cost (USD) |
|----------|----------------------:|
| ESP32-S3-DevKitC-1 (or C3) | ~$10 |
| USB-C data switch | ~$10 |
| USB-C cables (× 2) | ~$6 |
| Enclosure (optional) | ~$5 |
| Misc (silicone, passthrough) | ~$3 |
| **Total** | **~$34** |

Prices are approximate single-unit retail. Volume discounts apply. Cost is lower without the optional enclosure (~$26).

---

## 8  Notes

- **No battery is required.** The gateway is USB-powered and designed to run continuously.
- **No custom PCB is required.** Everything connects with off-the-shelf cables and a USB switch.
- **No soldering is required.** The entire build is plug-and-play.
- **The USB data switch is the key part.** It cleanly satisfies the two-USB-port requirement without board modifications.
- **International availability.** Every part is available globally from multiple distributors.
- **Both S3 and C3 are valid choices.** The S3 is recommended for headroom; the C3 is fine for simplicity.
- **The BOM is intentionally conservative.** All parts are in active production with long product life cycles.
