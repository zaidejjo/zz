//! Expression type-checking helpers extracted from `check_expr_impl`.
//! Free functions following the `definite_assignment.rs` pattern.

use zz_checker_core::Checker;
use zz_diag_core::error_at;
use zz_infer::InferOps;
use zz_span_ast::ast::{Block, Expr, FmtPart, Ident, Pattern};
use zz_span_ast::span::Span;
use zz_types::Type;

use crate::ScopeOps as _;
use crate::TypeCheckOps;
use zz_sigs::enums::EnumOps;
use zz_structs::StructOps;

/// Type-check an expression, dispatching to per-expression-kind helpers.
pub(crate) fn check_expr_impl(checker: &mut Checker, e: &Expr) -> Type {
    match e {
        Expr::Int { .. } => Type::Int,
        Expr::Float { .. } => Type::Float,
        Expr::Str { .. } => Type::Str,
        Expr::Bool { .. } => Type::Bool,
        Expr::Ident { name, span } => checker.lookup(name, *span),
        Expr::Path { parts, span } => check_expr_path(checker, parts, *span),
        Expr::Field { obj, name, span } => check_expr_field(checker, obj, name, *span),
        Expr::Range { start, end, .. } => check_expr_range(checker, start, end),
        Expr::StructInit { name, fields, span } => {
            check_expr_struct_init(checker, name, fields, *span)
        }
        Expr::Index { obj, index, span } => check_expr_index(checker, obj, index, *span),
        Expr::Fmt { parts, .. } => check_expr_fmt(checker, parts),
        Expr::Paren { expr, .. } => checker.check_expr(expr),
        Expr::Unary { op, expr, span } => checker.check_unary(*op, expr, *span),
        Expr::Binary {
            op,
            left,
            right,
            span,
        } => checker.check_binary(*op, left, right, *span),
        Expr::Call {
            callee,
            args,
            named,
            span,
        } => checker.check_call(callee, args, named, *span),
        Expr::Closure {
            params,
            ret_ty,
            body,
            span,
        } => checker.check_closure(params, ret_ty.as_ref(), body, *span, None),
        Expr::If {
            cond,
            then,
            els,
            span,
        } => check_expr_if(checker, cond, then, els, *span),
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            span,
        } => check_expr_if_let(checker, pat, value, then, els, *span),
        Expr::While { cond, body, .. } => check_expr_while(checker, cond, body),
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => checker.check_match(scrutinee, arms, *span),
        Expr::Try { expr, span } => checker.check_try(expr, *span),
        Expr::Block(b) => checker.check_block(b),
        Expr::Slice {
            obj,
            start,
            end,
            span,
        } => check_expr_slice(checker, obj, start, end, *span),
        Expr::Array { elems, span: _ } => check_expr_array(checker, elems),
        Expr::Tuple { items, .. } => check_expr_tuple(checker, items),
        Expr::Dict { entries, span: _ } => check_expr_dict(checker, entries),
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            span,
        } => check_expr_list_comp(checker, body, var, iter, filter, *span),
        Expr::Break { span } => check_expr_break(checker, *span),
        Expr::Continue { span } => check_expr_continue(checker, *span),
        Expr::Variant { name, arg, span } => check_expr_variant(checker, name, arg, *span),
        _ => Type::Error,
    }
}

// --- expression helpers -------------------------------------------------

