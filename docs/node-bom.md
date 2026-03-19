<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Standard Sonde Node BOM (Rev A)

> **Document status:** Draft
> **Scope:** Minimal, off-the-shelf hardware platform for battery-powered ESP-NOW sonde nodes with Qwiic/STEMMA QT sensors.
> **Audience:** Contributors, field deployers, and anyone building a sonde node from commodity parts.
> **Related:** [node-design.md](node-design.md), [getting-started.md](getting-started.md)

---

## Design philosophy

- **Simple** — no custom PCBs, no specialized tools.
- **Stable** — all parts are commodity, in-production, and globally available.
- **Vendor-agnostic** — no lock-in to a single supplier; at least two sources listed for every part.
- **Sensor-agnostic** — the platform makes no assumptions about what sensors you attach. Sensor logic lives in BPF programs, not firmware.
- **Reproducible** — a contributor anywhere in the world can order the same parts and get a working node.

---

## 1  Core compute

| Item | Description | Notes |
|------|-------------|-------|
| Adafruit QT Py ESP32-C3 | ESP32-C3 dev board with STEMMA QT | Low-power RISC-V MCU, Qwiic-compatible, USB-C |

**Why this board:**
- Excellent deep-sleep current (~5 µA).
- Built-in Qwiic/STEMMA QT connector for I²C sensors.
- No RF layout risk — proven antenna design.
- No custom PCB required.
- Globally available from multiple distributors.

