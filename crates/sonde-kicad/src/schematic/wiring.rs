// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Wire and label generation for schematic connectivity.

use crate::error::Error;
use crate::ir::IrBundle;
use crate::schematic::layout;
use crate::sexpr::SExpr;
use crate::uuid_gen::UuidGenerator;

/// Schematic connectivity result.
pub struct ConnectivityResult {
    pub instances: Vec<SExpr>,
    pub wires: Vec<SExpr>,
    pub labels: Vec<SExpr>,
    pub no_connects: Vec<SExpr>,
}

/// Build component instances, wires, labels, and no-connect markers.
pub fn build_connectivity(
    bundle: &IrBundle,
    uuid_gen: &mut UuidGenerator,
) -> Result<ConnectivityResult, Error> {
    let positions = layout::compute_layout(bundle);
    let pos_map: std::collections::HashMap<&str, &layout::ComponentPosition> = positions
        .iter()
        .map(|p| (p.ref_des.as_str(), p))
        .collect();

    let registry = crate::schematic::symbols::SymbolRegistry::new();

    let mut instances = Vec::new();
    let mut wires = Vec::new();
    let mut labels = Vec::new();
    let mut no_connects = Vec::new();

    // Build component instances sorted by ref_des
    let mut sorted_components = bundle.ir1e.components.clone();
    sorted_components.sort_by(|a, b| cmp_ref_des(&a.ref_des, &b.ref_des));

    for comp in &sorted_components {
        let pos = pos_map
            .get(comp.ref_des.as_str())
            .ok_or_else(|| Error::CrossValidation(format!(
                "no layout position for `{}`", comp.ref_des
            )))?;

        // Find the netlist entry for this component
        let netlist_entry = bundle
            .ir2
            .netlist
            .iter()
            .find(|e| e.ref_des == comp.ref_des);

        let value = netlist_entry
            .and_then(|e| e.value.as_deref())
            .unwrap_or("~");

        // Build component instance
        let mut inst_children = Vec::new();
        inst_children.push(SExpr::pair_quoted("lib_id", &comp.kicad_symbol));
        inst_children.push(SExpr::List(vec![
            SExpr::Atom("at".into()),
            SExpr::Atom(format_f64(pos.x)),
            SExpr::Atom(format_f64(pos.y)),
            SExpr::Atom("0".into()),
        ]));
        inst_children.push(SExpr::pair("unit", "1"));
        inst_children.push(SExpr::pair("exclude_from_sim", "no"));
        inst_children.push(SExpr::pair("in_bom", "yes"));
        inst_children.push(SExpr::pair("on_board", "yes"));
        inst_children.push(SExpr::pair("dnp", "no"));
        inst_children.push(SExpr::pair_quoted(
            "uuid",
            &uuid_gen.next(&format!("symbol:{}", comp.ref_des)),
        ));

        // Properties
        inst_children.push(property(
            "Reference",
            &comp.ref_des,
            pos.x + 2.54,
            pos.y,
        ));
        inst_children.push(property("Value", value, pos.x + 2.54, pos.y + 2.54));
        inst_children.push(property_hidden(
            "Footprint",
            &comp.kicad_footprint,
            pos.x,
            pos.y,
        ));
        inst_children.push(property_hidden("Datasheet", "~", pos.x, pos.y));

        // Pins — use actual pin positions from symbol definitions
        if let Some(entry) = netlist_entry {
            let sym_pins = registry.pin_positions(&comp.kicad_symbol);
            let pin_pos_map: std::collections::HashMap<&str, (f64, f64)> = sym_pins
                .iter()
                .map(|(num, x, y)| (num.as_str(), (*x, *y)))
                .collect();

            for pin in &entry.pins {
                let pin_uuid =
                    uuid_gen.next(&format!("pin:{}:{}", comp.ref_des, pin.pin));
                inst_children.push(SExpr::list(
                    "pin",
                    vec![
                        SExpr::Quoted(pin.pin.to_string()),
                        SExpr::pair_quoted("uuid", &pin_uuid),
                    ],
                ));

                // Pin endpoint: component position + symbol pin offset
                let pin_num_str = pin.pin.to_string();
                let (pin_dx, pin_dy) = pin_pos_map
                    .get(pin_num_str.as_str())
                    .copied()
                    .unwrap_or((-5.08, (pin.pin as f64 - 1.0) * -2.54));

                let pin_x = pos.x + pin_dx;
                let pin_y = pos.y + pin_dy;

                if pin.is_nc() {
                    // No-connect marker
                    no_connects.push(SExpr::list(
                        "no_connect",
                        vec![
                            SExpr::List(vec![
                                SExpr::Atom("at".into()),
                                SExpr::Atom(format_f64(pin_x)),
                                SExpr::Atom(format_f64(pin_y)),
                            ]),
                            SExpr::pair_quoted(
                                "uuid",
                                &uuid_gen.next(&format!("nc:{}:{}", comp.ref_des, pin.pin)),
                            ),
                        ],
                    ));
                } else {
                    // Wire stub + label
                    let wire_end_x = pin_x - 5.08;
                    wires.push(SExpr::list(
                        "wire",
                        vec![
                            SExpr::list(
                                "pts",
                                vec![
                                    SExpr::List(vec![
                                        SExpr::Atom("xy".into()),
                                        SExpr::Atom(format_f64(pin_x)),
                                        SExpr::Atom(format_f64(pin_y)),
                                    ]),
                                    SExpr::List(vec![
                                        SExpr::Atom("xy".into()),
                                        SExpr::Atom(format_f64(wire_end_x)),
                                        SExpr::Atom(format_f64(pin_y)),
                                    ]),
                                ],
                            ),
                            SExpr::list(
                                "stroke",
                                vec![
                                    SExpr::pair("width", "0"),
                                    SExpr::pair("type", "default"),
                                ],
                            ),
                            SExpr::pair_quoted(
                                "uuid",
                                &uuid_gen.next(&format!(
                                    "wire:{}:{}",
                                    comp.ref_des, pin.pin
                                )),
                            ),
                        ],
                    ));

                    // Net label
                    labels.push(SExpr::list(
                        "label",
                        vec![
                            SExpr::Quoted(pin.net.clone()),
                            SExpr::List(vec![
                                SExpr::Atom("at".into()),
                                SExpr::Atom(format_f64(wire_end_x)),
                                SExpr::Atom(format_f64(pin_y)),
                                SExpr::Atom("0".into()),
                            ]),
                            SExpr::list(
                                "effects",
                                vec![SExpr::list(
                                    "font",
                                    vec![SExpr::list(
                                        "size",
                                        vec![
                                            SExpr::Atom("1.27".into()),
                                            SExpr::Atom("1.27".into()),
                                        ],
                                    )],
                                )],
                            ),
                            SExpr::pair_quoted(
                                "uuid",
                                &uuid_gen.next(&format!(
                                    "label:{}:{}",
                                    comp.ref_des, pin.pin
                                )),
                            ),
                        ],
                    ));
                }
            }
        }

        instances.push(SExpr::list("symbol", inst_children));
    }

    Ok(ConnectivityResult {
        instances,
        wires,
        labels,
        no_connects,
    })
}