fn check_expr_path(checker: &mut Checker, parts: &[String], span: Span) -> Type {
    // Unit-variant value (`Token.Eof`, no parens): a path, not
    // a call. Resolves against the enum table; payload
    // variants must use call form (`Token.IntLit(1)`).
    if parts.len() >= 2 {
        let enum_head = parts[..parts.len() - 1].join(".");
        let canonical_head = checker.canonical_enum_name(&enum_head);
        if checker.enums.contains_key(&canonical_head)
            && !checker.funcs.contains_key(&parts.join("."))
            && checker.lookup_opt(&enum_head).is_none()
        {
            let variant = parts.last().cloned().unwrap_or_default();
            if checker
                .enum_variant_payload(&canonical_head, &variant, span)
                .is_some_and(|p| p.is_some())
            {
                checker.errors.push(error_at(
                    format!(
                        "variant `{canonical_head}.{variant}` holds a value: construct it as `{canonical_head}.{variant}(...)`"
                    ),
                    span,
                ));
            }
            if canonical_head.contains('.') {
                checker.used_names.insert(canonical_head.clone());
            }
            // Generic parameters stay inference variables
            // (defaulted like `Option` when never constrained).
            let (gen_vars, _) = checker.fresh_enum_vars(&canonical_head);
            return Type::Enum(canonical_head, gen_vars);
        }
    }
    checker.lookup_path(parts, span)
}

fn check_expr_field(checker: &mut Checker, obj: &Expr, name: &str, span: Span) -> Type {
    let ot = checker.check_expr(obj);
    let ot = checker.unifier.resolve(&ot);
    match ot {
        Type::Struct(sname, args) => checker.struct_field_access(&sname, &args, name, span),
        Type::Dict(k, v) => {
            // Dict field access: req.body returns the value type
            if let Err(e) = checker.unifier.unify(&Type::Str, &k) {
                checker.report_mismatch(e, span);
            }
            *v
        }
        Type::HttpRequest => {
            // Typed request field access: `req.method/path/body`
            // are `str`; `req.headers/query/params` are `{str: str}`.
            match name {
                "method" | "path" | "body" => Type::Str,
                "headers" | "query" | "params" => {
                    Type::Dict(Box::new(Type::Str), Box::new(Type::Str))
                }
                _ => {
                    checker.errors.push(error_at(
                        format!("http.request has no field `{name}`"),
                        span,
                    ));
                    Type::Unit
                }
            }
        }
        Type::Var(_id) => {
            // Inference variable — not yet resolved (e.g. untyped closure param).
            // Return a fresh var; unification will catch real mismatches later.
            checker.unifier.fresh_var()
        }
        Type::Error => Type::Error,
        other => {
            let method = name.to_string();
            let ns = match &other {
                Type::Str => Some("str"),
                Type::Array(_) => Some("vec"),
                Type::Option(_) => Some("option"),
                Type::Result(_, _) => Some("result"),
                Type::Int => Some("int"),
                Type::Float => Some("float"),
                Type::Bool => Some("bool"),
                _ => None,
            };
            if let Some(ns) = ns {
                if let Some(sig) =
                    zz_checker_core::lookup_std(&checker.funcs, &format!("{ns}.{method}")).cloned()
                {
                    let (ps, ret, subs) = checker.instantiate(&sig);
                    if !ps.is_empty() {
                        if let Err(e) = checker.unifier.unify(&other, &ps[0]) {
                            checker.report_mismatch(e, span);
                        }
                    }
                    let _ = subs;
                    ret
                } else {
                    Type::Error
                }
            } else {
                Type::Error
            }
        }
    }
}

fn check_expr_range(checker: &mut Checker, start: &Expr, end: &Expr) -> Type {
    let st = checker.check_expr(start);
    let en = checker.check_expr(end);
    let st = checker.unifier.resolve(&st);
    let en = checker.unifier.resolve(&en);
    match (&st, &en) {
        (Type::Int, Type::Int) => Type::Range(Box::new(Type::Int)),
        (Type::Var(_), Type::Var(_)) => Type::Range(Box::new(checker.unifier.fresh_var())),
        (Type::Var(_), concrete) => {
            checker.unifier.bind_var(&st, concrete.clone());
            Type::Range(Box::new(concrete.clone()))
        }
        (concrete, Type::Var(_)) => {
            checker.unifier.bind_var(&en, concrete.clone());
            Type::Range(Box::new(concrete.clone()))
        }
        (a, b) if a == b => Type::Range(Box::new(a.clone())),
        (a, b) => {
            checker.errors.push(error_at(
                format!("range bounds must have the same type, found `{a}` and `{b}`"),
                start.span().join(end.span()),
            ));
            Type::Error
        }
    }
}

