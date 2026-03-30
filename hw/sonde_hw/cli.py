# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""CLI for sonde-hw: validate, build, export, simulate."""

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

    # simulate
    p_sim = sub.add_parser("simulate", help="Run SPICE simulations")
    p_sim.add_argument("config", type=Path, help="Path to YAML config file")
    p_sim.add_argument("--test", type=str, default=None,
                       help="Run a single test by ID")
    p_sim.add_argument("--list", action="store_true",
                       help="List available tests and exit")
    p_sim.add_argument("--output", type=Path, default=None,
                       help="Output directory (default: hw/output/<name>)")
    p_sim.add_argument("--verbose", action="store_true",
                       help="Show full ngspice output")
    p_sim.add_argument("--timeout", type=int, default=30,
                       help="Simulation timeout in seconds (default: 30)")

    args = parser.parse_args(argv)

    try:
        if args.command == "validate":
            return _cmd_validate(args)
        elif args.command == "build":
            return _cmd_build(args)
        elif args.command == "export":
            return _cmd_export(args)
        elif args.command == "simulate":
            return _cmd_simulate(args)
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

    # Generate schematic (also exports netlist.json)
    sch_path = generate_schematic(cfg, output_dir)
    print(f"Schematic: {sch_path}")

    # Generate BOM
    bom_path = generate_bom(cfg, output_dir)
    print(f"BOM: {bom_path}")

    # Report netlist
    netlist_path = output_dir / "netlist.json"
    if netlist_path.exists():
        print(f"Netlist: {netlist_path}")

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


def _cmd_simulate(args: argparse.Namespace) -> int:
    from sonde_hw.spice.assertions import evaluate_assertions, format_results
    from sonde_hw.spice.deck import generate_deck, list_tests, load_netlist, load_test
    from sonde_hw.spice.runner import NgspiceNotFoundError, run_simulation

    cfg = load_config(args.config)

    output_dir = args.output
    if output_dir is None:
        output_dir = Path("output") / cfg.name
    output_dir = Path(output_dir)

    # List mode
    if args.list:
        tests = list_tests()
        print("Available simulation tests:")
        for t in tests:
            print(f"  {t['id']:30s} {t.get('name', '')}")
        return 0

    # Check netlist exists
    netlist_path = output_dir / "netlist.json"
    if not netlist_path.exists():
        print(
            f"ERROR: {netlist_path} not found.\n"
            f"Run 'sonde-hw build {args.config}' first.",
            file=sys.stderr,
        )
        return 1

    netlist = load_netlist(netlist_path)

    # Determine which tests to run
    if args.test:
        tests = [load_test(args.test)]
    else:
        tests = list_tests()

    spice_dir = output_dir / "spice"
    spice_dir.mkdir(parents=True, exist_ok=True)

    all_passed = True
    print(f"Running {len(tests)} simulation test(s) for {cfg.name!r}:\n")

    for test in tests:
        test_id = test["id"]
        test_name = test.get("name", test_id)

        # Generate deck
        cir_path = spice_dir / f"{test_id}.cir"
        generate_deck(netlist, test, cir_path)

        # Run simulation
        try:
            measurements = run_simulation(
                cir_path,
                timeout=args.timeout,
                verbose=args.verbose,
            )
        except NgspiceNotFoundError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            return 1
        except RuntimeError as exc:
            print(f"  [FAIL] {test_id}: {test_name}")
            print(f"    Simulation error: {exc}")
            all_passed = False
            continue

        # Evaluate assertions
        assertion_results = evaluate_assertions(test, measurements)
        summary = format_results(test_id, test_name, assertion_results)
        print(summary)

        if not all(r.passed for r in assertion_results):
            all_passed = False

    print()
    if all_passed:
        print("All tests passed.")
        return 0
    else:
        print("Some tests FAILED.")
        return 1
