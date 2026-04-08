// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Pick-and-place (CPL) CSV generation (JLCPCB format).

use crate::error::Error;
use crate::ir::IrBundle;

/// Generate a JLCPCB-compatible pick-and-place CSV from the IR bundle.
pub fn emit_cpl_csv(bundle: &IrBundle) -> Result<String, Error> {
    let ir3 = bundle.ir3.as_ref().ok_or(Error::MissingIrFile("IR-3.yaml".into()))?;
    let board_h = ir3.board.height_mm;

    let mut csv = String::from("Designator,Mid X,Mid Y,Layer,Rotation\n");

    // Build position map
    let mut pos_map: std::collections::HashMap<&str, (f64, f64)> =
        std::collections::HashMap::new();

    for cp in &ir3.connector_placement {
        let ky = board_h - cp.position.y_mm;
        pos_map.insert(&cp.ref_des, (cp.position.x_mm, ky));
    }
    for zone in &ir3.component_zones {
        let spacing = zone.proximity_constraint_mm;
        for (i, ref_des) in zone.components.iter().enumerate() {
            if !pos_map.contains_key(ref_des.as_str()) {
                let col = i % 3;
                let row = i / 3;
                let x = zone.zone.anchor.x_mm + col as f64 * spacing;
                let y = board_h - (zone.zone.anchor.y_mm + row as f64 * spacing);
                pos_map.insert(ref_des, (x, y));
            }
        }
    }

    let mut rows: Vec<_> = bundle
        .ir1e
        .components
        .iter()
        .map(|c| {
            let (x, y) = pos_map.get(c.ref_des.as_str()).copied().unwrap_or((0.0, 0.0));
            (c.ref_des.as_str(), x, y)
        })
        .collect();

    rows.sort_by(|a, b| {
        crate::schematic::wiring::cmp_ref_des_pub(a.0, b.0)
    });

    for (ref_des, x, y) in &rows {
        csv.push_str(&format!(
            "{},{:.4},{:.4},Top,0\n",
            ref_des, x, y
        ));
    }

    Ok(csv)
}
