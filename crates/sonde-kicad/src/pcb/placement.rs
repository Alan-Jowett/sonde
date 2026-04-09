// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Component placement from IR-3 positions and zones.
//!
//! Loads real KiCad footprint definitions from the installed library and
//! embeds them into the PCB with correct positions and net assignments.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::ir::IrBundle;
use crate::sexpr::{parser, SExpr};
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
    let fp_dir = find_kicad_footprint_dir();

    // Build position map: ref_des → (x, y, rotation_degrees) in KiCad coords
    let mut pos_map: HashMap<String, (f64, f64, f64)> = HashMap::new();

    for cp in &ir3.connector_placement {
        let ky = board_h - cp.position.y_mm;

        // Determine rotation from edge placement
        let rotation = match cp.edge.as_deref() {
            Some("left") => 180.0,  // Housing faces left (outward)
            Some("right") => 0.0,   // Housing faces right (outward)
            Some("top") => 90.0,
            Some("bottom") => 270.0,
            _ => 0.0,
        };

        // For edge connectors, offset inward so pads are on-board
        let (adj_x, adj_y) = match cp.edge.as_deref() {
            Some("left") => {
                let cw = cp.courtyard_mm.as_ref().map(|c| c.width).unwrap_or(0.0);
                (cp.position.x_mm + cw / 2.0, ky)
            }
            Some("right") => {
                let cw = cp.courtyard_mm.as_ref().map(|c| c.width).unwrap_or(0.0);
                (cp.position.x_mm - cw / 2.0, ky)
            }
            _ => (cp.position.x_mm, ky),
        };

        pos_map.insert(cp.ref_des.clone(), (adj_x, adj_y, rotation));
    }

    for zone in &ir3.component_zones {
        let anchor_x = zone.zone.anchor.x_mm;
        let anchor_ky = board_h - zone.zone.anchor.y_mm;
        let spacing = zone.proximity_constraint_mm;

        for (i, ref_des) in zone.components.iter().enumerate() {
            if pos_map.contains_key(ref_des) {
                continue;
            }
            let col = i % 3;
            let row = i / 3;
            let x = anchor_x + col as f64 * spacing;
            let y = anchor_ky + row as f64 * spacing;
            pos_map.insert(ref_des.clone(), (x, y, 0.0));
        }
    }

    // Generate footprint nodes sorted by ref_des
    let mut sorted_comps = bundle.ir1e.components.clone();
    sorted_comps.sort_by(|a, b| {
        crate::schematic::wiring::cmp_ref_des_pub(&a.ref_des, &b.ref_des)
    });

    for comp in &sorted_comps {
        let (x, y, rotation) = pos_map.get(&comp.ref_des).copied().unwrap_or((12.5, 17.5, 0.0));
        let netlist_entry = bundle.ir2.netlist.iter().find(|e| e.ref_des == comp.ref_des);
        let value = netlist_entry.and_then(|e| e.value.as_deref()).unwrap_or("~");

        // Try to load real footprint from KiCad library
        let fp_node = if let Some(ref dir) = fp_dir {
            let params = PlacementParams {
                qualified_name: &comp.kicad_footprint,
                ref_des: &comp.ref_des,
                value,
                x,
                y,
                rotation,
                netlist_entry,
                net_map,
            };
            load_library_footprint(dir, &params, uuid_gen)
        } else {
            None
        };

        if let Some(node) = fp_node {
            children.push(node);
        } else {
            children.push(build_stub_footprint(
                comp, value, x, y, rotation, netlist_entry, net_map, uuid_gen,
            ));
        }
    }

    Ok(())
}