fn property(name: &str, value: &str, x: f64, y: f64) -> SExpr {
    SExpr::list(
        "property",
        vec![
            SExpr::Quoted(name.into()),
            SExpr::Quoted(value.into()),
            SExpr::List(vec![
                SExpr::Atom("at".into()),
                SExpr::Atom(format_f64(x)),
                SExpr::Atom(format_f64(y)),
                SExpr::Atom("0".into()),
            ]),
            SExpr::list(
                "effects",
                vec![SExpr::list(
                    "font",
                    vec![SExpr::list(
                        "size",
                        vec![
                            SExpr::Atom("1.27".into()),
                            SExpr::Atom("1.27".into()),
                        ],
                    )],
                )],
            ),
        ],
    )
}

fn property_hidden(name: &str, value: &str, x: f64, y: f64) -> SExpr {
    SExpr::list(
        "property",
        vec![
            SExpr::Quoted(name.into()),
            SExpr::Quoted(value.into()),
            SExpr::List(vec![
                SExpr::Atom("at".into()),
                SExpr::Atom(format_f64(x)),
                SExpr::Atom(format_f64(y)),
                SExpr::Atom("0".into()),
            ]),
            SExpr::list(
                "effects",
                vec![
                    SExpr::list(
                        "font",
                        vec![SExpr::list(
                            "size",
                            vec![
                                SExpr::Atom("1.27".into()),
                                SExpr::Atom("1.27".into()),
                            ],
                        )],
                    ),
                    SExpr::Atom("hide".into()),
                ],
            ),
        ],
    )
}

fn format_f64(v: f64) -> String {
    let s = format!("{v:.4}");
    // Trim trailing zeros but keep at least one decimal place
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

/// Compare reference designators in natural sort order (C1 < C2 < C10 < J1 < R1).
fn cmp_ref_des(a: &str, b: &str) -> std::cmp::Ordering {
    let (a_prefix, a_num) = split_ref_des(a);
    let (b_prefix, b_num) = split_ref_des(b);
    a_prefix
        .cmp(b_prefix)
        .then(a_num.cmp(&b_num))
}

/// Public wrapper for cross-module use.
pub fn cmp_ref_des_pub(a: &str, b: &str) -> std::cmp::Ordering {
    cmp_ref_des(a, b)
}

fn split_ref_des(s: &str) -> (&str, u32) {
    let num_start = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    let prefix = &s[..num_start];
    let num: u32 = s[num_start..].parse().unwrap_or(0);
    (prefix, num)
}