fn check_expr_struct_init(
    checker: &mut Checker,
    name: &str,
    fields: &[(String, Expr)],
    span: Span,
) -> Type {
    // Struct initialization: resolve the struct signature, check each field.
    let sig = match checker.structs.get(name) {
        Some(s) => s.clone(),
        None => {
            checker
                .errors
                .push(error_at(format!("unknown struct `{name}`"), span));
            return Type::Error;
        }
    };
    let mut field_types = Vec::new();
    for (fname, fval) in fields {
        let fvt = checker.check_expr(fval);
        // Find the field type in the struct signature
        let field_ty = sig
            .fields
            .iter()
            .find(|(field_name, _)| field_name == fname)
            .map(|(_, t)| t.clone());
        match field_ty {
            Some(ft) => {
                if let Err(e) = checker.unifier.unify(&fvt, &ft) {
                    checker.report_mismatch(e, fval.span());
                }
            }
            None => {
                checker.errors.push(error_at(
                    format!("struct `{name}` has no field `{fname}`"),
                    fval.span(),
                ));
            }
        }
        field_types.push(fvt);
    }
    // Substitute generics if any
    let _ = sig;
    Type::Unit
}

fn check_expr_index(checker: &mut Checker, obj: &Expr, index: &Expr, span: Span) -> Type {
    let ot = checker.check_expr(obj);
    let ot = checker.unifier.resolve(&ot);
    let it = checker.check_expr(index);
    match ot {
        Type::Array(elem) => {
            checker.ensure_int(it, index.span());
            if let Expr::Array { elems, .. } = obj {
                checker.check_literal_array_index(elems.len(), index);
            }
            *elem
        }
        Type::Dict(k, v) => {
            if let Err(e) = checker.unifier.unify(&it, &k) {
                checker.report_mismatch(e, index.span());
            }
            *v
        }
        Type::Str => {
            checker
                .errors
                .push(error_at("cannot index into a string", span));
            Type::Unit
        }
        Type::Tuple(elems) => checker.check_tuple_index(&elems, index),
        Type::Bytes => {
            checker
                .errors
                .push(error_at("cannot index into bytes", span));
            Type::Unit
        }
        Type::Var(_) => {
            checker.errors.push(error_at(
                "cannot index into a value whose type could not be inferred",
                span,
            ));
            Type::Unit
        }
        other => {
            checker.errors.push(error_at(
                format!("cannot index into a value of type `{other}`"),
                span,
            ));
            Type::Unit
        }
    }
}

fn check_expr_fmt(checker: &mut Checker, parts: &[FmtPart]) -> Type {
    let _ = (checker, parts);
    Type::Str
}

fn check_expr_array(checker: &mut Checker, elems: &[Expr]) -> Type {
    let mut elem_types = Vec::new();
    for elem in elems {
        let et = checker.check_expr(elem);
        elem_types.push(et);
    }
    // Unify all element types
    let merged = if elem_types.is_empty() {
        Type::Unit
    } else {
        checker.merge_types(elem_types)
    };
    Type::Array(Box::new(merged))
}

fn check_expr_tuple(checker: &mut Checker, items: &[Expr]) -> Type {
    if items.is_empty() {
        // An empty tuple `()` is unit (#712): match arms like
        // `.err(_) => ()` must type-check as unit, not `tuple([])`.
        return Type::Unit;
    }
    let item_types: Vec<Type> = items.iter().map(|item| checker.check_expr(item)).collect();
    Type::Tuple(item_types)
}

fn check_expr_dict(checker: &mut Checker, entries: &[(Expr, Expr)]) -> Type {
    let mut key_types = Vec::new();
    let mut val_types = Vec::new();
    for (k, v) in entries {
        key_types.push(checker.check_expr(k));
        val_types.push(checker.check_expr(v));
    }
    Type::Dict(
        Box::new(checker.merge_types(key_types)),
        Box::new(checker.merge_types(val_types)),
    )
}

