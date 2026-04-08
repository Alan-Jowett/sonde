// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! SES file parser — extracts routed wires and vias.

use crate::error::Error;
use crate::sexpr::{parser, SExpr};

/// Parsed routing data from a SES file.
#[derive(Debug)]
pub struct SesRoutes {
    pub wires: Vec<SesWire>,
    pub vias: Vec<SesVia>,
}

/// A routed wire from the SES file.
#[derive(Debug)]
pub struct SesWire {
    pub net: String,
    pub layer: String,
    pub width_um: i64,
    pub segments: Vec<(i64, i64)>,
}

/// A placed via from the SES file.
#[derive(Debug)]
pub struct SesVia {
    pub net: String,
    pub x_um: i64,
    pub y_um: i64,
    pub padstack: String,
}

/// Parse a Freerouter SES file and extract routing data.
pub fn parse_ses(content: &str) -> Result<SesRoutes, Error> {
    let tree = parser::parse(content.trim())
        .map_err(|e| Error::SesParse(format!("SES parse error: {e}")))?;

    let mut wires = Vec::new();
    let mut vias = Vec::new();

    // Navigate to (session ... (routes ... (network_out ...)))
    let routes_node = find_child(&tree, "routes");
    if let Some(routes) = routes_node {
        let network_out = find_child(routes, "network_out");
        if let Some(SExpr::List(items)) = network_out {
            for item in items.iter().skip(1) {
                if let SExpr::List(children) = item {
                    if matches!(children.first(), Some(SExpr::Atom(tag)) if tag == "net") {
                        let net_name = match children.get(1) {
                            Some(SExpr::Atom(s) | SExpr::Quoted(s)) => s.clone(),
                            _ => continue,
                        };
                        parse_net_routes(children, &net_name, &mut wires, &mut vias);
                    }
                }
            }
        }
    }

    Ok(SesRoutes { wires, vias })
}

fn parse_net_routes(
    items: &[SExpr],
    net_name: &str,
    wires: &mut Vec<SesWire>,
    vias: &mut Vec<SesVia>,
) {
    for item in items.iter().skip(2) {
        if let SExpr::List(children) = item {
            match children.first() {
                Some(SExpr::Atom(tag)) if tag == "wire" => {
                    if let Some(wire) = parse_wire(children, net_name) {
                        wires.push(wire);
                    }
                }
                Some(SExpr::Atom(tag)) if tag == "via" => {
                    if let Some(via) = parse_via(children, net_name) {
                        vias.push(via);
                    }
                }
                _ => {}
            }
        }
    }
}

fn parse_wire(items: &[SExpr], net_name: &str) -> Option<SesWire> {
    // (wire (path <layer> <width> x1 y1 x2 y2 ...))
    for item in items.iter().skip(1) {
        if let SExpr::List(children) = item {
            if matches!(children.first(), Some(SExpr::Atom(tag)) if tag == "path") {
                let layer = match children.get(1) {
                    Some(SExpr::Atom(s) | SExpr::Quoted(s)) => s.clone(),
                    _ => return None,
                };
                let width: i64 = match children.get(2) {
                    Some(SExpr::Atom(s)) => s.parse().ok()?,
                    _ => return None,
                };
                let mut segments = Vec::new();
                let mut i = 3;
                while i + 1 < children.len() {
                    let x: i64 = match &children[i] {
                        SExpr::Atom(s) => s.parse().ok()?,
                        _ => return None,
                    };
                    let y: i64 = match &children[i + 1] {
                        SExpr::Atom(s) => s.parse().ok()?,
                        _ => return None,
                    };
                    segments.push((x, y));
                    i += 2;
                }
                return Some(SesWire {
                    net: net_name.to_string(),
                    layer,
                    width_um: width,
                    segments,
                });
            }
        }
    }
    None
}

fn parse_via(items: &[SExpr], net_name: &str) -> Option<SesVia> {
    // (via "<padstack>" x y)
    let padstack = match items.get(1) {
        Some(SExpr::Quoted(s) | SExpr::Atom(s)) => s.clone(),
        _ => return None,
    };
    let x: i64 = match items.get(2) {
        Some(SExpr::Atom(s)) => s.parse().ok()?,
        _ => return None,
    };
    let y: i64 = match items.get(3) {
        Some(SExpr::Atom(s)) => s.parse().ok()?,
        _ => return None,
    };
    Some(SesVia {
        net: net_name.to_string(),
        x_um: x,
        y_um: y,
        padstack,
    })
}

fn find_child<'a>(node: &'a SExpr, tag: &str) -> Option<&'a SExpr> {
    if let SExpr::List(items) = node {
        for item in items {
            if let SExpr::List(children) = item {
                if matches!(children.first(), Some(SExpr::Atom(t)) if t == tag) {
                    return Some(item);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_ses() {
        let ses = r#"(session "test.ses"
  (routes
    (resolution um 10)
    (network_out
      (net VBAT
        (wire (path F.Cu 5000 0 0 10000 0))
      )
      (net GND
        (via "Via[0-1]_600:300_um" 5000 5000)
      )
    )
  )
)"#;
        let routes = parse_ses(ses).unwrap();
        assert_eq!(routes.wires.len(), 1);
        assert_eq!(routes.wires[0].net, "VBAT");
        assert_eq!(routes.wires[0].layer, "F.Cu");
        assert_eq!(routes.wires[0].width_um, 5000);
        assert_eq!(routes.wires[0].segments, vec![(0, 0), (10000, 0)]);

        assert_eq!(routes.vias.len(), 1);
        assert_eq!(routes.vias[0].net, "GND");
        assert_eq!(routes.vias[0].x_um, 5000);
        assert_eq!(routes.vias[0].y_um, 5000);
    }
}
