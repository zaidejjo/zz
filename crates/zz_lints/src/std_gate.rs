//! Import hygiene for fully-qualified std paths (#227).
//!
//! The checker seed exposes every `std.*` native unconditionally, so
//! `std.fs.read_dir(".")` type-checks (and runs) with zero imports.
//! This lint errors on any `std.<mod>...` use the module did not import,
//! pointing at the missing import. Only enforced for loader-checked user
//! modules (which always know their owner namespace); stdlib internals,
//! the REPL, and merged HIR programs pass an empty owner and skip it.

use std::collections::HashSet;

use zz_diag_core::RawDiag;
use zz_span_ast::ast::{Block, Expr, FmtPart, Program, Stmt, Ty, TyKind};
use zz_span_ast::span::Span;

/// Collect `std.*` import paths declared by the module: the full dotted
/// path (`import std.sqlz.postgres` → `[std, sqlz, postgres]`), whatever
/// the alias (any spelling of the import unlocks the canonical paths).
fn imported_std_paths(program: &Program) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for stmt in &program.stmts {
        if let Stmt::Import { path, .. } = stmt {
            if path.first().map(String::as_str) == Some("std") && path.len() >= 2 {
                out.push(path.clone());
            }
        }
    }
    out
}

/// True when a `std.*` use is covered by an import: some imported std
/// path is a segment-wise prefix of the used path.
fn covered(imports: &[Vec<String>], used: &[String]) -> bool {
    imports
        .iter()
        .any(|p| p.len() <= used.len() && p.iter().zip(used.iter()).all(|(a, b)| a == b))
}

struct Lint {
    imports: Vec<Vec<String>>,
    seen: HashSet<String>,
    diags: Vec<RawDiag>,
}

impl Lint {
    fn check_use(&mut self, segments: &[String], span: Span) {
        if segments.len() < 2 || segments[0] != "std" {
            return;
        }
        if covered(&self.imports, segments) {
            return;
        }
        let joined = segments.join(".");
        if !self.seen.insert(joined.clone()) {
            return;
        }
        // Name the narrowest missing module (`std.fs` for
        // `std.fs.read_dir`): the fix is always one import.
        let module = segments
            .get(..2)
            .map(|s| s.join("."))
            .unwrap_or_else(|| "std".to_string());
        self.diags.push(
            zz_diag_core::error_at(format!("`{joined}` requires `import {module}`"), span)
                .with_note(format!(
                    "add `import {module}` at the top of the file to use `{joined}`"
                )),
        );
    }

    /// Split a dotted type/ctor name (`tkeys.Canvas`, `std.fs.Dir`) into
    /// segments for the prefix check.
    fn check_dotted(&mut self, name: &str, span: Span) {
        if !name.starts_with("std.") {
            return;
        }
        let segments: Vec<String> = name.split('.').map(str::to_string).collect();
        self.check_use(&segments, span);
    }

    fn walk_ty(&mut self, ty: &Ty) {
        match &ty.kind {
            TyKind::Named(name, args) => {
                self.check_dotted(name, ty.span);
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

    fn walk_expr(&mut self, e: &Expr) {
        match e {
            Expr::Path { parts, span } => {
                self.check_use(parts, *span);
            }
            Expr::StructInit { name, fields, span } => {
                self.check_dotted(name, *span);
                for (_, v) in fields {
                    self.walk_expr(v);
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
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let FmtPart::Expr(x, _) = part {
                        self.walk_expr(x);
                    }
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
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Ident { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {}
        }
    }

    fn walk_block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.walk_stmt(s);
        }
    }

    fn walk_stmt(&mut self, s: &Stmt) {
        match s {
            // Imports declare coverage; never linted themselves (the
            // `import std` bare form errors elsewhere).
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
}

/// Lint a loader-checked user module for fully-qualified std uses without
/// the matching import (#227). No-op when `module_ns` is empty (stdlib
/// internals, REPL, merged programs). Returns diagnostics to merge into
/// the check result.
pub fn lint_std_imports(program: &Program, module_ns: &str) -> Vec<RawDiag> {
    if module_ns.is_empty() {
        return Vec::new();
    }
    let mut lint = Lint {
        imports: imported_std_paths(program),
        seen: HashSet::new(),
        diags: Vec::new(),
    };
    for s in &program.stmts {
        lint.walk_stmt(s);
    }
    lint.diags
}
