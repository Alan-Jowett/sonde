# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Base template: ESP32-C3 module, USB-C, LDO, ESD, buttons, strapping."""

from __future__ import annotations

from kiutils.items.common import Effects, Font, Position, Property, Stroke
from kiutils.items.schitems import Connection, LocalLabel, SchematicSymbol
from kiutils.symbol import Symbol, SymbolPin

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


# ---------------------------------------------------------------------------
# Library symbol factories
# ---------------------------------------------------------------------------

def _make_esp32c3_symbol() -> Symbol:
    """ESP32-C3-MINI-1 module library symbol."""
    sym = Symbol(entryName="ESP32-C3-MINI-1", libraryNickname="sonde")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "U", 0, -20, 0),
        _prop("Value", "ESP32-C3-MINI-1", 0, 20, 1),
        _prop("Footprint", "RF_Module:ESP32-C3-MINI-1", 0, 22, 2, hide=True),
        _prop("Datasheet", "~", 0, 24, 3, hide=True),
    ]

    pin_unit = Symbol(entryName="ESP32-C3-MINI-1", unitId=1, styleId=1)
    pin_defs = [
        ("1", "GND", "power_in", Position(-15.24, 15.24, 0)),
        ("2", "GND", "passive", Position(-15.24, 12.7, 0)),
        ("3", "3V3", "power_in", Position(-15.24, 10.16, 0)),
        ("4", "EN", "input", Position(-15.24, 7.62, 0)),
        ("5", "GPIO0", "bidirectional", Position(-15.24, 5.08, 0)),
        ("6", "GPIO1", "bidirectional", Position(-15.24, 2.54, 0)),
        ("7", "GPIO2", "bidirectional", Position(-15.24, 0, 0)),
        ("8", "GPIO3", "bidirectional", Position(-15.24, -2.54, 0)),
        ("9", "GPIO4", "bidirectional", Position(-15.24, -5.08, 0)),
        ("10", "GPIO5", "bidirectional", Position(-15.24, -7.62, 0)),
        ("11", "GPIO6", "bidirectional", Position(-15.24, -10.16, 0)),
        ("12", "GPIO7", "bidirectional", Position(-15.24, -12.7, 0)),
        ("13", "GPIO8", "bidirectional", Position(15.24, -12.7, 180)),
        ("14", "GPIO9", "bidirectional", Position(15.24, -10.16, 180)),
        ("15", "GPIO10", "bidirectional", Position(15.24, -7.62, 180)),
        ("16", "GPIO18", "bidirectional", Position(15.24, -5.08, 180)),
        ("17", "GPIO19", "bidirectional", Position(15.24, -2.54, 180)),
        ("18", "GPIO20", "bidirectional", Position(15.24, 0, 180)),
        ("19", "GPIO21", "bidirectional", Position(15.24, 2.54, 180)),
    ]
    pin_unit.pins = [
        SymbolPin(electricalType=etype, graphicalStyle="line",
                  position=pos, length=2.54, name=name, number=num)
        for num, name, etype, pos in pin_defs
    ]
    sym.units = [
        Symbol(entryName="ESP32-C3-MINI-1", unitId=0, styleId=1),  # body
        pin_unit,
    ]
    return sym


