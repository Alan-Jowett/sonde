// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! BOM CSV generation (JLCPCB format).

use crate::ir::IrBundle;
use crate::Error;

/// Generate a JLCPCB-compatible BOM CSV from the IR bundle.
pub fn emit_bom_csv(bundle: &IrBundle) -> Result<String, Error> {
    let mut csv = String::from(
        "Designator,Value,Footprint,Manufacturer,Part Number,LCSC Part Number,Quantity\n",
    );

    let mut rows: Vec<_> = bundle
        .ir1e
        .components
        .iter()
        .map(|comp| {
            let netlist_entry = bundle
                .ir2
                .netlist
                .iter()
                .find(|e| e.ref_des == comp.ref_des);

            let value = netlist_entry
                .and_then(|e| e.value.as_deref())
                .unwrap_or("~");

            let (manufacturer, part_number, lcsc_pn) = bundle
                .ir1
                .as_ref()
                .and_then(|ir1| ir1.components.iter().find(|c| c.ref_des == comp.ref_des))
                .map(|c| {
                    (
                        c.manufacturer.as_deref().unwrap_or(""),
                        c.part_number.as_deref().unwrap_or(""),
                        c.sourcing
                            .as_ref()
                            .and_then(|s| s.lcsc_pn.as_deref())
                            .unwrap_or(""),
                    )
                })
                .unwrap_or(("", "", ""));

            (
                comp.ref_des.clone(),
                value.to_string(),
                comp.kicad_footprint.clone(),
                manufacturer.to_string(),
                part_number.to_string(),
                lcsc_pn.to_string(),
            )
        })
        .collect();

    rows.sort_by(|a, b| crate::schematic::wiring::cmp_ref_des_pub(&a.0, &b.0));

    for (ref_des, value, footprint, manufacturer, part_number, lcsc_pn) in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},1\n",
            csv_escape(ref_des),
            csv_escape(value),
            csv_escape(footprint),
            csv_escape(manufacturer),
            csv_escape(part_number),
            csv_escape(lcsc_pn),
        ));
    }

    Ok(csv)
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
