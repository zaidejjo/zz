//! Statement type-checking: free functions following the
//! `definite_assignment.rs` pattern. `Checker`'s fields are `pub`, so
//! free functions can read/write them directly.

use zz_checker_core::Checker;
use zz_diag_core::error_at;
use zz_infer::{contains_var, default_variant_vars, InferOps};
use zz_lints::http_lint::HttpLintOps;
use zz_scope::ScopeOps;
use zz_span_ast::ast::{BinOp, Expr, ImportItem, Pattern, Stmt, Ty};
use zz_span_ast::span::Span;
use zz_types::Type;

use crate::TypeCheckOps as _;

/// Type-check one statement. Dispatches to per-statement helpers.
pub(crate) fn check_stmt(checker: &mut Checker, stmt: &Stmt) -> Type {
    checker.had_undefined_var = false;
    match stmt {
        Stmt::Decl {
            name,
            ty,
            value,
            span: _,
            pub_: _,
            is_const,
        } => check_stmt_decl(checker, name, ty, value, *is_const),
        Stmt::Import {
            path,
            alias,
            items,
            span,
            pub_: _,
        } => check_stmt_import(checker, path, alias, items, *span),
        Stmt::Func { span, .. } => check_stmt_func(checker, stmt, *span),
        Stmt::ExternBlock { .. } => Type::Unit,
        Stmt::Link { lib, span } => check_stmt_link(checker, lib, *span),
        Stmt::Impl { name, methods, .. } => check_stmt_impl(checker, name, methods),
        Stmt::Return { value, span } => check_stmt_return(checker, value, *span),
        Stmt::Expr(e) => check_stmt_expr(checker, e),
        Stmt::Struct { .. } => Type::Unit,
        Stmt::TypeAlias { .. } => Type::Unit,
        Stmt::Enum { .. } => Type::Unit,
        Stmt::For {
            vars,
            iter,
            body,
            span,
        } => check_stmt_for(checker, vars, iter, body, *span),
        Stmt::Break { span } => check_stmt_break(checker, *span),
        Stmt::Continue { span } => check_stmt_continue(checker, *span),
        Stmt::Defer { expr, span } => check_stmt_defer(checker, expr, *span),
        Stmt::Destructure {
            pat,
            value,
            span: _,
        } => check_stmt_destructure(checker, pat, value),
        Stmt::Assign {
            target,
            value,
            span,
        } => check_stmt_assign(checker, target, value, *span),
        Stmt::CompoundAssign {
            target,
            op,
            value,
            span,
        } => check_stmt_compound_assign(checker, target, *op, value, *span),
    }
}

// --- statement helpers -------------------------------------------------

