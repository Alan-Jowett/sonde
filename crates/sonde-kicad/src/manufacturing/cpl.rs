// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Pick-and-place (CPL) CSV generation (JLCPCB format).

use crate::error::Error;
use crate::ir::IrBundle;
use crate::pcb::placement;

/// Generate a JLCPCB-compatible pick-and-place CSV from the IR bundle.
pub fn emit_cpl_csv(bundle: &IrBundle) -> Result<String, Error> {
    let pos_map = placement::compute_position_map(bundle)?;

    let mut csv = String::from("Designator,Mid X,Mid Y,Layer,Rotation\n");

    let mut rows: Vec<_> = bundle
        .ir1e
        .components
        .iter()
        .map(|c| {
            let (x, y, rotation) = pos_map.get(&c.ref_des).copied().unwrap_or((0.0, 0.0, 0.0));
            (c.ref_des.as_str(), x, y, rotation)
        })
        .collect();

    rows.sort_by(|a, b| crate::schematic::wiring::cmp_ref_des_pub(a.0, b.0));

    for (ref_des, x, y, rotation) in &rows {
        csv.push_str(&format!(
            "{},{:.4},{:.4},Top,{}\n",
            ref_des, x, y, *rotation as i32
        ));
    }

    Ok(csv)
}
