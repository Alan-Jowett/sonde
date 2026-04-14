// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! S-expression AST, serializer, and parser for KiCad file formats.

pub mod parser;

/// An S-expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum SExpr {
    /// Unquoted atom (identifier, number).
    Atom(String),
    /// Quoted string value.
    Quoted(String),
    /// Parenthesized list of child nodes.
    List(Vec<SExpr>),
}

impl SExpr {
    /// Create a list node from a tag and children.
    pub fn list(tag: &str, children: Vec<SExpr>) -> Self {
        let mut items = vec![SExpr::Atom(tag.to_string())];
        items.extend(children);
        SExpr::List(items)
    }

    /// Create a simple `(tag value)` pair with an atom value.
    pub fn pair(tag: &str, value: &str) -> Self {
        SExpr::List(vec![
            SExpr::Atom(tag.to_string()),
            SExpr::Atom(value.to_string()),
        ])
    }

    /// Create a simple `(tag "value")` pair with a quoted value.
    pub fn pair_quoted(tag: &str, value: &str) -> Self {
        SExpr::List(vec![
            SExpr::Atom(tag.to_string()),
            SExpr::Quoted(value.to_string()),
        ])
    }

    /// Serialize this S-expression to a string with indentation.
    pub fn serialize(&self) -> String {
        let mut buf = String::new();
        serialize_node(self, &mut buf, 0, true);
        // serialize_node adds a trailing newline for top-level Lists;
        // only add one here for Atoms/Quoted which don't get one.
        if !buf.ends_with('\n') {
            buf.push('\n');
        }
        buf
    }
}

fn serialize_node(node: &SExpr, buf: &mut String, indent: usize, top_level: bool) {
    match node {
        SExpr::Atom(s) => buf.push_str(s),
        SExpr::Quoted(s) => {
            buf.push('"');
            for c in s.chars() {
                match c {
                    '"' => buf.push_str("\\\""),
                    '\\' => buf.push_str("\\\\"),
                    _ => buf.push(c),
                }
            }
            buf.push('"');
        }
        SExpr::List(items) => {
            if items.is_empty() {
                buf.push_str("()");
                return;
            }

            let has_nested_list = items.iter().skip(1).any(|i| matches!(i, SExpr::List(_)));

            if has_nested_list {
                // Multi-line format
                buf.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i == 0 {
                        serialize_node(item, buf, indent + 1, false);
                    } else {
                        buf.push('\n');
                        for _ in 0..(indent + 1) {
                            buf.push_str("  ");
                        }
                        serialize_node(item, buf, indent + 1, false);
                    }
                }
                buf.push('\n');
                for _ in 0..indent {
                    buf.push_str("  ");
                }
                buf.push(')');
            } else {
                // Single-line format
                buf.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        buf.push(' ');
                    }
                    serialize_node(item, buf, indent + 1, false);
                }
                buf.push(')');
            }

            if top_level {
                buf.push('\n');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom_serialization() {
        let node = SExpr::Atom("version".into());
        assert_eq!(node.serialize().trim(), "version");
    }

    #[test]
    fn quoted_serialization() {
        let node = SExpr::Quoted("Device:R".into());
        assert_eq!(node.serialize().trim(), "\"Device:R\"");
    }

    #[test]
    fn simple_list() {
        let node = SExpr::pair("version", "20231120");
        assert_eq!(node.serialize().trim(), "(version 20231120)");
    }

    #[test]
    fn quoted_escape() {
        let node = SExpr::Quoted("hello \"world\"".into());
        assert_eq!(node.serialize().trim(), "\"hello \\\"world\\\"\"");
    }

    #[test]
    fn nested_list_indentation() {
        let inner = SExpr::pair("version", "20231120");
        let outer = SExpr::list("kicad_sch", vec![inner]);
        let s = outer.serialize();
        assert!(s.contains("(kicad_sch"));
        assert!(s.contains("  (version 20231120)"));
    }
}
