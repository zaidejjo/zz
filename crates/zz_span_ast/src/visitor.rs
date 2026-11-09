//! Shared AST visitor trait.
//!
//! Provides default structural walk implementations over the AST so that
//! multiple lints/checkers can share traversal logic instead of each
//! re-implementing `walk_expr`/`walk_stmt`/`walk_block`/`walk_ty`.

use crate::ast::{Block, Expr, FmtPart, Program, Stmt, Ty, TyKind};

/// Control flow for the visitor: continue descending into children or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitControl {
    /// Continue descending into the node's children.
    Recurse,
    /// Skip this node's children (but continue with siblings).
    Skip,
    /// Stop the entire traversal.
    Abort,
}

/// A visitor over the AST. Implementors override `visit_expr`, `visit_stmt`,
/// or `visit_ty` to add custom logic at specific node types before the
/// default descent happens.
///
/// The default `walk_*` methods call the corresponding `visit_*` hook,
/// then recurse into children if the hook returns `VisitControl::Recurse`.
pub trait AstVisitor {
    /// Hook called when visiting an expression, before descending into
    /// children. Return `VisitControl::Recurse` to descend, `Skip` to skip
    /// children, `Abort` to stop traversal entirely.
    fn visit_expr(&mut self, _e: &Expr) -> VisitControl {
        VisitControl::Recurse
    }

    /// Hook called when visiting a statement, before descending into
    /// children.
    fn visit_stmt(&mut self, _s: &Stmt) -> VisitControl {
        VisitControl::Recurse
    }

    /// Hook called when visiting a block, before descending into its
    /// statements.
    fn visit_block(&mut self, _b: &Block) -> VisitControl {
        VisitControl::Recurse
    }

    /// Hook called when visiting a type, before descending into its
    /// component types.
    fn visit_ty(&mut self, _ty: &Ty) -> VisitControl {
        VisitControl::Recurse
    }

