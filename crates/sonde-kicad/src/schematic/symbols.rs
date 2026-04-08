// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Vendored KiCad symbol definitions.
//!
//! Symbol shapes are embedded as S-expression fragments compiled into the binary.
//! This eliminates the dependency on a KiCad installation for schematic generation.

use std::collections::HashMap;

use crate::sexpr::{parser, SExpr};

/// Registry of vendored KiCad symbol definitions.
pub struct SymbolRegistry {
    symbols: HashMap<String, SExpr>,
}

impl Default for SymbolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolRegistry {
    /// Create a new registry with all vendored symbols loaded.
    pub fn new() -> Self {
        let mut symbols = HashMap::new();
        for (name, sexpr_str) in VENDORED_SYMBOLS {
            if let Ok(parsed) = parser::parse(sexpr_str) {
                symbols.insert(name.to_string(), parsed);
            }
        }
        Self { symbols }
    }

    /// Look up a symbol definition by library-qualified name.
    pub fn get(&self, name: &str) -> Option<&SExpr> {
        self.symbols.get(name)
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
