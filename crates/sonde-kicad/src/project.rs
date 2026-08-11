// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! KiCad 9 project generation — emits the companion `.kicad_pro` file that
//! stores routing rules and net classes for a board.

use std::collections::BTreeMap;

use serde_json::json;

use crate::{error::Error, ir::IrBundle};

const DEFAULT_CLEARANCE_MM: f64 = 0.2;
const DEFAULT_SIGNAL_WIDTH_MM: f64 = 0.25;
const DEFAULT_VIA_DIAMETER_MM: f64 = 0.8;
const DEFAULT_VIA_DRILL_MM: f64 = 0.4;

pub fn emit_project(bundle: &IrBundle) -> Result<String, Error> {
    let ir3 = bundle
        .ir3
        .as_ref()
        .ok_or(Error::MissingIrFile("IR-3.yaml".into()))?;
    let project_filename = format!("{}.kicad_pro", bundle.project);

    let signal_width = ir3
        .routing_constraints
        .as_ref()
        .and_then(|rc| rc.signal_traces.as_ref())
        .and_then(|signal_traces| signal_traces.iter().map(|st| st.width_mm).reduce(f64::min))
        .unwrap_or(DEFAULT_SIGNAL_WIDTH_MM);

    let (via_diameter, via_drill) = ir3
        .routing_constraints
        .as_ref()
        .and_then(|rc| rc.via_constraints.as_ref())
        .map(|via| (via.diameter_mm, via.drill_mm))
        .unwrap_or((DEFAULT_VIA_DIAMETER_MM, DEFAULT_VIA_DRILL_MM));

    let power_classes = build_power_classes(ir3);
    let mut track_widths = vec![signal_width];
    for class in &power_classes {
        if !track_widths.contains(&class.width_mm) {
            track_widths.push(class.width_mm);
        }
    }
    track_widths.sort_by(f64::total_cmp);

    let mut classes = vec![json!({
        "bus_width": 12,
        "clearance": DEFAULT_CLEARANCE_MM,
        "diff_pair_gap": 0.25,
        "diff_pair_via_gap": 0.25,
        "diff_pair_width": 0.2,
        "line_style": 0,
        "microvia_diameter": 0.3,
        "microvia_drill": 0.1,
        "name": "Default",
        "pcb_color": "rgba(0, 0, 0, 0.000)",
        "priority": 2147483647u32,
        "schematic_color": "rgba(0, 0, 0, 0.000)",
        "track_width": signal_width,
        "via_diameter": via_diameter,
        "via_drill": via_drill,
        "wire_width": 6
    })];

    let mut netclass_patterns = Vec::new();
    for (priority, class) in power_classes.iter().enumerate() {
        classes.push(json!({
            "bus_width": 12,
            "clearance": DEFAULT_CLEARANCE_MM,
            "diff_pair_gap": 0.25,
            "diff_pair_via_gap": 0.25,
            "diff_pair_width": 0.2,
            "line_style": 0,
            "microvia_diameter": 0.3,
            "microvia_drill": 0.1,
            "name": class.name,
            "pcb_color": "rgba(0, 0, 0, 0.000)",
            "priority": priority as u32,
            "schematic_color": "rgba(0, 0, 0, 0.000)",
            "track_width": class.width_mm,
            "via_diameter": via_diameter,
            "via_drill": via_drill,
            "wire_width": 6
        }));
        for net in &class.nets {
            netclass_patterns.push(json!({
                "netclass": class.name,
                "pattern": net
            }));
        }
    }

    let project = json!({
        "board": {
            "3dviewports": [],
            "design_settings": {
                "defaults": {
                    "apply_defaults_to_fp_fields": false,
                    "apply_defaults_to_fp_shapes": false,
                    "apply_defaults_to_fp_text": false,
                    "board_outline_line_width": 0.15,
                    "copper_line_width": signal_width,
                    "courtyard_line_width": 0.05,
                    "dimension_precision": 4,
                    "dimension_units": 3,
                    "dimensions": {
                        "arrow_length": 1270000,
                        "extension_offset": 500000,
                        "keep_text_aligned": true,
                        "suppress_zeroes": false,
                        "text_position": 0,
                        "units_format": 1
                    },
                    "fab_line_width": 0.1,
                    "fab_text_italic": false,
                    "fab_text_size_h": 1.0,
                    "fab_text_size_v": 1.0,
                    "fab_text_thickness": 0.15,
                    "fab_text_upright": false,
                    "other_line_width": 0.1,
                    "other_text_italic": false,
                    "other_text_size_h": 1.0,
                    "other_text_size_v": 1.0,
                    "other_text_thickness": 0.15,
                    "other_text_upright": false,
                    "pads": {
                        "drill": 0.762,
                        "height": 1.524,
                        "width": 1.524
                    },
                    "silk_line_width": 0.15,
                    "silk_text_italic": false,
                    "silk_text_size_h": 1.0,
                    "silk_text_size_v": 1.0,
                    "silk_text_thickness": 0.15,
                    "silk_text_upright": false,
                    "zones": {
                        "45_degree_only": false,
                        "min_clearance": 0.0
                    }
                },
                "diff_pair_dimensions": [
                    {
                        "gap": 0.0,
                        "via_gap": 0.0,
                        "width": 0.0
                    }
                ],
                "drc_exclusions": [],
                "meta": {
                    "filename": "board_design_settings.json",
                    "version": 2
                },
                "rule_severities": {
                    "track_width": "error",
                    "unconnected_items": "error",
                    "via_dangling": "warning"
                },
                "rules": {
                    "allow_blind_buried_vias": false,
                    "allow_microvias": false,
                    "max_error": 0.005,
                    "min_clearance": 0.0,
                    "min_connection": 0.0,
                    "min_copper_edge_clearance": 0.075,
                    "min_groove_width": 0.0,
                    "min_hole_clearance": 0.0,
                    "min_hole_to_hole": 0.25,
                    "min_microvia_diameter": 0.2,
                    "min_microvia_drill": 0.1,
                    "min_resolved_spokes": 2,
                    "min_silk_clearance": 0.0,
                    "min_text_height": 0.8,
                    "min_text_thickness": 0.08,
                    "min_through_hole_diameter": via_drill,
                    "min_track_width": signal_width,
                    "min_via_annular_width": ((via_diameter - via_drill) / 2.0).max(0.05),
                    "min_via_diameter": via_diameter,
                    "solder_mask_to_copper_clearance": 0.0,
                    "use_height_for_length_calcs": true
                },
                "teardrop_options": [
                    {
                        "td_onpthpad": true,
                        "td_onroundshapesonly": false,
                        "td_onsmdpad": true,
                        "td_ontrackend": false,
                        "td_onvia": true
                    }
                ],
                "teardrop_parameters": [
                    {
                        "td_allow_use_two_tracks": true,
                        "td_curve_segcount": 0,
                        "td_height_ratio": 1.0,
                        "td_length_ratio": 0.5,
                        "td_maxheight": 2.0,
                        "td_maxlen": 1.0,
                        "td_on_pad_in_zone": false,
                        "td_target_name": "td_round_shape",
                        "td_width_to_size_filter_ratio": 0.9
                    }
                ],
                "track_widths": track_widths,
                "tuning_pattern_settings": {
                    "diff_pair_defaults": {
                        "corner_radius_percentage": 80,
                        "corner_style": 1,
                        "max_amplitude": 1.0,
                        "min_amplitude": 0.2,
                        "single_sided": false,
                        "spacing": 1.0
                    },
                    "diff_pair_skew_defaults": {
                        "corner_radius_percentage": 80,
                        "corner_style": 1,
                        "max_amplitude": 1.0,
                        "min_amplitude": 0.2,
                        "single_sided": false,
                        "spacing": 0.6
                    },
                    "single_track_defaults": {
                        "corner_radius_percentage": 80,
                        "corner_style": 1,
                        "max_amplitude": 1.0,
                        "min_amplitude": 0.2,
                        "single_sided": false,
                        "spacing": 0.6
                    }
                },
                "via_dimensions": [
                    {
                        "diameter": via_diameter,
                        "drill": via_drill
                    }
                ],
                "zones_allow_external_fillets": false,
                "zones_use_no_outline": true
            },
            "ipc2581": {
                "dist": "",
                "distpn": "",
                "internal_id": "",
                "mfg": "",
                "mpn": ""
            },
            "layer_pairs": [],
            "layer_presets": [],
            "viewports": []
        },
        "boards": [],
        "cvpcb": {
            "equivalence_files": []
        },
        "meta": {
            "filename": project_filename,
            "version": 3
        },
        "net_settings": {
            "classes": classes,
            "meta": {
                "version": 4
            },
            "net_colors": serde_json::Value::Null,
            "netclass_assignments": serde_json::Value::Null,
            "netclass_patterns": netclass_patterns
        },
        "pcbnew": {
            "last_paths": {
                "gencad": "",
                "idf": "",
                "netlist": "",
                "plot": "",
                "pos_files": "",
                "specctra_dsn": "",
                "step": "",
                "svg": "",
                "vrml": ""
            },
            "page_layout_descr_file": ""
        }
    });

    serde_json::to_string_pretty(&project).map_err(|e| Error::CrossValidation(e.to_string()))
}

