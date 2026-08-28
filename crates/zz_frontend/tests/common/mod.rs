//! Common test utilities for zz_frontend tests.

use zz_frontend::{lex, parse};

pub fn parse_ok(src: &str) -> zz_frontend::ast::Program {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
    parsed.program
}

pub fn lex_kinds(src: &str) -> Vec<zz_frontend::token::TokenKind> {
    lex(src).tokens.into_iter().map(|t| t.kind).collect()
}
