//! Statement type-checking: free functions following the
//! `definite_assignment.rs` pattern. `Checker`'s fields are `pub`, so
//! free functions can read/write them directly.
#![allow(clippy::collapsible_match, clippy::single_match)]

use zz_checker_core::Checker;
use zz_diag_core::{error_at, ErrorCode};
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
            is_state,
        } => check_stmt_decl(checker, name, ty, value, *is_const, *is_state),
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
        // Trait declarations are registered in an earlier pass; their
        // signatures carry no bodies to check.
        Stmt::Trait { .. } => Type::Unit,
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
        Stmt::Atomic { body, span: _ } => {
            // D3: entering an `atomic { }` block increments the depth
            // counter. Blocking natives are rejected inside.
            checker.atomic_depth += 1;
            checker.check_block(body);
            checker.atomic_depth -= 1;
            Type::Unit
        }
    }
}

// --- statement helpers -------------------------------------------------

fn check_stmt_decl(
    checker: &mut Checker,
    name: &zz_span_ast::ast::Ident,
    ty: &Option<Ty>,
    value: &Expr,
    is_const: bool,
    is_state: bool,
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
    // D1: `state` is module-level only in v1. At top level the env stack
    // has exactly one entry (the module scope). Inside any function/block
    // it's deeper.
    if is_state && checker.env.len() != 1 {
        checker.errors.push(
            error_at(
                "`state` is only allowed at module level in v1\n\
             hint: move the declaration to the top level of the module",
                name.span,
            )
            .with_code(ErrorCode(11)),
        );
    }
    // D6: a `state` may not hold a closure/function value (or any type
    // containing one), because closures capture by reference through
    // shared heap cells and would smuggle unsynchronized shared cells
    // across tasks.
    if is_state {
        let rt = checker.unifier.resolve_deep(&vt);
        if type_contains_func(rt) {
            checker.errors.push(
                error_at(
                    format!(
                        "`state` cannot hold a closure or function value ({})\n\
                     hint: store the data the closure needs instead of the closure itself",
                        name.name
                    ),
                    name.span,
                )
                .with_code(ErrorCode(12)),
            );
        }
        checker.state_names.insert(name.name.clone());
    }
    checker.warn_if_shadowed_decl(probe, value, ty.is_some(), is_const);
    // Phase 2.2: inherit `std.http` route tables across server
    // builder chains (`s2 := s.route_get(...)`, `s2 := s`).
    checker.propagate_http_routes(&name.name, value);
    if let Some(ann) = ty {
        let gens = checker.current_generics.clone();
        let at = checker.ast_to_type(ann, &gens);
        if let Err(mut e) = checker.unifier.unify(&vt, &at) {
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
            e.context = Some(format!("variable `{}`", name.name));
            checker.report_mismatch_with_notes(e, ann.span, notes);
        }
    }
    let rt = checker.unifier.resolve_deep(&vt); // Variant types (Option/Result/Enum) may carry unconstrained payload vars
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
    // D2/D3: if this expression statement touches a state variable,
    // enforce auto-atomic safety on it.
    check_expr_atomic_safety(checker, e, e.span());
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
    // D2/D3: if this assignment touches a state variable, enforce
    // auto-atomic whitelist (outside atomic) or blocking-native (inside
    // atomic) rules on all expressions in the statement.
    check_expr_atomic_safety(checker, target, span);
    check_expr_atomic_safety(checker, value, span);
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
    // D2/D3: if this compound assignment touches a state variable,
    // enforce auto-atomic safety on all expressions in the statement.
    check_expr_atomic_safety(checker, target, span);
    check_expr_atomic_safety(checker, value, span);
    Type::Unit
}

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

/// Returns true if `t` is or contains a closure/function type.
/// Used by D6: a `state` may not hold a function value.
fn type_contains_func(t: zz_types::Type) -> bool {
    use zz_types::Type;
    match t {
        Type::Func(_, _) => true,
        Type::Array(inner) => type_contains_func(inner.as_ref().clone()),
        Type::Dict(_, v) => type_contains_func(v.as_ref().clone()),
        Type::Tuple(items) => items.iter().any(|i| type_contains_func(i.clone())),
        Type::Option(inner) => type_contains_func(inner.as_ref().clone()),
        Type::Result(ok, err) => {
            type_contains_func(ok.as_ref().clone()) || type_contains_func(err.as_ref().clone())
        }
        Type::Struct(_, args) => args.iter().any(|t| type_contains_func(t.clone())),
        Type::Enum(_, variants) => variants.iter().any(|t| type_contains_func(t.clone())),
        Type::Union(items) => items.iter().any(|t| type_contains_func(t.clone())),
        _ => false,
    }
}

