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
    ox: f64,
    oy: f64,
    uuid_gen: &mut UuidGenerator,
    children: &mut Vec<SExpr>,
) {
    let Some(silk) = &ir3.silkscreen else { return };
    let Some(labels) = &silk.labels else { return };

    for (i, label) in labels.iter().enumerate() {
        let layer = label.layer.as_deref().unwrap_or("F.Fab");

        // Use explicit position if provided, otherwise auto-distribute
        let (x, y) = if let Some(pos) = &label.position {
            let lx = ox + pos.x_mm;
            let ly = oy + board_height - pos.y_mm;
            (lx, ly)
        } else {
            let board_w = ir3.board.width_mm;
            let num_labels = labels.len().max(1) as f64;
            let lx = ox + (board_w / (num_labels + 1.0)) * (i as f64 + 1.0);
            let ly = oy + board_height - 1.5;
            (lx, ly)
        };

        let rot = label.rotation.unwrap_or(0.0);
        let mut at_items = vec![
            SExpr::Atom("at".into()),
            SExpr::Atom(fmt(x)),
            SExpr::Atom(fmt(y)),
        ];
        if rot.abs() > 0.01 {
            at_items.push(SExpr::Atom(fmt(rot)));
        }

        children.push(SExpr::list("gr_text", vec![
            SExpr::Quoted(label.text.clone()),
            SExpr::List(at_items),
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
