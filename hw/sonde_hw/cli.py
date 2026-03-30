# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""CLI for sonde-hw: validate, build, export."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from sonde_hw import __version__
from sonde_hw.bom import generate_bom
from sonde_hw.config import ConfigError, load_config
from sonde_hw.erc import ErcError, run_erc
from sonde_hw.schematic import generate_schematic


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="sonde-hw",
        description="Generate KiCad schematics from YAML board configurations.",
    )
    parser.add_argument("--version", action="version", version=f"sonde-hw {__version__}")

    sub = parser.add_subparsers(dest="command", required=True)

    # validate
    p_val = sub.add_parser("validate", help="Validate a board config")
    p_val.add_argument("config", type=Path, help="Path to YAML config file")

    # build
    p_build = sub.add_parser("build", help="Generate schematic + BOM")
    p_build.add_argument("config", type=Path, help="Path to YAML config file")
    p_build.add_argument("--output", type=Path, default=None,
                         help="Output directory (default: hw/output/<name>)")
    p_build.add_argument("--skip-erc", action="store_true",
                         help="Skip ERC check after generation")

    # export
    p_exp = sub.add_parser("export", help="Generate BOM only (assumes schematic exists)")
    p_exp.add_argument("config", type=Path, help="Path to YAML config file")

    args = parser.parse_args(argv)

    try:
        if args.command == "validate":
            return _cmd_validate(args)
        elif args.command == "build":
            return _cmd_build(args)
        elif args.command == "export":
            return _cmd_export(args)
    except ConfigError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    except ErcError as exc:
        print(f"ERC FAILED: {exc}", file=sys.stderr)
        return 2

    return 0


def _cmd_validate(args: argparse.Namespace) -> int:
    cfg = load_config(args.config)
    print(f"Config {cfg.name!r} is valid (hash: {cfg.config_hash[:16]}...)")
    return 0


def _cmd_build(args: argparse.Namespace) -> int:
    cfg = load_config(args.config)

    output_dir = args.output
    if output_dir is None:
        output_dir = Path("output") / cfg.name
    output_dir = Path(output_dir)

    # Generate schematic
    sch_path = generate_schematic(cfg, output_dir)
    print(f"Schematic: {sch_path}")

    # Generate BOM
    bom_path = generate_bom(cfg, output_dir)
    print(f"BOM: {bom_path}")

    # Run ERC
    if not args.skip_erc:
        report = run_erc(sch_path, output_dir)
        if report:
            print(f"ERC passed: {report}")
    else:
        print("ERC: skipped")

    print(f"Build complete for {cfg.name!r}")
    return 0


def _cmd_export(args: argparse.Namespace) -> int:
    cfg = load_config(args.config)
    output_dir = Path("output") / cfg.name

    bom_path = generate_bom(cfg, output_dir)
    print(f"BOM: {bom_path}")
    return 0
