# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Qwiic connector template: JST-SH 4-pin I2C connector."""

from __future__ import annotations

from kiutils.symbol import Symbol, SymbolPin
from kiutils.items.common import Position

from sonde_hw.config import BoardConfig
from sonde_hw.templates import (
    RefAllocator,
    TemplateResult,
    UuidGenerator,
    _prop,
    make_component,
    make_label,
    make_passive_symbol,
    make_wire,
)


def _make_qwiic_symbol() -> Symbol:
    """JST-SH 4-pin Qwiic connector symbol."""
    sym = Symbol(entryName="Qwiic_Connector", libraryNickname="sonde")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "J", 0, -5, 0),
        _prop("Value", "Qwiic", 0, 5, 1),
        _prop("Footprint", "", 0, 7, 2, hide=True),
        _prop("Datasheet", "~", 0, 9, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="Qwiic_Connector", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-5.08, 3.81, 0), length=2.54,
                  name="GND", number="1"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-5.08, 1.27, 0), length=2.54,
                  name="VCC", number="2"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-5.08, -1.27, 0), length=2.54,
                  name="SDA", number="3"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-5.08, -3.81, 0), length=2.54,
                  name="SCL", number="4"),
    ]
    sym.units = [
        Symbol(entryName="Qwiic_Connector", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def template_qwiic(
    config: BoardConfig,
    origin: tuple[float, float],
    ref_alloc: RefAllocator,
    uuid_gen: UuidGenerator,
    instance_index: int = 0,
) -> TemplateResult:
    """Generate a Qwiic connector block.

    For the first instance (index 0), also emits I2C pull-up resistors
    R7 and R8 (4.7 kΩ to SENSOR_3V3).
    """
    ox, oy = origin
    block = f"qwiic_{instance_index}"
    result = TemplateResult()
    overrides = config.bom_overrides

    # Library symbols
    result.lib_symbols.append(_make_qwiic_symbol())
    if instance_index == 0:
        result.lib_symbols.append(make_passive_symbol("Device", "R"))

    # Connector
    j_ref = ref_alloc.next("J")
    j_ov = overrides.get(j_ref, {})
    jx, jy = ox, oy
    j = make_component(
        "sonde", "Qwiic_Connector", j_ref, "Qwiic",
        "Connector_JST:JST_SH_SM04B-SRSS-TB_1x04-1MP_TopEntry",
        jx, jy, 0, uuid_gen, f"{block}/{j_ref}",
        ["1", "2", "3", "4"],
        lcsc=j_ov.get("lcsc", "C145956"),
        mpn=j_ov.get("mpn", "SM04B-SRSS-TB"),
    )
    result.instances.append(j)

    wn, ln = 0, 0

    def _l(text, x, y, rot=0):
        nonlocal ln
        lb = make_label(text, x, y, rot, uuid_gen, f"{block}/label/{ln}")
        ln += 1
        result.labels.append(lb)

    # Pin labels (Y-down: schematic_y = comp_y - pin_dy)
    _l("GND", jx - 5.08, jy - 3.81, 0)        # pin 1
    _l("SENSOR_3V3", jx - 5.08, jy - 1.27, 0) # pin 2
    _l("I2C0_SDA", jx - 5.08, jy + 1.27, 0)   # pin 3
    _l("I2C0_SCL", jx - 5.08, jy + 3.81, 0)   # pin 4

    # I2C pull-ups (only on first instance)
    if instance_index == 0:
        # R7 — SDA pull-up (4.7 kΩ)
        r7_ref = ref_alloc.next("R")
        r7_x, r7_y = ox + 15.24, oy - 10.16  # 6*G, -4*G
        r7 = make_component(
            "Device", "R", r7_ref, "4.7kΩ",
            "Resistor_SMD:R_0402_1005Metric",
            r7_x, r7_y, 0, uuid_gen, f"{block}/{r7_ref}",
            ["1", "2"], lcsc="C25900",
        )
        result.instances.append(r7)
        _l("SENSOR_3V3", r7_x, r7_y - 1.27, 0)   # pin 1
        _l("I2C0_SDA", r7_x, r7_y + 1.27, 0)     # pin 2

        # R8 — SCL pull-up (4.7 kΩ)
        r8_ref = ref_alloc.next("R")
        r8_x, r8_y = ox + 25.4, oy - 10.16  # 10*G, -4*G
        r8 = make_component(
            "Device", "R", r8_ref, "4.7kΩ",
            "Resistor_SMD:R_0402_1005Metric",
            r8_x, r8_y, 0, uuid_gen, f"{block}/{r8_ref}",
            ["1", "2"], lcsc="C25900",
        )
        result.instances.append(r8)
        _l("SENSOR_3V3", r8_x, r8_y - 1.27, 0)   # pin 1
        _l("I2C0_SCL", r8_x, r8_y + 1.27, 0)     # pin 2

    result.interface_nets = {
        "GND": (jx - 5.08, jy - 3.81),
        "SENSOR_3V3": (jx - 5.08, jy - 1.27),
        "I2C0_SDA": (jx - 5.08, jy + 1.27),
        "I2C0_SCL": (jx - 5.08, jy + 3.81),
    }

    return result