fn check_stmt_decl(
    checker: &mut Checker,
    name: &zz_span_ast::ast::Ident,
    ty: &Option<Ty>,
    value: &Expr,
    is_const: bool,
) -> Type {
    // #518: `x := x + 1` inside a nested block declares a *new*
    // `x` scoped to that block — the outer binding never advances.
    // The initializer is checked before the new binding exists, so
    // a read of `name` below resolves to the shadowed binding.
    let probe = checker.begin_shadow_probe(name.name.clone(), name.span);
    // Pre-bind closures so recursive references resolve.
    if matches!(value, Expr::Closure { .. }) {
        let fv = checker.unifier.fresh_var();
        checker.define_var_at(&name.name, fv, name.span, is_const);
    }
    let vt = checker.check_expr(value);
    checker.warn_if_shadowed_decl(probe, value, ty.is_some(), is_const);
    // Phase 2.2: inherit `std.http` route tables across server
    // builder chains (`s2 := s.route_get(...)`, `s2 := s`).
    checker.propagate_http_routes(&name.name, value);
    if let Some(ann) = ty {
        let gens = checker.current_generics.clone();
        let at = checker.ast_to_type(ann, &gens);
        if let Err(e) = checker.unifier.unify(&vt, &at) {
            // Sherlock: point at the fix direction. Skip the
            // note while the value type is still uninferred
            // (`?0` helps nobody).
            let mut notes = Vec::new();
            let vt_r = checker.unifier.resolve(&vt);
            if !matches!(vt_r, Type::Var(_)) {
                notes.push(format!(
                    "to fix: change the annotation to `{vt_r}` or convert the value"
                ));
            }
            checker.report_mismatch_with_notes(e, ann.span, notes);
        }
    }
    let rt = checker.unifier.resolve_deep(&vt);
    // Variant types (Option/Result/Enum) may carry unconstrained payload vars
    // that get resolved by later usage (e.g. `.unwrap_or(7)` on `.none`).
    // Defer those to span type resolution; other unconstrained vars still
    // error here (e.g. closure param types in `f := |x| x`).
    if matches!(rt, Type::Option(_) | Type::Result(_, _) | Type::Enum(_, _)) {
        checker.define_var_at(&name.name, rt.clone(), name.span, is_const);
        if checker.env.len() == 1 {
            checker.new_bindings.insert(name.name.clone(), rt.clone());
        }
        return rt;
    }
    if contains_var(&rt) {
        let mut d = rt.clone();
        default_variant_vars(&mut d);
        if contains_var(&d) {
            // Empty collections like `[]` or `{}` get a fresh var for
            // element/key/value types. Defer inference — the var may be
            // resolved later by usage context (e.g. function calls).
            let may_defer = matches!(
                &d,
                Type::Array(inner) if matches!(inner.as_ref(), Type::Var(_))
            ) || matches!(
                &d,
                Type::Dict(k, v) if matches!(k.as_ref(), Type::Var(_)) || matches!(v.as_ref(), Type::Var(_))
            );
            if !may_defer {
                checker.errors.push(error_at(
                    format!(
                        "cannot infer the type of `{}`; add a type annotation",
                        name.name
                    ),
                    name.span,
                ));
            }
        } else {
            checker.define_var_at(&name.name, d.clone(), name.span, is_const);
            if checker.env.len() == 1 {
                checker.new_bindings.insert(name.name.clone(), d.clone());
            }
            return d;
        }
    }
    if checker.env.len() == 1 {
        checker.new_bindings.insert(name.name.clone(), vt.clone());
    }
    checker.define_var_at(&name.name, rt.clone(), name.span, is_const);
    rt
}

fn check_stmt_import(
    checker: &mut Checker,
    path: &[String],
    alias: &Option<String>,
    items: &[ImportItem],
    span: Span,
) -> Type {
    if items.is_empty() {
        // Full module import: track namespace for unused-import warnings.
        let ns = alias
            .as_ref()
            .cloned()
            .or_else(|| path.last().cloned())
            .unwrap_or_default();
        checker.imports.push((ns.clone(), span));
        // Remember the module behind an aliased head (`m` →
        // `std.math`) for const/call diagnostics.
        checker
            .module_aliases
            .entry(ns.clone())
            .or_insert_with(|| path.join("."));
        // Full-path uses (`std.chan.send` under
        // `import std.chan`) must count for unused-import
        // tracking too — see scope.rs.
        checker
            .import_aliases
            .entry(ns)
            .or_insert_with(|| path.join("."));
    } else {
        // Selective/wildcard import: track each imported name.
        // Record bare→qualified aliases for call-site fallback
        // (generic functions have no value binding to find).
        let eff_ns = alias
            .as_ref()
            .cloned()
            .or_else(|| path.last().cloned())
            .unwrap_or_default();
        for item in items {
            let name = match item {
                ImportItem::Wildcard { .. } => {
                    // Wildcard: track the module namespace.
                    for k in checker.funcs.keys().cloned().collect::<Vec<_>>() {
                        if let Some(bare) = k.strip_prefix(&format!("{eff_ns}.")) {
                            if !bare.is_empty() && !bare.contains('.') {
                                checker.import_aliases.entry(bare.to_string()).or_insert(k);
                            }
                        }
                    }
                    eff_ns.clone()
                }
                ImportItem::Named {
                    name,
                    alias: item_alias,
                    ..
                } => {
                    let target = item_alias.clone().unwrap_or_else(|| name.clone());
                    checker
                        .import_aliases
                        .entry(target.clone())
                        .or_insert_with(|| format!("{eff_ns}.{name}"));
                    target
                }
            };
            checker.imports.push((name, span));
        }
    }
    Type::Unit
}

