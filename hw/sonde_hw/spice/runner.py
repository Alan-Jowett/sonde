# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""ngspice batch-mode runner — invokes ngspice and parses results."""

from __future__ import annotations

import re
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path


class NgspiceNotFoundError(Exception):
    """Raised when ngspice is not installed or not on PATH."""


@dataclass
class MeasureResult:
    """A single measurement extracted from ngspice output."""
    name: str
    value: float


def _find_ngspice() -> str:
    """Locate the ngspice binary, raising a helpful error if missing."""
    path = shutil.which("ngspice")
    if path is not None:
        return path

    # Check common Windows install locations
    for candidate in [
        r"C:\Program Files\ngspice\bin\ngspice.exe",
        r"C:\Program Files (x86)\ngspice\bin\ngspice.exe",
    ]:
        if Path(candidate).exists():
            return candidate

    raise NgspiceNotFoundError(
        "ngspice is not installed or not on PATH.\n"
        "Install it with one of:\n"
        "  Windows : choco install ngspice   (or download from ngspice.sourceforge.io)\n"
        "  macOS   : brew install ngspice\n"
        "  Linux   : apt install ngspice\n"
    )


_MEAS_RE = re.compile(r"^MEAS\s+(\S+)\s+=\s+(.+)$", re.MULTILINE)


def run_simulation(
    cir_path: Path,
    timeout: int = 30,
    verbose: bool = False,
) -> list[MeasureResult]:
    """Run an ngspice simulation in batch mode.

    Returns a list of :class:`MeasureResult` parsed from the output.
    """
    ngspice = _find_ngspice()

    cmd = [ngspice, "-b", str(cir_path)]
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        raise RuntimeError(
            f"ngspice timed out after {timeout}s on {cir_path.name}"
        )

    output = result.stdout + "\n" + result.stderr

    if verbose:
        print(output)

    if result.returncode != 0:
        raise RuntimeError(
            f"ngspice failed (exit {result.returncode}):\n{output}"
        )

    return _parse_measures(output)


def _parse_measures(output: str) -> list[MeasureResult]:
    """Extract MEAS lines from ngspice output."""
    results: list[MeasureResult] = []
    for match in _MEAS_RE.finditer(output):
        name = match.group(1)
        val_str = match.group(2).strip()
        try:
            value = float(val_str)
        except ValueError:
            continue
        results.append(MeasureResult(name=name, value=value))
    return results
