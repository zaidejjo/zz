//! Closure capture analysis: free-variable computation for env capture.
//!
//! The AOT backend lowers closures to static C functions with an explicit
//! environment (array of shared heap cells). To build that environment it
//! needs, per closure literal, the list of variables the body references
//! that are bound *outside* the closure (function params, locals, or outer
//! closure captures) — and per function body, the union of all such names
//! so captured bindings can be heap-cell-allocated from their declaration.
//!
//! Module-level bindings are excluded everywhere: after loader namespacing
//! they lower to C globals, which every function can read directly.

use std::collections::HashSet;

use zz_frontend::ast::{Block, Expr, Pattern, Stmt};

/// Lexical walker tracking closure boundaries.
///
/// `scopes` is the lexical scope stack (innermost last). `bounds` holds
/// `scopes.len()` at each enclosing closure-literal entry (innermost last).
/// A use of a name bound at scope depth `b` with innermost boundary `c` is
/// a capture exactly when `b < c`: the binding lives outside the current
/// closure. Uses bound nowhere are reported too; callers filter those
/// against the live scope (plain function names, namespaces).
struct Walk<'a> {
    scopes: Vec<HashSet<String>>,
    bounds: Vec<usize>,
    globals: &'a HashSet<String>,
    out: HashSet<String>,
}

impl<'a> Walk<'a> {
    fn use_one(&mut self, name: &str) {
        if self.globals.contains(name) {
            return;
        }
        match self.scopes.iter().rposition(|s| s.contains(name)) {
            None => {
                self.out.insert(name.to_string());
            }
            Some(b) => {
                if self.bounds.last().is_some_and(|c| b < *c) {
                    self.out.insert(name.to_string());
                }
            }
        }
    }

    fn use_path(&mut self, parts: &[String]) {
        if parts.is_empty() {
            return;
        }
        if self.globals.contains(&parts.join(".")) {
            return;
        }
        let base = parts[0].clone();
        self.use_one(&base);
    }

    fn bind(&mut self, name: &str) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string());
        }
    }

    fn bind_pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Binding { name } => {
                let n = name.name.clone();
                self.bind(&n);
            }
            Pattern::Variant { arg: Some(a), .. } => self.bind_pattern(a),
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                for p in pats {
                    self.bind_pattern(p);
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, block: &Block) {
        self.scopes.push(HashSet::new());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                self.expr(value);
                let n = name.name.clone();
                self.bind(&n);
            }
            Stmt::Func {
                name, params, body, ..
            } => {
                // Defaults evaluate at call sites — walk in the outer scope.
                for p in params {
                    if let Some(d) = &p.default {
                        self.expr(d);
                    }
                }
                // Bind the function name outside so recursion is not a capture.
                let full = name.join(".");
                self.bind(&full);
                if let Some(first) = name.first() {
                    let f = first.clone();
                    self.bind(&f);
                }
                self.scopes.push(
                    params
                        .iter()
                        .map(|p| p.name.name.clone())
                        .collect::<HashSet<_>>(),
                );
                for s in &body.stmts {
                    self.stmt(s);
                }
                self.scopes.pop();
            }
            Stmt::Struct { .. }
            | Stmt::Import { .. }
            | Stmt::ExternBlock { .. }
            | Stmt::Link { .. } => {}
            Stmt::Impl { methods, .. } => {
                for m in methods {
                    self.stmt(m);
                }
            }
            Stmt::For {
                vars, iter, body, ..
            } => {
                self.expr(iter);
                self.scopes
                    .push(vars.iter().map(|v| v.name.clone()).collect());
                for s in &body.stmts {
                    self.stmt(s);
                }
                self.scopes.pop();
            }
            Stmt::Assign { target, value, .. } => {
                // The target is a use too: `count = count + 1` captures `count`.
                self.expr(target);
                self.expr(value);
            }
            Stmt::Destructure { pat, value, .. } => {
                self.expr(value);
                self.bind_pattern(pat);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            Stmt::Defer { expr, .. } => self.expr(expr),
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Expr(e) => self.expr(e),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident { name, .. } => self.use_one(name),
            Expr::Path { parts, .. } => self.use_path(parts),
            Expr::Paren { expr: inner, .. } | Expr::Unary { expr: inner, .. } => {
                self.expr(inner);
            }
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                // Callee identifiers are uses: `f(5)` must capture
                // closure-typed `f`.
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
                for (_, v) in named {
                    self.expr(v);
                }
            }
            Expr::Closure { params, body, .. } => {
                for p in params {
                    if let Some(d) = &p.default {
                        self.expr(d);
                    }
                }
                self.bounds.push(self.scopes.len());
                self.scopes.push(
                    params
                        .iter()
                        .map(|p| p.name.name.clone())
                        .collect::<HashSet<_>>(),
                );
                self.expr(body);
                self.scopes.pop();
                self.bounds.pop();
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(e) = els {
                    self.expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.scopes.push(HashSet::new());
                    self.bind_pattern(&arm.pat);
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.expr(value);
                self.scopes.push(HashSet::new());
                self.bind_pattern(pat);
                self.block(then);
                self.scopes.pop();
                if let Some(e) = els {
                    self.expr(e);
                }
            }
            Expr::Try { expr: inner, .. } => self.expr(inner),
            Expr::Block(b) => self.block(b),
            Expr::Variant { arg, .. } => {
                if let Some(a) = arg {
                    self.expr(a);
                }
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.expr(e);
                }
            }
            Expr::Tuple { items, .. } => {
                for e in items {
                    self.expr(e);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let zz_frontend::ast::FmtPart::Expr(e, _) = part {
                        self.expr(e);
                    }
                }
            }
            Expr::Field { obj, .. } => self.expr(obj),
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            Expr::Index { obj, index, .. } => {
                self.expr(obj);
                self.expr(index);
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.expr(obj);
                if let Some(s) = start {
                    self.expr(s);
                }
                if let Some(e) = end {
                    self.expr(e);
                }
            }
            Expr::StructInit { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v);
                }
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                self.expr(iter);
                self.scopes.push(HashSet::from([var.name.clone()]));
                self.expr(body);
                if let Some(f) = filter {
                    self.expr(f);
                }
                self.scopes.pop();
            }
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {}
        }
    }
}

