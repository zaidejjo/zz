use std::collections::HashSet;

use zz_frontend::ast::{Block, Expr, FmtPart, Pattern, Stmt, Ty, TyKind};

pub(crate) struct Rewriter<'a> {
    pub(crate) ns: &'a str,
    pub(crate) top: &'a HashSet<String>,
    /// Stack of shadowing scopes; each holds names declared so far.
    pub(crate) scopes: Vec<HashSet<String>>,
}

impl<'a> Rewriter<'a> {
    pub(crate) fn new(ns: &'a str, top: &'a HashSet<String>) -> Self {
        Rewriter {
            ns,
            top,
            scopes: vec![HashSet::new()],
        }
    }

    pub(crate) fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn declare(&mut self, name: &str) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string());
        }
    }

    pub(crate) fn rewrite_stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Decl {
                name, ty, value, ..
            } => {
                if self.top.contains(&name.name) {
                    name.name = format!("{}.{}", self.ns, name.name);
                }
                if let Some(t) = ty {
                    self.rewrite_ty(t);
                }
                self.rewrite_expr(value);
                self.declare(&name.name);
            }
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                body,
                ..
            } => {
                let is_top = self.top.contains(&name.join("."));
                if is_top && name[0] != self.ns {
                    // Only prefix if first component doesn't already match namespace.
                    name[0] = format!("{}.{}", self.ns, name[0]);
                }
                self.push_scope();
                if !is_top {
                    // A nested func shadows its own name within its body.
                    self.declare(&name.join("."));
                }
                for g in generics {
                    self.declare(&g.name);
                }
                for p in params {
                    self.declare(&p.name.name);
                    if let Some(t) = &mut p.ty {
                        self.rewrite_ty(t);
                    }
                }
                if let Some(t) = ret {
                    self.rewrite_ty(t);
                }
                self.rewrite_block(body);
                self.pop_scope();
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.rewrite_expr(v);
                }
            }
            Stmt::Struct { name, fields, .. } => {
                if self.top.contains(&name.join(".")) && name[0] != self.ns {
                    name[0] = format!("{}.{}", self.ns, name[0]);
                }
                for (_, fty) in fields {
                    self.rewrite_ty(fty);
                }
            }
            Stmt::For {
                var, iter, body, ..
            } => {
                self.rewrite_expr(iter);
                self.push_scope();
                self.declare(&var.name);
                self.rewrite_block(body);
                self.pop_scope();
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Defer { expr, .. } => {
                self.rewrite_expr(expr);
            }
            Stmt::Assign { target, value, .. } => {
                self.rewrite_expr(target);
                self.rewrite_expr(value);
            }
            Stmt::Expr(e) => self.rewrite_expr(e),
            Stmt::Import { .. } => {}
        }
    }

    pub(crate) fn rewrite_block(&mut self, block: &mut Block) {
        self.push_scope();
        for stmt in &mut block.stmts {
            self.rewrite_stmt(stmt);
        }
        self.pop_scope();
    }

    /// Rewrite type annotations: struct names defined in this module become
    /// namespaced (`Point` → `shapes.Point`).
    pub(crate) fn rewrite_ty(&mut self, ty: &mut Ty) {
        match &mut ty.kind {
            TyKind::Named(name, args) => {
                if self.top.contains(name)
                    && !self.is_shadowed(name)
                    && !name.starts_with(&format!("{}.", self.ns))
                {
                    *name = format!("{}.{}", self.ns, name);
                }
                for a in args {
                    self.rewrite_ty(a);
                }
            }
            TyKind::Tuple(ts) => {
                for t in ts {
                    self.rewrite_ty(t);
                }
            }
            TyKind::Option(t) | TyKind::Array(t) => self.rewrite_ty(t),
            TyKind::Result(a, b) => {
                self.rewrite_ty(a);
                self.rewrite_ty(b);
            }
            TyKind::Func(ps, r) => {
                for p in ps {
                    self.rewrite_ty(p);
                }
                self.rewrite_ty(r);
            }
            TyKind::Dict(k, v) => {
                self.rewrite_ty(k);
                self.rewrite_ty(v);
            }
            TyKind::Union(ts) => {
                for t in ts {
                    self.rewrite_ty(t);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn rewrite_expr(&mut self, expr: &mut Expr) {
        match expr {
            Expr::Ident { name, span } => {
                if self.top.contains(name) && !self.is_shadowed(name) {
                    *expr = Expr::Path {
                        parts: vec![self.ns.to_string(), name.clone()],
                        span: *span,
                    };
                }
            }
            Expr::Paren { expr: inner, .. } => self.rewrite_expr(inner),
            Expr::Unary { expr: inner, .. } => self.rewrite_expr(inner),
            Expr::Binary { left, right, .. } => {
                self.rewrite_expr(left);
                self.rewrite_expr(right);
            }
            Expr::Call { callee, args, .. } => {
                // Method call: `p.dist()` — the method name is the last path
                // component; qualify it like a bare function reference so the
                // checker/runtime can resolve `ns.dist`.
                if let Expr::Path { parts, .. } = callee.as_mut() {
                    if parts.len() >= 2 {
                        if let Some(last) = parts.last_mut() {
                            if self.top.contains(last) && !self.is_shadowed(last) {
                                *last = format!("{}.{}", self.ns, last);
                            }
                        }
                    }
                }
                self.rewrite_expr(callee);
                for a in args {
                    self.rewrite_expr(a);
                }
            }
            Expr::Closure { params, body, .. } => {
                self.push_scope();
                for p in params {
                    self.declare(&p.name.name);
                    if let Some(t) = &mut p.ty {
                        self.rewrite_ty(t);
                    }
                }
                self.rewrite_expr(body);
                self.pop_scope();
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.rewrite_expr(cond);
                self.rewrite_block(then);
                if let Some(e) = els {
                    self.rewrite_expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.rewrite_expr(cond);
                self.rewrite_block(body);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.rewrite_expr(scrutinee);
                for arm in arms {
                    self.push_scope();
                    if let Pattern::Binding { name } = &arm.pat {
                        self.declare(&name.name);
                    }
                    self.rewrite_expr(&mut arm.body);
                    self.pop_scope();
                }
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.rewrite_expr(value);
                self.push_scope();
                if let Pattern::Binding { name } = pat {
                    self.declare(&name.name);
                }
                self.rewrite_block(then);
                if let Some(e) = els {
                    self.rewrite_expr(e);
                }
                self.pop_scope();
            }
            Expr::Try { expr: inner, .. } => self.rewrite_expr(inner),
            Expr::Block(b) => self.rewrite_block(b),
            Expr::Variant { arg, .. } => {
                if let Some(a) = arg {
                    self.rewrite_expr(a);
                }
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.rewrite_expr(e);
                }
            }
            Expr::ListComp {
                body,
                var: _,
                iter,
                filter,
                ..
            } => {
                self.rewrite_expr(iter);
                self.rewrite_expr(body);
                if let Some(f) = filter {
                    self.rewrite_expr(f);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.rewrite_expr(k);
                    self.rewrite_expr(v);
                }
            }
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let FmtPart::Expr(e, _) = part {
                        self.rewrite_expr(e);
                    }
                }
            }
            Expr::Field { obj, .. } => self.rewrite_expr(obj),
            Expr::Range { start, end, .. } => {
                self.rewrite_expr(start);
                self.rewrite_expr(end);
            }
            Expr::Index { obj, index, .. } => {
                self.rewrite_expr(obj);
                self.rewrite_expr(index);
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.rewrite_expr(obj);
                if let Some(s) = start {
                    self.rewrite_expr(s);
                }
                if let Some(e) = end {
                    self.rewrite_expr(e);
                }
            }
            Expr::StructInit { name, fields, .. } => {
                // A struct defined in this module is referenced by its
                // namespaced name; imported structs are already qualified.
                if self.top.contains(name)
                    && !self.is_shadowed(name)
                    && !name.starts_with(&format!("{}.", self.ns))
                {
                    *name = format!("{}.{}", self.ns, name);
                }
                for (_, v) in fields {
                    self.rewrite_expr(v);
                }
            }
            Expr::Path { parts, .. } => {
                // `p.x` on a top-level binding: qualify the root so the
                // struct-field walk finds `ns.p`.
                if let Some(first) = parts.first_mut() {
                    if self.top.contains(first)
                        && !self.is_shadowed(first)
                        && !first.starts_with(&format!("{}.", self.ns))
                    {
                        *first = format!("{}.{}", self.ns, first);
                    }
                }
            }
            Expr::Int { .. } | Expr::Float { .. } | Expr::Str { .. } | Expr::Bool { .. } => {}
        }
    }
}