// --- D2/D3: auto-atomic statement safety --------------------------------

/// Returns true if `expr` reads or writes any module-level `state` variable
/// by referencing its name as an `Expr::Ident`.
fn expr_references_state(checker: &Checker, expr: &Expr) -> bool {
    match expr {
        Expr::Ident { name, .. } => checker.state_names.contains(name),
        Expr::Path { parts, .. } => parts.iter().any(|p| checker.state_names.contains(p)),
        Expr::Field { obj, name, .. } => {
            checker.state_names.contains(name) || expr_references_state(checker, obj)
        }
        Expr::Index { obj, index, .. } => {
            expr_references_state(checker, obj) || expr_references_state(checker, index)
        }
        Expr::Binary { left, right, .. } => {
            expr_references_state(checker, left) || expr_references_state(checker, right)
        }
        Expr::Unary { expr, .. } => expr_references_state(checker, expr),
        Expr::Try { expr, .. } => expr_references_state(checker, expr),
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            expr_references_state(checker, callee)
                || args.iter().any(|a| expr_references_state(checker, a))
                || named.iter().any(|(_, v)| expr_references_state(checker, v))
        }
        Expr::Closure { body, .. } => expr_references_state(checker, body),
        Expr::If {
            cond, then, els, ..
        } => {
            expr_references_state(checker, cond)
                || expr_references_state_block(checker, then)
                || els
                    .as_ref()
                    .is_some_and(|e| expr_references_state_expr(checker, e))
        }
        Expr::While { cond, body, .. } => {
            expr_references_state(checker, cond) || expr_references_state_block(checker, body)
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            expr_references_state(checker, scrutinee)
                || arms.iter().any(|a| {
                    expr_references_state_pattern(checker, &a.pat)
                        || expr_references_state_expr(checker, &a.body)
                })
        }
        Expr::Block(b) => expr_references_state_block(checker, b),
        Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
            zz_span_ast::ast::FmtPart::Expr(e, _) => expr_references_state(checker, e),
            _ => false,
        }),
        Expr::Array { elems, .. } => elems.iter().any(|e| expr_references_state(checker, e)),
        Expr::Dict { entries, .. } => entries
            .iter()
            .any(|(k, v)| expr_references_state(checker, k) || expr_references_state(checker, v)),
        Expr::Paren { expr, .. } => expr_references_state(checker, expr),
        Expr::Tuple { items, .. } => items.iter().any(|e| expr_references_state(checker, e)),
        _ => false,
    }
}

/// Returns true if `expr` references a state variable (used in match arms
/// where the body is an `Expr`, not a `Block`).
fn expr_references_state_expr(checker: &Checker, expr: &Expr) -> bool {
    expr_references_state(checker, expr)
}

/// Returns true if any statement in `block` references a state variable.
fn expr_references_state_block(checker: &Checker, block: &zz_span_ast::ast::Block) -> bool {
    block
        .stmts
        .iter()
        .any(|s| expr_references_state_stmt(checker, s))
}

/// Returns true if `stmt` references a state variable.
fn expr_references_state_stmt(checker: &Checker, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Decl { value, .. } => expr_references_state(checker, value),
        Stmt::Assign { target, value, .. } => {
            expr_references_state(checker, target) || expr_references_state(checker, value)
        }
        Stmt::CompoundAssign { target, value, .. } => {
            expr_references_state(checker, target) || expr_references_state(checker, value)
        }
        Stmt::Expr(e) => expr_references_state(checker, e),
        Stmt::For { iter, body, .. } => {
            expr_references_state(checker, iter) || expr_references_state_block(checker, body)
        }
        Stmt::Return { value, .. } => value
            .as_ref()
            .is_some_and(|v| expr_references_state(checker, v)),
        Stmt::Defer { expr, .. } => expr_references_state(checker, expr),
        Stmt::Atomic { body, .. } => expr_references_state_block(checker, body),
        Stmt::Destructure { value, .. } => expr_references_state(checker, value),
        _ => false,
    }
}

