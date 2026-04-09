// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Silkscreen text generation from IR-3.

use crate::ir::ir3::Ir3;
use crate::sexpr::SExpr;
use crate::uuid_gen::UuidGenerator;

/// Build silkscreen text elements from IR-3.
pub fn build_silkscreen(
    ir3: &Ir3,
    board_height: f64,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) {
    let Some(silk) = &ir3.silkscreen else { return };
    let Some(labels) = &silk.labels else { return };

    for (i, label) in labels.iter().enumerate() {
        let layer = label.layer.as_deref().unwrap_or("F.SilkS");
        // Place labels along the bottom edge, within board bounds
        let board_w = ir3.board.width_mm;
        let num_labels = labels.len().max(1) as f64;
        let x = (board_w / (num_labels + 1.0)) * (i as f64 + 1.0);
        let y = board_height - 1.5; // near bottom edge in KiCad coords (bottom = board_height)
        let _ = board_height;

        children.push(SExpr::list("gr_text", vec![
            SExpr::Quoted(label.text.clone()),
            SExpr::List(vec![
                SExpr::Atom("at".into()),
                SExpr::Atom(fmt(x)),
                SExpr::Atom(fmt(y)),
            ]),
            SExpr::pair_quoted("layer", layer),
            SExpr::pair_quoted("uuid", &uuid_gen.next(&format!("silk:{i}"))),
            SExpr::list("effects", vec![
                SExpr::list("font", vec![
                    SExpr::list("size", vec![SExpr::Atom("1".into()), SExpr::Atom("1".into())]),
                    SExpr::list("thickness", vec![SExpr::Atom("0.15".into())]),
                ]),
            ]),
        ]));
    }
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}
