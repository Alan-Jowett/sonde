# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""YAML config loader and validation."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import jsonschema
import yaml


SCHEMA_PATH = Path(__file__).resolve().parent.parent / "schema.json"


class ConfigError(Exception):
    """Raised when a board configuration is invalid."""


@dataclass
class BoardConfig:
    """Parsed and validated board configuration."""

    raw: dict[str, Any]
    config_hash: str
    name: str

    # Convenience accessors
    @property
    def mcu(self) -> str:
        return self.raw["mcu"]

    @property
    def connectors(self) -> dict[str, Any]:
        return self.raw["connectors"]

    @property
    def power(self) -> dict[str, Any]:
        return self.raw["power"]

    @property
    def board(self) -> dict[str, Any]:
        return self.raw["board"]

    @property
    def pins(self) -> dict[str, Any]:
        return self.raw["pins"]

    @property
    def sensors(self) -> list[dict[str, Any]]:
        return self.raw.get("sensors", [])

    @property
    def strapping(self) -> dict[str, str]:
        return self.raw.get("strapping", {})

    @property
    def gpio_header_pins(self) -> list[dict[str, Any]]:
        return self.raw.get("gpio_header_pins", [])

    @property
    def bom_overrides(self) -> dict[str, dict[str, str]]:
        return self.raw.get("bom_overrides", {})


class _NoDuplicateLoader(yaml.SafeLoader):
    """YAML loader that rejects duplicate keys."""


def _no_dup_constructor(loader, node, deep=False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise ConfigError(
                f"Duplicate key {key!r} at line {key_node.start_mark.line + 1}"
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


_NoDuplicateLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _no_dup_constructor,
)


def load_config(path: str | Path) -> BoardConfig:
    """Load and validate a board configuration file.

    Raises ``ConfigError`` on any validation failure.
    """
    path = Path(path)
    if not path.exists():
        raise ConfigError(f"Config file not found: {path}")

    raw_text = path.read_text(encoding="utf-8")
    config_hash = hashlib.sha256(raw_text.encode("utf-8")).hexdigest()

    try:
        data = yaml.load(raw_text, Loader=_NoDuplicateLoader)
    except yaml.YAMLError as exc:
        raise ConfigError(f"YAML parse error: {exc}") from exc

    # Schema validation
    schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    try:
        jsonschema.validate(data, schema)
    except jsonschema.ValidationError as exc:
        raise ConfigError(f"Schema validation failed: {exc.message}") from exc

    # Semantic checks
    _check_pin_conflicts(data)

    name = path.stem  # e.g. "minimal-qwiic"
    return BoardConfig(raw=data, config_hash=config_hash, name=name)


def _check_pin_conflicts(data: dict[str, Any]) -> None:
    """Ensure no GPIO is used for more than one role."""
    used: dict[int, str] = {}
    pins = data.get("pins", {})
    for role, gpio in pins.items():
        if isinstance(gpio, int):
            if gpio in used:
                raise ConfigError(
                    f"GPIO {gpio} used for both {used[gpio]!r} and {role!r}"
                )
            used[gpio] = role

    power = data.get("power", {})
    for key in ("sensor_gate_gpio", "battery_adc_gpio"):
        gpio = power.get(key)
        if gpio is not None and isinstance(gpio, int):
            if gpio in used:
                # Allow overlap if it matches the pins section
                pins_key = {
                    "sensor_gate_gpio": "sensor_pwr_en",
                    "battery_adc_gpio": "vbat_adc",
                }.get(key)
                if pins_key and pins.get(pins_key) == gpio:
                    continue
                raise ConfigError(
                    f"GPIO {gpio} used for both {used[gpio]!r} and power.{key!r}"
                )
            used[gpio] = f"power.{key}"
