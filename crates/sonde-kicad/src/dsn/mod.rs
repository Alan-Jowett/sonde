// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Specctra DSN export for Freerouter autorouting.

pub mod structure;

use crate::error::Error;
use crate::ir::IrBundle;

/// Generate a Specctra DSN file from an IR bundle.
pub fn emit_dsn(bundle: &IrBundle) -> Result<String, Error> {
    let ir3 = bundle
        .ir3
        .as_ref()
        .ok_or(Error::MissingIrFile("IR-3.yaml".into()))?;
    let board = &ir3.board;

    // Use same page offset as PCB generation (A4 centered)
    let page_w = 297.0;
    let page_h = 210.0;
    let offset_x = (page_w - board.width_mm) / 2.0;
    let offset_y = (page_h - board.height_mm) / 2.0;

    let mut dsn = String::new();
    dsn.push_str(&format!("(pcb \"{}.dsn\"\n", bundle.project));
    dsn.push_str("  (parser\n");
    dsn.push_str("    (string_quote \")\n");
    dsn.push_str("    (space_in_quoted_tokens on)\n");
    dsn.push_str("    (host_cad \"sonde-kicad\")\n");
    dsn.push_str(&format!(
        "    (host_version \"{}\")\n",
        env!("CARGO_PKG_VERSION")
    ));
    dsn.push_str("  )\n");
    dsn.push_str("  (resolution um 10)\n");
    dsn.push_str("  (unit um)\n");

    // Structure (board boundary with page offset)
    structure::write_structure(&mut dsn, ir3, offset_x, offset_y);

    // Placement (component positions with page offset)
    write_placement(&mut dsn, bundle, board.height_mm, offset_x, offset_y);

    // Library (pad images)
    write_library(&mut dsn, bundle);

    // Network
    write_network(&mut dsn, bundle, ir3);

    // Empty wiring section
    dsn.push_str("  (wiring)\n");
    dsn.push_str(")\n");

    Ok(dsn)
}

fn write_placement(dsn: &mut String, bundle: &IrBundle, board_height: f64, ox: f64, oy: f64) {
    dsn.push_str("  (placement\n");

    // Group components by footprint
    let mut by_footprint: std::collections::BTreeMap<&str, Vec<(&str, f64, f64)>> =
        std::collections::BTreeMap::new();

    let ir3 = bundle.ir3.as_ref().unwrap();

    // Build position map
    let mut pos_map: std::collections::HashMap<String, (f64, f64)> =
        std::collections::HashMap::new();
    for cp in &ir3.connector_placement {
        pos_map.insert(cp.ref_des.clone(), (cp.position.x_mm, cp.position.y_mm));
    }
    for zone in &ir3.component_zones {
        let spacing = zone.proximity_constraint_mm;
        for (i, ref_des) in zone.components.iter().enumerate() {
            if !pos_map.contains_key(ref_des) {
                let col = i % 3;
                let row = i / 3;
                let x = zone.zone.anchor.x_mm + col as f64 * spacing;
                let y = zone.zone.anchor.y_mm + row as f64 * spacing;
                pos_map.insert(ref_des.clone(), (x, y));
            }
        }
    }

    for comp in &bundle.ir1e.components {
        let (x, y) = pos_map.get(&comp.ref_des).copied().unwrap_or((10.0, 10.0));
        // Convert IR-3 coords to KiCad page coords, then to DSN coords.
        // KiCad page: x_kicad = x + ox, y_kicad = board_height - y + oy
        // DSN: same X as KiCad, but Y is negated (DSN Y-up vs KiCad Y-down)
        let kicad_x = x + ox;
        let kicad_y = board_height - y + oy;
        let x_um = mm_to_um(kicad_x);
        let y_um = mm_to_um(-kicad_y); // negate Y for DSN
        by_footprint
            .entry(comp.kicad_footprint.as_str())
            .or_default()
            .push((comp.ref_des.as_str(), x_um, y_um));
    }

    for (footprint, placements) in &by_footprint {
        dsn.push_str(&format!("    (component \"{footprint}\"\n"));
        for (ref_des, x, y) in placements {
            dsn.push_str(&format!("      (place {ref_des} {x:.0} {y:.0} front 0)\n"));
        }
        dsn.push_str("    )\n");
    }
    dsn.push_str("  )\n");
}