fn check_stmt_func(checker: &mut Checker, stmt: &Stmt, span: Span) -> Type {
    let fname = Checker::func_name(stmt);
    let sig = match zz_checker_core::lookup_std(&checker.funcs, &fname) {
        Some(s) => s.clone(),
        None => {
            checker.errors.push(zz_diag_core::error_at(
                format!(
                    "nested function `{}` is not supported; use a closure (`|params| body`) instead",
                    fname
                ),
                span,
            ));
            return Type::Unit;
        }
    };
    // Extern functions have no ZZ body — already registered in Pass 1d.
    if sig.is_extern {
        return Type::Unit;
    }
    // Scope span recording to this function: spans repeat
    // across modules, so the typed map keys (scope, span).
    checker.scope.push(std::sync::Arc::from(fname));
    checker.check_func_body(stmt, &sig);
    checker.scope.pop();
    Type::Unit
}

fn check_stmt_link(checker: &mut Checker, lib: &str, span: Span) -> Type {
    if lib.trim().is_empty() {
        checker
            .errors
            .push(error_at("`@link` requires a non-empty library name", span));
    } else if !checker.link_libs.iter().any(|l| l == lib) {
        checker.link_libs.push(lib.to_string());
    }
    Type::Unit
}

fn check_stmt_impl(checker: &mut Checker, name: &[String], methods: &[Stmt]) -> Type {
    let type_name = name.join(".");
    for method in methods {
        if let Stmt::Func { .. } = method {
            let method_name = Checker::func_name(method);
            let full_name = format!("{}.{}", type_name, method_name);
            let sig = zz_checker_core::lookup_std(&checker.funcs, &full_name)
                .unwrap()
                .clone();
            checker.scope.push(std::sync::Arc::from(full_name));
            checker.check_func_body(method, &sig);
            checker.scope.pop();
        }
    }
    Type::Unit
}

fn check_stmt_return(checker: &mut Checker, value: &Option<Expr>, span: Span) -> Type {
    let ret = match checker.current_ret.clone() {
        Some(r) => r,
        None => {
            checker
                .errors
                .push(error_at("`return` outside of a function", span));
            return Type::Unit;
        }
    };
    match value {
        Some(v) => {
            let vt = checker.check_expr(v);
            if let Err(e) = checker.unifier.unify(&vt, &ret) {
                if !checker.report_result_return_hint(&vt, &ret, v.span()) {
                    checker.report_mismatch(e, v.span());
                }
            }
            // A `return` diverges: it never yields a value to the
            // enclosing block. Its statement type is `Never`
            // (bottom): it vanishes from if/match joins without
            // constraining sibling arms, while the function's
            // fall-through type is still verified against its
            // signature in `check_func_body` — so divergent
            // arms can never mask a wrong-typed value path.
            Type::Never
        }
        None => {
            if let Err(e) = checker.unifier.unify(&Type::Unit, &ret) {
                checker.report_mismatch(e, span);
            }
            Type::Unit
        }
    }
}

fn check_stmt_expr(checker: &mut Checker, e: &Expr) -> Type {
    // A bare `match` statement discards the match value: tell
    // `check_match` so bare `Result`/`Option` arm bodies warn
    // (they never pass through this arm themselves). Tails
    // are values, not discards.
    let prev_discard = std::mem::replace(
        &mut checker.direct_discard,
        matches!(e, Expr::Match { .. }) && !checker.tail_is_value,
    );
    let t = checker.check_expr(e);
    checker.direct_discard = prev_discard;
    // Sherlock: a discarded `Result`/`Option` silently drops
    // errors — unless this tail is the block's value.
    if !checker.tail_is_value {
        checker.warn_if_discarded(e, &t);
    }
    t
}

fn check_stmt_break(checker: &mut Checker, span: Span) -> Type {
    if checker.loop_depth == 0 {
        checker
            .errors
            .push(error_at("`break` outside of a loop", span));
    }
    Type::Unit
}

fn check_stmt_continue(checker: &mut Checker, span: Span) -> Type {
    if checker.loop_depth == 0 {
        checker
            .errors
            .push(error_at("`continue` outside of a loop", span));
    }
    Type::Unit
}

fn check_stmt_defer(checker: &mut Checker, expr: &Expr, span: Span) -> Type {
    if checker.current_ret.is_none() {
        checker
            .errors
            .push(error_at("`defer` outside of a function", span));
    }
    let prev_discard = std::mem::replace(
        &mut checker.direct_discard,
        matches!(expr, Expr::Match { .. }),
    );
    let t = checker.check_expr(expr);
    checker.direct_discard = prev_discard;
    // Deferred results still vanish — same must-use rule.
    checker.warn_if_discarded(expr, &t);
    Type::Unit
}

