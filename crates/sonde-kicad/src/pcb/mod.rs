// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! PCB generation — converts IR-1e + IR-2 + IR-3 into KiCad 8 `.kicad_pcb`.

pub mod footprints;
pub mod placement;
pub mod silkscreen;
pub mod zones;

use crate::error::Error;
use crate::ir::IrBundle;
use crate::sexpr::SExpr;
use crate::uuid_gen::UuidGenerator;

/// Generate a KiCad 8 PCB from an IR bundle.
pub fn emit_pcb(bundle: &IrBundle, uuid_gen: &mut UuidGenerator) -> Result<String, Error> {
    let ir3 = bundle
        .ir3
        .as_ref()
        .ok_or(Error::MissingIrFile("IR-3.yaml".into()))?;
    let board = &ir3.board;

    // Center the board on the A4 page (297 × 210 mm)
    let page_w = 297.0;
    let page_h = 210.0;
    let offset_x = (page_w - board.width_mm) / 2.0;
    let offset_y = (page_h - board.height_mm) / 2.0;

    let mut children = vec![
        SExpr::pair("version", "20240108"),
        SExpr::pair_quoted("generator", "sonde-kicad"),
        SExpr::pair_quoted("generator_version", env!("CARGO_PKG_VERSION")),
        SExpr::list(
            "general",
            vec![
                SExpr::list("thickness", vec![SExpr::Atom("1.6".into())]),
                SExpr::pair("legacy_teardrops", "no"),
            ],
        ),
        SExpr::pair_quoted("paper", "A4"),
    ];

    // Layers
    children.push(build_layers(board.layers));

    // Setup (design rules)
    children.push(build_setup(ir3));

    // Net definitions
    let net_map = build_nets(&bundle.ir2, &mut children);

    // Board outline
    build_outline(board, offset_x, offset_y, uuid_gen, &mut children);

    // Keep-out zones
    if let Some(keepouts) = &ir3.keepout_zones {
        zones::build_keepout_zones(
            keepouts,
            board.height_mm,
            offset_x,
            offset_y,
            uuid_gen,
            &mut children,
        );
    }

    // Ground plane copper pour
    zones::build_ground_pour(ir3, &net_map, offset_x, offset_y, uuid_gen, &mut children);

    // Component footprints (placed)
    placement::build_placements(
        bundle,
        &net_map,
        offset_x,
        offset_y,
        uuid_gen,
        &mut children,
    )?;

    // Silkscreen labels
    silkscreen::build_silkscreen(
        ir3,
        board.height_mm,
        offset_x,
        offset_y,
        uuid_gen,
        &mut children,
    );

    let root = SExpr::list("kicad_pcb", children);
    Ok(root.serialize())
}

fn build_layers(layer_count: u32) -> SExpr {
    let mut layers = vec![
        SExpr::List(vec![
            SExpr::Atom("0".into()),
            SExpr::Quoted("F.Cu".into()),
            SExpr::Atom("signal".into()),
        ]),
        SExpr::List(vec![
            SExpr::Atom("31".into()),
            SExpr::Quoted("B.Cu".into()),
            SExpr::Atom("signal".into()),
        ]),
    ];
    if layer_count >= 4 {
        layers.insert(
            1,
            SExpr::List(vec![
                SExpr::Atom("1".into()),
                SExpr::Quoted("In1.Cu".into()),
                SExpr::Atom("signal".into()),
            ]),
        );
        layers.insert(
            2,
            SExpr::List(vec![
                SExpr::Atom("2".into()),
                SExpr::Quoted("In2.Cu".into()),
                SExpr::Atom("signal".into()),
            ]),
        );
    }
    for &(id, name, display) in &[
        ("32", "B.Adhes", "B.Adhesive"),
        ("33", "F.Adhes", "F.Adhesive"),
        ("34", "B.Paste", ""),
        ("35", "F.Paste", ""),
        ("36", "B.SilkS", "B.Silkscreen"),
        ("37", "F.SilkS", "F.Silkscreen"),
        ("38", "B.Mask", ""),
        ("39", "F.Mask", ""),
        ("44", "Edge.Cuts", ""),
        ("46", "B.CrtYd", "B.Courtyard"),
        ("47", "F.CrtYd", "F.Courtyard"),
        ("48", "B.Fab", "B.Fabrication"),
        ("49", "F.Fab", "F.Fabrication"),
    ] {
        let mut items = vec![
            SExpr::Atom(id.into()),
            SExpr::Quoted(name.into()),
            SExpr::Atom("user".into()),
        ];
        if !display.is_empty() {
            items.push(SExpr::Quoted(display.into()));
        }
        layers.push(SExpr::List(items));
    }
    SExpr::list("layers", layers)
}

