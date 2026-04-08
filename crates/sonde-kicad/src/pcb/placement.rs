// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Component placement from IR-3 positions and zones.

use std::collections::HashMap;

use crate::error::Error;
use crate::ir::IrBundle;
use crate::sexpr::SExpr;
use crate::uuid_gen::UuidGenerator;

/// Build footprint placement nodes for all components.
pub fn build_placements(
    bundle: &IrBundle,
    net_map: &HashMap<String, u32>,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) -> Result<(), Error> {
    let ir3 = bundle.ir3.as_ref().ok_or(Error::MissingIrFile("IR-3.yaml".into()))?;
    let board_h = ir3.board.height_mm;

    // Build position map: ref_des → (x, y) in KiCad coords
    let mut pos_map: HashMap<String, (f64, f64)> = HashMap::new();

    // Explicit connector placements
    for cp in &ir3.connector_placement {
        let ky = board_h - cp.position.y_mm;
        pos_map.insert(cp.ref_des.clone(), (cp.position.x_mm, ky));
    }

    // Zone-based placements
    for zone in &ir3.component_zones {
        let anchor_x = zone.zone.anchor.x_mm;
        let anchor_ky = board_h - zone.zone.anchor.y_mm;
        let spacing = zone.proximity_constraint_mm;

        for (i, ref_des) in zone.components.iter().enumerate() {
            if pos_map.contains_key(ref_des) {
                continue; // already placed via connector_placement
            }
            let col = i % 3;
            let row = i / 3;
            let x = anchor_x + col as f64 * spacing;
            let y = anchor_ky + row as f64 * spacing;
            pos_map.insert(ref_des.clone(), (x, y));
        }
    }

    // Generate footprint nodes sorted by ref_des
    let mut sorted_comps = bundle.ir1e.components.clone();
    sorted_comps.sort_by(|a, b| {
        crate::schematic::wiring::cmp_ref_des_pub(&a.ref_des, &b.ref_des)
    });

    for comp in &sorted_comps {
        let (x, y) = pos_map.get(&comp.ref_des).copied().unwrap_or((10.0, 10.0));
        let netlist_entry = bundle.ir2.netlist.iter().find(|e| e.ref_des == comp.ref_des);
        let value = netlist_entry.and_then(|e| e.value.as_deref()).unwrap_or("~");

        let mut fp_children = vec![
            SExpr::Quoted(comp.kicad_footprint.clone()),
            SExpr::pair_quoted("layer", "F.Cu"),
            SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("fp:{}", comp.ref_des))),
            SExpr::List(vec![
                SExpr::Atom("at".into()),
                SExpr::Atom(fmt(x)),
                SExpr::Atom(fmt(y)),
            ]),
        ];

        // Properties
        fp_children.push(fp_property("Reference", &comp.ref_des, 0.0, -2.0));
        fp_children.push(fp_property("Value", value, 0.0, 2.0));
        fp_children.push(fp_property_hidden("Footprint", &comp.kicad_footprint));
        fp_children.push(fp_property_hidden("Datasheet", "~"));

        // Pads — generate from IR-2 netlist pins
        if let Some(entry) = netlist_entry {
            for pin in &entry.pins {
                let net_id = if pin.is_nc() {
                    0
                } else {
                    net_map.get(&pin.net).copied().unwrap_or(0)
                };
                let net_name = if pin.is_nc() {
                    String::new()
                } else {
                    pin.net.clone()
                };

                // Simple pad layout: offset by pin index
                let pad_y = (pin.pin as f64 - 1.0) * 1.27;
                fp_children.push(SExpr::list("pad", vec![
                    SExpr::Quoted(pin.pin.to_string()),
                    SExpr::Atom("smd".into()),
                    SExpr::Atom("rect".into()),
                    SExpr::List(vec![
                        SExpr::Atom("at".into()),
                        SExpr::Atom("0".into()),
                        SExpr::Atom(fmt(pad_y)),
                    ]),
                    SExpr::List(vec![
                        SExpr::Atom("size".into()),
                        SExpr::Atom("1.0".into()),
                        SExpr::Atom("0.6".into()),
                    ]),
                    SExpr::list("layers", vec![SExpr::Quoted("F.Cu".into()), SExpr::Quoted("F.Paste".into()), SExpr::Quoted("F.Mask".into())]),
                    SExpr::List(vec![
                        SExpr::Atom("net".into()),
                        SExpr::Atom(net_id.to_string()),
                        SExpr::Quoted(net_name),
                    ]),
                    SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("pad:{}:{}", comp.ref_des, pin.pin))),
                ]));
            }
        }

        children.push(SExpr::list("footprint", fp_children));
    }

    Ok(())
}

fn fp_property(name: &str, value: &str, dx: f64, dy: f64) -> SExpr {
    SExpr::list("property", vec![
        SExpr::Quoted(name.into()),
        SExpr::Quoted(value.into()),
        SExpr::List(vec![
            SExpr::Atom("at".into()),
            SExpr::Atom(fmt(dx)),
            SExpr::Atom(fmt(dy)),
        ]),
        SExpr::list("effects", vec![
            SExpr::list("font", vec![
                SExpr::list("size", vec![SExpr::Atom("1".into()), SExpr::Atom("1".into())]),
                SExpr::list("thickness", vec![SExpr::Atom("0.15".into())]),
            ]),
        ]),
    ])
}

fn fp_property_hidden(name: &str, value: &str) -> SExpr {
    SExpr::list("property", vec![
        SExpr::Quoted(name.into()),
        SExpr::Quoted(value.into()),
        SExpr::List(vec![SExpr::Atom("at".into()), SExpr::Atom("0".into()), SExpr::Atom("0".into())]),
        SExpr::list("effects", vec![
            SExpr::list("font", vec![
                SExpr::list("size", vec![SExpr::Atom("1".into()), SExpr::Atom("1".into())]),
            ]),
            SExpr::Atom("hide".into()),
        ]),
    ])
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}
