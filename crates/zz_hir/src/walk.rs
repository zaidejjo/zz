//! Typed AST traversal helpers.
//!
//! `walk` provides a structural visitor over the `Program` that yields each
//! expression paired with its resolved type (looked up from
//! [`TypedProgram::types`]). Used by the call-graph analyzer (Phase 2) and
//! the C lowering pass (Phase 3) without duplicating the AST-shape logic.

use crate::{Expr, Stmt, TypedProgram, TOP_SCOPE};

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

/// Visit a single statement's expressions (scope-aware entry).
pub fn walk_stmt_scoped<'a>(
    tp: &'a TypedProgram,
    scope: &str,
    stmt: &'a Stmt,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    walk_stmt_in(tp, scope, stmt, f);
}

/// Visit a single statement's expressions (top scope; prefer
/// [`walk_stmt_scoped`] when the enclosing item is known).
pub fn walk_stmt<'a>(
    tp: &'a TypedProgram,
    stmt: &'a Stmt,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    walk_stmt_in(tp, TOP_SCOPE, stmt, f);
}

fn walk_stmt_in<'a>(
    tp: &'a TypedProgram,
    scope: &str,
    stmt: &'a Stmt,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    match stmt {
        Stmt::Decl { value, .. } => {
            walk_expr_in(tp, scope, value, f);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                walk_expr_in(tp, scope, v, f);
            }
        }
        Stmt::Func { name, body, .. } => {
            let fname = name.join(".");
            for s in &body.stmts {
                walk_stmt_in(tp, &fname, s, f);
            }
        }
        Stmt::Struct { .. } | Stmt::TypeAlias { .. } | Stmt::Enum { .. } | Stmt::Import { .. } => {}
        Stmt::ExternBlock { .. } | Stmt::Link { .. } => {}
        Stmt::Impl { name, methods, .. } => {
            let tname = name.join(".");
            for m in methods {
                if let Stmt::Func { name: mname, .. } = m {
                    let fname = format!("{tname}.{}", mname.join("."));
                    walk_stmt_in(tp, &fname, m, f);
                } else {
                    walk_stmt_in(tp, scope, m, f);
                }
            }
        }
        Stmt::For { iter, body, .. } => {
            walk_expr_in(tp, scope, iter, f);
            for s in &body.stmts {
                walk_stmt_in(tp, scope, s, f);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Defer { expr, .. } => {
            walk_expr_in(tp, scope, expr, f);
        }
        Stmt::Assign { target, value, .. } => {
            walk_expr_in(tp, scope, target, f);
            walk_expr_in(tp, scope, value, f);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            walk_expr_in(tp, scope, target, f);
            walk_expr_in(tp, scope, value, f);
        }
        Stmt::Destructure { value, .. } => {
            walk_expr_in(tp, scope, value, f);
        }
        Stmt::Expr(e) => walk_expr_in(tp, scope, e, f),
    }
}

/// Visit one expression subtree (top scope; inline the `_in` variant when
/// the enclosing item is known).
pub fn walk_expr<'a>(
    tp: &'a TypedProgram,
    e: &'a Expr,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    walk_expr_in(tp, TOP_SCOPE, e, f);
}

fn walk_expr_in<'a>(
    tp: &'a TypedProgram,
    scope: &str,
    e: &'a Expr,
    f: &mut impl FnMut(&TypedExpr<'a>) -> bool,
) {
    let te = TypedExpr {
        expr: e,
        ty: tp.type_at(scope, e.span()),
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
        | Expr::Path { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_frontend::ast::FmtPart::Expr(inner, _) = p {
                    walk_expr_in(tp, scope, inner, f);
                }
            }
        }
        Expr::Paren { expr, .. } => walk_expr_in(tp, scope, expr, f),
        Expr::Tuple { items, .. } => {
            for it in items {
                walk_expr_in(tp, scope, it, f);
            }
        }
        Expr::Unary { expr, .. } => walk_expr_in(tp, scope, expr, f),
        Expr::Binary { left, right, .. } => {
            walk_expr_in(tp, scope, left, f);
            walk_expr_in(tp, scope, right, f);
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            walk_expr_in(tp, scope, callee, f);
            for a in args {
                walk_expr_in(tp, scope, a, f);
            }
            for (_, a) in named {
                walk_expr_in(tp, scope, a, f);
            }
        }
        Expr::Closure { body, .. } => walk_expr_in(tp, scope, body, f),
        Expr::If {
            cond, then, els, ..
        } => {
            walk_expr_in(tp, scope, cond, f);
            for s in &then.stmts {
                walk_stmt_in(tp, scope, s, f);
            }
            if let Some(el) = els {
                walk_expr_in(tp, scope, el, f);
            }
        }
        Expr::While { cond, body, .. } => {
            walk_expr_in(tp, scope, cond, f);
            for s in &body.stmts {
                walk_stmt_in(tp, scope, s, f);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr_in(tp, scope, scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr_in(tp, scope, g, f);
                }
                walk_expr(tp, &arm.body, f);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            walk_expr_in(tp, scope, value, f);
            for s in &then.stmts {
                walk_stmt_in(tp, scope, s, f);
            }
            if let Some(el) = els {
                walk_expr_in(tp, scope, el, f);
            }
        }
        Expr::Try { expr, .. } => walk_expr_in(tp, scope, expr, f),
        Expr::Block(b) => {
            for s in &b.stmts {
                walk_stmt_in(tp, scope, s, f);
            }
        }
        Expr::Variant { arg, .. } => {
            if let Some(a) = arg {
                walk_expr_in(tp, scope, a, f);
            }
        }
        Expr::Array { elems, .. } => {
            for el in elems {
                walk_expr_in(tp, scope, el, f);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                walk_expr_in(tp, scope, k, f);
                walk_expr_in(tp, scope, v, f);
            }
        }
        Expr::Field { obj, .. } => walk_expr_in(tp, scope, obj, f),
        Expr::Range { start, end, .. } => {
            walk_expr_in(tp, scope, start, f);
            walk_expr_in(tp, scope, end, f);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                walk_expr_in(tp, scope, v, f);
            }
        }
        Expr::Index { obj, index, .. } => {
            walk_expr_in(tp, scope, obj, f);
            walk_expr_in(tp, scope, index, f);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            walk_expr_in(tp, scope, obj, f);
            if let Some(s) = start {
                walk_expr_in(tp, scope, s, f);
            }
            if let Some(e) = end {
                walk_expr_in(tp, scope, e, f);
            }
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            walk_expr_in(tp, scope, iter, f);
            if let Some(flt) = filter {
                walk_expr_in(tp, scope, flt, f);
            }
            walk_expr_in(tp, scope, body, f);
        }
    }
}