fn build_setup(ir3: &crate::ir::Ir3) -> SExpr {
    let mut setup_children = vec![
        SExpr::list("pad_to_mask_clearance", vec![SExpr::Atom("0.1".into())]),
        SExpr::list("solder_mask_min_width", vec![SExpr::Atom("0.1".into())]),
    ];

    // Emit net-class definitions from IR-3 routing constraints.
    if let Some(rc) = &ir3.routing_constraints {
        // Default net class (always present)
        let mut default_width = "0.25".to_string();
        if let Some(signal_traces) = &rc.signal_traces {
            // Use the minimum signal trace width (most conservative)
            let min_width = signal_traces
                .iter()
                .map(|st| st.width_mm)
                .fold(f64::INFINITY, f64::min);
            if min_width.is_finite() {
                default_width = fmt(min_width);
            }
        }
        let mut default_nc = vec![
            SExpr::Quoted("Default".into()),
            SExpr::pair_quoted("description", "Default net class"),
            SExpr::list("clearance", vec![SExpr::Atom("0.2".into())]),
            SExpr::list("trace_width", vec![SExpr::Atom(default_width)]),
        ];
        if let Some(via) = &rc.via_constraints {
            default_nc.push(SExpr::list(
                "via_dia",
                vec![SExpr::Atom(fmt(via.diameter_mm))],
            ));
            default_nc.push(SExpr::list(
                "via_drill",
                vec![SExpr::Atom(fmt(via.drill_mm))],
            ));
        }
        setup_children.push(SExpr::list("net_class", default_nc));

        // Power net class from power_traces
        if let Some(power_traces) = &rc.power_traces {
            if let Some(first) = power_traces.first() {
                let pw = first.min_width_mm.unwrap_or(0.5);
                let mut power_nc = vec![
                    SExpr::Quoted("Power".into()),
                    SExpr::pair_quoted("description", "Power net class"),
                    SExpr::list("clearance", vec![SExpr::Atom("0.2".into())]),
                    SExpr::list("trace_width", vec![SExpr::Atom(fmt(pw))]),
                ];
                if let Some(via) = &rc.via_constraints {
                    power_nc.push(SExpr::list(
                        "via_dia",
                        vec![SExpr::Atom(fmt(via.diameter_mm))],
                    ));
                    power_nc.push(SExpr::list(
                        "via_drill",
                        vec![SExpr::Atom(fmt(via.drill_mm))],
                    ));
                }
                // Add net assignments for each power trace (skip copper pours)
                for pt in power_traces {
                    if pt.trace_type.as_deref() == Some("copper pour") {
                        continue; // copper pours handled separately, not as net class traces
                    }
                    power_nc.push(SExpr::list("add_net", vec![SExpr::Quoted(pt.net.clone())]));
                }
                setup_children.push(SExpr::list("net_class", power_nc));
            }
        }
    }

    if let Some(rc) = &ir3.routing_constraints {
        if let Some(_via) = &rc.via_constraints {
            setup_children.push(SExpr::list(
                "pcbplotparams",
                vec![
                    SExpr::pair("layerselection", "0x00010fc_ffffffff"),
                    SExpr::pair_quoted("outputdirectory", ""),
                ],
            ));
        }
    }
    SExpr::list("setup", setup_children)
}

/// Build net definitions and return a name→id mapping.
fn build_nets(
    ir2: &crate::ir::Ir2,
    children: &mut Vec<SExpr>,
) -> std::collections::HashMap<String, u32> {
    let mut net_map = std::collections::HashMap::new();

    // Net 0 is always the unconnected net
    children.push(SExpr::List(vec![
        SExpr::Atom("net".into()),
        SExpr::Atom("0".into()),
        SExpr::Quoted(String::new()),
    ]));

    let mut net_names: Vec<&str> = ir2.nets.iter().map(|n| n.name.as_str()).collect();
    net_names.sort();
    net_names.dedup();

    for (i, name) in net_names.iter().enumerate() {
        let id = (i + 1) as u32;
        net_map.insert(name.to_string(), id);
        children.push(SExpr::List(vec![
            SExpr::Atom("net".into()),
            SExpr::Atom(id.to_string()),
            SExpr::Quoted(name.to_string()),
        ]));
    }

    net_map
}

fn build_outline(
    board: &crate::ir::ir3::Board,
    ox: f64,
    oy: f64,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) {
    let w = board.width_mm;
    let h = board.height_mm;
    let corners = [
        (ox, oy, ox + w, oy),
        (ox + w, oy, ox + w, oy + h),
        (ox + w, oy + h, ox, oy + h),
        (ox, oy + h, ox, oy),
    ];

    for (x1, y1, x2, y2) in &corners {
        children.push(SExpr::list(
            "gr_line",
            vec![
                SExpr::list("start", vec![SExpr::Atom(fmt(*x1)), SExpr::Atom(fmt(*y1))]),
                SExpr::list("end", vec![SExpr::Atom(fmt(*x2)), SExpr::Atom(fmt(*y2))]),
                SExpr::list(
                    "stroke",
                    vec![SExpr::pair("width", "0.05"), SExpr::pair("type", "default")],
                ),
                SExpr::pair_quoted("layer", "Edge.Cuts"),
                SExpr::pair_quoted(
                    "uuid",
                    &uuid_gen.next(&format!("outline:{x1}:{y1}:{x2}:{y2}")),
                ),
            ],
        ));
    }
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}
