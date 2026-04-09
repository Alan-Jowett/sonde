// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! KiCad symbol definitions — loaded from installed library or vendored fallback.
//!
//! When a KiCad installation is detected, symbol definitions are read from the
//! installed `.kicad_sym` files. This ensures pin positions, shapes, and electrical
//! types match exactly what KiCad ERC expects. Vendored symbols serve as a fallback
//! when KiCad is not installed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::sexpr::{parser, SExpr};

/// Registry of KiCad symbol definitions.
pub struct SymbolRegistry {
    symbols: HashMap<String, SExpr>,
}

impl Default for SymbolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolRegistry {
    /// Create a new registry, preferring installed KiCad symbols over vendored ones.
    pub fn new() -> Self {
        let mut symbols = HashMap::new();

        // Try loading from installed KiCad
        if let Some(sym_dir) = find_kicad_symbol_dir() {
            let libs = ["Device", "Connector_Generic", "power"];
            for lib_name in &libs {
                let path = sym_dir.join(format!("{lib_name}.kicad_sym"));
                if path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        load_kicad_sym_file(&content, lib_name, &mut symbols);
                    }
                }
            }
        }

        // Fill in any missing symbols from vendored fallback
        for (name, sexpr_str) in VENDORED_SYMBOLS {
            if !symbols.contains_key(*name) {
                if let Ok(parsed) = parser::parse(sexpr_str) {
                    symbols.insert(name.to_string(), parsed);
                }
            }
        }

        Self { symbols }
    }

    /// Create a registry from a specific KiCad symbol directory.
    pub fn from_dir(sym_dir: &Path) -> Self {
        let mut symbols = HashMap::new();
        let libs = ["Device", "Connector_Generic", "power"];
        for lib_name in &libs {
            let path = sym_dir.join(format!("{lib_name}.kicad_sym"));
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    load_kicad_sym_file(&content, lib_name, &mut symbols);
                }
            }
        }
        // Vendored fallback
        for (name, sexpr_str) in VENDORED_SYMBOLS {
            if !symbols.contains_key(*name) {
                if let Ok(parsed) = parser::parse(sexpr_str) {
                    symbols.insert(name.to_string(), parsed);
                }
            }
        }
        Self { symbols }
    }

    /// Look up a symbol definition by library-qualified name.
    pub fn get(&self, name: &str) -> Option<&SExpr> {
        self.symbols.get(name)
    }

    /// Extract pin positions from a symbol definition.
    ///
    /// Returns `(pin_number, x, y)` for each pin in the symbol.
    pub fn pin_positions(&self, name: &str) -> Vec<(String, f64, f64)> {
        let Some(sym) = self.symbols.get(name) else {
            return Vec::new();
        };
        extract_pins_recursive(sym)
    }
}

/// Map a net name to the KiCad power symbol library name.
pub fn power_symbol_name(net_name: &str) -> String {
    match net_name {
        "GND" => "power:GND".to_string(),
        "+3V3" | "3V3" => "power:+3V3".to_string(),
        "+5V" | "5V" => "power:+5V".to_string(),
        other => format!("power:{other}"),
    }
}