> **Note:** Adafruit does not currently produce an ESP32-C3 board in the Feather
> form factor. The QT Py ESP32-C3 (product 5405) is the recommended board. If
> you need a Feather-sized board, the
> [ESP32-C6 Feather](https://www.adafruit.com/product/5933) (product 5933) is
> the closest alternative, but `sonde-node` firmware currently targets the C3
> (`riscv32imc-esp-espidf`). A C6 port would require minor target changes.

**Where to buy:**

| Vendor | Link |
|--------|------|
| Adafruit (official) | <https://www.adafruit.com/product/5405> |
| Mouser | <https://www.mouser.com/ProductDetail/Adafruit/5405> |
| Newark | <https://www.newark.com/adafruit/5405/adafruit-qt-py-esp32-c3-wifi-dev/dp/47AK0768> |
| Pimoroni (UK) | <https://shop.pimoroni.com/products/adafruit-qt-py-esp32-c3-wifi-dev-board-with-stemma-qt> |
| Core Electronics (AU) | <https://core-electronics.com.au/adafruit-qt-py-esp32-c3-wifi-dev-board-with-stemma-qt.html> |

---

## 2  Power system

| Item | Description | Notes |
|------|-------------|-------|
| 2× Energizer L91 AA Lithium | Primary lithium (non-rechargeable) | Long shelf life (20 yr), cold-tolerant (−40 °C), low self-discharge |
| AA Battery Holder (2-cell, series) | Side-by-side or stacked, wire leads | Choose based on enclosure fit |
| Pololu S7V8F3 Buck/Boost Regulator | 3.3 V fixed output, 2.7–11.8 V input | Efficient across the full AA discharge curve |

### Power wiring

```
AA holder (+) ──→ Pololu VIN
Pololu 3.3V  ──→ QT Py 3V pin
Pololu GND   ──→ QT Py GND
```

This bypasses any on-board charging circuitry and ensures stable 3.3 V operation across the entire AA discharge curve (3.0 V fresh → ~1.8 V depleted × 2 cells = 3.6–3.0 V input range).

### Where to buy — batteries

| Vendor | Link |
|--------|------|
| DigiKey | <https://www.digikey.com/en/products/detail/energizer-battery-company/L91/704843> |
| Newark | <https://www.newark.com/energizer/l91/battery-non-rechargeable-1-5v/dp/18T1822> |
| Amazon | <https://www.amazon.com/energizer-aa-lithium-batteries/s?k=energizer+aa+lithium+batteries> |

### Where to buy — battery holder

| Vendor | Link |
|--------|------|
| Adafruit (with switch + JST) | <https://www.adafruit.com/product/4193> |
| Adafruit (open, with JST) | <https://www.adafruit.com/product/4194> |
| DigiKey | <https://www.digikey.com/en/products/filter/battery-holders-clips-contacts/aa/86> |
| Mouser | <https://www.mouser.com/c/power/battery-holders-clips-contacts/?battery%20cell%20size=AA&number%20of%20batteries=2%20Battery> |

### Where to buy — regulator

| Vendor | Link |
|--------|------|
| Pololu (official) | <https://www.pololu.com/product/2122> |
| Pimoroni (UK) | <https://shop.pimoroni.com/products/pololu-3-3v-step-up-step-down-voltage-regulator-s7v8f3> |
| RobotShop | <https://www.robotshop.com/products/pololu-33v-step-up-step-down-voltage-regulator-s7v8f3> |

---

## 3  Sensor connectivity

| Item | Description | Notes |
|------|-------------|-------|
| Qwiic/STEMMA QT Cable (100–200 mm) | JST SH 4-pin, I²C | Main sensor lead exiting enclosure; length depends on deployment |
| Qwiic MultiPort (optional) | 4-port hub for branching | Only needed if cable routing prevents daisy-chaining |

Any 3.3 V I²C sensor with a Qwiic/STEMMA QT connector is supported. Sensor logic lives entirely in BPF programs — no firmware changes needed when swapping sensors.

### Where to buy — cable

| Vendor | Link |
|--------|------|
| Adafruit (100 mm) | <https://www.adafruit.com/product/4210> |
| Adafruit (200 mm) | <https://www.adafruit.com/product/4401> |
| SparkFun | <https://www.sparkfun.com/components/cables/qwiic-cables.html> |
| Newark (100 mm) | <https://www.newark.com/adafruit/4210/stemma-qt-qwiic-jst-sh-4-pin-cable/dp/27AH9039> |

### Where to buy — multiport (optional)

| Vendor | Link |
|--------|------|
| SparkFun (official) | <https://www.sparkfun.com/sparkfun-qwiic-multiport.html> |
| Adafruit | <https://www.adafruit.com/product/4861> |
| Mouser | <https://www.mouser.com/ProductDetail/SparkFun/BOB-18012> |

---

## 4  Enclosure and mounting

| Item | Description | Notes |
|------|-------------|-------|
| Hammond 1591B or 1591G | Small ABS project box | Fits QT Py + AA holder + regulator |
| Cable Gland (PG7 or PG9) | Nylon, IP68 rated | Weather-resistant strain relief for sensor cable exit |
| Silicone Adhesive (RTV) | General-purpose RTV sealant | Internal mounting — no screws or standoffs needed |

**Why silicone adhesive (not screws):**
- Vibration-resistant.
- Easy to service (peel apart).
- Works across wide temperature range.
- No standoffs or hardware to source.

### Where to buy — enclosure

| Vendor | Link |
|--------|------|
| DigiKey | <https://www.digikey.com/en/product-highlight/h/hammond/1591-series-multipurpose-enclosures> |
| Mouser | <https://www.mouser.com/c/enclosures/enclosures-boxes-cases/?m=Hammond&series=1591> |
| Newark | <https://www.newark.com/hammond/1591asbk/enclosure-multipurpose-abs-black/dp/87F2522> |

### Where to buy — cable gland

| Vendor | Link |
|--------|------|
| DigiKey | <https://www.digikey.com/en/products/detail/phoenix-contact/1424495/7557542> |
| Mouser | <https://www.mouser.com/c/?product%20type=Cable%20Glands&screw%2Fthread%20size=PG7> |
| Amazon | <https://www.amazon.com/pg7-cable-gland/s?k=pg7+cable+gland> |

---

## 5  Optional quality-of-life items

These are not required but improve build quality in the field:

| Item | Purpose |
|------|---------|
| Small perfboard (25 × 50 mm) | Clean mounting surface for the regulator |
| Heat-shrink tubing (assorted) | Strain relief on power leads |
| Desiccant pack | Moisture control if enclosure is sealed |
| Conformal coating spray | PCB protection for high-humidity deployments |

These are generic supplies available at any electronics distributor or hardware store.

---

## 6  Block diagram

```
+----------------+     +---------------------+     +------------------+     +--------------+
| 2x AA          |     | Pololu S7V8F3       |     | QT Py ESP32-C3   |     | Qwiic/STEMMA |
| Lithium L91    |---->| 3.3 V Buck/Boost    |---->| sonde-node fw    |---->| QT Sensor(s) |
+----------------+     +---------------------+     +------------------+     +--------------+
                                                           |
                                                           | ESP-NOW
                                                           v
                                                    +--------------+
                                                    |   Gateway    |
                                                    |  (via modem) |
                                                    +--------------+
```

**Duty cycle:** Wake → run BPF program → read sensors → ESP-NOW transmit → deep sleep.

---

## 7  Estimated per-node cost

| Category | Approximate cost (USD) |
|----------|----------------------:|
| QT Py ESP32-C3 | ~$10 |
| 2× AA Lithium | ~$3 |
| Battery holder | ~$2 |
| Pololu regulator | ~$10 |
| Qwiic cable | ~$1 |
| Enclosure | ~$8 |
| Cable gland | ~$1 |
| Misc (silicone, heat-shrink) | ~$2 |
| **Total (excluding sensor)** | **~$37** |

Prices are approximate single-unit retail. Volume discounts apply.

---

## 8  Notes

- **Sensors are not included in this BOM.** Sonde follows a BYOS (Bring Your Own Sensor) model — any 3.3 V I²C Qwiic/STEMMA QT sensor works. Sensor behavior is defined by BPF programs distributed by the gateway.
- **No custom PCB is required.** Everything connects with wire leads and Qwiic cables.
- **The BOM is intentionally conservative.** All parts are in active production with long product life cycles.
- **International availability.** Every part is listed with at least two vendors spanning North America, Europe, and Oceania.
- **Lithium battery shipping restrictions** may apply. Check your carrier's hazmat policies before ordering large quantities.
