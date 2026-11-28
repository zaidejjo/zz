//! Abstract syntax tree.
//!
//! Every node carries a `Span` covering its exact source text. The printer
//! re-emits those slices verbatim, which makes parsing → printing a perfect
//! round-trip (lossless).

pub mod expr;
pub mod regions;
pub mod stmt;
pub mod types;

pub use expr::{Expr, FmtPart, Ident, Lit, MatchArm, Pattern};
pub use regions::{call_cell_is_pure, expr_is_pure, expr_regions, expr_state_reads};
pub use stmt::{
    Block, Decorator, ExternFunc, ImportItem, Param, Program, Stmt, TraitBound, TraitMethod,
    TypeParam,
};
pub use types::{BinOp, Ty, TyKind, UnOp};
