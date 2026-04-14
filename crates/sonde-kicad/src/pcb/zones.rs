// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Copper pour zones and keep-out zone generation.

use std::collections::HashMap;

use crate::ir::ir3::{Ir3, KeepoutZone};
use crate::sexpr::SExpr;
use crate::uuid_gen::UuidGenerator;

/// Build keep-out zones from IR-3.
pub fn build_keepout_zones(
    keepouts: &[KeepoutZone],
    board_height: f64,
    ox: f64,
    oy: f64,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) {
    for kz in keepouts {
        let x1 = ox + kz.boundary.x_mm;
        let y1 = oy + board_height - (kz.boundary.y_mm + kz.boundary.height_mm);
        let x2 = x1 + kz.boundary.width_mm;
        let y2 = y1 + kz.boundary.height_mm;
        let layer = kz.layer.as_deref().unwrap_or("F.Cu");

        children.push(SExpr::list(
            "zone",
            vec![
                SExpr::List(vec![SExpr::Atom("net".into()), SExpr::Atom("0".into())]),
                SExpr::pair_quoted("net_name", ""),
                SExpr::pair_quoted("layer", layer),
                SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("keepout:{}", kz.name))),
                SExpr::list(
                    "hatch",
                    vec![SExpr::Atom("edge".into()), SExpr::Atom("0.5".into())],
                ),
                SExpr::list(
                    "connect_pads",
                    vec![SExpr::list("clearance", vec![SExpr::Atom("0".into())])],
                ),
                SExpr::list(
                    "keepout",
                    vec![
                        SExpr::pair("tracks", "not_allowed"),
                        SExpr::pair("vias", "not_allowed"),
                        SExpr::pair("pads", "not_allowed"),
                        SExpr::pair("copperpour", "not_allowed"),
                        SExpr::pair("footprints", "not_allowed"),
                    ],
                ),
                SExpr::list(
                    "fill",
                    vec![
                        SExpr::list("thermal_gap", vec![SExpr::Atom("0.5".into())]),
                        SExpr::list("thermal_bridge_width", vec![SExpr::Atom("0.5".into())]),
                    ],
                ),
                SExpr::list(
                    "polygon",
                    vec![SExpr::list(
                        "pts",
                        vec![xy(x1, y1), xy(x2, y1), xy(x2, y2), xy(x1, y2)],
                    )],
                ),
            ],
        ));
    }
}

/// Build a GND copper pour zone if specified in IR-3 routing constraints.
pub fn build_ground_pour(
    ir3: &Ir3,
    net_map: &HashMap<String, u32>,
    ox: f64,
    oy: f64,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) {
    let Some(rc) = &ir3.routing_constraints else {
        return;
    };
    let Some(power_traces) = &rc.power_traces else {
        return;
    };

    for pt in power_traces {
        if pt.trace_type.as_deref() == Some("copper pour") {
            let layer = pt.layer.as_deref().unwrap_or("B.Cu");
            let net_id = net_map.get(&pt.net).copied().unwrap_or(0);
            let w = ir3.board.width_mm;
            let h = ir3.board.height_mm;

            children.push(SExpr::list(
                "zone",
                vec![
                    SExpr::List(vec![
                        SExpr::Atom("net".into()),
                        SExpr::Atom(net_id.to_string()),
                    ]),
                    SExpr::pair_quoted("net_name", &pt.net),
                    SExpr::pair_quoted("layer", layer),
                    SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("gnd_pour:{}", pt.net))),
                    SExpr::list(
                        "hatch",
                        vec![SExpr::Atom("edge".into()), SExpr::Atom("0.5".into())],
                    ),
                    SExpr::list(
                        "connect_pads",
                        vec![SExpr::list("clearance", vec![SExpr::Atom("0.25".into())])],
                    ),
                    SExpr::list("min_thickness", vec![SExpr::Atom("0.2".into())]),
                    SExpr::list(
                        "fill",
                        vec![
                            SExpr::Atom("yes".into()),
                            SExpr::list("thermal_gap", vec![SExpr::Atom("0.3".into())]),
                            SExpr::list("thermal_bridge_width", vec![SExpr::Atom("0.3".into())]),
                        ],
                    ),
                    SExpr::list(
                        "polygon",
                        vec![SExpr::list(
                            "pts",
                            vec![
                                xy(ox, oy),
                                xy(ox + w, oy),
                                xy(ox + w, oy + h),
                                xy(ox, oy + h),
                            ],
                        )],
                    ),
                ],
            ));
        }
    }
}

fn xy(x: f64, y: f64) -> SExpr {
    SExpr::List(vec![
        SExpr::Atom("xy".into()),
        SExpr::Atom(fmt(x)),
        SExpr::Atom(fmt(y)),
    ])
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}
