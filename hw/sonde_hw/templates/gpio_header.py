# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""GPIO breakout header template: 2x5 pin header."""

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
    make_wire,
)


def _make_header_2x5_symbol() -> Symbol:
    """2x5 pin header symbol."""
    sym = Symbol(entryName="Conn_02x05_Odd_Even",
                 libraryNickname="Connector_Generic")
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.properties = [
        _prop("Reference", "J", 0, -8, 0),
        _prop("Value", "Conn_02x05", 0, 8, 1),
        _prop("Footprint", "", 0, 10, 2, hide=True),
        _prop("Datasheet", "~", 0, 12, 3, hide=True),
    ]
    pin_unit = Symbol(entryName="Conn_02x05_Odd_Even", unitId=1, styleId=1)
    pins = []
    for i in range(5):
        # Odd pins (left side): 1, 3, 5, 7, 9
        odd = 2 * i + 1
        pins.append(SymbolPin(
            electricalType="passive", graphicalStyle="line",
            position=Position(-5.08, (4 - i) * 2.54, 0), length=2.54,
            name=f"Pin_{odd}", number=str(odd),
        ))
        # Even pins (right side): 2, 4, 6, 8, 10
        even = 2 * i + 2
        pins.append(SymbolPin(
            electricalType="passive", graphicalStyle="line",
            position=Position(5.08, (4 - i) * 2.54, 180), length=2.54,
            name=f"Pin_{even}", number=str(even),
        ))
    pin_unit.pins = pins
    sym.units = [
        Symbol(entryName="Conn_02x05_Odd_Even", unitId=0, styleId=1),
        pin_unit,
    ]
    return sym


def template_gpio_header(
    config: BoardConfig,
    origin: tuple[float, float],
    ref_alloc: RefAllocator,
    uuid_gen: UuidGenerator,
) -> TemplateResult:
    """Generate the GPIO breakout header (J6, 2x5)."""
    ox, oy = origin
    block = "gpio_header"
    result = TemplateResult()

    result.lib_symbols.append(_make_header_2x5_symbol())

    j6_ref = "J6"
    j6_x, j6_y = ox, oy
    j6 = make_component(
        "Connector_Generic", "Conn_02x05_Odd_Even", j6_ref, "GPIO_Header",
        "Connector_PinHeader_2.54mm:PinHeader_2x05_P2.54mm_Vertical",
        j6_x, j6_y, 0, uuid_gen, f"{block}/{j6_ref}",
        [str(i) for i in range(1, 11)],
        lcsc="C124378",
    )
    result.instances.append(j6)

    # Labels for each header pin from config
    header_pins = config.gpio_header_pins
    ln = 0

    def _l(text, x, y, rot=0):
        nonlocal ln
        lb = make_label(text, x, y, rot, uuid_gen, f"{block}/label/{ln}")
        ln += 1
        result.labels.append(lb)

    for hp in header_pins:
        pin_num = hp["pin"]
        net = hp["net"]
        # Y-down: schematic_y = comp_y - pin_dy, pin_dy = (4-row)*2.54
        if pin_num % 2 == 1:  # odd = left side (pin offset -5.08)
            row = (pin_num - 1) // 2
            _l(net, j6_x - 5.08, j6_y - (4 - row) * 2.54, 0)
        else:  # even = right side (pin offset +5.08)
            row = (pin_num - 2) // 2
            _l(net, j6_x + 5.08, j6_y - (4 - row) * 2.54, 0)

    result.interface_nets = {
        "3V3": (j6_x - 5.08, j6_y - 10.16),
        "GND": (j6_x + 5.08, j6_y - 10.16),
    }

    return result
