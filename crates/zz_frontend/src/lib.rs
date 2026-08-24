//! ZZ frontend: lexing, parsing, and lossless printing.
//!
//! Phase 0 scope: integer/float literals, identifiers, arithmetic with
//! precedence, parenthesized expressions, and `let` bindings. The pipeline is
//! deliberately small but the architecture (spans everywhere, lossless
//! round-trip, statement-level error recovery) is the permanent foundation.

pub mod ast;
pub mod diag;
pub mod levenshtein;
pub mod lexer;
pub mod parser;
pub mod printer;
pub mod span;
pub mod token;

pub use ast::{BinOp, Expr, Ident, Program, Stmt, UnOp};
pub use diag::{Diag, Files};
pub use lexer::lex;
pub use parser::{parse, Parsed};
pub use printer::Printer;
pub use span::Span;
pub use token::{Token, TokenKind};