/// Returns true if the pattern binds or references a state variable name.
fn expr_references_state_pattern(_checker: &Checker, _pat: &zz_span_ast::ast::Pattern) -> bool {
    // Patterns don't read state; they only bind names.
    false
}

/// Returns true if `name` is a stdlib native marked `readonly` (pure,
/// non-blocking) — these are whitelisted for D2 auto-atomic statements.
fn is_whitelisted_native(checker: &Checker, name: &str) -> bool {
    zz_checker_core::lookup_std(&checker.funcs, name)
        .map(|sig| sig.readonly)
        .unwrap_or(false)
}

/// Returns true if `name` is a known-blocking native — forbidden inside
/// `atomic { }` (D3).
fn is_blocking_native(name: &str) -> bool {
    matches!(
        name,
        "chan.recv"
            | "chan.send"
            | "chan.send_batch"
            | "chan.recv_batch"
            | "task.join"
            | "task.spawn"
            | "time.sleep_ms"
            | "time.sleep_micros"
            | "std.env.set_var"
            | "print"
            | "println"
    ) || name.starts_with("std.fs.")
        || name.starts_with("std.net.")
        || name.starts_with("std.http.")
        || name.starts_with("std.process.")
        || name.starts_with("sqlz.")
        || name.starts_with("io.")
}

/// Check D2/D3 rules for an expression that appears in a statement touching
/// state. Called from assign/compound-assign/expr statement checkers.
fn check_expr_atomic_safety(checker: &mut Checker, expr: &Expr, span: Span) {
    if !expr_references_state(checker, expr) {
        return; // statement doesn't touch state — nothing to enforce
    }

    if checker.atomic_depth > 0 {
        // D3: inside `atomic { }` — reject known-blocking natives.
        check_for_blocking_natives(checker, expr, span);
    } else {
        // D2: outside `atomic { }` — only whitelisted natives / pure
        // constructs allowed.
        check_whitelist_only(checker, expr, span);
    }
}

/// Walk `expr` and report any call to a known-blocking native (D3).
fn check_for_blocking_natives(checker: &mut Checker, expr: &Expr, span: Span) {
    match expr {
        Expr::Call {
            callee,
            args,
            named,
            span: s,
            ..
        } => {
            if let Some(name) = callee_name(callee) {
                if is_blocking_native(&name) {
                    checker.errors.push(
                        error_at(
                            format!(
                            "`{name}` is a blocking call and cannot be used inside `atomic {{ }}`\n\
                             hint: move the blocking call outside the atomic block",
                            name = name
                        ),
                            *s,
                        )
                        .with_code(ErrorCode(15)),
                    );
                }
            }
            // Recurse into args — a blocking call nested in an argument
            // still executes inside the atomic statement.
            for a in args {
                check_for_blocking_natives(checker, a, *s);
            }
            for (_, v) in named {
                check_for_blocking_natives(checker, v, *s);
            }
        }
        Expr::Binary { left, right, .. } => {
            check_for_blocking_natives(checker, left, span);
            check_for_blocking_natives(checker, right, span);
        }
        Expr::Unary { expr, .. } => check_for_blocking_natives(checker, expr, span),
        Expr::Try { expr, .. } => check_for_blocking_natives(checker, expr, span),
        Expr::Closure { body, .. } => {
            // User closures inside atomic are allowed (D3), but their
            // bodies might contain blocking calls.
            check_for_blocking_natives(checker, body, span);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            check_for_blocking_natives(checker, cond, span);
            check_for_blocking_natives_block(checker, then, span);
            if let Some(e) = els {
                check_for_blocking_natives_expr(checker, e, span);
            }
        }
        Expr::While { cond, body, .. } => {
            check_for_blocking_natives(checker, cond, span);
            check_for_blocking_natives_block(checker, body, span);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            check_for_blocking_natives(checker, scrutinee, span);
            for a in arms {
                check_for_blocking_natives_expr(checker, &a.body, span);
            }
        }
        Expr::Block(b) => {
            for s in &b.stmts {
                check_for_blocking_natives_stmt(checker, s, span);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_span_ast::ast::FmtPart::Expr(e, _) = p {
                    check_for_blocking_natives(checker, e, span);
                }
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                check_for_blocking_natives(checker, e, span);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                check_for_blocking_natives(checker, k, span);
                check_for_blocking_natives(checker, v, span);
            }
        }
        Expr::Paren { expr, .. } => check_for_blocking_natives(checker, expr, span),
        Expr::Tuple { items, .. } => {
            for e in items {
                check_for_blocking_natives(checker, e, span);
            }
        }
        _ => {}
    }
}

