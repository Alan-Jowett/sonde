# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Schematic template building blocks.

Each template is a pure function that produces components, wires, and
labels for one functional block of the schematic.
"""

from __future__ import annotations

import hashlib
import math
import uuid as _uuid
from dataclasses import dataclass, field
from typing import Any

from kiutils.items.common import Effects, Font, Position, Property, Stroke
from kiutils.items.schitems import Connection, LocalLabel, SchematicSymbol
from kiutils.symbol import Symbol, SymbolPin


# ---------------------------------------------------------------------------
# Shared types
# ---------------------------------------------------------------------------

@dataclass
class TemplateResult:
    """Output of a template function."""

    lib_symbols: list[Symbol] = field(default_factory=list)
    instances: list[SchematicSymbol] = field(default_factory=list)
    wires: list[Connection] = field(default_factory=list)
    labels: list[LocalLabel] = field(default_factory=list)
    interface_nets: dict[str, tuple[float, float]] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Deterministic UUID generator (HW-0900)
# ---------------------------------------------------------------------------

class UuidGenerator:
    """Generate deterministic UUIDs from a config hash + path string."""

    def __init__(self, config_hash: str):
        self._seed = bytes.fromhex(config_hash)

    def next(self, path: str) -> str:
        h = hashlib.sha256(self._seed + path.encode()).digest()
        return str(_uuid.UUID(bytes=h[:16], version=4))


# ---------------------------------------------------------------------------
# Reference designator allocator
# ---------------------------------------------------------------------------

class RefAllocator:
    """Assigns sequential reference designators per prefix."""

    def __init__(self) -> None:
        self._counters: dict[str, int] = {}

    def next(self, prefix: str) -> str:
        idx = self._counters.get(prefix, 1)
        self._counters[prefix] = idx + 1
        return f"{prefix}{idx}"

    def peek(self, prefix: str) -> str:
        idx = self._counters.get(prefix, 1)
        return f"{prefix}{idx}"

    def set_counter(self, prefix: str, value: int) -> None:
        self._counters[prefix] = value


# ---------------------------------------------------------------------------
# Helper functions shared by templates
# ---------------------------------------------------------------------------

def _prop(key: str, value: str, x: float, y: float, pid: int,
          hide: bool = False, size: float = 1.27) -> Property:
    eff = Effects(font=Font(width=size, height=size))
    if hide:
        eff.hide = True
    return Property(key=key, value=value, id=pid, position=Position(x, y, 0),
                    effects=eff)


def _make_pin_uuid(uuid_gen: UuidGenerator, comp_path: str, pin_num: str) -> str:
    return uuid_gen.next(f"{comp_path}:pin:{pin_num}")


def pin_pos(comp_x: float, comp_y: float, comp_rot: int,
            pin_dx: float, pin_dy: float) -> tuple[float, float]:
    """Compute schematic position of a pin endpoint.

    KiCad schematics use Y-down coordinates, while symbol pin
    definitions use Y-up.  The Y component is therefore negated.
    """
    if comp_rot == 0:
        return (comp_x + pin_dx, comp_y - pin_dy)
    rad = math.radians(comp_rot)
    rx = pin_dx * math.cos(rad) - pin_dy * math.sin(rad)
    ry = pin_dx * math.sin(rad) + pin_dy * math.cos(rad)
    return (comp_x + rx, comp_y - ry)


def make_passive_symbol(lib_name: str, entry: str,
                        pin_count: int = 2,
                        pin_type: str = "passive") -> Symbol:
    """Create a simple library symbol definition for passive components."""
    sym = Symbol(entryName=entry, libraryNickname=lib_name)
    sym.inBom = True
    sym.onBoard = True
    sym.pinNames = True
    sym.pinNamesOffset = 0
    sym.properties = [
        _prop("Reference", entry[0], 0, -1.5, 0),
        _prop("Value", entry, 0, 1.5, 1),
        _prop("Footprint", "", 0, 3.0, 2, hide=True),
        _prop("Datasheet", "~", 0, 4.5, 3, hide=True),
    ]

    # Body unit (drawing)
    body = Symbol(entryName=entry, unitId=0, styleId=1)
    sym.units.append(body)

    # Pin unit
    pin_unit = Symbol(entryName=entry, unitId=1, styleId=1)
    if pin_count == 2:
        pin_unit.pins = [
            SymbolPin(electricalType=pin_type, graphicalStyle="line",
                      position=Position(0, 1.27, 270), length=0.508,
                      name="~", number="1"),
            SymbolPin(electricalType=pin_type, graphicalStyle="line",
                      position=Position(0, -1.27, 90), length=0.508,
                      name="~", number="2"),
        ]
    sym.units.append(pin_unit)
    return sym


def make_component(
    lib_nick: str, entry: str,
    ref: str, value: str, footprint: str,
    x: float, y: float, rotation: int,
    uuid_gen: UuidGenerator, comp_path: str,
    pin_numbers: list[str],
    lcsc: str = "", mpn: str = "",
    extra_props: list[Property] | None = None,
) -> SchematicSymbol:
    """Create a placed schematic symbol instance."""
    props = [
        _prop("Reference", ref, x, y - 2.0, 0),
        _prop("Value", value, x, y + 2.0, 1),
        _prop("Footprint", footprint, x, y + 4.0, 2, hide=True),
        _prop("Datasheet", "~", x, y + 6.0, 3, hide=True),
    ]
    pid = 4
    if lcsc:
        props.append(_prop("LCSC", lcsc, x, y + 8.0, pid, hide=True))
        pid += 1
    if mpn:
        props.append(_prop("MPN", mpn, x, y + 10.0, pid, hide=True))
        pid += 1
    if extra_props:
        props.extend(extra_props)

    pins = {}
    for pn in pin_numbers:
        pins[pn] = _make_pin_uuid(uuid_gen, comp_path, pn)

    return SchematicSymbol(
        libraryNickname=lib_nick,
        entryName=entry,
        position=Position(x, y, rotation),
        unit=1,
        inBom=True,
        onBoard=True,
        uuid=uuid_gen.next(comp_path),
        properties=props,
        pins=pins,
    )


def make_wire(x1: float, y1: float, x2: float, y2: float,
              uuid_gen: UuidGenerator, wire_path: str) -> Connection:
    return Connection(
        type="wire",
        points=[Position(x1, y1, 0), Position(x2, y2, 0)],
        stroke=Stroke(width=0),
        uuid=uuid_gen.next(wire_path),
    )


def make_label(text: str, x: float, y: float, rotation: int,
               uuid_gen: UuidGenerator, label_path: str) -> LocalLabel:
    return LocalLabel(
        text=text,
        position=Position(x, y, rotation),
        effects=Effects(font=Font(width=1.27, height=1.27)),
        uuid=uuid_gen.next(label_path),
    )
