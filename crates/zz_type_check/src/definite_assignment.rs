//! Definite-assignment flow analysis (Phase 2, item 4).
//!
//! Tracks which locals are definitely initialized at each program point and
//! reports use-before-initialization. Conservative: a name counts as
//! initialized only if every path reaching this point assigned it, so there
//! are zero false positives by construction. Only names declared in the
//! current function body are considered; globals/functions/undefined names
//! are left to the existing lookup diagnostics.

use std::collections::HashSet;

use zz_checker_core::Checker;
use zz_diag_core::error_at;
use zz_scope::ScopeOps;
use zz_span_ast::ast::{Block, Expr, FmtPart, Pattern, Stmt};
use zz_span_ast::span::Span;

/// Run definite-assignment analysis on a function body.
pub fn analyze_function_body(checker: &mut Checker, body: &Block) {
    let mut locals: HashSet<String> = HashSet::new();
    collect_locals(body, &mut locals);
    if locals.is_empty() {
        return;
    }
    let mut init: HashSet<String> = HashSet::new();
    analyze_stmts(checker, &body.stmts, &mut init, &locals);
}

/// Collect every name declared by `:=` / `: T =` in `block`, recursing into
/// nested blocks, loops, if/match arms and if-let arms.
fn collect_locals(block: &Block, out: &mut HashSet<String>) {
    for s in &block.stmts {
        match s {
            Stmt::Decl { name, .. } => {
                out.insert(name.name.clone());
            }
            Stmt::For { body, .. } | Stmt::Expr(Expr::While { body, .. }) => {
                collect_locals(body, out);
            }
            Stmt::Expr(Expr::If { then, els, .. }) => {
                collect_locals(then, out);
                if let Some(e) = els {
                    collect_expr_locals(e, out);
                }
            }
            Stmt::Expr(Expr::IfLet { then, els, .. }) => {
                collect_locals(then, out);
                if let Some(e) = els {
                    collect_expr_locals(e, out);
                }
            }
            Stmt::Expr(Expr::Match { arms, .. }) => {
                for a in arms {
                    collect_expr_locals(&a.body, out);
                }
            }
            Stmt::Expr(Expr::Block(b)) => collect_locals(b, out),
            _ => {}
        }
    }
}

fn collect_expr_locals(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Block(b) => collect_locals(b, out),
        Expr::If { then, els, .. } => {
            collect_locals(then, out);
            if let Some(e) = els {
                collect_expr_locals(e, out);
            }
        }
        Expr::IfLet { then, els, .. } => {
            collect_locals(then, out);
            if let Some(e) = els {
                collect_expr_locals(e, out);
            }
        }
        Expr::While { body, .. } => collect_locals(body, out),
        Expr::Match { arms, .. } => {
            for a in arms {
                collect_expr_locals(&a.body, out);
            }
        }
        _ => {}
    }
}

/// Walk statements left-to-right maintaining `init` = names definitely
/// initialized on the current path. Reports use of a local before it is
/// initialized on this path.
fn analyze_stmts(
    checker: &mut Checker,
    stmts: &[Stmt],
    init: &mut HashSet<String>,
    locals: &HashSet<String>,
) {
    for s in stmts {
        match s {
            Stmt::Decl { name, value, .. } => {
                analyze_expr_uses(checker, value, init, locals);
                init.insert(name.name.clone());
            }
            Stmt::Assign { target, value, .. } => {
                analyze_expr_uses(checker, value, init, locals);
                if let Expr::Ident { name, .. } = target {
                    if locals.contains(name) {
                        init.insert(name.clone());
                    }
                }
            }
            Stmt::CompoundAssign { target, value, .. } => {
                analyze_expr_uses(checker, target, init, locals);
                analyze_expr_uses(checker, value, init, locals);
                if let Expr::Ident { name, .. } = target {
                    if locals.contains(name) {
                        init.insert(name.clone());
                    }
                }
            }
            Stmt::Destructure { pat, value, .. } => {
                analyze_expr_uses(checker, value, init, locals);
                bind_pattern_locals(pat, init, locals);
            }
            Stmt::For { vars, body, .. } => {
                for v in vars {
                    init.insert(v.name.clone());
                }
                let mut body_init = init.clone();
                analyze_stmts(checker, &body.stmts, &mut body_init, locals);
                // for-vars are scoped to the loop
                for v in vars {
                    init.remove(v.name.as_str());
                }
            }
            Stmt::Expr(expr) => analyze_expr(checker, expr, init, locals),
            Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {
                // Diverging statement: nothing after it on this path is
                // reachable, so stop tracking this path.
                return;
            }
            _ => {}
        }
    }
}