fn check_for_blocking_natives_expr(checker: &mut Checker, expr: &Expr, span: Span) {
    check_for_blocking_natives(checker, expr, span);
}

fn check_for_blocking_natives_block(
    checker: &mut Checker,
    block: &zz_span_ast::ast::Block,
    span: Span,
) {
    for s in &block.stmts {
        check_for_blocking_natives_stmt(checker, s, span);
    }
}

fn check_for_blocking_natives_stmt(checker: &mut Checker, stmt: &Stmt, span: Span) {
    match stmt {
        Stmt::Decl { value, .. } => check_for_blocking_natives(checker, value, span),
        Stmt::Assign { target, value, .. } => {
            check_for_blocking_natives(checker, target, span);
            check_for_blocking_natives(checker, value, span);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            check_for_blocking_natives(checker, target, span);
            check_for_blocking_natives(checker, value, span);
        }
        Stmt::Expr(e) => check_for_blocking_natives(checker, e, span),
        Stmt::For { iter, body, .. } => {
            check_for_blocking_natives(checker, iter, span);
            check_for_blocking_natives_block(checker, body, span);
        }
        Stmt::Return { value, .. } => value
            .as_ref()
            .map_or((), |v| check_for_blocking_natives(checker, v, span)),
        Stmt::Defer { expr, .. } => check_for_blocking_natives(checker, expr, span),
        Stmt::Atomic { body, .. } => {
            // Nested atomic: blocking check applies inside too, but
            // depth > 0 already so check_for_blocking_natives handles it.
            check_for_blocking_natives_block(checker, body, span);
        }
        Stmt::Destructure { value, .. } => check_for_blocking_natives(checker, value, span),
        _ => {}
    }
}

/// Extract the callee name from a call expression, if it's a direct
/// identifier or path reference.
fn callee_name(callee: &Expr) -> Option<String> {
    match callee {
        Expr::Ident { name, .. } => Some(name.clone()),
        Expr::Path { parts, .. } => Some(parts.join(".")),
        _ => None,
    }
}

/// Walk `expr` and report any call that is not a whitelisted readonly
/// stdlib native or a user-defined function/closure (D2).
fn check_whitelist_only(checker: &mut Checker, expr: &Expr, span: Span) {
    match expr {
        Expr::Call {
            callee,
            args,
            named,
            span: s,
            ..
        } => {
            if let Some(name) = callee_name(callee) {
                if !is_whitelisted_native(checker, &name) {
                    checker.errors.push(
                        error_at(
                            format!(
                                "`{name}` cannot be called in a statement that touches `state`\n\
                             hint: wrap the statement in `atomic {{ }}`",
                                name = name
                            ),
                            *s,
                        )
                        .with_code(ErrorCode(14)),
                    );
                }
            }
            // Recurse into arguments — they're part of the same atomic statement.
            for a in args {
                check_whitelist_only(checker, a, *s);
            }
            for (_, v) in named {
                check_whitelist_only(checker, v, *s);
            }
        }
        Expr::Closure { .. } => {
            // A closure literal inside a state-touching statement is a
            // user function value — not whitelisted.
            checker.errors.push(error_at(
                "closure literal cannot appear in a statement that touches `state`\n\
                 hint: wrap the statement in `atomic { }`",
                span,
            ));
        }
        Expr::Binary { left, right, .. } => {
            check_whitelist_only(checker, left, span);
            check_whitelist_only(checker, right, span);
        }
        Expr::Unary { expr, .. } => check_whitelist_only(checker, expr, span),
        Expr::Try { expr, .. } => check_whitelist_only(checker, expr, span),
        Expr::If {
            cond, then, els, ..
        } => {
            check_whitelist_only(checker, cond, span);
            check_whitelist_only_block(checker, then, span);
            if let Some(e) = els {
                check_whitelist_only_expr(checker, e, span);
            }
        }
        Expr::While { cond, body, .. } => {
            check_whitelist_only(checker, cond, span);
            check_whitelist_only_block(checker, body, span);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            check_whitelist_only(checker, scrutinee, span);
            for a in arms {
                check_whitelist_only_expr(checker, &a.body, span);
            }
        }
        Expr::Block(b) => {
            for s in &b.stmts {
                check_whitelist_only_stmt(checker, s, span);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_span_ast::ast::FmtPart::Expr(e, _) = p {
                    check_whitelist_only(checker, e, span);
                }
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                check_whitelist_only(checker, e, span);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                check_whitelist_only(checker, k, span);
                check_whitelist_only(checker, v, span);
            }
        }
        Expr::Paren { expr, .. } => check_whitelist_only(checker, expr, span),
        Expr::Tuple { items, .. } => {
            for e in items {
                check_whitelist_only(checker, e, span);
            }
        }
        // Literals, idents, field/index access: always allowed.
        _ => {}
    }
}

