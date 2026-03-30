# SPDX-License-Identifier: MIT
# Copyright (c) 2026 sonde contributors

"""BOM (Bill of Materials) generator — CSV output."""

from __future__ import annotations

import csv
from pathlib import Path

from sonde_hw.config import BoardConfig


# Component database: ref_pattern → (value, footprint, lcsc, unit_cost)
# This captures the BOM from hw-schematic-design.md §9.
BOM_DATABASE: dict[str, dict[str, str]] = {
    "U1": {"value": "ESP32-C3-MINI-1-N4", "footprint": "RF_Module:ESP32-C3-MINI-1",
            "lcsc": "C2838502", "mpn": "ESP32-C3-MINI-1-N4", "cost": "2.50"},
    "U2": {"value": "MCP1700-3302E/TT", "footprint": "Package_TO_SOT_SMD:SOT-23-3",
            "lcsc": "C54447", "mpn": "MCP1700-3302E/TT", "cost": "0.15"},
    "U3": {"value": "USBLC6-2SC6", "footprint": "Package_TO_SOT_SMD:SOT-23-6",
            "lcsc": "C7519", "mpn": "USBLC6-2SC6", "cost": "0.08"},
    "Q1": {"value": "Si2301", "footprint": "Package_TO_SOT_SMD:SOT-23",
            "lcsc": "C306861", "mpn": "Si2301", "cost": "0.03"},
    "D1": {"value": "SS14", "footprint": "Diode_SMD:D_SMA",
            "lcsc": "C2480", "mpn": "SS14", "cost": "0.02"},
    "D2": {"value": "SS14", "footprint": "Diode_SMD:D_SMA",
            "lcsc": "C2480", "mpn": "SS14", "cost": "0.02"},
    "J1": {"value": "Qwiic", "footprint": "Connector_JST:JST_SH_SM04B-SRSS-TB_1x04-1MP_TopEntry",
            "lcsc": "C145956", "mpn": "SM04B-SRSS-TB", "cost": "0.12"},
    "J2": {"value": "Qwiic", "footprint": "Connector_JST:JST_SH_SM04B-SRSS-TB_1x04-1MP_TopEntry",
            "lcsc": "C145956", "mpn": "SM04B-SRSS-TB", "cost": "0.12"},
    "J3": {"value": "Battery", "footprint": "Connector_JST:JST_PH_S2B-PH-SM4-TB_1x02-1MP_Horizontal",
            "lcsc": "C295747", "mpn": "S2B-PH-SM4-TB", "cost": "0.08"},
    "J4": {"value": "USB-C", "footprint": "Connector_USB:USB_C_Receptacle_USB2.0",
            "lcsc": "C2765186", "mpn": "TYPE-C-16PIN-2MD-073", "cost": "0.10"},
    "J6": {"value": "GPIO_Header", "footprint": "Connector_PinHeader_2.54mm:PinHeader_2x05_P2.54mm_Vertical",
            "lcsc": "C124378", "mpn": "—", "cost": "0.05"},
    "SW1": {"value": "BOOT", "footprint": "Button_Switch_SMD:SW_SPST_TL3342",
             "lcsc": "C318884", "mpn": "—", "cost": "0.02"},
    "SW2": {"value": "RESET", "footprint": "Button_Switch_SMD:SW_SPST_TL3342",
             "lcsc": "C318884", "mpn": "—", "cost": "0.02"},
    "SJ1": {"value": "SJ_Bypass", "footprint": "Jumper:SolderJumper-2_P1.3mm_Open_Pad1.0x1.5mm",
             "lcsc": "—", "mpn": "—", "cost": "0.00"},
    "R1": {"value": "22Ω", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25092", "mpn": "—", "cost": "0.01"},
    "R2": {"value": "22Ω", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25092", "mpn": "—", "cost": "0.01"},
    "R3": {"value": "10kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25744", "mpn": "—", "cost": "0.01"},
    "R4": {"value": "10kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25744", "mpn": "—", "cost": "0.01"},
    "R5": {"value": "10kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25744", "mpn": "—", "cost": "0.01"},
    "R6": {"value": "10kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25744", "mpn": "—", "cost": "0.01"},
    "R7": {"value": "4.7kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25900", "mpn": "—", "cost": "0.01"},
    "R8": {"value": "4.7kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25900", "mpn": "—", "cost": "0.01"},
    "R9": {"value": "10kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
            "lcsc": "C25744", "mpn": "—", "cost": "0.01"},
    "R10": {"value": "10kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
             "lcsc": "C25744", "mpn": "—", "cost": "0.01"},
    "R11": {"value": "10MΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
             "lcsc": "C26083", "mpn": "—", "cost": "0.01"},
    "R12": {"value": "10MΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
             "lcsc": "C26083", "mpn": "—", "cost": "0.01"},
    "R13": {"value": "5.1kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
             "lcsc": "C25905", "mpn": "—", "cost": "0.01"},
    "R14": {"value": "5.1kΩ", "footprint": "Resistor_SMD:R_0402_1005Metric",
             "lcsc": "C25905", "mpn": "—", "cost": "0.01"},
    "C1": {"value": "100nF", "footprint": "Capacitor_SMD:C_0402_1005Metric",
            "lcsc": "C1525", "mpn": "—", "cost": "0.01"},
    "C2": {"value": "100pF", "footprint": "Capacitor_SMD:C_0402_1005Metric",
            "lcsc": "C1546", "mpn": "—", "cost": "0.01"},
    "C3": {"value": "1µF", "footprint": "Capacitor_SMD:C_0402_1005Metric",
            "lcsc": "C52923", "mpn": "—", "cost": "0.01"},
    "C4": {"value": "1µF", "footprint": "Capacitor_SMD:C_0402_1005Metric",
            "lcsc": "C52923", "mpn": "—", "cost": "0.01"},
    "C5": {"value": "10µF", "footprint": "Capacitor_SMD:C_0805_2012Metric",
            "lcsc": "C15850", "mpn": "—", "cost": "0.02"},
    "C6": {"value": "100nF", "footprint": "Capacitor_SMD:C_0402_1005Metric",
            "lcsc": "C1525", "mpn": "—", "cost": "0.01"},
}


def generate_bom(config: BoardConfig, output_dir: Path) -> Path:
    """Generate a BOM CSV file.

    Returns the path to the generated ``bom.csv``.
    """
    output_dir.mkdir(parents=True, exist_ok=True)
    bom_path = output_dir / "bom.csv"

    overrides = config.bom_overrides

    rows: list[dict[str, str]] = []
    for ref in sorted(BOM_DATABASE, key=_ref_sort_key):
        entry = dict(BOM_DATABASE[ref])
        ov = overrides.get(ref, {})
        if "lcsc" in ov:
            entry["lcsc"] = ov["lcsc"]
        if "mpn" in ov:
            entry["mpn"] = ov["mpn"]

        rows.append({
            "Reference": ref,
            "Value": entry["value"],
            "Footprint": entry["footprint"],
            "MPN": entry.get("mpn", ""),
            "LCSC": entry["lcsc"],
            "Quantity": "1",
            "Unit Cost (USD)": entry.get("cost", ""),
        })

    with open(bom_path, "w", newline="", encoding="utf-8") as f:
        f.write(f"# Config hash: {config.config_hash}\n")
        writer = csv.DictWriter(
            f,
            fieldnames=["Reference", "Value", "Footprint", "MPN",
                         "LCSC", "Quantity", "Unit Cost (USD)"],
        )
        writer.writeheader()
        writer.writerows(rows)

    return bom_path


def _ref_sort_key(ref: str) -> tuple[str, int]:
    prefix = ref.rstrip("0123456789")
    num_str = ref[len(prefix):]
    return (prefix, int(num_str) if num_str else 0)