fn check_expr_list_comp(
    checker: &mut Checker,
    body: &Expr,
    var: &Ident,
    iter: &Expr,
    filter: &Option<Box<Expr>>,
    span: Span,
) -> Type {
    let it = checker.check_expr(iter);
    let it = checker.unifier.resolve(&it);
    let elem = match it {
        Type::Array(elem) => *elem,
        Type::Range(elem) => *elem,
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
    checker.define_at(&var.name, elem, var.span);
    if let Some(f) = filter {
        let ft = checker.check_expr(f);
        let ft = checker.unifier.resolve(&ft);
        if let Err(e) = checker.unifier.unify(&ft, &Type::Bool) {
            checker.report_mismatch(e, f.span());
        }
    }
    let body_t = checker.check_expr(body);
    checker.pop_scope();
    Type::Array(Box::new(body_t))
}

fn check_expr_break(checker: &mut Checker, span: Span) -> Type {
    if checker.loop_depth == 0 {
        checker
            .errors
            .push(error_at("`break` outside of a loop", span));
    }
    Type::Unit
}

fn check_expr_continue(checker: &mut Checker, span: Span) -> Type {
    if checker.loop_depth == 0 {
        checker
            .errors
            .push(error_at("`continue` outside of a loop", span));
    }
    Type::Unit
}

fn check_expr_variant(
    checker: &mut Checker,
    name: &str,
    arg: &Option<Box<Expr>>,
    span: Span,
) -> Type {
    let arg_t = arg.as_ref().map(|a| checker.check_expr(a));
    match (name, arg_t) {
        ("ok", Some(t)) => Type::Result(Box::new(t), Box::new(checker.unifier.fresh_var())),
        ("ok", None) => {
            checker
                .errors
                .push(error_at("`.ok` requires an argument", span));
            Type::Result(Box::new(Type::Unit), Box::new(checker.unifier.fresh_var()))
        }
        ("err", Some(e)) => Type::Result(Box::new(checker.unifier.fresh_var()), Box::new(e)),
        ("err", None) => {
            checker
                .errors
                .push(error_at("`.err` requires an argument", span));
            Type::Result(Box::new(checker.unifier.fresh_var()), Box::new(Type::Unit))
        }
        ("some", Some(t)) => Type::Option(Box::new(t)),
        ("some", None) => {
            checker
                .errors
                .push(error_at("`.some` requires an argument", span));
            Type::Option(Box::new(Type::Unit))
        }
        ("none", None) => Type::Option(Box::new(checker.unifier.fresh_var())),
        ("none", Some(_)) => {
            checker
                .errors
                .push(error_at("`.none` takes no argument", span));
            Type::Option(Box::new(checker.unifier.fresh_var()))
        }
        _ => {
            checker
                .errors
                .push(error_at(format!("unknown variant `.{name}`"), span));
            Type::Unit
        }
    }
}

fn check_expr_if(
    checker: &mut Checker,
    cond: &Expr,
    then: &Block,
    els: &Option<Box<Expr>>,
    span: Span,
) -> Type {
    let ct = checker.check_expr(cond);
    let glob_hint = match cond {
        Expr::Str { value, .. } if Checker::looks_like_glob(value) => Some(value.as_str()),
        _ => None,
    };
    checker.ensure_bool(ct, cond.span(), glob_hint);
    let tt = checker.check_block(then);
    match els {
        Some(e) => {
            let et = checker.check_expr(e);
            let tt_r = checker.unifier.resolve(&tt);
            let et_r = checker.unifier.resolve(&et);
            match (tt_r, et_r) {
                (Type::Never, Type::Never) => Type::Never,
                (Type::Never, _) => et,
                (_, Type::Never) => tt,
                _ => {
                    if let Err(err) = checker.unifier.unify(&et, &tt) {
                        checker.report_mismatch(err, e.span());
                    }
                    tt
                }
            }
        }
        None => {
            if !<Checker as TypeCheckOps>::block_has_return(then) {
                let tt_r = checker.unifier.resolve(&tt);
                match tt_r {
                    Type::Unit | Type::Never | Type::Var(_) => {
                        if let Err(err) = checker.unifier.unify(&Type::Unit, &tt) {
                            checker.report_mismatch(err, span);
                        }
                    }
                    _ => {
                        let tail = then.stmts.last().map(|s| s.span()).unwrap_or(span);
                        checker.errors.push(
                            error_at(
                                format!(
                                    "value of type `{tt_r}` discarded: `if` without `else` used as a statement must end in `unit`"
                                ),
                                tail,
                            )
                            .with_note(
                                "assign the value or end the branch with a statement of type `unit`",
                            ),
                        );
                    }
                }
            }
            Type::Unit
        }
    }
}

fn check_expr_if_let(
    checker: &mut Checker,
    pat: &Pattern,
    value: &Expr,
    then: &Block,
    els: &Option<Box<Expr>>,
    span: Span,
) -> Type {
    let vt = checker.check_expr(value);
    checker.push_scope();
    checker.bind_pattern(pat, &vt);
    let tt = checker.check_block(then);
    checker.pop_scope();
    match els {
        Some(e) => {
            let et = checker.check_expr(e);
            let tt_r = checker.unifier.resolve(&tt);
            let et_r = checker.unifier.resolve(&et);
            match (tt_r, et_r) {
                (Type::Never, Type::Never) => Type::Never,
                (Type::Never, _) => et,
                (_, Type::Never) => tt,
                _ => {
                    if let Err(err) = checker.unifier.unify(&et, &tt) {
                        checker.report_mismatch(err, e.span());
                    }
                    tt
                }
            }
        }
        None => {
            let tt_r = checker.unifier.resolve(&tt);
            match tt_r {
                Type::Unit | Type::Never | Type::Var(_) => {
                    if let Err(err) = checker.unifier.unify(&Type::Unit, &tt) {
                        checker.report_mismatch(err, span);
                    }
                }
                _ => {
                    // Same discarded-tail rule as `if` (#388).
                    let tail = then.stmts.last().map(|s| s.span()).unwrap_or(span);
                    checker.errors.push(
                        error_at(
                            format!(
                                "value of type `{tt_r}` discarded: `if let` without `else` used as a statement must end in `unit`"
                            ),
                            tail,
                        )
                        .with_note(
                            "assign the value or end the branch with a statement of type `unit`",
                        ),
                    );
                }
            }
            Type::Unit
        }
    }
}

fn check_expr_slice(
    checker: &mut Checker,
    obj: &Expr,
    start: &Option<Box<Expr>>,
    end: &Option<Box<Expr>>,
    span: Span,
) -> Type {
    let ot = checker.check_expr(obj);
    let ot = checker.unifier.resolve(&ot);
    for bound in [start.as_deref(), end.as_deref()].into_iter().flatten() {
        let bt = checker.check_expr(bound);
        checker.ensure_int(bt, bound.span());
    }
    match ot {
        Type::Array(elem) => Type::Array(elem),
        Type::Bytes => Type::Bytes,
        Type::Str => Type::Str,
        Type::Var(_) => {
            checker.errors.push(error_at(
                "cannot slice a value whose type could not be inferred",
                span,
            ));
            Type::Unit
        }
        Type::Error => Type::Unit,
        other => {
            checker.errors.push(error_at(
                format!("cannot slice a value of type `{other}`"),
                span,
            ));
            Type::Unit
        }
    }
}

fn check_expr_while(checker: &mut Checker, cond: &Expr, body: &Block) -> Type {
    let ct = checker.check_expr(cond);
    let glob_hint = match cond {
        Expr::Str { value, .. } if Checker::looks_like_glob(value) => Some(value.as_str()),
        _ => None,
    };
    checker.ensure_bool(ct, cond.span(), glob_hint);
    checker.loop_depth += 1;
    checker.check_block_discarded(body);
    checker.loop_depth -= 1;
    Type::Unit
}
