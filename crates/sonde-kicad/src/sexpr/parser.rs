// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! S-expression parser for `.kicad_mod` footprint files and `.ses` session files.

use super::SExpr;

/// Parse an S-expression string into an AST.
pub fn parse(input: &str) -> Result<SExpr, String> {
    let tokens = tokenize(input)?;
    let mut pos = 0;
    let result = parse_expr(&tokens, &mut pos)?;
    Ok(result)
}

/// Parse all top-level S-expressions from input.
pub fn parse_all(input: &str) -> Result<Vec<SExpr>, String> {
    let tokens = tokenize(input)?;
    let mut pos = 0;
    let mut results = Vec::new();
    while pos < tokens.len() {
        results.push(parse_expr(&tokens, &mut pos)?);
    }
    Ok(results)
}

#[derive(Debug, Clone)]
enum Token {
    Open,
    Close,
    Atom(String),
    Quoted(String),
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '(' => {
                tokens.push(Token::Open);
                chars.next();
            }
            ')' => {
                tokens.push(Token::Close);
                chars.next();
            }
            '"' => {
                chars.next(); // consume opening quote
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => match chars.next() {
                            Some('"') => s.push('"'),
                            Some('\\') => s.push('\\'),
                            Some(c) => {
                                s.push('\\');
                                s.push(c);
                            }
                            None => return Err("unexpected end of input in escape".into()),
                        },
                        Some('"') => break,
                        Some(c) => s.push(c),
                        None => return Err("unexpected end of input in string".into()),
                    }
                }
                tokens.push(Token::Quoted(s));
            }
            '#' => {
                // Comment — skip to end of line
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            _ => {
                let mut atom = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '(' || c == ')' || c == '"' || c.is_whitespace() {
                        break;
                    }
                    atom.push(c);
                    chars.next();
                }
                tokens.push(Token::Atom(atom));
            }
        }
    }
    Ok(tokens)
}

fn parse_expr(tokens: &[Token], pos: &mut usize) -> Result<SExpr, String> {
    if *pos >= tokens.len() {
        return Err("unexpected end of input".into());
    }
    match &tokens[*pos] {
        Token::Open => {
            *pos += 1;
            let mut items = Vec::new();
            while *pos < tokens.len() {
                if matches!(&tokens[*pos], Token::Close) {
                    *pos += 1;
                    return Ok(SExpr::List(items));
                }
                items.push(parse_expr(tokens, pos)?);
            }
            Err("unexpected end of input, expected ')'".into())
        }
        Token::Close => Err("unexpected ')'".into()),
        Token::Atom(s) => {
            let result = SExpr::Atom(s.clone());
            *pos += 1;
            Ok(result)
        }
        Token::Quoted(s) => {
            let result = SExpr::Quoted(s.clone());
            *pos += 1;
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_list() {
        let result = parse("(version 20231120)").unwrap();
        assert_eq!(
            result,
            SExpr::List(vec![
                SExpr::Atom("version".into()),
                SExpr::Atom("20231120".into()),
            ])
        );
    }

    #[test]
    fn parse_quoted_string() {
        let result = parse("(lib_id \"Device:R\")").unwrap();
        assert_eq!(
            result,
            SExpr::List(vec![
                SExpr::Atom("lib_id".into()),
                SExpr::Quoted("Device:R".into()),
            ])
        );
    }

    #[test]
    fn parse_nested() {
        let result = parse("(a (b c) (d e))").unwrap();
        match result {
            SExpr::List(items) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(&items[1], SExpr::List(_)));
                assert!(matches!(&items[2], SExpr::List(_)));
            }
            _ => panic!("expected list"),
        }
    }

    #[test]
    fn parse_escaped_quote() {
        let result = parse("(value \"4.7k\\\"test\\\"\")").unwrap();
        match &result {
            SExpr::List(items) => {
                assert_eq!(items[1], SExpr::Quoted("4.7k\"test\"".into()));
            }
            _ => panic!("expected list"),
        }
    }

    #[test]
    fn round_trip() {
        let original = SExpr::list(
            "kicad_sch",
            vec![
                SExpr::pair("version", "20231120"),
                SExpr::pair_quoted("generator", "sonde-kicad"),
            ],
        );
        let serialized = original.serialize();
        let parsed = parse(serialized.trim()).unwrap();
        assert_eq!(original, parsed);
    }
}
