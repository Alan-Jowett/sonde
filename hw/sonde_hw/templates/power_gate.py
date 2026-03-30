# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Power gate template: Si2301 P-FET with solder jumper bypass."""

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


def _make_pfet_symbol() -> Symbol:
    """P-channel MOSFET symbol (Gate, Source, Drain)."""
    sym = Symbol(entryName="Q_PMOS_GSD", libraryNickname="Device")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", "Q", 0, -4, 0),
        _prop("Value", "Q_PMOS_GSD", 0, 4, 1),
        _prop("Footprint", "", 0, 6, 2, hide=True),
        _prop("Datasheet", "~", 0, 8, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="Q_PMOS_GSD", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="input", graphicalStyle="line",
                  position=Position(-5.08, 0, 0), length=2.54,
                  name="G", number="1"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(0, 5.08, 270), length=2.54,
                  name="S", number="2"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(0, -5.08, 90), length=2.54,
                  name="D", number="3"),
    ]
    sym.units = [
        Symbol(entryName="Q_PMOS_GSD", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_solder_jumper_symbol() -> Symbol:
    """Solder jumper (normally open)."""
    sym = Symbol(entryName="SolderJumper_2_Open",
                 libraryNickname="Jumper")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", "SJ", 0, -3, 0),
        _prop("Value", "SolderJumper_2_Open", 0, 3, 1),
        _prop("Footprint", "", 0, 5, 2, hide=True),
        _prop("Datasheet", "~", 0, 7, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="SolderJumper_2_Open", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-2.54, 0, 0), length=2.54,
                  name="A", number="1"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(2.54, 0, 180), length=2.54,
                  name="B", number="2"),
    ]
    sym.units = [
        Symbol(entryName="SolderJumper_2_Open", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def template_power_gate(
    config: BoardConfig,
    origin: tuple[float, float],
    ref_alloc: RefAllocator,
    uuid_gen: UuidGenerator,
) -> TemplateResult:
    """Generate the sensor power gate block.

    Contains: Si2301 P-FET (Q1), gate pull-up (R9), gate drive (R10),
    solder jumper (SJ1).
    """
    ox, oy = origin
    block = "power_gate"
    result = TemplateResult()
    overrides = config.bom_overrides

    result.lib_symbols.extend([
        _make_pfet_symbol(),
        _make_solder_jumper_symbol(),
        make_passive_symbol("Device", "R"),
    ])

    # Q1 — Si2301 P-FET
    q1_ref = "Q1"
    q1_ov = overrides.get(q1_ref, {})
    q1_x, q1_y = ox, oy
    q1 = make_component(
        "Device", "Q_PMOS_GSD", q1_ref, "Si2301",
        "Package_TO_SOT_SMD:SOT-23",
        q1_x, q1_y, 0, uuid_gen, f"{block}/{q1_ref}",
        ["1", "2", "3"],
        lcsc=q1_ov.get("lcsc", "C306861"),
        mpn=q1_ov.get("mpn", "Si2301"),
    )
    result.instances.append(q1)

    # R9 — Gate pull-up (10 kΩ to 3V3)
    r9_ref = "R9"
    r9_x, r9_y = ox - 10.16, oy - 5.08  # -4*G, -2*G
    r9 = make_component(
        "Device", "R", r9_ref, "10kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r9_x, r9_y, 0, uuid_gen, f"{block}/{r9_ref}",
        ["1", "2"], lcsc="C25744",
    )
    result.instances.append(r9)

    # R10 — Gate drive resistor (10 kΩ from GPIO3)
    r10_ref = "R10"
    r10_x, r10_y = ox - 15.24, oy  # -6*G
    r10 = make_component(
        "Device", "R", r10_ref, "10kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r10_x, r10_y, 0, uuid_gen, f"{block}/{r10_ref}",
        ["1", "2"], lcsc="C25744",
    )
    result.instances.append(r10)

    # JP1 — Solder jumper bypass
    jp1_ref = "JP1"
    jp1_x, jp1_y = ox + 10.16, oy  # 4*G
    jp1 = make_component(
        "Jumper", "SolderJumper_2_Open", jp1_ref, "SJ_Bypass",
        "Jumper:SolderJumper-2_P1.3mm_Open_Pad1.0x1.5mm",
        jp1_x, jp1_y, 0, uuid_gen, f"{block}/{jp1_ref}",
        ["1", "2"],
    )
    result.instances.append(jp1)

    # Labels at exact pin endpoint coordinates (Y-down: schematic_y = comp_y - pin_dy)
    ln = 0

    def _l(text, x, y, rot=0):
        nonlocal ln
        lb = make_label(text, x, y, rot, uuid_gen, f"{block}/label/{ln}")
        ln += 1
        result.labels.append(lb)

    # Q1 P-FET (G=(-5.08,0)→y unchanged, S=(0,5.08)→y-5.08, D=(0,-5.08)→y+5.08)
    _l("GATE_Q1", q1_x - 5.08, q1_y, 0)            # pin 1 Gate
    _l("3V3", q1_x, q1_y - 5.08, 0)                 # pin 2 Source
    _l("SENSOR_3V3", q1_x, q1_y + 5.08, 0)          # pin 3 Drain

    # R9 gate pull-up (passive: pin1 dy=+1.27 → y-1.27, pin2 dy=-1.27 → y+1.27)
    _l("3V3", r9_x, r9_y - 1.27, 0)                 # pin 1
    _l("GATE_Q1", r9_x, r9_y + 1.27, 0)             # pin 2

    # R10 gate drive resistor
    _l("GATE_Q1", r10_x, r10_y - 1.27, 0)           # pin 1
    _l("SENSOR_PWR_EN", r10_x, r10_y + 1.27, 0)     # pin 2

    # JP1 solder jumper bypass (pin_dy=0, no Y-flip needed)
    _l("3V3", jp1_x - 2.54, jp1_y, 0)              # pin 1 A
    _l("SENSOR_3V3", jp1_x + 2.54, jp1_y, 0)       # pin 2 B

    result.interface_nets = {
        "3V3": (q1_x, q1_y - 5.08),
        "SENSOR_3V3": (q1_x, q1_y + 5.08),
        "SENSOR_PWR_EN": (r10_x, r10_y + 1.27),
    }

    return result