/// Union of names captured by any closure nested in a function body.
///
/// `params` are the function body's bound parameter names. Callers use this
/// to decide which bindings need shared heap cells.
pub fn captured_in_block(
    params: &[String],
    block: &Block,
    globals: &HashSet<String>,
) -> HashSet<String> {
    let mut w = Walk {
        scopes: vec![params.iter().cloned().collect::<HashSet<_>>()],
        bounds: Vec::new(),
        globals,
        out: HashSet::new(),
    };
    for stmt in &block.stmts {
        w.stmt(stmt);
    }
    w.out
}

/// Union of names captured by any closure nested in a single expression
/// (for closure bodies that are not blocks, e.g. `|x| (|y| x + y)`).
pub fn captured_in_expr(
    params: &[String],
    body: &Expr,
    globals: &HashSet<String>,
) -> HashSet<String> {
    let mut w = Walk {
        scopes: vec![params.iter().cloned().collect::<HashSet<_>>()],
        bounds: Vec::new(),
        globals,
        out: HashSet::new(),
    };
    w.expr(body);
    w.out
}

/// Free variables of a single closure body: names used in `body` that are
/// bound by neither the closure's own `params` (nor any nested scope inside
/// the body) nor a module global.
///
/// Sorted for deterministic environment layout. The caller filters this
/// against the live scope at the creation site: names that resolve to a
/// local/param/capture become environment cells, anything else (plain
/// function names, namespaces) is skipped.
pub fn closure_free_vars(params: &[String], body: &Expr, globals: &HashSet<String>) -> Vec<String> {
    let mut w = Walk {
        scopes: vec![params.iter().cloned().collect::<HashSet<_>>()],
        // Pretend an enclosing closure boundary at depth 0 so every use
        // bound outside the closure's own scope is reported.
        bounds: vec![0],
        globals,
        out: HashSet::new(),
    };
    w.expr(body);
    let mut vars: Vec<String> = w.out.into_iter().collect();
    vars.sort();
    vars
}
