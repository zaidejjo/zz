//! Dead-mutation lint for value-typed parameters (#278).
//!
//! Arrays, tuples, and structs are values on both engines (the VM
//! deep-copies across bindings; AOT detaches shared buffers on write),
//! so storing through such a parameter without ever returning (or
//! otherwise observing) it is dead code — and the kind of bug only a
//! differential test catches. This lint warns once per parameter at the
//! first dead store.
//!
//! Conservative by design (a lint must never cry wolf):
//! - only parameters (never locals — scratch buffers are idiomatic),
//! - only value types (Array, Tuple, Struct of any boxing; Dict, Bytes,
//!   and opaque handles are excluded pending a semantics verdict),
//! - a store counts only through Index/Field/Path targets rooted at the
//!   parameter (plain rebinding is ordinary assignment),
//! - ANY other mention of the parameter (returned, passed to a call,
//!   copied into a binding, captured, used in a condition…) counts as
//!   observed and silences the lint, regardless of order,
//! - shadowing bindings hide the parameter correctly via scope tracking.

use std::collections::HashMap;

use zz_diag_core::RawDiag;
use zz_span_ast::ast::{Block, Expr, FmtPart, Pattern, Stmt};
use zz_span_ast::span::Span;
use zz_types::Type;

/// Value types whose in-place mutation a caller can never observe.
fn is_value_type(ty: &zz_types::Type) -> bool {
    matches!(ty, Type::Array(_) | Type::Tuple(_) | Type::Struct(_, _))
}

struct Walk {
    /// Innermost-first scope stack; every bound name maps to whether it
    /// is a watched parameter (shadowing bindings map to false).
    scopes: Vec<HashMap<String, bool>>,
    /// Params under observation: name → (first dead-store span, observed).
    params: HashMap<String, (Option<Span>, bool)>,
}

impl Walk {
    fn new(params: &[(String, zz_types::Type)]) -> Self {
        let mut root = HashMap::new();
        let mut watched = HashMap::new();
        for (name, ty) in params {
            if is_value_type(ty) {
                root.insert(name.clone(), true);
                watched.insert(name.clone(), (None, false));
            } else {
                root.insert(name.clone(), false);
            }
        }
        Walk {
            scopes: vec![root],
            params: watched,
        }
    }

    /// True when `name` resolves to a watched parameter (through any
    /// number of shadowing scopes that did NOT rebind it).
    fn is_param(&self, name: &str) -> bool {
        for scope in self.scopes.iter().rev() {
            if let Some(watched) = scope.get(name) {
                return *watched;
            }
        }
        false
    }

    fn mark_mutated(&mut self, name: &str, span: Span) {
        if let Some((slot, _)) = self.params.get_mut(name) {
            if slot.is_none() {
                *slot = Some(span);
            }
        }
    }

    fn mark_used(&mut self, name: &str) {
        if let Some((_, used)) = self.params.get_mut(name) {
            *used = true;
        }
    }

    /// Head of a value position: marks use when it resolves to a param.
    fn use_head(&mut self, head: &str) {
        if self.is_param(head) {
            self.mark_used(head);
        }
    }