    /// Walk an expression subtree. Calls `visit_expr` first, then recurses
    /// into all child expressions, types, blocks, and statements unless the
    /// hook returned `Skip` or `Abort`.
    fn walk_expr(&mut self, e: &Expr) {
        match self.visit_expr(e) {
            VisitControl::Abort => return,
            VisitControl::Skip => {}
            VisitControl::Recurse => {}
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
                for part in parts {
                    if let FmtPart::Expr(x, _) = part {
                        self.walk_expr(x);
                    }
                }
            }
            Expr::Paren { expr, .. } | Expr::Unary { expr, .. } | Expr::Try { expr, .. } => {
                self.walk_expr(expr)
            }
            Expr::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                self.walk_expr(callee);
                for a in args {
                    self.walk_expr(a);
                }
                for (_, v) in named {
                    self.walk_expr(v);
                }
            }
            Expr::Closure { params, body, .. } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.walk_ty(t);
                    }
                    if let Some(d) = &p.default {
                        self.walk_expr(d);
                    }
                }
                self.walk_expr(body);
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.walk_expr(cond);
                self.walk_block(then);
                if let Some(x) = els {
                    self.walk_expr(x);
                }
            }
            Expr::While { cond, body, .. } => {
                self.walk_expr(cond);
                self.walk_block(body);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&arm.body);
                }
            }
            Expr::IfLet {
                value, then, els, ..
            } => {
                self.walk_expr(value);
                self.walk_block(then);
                if let Some(x) = els {
                    self.walk_expr(x);
                }
            }
            Expr::Block(b) => self.walk_block(b),
            Expr::Variant { arg, .. } => {
                if let Some(a) = arg {
                    self.walk_expr(a);
                }
            }
            Expr::Array { elems, .. } | Expr::Tuple { items: elems, .. } => {
                for x in elems {
                    self.walk_expr(x);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            Expr::Field { obj, .. } => self.walk_expr(obj),
            Expr::Range { start, end, .. } => {
                self.walk_expr(start);
                self.walk_expr(end);
            }
            Expr::Index { obj, index, .. } => {
                self.walk_expr(obj);
                self.walk_expr(index);
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.walk_expr(obj);
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(x) = end {
                    self.walk_expr(x);
                }
            }
            Expr::ListComp {
                body, iter, filter, ..
            } => {
                self.walk_expr(body);
                self.walk_expr(iter);
                if let Some(f) = filter {
                    self.walk_expr(f);
                }
            }
            Expr::StructInit { fields, .. } => {
                for (_, v) in fields {
                    self.walk_expr(v);
                }
            }
        }
    }

    /// Walk a statement. Calls `visit_stmt` first, then recurses into all
    /// child expressions, types, blocks, and statements unless the hook
    /// returned `Skip` or `Abort`.
    fn walk_stmt(&mut self, s: &Stmt) {
        match self.visit_stmt(s) {
            VisitControl::Abort => return,
            VisitControl::Skip => {}
            VisitControl::Recurse => {}
        }
        match s {
            Stmt::Import { .. } => {}
            Stmt::Decl { ty, value, .. } => {
                if let Some(t) = ty {
                    self.walk_ty(t);
                }
                self.walk_expr(value);
            }
            Stmt::Func {
                params, ret, body, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.walk_ty(t);
                    }
                    if let Some(d) = &p.default {
                        self.walk_expr(d);
                    }
                }
                if let Some(t) = ret {
                    self.walk_ty(t);
                }
                self.walk_block(body);
            }
            Stmt::Struct { fields, .. } => {
                for (_, t) in fields {
                    self.walk_ty(t);
                }
            }
            Stmt::TypeAlias { target, .. } => {
                self.walk_ty(target);
            }
            Stmt::Enum { variants, .. } => {
                for (_, payload) in variants {
                    if let Some(t) = payload {
                        self.walk_ty(t);
                    }
                }
            }
            Stmt::Impl { methods, .. } => {
                for m in methods {
                    self.walk_stmt(m);
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            Stmt::Expr(e) => {
                self.walk_expr(e);
            }
            Stmt::Assign { target, value, .. } | Stmt::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            Stmt::Destructure { value, .. } => {
                self.walk_expr(value);
            }
            Stmt::For { iter, body, .. } => {
                self.walk_expr(iter);
                self.walk_block(body);
            }
            Stmt::Defer { expr, .. } => {
                self.walk_expr(expr);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::ExternBlock { items, .. } => {
                for item in items {
                    for p in &item.params {
                        if let Some(t) = &p.ty {
                            self.walk_ty(t);
                        }
                    }
                    if let Some(t) = &item.ret {
                        self.walk_ty(t);
                    }
                }
            }
            Stmt::Link { .. } => {}
        }
    }

    /// Walk a block (sequence of statements). Calls `visit_block` first,
    /// then recurses into each statement unless the hook returned `Skip`
    /// or `Abort`.
    fn walk_block(&mut self, b: &Block) {
        match self.visit_block(b) {
            VisitControl::Abort => return,
            VisitControl::Skip => {}
            VisitControl::Recurse => {}
        }
        for s in &b.stmts {
            self.walk_stmt(s);
        }
    }

    /// Walk a type. Calls `visit_ty` first, then recurses into all
    /// component types unless the hook returned `Skip` or `Abort`.
    fn walk_ty(&mut self, ty: &Ty) {
        match self.visit_ty(ty) {
            VisitControl::Abort => return,
            VisitControl::Skip => {}
            VisitControl::Recurse => {}
        }
        match &ty.kind {
            TyKind::Named(_, args) => {
                for a in args {
                    self.walk_ty(a);
                }
            }
            TyKind::Ptr { inner, .. } => self.walk_ty(inner),
            TyKind::Tuple(ts) => {
                for t in ts {
                    self.walk_ty(t);
                }
            }
            TyKind::Option(t) | TyKind::Array(t) => self.walk_ty(t),
            TyKind::Result(a, b) => {
                self.walk_ty(a);
                self.walk_ty(b);
            }
            TyKind::Func(ps, r) => {
                for p in ps {
                    self.walk_ty(p);
                }
                self.walk_ty(r);
            }
            TyKind::Dict(k, v) => {
                self.walk_ty(k);
                self.walk_ty(v);
            }
            TyKind::Union(ts) => {
                for t in ts {
                    self.walk_ty(t);
                }
            }
            TyKind::Int
            | TyKind::Float
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Void => {}
        }
    }

    /// Walk every statement in a program.
    fn walk_program(&mut self, program: &Program) {
        for s in &program.stmts {
            self.walk_stmt(s);
        }
    }
}