def _make_usbc_symbol() -> Symbol:
    """USB-C receptacle library symbol (simplified UFP)."""
    sym = Symbol(entryName="USB_C_Receptacle_USB2.0",
                 libraryNickname="Connector_USB")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "J", 0, -10, 0),
        _prop("Value", "USB_C_Receptacle_USB2.0", 0, 10, 1),
        _prop("Footprint", "", 0, 12, 2, hide=True),
        _prop("Datasheet", "~", 0, 14, 3, hide=True),
    ]

    pin_unit = Symbol(entryName="USB_C_Receptacle_USB2.0", unitId=1, styleId=1)
    pin_defs = [
        ("A1", "GND", "passive", Position(-10.16, 7.62, 0)),
        ("A4", "VBUS", "power_in", Position(-10.16, 5.08, 0)),
        ("A5", "CC1", "bidirectional", Position(-10.16, 2.54, 0)),
        ("A6", "D+", "bidirectional", Position(-10.16, 0, 0)),
        ("A7", "D-", "bidirectional", Position(-10.16, -2.54, 0)),
        ("B1", "GND", "passive", Position(-10.16, -5.08, 0)),
        ("B4", "VBUS", "passive", Position(-10.16, -7.62, 0)),
        ("B5", "CC2", "bidirectional", Position(10.16, 7.62, 180)),
        ("B6", "D+", "passive", Position(10.16, 5.08, 180)),
        ("B7", "D-", "passive", Position(10.16, 2.54, 180)),
        ("S1", "SHIELD", "passive", Position(10.16, -2.54, 180)),
    ]
    pin_unit.pins = [
        SymbolPin(electricalType=etype, graphicalStyle="line",
                  position=pos, length=2.54, name=name, number=num)
        for num, name, etype, pos in pin_defs
    ]
    sym.units = [
        Symbol(entryName="USB_C_Receptacle_USB2.0", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_mcp1700_symbol() -> Symbol:
    """MCP1700 LDO (VIN, GND, VOUT) — SOT-23-3."""
    sym = Symbol(entryName="MCP1700-3302E_TT", libraryNickname="Regulator_Linear")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "U", 0, -5, 0),
        _prop("Value", "MCP1700-3302E/TT", 0, 5, 1),
        _prop("Footprint", "Package_TO_SOT_SMD:SOT-23-3", 0, 7, 2, hide=True),
        _prop("Datasheet", "~", 0, 9, 3, hide=True),
    ]

    pin_unit = Symbol(entryName="MCP1700-3302E_TT", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="power_in", graphicalStyle="line",
                  position=Position(-7.62, 2.54, 0), length=2.54,
                  name="VIN", number="1"),
        SymbolPin(electricalType="power_in", graphicalStyle="line",
                  position=Position(0, -5.08, 90), length=2.54,
                  name="GND", number="2"),
        SymbolPin(electricalType="power_out", graphicalStyle="line",
                  position=Position(7.62, 2.54, 180), length=2.54,
                  name="VOUT", number="3"),
    ]
    sym.units = [
        Symbol(entryName="MCP1700-3302E_TT", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_usblc6_symbol() -> Symbol:
    """USBLC6-2SC6 ESD protection — SOT-23-6."""
    sym = Symbol(entryName="USBLC6-2SC6",
                 libraryNickname="Power_Protection")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "U", 0, -7, 0),
        _prop("Value", "USBLC6-2SC6", 0, 7, 1),
        _prop("Footprint", "Package_TO_SOT_SMD:SOT-23-6", 0, 9, 2, hide=True),
        _prop("Datasheet", "~", 0, 11, 3, hide=True),
    ]

    pin_unit = Symbol(entryName="USBLC6-2SC6", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-7.62, 2.54, 0), length=2.54,
                  name="I/O1", number="1"),
        SymbolPin(electricalType="power_in", graphicalStyle="line",
                  position=Position(0, 5.08, 270), length=2.54,
                  name="GND", number="2"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-7.62, -2.54, 0), length=2.54,
                  name="I/O2", number="3"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(7.62, -2.54, 180), length=2.54,
                  name="O2", number="4"),
        SymbolPin(electricalType="power_in", graphicalStyle="line",
                  position=Position(0, -5.08, 90), length=2.54,
                  name="VBUS", number="5"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(7.62, 2.54, 180), length=2.54,
                  name="O1", number="6"),
    ]
    sym.units = [
        Symbol(entryName="USBLC6-2SC6", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_power_symbol(entry: str, pin_type: str = "power_in") -> Symbol:
    """Create a power-flag symbol (GND, +3V3, etc.)."""
    sym = Symbol(entryName=entry, libraryNickname="power")
    sym.isPower = True
    sym.inBom = False
    sym.onBoard = True
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", "#PWR", 0, -2, 0, hide=True),
        _prop("Value", entry, 0, 2, 1),
        _prop("Footprint", "", 0, 4, 2, hide=True),
        _prop("Datasheet", "~", 0, 6, 3, hide=True),
    ]
    pin_unit = Symbol(entryName=entry, unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType=pin_type, graphicalStyle="line",
                  position=Position(0, 0, 0), length=0,
                  name=entry, number="1"),
    ]
    sym.units = [
        Symbol(entryName=entry, unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_diode_symbol() -> Symbol:
    """Schottky diode symbol."""
    sym = Symbol(entryName="D_Schottky", libraryNickname="Device")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", "D", 0, -2, 0),
        _prop("Value", "D_Schottky", 0, 2, 1),
        _prop("Footprint", "", 0, 4, 2, hide=True),
        _prop("Datasheet", "~", 0, 6, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="D_Schottky", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-2.54, 0, 0), length=2.54,
                  name="K", number="1"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(2.54, 0, 180), length=2.54,
                  name="A", number="2"),
    ]
    sym.units = [
        Symbol(entryName="D_Schottky", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_switch_symbol() -> Symbol:
    """Tactile push-button switch."""
    sym = Symbol(entryName="SW_Push", libraryNickname="Switch")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", "SW", 0, -3, 0),
        _prop("Value", "SW_Push", 0, 3, 1),
        _prop("Footprint", "", 0, 5, 2, hide=True),
        _prop("Datasheet", "~", 0, 7, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="SW_Push", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(-2.54, 0, 0), length=2.54,
                  name="1", number="1"),
        SymbolPin(electricalType="passive", graphicalStyle="line",
                  position=Position(2.54, 0, 180), length=2.54,
                  name="2", number="2"),
    ]
    sym.units = [
        Symbol(entryName="SW_Push", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def _make_pwr_flag_symbol() -> Symbol:
    """PWR_FLAG symbol — declares a net as power-driven for ERC."""
    sym = Symbol(entryName="PWR_FLAG", libraryNickname="power")
    sym.isPower = False
    sym.inBom = False
    sym.onBoard = False
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", "#FLG", 0, -2, 0, hide=True),
        _prop("Value", "PWR_FLAG", 0, 2, 1),
        _prop("Footprint", "", 0, 4, 2, hide=True),
        _prop("Datasheet", "~", 0, 6, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="PWR_FLAG", unitId=1, styleId=1)
    pin_unit.pins = [
        SymbolPin(electricalType="power_out", graphicalStyle="line",
                  position=Position(0, 0, 0), length=0,
                  name="pwr", number="1"),
    ]
    sym.units = [
        Symbol(entryName="PWR_FLAG", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


# ---------------------------------------------------------------------------
# Base template function
# ---------------------------------------------------------------------------

def template_base(
    config: BoardConfig,
    origin: tuple[float, float],
    ref_alloc: RefAllocator,
    uuid_gen: UuidGenerator,
) -> TemplateResult:
    """Generate the base schematic block.

    Contains: ESP32-C3 (U1), MCP1700 LDO (U2), USBLC6-2SC6 (U3),
    USB-C (J4), Schottky diodes (D1, D2), reset circuit (R6, C1, SW2),
    BOOT button (R5, SW1), strapping pull-ups (R3, R4), VDD bypass
    (C6), LDO decoupling (C3, C4, C5), USB series resistors (R1, R2),
    USB CC resistors (R13, R14).
    """
    ox, oy = origin
    result = TemplateResult()
    block = "base"
    overrides = config.bom_overrides

    # --- Library symbols ---
    result.lib_symbols.extend([
        _make_esp32c3_symbol(),
        _make_usbc_symbol(),
        _make_mcp1700_symbol(),
        _make_usblc6_symbol(),
        _make_diode_symbol(),
        _make_switch_symbol(),
        make_passive_symbol("Device", "R"),
        make_passive_symbol("Device", "C"),
        _make_power_symbol("GND"),
        _make_power_symbol("+3V3", "power_in"),
        _make_power_symbol("+5V", "power_in"),
        _make_pwr_flag_symbol(),
    ])

    # ---------------------------------------------------------------
    # U1 — ESP32-C3-MINI-1
    # ---------------------------------------------------------------
    u1_x, u1_y = ox + 101.6, oy + 60.96  # 40*G, 24*G
    u1_ref = "U1"
    u1_ov = overrides.get(u1_ref, {})
    u1 = make_component(
        "sonde", "ESP32-C3-MINI-1", u1_ref, "ESP32-C3-MINI-1-N4",
        "RF_Module:ESP32-C3-MINI-1",
        u1_x, u1_y, 0, uuid_gen, f"{block}/{u1_ref}",
        [str(i) for i in range(1, 20)],
        lcsc=u1_ov.get("lcsc", "C2838502"),
        mpn=u1_ov.get("mpn", "ESP32-C3-MINI-1-N4"),
    )
    result.instances.append(u1)

    # ---------------------------------------------------------------
    # U2 — MCP1700-3302E/TT LDO
    # ---------------------------------------------------------------
    u2_x, u2_y = ox + 45.72, oy + 30.48  # 18*G, 12*G
    u2_ref = "U2"
    u2_ov = overrides.get(u2_ref, {})
    u2 = make_component(
        "Regulator_Linear", "MCP1700-3302E_TT", u2_ref, "MCP1700-3302E/TT",
        "Package_TO_SOT_SMD:SOT-23-3",
        u2_x, u2_y, 0, uuid_gen, f"{block}/{u2_ref}",
        ["1", "2", "3"],
        lcsc=u2_ov.get("lcsc", "C54447"),
        mpn=u2_ov.get("mpn", "MCP1700-3302E/TT"),
    )
    result.instances.append(u2)

    # ---------------------------------------------------------------
    # U3 — USBLC6-2SC6 ESD
    # ---------------------------------------------------------------
    u3_x, u3_y = ox + 45.72, oy + 66.04  # 18*G, 26*G
    u3_ref = "U3"
    u3_ov = overrides.get(u3_ref, {})
    u3 = make_component(
        "Power_Protection", "USBLC6-2SC6", u3_ref, "USBLC6-2SC6",
        "Package_TO_SOT_SMD:SOT-23-6",
        u3_x, u3_y, 0, uuid_gen, f"{block}/{u3_ref}",
        ["1", "2", "3", "4", "5", "6"],
        lcsc=u3_ov.get("lcsc", "C7519"),
        mpn=u3_ov.get("mpn", "USBLC6-2SC6"),
    )
    result.instances.append(u3)

    # ---------------------------------------------------------------
    # J4 — USB-C connector
    # ---------------------------------------------------------------
    j4_x, j4_y = ox + 10.16, oy + 66.04  # 4*G, 26*G
    j4_ref = "J4"
    j4_ov = overrides.get(j4_ref, {})
    j4 = make_component(
        "Connector_USB", "USB_C_Receptacle_USB2.0", j4_ref, "USB-C",
        "Connector_USB:USB_C_Receptacle_USB2.0",
        j4_x, j4_y, 0, uuid_gen, f"{block}/{j4_ref}",
        ["A1", "A4", "A5", "A6", "A7", "B1", "B4", "B5", "B6", "B7", "S1"],
        lcsc=j4_ov.get("lcsc", "C2765186"),
        mpn=j4_ov.get("mpn", "TYPE-C-16PIN-2MD-073"),
    )
    result.instances.append(j4)

    # ---------------------------------------------------------------
    # D1, D2 — Schottky diodes (power OR-ing)
    # ---------------------------------------------------------------
    d1_ref = "D1"
    d1_x, d1_y = ox + 30.48, oy + 20.32  # 12*G, 8*G
    d1 = make_component(
        "Device", "D_Schottky", d1_ref, "SS14",
        "Diode_SMD:D_SMA",
        d1_x, d1_y, 0, uuid_gen, f"{block}/{d1_ref}",
        ["1", "2"], lcsc="C2480", mpn="SS14",
    )
    result.instances.append(d1)

    d2_ref = "D2"
    d2_x, d2_y = ox + 30.48, oy + 10.16  # 12*G, 4*G
    d2 = make_component(
        "Device", "D_Schottky", d2_ref, "SS14",
        "Diode_SMD:D_SMA",
        d2_x, d2_y, 0, uuid_gen, f"{block}/{d2_ref}",
        ["1", "2"], lcsc="C2480", mpn="SS14",
    )
    result.instances.append(d2)

    # ---------------------------------------------------------------
    # R1, R2 — USB series resistors (22 Ω)
    # ---------------------------------------------------------------
    r1_ref = "R1"
    r1_x, r1_y = ox + 66.04, oy + 63.5  # 26*G, 25*G
    r1 = make_component(
        "Device", "R", r1_ref, "22Ω",
        "Resistor_SMD:R_0402_1005Metric",
        r1_x, r1_y, 0, uuid_gen, f"{block}/{r1_ref}",
        ["1", "2"], lcsc="C25092",
    )
    result.instances.append(r1)

    r2_ref = "R2"
    r2_x, r2_y = ox + 66.04, oy + 68.58  # 26*G, 27*G
    r2 = make_component(
        "Device", "R", r2_ref, "22Ω",
        "Resistor_SMD:R_0402_1005Metric",
        r2_x, r2_y, 0, uuid_gen, f"{block}/{r2_ref}",
        ["1", "2"], lcsc="C25092",
    )
    result.instances.append(r2)

    # ---------------------------------------------------------------
    # R3, R4 — Strapping pull-ups (10 kΩ, GPIO2 & GPIO8)
    # ---------------------------------------------------------------
    r3_ref = "R3"
    r3_x, r3_y = ox + 86.36, oy + 45.72  # 34*G, 18*G
    r3 = make_component(
        "Device", "R", r3_ref, "10kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r3_x, r3_y, 0, uuid_gen, f"{block}/{r3_ref}",
        ["1", "2"], lcsc="C25744",
    )
    result.instances.append(r3)

    r4_ref = "R4"
    r4_x, r4_y = ox + 121.92, oy + 45.72  # 48*G, 18*G
    r4 = make_component(
        "Device", "R", r4_ref, "10kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r4_x, r4_y, 0, uuid_gen, f"{block}/{r4_ref}",
        ["1", "2"], lcsc="C25744",
    )
    result.instances.append(r4)

    # ---------------------------------------------------------------
    # R5 — GPIO9 strap pull-up (10 kΩ)
    # ---------------------------------------------------------------
    r5_ref = "R5"
    r5_x, r5_y = ox + 134.62, oy + 45.72  # 53*G, 18*G
    r5 = make_component(
        "Device", "R", r5_ref, "10kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r5_x, r5_y, 0, uuid_gen, f"{block}/{r5_ref}",
        ["1", "2"], lcsc="C25744",
    )
    result.instances.append(r5)

    # ---------------------------------------------------------------
    # R6 — EN pull-up (10 kΩ)
    # ---------------------------------------------------------------
    r6_ref = "R6"
    r6_x, r6_y = ox + 76.2, oy + 45.72  # 30*G, 18*G
    r6 = make_component(
        "Device", "R", r6_ref, "10kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r6_x, r6_y, 0, uuid_gen, f"{block}/{r6_ref}",
        ["1", "2"], lcsc="C25744",
    )
    result.instances.append(r6)

    # ---------------------------------------------------------------
    # R13, R14 — USB CC resistors (5.1 kΩ)
    # ---------------------------------------------------------------
    r13_ref = "R13"
    r13_x, r13_y = ox + 15.24, oy + 81.28  # 6*G, 32*G
    r13 = make_component(
        "Device", "R", r13_ref, "5.1kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r13_x, r13_y, 0, uuid_gen, f"{block}/R_CC1",
        ["1", "2"], lcsc="C25905",
    )
    result.instances.append(r13)

    r14_ref = "R14"
    r14_x, r14_y = ox + 25.4, oy + 81.28  # 10*G, 32*G
    r14 = make_component(
        "Device", "R", r14_ref, "5.1kΩ",
        "Resistor_SMD:R_0402_1005Metric",
        r14_x, r14_y, 0, uuid_gen, f"{block}/R_CC2",
        ["1", "2"], lcsc="C25905",
    )
    result.instances.append(r14)

    # ---------------------------------------------------------------
    # SW1 — BOOT button
    # ---------------------------------------------------------------
    sw1_ref = "SW1"
    sw1_x, sw1_y = ox + 139.7, oy + 76.2  # 55*G, 30*G
    sw1 = make_component(
        "Switch", "SW_Push", sw1_ref, "BOOT",
        "Button_Switch_SMD:SW_SPST_TL3342",
        sw1_x, sw1_y, 0, uuid_gen, f"{block}/{sw1_ref}",
        ["1", "2"], lcsc="C318884",
    )
    result.instances.append(sw1)

    # ---------------------------------------------------------------
    # SW2 — RESET button
    # ---------------------------------------------------------------
    sw2_ref = "SW2"
    sw2_x, sw2_y = ox + 76.2, oy + 81.28  # 30*G, 32*G
    sw2 = make_component(
        "Switch", "SW_Push", sw2_ref, "RESET",
        "Button_Switch_SMD:SW_SPST_TL3342",
        sw2_x, sw2_y, 0, uuid_gen, f"{block}/{sw2_ref}",
        ["1", "2"], lcsc="C318884",
    )
    result.instances.append(sw2)

    # ---------------------------------------------------------------
    # C1 — EN debounce (100 nF)
    # ---------------------------------------------------------------
    c1_ref = "C1"
    c1_x, c1_y = ox + 81.28, oy + 81.28  # 32*G, 32*G
    c1 = make_component(
        "Device", "C", c1_ref, "100nF",
        "Capacitor_SMD:C_0402_1005Metric",
        c1_x, c1_y, 0, uuid_gen, f"{block}/{c1_ref}",
        ["1", "2"], lcsc="C1525",
    )
    result.instances.append(c1)

    # ---------------------------------------------------------------
    # C3 — LDO input cap (1 µF)
    # ---------------------------------------------------------------
    c3_ref = "C3"
    c3_x, c3_y = ox + 38.1, oy + 22.86  # 15*G, 9*G
    c3 = make_component(
        "Device", "C", c3_ref, "1µF",
        "Capacitor_SMD:C_0402_1005Metric",
        c3_x, c3_y, 0, uuid_gen, f"{block}/{c3_ref}",
        ["1", "2"], lcsc="C52923",
    )
    result.instances.append(c3)

    # ---------------------------------------------------------------
    # C4 — LDO output cap (1 µF)
    # ---------------------------------------------------------------
    c4_ref = "C4"
    c4_x, c4_y = ox + 55.88, oy + 22.86  # 22*G, 9*G
    c4 = make_component(
        "Device", "C", c4_ref, "1µF",
        "Capacitor_SMD:C_0402_1005Metric",
        c4_x, c4_y, 0, uuid_gen, f"{block}/{c4_ref}",
        ["1", "2"], lcsc="C52923",
    )
    result.instances.append(c4)

    # ---------------------------------------------------------------
    # C5 — LDO output bulk cap (10 µF)
    # ---------------------------------------------------------------
    c5_ref = "C5"
    c5_x, c5_y = ox + 60.96, oy + 22.86  # 24*G, 9*G
    c5 = make_component(
        "Device", "C", c5_ref, "10µF",
        "Capacitor_SMD:C_0805_2012Metric",
        c5_x, c5_y, 0, uuid_gen, f"{block}/{c5_ref}",
        ["1", "2"], lcsc="C15850",
    )
    result.instances.append(c5)

    # ---------------------------------------------------------------
    # C6 — ESP32 VDD bypass (100 nF)
    # ---------------------------------------------------------------
    c6_ref = "C6"
    c6_x, c6_y = ox + 91.44, oy + 43.18  # 36*G, 17*G
    c6 = make_component(
        "Device", "C", c6_ref, "100nF",
        "Capacitor_SMD:C_0402_1005Metric",
        c6_x, c6_y, 0, uuid_gen, f"{block}/{c6_ref}",
        ["1", "2"], lcsc="C1525",
    )
    result.instances.append(c6)

    # ---------------------------------------------------------------
    # PWR_FLAG — drive power-input pins for ERC compliance
    # ---------------------------------------------------------------
    ref_alloc.set_counter("#FLG", 1)
    for flg_net, flg_x, flg_y in [
        ("GND", u1_x - 15.24, u1_y - 15.24),
        ("VUSB", j4_x - 10.16, j4_y - 5.08),
        ("VIN", u2_x - 7.62, u2_y - 2.54),
    ]:
        flg_ref = ref_alloc.next("#FLG")
        flg = make_component(
            "power", "PWR_FLAG", flg_ref, "PWR_FLAG", "",
            flg_x, flg_y, 0, uuid_gen, f"{block}/flg_{flg_net}",
            ["1"],
        )
        flg.inBom = False
        flg.onBoard = False
        result.instances.append(flg)

    # ---------------------------------------------------------------
    # Wires and labels — connect everything via named nets.
    # Labels are placed at exact pin endpoint coordinates so KiCad
    # recognises the connections (pin_endpoint = comp_pos + pin_offset).
    # ---------------------------------------------------------------
    wn = 0  # wire counter

    def _w(x1, y1, x2, y2):
        nonlocal wn
        w = make_wire(x1, y1, x2, y2, uuid_gen, f"{block}/wire/{wn}")
        wn += 1
        result.wires.append(w)

    ln = 0  # label counter

    def _l(text, x, y, rot=0):
        nonlocal ln
        lb = make_label(text, x, y, rot, uuid_gen, f"{block}/label/{ln}")
        ln += 1
        result.labels.append(lb)

    # --- J4 USB-C connector — Y-down: schematic_y = comp_y - pin_dy ---
    _l("GND", j4_x - 10.16, j4_y - 7.62, 0)        # A1 GND
    _l("VUSB", j4_x - 10.16, j4_y - 5.08, 0)       # A4 VBUS
    _l("CC1", j4_x - 10.16, j4_y - 2.54, 0)        # A5 CC1
    _l("USB_DP", j4_x - 10.16, j4_y, 0)             # A6 D+
    _l("USB_DN", j4_x - 10.16, j4_y + 2.54, 0)     # A7 D-
    _l("GND", j4_x - 10.16, j4_y + 5.08, 0)        # B1 GND
    _l("VUSB", j4_x - 10.16, j4_y + 7.62, 0)       # B4 VBUS
    _l("CC2", j4_x + 10.16, j4_y - 7.62, 0)        # B5 CC2
    _l("USB_DP", j4_x + 10.16, j4_y - 5.08, 0)     # B6 D+
    _l("USB_DN", j4_x + 10.16, j4_y - 2.54, 0)     # B7 D-
    _l("GND", j4_x + 10.16, j4_y + 2.54, 0)        # S1 SHIELD

    # --- U3 USBLC6-2SC6 ESD protection ---
    _l("USB_DP", u3_x - 7.62, u3_y - 2.54, 0)      # pin 1 I/O1
    _l("GND", u3_x, u3_y - 5.08, 0)                 # pin 2 GND
    _l("USB_DN", u3_x - 7.62, u3_y + 2.54, 0)      # pin 3 I/O2
    _l("USB_DN", u3_x + 7.62, u3_y + 2.54, 0)      # pin 4 O2
    _l("VUSB", u3_x, u3_y + 5.08, 0)                # pin 5 VBUS
    _l("USB_DP", u3_x + 7.62, u3_y - 2.54, 0)      # pin 6 O1

    # --- R1, R2 USB series resistors (22 Ω, vertical passives) ---
    # Passive pin1 dy=+1.27 → schematic y = comp_y - 1.27
    # Passive pin2 dy=-1.27 → schematic y = comp_y + 1.27
    _l("USB_DP", r1_x, r1_y - 1.27, 0)              # R1 pin 1
    _l("USB_DP", r1_x, r1_y + 1.27, 0)              # R1 pin 2
    _l("USB_DN", r2_x, r2_y - 1.27, 0)              # R2 pin 1
    _l("USB_DN", r2_x, r2_y + 1.27, 0)              # R2 pin 2

    # --- U2 MCP1700 LDO ---
    _l("VIN", u2_x - 7.62, u2_y - 2.54, 0)         # pin 1 VIN
    _l("GND", u2_x, u2_y + 5.08, 0)                 # pin 2 GND
    _l("3V3", u2_x + 7.62, u2_y - 2.54, 0)         # pin 3 VOUT

    # --- D1 Schottky (USB power OR-ing, pin_dy=0) ---
    _l("VUSB", d1_x - 2.54, d1_y, 0)                # pin 1 K
    _l("VIN", d1_x + 2.54, d1_y, 0)                  # pin 2 A

    # --- D2 Schottky (battery power OR-ing, pin_dy=0) ---
    _l("VBAT", d2_x - 2.54, d2_y, 0)                # pin 1 K
    _l("VIN", d2_x + 2.54, d2_y, 0)                  # pin 2 A

    # --- Capacitors (vertical passives: pin1 y-1.27, pin2 y+1.27) ---
    _l("VIN", c3_x, c3_y - 1.27, 0)                 # C3 pin 1
    _l("GND", c3_x, c3_y + 1.27, 0)                 # C3 pin 2
    _l("3V3", c4_x, c4_y - 1.27, 0)                 # C4 pin 1
    _l("GND", c4_x, c4_y + 1.27, 0)                 # C4 pin 2
    _l("3V3", c5_x, c5_y - 1.27, 0)                 # C5 pin 1
    _l("GND", c5_x, c5_y + 1.27, 0)                 # C5 pin 2
    _l("3V3", c6_x, c6_y - 1.27, 0)                 # C6 pin 1
    _l("GND", c6_x, c6_y + 1.27, 0)                 # C6 pin 2

    # --- U1 ESP32-C3-MINI-1 (all 19 pins) ---
    _l("GND", u1_x - 15.24, u1_y - 15.24, 0)       # pin 1 GND
    _l("GND", u1_x - 15.24, u1_y - 12.7, 0)        # pin 2 GND
    _l("3V3", u1_x - 15.24, u1_y - 10.16, 0)       # pin 3 3V3
    _l("EN", u1_x - 15.24, u1_y - 7.62, 0)         # pin 4 EN
    _l("VBAT_SENSE", u1_x - 15.24, u1_y - 5.08, 0) # pin 5 GPIO0
    _l("GPIO1", u1_x - 15.24, u1_y - 2.54, 0)      # pin 6 GPIO1
    _l("GPIO2", u1_x - 15.24, u1_y, 0)              # pin 7 GPIO2
    _l("SENSOR_PWR_EN", u1_x - 15.24, u1_y + 2.54, 0)  # pin 8 GPIO3
    _l("I2C0_SDA", u1_x - 15.24, u1_y + 5.08, 0)   # pin 9 GPIO4
    _l("I2C0_SCL", u1_x - 15.24, u1_y + 7.62, 0)   # pin 10 GPIO5
    _l("GPIO6", u1_x - 15.24, u1_y + 10.16, 0)     # pin 11 GPIO6
    _l("GPIO7", u1_x - 15.24, u1_y + 12.7, 0)      # pin 12 GPIO7
    _l("GPIO8", u1_x + 15.24, u1_y + 12.7, 0)      # pin 13 GPIO8
    _l("BOOT", u1_x + 15.24, u1_y + 10.16, 0)      # pin 14 GPIO9
    _l("GPIO10", u1_x + 15.24, u1_y + 7.62, 0)     # pin 15 GPIO10
    _l("USB_DN", u1_x + 15.24, u1_y + 5.08, 0)     # pin 16 GPIO18
    _l("USB_DP", u1_x + 15.24, u1_y + 2.54, 0)     # pin 17 GPIO19
    _l("GPIO20", u1_x + 15.24, u1_y, 0)             # pin 18 GPIO20
    _l("GPIO21", u1_x + 15.24, u1_y - 2.54, 0)     # pin 19 GPIO21

    # --- R3 GPIO2 strapping pull-up ---
    _l("3V3", r3_x, r3_y - 1.27, 0)                 # pin 1
    _l("GPIO2", r3_x, r3_y + 1.27, 0)               # pin 2

    # --- R4 GPIO8 strapping pull-up ---
    _l("3V3", r4_x, r4_y - 1.27, 0)                 # pin 1
    _l("GPIO8", r4_x, r4_y + 1.27, 0)               # pin 2

    # --- R5 GPIO9/BOOT pull-up ---
    _l("3V3", r5_x, r5_y - 1.27, 0)                 # pin 1
    _l("BOOT", r5_x, r5_y + 1.27, 0)                # pin 2

    # --- R6 EN pull-up ---
    _l("3V3", r6_x, r6_y - 1.27, 0)                 # pin 1
    _l("EN", r6_x, r6_y + 1.27, 0)                  # pin 2

    # --- C1 EN debounce ---
    _l("EN", c1_x, c1_y - 1.27, 0)                  # pin 1
    _l("GND", c1_x, c1_y + 1.27, 0)                 # pin 2

    # --- SW2 RESET button (pin_dy=0) ---
    _l("EN", sw2_x - 2.54, sw2_y, 0)                # pin 1
    _l("GND", sw2_x + 2.54, sw2_y, 0)               # pin 2

    # --- SW1 BOOT button (pin_dy=0) ---
    _l("BOOT", sw1_x - 2.54, sw1_y, 0)              # pin 1
    _l("GND", sw1_x + 2.54, sw1_y, 0)               # pin 2

    # --- R13 CC1 pull-down ---
    _l("CC1", r13_x, r13_y - 1.27, 0)               # pin 1
    _l("GND", r13_x, r13_y + 1.27, 0)               # pin 2

    # --- R14 CC2 pull-down ---
    _l("CC2", r14_x, r14_y - 1.27, 0)               # pin 1
    _l("GND", r14_x, r14_y + 1.27, 0)               # pin 2

    # Interface nets exposed for other templates
    result.interface_nets = {
        "3V3": (u2_x + 7.62, u2_y - 2.54),
        "GND": (u1_x - 15.24, u1_y - 15.24),
        "I2C0_SDA": (u1_x - 15.24, u1_y + 5.08),
        "I2C0_SCL": (u1_x - 15.24, u1_y + 7.62),
        "SENSOR_PWR_EN": (u1_x - 15.24, u1_y + 2.54),
        "VBAT_SENSE": (u1_x - 15.24, u1_y - 5.08),
        "EN": (u1_x - 15.24, u1_y - 7.62),
        "BOOT": (u1_x + 15.24, u1_y + 10.16),
        "VUSB": (j4_x - 10.16, j4_y - 5.08),
        "VBAT": (d2_x - 2.54, d2_y),
        "VIN": (d1_x + 2.54, d1_y),
    }

    return result
