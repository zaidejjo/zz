//! Statement AST nodes.

use crate::ast::expr::{Expr, Ident};
use crate::ast::types::Ty;
use crate::span::Span;

/// A parameter in a function signature or closure.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub ty: Option<Ty>,
    pub default: Option<Box<Expr>>,
    pub span: Span,
}

/// A `{ ... }` block. Its value is the last expression statement, or unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

/// Top-level program.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
    /// Covers the entire source buffer, so printing a program reproduces the
    /// input exactly.
    pub span: Span,
}

/// Statement AST.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// A variable declaration. `ty == None` is the short form (`x := 10`);
    /// `ty == Some(t)` is the explicit form (`x: int = 10`).
    Decl {
        ty: Option<Ty>,
        name: Ident,
        value: Expr,
        span: Span,
    },
    /// `import std.io` — a dotted path of identifiers, optionally aliased
    /// (`import std.io as console`).
    Import {
        path: Vec<String>,
        alias: Option<String>,
        span: Span,
    },
    Func {
        name: Vec<String>,
        generics: Vec<Ident>,
        params: Vec<Param>,
        ret: Option<Ty>,
        body: Block,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    /// `struct Point { x: int, y: int }` — a named record type.
    /// For cross-module: `struct shapes.Point { ... }` stores ["shapes", "Point"].
    Struct {
        name: Vec<String>,
        fields: Vec<(Ident, Ty)>,
        span: Span,
    },
    /// `for x in xs { ... }` or `for k, v in dict { ... }` — iterate an
    /// array, range, or dictionary.
    For {
        vars: Vec<Ident>,
        iter: Box<Expr>,
        body: Block,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    /// `defer expr` — schedule expr to run when the enclosing scope exits.
    Defer {
        expr: Box<Expr>,
        span: Span,
    },
    /// `target = value` — assignment to a variable or struct field.
    Assign {
        target: Expr,
        value: Expr,
        span: Span,
    },
    Expr(Expr),
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Decl { span, .. }
            | Stmt::Func { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Import { span, .. }
            | Stmt::Struct { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Defer { span, .. }
            | Stmt::Assign { span, .. } => *span,
            Stmt::Expr(e) => e.span(),
        }
    }
}