fn check_stmt_destructure(checker: &mut Checker, pat: &Pattern, value: &Expr) -> Type {
    fn has_or(pat: &Pattern) -> bool {
        match pat {
            Pattern::Or { .. } => true,
            Pattern::Variant { arg: Some(a), .. } => has_or(a),
            Pattern::Tuple { pats, .. } => pats.iter().any(has_or),
            _ => false,
        }
    }
    if has_or(pat) {
        checker.errors.push(error_at(
            "or-patterns (`|`) are only allowed in match arms",
            pat.span(),
        ));
    }
    // #518: `a, b := f(b)` re-declares the body-scoped `b` from
    // the untouched outer value every iteration, exactly like the
    // single-binding form above.
    let mark = checker.shadow_probes.len();
    for (bound, bound_span) in super::pattern_bindings(pat) {
        checker.begin_shadow_probe(bound, bound_span);
    }
    let vt = checker.check_expr(value);
    checker.warn_if_shadowed_destructure(mark);
    let vt = checker.unifier.resolve(&vt);
    checker.bind_pattern(pat, &vt);
    Type::Unit
}

fn check_stmt_assign(checker: &mut Checker, target: &Expr, value: &Expr, span: Span) -> Type {
    // `_ = expr` — evaluate `expr` for effect, drop the value.
    // `_` is never a real binding, so there is nothing to assign
    // to: type-check the RHS and return unit without const-checking
    // or unifying against a target type.
    if let Expr::Ident { name, .. } = target {
        if name == "_" {
            let _ = checker.check_expr(value);
            return Type::Unit;
        }
    }

    // Reject assignment to immutable (`const`) variables. The target is an
    // `Ident` in plain programs and a `Path` (e.g. `ns.x`) after
    // the loader namespaces top-level bindings.
    checker.reject_const_target(target);
    let errors_before = checker.errors.len();
    let tt = checker.check_assign_target(target);
    let vt = checker.check_expr(value);
    // Phase 2.2: `s = s.route_get(...)` keeps the same root, but
    // `t = s.route_get(...)` must inherit `s`'s table under `t`.
    match target {
        Expr::Ident { name, .. } => checker.propagate_http_routes(name, value),
        Expr::Path { parts, .. } => checker.propagate_http_routes(&parts.join("."), value),
        _ => {}
    }
    if checker.errors.len() == errors_before {
        if let Err(e) = checker.unifier.unify(&vt, &tt) {
            checker.report_mismatch(e, span);
        }
    }
    Type::Unit
}

fn check_stmt_compound_assign(
    checker: &mut Checker,
    target: &Expr,
    op: BinOp,
    value: &Expr,
    span: Span,
) -> Type {
    // `target OP= value` checks exactly like
    // `target = target OP value`: same const rule, same
    // target rules, same binary-op rules (check_binary
    // re-checks both sides, so float promotion, int-only
    // bitwise, and error messages are identical).
    checker.reject_const_target(target);
    let errors_before = checker.errors.len();
    let tt = checker.check_assign_target(target);
    let bt = checker.check_binary(op, target, value, span);
    // Phase 2.2: same route-table propagation as `=`.
    match target {
        Expr::Ident { name, .. } => checker.propagate_http_routes(name, value),
        Expr::Path { parts, .. } => checker.propagate_http_routes(&parts.join("."), value),
        _ => {}
    }
    if checker.errors.len() == errors_before {
        if let Err(e) = checker.unifier.unify(&bt, &tt) {
            checker.report_mismatch(e, span);
        }
    }
    Type::Unit
}

// --- `for` statement helpers ------------------------------------------

fn check_stmt_for(
    checker: &mut Checker,
    vars: &[zz_span_ast::ast::Ident],
    iter: &Expr,
    body: &zz_span_ast::ast::Block,
    span: Span,
) -> Type {
    let it = checker.check_expr(iter);
    let it = checker.unifier.resolve(&it);
    match vars.len() {
        0 => {
            checker
                .errors
                .push(error_at("internal: for-loop with no variables", span));
        }
        1 => check_for_single(checker, vars, it, body, span),
        2 => check_for_pair(checker, vars, it, body, span),
        _ => {
            checker.errors.push(error_at(
                "for loop supports at most 2 variables (e.g. `for k, v in dict` or `for i, x in xs.enumerate()`)",
                span,
            ));
        }
    }
    Type::Unit
}