struct PowerClass {
    name: String,
    width_mm: f64,
    nets: Vec<String>,
}

fn build_power_classes(ir3: &crate::ir::Ir3) -> Vec<PowerClass> {
    let Some(rc) = &ir3.routing_constraints else {
        return Vec::new();
    };
    let Some(power_traces) = &rc.power_traces else {
        return Vec::new();
    };

    let mut groups: BTreeMap<String, (f64, Vec<String>)> = BTreeMap::new();
    for trace in power_traces {
        if trace.trace_type.as_deref() == Some("copper pour") {
            continue;
        }
        let Some(width_mm) = trace.min_width_mm else {
            continue;
        };
        let key = fmt(width_mm);
        groups
            .entry(key)
            .and_modify(|(_, nets)| nets.push(trace.net.clone()))
            .or_insert_with(|| (width_mm, vec![trace.net.clone()]));
    }

    let mut classes: Vec<_> = groups.into_values().collect();
    classes.sort_by(|a, b| b.0.total_cmp(&a.0));

    classes
        .into_iter()
        .enumerate()
        .map(|(idx, (width_mm, nets))| PowerClass {
            name: if idx == 0 && nets.len() == 1 {
                "Power".into()
            } else {
                format!("Power_{}", idx + 1)
            },
            width_mm,
            nets,
        })
        .collect()
}

fn fmt(v: f64) -> String {
    let mut s = format!("{v:.3}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}
