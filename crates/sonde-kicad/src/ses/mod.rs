// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Freerouter SES (session) file import.

pub mod parser;

use crate::error::Error;
use crate::sexpr::{self, SExpr};
use crate::uuid_gen::UuidGenerator;

/// Merge Freerouter SES routing into an existing KiCad PCB.
///
/// Returns the modified PCB content as a string.
pub fn import_ses(
    pcb_content: &str,
    ses_content: &str,
    uuid_gen: &mut UuidGenerator,
) -> Result<String, Error> {
    let routes = parser::parse_ses(ses_content)?;

    // Parse the existing PCB
    let mut pcb_tree = sexpr::parser::parse(pcb_content.trim())
        .map_err(|e| Error::SesParse(format!("PCB parse error: {e}")))?;

    // Find the net name→id mapping from the PCB
    let net_map = extract_net_map(&pcb_tree);

    // Add segments and vias to the PCB tree
    if let SExpr::List(ref mut items) = pcb_tree {
        for route in &routes.wires {
            let net_id = *net_map
                .get(&route.net)
                .ok_or_else(|| Error::Ses(format!("unmapped net `{}` in wire", route.net)))?;
            let width_mm = route.width_um as f64 / 10000.0;

            for seg in route.segments.windows(2) {
                let (x1, y1) = seg[0];
                let (x2, y2) = seg[1];
                // DSN/SES uses a Y-up coordinate system; KiCad uses Y-down.
                // Negate Y values during conversion.
                // SES coordinates use resolution um 10 = 0.1µm units.
                // Convert to mm: divide by 10000.
                let x1_mm = x1 as f64 / 10000.0;
                let y1_mm = -(y1 as f64) / 10000.0;
                let x2_mm = x2 as f64 / 10000.0;
                let y2_mm = -(y2 as f64) / 10000.0;

                items.push(SExpr::list(
                    "segment",
                    vec![
                        SExpr::list(
                            "start",
                            vec![SExpr::Atom(fmt(x1_mm)), SExpr::Atom(fmt(y1_mm))],
                        ),
                        SExpr::list(
                            "end",
                            vec![SExpr::Atom(fmt(x2_mm)), SExpr::Atom(fmt(y2_mm))],
                        ),
                        SExpr::list("width", vec![SExpr::Atom(fmt(width_mm))]),
                        SExpr::pair_quoted("layer", &route.layer),
                        SExpr::List(vec![
                            SExpr::Atom("net".into()),
                            SExpr::Atom(net_id.to_string()),
                        ]),
                        SExpr::pair_quoted(
                            "uuid",
                            &uuid_gen.next(&format!("seg:{}:{}:{}", route.net, x1, y1)),
                        ),
                    ],
                ));
            }
        }

        for via in &routes.vias {
            let net_id = *net_map
                .get(&via.net)
                .ok_or_else(|| Error::Ses(format!("unmapped net `{}` in via", via.net)))?;
            // DSN/SES Y-up → KiCad Y-down: negate Y.
            let x_mm = via.x_um as f64 / 10000.0;
            let y_mm = -(via.y_um as f64) / 10000.0;

            // Extract size/drill from padstack name (e.g., "Via[0-1]_600:300_um").
            let (size_mm, drill_mm) = parse_via_padstack(&via.padstack).unwrap_or((0.6, 0.3));

            items.push(SExpr::list(
                "via",
                vec![
                    SExpr::List(vec![
                        SExpr::Atom("at".into()),
                        SExpr::Atom(fmt(x_mm)),
                        SExpr::Atom(fmt(y_mm)),
                    ]),
                    SExpr::list("size", vec![SExpr::Atom(fmt(size_mm))]),
                    SExpr::list("drill", vec![SExpr::Atom(fmt(drill_mm))]),
                    SExpr::list(
                        "layers",
                        vec![SExpr::Quoted("F.Cu".into()), SExpr::Quoted("B.Cu".into())],
                    ),
                    SExpr::List(vec![
                        SExpr::Atom("net".into()),
                        SExpr::Atom(net_id.to_string()),
                    ]),
                    SExpr::pair_quoted(
                        "uuid",
                        &uuid_gen.next(&format!("via:{}:{}:{}", via.net, via.x_um, via.y_um)),
                    ),
                ],
            ));
        }
    }

    Ok(pcb_tree.serialize())
}

/// Report routing completeness.
pub fn routing_report(
    pcb_content: &str,
    ses_content: &str,
) -> Result<(usize, usize, Vec<String>), Error> {
    let routes = parser::parse_ses(ses_content)?;
    let pcb_tree = sexpr::parser::parse(pcb_content.trim())
        .map_err(|e| Error::SesParse(format!("PCB parse error: {e}")))?;
    let net_map = extract_net_map(&pcb_tree);

    let total_nets = net_map.len();
    let routed_nets: std::collections::HashSet<&str> =
        routes.wires.iter().map(|w| w.net.as_str()).collect();

    let unrouted: Vec<String> = net_map
        .keys()
        .filter(|name| !name.is_empty() && !routed_nets.contains(name.as_str()))
        .cloned()
        .collect();

    let routed_count = total_nets - unrouted.len();
    Ok((routed_count, total_nets, unrouted))
}

fn extract_net_map(pcb: &SExpr) -> std::collections::HashMap<String, u32> {
    let mut net_map = std::collections::HashMap::new();
    if let SExpr::List(items) = pcb {
        for item in items {
            if let SExpr::List(children) = item {
                if matches!(children.first(), Some(SExpr::Atom(tag)) if tag == "net") {
                    if let (Some(SExpr::Atom(id_str)), Some(name_node)) =
                        (children.get(1), children.get(2))
                    {
                        if let Ok(id) = id_str.parse::<u32>() {
                            let name = match name_node {
                                SExpr::Quoted(s) => s.clone(),
                                SExpr::Atom(s) => s.clone(),
                                _ => continue,
                            };
                            net_map.insert(name, id);
                        }
                    }
                }
            }
        }
    }
    net_map
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// Parse via size and drill from a Specctra padstack name.
///
/// Expected format: `Via[0-1]_<size>:<drill>_um` where size/drill are in µm.
/// Returns (size_mm, drill_mm) or None if the name doesn't match.
fn parse_via_padstack(padstack: &str) -> Option<(f64, f64)> {
    // Find the segment between '_' delimiters, e.g., "600:300" from "Via[0-1]_600:300_um"
    let after_first = padstack.find('_')? + 1;
    let rest = &padstack[after_first..];
    let colon = rest.find(':')?;
    let end = rest[colon + 1..]
        .find('_')
        .map(|i| colon + 1 + i)
        .unwrap_or(rest.len());
    let size_um: f64 = rest[..colon].parse().ok()?;
    let drill_um: f64 = rest[colon + 1..end].parse().ok()?;
    Some((size_um / 1000.0, drill_um / 1000.0))
}
