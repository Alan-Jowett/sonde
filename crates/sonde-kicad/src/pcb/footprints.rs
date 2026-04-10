// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Footprint file parser — extracts pad geometry from `.kicad_mod` files.

use std::collections::HashMap;
use std::path::Path;

use crate::sexpr::{parser, SExpr};

/// Pad shape type.
#[derive(Debug, Clone)]
pub enum PadShape {
    Rect,
    RoundRect,
    Circle,
    Oval,
}

/// Pad type (SMD or through-hole).
#[derive(Debug, Clone)]
pub enum PadType {
    Smd,
    ThroughHole,
    NpThroughHole,
}

/// A single pad from a footprint.
#[derive(Debug, Clone)]
pub struct Pad {
    pub number: String,
    pub pad_type: PadType,
    pub shape: PadShape,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub drill: Option<f64>,
    pub layers: Vec<String>,
}

/// Parsed footprint data.
#[derive(Debug, Clone)]
pub struct Footprint {
    pub name: String,
    pub pads: Vec<Pad>,
    pub courtyard_width: Option<f64>,
    pub courtyard_height: Option<f64>,
}

/// Registry of loaded footprint definitions.
pub struct FootprintRegistry {
    footprints: HashMap<String, Footprint>,
}

impl FootprintRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            footprints: HashMap::new(),
        }
    }

    /// Load all `.kicad_mod` files from a directory tree.
    pub fn load_dir(&mut self, dir: &Path) -> Result<usize, crate::Error> {
        let mut count = 0;
        if !dir.exists() {
            return Ok(0);
        }
        for entry in walkdir(dir)? {
            if entry.extension().is_some_and(|e| e == "kicad_mod") {
                if let Ok(fp) = parse_kicad_mod(&std::fs::read_to_string(&entry)?) {
                    self.footprints.insert(fp.name.clone(), fp);
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Look up a footprint by library-qualified name (e.g., `Resistor_SMD:R_0402_1005Metric`).
    pub fn get(&self, name: &str) -> Option<&Footprint> {
        // Try full name first, then just the footprint part after ':'
        self.footprints.get(name).or_else(|| {
            let short = name.split(':').nth(1).unwrap_or(name);
            self.footprints.get(short)
        })
    }
}

impl Default for FootprintRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Walk a directory tree recursively, collecting file paths.
fn walkdir(dir: &Path) -> Result<Vec<std::path::PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(walkdir(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

/// Parse a `.kicad_mod` file content into a `Footprint`.
pub fn parse_kicad_mod(content: &str) -> Result<Footprint, String> {
    let sexpr = parser::parse(content)?;
    let items = match &sexpr {
        SExpr::List(items) => items,
        _ => return Err("expected list at root".into()),
    };

    // Extract footprint name
    let name = match items.get(1) {
        Some(SExpr::Quoted(s)) => s.clone(),
        Some(SExpr::Atom(s)) => s.clone(),
        _ => return Err("expected footprint name".into()),
    };

    let mut pads = Vec::new();

    for item in items.iter().skip(2) {
        if let SExpr::List(children) = item {
            if matches!(children.first(), Some(SExpr::Atom(tag)) if tag == "pad") {
                if let Some(pad) = parse_pad(children) {
                    pads.push(pad);
                }
            }
        }
    }

    Ok(Footprint {
        name,
        pads,
        courtyard_width: None,
        courtyard_height: None,
    })
}

fn parse_pad(items: &[SExpr]) -> Option<Pad> {
    // (pad "1" smd rect (at x y) (size w h) (layers ...) ...)
    let number = match items.get(1) {
        Some(SExpr::Quoted(s) | SExpr::Atom(s)) => s.clone(),
        _ => return None,
    };

    let pad_type = match items.get(2) {
        Some(SExpr::Atom(s)) => match s.as_str() {
            "smd" => PadType::Smd,
            "thru_hole" => PadType::ThroughHole,
            "np_thru_hole" => PadType::NpThroughHole,
            _ => PadType::Smd,
        },
        _ => PadType::Smd,
    };

    let shape = match items.get(3) {
        Some(SExpr::Atom(s)) => match s.as_str() {
            "rect" => PadShape::Rect,
            "roundrect" => PadShape::RoundRect,
            "circle" => PadShape::Circle,
            "oval" => PadShape::Oval,
            _ => PadShape::Rect,
        },
        _ => PadShape::Rect,
    };

    let mut x = 0.0;
    let mut y = 0.0;
    let mut width = 1.0;
    let mut height = 1.0;
    let mut drill = None;
    let mut layers = Vec::new();

    for item in items.iter().skip(4) {
        if let SExpr::List(children) = item {
            match children.first() {
                Some(SExpr::Atom(tag)) if tag == "at" => {
                    x = parse_f64(children.get(1)).unwrap_or(0.0);
                    y = parse_f64(children.get(2)).unwrap_or(0.0);
                }
                Some(SExpr::Atom(tag)) if tag == "size" => {
                    width = parse_f64(children.get(1)).unwrap_or(1.0);
                    height = parse_f64(children.get(2)).unwrap_or(1.0);
                }
                Some(SExpr::Atom(tag)) if tag == "drill" => {
                    drill = parse_f64(children.get(1));
                }
                Some(SExpr::Atom(tag)) if tag == "layers" => {
                    for child in children.iter().skip(1) {
                        if let SExpr::Quoted(s) | SExpr::Atom(s) = child {
                            layers.push(s.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Some(Pad {
        number,
        pad_type,
        shape,
        x,
        y,
        width,
        height,
        drill,
        layers,
    })
}

fn parse_f64(node: Option<&SExpr>) -> Option<f64> {
    match node {
        Some(SExpr::Atom(s)) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_footprint() {
        let input = r#"(footprint "R_0402_1005Metric"
  (pad "1" smd rect (at -0.48 0) (size 0.56 0.62) (layers "F.Cu" "F.Paste" "F.Mask"))
  (pad "2" smd rect (at 0.48 0) (size 0.56 0.62) (layers "F.Cu" "F.Paste" "F.Mask"))
)"#;
        let fp = parse_kicad_mod(input).unwrap();
        assert_eq!(fp.name, "R_0402_1005Metric");
        assert_eq!(fp.pads.len(), 2);
        assert_eq!(fp.pads[0].number, "1");
        assert!((fp.pads[0].x - (-0.48)).abs() < 0.001);
        assert!((fp.pads[0].width - 0.56).abs() < 0.001);
        assert_eq!(fp.pads[1].number, "2");
    }

    #[test]
    fn parse_through_hole_pad() {
        let input = r#"(footprint "PinSocket_1x07"
  (pad "1" thru_hole circle (at 0 0) (size 1.7 1.7) (drill 1.0) (layers "*.Cu" "*.Mask"))
)"#;
        let fp = parse_kicad_mod(input).unwrap();
        assert_eq!(fp.pads.len(), 1);
        assert!(matches!(fp.pads[0].pad_type, PadType::ThroughHole));
        assert_eq!(fp.pads[0].drill, Some(1.0));
    }
}
