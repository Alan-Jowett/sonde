// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Schematic layout algorithm — places functional groups in a column-based layout.

use crate::ir::IrBundle;
use crate::schematic::symbols::SymbolRegistry;

/// Grid spacing in mm (KiCad 1.27mm grid).
const GRID: f64 = 1.27;

/// Minimum spacing between components within a group (4 grid units).
const MIN_COMPONENT_SPACING: f64 = GRID * 8.0;

/// Spacing between groups (8 grid units).
const GROUP_SPACING: f64 = GRID * 16.0;

/// Column separation (24 grid units).
const COLUMN_SPACING: f64 = GRID * 48.0;

/// Starting X offset.
const START_X: f64 = 50.0;

/// Starting Y offset.
const START_Y: f64 = 50.0;

/// Computed position for a component in the schematic.
#[derive(Debug, Clone)]
pub struct ComponentPosition {
    pub ref_des: String,
    pub x: f64,
    pub y: f64,
}

/// Compute schematic positions for all components based on functional groups.
///
/// Pin-count-aware: components with many pins get proportionally more vertical
/// space to prevent pin/wire/label overlap between adjacent components.
pub fn compute_layout(bundle: &IrBundle) -> Vec<ComponentPosition> {
    let registry = SymbolRegistry::new();
    let mut positions = Vec::new();
    let groups = &bundle.ir2.functional_groups;

    // Build a map of ref_des → pin count from IR-1e symbol
    let pin_counts: std::collections::HashMap<&str, usize> = bundle
        .ir1e
        .components
        .iter()
        .map(|c| {
            let count = registry.pin_positions(&c.kicad_symbol).len();
            // Fall back to netlist pin count if symbol not found
            let count = if count > 0 {
                count
            } else {
                bundle
                    .ir2
                    .netlist
                    .iter()
                    .find(|e| e.ref_des == c.ref_des)
                    .map(|e| e.pins.len())
                    .unwrap_or(2)
            };
            (c.ref_des.as_str(), count)
        })
        .collect();

    // Split groups into two columns
    let mid = groups.len().div_ceil(2);

    for (col, chunk) in [&groups[..mid], &groups[mid..]].iter().enumerate() {
        let col_x = START_X + col as f64 * COLUMN_SPACING;
        let mut y = START_Y;

        for group in *chunk {
            for ref_des in &group.components {
                positions.push(ComponentPosition {
                    ref_des: ref_des.clone(),
                    x: snap_to_grid(col_x),
                    y: snap_to_grid(y),
                });
                // Space based on pin count: each pin takes 2.54mm vertically,
                // plus margin for wire stubs and labels
                let pins = *pin_counts.get(ref_des.as_str()).unwrap_or(&2);
                let pin_span = (pins as f64) * 2.54 + 5.08; // pin span + label margin
                let spacing = pin_span.max(MIN_COMPONENT_SPACING);
                y += snap_to_grid(spacing);
            }
            y += GROUP_SPACING;
        }
    }

    positions
}

fn snap_to_grid(v: f64) -> f64 {
    (v / GRID).round() * GRID
}
