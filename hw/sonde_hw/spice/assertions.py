# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""Assertion evaluator — compares measured values against test thresholds."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from sonde_hw.spice.runner import MeasureResult


@dataclass
class AssertionResult:
    """Result of evaluating a single assertion."""
    index: int
    description: str
    passed: bool
    measured: float | None
    threshold: float
    operator: str
    unit: str
    margin: str  # human-readable margin info


def evaluate_assertions(
    test: dict[str, Any],
    measurements: list[MeasureResult],
) -> list[AssertionResult]:
    """Evaluate test assertions against measured values.

    Each assertion in the test YAML is matched to a ``meas_<index>``
    measurement from ngspice output.
    """
    results: list[AssertionResult] = []
    assertions = test.get("assertions", [])

    meas_map: dict[str, float] = {m.name: m.value for m in measurements}

    for i, assertion in enumerate(assertions):
        meas_name = f"meas_{i}"
        description = assertion.get("description", f"Assertion {i}")
        operator = assertion.get("operator", "==")
        threshold = float(assertion.get("threshold", 0))
        unit = assertion.get("unit", "")
        tolerance_pct = assertion.get("tolerance_pct", 0)

        measured = meas_map.get(meas_name)

        if measured is None:
            results.append(AssertionResult(
                index=i,
                description=description,
                passed=False,
                measured=None,
                threshold=threshold,
                operator=operator,
                unit=unit,
                margin="no measurement",
            ))
            continue

        passed, margin = _check(measured, operator, threshold, tolerance_pct)

        results.append(AssertionResult(
            index=i,
            description=description,
            passed=passed,
            measured=measured,
            threshold=threshold,
            operator=operator,
            unit=unit,
            margin=margin,
        ))

    return results


def _check(
    measured: float,
    operator: str,
    threshold: float,
    tolerance_pct: float,
) -> tuple[bool, str]:
    """Evaluate a single comparison and return (passed, margin_info)."""
    if operator == "approx":
        if threshold == 0:
            passed = abs(measured) < 1e-12
            margin = f"measured={measured:.6g}"
        else:
            pct_error = abs(measured - threshold) / abs(threshold) * 100
            passed = pct_error <= tolerance_pct
            margin = f"error={pct_error:.2f}% (limit={tolerance_pct}%)"
    elif operator == "<=":
        passed = measured <= threshold
        if threshold != 0:
            margin = f"measured={_eng(measured)}, limit={_eng(threshold)}"
        else:
            margin = f"measured={measured:.6g}"
    elif operator == ">=":
        passed = measured >= threshold
        margin = f"measured={_eng(measured)}, limit={_eng(threshold)}"
    elif operator == "<":
        passed = measured < threshold
        margin = f"measured={_eng(measured)}, limit={_eng(threshold)}"
    elif operator == ">":
        passed = measured > threshold
        margin = f"measured={_eng(measured)}, limit={_eng(threshold)}"
    elif operator == "==":
        passed = abs(measured - threshold) < 1e-12
        margin = f"measured={measured:.6g}, expected={threshold:.6g}"
    else:
        passed = False
        margin = f"unknown operator {operator!r}"

    return passed, margin


def _eng(value: float) -> str:
    """Format a value in engineering notation."""
    abs_val = abs(value)
    if abs_val == 0:
        return "0"
    elif abs_val >= 1:
        return f"{value:.4g}"
    elif abs_val >= 1e-3:
        return f"{value * 1e3:.4g} m"
    elif abs_val >= 1e-6:
        return f"{value * 1e6:.4g} µ"
    elif abs_val >= 1e-9:
        return f"{value * 1e9:.4g} n"
    else:
        return f"{value:.4g}"


def format_results(
    test_id: str,
    test_name: str,
    assertion_results: list[AssertionResult],
) -> str:
    """Format assertion results as a human-readable summary."""
    lines: list[str] = []
    all_passed = all(r.passed for r in assertion_results)
    status = "PASS" if all_passed else "FAIL"
    lines.append(f"  [{status}] {test_id}: {test_name}")

    for r in assertion_results:
        icon = "✓" if r.passed else "✗"
        meas_str = _eng(r.measured) if r.measured is not None else "N/A"
        lines.append(f"    {icon} {r.description}")
        lines.append(f"      measured={meas_str}, {r.margin}")

    return "\n".join(lines)
