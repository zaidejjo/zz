//! ZZ frontend: lexing, parsing, and lossless printing.
//!
//! Phase 0 scope: integer/float literals, identifiers, arithmetic with
//! precedence, parenthesized expressions, and `let` bindings. The pipeline is
//! deliberately small but the architecture (spans everywhere, lossless
//! round-trip, statement-level error recovery) is the permanent foundation.

pub mod bench_attr;
pub mod decorators;
pub mod diag;
pub mod doctest;
pub mod fmt;
pub mod lexer;
pub mod move_elide;
pub mod parser;
pub mod printer;
pub mod test_attr;
pub mod tests;
pub mod token;

// Syntax data lives in `zz_span_ast`; re-exported as modules so every
// existing `zz_frontend::ast::…` / `zz_frontend::span::…` path keeps working.
pub use zz_span_ast::{ast, span};
pub use zz_suggest as levenshtein;

pub use ast::{BinOp, Decorator, Expr, Ident, ImportItem, Program, Stmt, UnOp};
pub use decorators::expand_program;
pub use diag::{Diag, Files};
pub use fmt::{format_program, is_formatted, FormatConfig};
pub use lexer::lex;
pub use parser::{parse, Parsed};
pub use printer::Printer;
pub use span::Span;
pub use token::{Token, TokenKind};