fn check_whitelist_only_expr(checker: &mut Checker, expr: &Expr, span: Span) {
    check_whitelist_only(checker, expr, span);
}

fn check_whitelist_only_block(checker: &mut Checker, block: &zz_span_ast::ast::Block, span: Span) {
    for s in &block.stmts {
        check_whitelist_only_stmt(checker, s, span);
    }
}

fn check_whitelist_only_stmt(checker: &mut Checker, stmt: &Stmt, span: Span) {
    match stmt {
        Stmt::Decl { value, .. } => check_whitelist_only(checker, value, span),
        Stmt::Assign { target, value, .. } => {
            check_whitelist_only(checker, target, span);
            check_whitelist_only(checker, value, span);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            check_whitelist_only(checker, target, span);
            check_whitelist_only(checker, value, span);
        }
        Stmt::Expr(e) => check_whitelist_only(checker, e, span),
        Stmt::For { iter, body, .. } => {
            check_whitelist_only(checker, iter, span);
            check_whitelist_only_block(checker, body, span);
        }
        Stmt::Return { value, .. } => value
            .as_ref()
            .map_or((), |v| check_whitelist_only(checker, v, span)),
        Stmt::Defer { expr, .. } => check_whitelist_only(checker, expr, span),
        Stmt::Atomic { .. } => {
            // Inside an explicit atomic block, D2 doesn't apply (the user
            // opted in to manual locking). D3 blocking checks still apply,
            // but those are handled by check_for_blocking_natives.
        }
        Stmt::Destructure { value, .. } => check_whitelist_only(checker, value, span),
        _ => {}
    }
}

// --- D4: R1 check-then-act ----------------------------------------------

/// Within one function/closure body, outside any `atomic` block, if a `state`
/// S is read in one statement and written in a DIFFERENT statement, report R1
/// error. Conservative: uses statement order as reachability approximation.
/// Runs as a post-pass on each function/closure body after type checking.
pub(crate) fn check_r1_in_block(checker: &mut Checker, block: &zz_span_ast::ast::Block) {
    let mut reads: std::collections::HashMap<String, Vec<(usize, Span)>> =
        std::collections::HashMap::new();
    let mut writes: std::collections::HashMap<String, Vec<(usize, Span)>> =
        std::collections::HashMap::new();
    let mut stmt_idx: usize = 0;
    let mut in_atomic: bool = false;

    collect_state_rw(
        checker,
        block,
        &mut reads,
        &mut writes,
        &mut stmt_idx,
        &mut in_atomic,
    );

    for (name, read_entries) in &reads {
        if let Some(write_entries) = writes.get(name) {
            for &(ri, rspan) in read_entries {
                for &(wi, _) in write_entries {
                    if ri < wi {
                        checker.errors.push(
                            error_at(
                                format!(
                                    "check-then-act on `state` `{name}`:\n\
                                     read at statement {ri} can reach write at statement {wi}\n\
                                     hint: wrap both in `atomic {{ }}`",
                                    name = name,
                                    ri = ri,
                                    wi = wi,
                                ),
                                rspan,
                            )
                            .with_code(ErrorCode(16)),
                        );
                    }
                }
            }
        }
    }
}

