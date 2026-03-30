# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Netlist extraction — builds a net graph from schematic template results.

The template system places labels at exact pin endpoint positions.  This
module correlates those positions to derive component-pin-net connectivity
and serialises the result as JSON (spec §3.3).
"""

from __future__ import annotations

import json
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from kiutils.items.schitems import LocalLabel, SchematicSymbol
from kiutils.symbol import Symbol


# ---------------------------------------------------------------------------
# Public data structures
# ---------------------------------------------------------------------------

@dataclass
class PinInfo:
    """A single pin on a component."""
    number: str
    name: str
    net: str = ""


@dataclass
class ComponentInfo:
    """A component instance in the netlist."""
    ref: str
    comp_type: str
    value: str
    model: str
    pins: dict[str, PinInfo] = field(default_factory=dict)


@dataclass
class NetInfo:
    """A named net with connected pins."""
    name: str
    pins: list[str] = field(default_factory=list)  # "REF:pin_number"


@dataclass
class Netlist:
    """Complete netlist extracted from schematic data."""
    config_name: str
    components: list[ComponentInfo] = field(default_factory=list)
    nets: dict[str, NetInfo] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Component type / model inference
# ---------------------------------------------------------------------------

_ENTRY_TO_TYPE: dict[str, tuple[str, str]] = {
    "ESP32-C3-MINI-1": ("mcu", "esp32c3"),
    "MCP1700-3302E_TT": ("ldo", "mcp1700"),
    "USBLC6-2SC6": ("esd", "usblc6"),
    "Q_PMOS_GSD": ("pfet", "si2301"),
    "D_Schottky": ("diode", "schottky"),
    "R": ("resistor", ""),
    "C": ("capacitor", ""),
    "SW_Push": ("switch", ""),
    "USB_C_Receptacle_USB2.0": ("connector", ""),
    "Conn_01x02": ("connector", ""),
    "Qwiic_Connector": ("connector", ""),
    "Conn_02x05_Odd_Even": ("connector", ""),
    "SolderJumper_2_Open": ("jumper", ""),
    "PWR_FLAG": ("power_flag", ""),
    "GND": ("power_symbol", ""),
    "+3V3": ("power_symbol", ""),
    "+5V": ("power_symbol", ""),
}


def _infer_type_model(entry_name: str) -> tuple[str, str]:
    """Return (component_type, spice_model) for a library entry name."""
    return _ENTRY_TO_TYPE.get(entry_name, ("unknown", ""))


# ---------------------------------------------------------------------------
# Pin endpoint computation
# ---------------------------------------------------------------------------

def _pin_positions_from_symbol(sym: Symbol) -> dict[str, tuple[float, float]]:
    """Extract pin number → (dx, dy) from a library symbol definition.

    ``dx``/``dy`` are in the symbol's local coordinate system (Y-up).
    """
    pins: dict[str, tuple[float, float]] = {}
    for unit in sym.units:
        for pin in getattr(unit, "pins", []):
            pins[pin.number] = (pin.position.X, pin.position.Y)
    return pins


def _rotate(dx: float, dy: float, rotation: int) -> tuple[float, float]:
    """Rotate a vector by *rotation* degrees (counter-clockwise)."""
    if rotation == 0:
        return dx, dy
    rad = math.radians(rotation)
    cos_r, sin_r = math.cos(rad), math.sin(rad)
    return (dx * cos_r - dy * sin_r, dx * sin_r + dy * cos_r)


def _pin_endpoint(comp_x: float, comp_y: float, comp_rot: int,
                  pin_dx: float, pin_dy: float) -> tuple[float, float]:
    """Compute schematic-space endpoint for a pin.

    KiCad schematics use Y-down; symbol pin definitions use Y-up.
    """
    rx, ry = _rotate(pin_dx, pin_dy, comp_rot)
    return (round(comp_x + rx, 4), round(comp_y - ry, 4))


_POS_TOL = 0.01  # floating-point tolerance for position matching


def _pos_key(x: float, y: float) -> tuple[int, int]:
    """Quantise a position to a ``_POS_TOL``-mm grid for dict lookup."""
    scale = 1.0 / _POS_TOL
    return (round(x * scale), round(y * scale))


# ---------------------------------------------------------------------------
# Netlist builder
# ---------------------------------------------------------------------------

def build_netlist(
    config_name: str,
    lib_symbols: list[Symbol],
    instances: list[SchematicSymbol],
    labels: list[LocalLabel],
) -> Netlist:
    """Build a :class:`Netlist` from schematic template output."""
    netlist = Netlist(config_name=config_name)

    # 1. Index library symbol pin positions by qualified name
    sym_pins: dict[str, dict[str, tuple[float, float]]] = {}
    for sym in lib_symbols:
        key = sym.entryName
        sym_pins[key] = _pin_positions_from_symbol(sym)

    # 1b. Precompute entryName → {pin_number: pin_name}
    pin_names: dict[str, dict[str, str]] = {}
    for sym in lib_symbols:
        names: dict[str, str] = {}
        for unit in sym.units:
            for pin in getattr(unit, "pins", []):
                names[pin.number] = pin.name
        pin_names[sym.entryName] = names

    # 2. Build component list and spatial pin index
    pin_index: dict[tuple[int, int], list[tuple[str, str]]] = {}  # pos → [(ref, pin_num)]

    for inst in instances:
        ref = ""
        value = ""
        for p in inst.properties:
            if p.key == "Reference":
                ref = p.value
            elif p.key == "Value":
                value = p.value

        # Skip power flags / symbols — they don't appear in the netlist
        entry = inst.entryName
        comp_type, model = _infer_type_model(entry)
        if comp_type in ("power_flag", "power_symbol"):
            continue

        comp = ComponentInfo(ref=ref, comp_type=comp_type, value=value, model=model)

        # Get pin definitions from lib symbol
        lib_pins = sym_pins.get(entry, {})
        comp_rot = int(inst.position.angle) if inst.position.angle else 0

        for pin_num, (pdx, pdy) in lib_pins.items():
            pin_name = pin_names.get(entry, {}).get(pin_num, pin_num)
            pi = PinInfo(number=pin_num, name=pin_name)
            comp.pins[pin_num] = pi

            ex, ey = _pin_endpoint(inst.position.X, inst.position.Y,
                                   comp_rot, pdx, pdy)
            pk = _pos_key(ex, ey)
            pin_index.setdefault(pk, []).append((ref, pin_num))

        netlist.components.append(comp)

    # Build ref → ComponentInfo lookup for O(1) access
    comp_by_ref: dict[str, ComponentInfo] = {c.ref: c for c in netlist.components}

    # 3. Map labels to pins via position
    for label in labels:
        net_name = label.text
        lk = _pos_key(label.position.X, label.position.Y)
        matched = pin_index.get(lk, [])
        for ref, pin_num in matched:
            # Set net on the pin
            comp = comp_by_ref.get(ref)
            if comp and pin_num in comp.pins:
                comp.pins[pin_num].net = net_name

            # Add to net
            if net_name not in netlist.nets:
                netlist.nets[net_name] = NetInfo(name=net_name)
            pin_id = f"{ref}:{pin_num}"
            if pin_id not in netlist.nets[net_name].pins:
                netlist.nets[net_name].pins.append(pin_id)

    return netlist


# ---------------------------------------------------------------------------
# JSON serialisation
# ---------------------------------------------------------------------------

def netlist_to_dict(netlist: Netlist) -> dict[str, Any]:
    """Convert a Netlist to a JSON-serialisable dict (spec §3.3)."""
    components = []
    for comp in sorted(netlist.components, key=lambda c: _ref_sort_key(c.ref)):
        pins_dict: dict[str, dict[str, str]] = {}
        for pn, pi in sorted(comp.pins.items(), key=lambda x: _pin_sort_key(x[0])):
            pins_dict[pn] = {"name": pi.name, "net": pi.net}
        components.append({
            "ref": comp.ref,
            "type": comp.comp_type,
            "value": comp.value,
            "model": comp.model,
            "pins": pins_dict,
        })

    nets = []
    for net in sorted(netlist.nets.values(), key=lambda n: n.name):
        nets.append({
            "name": net.name,
            "pins": sorted(net.pins),
        })

    return {
        "config": netlist.config_name,
        "components": components,
        "nets": nets,
    }


def export_netlist_json(netlist: Netlist, output_path: Path) -> Path:
    """Write netlist to a JSON file."""
    data = netlist_to_dict(netlist)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(data, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return output_path


def _pin_sort_key(pin: str) -> tuple[bool, int, str]:
    """Sort pin keys numerically when possible, falling back to string."""
    try:
        return (False, int(pin), pin)
    except ValueError:
        return (True, 0, pin)


def _ref_sort_key(ref: str) -> tuple[str, int]:
    prefix = ref.rstrip("0123456789")
    num_str = ref[len(prefix):]
    return (prefix, int(num_str) if num_str else 0)