/// Find the installed KiCad footprint library directory.
fn find_kicad_footprint_dir() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(r"C:\Program Files\KiCad\8.0\share\kicad\footprints"),
        PathBuf::from(r"C:\Program Files\KiCad\9.0\share\kicad\footprints"),
        PathBuf::from("/usr/share/kicad/footprints"),
        PathBuf::from("/usr/local/share/kicad/footprints"),
    ];
    if let Ok(dir) = std::env::var("KICAD8_FOOTPRINT_DIR") {
        let p = PathBuf::from(dir);
        if p.exists() {
            return Some(p);
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Resolve a library-qualified footprint name to a .kicad_mod file path.
///
/// e.g., `Resistor_SMD:R_0402_1005Metric` → `<dir>/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod`
fn resolve_footprint_path(fp_dir: &Path, qualified_name: &str) -> Option<PathBuf> {
    let (lib, name) = qualified_name.split_once(':')?;
    let path = fp_dir.join(format!("{lib}.pretty")).join(format!("{name}.kicad_mod"));
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Parameters for placing a footprint.
struct PlacementParams<'a> {
    qualified_name: &'a str,
    ref_des: &'a str,
    value: &'a str,
    x: f64,
    y: f64,
    rotation: f64,
    netlist_entry: Option<&'a crate::ir::ir2::NetlistEntry>,
    net_map: &'a HashMap<String, u32>,
}

/// Load a footprint from the KiCad library, set position/nets, and return as SExpr.
fn load_library_footprint(
    fp_dir: &Path,
    params: &PlacementParams<'_>,
    uuid_gen: &mut UuidGenerator,
) -> Option<SExpr> {
    let path = resolve_footprint_path(fp_dir, params.qualified_name)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let parsed = parser::parse(content.trim()).ok()?;

    let SExpr::List(mut items) = parsed else {
        return None;
    };

    // Build a pad-number-to-net mapping from the netlist
    let pad_nets: HashMap<String, (u32, String)> = params.netlist_entry
        .map(|entry| {
            entry
                .pins
                .iter()
                .map(|pin| {
                    let net_id = if pin.is_nc() {
                        0
                    } else {
                        params.net_map.get(&pin.net).copied().unwrap_or(0)
                    };
                    let net_name = if pin.is_nc() {
                        String::new()
                    } else {
                        pin.net.clone()
                    };
                    (pin.pin.to_string(), (net_id, net_name))
                })
                .collect()
        })
        .unwrap_or_default();

    // Replace the footprint name with library-qualified name
    if items.len() > 1 {
        items[1] = SExpr::Quoted(params.qualified_name.to_string());
    }

    // Insert position, layer, and UUID right after the name
    let insert_pos = 2; // after "footprint" and name
    items.insert(insert_pos, SExpr::pair_quoted("layer", "F.Cu"));
    items.insert(
        insert_pos + 1,
        SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("fp:{}", params.ref_des))),
    );
    items.insert(
        insert_pos + 2,
        SExpr::List(vec![
            SExpr::Atom("at".into()),
            SExpr::Atom(fmt(params.x)),
            SExpr::Atom(fmt(params.y)),
            SExpr::Atom(fmt(params.rotation)),
        ]),
    );

    // Update properties and pads throughout
    for item in items.iter_mut().skip(insert_pos + 3) {
        if let SExpr::List(children) = item {
            match children.first() {
                Some(SExpr::Atom(tag)) if tag == "property" || tag == "fp_text" => {
                    update_property(children, params.ref_des, params.value);
                }
                Some(SExpr::Atom(tag)) if tag == "pad" => {
                    assign_pad_net(children, &pad_nets);
                }
                _ => {}
            }
        }
    }

    Some(SExpr::List(items))
}

/// Update a property or fp_text node with the correct Reference/Value.
fn update_property(children: &mut [SExpr], ref_des: &str, value: &str) {
    if children.len() < 3 {
        return;
    }
    let prop_name = match &children[1] {
        SExpr::Quoted(s) => s.clone(),
        SExpr::Atom(s) => s.clone(),
        _ => return,
    };
    match prop_name.as_str() {
        "Reference" | "reference" => {
            children[2] = SExpr::Quoted(ref_des.to_string());
        }
        "Value" | "value" => {
            children[2] = SExpr::Quoted(value.to_string());
        }
        _ => {}
    }
}