    fn walk_expr_use(&mut self, e: &Expr) {
        match e {
            Expr::Ident { name, .. } => {
                self.use_head(name);
            }
            Expr::Path { parts, .. } => {
                if let Some(head) = parts.first() {
                    self.use_head(head);
                }
            }
            Expr::Paren { expr, .. } | Expr::Unary { expr, .. } | Expr::Try { expr, .. } => {
                self.walk_expr_use(expr)
            }
            Expr::Binary { left, right, .. } => {
                self.walk_expr_use(left);
                self.walk_expr_use(right);
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                self.walk_expr_use(callee);
                for a in args {
                    self.walk_expr_use(a);
                }
                for (_, v) in named {
                    self.walk_expr_use(v);
                }
            }
            Expr::Closure { params, body, .. } => {
                self.push();
                for p in params {
                    self.define(&p.name.name, false);
                }
                self.walk_expr_use(body);
                self.pop();
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.walk_expr_use(cond);
                self.walk_block(then);
                if let Some(x) = els {
                    self.walk_expr_use(x);
                }
            }
            Expr::While { cond, body, .. } => {
                self.walk_expr_use(cond);
                self.walk_block(body);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.walk_expr_use(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.walk_expr_use(g);
                    }
                    self.walk_expr_use(&arm.body);
                }
            }
            Expr::IfLet {
                value, then, els, ..
            } => {
                self.walk_expr_use(value);
                self.walk_block(then);
                if let Some(x) = els {
                    self.walk_expr_use(x);
                }
            }
            Expr::Block(b) => self.walk_block(b),
            Expr::Variant { arg, .. } => {
                if let Some(a) = arg {
                    self.walk_expr_use(a);
                }
            }
            Expr::Array { elems, .. } | Expr::Tuple { items: elems, .. } => {
                for x in elems {
                    self.walk_expr_use(x);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.walk_expr_use(k);
                    self.walk_expr_use(v);
                }
            }
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let FmtPart::Expr(x, _) = part {
                        self.walk_expr_use(x);
                    }
                }
            }
            Expr::Field { obj, .. } => self.walk_expr_use(obj),
            Expr::Range { start, end, .. } => {
                self.walk_expr_use(start);
                self.walk_expr_use(end);
            }
            Expr::Index { obj, index, .. } => {
                self.walk_expr_use(obj);
                self.walk_expr_use(index);
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.walk_expr_use(obj);
                if let Some(s) = start {
                    self.walk_expr_use(s);
                }
                if let Some(x) = end {
                    self.walk_expr_use(x);
                }
            }
            Expr::StructInit { fields, .. } => {
                for (_, v) in fields {
                    self.walk_expr_use(v);
                }
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                self.walk_expr_use(iter);
                self.push();
                self.define(&var.name, false);
                self.walk_expr_use(body);
                if let Some(f) = filter {
                    self.walk_expr_use(f);
                }
                self.pop();
            }
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {}
        }
    }

    /// A store through `target` mutates caller-visible state only via the
    /// root binding: record a dead-store candidate when it resolves to a
    /// watched parameter. Plain `Ident` targets rebind (ordinary
    /// assignment) and are not stores.
    fn walk_store_target(&mut self, target: &Expr) {
        let head: Option<&str> = match target {
            Expr::Ident { .. } => None,
            Expr::Path { parts, .. } => parts.first().map(String::as_str),
            Expr::Index { obj, .. } | Expr::Field { obj, .. } => match obj.as_ref() {
                Expr::Ident { name, .. } => Some(name.as_str()),
                Expr::Path { parts, .. } => parts.first().map(String::as_str),
                _ => {
                    self.walk_expr_use(obj);
                    None
                }
            },
            _ => {
                self.walk_expr_use(target);
                None
            }
        };
        if let Some(h) = head {
            if self.is_param(h) {
                self.mark_mutated(h, target.span());
            }
        }
    }

    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str, watched: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), watched);
        }
    }

    fn walk_block(&mut self, b: &Block) {
        self.push();
        for s in &b.stmts {
            self.walk_stmt(s);
        }
        self.pop();
    }

    fn walk_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Decl { name, value, .. } => {
                self.walk_expr_use(value);
                self.define(&name.name, false);
            }
            Stmt::Func { params, body, .. } => {
                // Nested definition: its references observe.
                self.push();
                for p in params {
                    self.define(&p.name.name, false);
                }
                self.walk_block(body);
                self.pop();
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr_use(v);
                }
            }
            Stmt::Expr(e) => self.walk_expr_use(e),
            Stmt::Assign { target, value, .. } | Stmt::CompoundAssign { target, value, .. } => {
                self.walk_store_target(target);
                self.walk_expr_use(value);
            }
            Stmt::Destructure { pat, value, .. } => {
                self.walk_expr_use(value);
                declare_pattern(pat, &mut |n| self.define(n, false));
            }
            Stmt::For {
                vars, iter, body, ..
            } => {
                self.walk_expr_use(iter);
                self.push();
                for v in vars {
                    self.define(&v.name, false);
                }
                self.walk_block(body);
                self.pop();
            }
            Stmt::Defer { expr, .. } => self.walk_expr_use(expr),
            Stmt::Import { .. }
            | Stmt::Struct { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Enum { .. }
            | Stmt::Impl { .. }
            | Stmt::ExternBlock { .. }
            | Stmt::Link { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => {}
        }
    }

    fn finish(self) -> Vec<(String, Span)> {
        let mut out = Vec::new();
        for (name, (mutated, used)) in self.params {
            if !used {
                if let Some(span) = mutated {
                    out.push((name, span));
                }
            }
        }
        out
    }
}

fn declare_pattern(pat: &Pattern, declare: &mut impl FnMut(&str)) {
    match pat {
        Pattern::Binding { name } => declare(&name.name),
        Pattern::Variant { arg: Some(a), .. } => declare_pattern(a, declare),
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for p in pats {
                declare_pattern(p, declare);
            }
        }
        _ => {}
    }
}

/// Lint one checked function body: warn for each value-typed parameter
/// stored through but never otherwise observed.
pub fn lint_dead_param_mutation(
    params: &[(String, zz_types::Type)],
    body: &Block,
    errors: &mut Vec<RawDiag>,
) {
    let mut walk = Walk::new(params);
    walk.push();
    for s in &body.stmts {
        walk.walk_stmt(s);
    }
    walk.pop();
    for (name, span) in walk.finish() {
        errors.push(
            zz_diag_core::warning_at(
                format!(
                    "mutating parameter `{name}` which is never returned — the mutation is lost (parameters are values)"
                ),
                span,
            )
            .with_note(
                "return the parameter (or a value derived from it), or remove the mutation",
            ),
        );
    }
}
