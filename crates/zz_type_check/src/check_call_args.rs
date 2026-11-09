//! Call-argument checking helpers extracted from `check_args_against`.
//! Free functions following the `definite_assignment.rs` pattern.

use zz_checker_core::Checker;
use zz_diag_core::{error_at, FixIt};
use zz_infer::InferOps;
use zz_span_ast::ast::{Expr, Ident};
use zz_span_ast::span::Span;
use zz_suggest::suggest_all;
use zz_types::Type;

use crate::TypeCheckOps as _;

/// Validate that the number of provided arguments falls within the
/// `[allowed_min, total_params]` range. Reports an error and returns `true`
/// (caller should return early) when out of range.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_arg_count(
    checker: &mut Checker,
    callee: Option<&str>,
    param_names: &[String],
    ps: &[Type],
    has_default: &[bool],
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> bool {
    let total_provided = args.len() + named.len();
    let total_params = ps.len();
    let allowed_min = total_params - has_default.iter().filter(|&&d| d).count();

    if total_provided < allowed_min || total_provided > total_params {
        let sig = param_names
            .iter()
            .zip(ps.iter())
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        let count = if allowed_min == total_params {
            if total_params == 1 {
                "takes 1 argument".to_string()
            } else {
                format!("takes {total_params} arguments")
            }
        } else {
            format!("takes {allowed_min} to {total_params} arguments")
        };
        let head = match callee.filter(|c| !c.is_empty()) {
            Some(name) => format!("`{name}` {count}"),
            None => count,
        };
        let mut diag = error_at(format!("{head} ({sig}), found {total_provided}"), span);
        if total_provided < allowed_min {
            let missing: Vec<String> = param_names
                .iter()
                .zip(ps.iter())
                .enumerate()
                .filter(|(i, (n, _))| {
                    *i >= args.len() && !named.iter().any(|(an, _)| an.name == **n)
                })
                .map(|(_, (n, t))| format!("{n}: {t}"))
                .collect();
            if let Some(first) = missing.first() {
                diag = diag.with_note(format!("missing argument for `{first}`"));
            }
        }
        checker.errors.push(diag);
        return true;
    }
    false
}

/// Assign positional arguments to slots. Reports an error and returns `true`
/// (caller should return early) when a positional arg exceeds the parameter
/// count or conflicts with an already-filled slot.
pub(crate) fn assign_positional_slots<'a>(
    checker: &mut Checker,
    total_params: usize,
    args: &'a [Expr],
    slots: &mut [Option<&'a Expr>],
) -> bool {
    for (i, arg) in args.iter().enumerate() {
        if i >= total_params {
            checker.errors.push(error_at(
                format!("too many positional arguments (max {})", total_params),
                arg.span(),
            ));
            return true;
        }
        if slots[i].is_some() {
            checker.errors.push(error_at(
                format!("positional argument `{}` conflicts with named argument", i),
                arg.span(),
            ));
            return true;
        }
        slots[i] = Some(arg);
    }
    false
}

/// Check named arguments, filling slots or reporting unknown-parameter
/// errors with typo suggestions and fix-its. Returns `true` (caller should
/// return early) when an error occurs.
pub(crate) fn check_named_args<'a>(
    checker: &mut Checker,
    param_names: &[String],
    named: &'a [(Ident, Expr)],
    slots: &mut [Option<&'a Expr>],
) -> bool {
    for (name, val) in named {
        let pos = param_names.iter().position(|pn| pn == &name.name);
        match pos {
            Some(i) => {
                if slots[i].is_some() {
                    checker.errors.push(error_at(
                        format!("argument `{}` already provided positionally", name.name),
                        val.span(),
                    ));
                    return true;
                }
                slots[i] = Some(val);
            }
            None => {
                let mut diag = error_at(format!("unknown parameter `{}`", name.name), name.span);
                let refs: Vec<&str> = param_names.iter().map(|s| s.as_str()).collect();
                let all = suggest_all(&name.name, &refs);
                if let Some((suggestion, _)) = all.first() {
                    diag = diag.with_note(format!("did you mean `{suggestion}`?"));
                    let alts: Vec<String> = all.iter().map(|(s, _)| s.to_string()).collect();
                    let fixit = if alts.len() == 1 {
                        FixIt::safe(name.span, suggestion.to_string(), "replace parameter")
                    } else {
                        FixIt::ambiguous(
                            name.span,
                            suggestion.to_string(),
                            "replace parameter",
                            alts,
                        )
                    };
                    diag = diag.with_fixit(fixit);
                }
                checker.errors.push(diag);
                return true;
            }
        }
    }
    false
}

/// Type-check each filled slot, propagating expected parameter types into
/// closures and expanding named-function values where needed.
pub(crate) fn check_filled_slots(
    checker: &mut Checker,
    param_names: &[String],
    ps: &[Type],
    slots: &[Option<&Expr>],
) {
    for (i, slot) in slots.iter().enumerate() {
        if let Some(arg) = slot {
            // When passing a closure to a function with a known
            // signature, propagate expected param types so the body
            // can resolve method calls on known types (e.g. `tx.exec`
            // when `tx: Db`).  Without this, unannotated params get
            // fresh vars and method dispatch fails on `?0`.
            if let Expr::Closure {
                params,
                ret_ty,
                body,
                span,
            } = arg
            {
                let expected = checker.unifier.resolve(&ps[i]);
                let ep = if let Type::Func(ep, _) = &expected {
                    Some(ep.as_slice())
                } else {
                    None
                };
                let at = checker.check_closure(params, ret_ty.as_ref(), body, *span, ep);
                if let Err(e) = checker.unifier.unify(&at, &ps[i]) {
                    checker.report_mismatch_with_notes(
                        e,
                        arg.span(),
                        Checker::param_mismatch_notes(param_names, ps, i),
                    );
                }
            } else {
                let at = checker.check_expr(arg);
                let at = checker.unifier.resolve(&at);
                let expected = checker.unifier.resolve(&ps[i]);
                // A named function passed where a `func(...)` value is
                // expected (e.g. decorator application `dec(target)` or
                // HOFs like `map(xs, double)`): expand the signature so
                // structural unification applies instead of failing on
                // `Named` vs `Func`.
                let at = match (&at, &expected) {
                    (Type::Named(n), Type::Func(_, _)) => {
                        match zz_checker_core::lookup_std(&checker.funcs, n).cloned() {
                            Some(sig) => {
                                let (sps, sret, _) = checker.instantiate(&sig);
                                Type::Func(sps, Box::new(sret))
                            }
                            None => at,
                        }
                    }
                    _ => at,
                };
                if let Err(e) = checker.unifier.unify(&at, &expected) {
                    checker.report_mismatch_with_notes(
                        e,
                        arg.span(),
                        Checker::param_mismatch_notes(param_names, ps, i),
                    );
                }
            }
        }
    }
}