/// Walk `block` recording state reads/writes per statement index and span.
fn collect_state_rw(
    checker: &Checker,
    block: &zz_span_ast::ast::Block,
    reads: &mut std::collections::HashMap<String, Vec<(usize, Span)>>,
    writes: &mut std::collections::HashMap<String, Vec<(usize, Span)>>,
    stmt_idx: &mut usize,
    in_atomic: &mut bool,
) {
    for stmt in &block.stmts {
        let span = stmt.span();
        match stmt {
            Stmt::Atomic { body, .. } => {
                let prev = *in_atomic;
                *in_atomic = true;
                collect_state_rw(checker, body, reads, writes, stmt_idx, in_atomic);
                *in_atomic = prev;
                *stmt_idx += 1;
            }
            Stmt::Decl {
                value, is_state, ..
            } => {
                if *is_state {
                    collect_state_names_from_expr(checker, value, reads, *stmt_idx, span);
                }
                *stmt_idx += 1;
            }
            Stmt::Assign { target, value, .. } => {
                if is_state_target(checker, target) {
                    collect_state_names_from_expr(checker, target, reads, *stmt_idx, span);
                    collect_state_names_from_expr(checker, target, writes, *stmt_idx, span);
                }
                collect_state_names_from_expr(checker, value, reads, *stmt_idx, span);
                *stmt_idx += 1;
            }
            Stmt::CompoundAssign { target, value, .. } => {
                if is_state_target(checker, target) {
                    collect_state_names_from_expr(checker, target, reads, *stmt_idx, span);
                    collect_state_names_from_expr(checker, target, writes, *stmt_idx, span);
                }
                collect_state_names_from_expr(checker, value, reads, *stmt_idx, span);
                *stmt_idx += 1;
            }
            Stmt::Expr(e) => {
                collect_state_names_from_expr(checker, e, reads, *stmt_idx, span);
                *stmt_idx += 1;
            }
            Stmt::For { body, .. } => {
                collect_state_rw(checker, body, reads, writes, stmt_idx, in_atomic);
                *stmt_idx += 1;
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    collect_state_names_from_expr(checker, v, reads, *stmt_idx, span);
                }
                *stmt_idx += 1;
            }
            Stmt::Defer { expr, .. } => {
                collect_state_names_from_expr(checker, expr, reads, *stmt_idx, span);
                *stmt_idx += 1;
            }
            Stmt::Destructure { value, .. } => {
                collect_state_names_from_expr(checker, value, reads, *stmt_idx, span);
                *stmt_idx += 1;
            }
            _ => {
                *stmt_idx += 1;
            }
        }
    }
}

