//! Common test utilities for zz_frontend tests.

use crate::{lex, parse};

pub fn parse_ok(src: &str) -> crate::ast::Program {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
    parsed.program
}

pub fn lex_kinds(src: &str) -> Vec<crate::token::TokenKind> {
    lex(src).tokens.into_iter().map(|t| t.kind).collect()
}
