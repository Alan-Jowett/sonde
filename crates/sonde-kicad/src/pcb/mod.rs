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
        ("45", "Margin", ""),
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

fn build_setup(_ir3: &crate::ir::Ir3) -> SExpr {
    let stackup = SExpr::list(
        "stackup",
        vec![
            stackup_layer("F.SilkS", "Top Silk Screen", None),
            stackup_layer("F.Paste", "Top Solder Paste", None),
            stackup_layer("F.Mask", "Top Solder Mask", Some("0.01")),
            stackup_layer("F.Cu", "copper", Some("0.035")),
            dielectric_layer("dielectric 1", "core", "1.51"),
            stackup_layer("B.Cu", "copper", Some("0.035")),
            stackup_layer("B.Mask", "Bottom Solder Mask", Some("0.01")),
            stackup_layer("B.Paste", "Bottom Solder Paste", None),
            stackup_layer("B.SilkS", "Bottom Silk Screen", None),
            SExpr::pair_quoted("copper_finish", "None"),
            SExpr::pair("dielectric_constraints", "no"),
        ],
    );

    let pcbplotparams = SExpr::list(
        "pcbplotparams",
        vec![
            SExpr::pair("layerselection", "0x00010fc_ffffffff"),
            SExpr::pair("plot_on_all_layers_selection", "0x0000000_00000000"),
            SExpr::pair("disableapertmacros", "no"),
            SExpr::pair("usegerberextensions", "no"),
            SExpr::pair("usegerberattributes", "yes"),
            SExpr::pair("usegerberadvancedattributes", "yes"),
            SExpr::pair("creategerberjobfile", "yes"),
            SExpr::pair("dashed_line_dash_ratio", "12.000000"),
            SExpr::pair("dashed_line_gap_ratio", "3.000000"),
            SExpr::pair("svgprecision", "4"),
            SExpr::pair("plotframeref", "no"),
            SExpr::pair("viasonmask", "no"),
            SExpr::pair("mode", "1"),
            SExpr::pair("useauxorigin", "no"),
            SExpr::pair("hpglpennumber", "1"),
            SExpr::pair("hpglpenspeed", "20"),
            SExpr::pair("hpglpendiameter", "15.000000"),
            SExpr::pair("pdf_front_fp_property_popups", "yes"),
            SExpr::pair("pdf_back_fp_property_popups", "yes"),
            SExpr::pair("dxfpolygonmode", "yes"),
            SExpr::pair("dxfimperialunits", "yes"),
            SExpr::pair("dxfusepcbnewfont", "yes"),
            SExpr::pair("psnegative", "no"),
            SExpr::pair("psa4output", "no"),
            SExpr::pair("plotreference", "yes"),
            SExpr::pair("plotvalue", "yes"),
            SExpr::pair("plotfptext", "yes"),
            SExpr::pair("plotinvisibletext", "no"),
            SExpr::pair("sketchpadsonfab", "no"),
            SExpr::pair("subtractmaskfromsilk", "no"),
            SExpr::pair("outputformat", "1"),
            SExpr::pair("mirror", "no"),
            SExpr::pair("drillshape", "1"),
            SExpr::pair("scaleselection", "1"),
            SExpr::pair_quoted("outputdirectory", ""),
        ],
    );

    // KiCad 9 stores route widths, via dimensions, and net classes in the
    // companion `.kicad_pro` file rather than the board's `(setup ...)` block.
    SExpr::list(
        "setup",
        vec![
            stackup,
            SExpr::list("pad_to_mask_clearance", vec![SExpr::Atom("0".into())]),
            SExpr::pair("allow_soldermask_bridges_in_footprints", "no"),
            pcbplotparams,
        ],
    )
}

fn stackup_layer(name: &str, layer_type: &str, thickness: Option<&str>) -> SExpr {
    let mut children = vec![SExpr::pair_quoted("type", layer_type)];
    if let Some(value) = thickness {
        children.push(SExpr::list("thickness", vec![SExpr::Atom(value.into())]));
    }
    let mut items = vec![SExpr::Atom("layer".into()), SExpr::Quoted(name.into())];
    items.extend(children);
    SExpr::List(items)
}

fn dielectric_layer(name: &str, layer_type: &str, thickness: &str) -> SExpr {
    SExpr::List(vec![
        SExpr::Atom("layer".into()),
        SExpr::Quoted(name.into()),
        SExpr::pair_quoted("type", layer_type),
        SExpr::list("thickness", vec![SExpr::Atom(thickness.into())]),
        SExpr::pair_quoted("material", "FR4"),
        SExpr::list("epsilon_r", vec![SExpr::Atom("4.5".into())]),
        SExpr::list("loss_tangent", vec![SExpr::Atom("0.02".into())]),
    ])
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
