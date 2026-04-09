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

/// Axis-aligned bounding box (in board coordinates after rotation).
#[derive(Debug, Clone, Copy)]
struct BBox {
    x_min: f64,
    y_min: f64,
    x_max: f64,
    y_max: f64,
}

impl BBox {
    fn overlaps(&self, other: &BBox) -> bool {
        self.x_min < other.x_max
            && self.x_max > other.x_min
            && self.y_min < other.y_max
            && self.y_max > other.y_min
    }

    /// Create a bbox from actual asymmetric courtyard bounds + position + rotation.
    ///
    /// `bounds` = (min_x, min_y, max_x, max_y) relative to footprint origin.
    /// Rotation transforms the bounds before adding position offset.
    /// KiCad uses Y-down, so 90° CW: (x,y) → (y, -x).
    fn from_asymmetric(
        cx: f64,
        cy: f64,
        bounds: (f64, f64, f64, f64),
        rotation: f64,
        margin: f64,
    ) -> Self {
        let (bx_min, by_min, bx_max, by_max) = bounds;

        // Rotate all four corners and take the new AABB
        let corners = [
            (bx_min, by_min),
            (bx_max, by_min),
            (bx_max, by_max),
            (bx_min, by_max),
        ];

        let rot_rad = rotation.to_radians();
        let cos_r = rot_rad.cos();
        let sin_r = rot_rad.sin();

        let rotated: Vec<(f64, f64)> = corners
            .iter()
            .map(|&(x, y)| (x * cos_r - y * sin_r, x * sin_r + y * cos_r))
            .collect();

        let rxs: Vec<f64> = rotated.iter().map(|c| c.0).collect();
        let rys: Vec<f64> = rotated.iter().map(|c| c.1).collect();

        BBox {
            x_min: cx + rxs.iter().cloned().reduce(f64::min).unwrap() - margin,
            y_min: cy + rys.iter().cloned().reduce(f64::min).unwrap() - margin,
            x_max: cx + rxs.iter().cloned().reduce(f64::max).unwrap() + margin,
            y_max: cy + rys.iter().cloned().reduce(f64::max).unwrap() + margin,
        }
    }
}

/// Extract actual courtyard bounds from a KiCad .kicad_mod file.
///
/// Returns (min_x, min_y, max_x, max_y) relative to footprint origin,
/// or a fallback from IR-1e courtyard_mm if the file isn't available.
fn get_courtyard_bounds(
    ref_des: &str,
    bundle: &IrBundle,
    fp_dir: &Option<PathBuf>,
) -> (f64, f64, f64, f64) {
    // Try to read actual courtyard from .kicad_mod
    if let Some(dir) = fp_dir {
        let comp = bundle.ir1e.components.iter().find(|c| c.ref_des == ref_des);
        if let Some(comp) = comp {
            if let Some(path) = resolve_footprint_path(dir, &comp.kicad_footprint) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(bounds) = extract_courtyard_bounds(&content) {
                        return bounds;
                    }
                }
            }
        }
    }

    // Fallback: use IR-1e courtyard_mm as symmetric bounds
    let comp = bundle.ir1e.components.iter().find(|c| c.ref_des == ref_des);
    if let Some(comp) = comp {
        if let Some(d) = &comp.courtyard_mm {
            return (-d.width / 2.0, -d.height / 2.0, d.width / 2.0, d.height / 2.0);
        }
    }
    (-1.0, -1.0, 1.0, 1.0)
}

/// Parse courtyard bounds from .kicad_mod file content.
fn extract_courtyard_bounds(content: &str) -> Option<(f64, f64, f64, f64)> {
    let mut xs = Vec::new();
    let mut ys = Vec::new();

    // Match fp_line with CrtYd layer — these define the courtyard rectangle
    // Pattern: (fp_line (start X Y) (end X Y) ... (layer "F.CrtYd") ...)
    // or       (fp_rect (start X Y) (end X Y) ... (layer "F.CrtYd") ...)
    for line in content.lines() {
        if !line.contains("CrtYd") {
            continue;
        }
        // Extract coordinates from (start X Y) and (end X Y)
        let mut pos = 0;
        while let Some(idx) = line[pos..].find("(start ").or_else(|| line[pos..].find("(end ")) {
            let start = pos + idx;
            let after_paren = start + line[start..].find(' ').unwrap_or(0) + 1;
            let rest = &line[after_paren..];
            let parts: Vec<&str> = rest.splitn(3, |c: char| c.is_whitespace() || c == ')').collect();
            if parts.len() >= 2 {
                if let (Ok(x), Ok(y)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                    xs.push(x);
                    ys.push(y);
                }
            }
            pos = after_paren + 1;
        }
    }

    if xs.len() >= 2 && ys.len() >= 2 {
        Some((
            xs.iter().cloned().reduce(f64::min)?,
            ys.iter().cloned().reduce(f64::min)?,
            xs.iter().cloned().reduce(f64::max)?,
            ys.iter().cloned().reduce(f64::max)?,
        ))
    } else {
        None
    }
}

