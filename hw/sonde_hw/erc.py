# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""ERC runner — wraps kicad-cli sch erc."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path


class ErcError(Exception):
    """Raised when ERC finds violations."""


def run_erc(sch_path: Path, output_dir: Path | None = None) -> Path | None:
    """Run KiCad ERC on the given schematic.

    Returns the path to the ERC report if kicad-cli is available,
    or ``None`` if kicad-cli is not found.

    Raises ``ErcError`` if ERC errors are found.  Library-configuration
    warnings (missing footprint/symbol libraries) are expected for
    standalone schematics without a KiCad project and are reported
    but do not cause failure.
    """
    kicad_cli = shutil.which("kicad-cli")
    if kicad_cli is None:
        print("WARNING: kicad-cli not found on PATH — skipping ERC")
        return None

    if output_dir is None:
        output_dir = sch_path.parent

    report_path = output_dir / "erc-report.json"

    cmd = [
        kicad_cli, "sch", "erc",
        str(sch_path),
        "--exit-code-violations",
        "--severity-error",
        "--output", str(report_path),
        "--format", "json",
    ]

    result = subprocess.run(cmd, capture_output=True, text=True)

    if result.returncode != 0:
        msg = f"ERC found errors (exit code {result.returncode})"
        if result.stdout:
            msg += f"\n{result.stdout}"
        if result.stderr:
            msg += f"\n{result.stderr}"
        raise ErcError(msg)

    # Print summary from stdout (may mention warning count)
    if result.stdout.strip():
        print(f"ERC: {result.stdout.strip()}")

    return report_path