/// Detect the installed KiCad symbol library directory.
fn find_kicad_symbol_dir() -> Option<PathBuf> {
    // Check common installation paths
    let candidates = [
        PathBuf::from(r"C:\Program Files\KiCad\8.0\share\kicad\symbols"),
        PathBuf::from(r"C:\Program Files\KiCad\9.0\share\kicad\symbols"),
        PathBuf::from("/usr/share/kicad/symbols"),
        PathBuf::from("/usr/local/share/kicad/symbols"),
        PathBuf::from("/opt/kicad/share/kicad/symbols"),
    ];
    // Also check KICAD8_SYMBOL_DIR env var
    if let Ok(dir) = std::env::var("KICAD8_SYMBOL_DIR") {
        let p = PathBuf::from(dir);
        if p.exists() {
            return Some(p);
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Parse a `.kicad_sym` file and extract symbol definitions into the registry.
///
/// A `.kicad_sym` file has the structure:
/// ```text
/// (kicad_symbol_lib
///   (symbol "R" ...)
///   (symbol "C" ...)
/// )
/// ```
fn load_kicad_sym_file(content: &str, lib_name: &str, symbols: &mut HashMap<String, SExpr>) {
    let Ok(tree) = parser::parse(content.trim()) else {
        return;
    };
    let SExpr::List(items) = &tree else { return };

    for item in items {
        let SExpr::List(children) = item else { continue };
        let Some(SExpr::Atom(tag)) = children.first() else {
            continue;
        };
        if tag != "symbol" {
            continue;
        }
        let Some(sym_name) = children.get(1) else {
            continue;
        };
        let name_str = match sym_name {
            SExpr::Quoted(s) => s.clone(),
            SExpr::Atom(s) => s.clone(),
            _ => continue,
        };
        // Skip sub-units (names like "R_0_1", "R_1_1")
        if name_str.contains('_')
            && name_str
                .rsplit('_')
                .next()
                .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let qualified_name = format!("{lib_name}:{name_str}");

        // Clone the symbol and replace its name with the library-qualified version
        let mut modified = children.clone();
        modified[1] = SExpr::Quoted(qualified_name.clone());
        // Also qualify any sub-unit symbol names inside
        qualify_sub_symbols(&mut modified, lib_name);

        symbols.insert(qualified_name, SExpr::List(modified));
    }
}

/// In KiCad's lib_symbols format, sub-unit symbols (e.g., "R_0_1", "R_1_1")
/// do NOT get library-qualified names — only the top-level symbol does.
/// This function is intentionally a no-op.
fn qualify_sub_symbols(_items: &mut [SExpr], _lib_name: &str) {
    // Sub-unit names must remain unqualified in KiCad's format.
}

/// Extract pin positions from an S-expression symbol definition.
fn extract_pins_recursive(node: &SExpr) -> Vec<(String, f64, f64)> {
    let mut pins = Vec::new();
    if let SExpr::List(items) = node {
        if matches!(items.first(), Some(SExpr::Atom(tag)) if tag == "pin") {
            // (pin <type> <dir> (at x y [rot]) ... (number "N" ...))
            let mut x = 0.0;
            let mut y = 0.0;
            let mut number = String::new();
            for child in items {
                if let SExpr::List(inner) = child {
                    match inner.first() {
                        Some(SExpr::Atom(tag)) if tag == "at" => {
                            if let Some(SExpr::Atom(xv)) = inner.get(1) {
                                x = xv.parse().unwrap_or(0.0);
                            }
                            if let Some(SExpr::Atom(yv)) = inner.get(2) {
                                y = yv.parse().unwrap_or(0.0);
                            }
                        }
                        Some(SExpr::Atom(tag)) if tag == "number" => {
                            if let Some(SExpr::Quoted(n) | SExpr::Atom(n)) = inner.get(1) {
                                number = n.clone();
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !number.is_empty() {
                pins.push((number, x, y));
            }
        } else {
            for child in items {
                pins.extend(extract_pins_recursive(child));
            }
        }
    }
    pins
}

/// Vendored symbol definitions as (name, S-expression) pairs.
///
/// These are minimal but valid KiCad 8 symbol definitions with correct
/// pin counts, numbers, names, and electrical types.
const VENDORED_SYMBOLS: &[(&str, &str)] = &[
    (
        "Device:R",
        r#"(symbol "Device:R"
  (pin_names (offset 0)) (in_bom yes) (on_board yes)
  (property "Reference" "R" (at 2.032 0 90) (effects (font (size 1.27 1.27))))
  (property "Value" "R" (at -2.032 0 90) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at -1.778 0 90) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "R_0_1"
    (rectangle (start -1.016 -2.54) (end 1.016 2.54) (stroke (width 0.254) (type default)) (fill (type none)))
  )
  (symbol "R_1_1"
    (pin passive line (at 0 3.81 270) (length 1.27) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 0 -3.81 90) (length 1.27) (name "~" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Device:C",
        r#"(symbol "Device:C"
  (pin_names (offset 0.254)) (in_bom yes) (on_board yes)
  (property "Reference" "C" (at 0.635 2.54 0) (effects (font (size 1.27 1.27)) (justify left)))
  (property "Value" "C" (at 0.635 -2.54 0) (effects (font (size 1.27 1.27)) (justify left)))
  (property "Footprint" "" (at 0.9652 -3.81 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "C_0_1"
    (polyline (pts (xy -2.032 -0.762) (xy 2.032 -0.762)) (stroke (width 0.508) (type default)) (fill (type none)))
    (polyline (pts (xy -2.032 0.762) (xy 2.032 0.762)) (stroke (width 0.508) (type default)) (fill (type none)))
  )
  (symbol "C_1_1"
    (pin passive line (at 0 3.81 270) (length 2.794) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 0 -3.81 90) (length 2.794) (name "~" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Device:FerriteBead",
        r#"(symbol "Device:FerriteBead"
  (pin_names (offset 0)) (in_bom yes) (on_board yes)
  (property "Reference" "FB" (at 2.032 0 90) (effects (font (size 1.27 1.27))))
  (property "Value" "FerriteBead" (at -2.032 0 90) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at -1.778 0 90) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "FerriteBead_0_1"
    (rectangle (start -1.016 -2.54) (end 1.016 2.54) (stroke (width 0.254) (type default)) (fill (type none)))
  )
  (symbol "FerriteBead_1_1"
    (pin passive line (at 0 3.81 270) (length 1.27) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 0 -3.81 90) (length 1.27) (name "~" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Device:Q_PMOS_GSD",
        r#"(symbol "Device:Q_PMOS_GSD"
  (pin_names (offset 0) hide) (in_bom yes) (on_board yes)
  (property "Reference" "Q" (at 5.08 1.905 0) (effects (font (size 1.27 1.27)) (justify left)))
  (property "Value" "Q_PMOS_GSD" (at 5.08 0 0) (effects (font (size 1.27 1.27)) (justify left)))
  (property "Footprint" "" (at 5.08 -1.905 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "Q_PMOS_GSD_0_1"
    (polyline (pts (xy 0.254 0) (xy -2.54 0)) (stroke (width 0) (type default)) (fill (type none)))
    (polyline (pts (xy 0.254 1.905) (xy 0.254 -1.905)) (stroke (width 0.254) (type default)) (fill (type none)))
    (polyline (pts (xy 0.762 -1.27) (xy 0.762 -2.286)) (stroke (width 0.254) (type default)) (fill (type none)))
    (polyline (pts (xy 0.762 0.508) (xy 0.762 -0.508)) (stroke (width 0.254) (type default)) (fill (type none)))
    (polyline (pts (xy 0.762 2.286) (xy 0.762 1.27)) (stroke (width 0.254) (type default)) (fill (type none)))
  )
  (symbol "Q_PMOS_GSD_1_1"
    (pin passive line (at -5.08 0 0) (length 2.54) (name "G" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 2.54 5.08 270) (length 2.54) (name "S" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    (pin passive line (at 2.54 -5.08 90) (length 2.54) (name "D" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Connector_Generic:Conn_01x02",
        r#"(symbol "Connector_Generic:Conn_01x02"
  (pin_names (offset 1.016) hide) (in_bom yes) (on_board yes)
  (property "Reference" "J" (at 0 2.54 0) (effects (font (size 1.27 1.27))))
  (property "Value" "Conn_01x02" (at 0 -5.08 0) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "Conn_01x02_1_1"
    (pin passive line (at -5.08 0 0) (length 3.81) (name "Pin_1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -2.54 0) (length 3.81) (name "Pin_2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Connector_Generic:Conn_01x03",
        r#"(symbol "Connector_Generic:Conn_01x03"
  (pin_names (offset 1.016) hide) (in_bom yes) (on_board yes)
  (property "Reference" "J" (at 0 5.08 0) (effects (font (size 1.27 1.27))))
  (property "Value" "Conn_01x03" (at 0 -5.08 0) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "Conn_01x03_1_1"
    (pin passive line (at -5.08 2.54 0) (length 3.81) (name "Pin_1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 0 0) (length 3.81) (name "Pin_2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -2.54 0) (length 3.81) (name "Pin_3" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Connector_Generic:Conn_01x04",
        r#"(symbol "Connector_Generic:Conn_01x04"
  (pin_names (offset 1.016) hide) (in_bom yes) (on_board yes)
  (property "Reference" "J" (at 0 5.08 0) (effects (font (size 1.27 1.27))))
  (property "Value" "Conn_01x04" (at 0 -7.62 0) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "Conn_01x04_1_1"
    (pin passive line (at -5.08 2.54 0) (length 3.81) (name "Pin_1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 0 0) (length 3.81) (name "Pin_2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -2.54 0) (length 3.81) (name "Pin_3" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -5.08 0) (length 3.81) (name "Pin_4" (effects (font (size 1.27 1.27)))) (number "4" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "Connector_Generic:Conn_01x07",
        r#"(symbol "Connector_Generic:Conn_01x07"
  (pin_names (offset 1.016) hide) (in_bom yes) (on_board yes)
  (property "Reference" "J" (at 0 10.16 0) (effects (font (size 1.27 1.27))))
  (property "Value" "Conn_01x07" (at 0 -10.16 0) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "Conn_01x07_1_1"
    (pin passive line (at -5.08 7.62 0) (length 3.81) (name "Pin_1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 5.08 0) (length 3.81) (name "Pin_2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 2.54 0) (length 3.81) (name "Pin_3" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 0 0) (length 3.81) (name "Pin_4" (effects (font (size 1.27 1.27)))) (number "4" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -2.54 0) (length 3.81) (name "Pin_5" (effects (font (size 1.27 1.27)))) (number "5" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -5.08 0) (length 3.81) (name "Pin_6" (effects (font (size 1.27 1.27)))) (number "6" (effects (font (size 1.27 1.27)))))
    (pin passive line (at -5.08 -7.62 0) (length 3.81) (name "Pin_7" (effects (font (size 1.27 1.27)))) (number "7" (effects (font (size 1.27 1.27)))))
  )
)"#,
    ),
    (
        "power:GND",
        r##"(symbol "power:GND"
  (power) (pin_names (offset 0)) (in_bom yes) (on_board yes)
  (property "Reference" "#PWR" (at 0 -6.35 0) (effects (font (size 1.27 1.27)) hide))
  (property "Value" "GND" (at 0 -3.81 0) (effects (font (size 1.27 1.27))))
  (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (property "Datasheet" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
  (symbol "GND_0_1"
    (polyline (pts (xy 0 0) (xy 0 -1.27) (xy 1.27 -1.27) (xy 0 -2.54) (xy -1.27 -1.27) (xy 0 -1.27))
      (stroke (width 0) (type default)) (fill (type none)))
  )
  (symbol "GND_1_1"
    (pin power_in line (at 0 0 270) (length 0) (name "GND" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
  )
)"##,
    ),
];
