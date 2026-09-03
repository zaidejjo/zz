//! Typed AST traversal helpers.
//!
//! `walk` provides a structural visitor over the `Program` that yields each
//! expression paired with its resolved type (looked up from
//! [`TypedProgram::types`]). Used by the call-graph analyzer (Phase 2) and
//! the C lowering pass (Phase 3) without duplicating the AST-shape logic.

use crate::{Expr, Stmt, TypedProgram};

/// A typed expression visited during traversal: the node plus its resolved
/// type (when available).
#[derive(Debug, Clone)]
pub struct TypedExpr<'a> {
    pub expr: &'a Expr,
    /// Resolved type, `None` when the checker left it unresolved (dynamic).
    pub ty: Option<&'a crate::Type>,
}

impl<'a> TypedExpr<'a> {
    pub fn span(&self) -> zz_frontend::span::Span {
        self.expr.span()
    }
}

/// Visit every expression in the program (pre-order) via `f`.
///
/// `f` receives the typed expression; a `false` return prunes the subtree
/// (avoids descending into `Block`/`Closure` bodies when not wanted).
pub fn walk_exprs<'a>(tp: &'a TypedProgram, f: &mut impl FnMut(&TypedExpr<'a>) -> bool) {
    for stmt in tp.stmts() {
        walk_stmt(tp, stmt, f);
    }
}

/// Visit a single statement's expressions.
pub fn walk_stmt<'a>(
    tp: &'a TypedProgram,
    stmt: &'a Stmt,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    match stmt {
        Stmt::Decl { value, .. } => {
            walk_expr(tp, value, f);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                walk_expr(tp, v, f);
            }
        }
        Stmt::Func { body, .. } => {
            for s in &body.stmts {
                walk_stmt(tp, s, f);
            }
        }
        Stmt::Struct { .. } | Stmt::Import { .. } => {}
        Stmt::Impl { methods, .. } => {
            for m in methods {
                walk_stmt(tp, m, f);
            }
        }
        Stmt::For { iter, body, .. } => {
            walk_expr(tp, iter, f);
            for s in &body.stmts {
                walk_stmt(tp, s, f);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Defer { expr, .. } => {
            walk_expr(tp, expr, f);
        }
        Stmt::Assign { target, value, .. } => {
            walk_expr(tp, target, f);
            walk_expr(tp, value, f);
        }
        Stmt::Destructure { value, .. } => {
            walk_expr(tp, value, f);
        }
        Stmt::Expr(e) => walk_expr(tp, e, f),
    }
}

/// Visit one expression subtree.
pub fn walk_expr<'a>(
    tp: &'a TypedProgram,
    e: &'a Expr,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    let te = TypedExpr {
        expr: e,
        ty: tp.type_at(e.span()),
    };
    if !f(&te) {
        return;
    }
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Ident { .. }
        | Expr::Path { .. } => {}
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = p {
                    walk_expr(tp, inner, f);
                }
            }
        }
        Expr::Paren { expr, .. } => walk_expr(tp, expr, f),
        Expr::Tuple { items, .. } => {
            for it in items {
                walk_expr(tp, it, f);
            }
        }
        Expr::Unary { expr, .. } => walk_expr(tp, expr, f),
        Expr::Binary { left, right, .. } => {
            walk_expr(tp, left, f);
            walk_expr(tp, right, f);
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            walk_expr(tp, callee, f);
            for a in args {
                walk_expr(tp, a, f);
            }
            for (_, a) in named {
                walk_expr(tp, a, f);
            }
        }
        Expr::Closure { body, .. } => walk_expr(tp, body, f),
        Expr::If {
            cond, then, els, ..
        } => {
            walk_expr(tp, cond, f);
            for s in &then.stmts {
                walk_stmt(tp, s, f);
            }
            if let Some(el) = els {
                walk_expr(tp, el, f);
            }
        }
        Expr::While { cond, body, .. } => {
            walk_expr(tp, cond, f);
            for s in &body.stmts {
                walk_stmt(tp, s, f);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(tp, scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(tp, g, f);
                }
                walk_expr(tp, &arm.body, f);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            walk_expr(tp, value, f);
            for s in &then.stmts {
                walk_stmt(tp, s, f);
            }
            if let Some(el) = els {
                walk_expr(tp, el, f);
            }
        }
        Expr::Try { expr, .. } => walk_expr(tp, expr, f),
        Expr::Block(b) => {
            for s in &b.stmts {
                walk_stmt(tp, s, f);
            }
        }
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                walk_expr(tp, a, f);
            }
        }
        Expr::Array { elems, .. } => {
            for el in elems {
                walk_expr(tp, el, f);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                walk_expr(tp, k, f);
                walk_expr(tp, v, f);
            }
        }
        Expr::Field { obj, .. } => walk_expr(tp, obj, f),
        Expr::Range { start, end, .. } => {
            walk_expr(tp, start, f);
            walk_expr(tp, end, f);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                walk_expr(tp, v, f);
            }
        }
        Expr::Index { obj, index, .. } => {
            walk_expr(tp, obj, f);
            walk_expr(tp, index, f);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            walk_expr(tp, obj, f);
            if let Some(s) = start {
                walk_expr(tp, s, f);
            }
            if let Some(e) = end {
                walk_expr(tp, e, f);
            }
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            walk_expr(tp, iter, f);
            if let Some(flt) = filter {
                walk_expr(tp, flt, f);
            }
            walk_expr(tp, body, f);
        }
    }
}