/// Build footprint placement nodes for all components.
pub fn build_placements(
    bundle: &IrBundle,
    net_map: &HashMap<String, u32>,
    offset_x: f64,
    offset_y: f64,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) -> Result<(), Error> {
    let ir3 = bundle.ir3.as_ref().ok_or(Error::MissingIrFile("IR-3.yaml".into()))?;
    let board_h = ir3.board.height_mm;
    let board_w = ir3.board.width_mm;
    let fp_dir = find_kicad_footprint_dir();

    // Phase 1: Place connectors from IR-3 connector_placement
    let mut pos_map: HashMap<String, (f64, f64, f64)> = HashMap::new();
    let mut placed_bboxes: Vec<BBox> = Vec::new();

    for cp in &ir3.connector_placement {
        let ky = board_h - cp.position.y_mm;

        let rotation = match cp.edge.as_deref() {
            Some("left") => 90.0,
            Some("right") => 270.0,
            Some("top") => 180.0,
            Some("bottom") => 0.0,
            None | Some(_) => {
                let orient = cp.orientation.as_deref().unwrap_or("");
                if orient.contains("pin 1 at bottom") { 180.0 } else { 0.0 }
            }
        };

        let bounds = get_courtyard_bounds(&cp.ref_des, bundle, &fp_dir);
        let bbox = BBox::from_asymmetric(cp.position.x_mm, ky, bounds, rotation, 0.5);
        placed_bboxes.push(bbox);
        pos_map.insert(cp.ref_des.clone(), (cp.position.x_mm, ky, rotation));
    }

    // Phase 2: Place zone components, checking for courtyard overlap
    for zone in &ir3.component_zones {
        let anchor_x = zone.zone.anchor.x_mm;
        let anchor_ky = board_h - zone.zone.anchor.y_mm;
        let spacing = zone.proximity_constraint_mm.max(3.0);

        let to_place: Vec<String> = zone
            .components
            .iter()
            .filter(|r| !pos_map.contains_key(r.as_str()))
            .cloned()
            .collect();

        for (i, ref_des) in to_place.iter().enumerate() {
            let bounds = get_courtyard_bounds(ref_des, bundle, &fp_dir);
            let mut x = anchor_x + (i as f64 % 2.0) * spacing;
            let mut y = anchor_ky + (i as f64 / 2.0).floor() * spacing;

            // Try to find a non-overlapping position
            for _ in 0..200 {
                let candidate = BBox::from_asymmetric(x, y, bounds, 0.0, 0.5);

                let in_bounds = candidate.x_min >= 0.5
                    && candidate.x_max <= board_w - 0.5
                    && candidate.y_min >= 0.5
                    && candidate.y_max <= board_h - 0.5;

                let overlaps = placed_bboxes.iter().any(|b| candidate.overlaps(b));

                let in_keepout = ir3.keepout_zones.as_ref().is_some_and(|kzs| {
                    kzs.iter().any(|kz| {
                        let kx1 = kz.boundary.x_mm;
                        let ky1 = board_h - (kz.boundary.y_mm + kz.boundary.height_mm);
                        let kx2 = kx1 + kz.boundary.width_mm;
                        let ky2 = ky1 + kz.boundary.height_mm;
                        let keepout = BBox { x_min: kx1, y_min: ky1, x_max: kx2, y_max: ky2 };
                        candidate.overlaps(&keepout)
                    })
                });

                if in_bounds && !overlaps && !in_keepout {
                    break;
                }

                // Scan across the board in a grid pattern
                x += spacing;
                if x > board_w - 2.0 {
                    x = 2.0;
                    y += spacing;
                }
                if y > board_h - 2.0 {
                    y = 2.0;
                    x += 1.0;
                }
            }

            let bbox = BBox::from_asymmetric(x, y, bounds, 0.0, 0.25);
            placed_bboxes.push(bbox);
            pos_map.insert(ref_des.clone(), (x, y, 0.0));
        }
    }

    // Phase 3: Generate footprint nodes sorted by ref_des
    let mut sorted_comps = bundle.ir1e.components.clone();
    sorted_comps.sort_by(|a, b| {
        crate::schematic::wiring::cmp_ref_des_pub(&a.ref_des, &b.ref_des)
    });

    for comp in &sorted_comps {
        let (x, y, rotation) = pos_map.get(&comp.ref_des).copied().unwrap_or((12.5, 17.5, 0.0));
        // Apply page offset for centered-on-A4 placement
        let page_x = x + offset_x;
        let page_y = y + offset_y;
        let netlist_entry = bundle.ir2.netlist.iter().find(|e| e.ref_des == comp.ref_des);
        let value = netlist_entry.and_then(|e| e.value.as_deref()).unwrap_or("~");

        let fp_node = if let Some(ref dir) = fp_dir {
            let params = PlacementParams {
                qualified_name: &comp.kicad_footprint,
                ref_des: &comp.ref_des,
                value,
                x: page_x,
                y: page_y,
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
                comp, value, page_x, page_y, rotation, netlist_entry, net_map, uuid_gen,
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

    // Update properties and pads, hide silkscreen text
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

    // Remove fp_text and silkscreen fp_line/fp_poly elements to prevent
    // silk_overlap, silk_over_copper, and silk_edge_clearance violations.
    items.retain(|item| {
        if let SExpr::List(children) = item {
            let tag = match children.first() {
                Some(SExpr::Atom(t)) => t.as_str(),
                _ => return true,
            };
            // Remove all fp_text nodes
            if tag == "fp_text" {
                return false;
            }
            // Remove fp_line/fp_poly/fp_arc on silkscreen layers
            if matches!(tag, "fp_line" | "fp_poly" | "fp_arc" | "fp_rect") {
                let on_silk = children.iter().any(|c| {
                    if let SExpr::List(inner) = c {
                        if matches!(inner.first(), Some(SExpr::Atom(t)) if t == "layer") {
                            return inner.iter().any(|v| matches!(v, SExpr::Quoted(s) if s.contains("SilkS")));
                        }
                    }
                    // Also check for layer as a direct quoted value
                    matches!(c, SExpr::Quoted(s) if s.contains("SilkS"))
                });
                if on_silk {
                    return false;
                }
            }
        }
        true
    });

    // Deduplicate pads: if a pad number appears with and without a net,
    // keep only the one with a net assignment.
    let mut seen_pads_with_net: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items.iter() {
        if let SExpr::List(children) = item {
            if matches!(children.first(), Some(SExpr::Atom(t)) if t == "pad") {
                if let Some(SExpr::Quoted(num) | SExpr::Atom(num)) = children.get(1) {
                    let has_net = children.iter().any(|c| {
                        matches!(c, SExpr::List(inner) if matches!(inner.first(), Some(SExpr::Atom(t)) if t == "net"))
                    });
                    if has_net {
                        seen_pads_with_net.insert(num.clone());
                    }
                }
            }
        }
    }
    items.retain(|item| {
        if let SExpr::List(children) = item {
            if matches!(children.first(), Some(SExpr::Atom(t)) if t == "pad") {
                if let Some(SExpr::Quoted(num) | SExpr::Atom(num)) = children.get(1) {
                    let has_net = children.iter().any(|c| {
                        matches!(c, SExpr::List(inner) if matches!(inner.first(), Some(SExpr::Atom(t)) if t == "net"))
                    });
                    // If this pad has no net but another copy does, remove it
                    if !has_net && seen_pads_with_net.contains(num) {
                        return false;
                    }
                }
            }
        }
        true
    });

    Some(SExpr::List(items))
}

/// Update a property or fp_text node with the correct Reference/Value.
/// Also ensures the text is hidden on silkscreen to prevent overlap.
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

    // Ensure text is hidden to prevent silkscreen overlap
    // Look for (effects ...) and add hide if not present
    for child in children.iter_mut() {
        if let SExpr::List(eff) = child {
            if matches!(eff.first(), Some(SExpr::Atom(t)) if t == "effects")
                && !eff.iter().any(|e| matches!(e, SExpr::Atom(s) if s == "hide"))
                && !eff.iter().any(|e| matches!(e, SExpr::List(inner) if matches!(inner.first(), Some(SExpr::Atom(t)) if t == "hide")))
            {
                eff.push(SExpr::list("hide", vec![SExpr::Atom("yes".into())]));
            }
        }
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
