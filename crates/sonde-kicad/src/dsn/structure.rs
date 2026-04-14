// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! DSN structure section builder — layers, boundaries, keep-outs, rules.

use crate::ir::ir3::Ir3;

/// Write the DSN `(structure ...)` section.
pub fn write_structure(dsn: &mut String, ir3: &Ir3, ox: f64, oy: f64) {
    let board = &ir3.board;
    dsn.push_str("  (structure\n");

    // Layers
    dsn.push_str("    (layer F.Cu (type signal) (property (index 0)))\n");
    dsn.push_str("    (layer B.Cu (type signal) (property (index 1)))\n");
    if board.layers >= 4 {
        dsn.push_str("    (layer In1.Cu (type signal) (property (index 2)))\n");
        dsn.push_str("    (layer In2.Cu (type signal) (property (index 3)))\n");
    }

    // Board boundary (DSN units with page offset, Y negated for DSN)
    // DSN resolution um 10 → 1 unit = 0.1µm → mm × 10000
    let x1 = (ox * 10000.0) as i64;
    let y1 = -(((oy + board.height_mm) * 10000.0) as i64);
    let x2 = x1 + (board.width_mm * 10000.0) as i64;
    let y2 = -((oy * 10000.0) as i64);
    dsn.push_str("    (boundary\n");
    dsn.push_str(&format!(
        "      (path signal 0 {x1} {y1} {x2} {y1} {x2} {y2} {x1} {y2} {x1} {y1})\n"
    ));
    dsn.push_str("    )\n");

    // Keep-outs
    if let Some(keepouts) = &ir3.keepout_zones {
        for kz in keepouts {
            let kx1 = ((kz.boundary.x_mm + ox) * 10000.0) as i64;
            let ky1 = -(((kz.boundary.y_mm + kz.boundary.height_mm + oy) * 10000.0) as i64);
            let kx2 = kx1 + (kz.boundary.width_mm * 10000.0) as i64;
            let ky2 = -(((kz.boundary.y_mm + oy) * 10000.0) as i64);
            dsn.push_str(&format!("    (keepout \"{}\"\n", kz.name));
            dsn.push_str(&format!(
                "      (polygon signal 0 {kx1} {ky1} {kx2} {ky1} {kx2} {ky2} {kx1} {ky2})\n"
            ));
            dsn.push_str("    )\n");
        }
    }

    // Via
    dsn.push_str("    (via \"Via[0-1]_600:300_um\")\n");

    // Default rules
    let (width, clearance) = ir3
        .routing_constraints
        .as_ref()
        .map(|rc| {
            let w = rc
                .signal_traces
                .as_ref()
                .and_then(|st| st.first())
                .map(|s| (s.width_mm * 10000.0) as i64)
                .unwrap_or(250);
            let c = 200i64; // default clearance
            (w, c)
        })
        .unwrap_or((250, 200));

    dsn.push_str(&format!(
        "    (rule (width {width}) (clearance {clearance}))\n"
    ));
    dsn.push_str("  )\n");
}
