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
use zz_span_ast::ast::{Expr, Program, Stmt, Ty, TyKind};
use zz_span_ast::span::Span;
use zz_span_ast::visitor::{AstVisitor, VisitControl};

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
    /// Short names of imported std modules (`import std.fs` → `"fs"`),
    /// used to recognize that `fs.delete` references an already-imported
    /// module even though the source writes the bare head `fs`.
    imported_heads: HashSet<String>,
    seen: HashSet<String>,
    diags: Vec<RawDiag>,
}

impl Lint {
    fn check_use(&mut self, segments: &[String], span: Span) {
        if segments.len() < 2 || segments[0] != "std" {
            return;
        }
        // A bare module head that was already imported (e.g. `fs` from
        // `import std.fs`) covers the use — no import hint needed. The
        // member-name problem, if any, is reported by the scope checker.
        if segments.len() >= 2 && self.imported_heads.contains(&segments[0]) {
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
}

impl AstVisitor for Lint {
    fn visit_expr(&mut self, e: &Expr) -> VisitControl {
        match e {
            Expr::Path { parts, span } => {
                self.check_use(parts, *span);
            }
            Expr::StructInit { name, span, .. } => {
                self.check_dotted(name, *span);
            }
            _ => {}
        }
        VisitControl::Recurse
    }

    fn visit_stmt(&mut self, s: &Stmt) -> VisitControl {
        // Imports declare coverage; never linted themselves (the
        // `import std` bare form errors elsewhere).
        if matches!(s, Stmt::Import { .. }) {
            return VisitControl::Skip;
        }
        VisitControl::Recurse
    }

    fn visit_ty(&mut self, ty: &Ty) -> VisitControl {
        if let TyKind::Named(name, _) = &ty.kind {
            if name.starts_with("std.") {
                let segments: Vec<String> = name.split('.').map(str::to_string).collect();
                self.check_use(&segments, ty.span);
            }
        }
        VisitControl::Recurse
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
        imported_heads: imported_std_paths(program)
            .iter()
            .filter_map(|p| p.last().cloned())
            .collect(),
        seen: HashSet::new(),
        diags: Vec::new(),
    };
    lint.walk_program(program);
    lint.diags
}
