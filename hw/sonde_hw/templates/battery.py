# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Battery input template: JST-PH connector, Schottky, voltage divider."""

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


def _make_jst_ph_symbol() -> Symbol:
    """JST-PH 2-pin battery connector."""
    sym = Symbol(entryName="Conn_01x02", libraryNickname="Connector_Generic")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "J", 0, -4, 0),
        _prop("Value", "JST-PH", 0, 4, 1),
        _prop("Footprint", "", 0, 6, 2, hide=True),
        _prop("Datasheet", "~", 0, 8, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="Conn_01x02", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-5.08, 1.27, 0), length=2.54,
                  name="Pin_1", number="1"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-5.08, -1.27, 0), length=2.54,
                  name="Pin_2", number="2"),
    ]
    sym.units = [
        Symbol(entryName="Conn_01x02", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def template_battery(
    config: BoardConfig,
    origin: tuple[float, float],
    ref_alloc: RefAllocator,
    uuid_gen: UuidGenerator,
) -> TemplateResult:
    """Generate the battery input block.

    Contains: JST-PH connector (J3), voltage divider (R11, R12),
    ADC filter cap (C2).
    """
    ox, oy = origin
    block = "battery"
    result = TemplateResult()
    overrides = config.bom_overrides

    result.lib_symbols.extend([
        _make_jst_ph_symbol(),
        make_passive_symbol("Device", "R"),
        make_passive_symbol("Device", "C"),
    ])

    # J3 — Battery connector
    j3_ref = "J3"
    j3_ov = overrides.get(j3_ref, {})
    j3_x, j3_y = ox, oy
    j3 = make_component(
        "Connector_Generic", "Conn_01x02", j3_ref, "Battery",
        "Connector_JST:JST_PH_S2B-PH-SM4-TB_1x02-1MP_Horizontal",
        j3_x, j3_y, 0, uuid_gen, f"{block}/{j3_ref}",
        ["1", "2"],
        lcsc=j3_ov.get("lcsc", "C295747"),
        mpn=j3_ov.get("mpn", "S2B-PH-SM4-TB"),
    )
    result.instances.append(j3)

    # R11 — Top divider resistor (10 MΩ)
    r11_ref = "R11"
    r11_x, r11_y = ox + 15.24, oy  # 6*G
    r11 = make_component(
        "Device", "R", r11_ref, "10MΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r11_x, r11_y, 0, uuid_gen, f"{block}/{r11_ref}",
        ["1", "2"], lcsc="C26083",
    )
    result.instances.append(r11)

    # R12 — Bottom divider resistor (10 MΩ)
    r12_ref = "R12"
    r12_x, r12_y = ox + 15.24, oy + 7.62  # 6*G, 3*G
    r12 = make_component(
        "Device", "R", r12_ref, "10MΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r12_x, r12_y, 0, uuid_gen, f"{block}/{r12_ref}",
        ["1", "2"], lcsc="C26083",
    )
    result.instances.append(r12)

    # C2 — ADC filter cap (100 pF)
    c2_ref = "C2"
    c2_x, c2_y = ox + 22.86, oy + 5.08  # 9*G, 2*G
    c2 = make_component(
        "Device", "C", c2_ref, "100pF",
        "Capacitor_SMD:C_0402_1005Metric",
        c2_x, c2_y, 0, uuid_gen, f"{block}/{c2_ref}",
        ["1", "2"], lcsc="C1546",
    )
    result.instances.append(c2)

    # Labels
    ln = 0

    def _l(text, x, y, rot=0):
        nonlocal ln
        lb = make_label(text, x, y, rot, uuid_gen, f"{block}/label/{ln}")
        ln += 1
        result.labels.append(lb)

    # JST-PH connector (Y-down: pin1 dy=+1.27 → y-1.27, pin2 dy=-1.27 → y+1.27)
    _l("VBAT", j3_x - 5.08, j3_y - 1.27, 0)        # pin 1
    _l("GND", j3_x - 5.08, j3_y + 1.27, 0)         # pin 2

    # R11 voltage divider top (vertical passive)
    _l("VBAT", r11_x, r11_y - 1.27, 0)              # pin 1
    _l("VBAT_SENSE", r11_x, r11_y + 1.27, 0)        # pin 2

    # R12 voltage divider bottom
    _l("VBAT_SENSE", r12_x, r12_y - 1.27, 0)        # pin 1
    _l("GND", r12_x, r12_y + 1.27, 0)               # pin 2

    # C2 ADC filter cap
    _l("VBAT_SENSE", c2_x, c2_y - 1.27, 0)          # pin 1
    _l("GND", c2_x, c2_y + 1.27, 0)                 # pin 2

    result.interface_nets = {
        "VBAT": (j3_x - 5.08, j3_y - 1.27),
        "GND": (j3_x - 5.08, j3_y + 1.27),
        "VBAT_SENSE": (r11_x, r11_y + 1.27),
    }

    return result