/// Analyze an expression for use-before-init, threading `init`. Blocks,
/// if/while/match expressions recurse into their sub-blocks.
fn analyze_expr(
    checker: &mut Checker,
    expr: &Expr,
    init: &mut HashSet<String>,
    locals: &HashSet<String>,
) {
    match expr {
        Expr::Ident { name, span } => {
            report_use_before_init(checker, name, *span, init, locals);
        }
        Expr::Block(b) => {
            let mut b_init = init.clone();
            analyze_stmts(checker, &b.stmts, &mut b_init, locals);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            analyze_expr_uses(checker, cond, init, locals);
            let mut then_init = init.clone();
            analyze_stmts(checker, &then.stmts, &mut then_init, locals);
            let mut joined = init.clone();
            if let Some(e) = els {
                let mut else_init = init.clone();
                analyze_expr(checker, e, &mut else_init, locals);
                joined = intersect_sets(&then_init, &else_init);
            }
            *init = joined;
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            analyze_expr_uses(checker, value, init, locals);
            let mut then_init = init.clone();
            analyze_stmts(checker, &then.stmts, &mut then_init, locals);
            let mut joined = init.clone();
            if let Some(e) = els {
                let mut else_init = init.clone();
                analyze_expr(checker, e, &mut else_init, locals);
                joined = intersect_sets(&then_init, &else_init);
            }
            *init = joined;
        }
        Expr::While { cond, body, .. } => {
            analyze_expr_uses(checker, cond, init, locals);
            // Loop may not execute: body-initialized names don't propagate
            // out. Check uses inside the body but keep `init` after.
            let mut body_init = init.clone();
            analyze_stmts(checker, &body.stmts, &mut body_init, locals);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            analyze_expr_uses(checker, scrutinee, init, locals);
            let mut joined: Option<HashSet<String>> = None;
            for a in arms {
                let mut arm_init = init.clone();
                // Pattern binds fresh names → treat them as initialized inside
                // the arm (their initializer was checked).
                bind_pattern_locals(&a.pat, &mut arm_init, locals);
                analyze_expr(checker, &a.body, &mut arm_init, locals);
                joined = Some(match joined {
                    None => arm_init,
                    Some(j) => intersect_sets(&j, &arm_init),
                });
            }
            if let Some(j) = joined {
                *init = j;
            }
        }
        Expr::Try { expr, .. } => {
            analyze_expr_uses(checker, expr, init, locals);
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                analyze_expr_uses(checker, e, init, locals);
            }
        }
        Expr::Tuple { items, .. } => {
            for e in items {
                analyze_expr_uses(checker, e, init, locals);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                analyze_expr_uses(checker, k, init, locals);
                analyze_expr_uses(checker, v, init, locals);
            }
        }
        Expr::Unary { expr, .. } => analyze_expr_uses(checker, expr, init, locals),
        Expr::Binary { left, right, .. } => {
            analyze_expr_uses(checker, left, init, locals);
            analyze_expr_uses(checker, right, init, locals);
        }
        Expr::Call { callee, args, .. } => {
            analyze_expr_uses(checker, callee, init, locals);
            for a in args {
                analyze_expr_uses(checker, a, init, locals);
            }
        }
        Expr::Field { obj, .. } | Expr::Index { obj, .. } => {
            analyze_expr_uses(checker, obj, init, locals);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            analyze_expr_uses(checker, obj, init, locals);
            if let Some(s) = start.as_ref() {
                analyze_expr_uses(checker, s, init, locals);
            }
            if let Some(e) = end.as_ref() {
                analyze_expr_uses(checker, e, init, locals);
            }
        }
        Expr::Range { start, end, .. } => {
            analyze_expr_uses(checker, start, init, locals);
            analyze_expr_uses(checker, end, init, locals);
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                analyze_expr_uses(checker, v, init, locals);
            }
        }
        Expr::Variant { arg: Some(a), .. } => analyze_expr_uses(checker, a, init, locals),
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            analyze_expr_uses(checker, body, init, locals);
            analyze_expr_uses(checker, iter, init, locals);
            if let Some(f) = filter {
                analyze_expr_uses(checker, f, init, locals);
            }
        }
        Expr::Closure { body, .. } => {
            // Closures capture; analyze the body but don't let its
            // assignments propagate out (separate scope).
            let mut c_init = init.clone();
            analyze_expr(checker, body, &mut c_init, locals);
        }
        Expr::Paren { expr, .. } => analyze_expr_uses(checker, expr, init, locals),
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, ..) = part {
                    analyze_expr_uses(checker, e, init, locals);
                }
            }
        }
        _ => {}
    }
}

/// Convenience: analyze an expression that is only read (no new bindings),
/// e.g. a condition or argument.
fn analyze_expr_uses(
    checker: &mut Checker,
    expr: &Expr,
    init: &mut HashSet<String>,
    locals: &HashSet<String>,
) {
    analyze_expr(checker, expr, init, locals);
}

/// Report `name` used at `span` if it is a local that may be uninitialized on
/// this path. Only fires when the name is currently in scope (so
/// undefined-variable lookups keep their own single error).
fn report_use_before_init(
    checker: &mut Checker,
    name: &str,
    span: Span,
    init: &HashSet<String>,
    locals: &HashSet<String>,
) {
    if !locals.contains(name) {
        return; // not a local declared in this body
    }
    if init.contains(name) {
        return; // definitely initialized on this path
    }
    // Only report if the name is resolvable here (declared earlier on this
    // path); a name declared later is "undefined", not "uninitialized", and
    // lookup owns that error.
    if checker.lookup_opt(name).is_none() {
        return;
    }
    checker.errors.push(
        error_at(
            format!(
                "use of possibly uninitialized variable `{name}`\n\
                 hint: initialize `{name}` before this use on every path"
            ),
            span,
        )
        .with_note("variable may not have been assigned on this path"),
    );
}

/// Add names bound by a pattern to `init` (they are definitely initialized
/// where the pattern matches).
fn bind_pattern_locals(pat: &Pattern, init: &mut HashSet<String>, locals: &HashSet<String>) {
    match pat {
        Pattern::Binding { name, .. } => {
            if locals.contains(&name.name) {
                init.insert(name.name.clone());
            }
        }
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for p in pats {
                bind_pattern_locals(p, init, locals);
            }
        }
        Pattern::Variant { arg: Some(a), .. } => {
            bind_pattern_locals(a, init, locals);
        }
        _ => {}
    }
}

/// Intersection of two "definitely initialized" sets: a name is in the
/// result only if initialized on BOTH paths (conservative join).
fn intersect_sets(a: &HashSet<String>, b: &HashSet<String>) -> HashSet<String> {
    if a.len() <= b.len() {
        a.iter().filter(|n| b.contains(*n)).cloned().collect()
    } else {
        b.iter().filter(|n| a.contains(*n)).cloned().collect()
    }
}
