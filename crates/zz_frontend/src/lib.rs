//! ZZ frontend: lexing, parsing, and lossless printing.
//!
//! Phase 0 scope: integer/float literals, identifiers, arithmetic with
//! precedence, parenthesized expressions, and `let` bindings. The pipeline is
//! deliberately small but the architecture (spans everywhere, lossless
//! round-trip, statement-level error recovery) is the permanent foundation.
//!
//! Lexer, parser, and diagnostics renderer now live in `zz_lexer`,
//! `zz_parser`, and `zz_diag`. This facade re-exports them so downstream
//! crates keep `use zz_frontend::{…}` unchanged.

pub mod bench_attr;
pub mod decorators;
pub mod doctest;
pub mod fmt;
pub mod move_elide;
pub mod printer;
pub mod test_attr;
pub mod tests;

// Syntax data lives in `zz_span_ast`; re-exported as modules so every
// existing `zz_frontend::ast::…` / `zz_frontend::span::…` path keeps working.
pub use zz_span_ast::{ast, span};
pub use zz_suggest as levenshtein;

pub use ast::{BinOp, Decorator, Expr, Ident, ImportItem, Program, Stmt, UnOp};
pub use decorators::expand_program;
pub use fmt::{format_program, is_formatted, FormatConfig};
pub use printer::Printer;
pub use span::Span;
pub use zz_diag::{Diag, Files};
pub use zz_lexer::lex;
pub use zz_lexer::token::{Token, TokenKind};
pub use zz_parser::{parse, Parsed};

// Compatibility modules so downstream crates keep `zz_frontend::lexer::…`,
// `zz_frontend::parser::…`, `zz_frontend::token::…`, `zz_frontend::diag::…`
// paths unchanged after the split.
pub mod lexer {
    pub use zz_lexer::token::{Token, TokenKind, Trivia, TriviaKind};
    pub use zz_lexer::{lex, Lexed};
}

pub mod parser {
    pub use zz_parser::{parse, Parsed};
}

pub mod token {
    pub use zz_lexer::token::{Token, TokenKind, Trivia, TriviaKind};
}

pub mod diag {
    pub use zz_diag::{Diag, Files, FixIt, FixSafety, RawDiag, Severity};
    pub use zz_diag_core::{error_at, note, warning_at};
}