/// Collect state variable names referenced in `expr` into `map` at `stmt_idx`.
fn collect_state_names_from_expr(
    checker: &Checker,
    expr: &Expr,
    map: &mut std::collections::HashMap<String, Vec<(usize, Span)>>,
    stmt_idx: usize,
    span: Span,
) {
    match expr {
        Expr::Ident { name, .. } => {
            if checker.state_names.contains(name) {
                map.entry(name.clone()).or_default().push((stmt_idx, span));
            }
        }
        Expr::Path { parts, .. } => {
            for p in parts {
                if checker.state_names.contains(p) {
                    map.entry(p.clone()).or_default().push((stmt_idx, span));
                }
            }
        }
        Expr::Field { obj, name, .. } => {
            if checker.state_names.contains(name) {
                map.entry(name.clone()).or_default().push((stmt_idx, span));
            }
            collect_state_names_from_expr(checker, obj, map, stmt_idx, span);
        }
        Expr::Index { obj, index, .. } => {
            collect_state_names_from_expr(checker, obj, map, stmt_idx, span);
            collect_state_names_from_expr(checker, index, map, stmt_idx, span);
        }
        Expr::Binary { left, right, .. } => {
            collect_state_names_from_expr(checker, left, map, stmt_idx, span);
            collect_state_names_from_expr(checker, right, map, stmt_idx, span);
        }
        Expr::Unary { expr, .. } => {
            collect_state_names_from_expr(checker, expr, map, stmt_idx, span);
        }
        Expr::Try { expr, .. } => {
            collect_state_names_from_expr(checker, expr, map, stmt_idx, span);
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_state_names_from_expr(checker, callee, map, stmt_idx, span);
            for a in args {
                collect_state_names_from_expr(checker, a, map, stmt_idx, span);
            }
            for (_, v) in named {
                collect_state_names_from_expr(checker, v, map, stmt_idx, span);
            }
        }
        Expr::Closure { body, .. } => {
            collect_state_names_from_expr(checker, body, map, stmt_idx, span);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_state_names_from_expr(checker, cond, map, stmt_idx, span);
            collect_state_names_from_block(checker, then, map, stmt_idx, span);
            if let Some(e) = els {
                collect_state_names_from_expr(checker, e, map, stmt_idx, span);
            }
        }
        Expr::While { cond, body, .. } => {
            collect_state_names_from_expr(checker, cond, map, stmt_idx, span);
            collect_state_names_from_block(checker, body, map, stmt_idx, span);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_state_names_from_expr(checker, scrutinee, map, stmt_idx, span);
            for a in arms {
                collect_state_names_from_expr(checker, &a.body, map, stmt_idx, span);
            }
        }
        Expr::Block(b) => {
            for s in &b.stmts {
                collect_state_names_from_stmt(checker, s, map, stmt_idx, span);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_span_ast::ast::FmtPart::Expr(e, _) = p {
                    collect_state_names_from_expr(checker, e, map, stmt_idx, span);
                }
            }
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_state_names_from_expr(checker, e, map, stmt_idx, span);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_state_names_from_expr(checker, k, map, stmt_idx, span);
                collect_state_names_from_expr(checker, v, map, stmt_idx, span);
            }
        }
        Expr::Paren { expr, .. } => {
            collect_state_names_from_expr(checker, expr, map, stmt_idx, span);
        }
        Expr::Tuple { items, .. } => {
            for e in items {
                collect_state_names_from_expr(checker, e, map, stmt_idx, span);
            }
        }
        _ => {}
    }
}

/// Collect state names from all statements in a block.
fn collect_state_names_from_block(
    checker: &Checker,
    block: &zz_span_ast::ast::Block,
    map: &mut std::collections::HashMap<String, Vec<(usize, Span)>>,
    stmt_idx: usize,
    span: Span,
) {
    for s in &block.stmts {
        collect_state_names_from_stmt(checker, s, map, stmt_idx, span);
    }
}

/// Collect state names from a single statement (expression-level only).
fn collect_state_names_from_stmt(
    checker: &Checker,
    stmt: &Stmt,
    map: &mut std::collections::HashMap<String, Vec<(usize, Span)>>,
    stmt_idx: usize,
    span: Span,
) {
    match stmt {
        Stmt::Expr(e) => collect_state_names_from_expr(checker, e, map, stmt_idx, span),
        Stmt::Assign { target, value, .. } => {
            collect_state_names_from_expr(checker, target, map, stmt_idx, span);
            collect_state_names_from_expr(checker, value, map, stmt_idx, span);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            collect_state_names_from_expr(checker, target, map, stmt_idx, span);
            collect_state_names_from_expr(checker, value, map, stmt_idx, span);
        }
        Stmt::Decl { value, .. } => {
            collect_state_names_from_expr(checker, value, map, stmt_idx, span);
        }
        Stmt::For { body, .. } => {
            collect_state_names_from_block(checker, body, map, stmt_idx, span);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_state_names_from_expr(checker, v, map, stmt_idx, span);
            }
        }
        Stmt::Defer { expr, .. } => {
            collect_state_names_from_expr(checker, expr, map, stmt_idx, span);
        }
        Stmt::Destructure { value, .. } => {
            collect_state_names_from_expr(checker, value, map, stmt_idx, span);
        }
        _ => {}
    }
}

/// Check if `expr` is an identifier or path referencing a state variable.
fn is_state_target(checker: &Checker, expr: &Expr) -> bool {
    match expr {
        Expr::Ident { name, .. } => checker.state_names.contains(name),
        Expr::Path { parts, .. } => parts.iter().any(|p| checker.state_names.contains(p)),
        Expr::Field { name, .. } => checker.state_names.contains(name),
        _ => false,
    }
}
