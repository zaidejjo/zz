//! Shared syntax data: source spans and the ZZ abstract syntax tree.
//!
//! This crate is the leaf of the frontend/checker stack. It has no
//! dependency on the lexer, parser, diagnostics renderer, or checker, so
//! downstream crates can consume spans and AST nodes without paying for a
//! full frontend rebuild.

pub mod ast;
pub mod span;

pub use ast::{
    BinOp, Block, Decorator, Expr, ExternFunc, FmtPart, Ident, ImportItem, Lit, MatchArm, Param,
    Pattern, Program, Stmt, TraitBound, Ty, TyKind, TypeParam, UnOp,
};
pub use span::Span;
