//! Abstract syntax tree.
//!
//! Every node carries a `Span` covering its exact source text. The printer
//! re-emits those slices verbatim, which makes parsing → printing a perfect
//! round-trip (lossless).

pub mod expr;
pub mod stmt;
pub mod types;

pub use expr::{Expr, FmtPart, Ident, Lit, MatchArm, Pattern};
pub use stmt::{Block, Param, Program, Stmt};
pub use types::{BinOp, Ty, TyKind, UnOp};