fn write_library(dsn: &mut String, bundle: &IrBundle) {
    dsn.push_str("  (library\n");

    let mut seen_fps: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for comp in &bundle.ir1e.components {
        if !seen_fps.insert(comp.kicad_footprint.as_str()) {
            continue;
        }
        // Find pin count from IR-2
        let pin_count = bundle
            .ir2
            .netlist
            .iter()
            .find(|e| e.ref_des == comp.ref_des)
            .map(|e| e.pins.len())
            .unwrap_or(2);

        dsn.push_str(&format!("    (image \"{}\"\n", comp.kicad_footprint));
        for i in 1..=pin_count {
            let pad_y = (i as f64 - 1.0) * 1270.0; // 1.27mm in µm
            dsn.push_str(&format!(
                "      (pin Rect[T]Pad_600x600_um {i} 0 {pad_y:.0})\n"
            ));
        }
        dsn.push_str("    )\n");
    }

    // Via padstack
    dsn.push_str("    (padstack Via[0-1]_600:300_um\n");
    dsn.push_str("      (shape (circle F.Cu 600 0 0))\n");
    dsn.push_str("      (shape (circle B.Cu 600 0 0))\n");
    dsn.push_str("      (attach off)\n");
    dsn.push_str("    )\n");

    // Pad padstack
    dsn.push_str("    (padstack Rect[T]Pad_600x600_um\n");
    dsn.push_str("      (shape (rect F.Cu -300 -300 300 300))\n");
    dsn.push_str("      (attach off)\n");
    dsn.push_str("    )\n");

    dsn.push_str("  )\n");
}

fn write_network(dsn: &mut String, bundle: &IrBundle, ir3: &crate::ir::Ir3) {
    dsn.push_str("  (network\n");

    // Build pin lists per net
    let mut net_pins: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for entry in &bundle.ir2.netlist {
        for pin in &entry.pins {
            if !pin.is_nc() {
                net_pins
                    .entry(pin.net.as_str())
                    .or_default()
                    .push(format!("{}-{}", entry.ref_des, pin.pin));
            }
        }
    }

    for (net, pins) in &net_pins {
        dsn.push_str(&format!("    (net {net}\n"));
        dsn.push_str(&format!("      (pins {})\n", pins.join(" ")));
        dsn.push_str("    )\n");
    }

    // Net classes
    let default_width = 250; // 0.25mm in µm
    let default_clearance = 200;

    // Power net class
    let mut power_nets = Vec::new();
    if let Some(rc) = &ir3.routing_constraints {
        if let Some(pts) = &rc.power_traces {
            for pt in pts {
                if pt.trace_type.as_deref() != Some("copper pour") {
                    power_nets.push(pt.net.as_str());
                }
            }
        }
    }

    if !power_nets.is_empty() {
        let power_width = ir3
            .routing_constraints
            .as_ref()
            .and_then(|rc| rc.power_traces.as_ref())
            .and_then(|pts| pts.first())
            .and_then(|pt| pt.min_width_mm)
            .map(|w| mm_to_um(w) as i64)
            .unwrap_or(500);

        dsn.push_str(&format!("    (class Power {}\n", power_nets.join(" ")));
        dsn.push_str("      (circuit (use_via \"Via[0-1]_600:300_um\"))\n");
        dsn.push_str(&format!(
            "      (rule (width {power_width}) (clearance {default_clearance}))\n"
        ));
        dsn.push_str("    )\n");
    }

    dsn.push_str("    (class Default\n");
    dsn.push_str("      (circuit (use_via \"Via[0-1]_600:300_um\"))\n");
    dsn.push_str(&format!(
        "      (rule (width {default_width}) (clearance {default_clearance}))\n"
    ));
    dsn.push_str("    )\n");

    dsn.push_str("  )\n");
}

fn mm_to_um(mm: f64) -> f64 {
    mm * 1000.0
}