fn check_for_single(
    checker: &mut Checker,
    vars: &[zz_span_ast::ast::Ident],
    it: Type,
    body: &zz_span_ast::ast::Block,
    span: Span,
) {
    let elem = match it {
        Type::Array(elem) => *elem,
        Type::Bytes => Type::Int,
        Type::Range(elem) => *elem,
        Type::Dict(_k, _v) => {
            // for x in dict → iterates keys
            *_k
        }
        Type::Var(_) => {
            checker.errors.push(error_at(
                "cannot iterate a value whose type could not be inferred",
                span,
            ));
            Type::Unit
        }
        other => {
            checker.errors.push(error_at(
                format!("cannot iterate a value of type `{other}`"),
                span,
            ));
            Type::Unit
        }
    };
    checker.push_scope();
    checker.define_binding_at(&vars[0].name, elem, vars[0].span, false);
    checker.loop_depth += 1;
    checker.check_block_discarded(body);
    checker.loop_depth -= 1;
    checker.pop_scope();
}

fn check_for_pair(
    checker: &mut Checker,
    vars: &[zz_span_ast::ast::Ident],
    it: Type,
    body: &zz_span_ast::ast::Block,
    span: Span,
) {
    match it {
        Type::Dict(k, v) => {
            checker.push_scope();
            checker.define_binding_at(&vars[0].name, *k, vars[0].span, false);
            checker.define_binding_at(&vars[1].name, *v, vars[1].span, false);
            checker.loop_depth += 1;
            checker.check_block_discarded(body);
            checker.loop_depth -= 1;
            checker.pop_scope();
        }
        Type::Array(elem) => {
            match checker.unifier.resolve(&elem) {
                Type::Tuple(pair) if pair.len() == 2 => {
                    checker.push_scope();
                    checker.define_binding_at(&vars[0].name, pair[0].clone(), vars[0].span, false);
                    checker.define_binding_at(&vars[1].name, pair[1].clone(), vars[1].span, false);
                    checker.loop_depth += 1;
                    checker.check_block_discarded(body);
                    checker.loop_depth -= 1;
                    checker.pop_scope();
                }
                Type::Var(_) => {
                    // Element type not yet inferred
                    // (e.g. generic): fresh vars; body
                    // usage constrains them later.
                    let k_var = checker.unifier.fresh_var();
                    let v_var = checker.unifier.fresh_var();
                    checker.push_scope();
                    checker.define_binding_at(&vars[0].name, k_var, vars[0].span, false);
                    checker.define_binding_at(&vars[1].name, v_var, vars[1].span, false);
                    checker.loop_depth += 1;
                    checker.check_block_discarded(body);
                    checker.loop_depth -= 1;
                    checker.pop_scope();
                }
                other => {
                    checker.errors.push(error_at(
                        format!(
                            "expected a dictionary or an array of tuples for `for k, v in ...`, got `{other}`"
                        ),
                        span,
                    ));
                    checker.push_scope();
                    checker.define_binding_at(&vars[0].name, Type::Unit, vars[0].span, false);
                    checker.define_binding_at(&vars[1].name, Type::Unit, vars[1].span, false);
                    checker.loop_depth += 1;
                    checker.check_block_discarded(body);
                    checker.loop_depth -= 1;
                    checker.pop_scope();
                }
            }
        }
        Type::Var(_) => {
            checker.errors.push(error_at(
                "cannot iterate a value whose type could not be inferred",
                span,
            ));
            checker.push_scope();
            checker.define_binding_at(&vars[0].name, Type::Unit, vars[0].span, false);
            checker.define_binding_at(&vars[1].name, Type::Unit, vars[1].span, false);
            checker.loop_depth += 1;
            checker.check_block_discarded(body);
            checker.loop_depth -= 1;
            checker.pop_scope();
        }
        other => {
            checker.errors.push(error_at(
                format!("expected a dictionary for `for k, v in ...`, got `{other}`"),
                span,
            ));
            checker.push_scope();
            checker.define_binding_at(&vars[0].name, Type::Unit, vars[0].span, false);
            checker.define_binding_at(&vars[1].name, Type::Unit, vars[1].span, false);
            checker.loop_depth += 1;
            checker.check_block_discarded(body);
            checker.loop_depth -= 1;
            checker.pop_scope();
        }
    }
}