/// Assign net to a pad based on pad number.
fn assign_pad_net(children: &mut Vec<SExpr>, pad_nets: &HashMap<String, (u32, String)>) {
    // Pad number is the second element: (pad "1" ...)
    let pad_num = match children.get(1) {
        Some(SExpr::Quoted(s) | SExpr::Atom(s)) => s.clone(),
        _ => return,
    };

    if let Some((net_id, net_name)) = pad_nets.get(&pad_num) {
        // Remove any existing net assignment
        children.retain(|c| {
            !matches!(c, SExpr::List(inner) if matches!(inner.first(), Some(SExpr::Atom(t)) if t == "net"))
        });
        // Add net assignment
        children.push(SExpr::List(vec![
            SExpr::Atom("net".into()),
            SExpr::Atom(net_id.to_string()),
            SExpr::Quoted(net_name.clone()),
        ]));
    }
}

/// Build a stub footprint as fallback when library is unavailable.
#[allow(clippy::too_many_arguments)]
fn build_stub_footprint(
    comp: &crate::ir::ir1e::Ir1eComponent,
    value: &str,
    x: f64,
    y: f64,
    rotation: f64,
    netlist_entry: Option<&crate::ir::ir2::NetlistEntry>,
    net_map: &HashMap<String, u32>,
    uuid_gen: &mut UuidGenerator,
) -> SExpr {
    let mut fp_children = vec![
        SExpr::Quoted(comp.kicad_footprint.clone()),
        SExpr::pair_quoted("layer", "F.Cu"),
        SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("fp:{}", comp.ref_des))),
        SExpr::List(vec![
            SExpr::Atom("at".into()),
            SExpr::Atom(fmt(x)),
            SExpr::Atom(fmt(y)),
            SExpr::Atom(fmt(rotation)),
        ]),
    ];

    fp_children.push(fp_property("Reference", &comp.ref_des, 0.0, -2.0));
    fp_children.push(fp_property("Value", value, 0.0, 2.0));

    if let Some(entry) = netlist_entry {
        for pin in &entry.pins {
            let net_id = if pin.is_nc() { 0 } else { net_map.get(&pin.net).copied().unwrap_or(0) };
            let net_name = if pin.is_nc() { String::new() } else { pin.net.clone() };
            let pad_y = (pin.pin as f64 - 1.0) * 1.27;
            fp_children.push(SExpr::list("pad", vec![
                SExpr::Quoted(pin.pin.to_string()),
                SExpr::Atom("smd".into()),
                SExpr::Atom("rect".into()),
                SExpr::List(vec![SExpr::Atom("at".into()), SExpr::Atom("0".into()), SExpr::Atom(fmt(pad_y))]),
                SExpr::List(vec![SExpr::Atom("size".into()), SExpr::Atom("1.0".into()), SExpr::Atom("0.6".into())]),
                SExpr::list("layers", vec![SExpr::Quoted("F.Cu".into()), SExpr::Quoted("F.Paste".into()), SExpr::Quoted("F.Mask".into())]),
                SExpr::List(vec![SExpr::Atom("net".into()), SExpr::Atom(net_id.to_string()), SExpr::Quoted(net_name)]),
                SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("pad:{}:{}", comp.ref_des, pin.pin))),
            ]));
        }
    }

    SExpr::list("footprint", fp_children)
}

fn fp_property(name: &str, value: &str, dx: f64, dy: f64) -> SExpr {
    SExpr::list("property", vec![
        SExpr::Quoted(name.into()),
        SExpr::Quoted(value.into()),
        SExpr::List(vec![SExpr::Atom("at".into()), SExpr::Atom(fmt(dx)), SExpr::Atom(fmt(dy))]),
        SExpr::list("effects", vec![
            SExpr::list("font", vec![
                SExpr::list("size", vec![SExpr::Atom("1".into()), SExpr::Atom("1".into())]),
                SExpr::list("thickness", vec![SExpr::Atom("0.15".into())]),
            ]),
        ]),
    ])
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}
