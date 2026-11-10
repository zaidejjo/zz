//! Call-expression type-checking helpers extracted from `check_call`.
//! Free functions following the `definite_assignment.rs` pattern.

use crate::ScopeOps as _;
use crate::TypeCheckOps;
use zz_checker_core::Checker;
use zz_diag_core::{error_at, FixIt};
use zz_http_routes::is_route_method;
use zz_infer::{subst as infer_subst, InferOps};
use zz_lints::http_lint::HttpLintOps;
use zz_sigs::enums::EnumOps;
use zz_span_ast::ast::{Expr, Ident};
use zz_span_ast::span::Span;
use zz_structs::StructOps;
use zz_suggest::suggest_all;
use zz_types::Type;
use zz_unify::Unifier;

/// Check math constants are values, not functions: `math.PI()` is always an
/// error — use bare `math.PI`.
pub(crate) fn check_call_math_const(
    checker: &mut Checker,
    name: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    if checker.is_math_const(name) && checker.funcs.contains_key(name) {
        checker.used_names.insert(name.to_string());
        for arg in args {
            checker.check_expr(arg);
        }
        for (_, val) in named {
            checker.check_expr(val);
        }
        checker.errors.push(error_at(
            format!("cannot call `{name}`: it is a numeric constant value, remove the `()`"),
            span,
        ));
        return Some(Type::Float);
    }
    None
}

/// Removed/renamed stdlib APIs, checked table-driven so each arm doesn't
/// repeat the "check args, report error, return type" boilerplate.
/// Entries are consulted only when `name` does not resolve to a known
/// function (so real `pg.query` after `import std.sqlz.postgres as pg`
/// still resolves through the signature table).
static REMOVED_APIS: &[RemovedApi] = &[
    RemovedApi {
        names: &["vec.append", "std.vec.append", "append"],
        replacement: "vec.push(v, x) or the method form xs.push(x)",
        ret: RemovedRet::Array,
    },
    RemovedApi {
        names: &["pg.query", "postgres.query", "my.query", "mysql.query"],
        replacement: "pg.query(..) / my.query(..)",
        ret: RemovedRet::Array,
    },
    RemovedApi {
        names: &["pg.exec", "postgres.exec", "my.exec", "mysql.exec"],
        replacement: "pg.query(..) / my.query(..)",
        ret: RemovedRet::Int,
    },
    RemovedApi {
        names: &["std.env.unset", "env.unset"],
        replacement: "env.remove(key)",
        ret: RemovedRet::Unit,
    },
    RemovedApi {
        names: &[
            "std.fs.read_file",
            "fs.read_file",
            "std.fs.read_to_string",
            "fs.read_to_string",
        ],
        replacement: "std.fs.read",
        ret: RemovedRet::ResultStrStr,
    },
    RemovedApi {
        names: &["std.fs.write_file", "fs.write_file"],
        replacement: "std.fs.write",
        ret: RemovedRet::ResultUnitStr,
    },
    RemovedApi {
        names: &["std.fs.remove_file", "fs.remove_file"],
        replacement: "std.fs.remove",
        ret: RemovedRet::ResultUnitStr,
    },
    RemovedApi {
        names: &["std.fs.readdir", "fs.readdir"],
        replacement: "std.fs.read_dir",
        ret: RemovedRet::ResultArrayStr,
    },
    RemovedApi {
        names: &["std.fs.rename", "fs.rename"],
        replacement: "fs.move(src, dst)",
        ret: RemovedRet::ResultUnitStr,
    },
    RemovedApi {
        names: &["std.net.tcp_close", "net.tcp_close", "tcp_close"],
        replacement: "stream.close() / stream.shutdown()",
        ret: RemovedRet::ResultUnitStr,
    },
];

struct RemovedApi {
    names: &'static [&'static str],
    replacement: &'static str,
    ret: RemovedRet,
}

#[derive(Clone, Copy)]
enum RemovedRet {
    Unit,
    Int,
    Array,
    /// `Result<Str, Str>`
    ResultStrStr,
    /// `Result<Unit, Str>`
    ResultUnitStr,
    /// `Result<Array<Str>, Str>`
    ResultArrayStr,
}

impl RemovedRet {
    fn to_type(self, unifier: &mut Unifier) -> Type {
        match self {
            RemovedRet::Unit => Type::Unit,
            RemovedRet::Int => Type::Int,

            RemovedRet::Array => Type::Array(Box::new(unifier.fresh_var())),
            RemovedRet::ResultStrStr => Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
            RemovedRet::ResultUnitStr => Type::Result(Box::new(Type::Unit), Box::new(Type::Str)),
            RemovedRet::ResultArrayStr => Type::Result(
                Box::new(Type::Array(Box::new(Type::Str))),
                Box::new(Type::Str),
            ),
        }
    }
}

/// Check removed/renamed stdlib APIs. Returns `Some(Type)` when `name` is a
/// removed API (args are still checked so nested errors surface), `None`
/// otherwise.
pub(crate) fn check_call_removed_api(
    checker: &mut Checker,
    name: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    if checker.funcs.contains_key(name) {
        return None;
    }
    let api = REMOVED_APIS.iter().find(|api| api.names.contains(&name))?;
    // Still check the args so nested errors inside them surface, then
    // stop: the name no longer resolves, and letting lookup run on would
    // add a confusing second error.
    for arg in args {
        checker.check_expr(arg);
    }
    for (_, val) in named {
        checker.check_expr(val);
    }
    checker.errors.push(error_at(
        format!(
            "removed: `{name}` is gone — use {} instead",
            api.replacement
        ),
        span,
    ));
    Some(api.ret.to_type(&mut checker.unifier))
}

/// True when `name` is `print` or `println`.
pub(crate) fn is_print_builtin(name: &str) -> bool {
    matches!(name, "print" | "println")
}

/// Handle `print`/`println` builtin calls: variadic, checks each argument,
/// warns about bare function values passed as arguments. Returns `Some(Unit)`
/// if `name` is a print builtin and no local shadows it, `None` otherwise.
pub(crate) fn check_call_print_builtin(
    checker: &mut Checker,
    name: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    _span: Span,
) -> Option<Type> {
    if !is_print_builtin(name) || checker.local_lookup_opt(name).is_some() {
        return None;
    }
    checker.used_names.insert(name.to_string());
    for arg in args {
        checker.check_expr(arg);
    }
    for (_, val) in named {
        checker.check_expr(val);
    }
    // A bare function value as any print argument is always a missing
    // `()` (`println(env.os)` would print the function itself instead
    // of calling it). Catch it here with the name attached.
    for arg_expr in args.iter().chain(named.iter().map(|(_, v)| v)) {
        // Resolve silently (lookup_opt, never check_expr: the argument
        // was already checked above and a second pass would duplicate
        // diagnostics).
        let (arg_t, fname) = match arg_expr {
            Expr::Ident { name: n, .. } => (checker.lookup_opt(n), Some(n.clone())),
            Expr::Path { parts, .. } => {
                let joined = parts.join(".");
                (checker.lookup_opt(&joined), Some(joined))
            }
            _ => (None, None),
        };
        let is_func = matches!(
            arg_t.as_ref().map(|t| checker.unifier.resolve(t)),
            Some(Type::Func(_, _))
        ) || matches!(fname.as_deref(), Some(n)
            if zz_checker_core::lookup_std(&checker.funcs, n).is_some()
                && !checker.is_math_const(n));
        if !is_func {
            continue;
        }
        let Some(fname) = fname else { continue };
        let mut diag = error_at(
            format!("cannot print function `{fname}`: call it with arguments"),
            arg_expr.span(),
        );
        diag = diag.with_note(format!("did you mean `{fname}()`?"));
        diag = diag.with_fixit(FixIt::safe(
            Span::new(arg_expr.span().end, arg_expr.span().end),
            "()".to_string(),
            "call function",
        ));
        checker.errors.push(diag);
    }
    // #713: print/println does not take named arguments. ZZ interpolates with
    // `{name}` inside the string instead:
    //   println("{path}\t{size}", path=path, size=size)   // wrong
    //   println("path={path} size={size}")                  // right
    if !named.is_empty() {
        let names: Vec<String> = named.iter().map(|(ident, _)| ident.name.clone()).collect();
        let mut diag = error_at(
            format!(
                "`{name}` does not take named arguments; interpolate with `{{name}}` instead",
                name = name
            ),
            named
                .first()
                .map(|(ident, _)| ident.span)
                .unwrap_or(Span::new(0, 0)),
        );
        diag = diag.with_note(format!(
            "use `{}={{{}}}` inside the string, e.g. `println(\"{}\")`",
            names.join(", "),
            names[0],
            names
                .iter()
                .map(|n| format!("{n}={{{n}}}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        checker.errors.push(diag);
    }
    Some(Type::Unit)
}

/// Handle a `Field` callee (`obj.method(...)`): type-check the receiver,
/// resolve the method signature against the receiver's type (builtins,
/// struct/enum impls, embedded promotion), lint HTTP route/param usage,
/// and delegate to `check_method_call`. Returns `Some(type)` if a method
/// was dispatched, `None` to fall through to direct-name handling.
pub(crate) fn check_call_field_callee(
    checker: &mut Checker,
    obj: &Expr,
    method: String,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    let recv_t = checker.check_expr(obj);
    let recv_t = checker.unifier.resolve(&recv_t);
    // Phase 2.2: `std.http` route + param lint on typed receivers.
    if matches!(checker.unifier.resolve(&recv_t), Type::HttpServer) && is_route_method(&method) {
        let root = match obj {
            Expr::Ident { name, .. } => Some(name.clone()),
            _ => None,
        };
        checker.lint_http_route_method(root, &method, args, span);
    } else if method == "param" && matches!(checker.unifier.resolve(&recv_t), Type::HttpRequest) {
        // Field-call args exclude the receiver: `req.param(name)`.
        checker.lint_http_param(args.first());
    }
    // Dot-call order for a `Field` callee (`makePoint().get()`):
    // the receiver's own `impl` method (`{Type}.{method}`) wins
    // over a same-named receiver-implicit free function (#500).
    // The free signature is restored below when nothing matches,
    // so its mismatch diagnostics are unchanged.
    let impl_owned = checker.receiver_impl_method(&recv_t, &method);
    let free_sig = zz_checker_core::lookup_std(&checker.funcs, &method).cloned();
    let mut sig = if impl_owned { None } else { free_sig.clone() };
    if sig.is_none() {
        sig = checker.resolve_method_sig(&recv_t, &method);
    }
    // Embedded promotion: `u.area()` dispatches to `Base.area`
    // when `User` embeds `Base`. The runtime passes the embedded
    // value as the receiver, so the outer type is not unified
    // against the method's receiver below.
    let mut promoted_recv: Option<Type> = None;
    if sig.is_none() {
        if let Type::Struct(sname, sargs) = checker.unifier.resolve(&recv_t) {
            if let Some((defining, psig)) = checker.find_struct_method(&sname, &method) {
                promoted_recv = checker
                    .promoted_method_receiver(&sname, &sargs, &defining)
                    .or_else(|| Some(Type::Struct(defining, Vec::new())));
                sig = Some(psig);
            }
        }
    }
    // No method matched: restore the free signature so the call
    // reports a mismatch against it rather than falling through.
    if sig.is_none() {
        sig = free_sig;
    }
    sig.map(|sig| {
        checker.check_method_call(&crate::MethodCall {
            sig: &sig,
            recv_t: &recv_t,
            promoted_recv,
            method: &method,
            args,
            named,
            span,
        })
    })
}

/// Check `sqlz.query` / `sqlz.exec` / `sqlz.transaction` / `pg.*` / `mysql.*`
/// direct-name calls. Returns `Some(Type)` when handled, `None` otherwise.
pub(crate) fn check_call_sqlz(
    checker: &mut Checker,
    name: &str,
    callee: &Expr,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    let is_pg_call = name == "pg.query"
        || name == "pg.exec"
        || name == "postgres.query"
        || name == "postgres.exec"
        || name == "std.sqlz.postgres.query"
        || name == "std.sqlz.postgres.exec"
        || name == "my.query"
        || name == "my.exec"
        || name == "mysql.query"
        || name == "mysql.exec"
        || name == "std.sqlz.mysql.query"
        || name == "std.sqlz.mysql.exec";
    if !name.starts_with("sqlz") && !name.starts_with("std.sqlz") && !is_pg_call {
        return None;
    }
    let sig = zz_checker_core::lookup_std(&checker.funcs, name).cloned()?;
    // Only take this path when the leading component really
    // is the module namespace (NOT a local variable).
    let recv_is_module = match callee {
        Expr::Path { parts, .. } => parts.len() >= 2 && checker.lookup_opt(&parts[0]).is_none(),
        _ => false,
    };
    if !recv_is_module {
        return None;
    }
    checker.used_names.insert(name.to_string());
    let (ps, ret, subs) = checker.instantiate(&sig);
    let pnames: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
    checker.check_args_against(
        Some(short_name(name)),
        &pnames,
        &ps,
        &sig.has_default,
        args,
        named,
        span,
    );
    // Explicit-receiver forms (`pg.query(db, sql)`,
    // `sqlz.query(db, sql)`) carry the SQL second;
    // the bare method-namespace form carries it first.
    // Skip for transaction — second arg is a closure, not SQL.
    let is_tx_call = name == "sqlz.transaction" || name == "std.sqlz.transaction";
    if !is_tx_call {
        let sql_idx = if is_pg_call || args.len() >= 2 { 1 } else { 0 };
        if let Some(sql_arg) = args.get(sql_idx) {
            checker.verify_sql_params(sql_arg, span);
        }
    }
    checker.validate_bounds(&sig, &subs, span);
    Some(ret)
}

/// Short display name for call diagnostics: `math.sin` → `sin`.
fn short_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

#[allow(clippy::too_many_arguments)]
/// Check `input` / `range` builtins. Returns `Some(Type)` when handled, `None` otherwise.
pub(crate) fn check_call_input_range(
    checker: &mut Checker,
    name: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    sig: &zz_checker_core::FuncSig,
    ret: Type,
    subs: &std::collections::HashMap<String, Type>,
    span: Span,
) -> Option<Type> {
    if name != "input" && name != "range" {
        return None;
    }
    checker.used_names.insert(name.to_string());
    if name == "input" {
        if args.len() + named.len() > 1 {
            checker.errors.push(error_at(
                format!(
                    "expected 0 or 1 arguments, found {}",
                    args.len() + named.len()
                ),
                span,
            ));
        } else if args.len() + named.len() == 1 {
            let arg_expr = if !args.is_empty() {
                &args[0]
            } else {
                &named[0].1
            };
            let at = checker.check_expr(arg_expr);
            if let Err(e) = checker.unifier.unify(&at, &Type::Str) {
                checker.report_mismatch(e, arg_expr.span());
            }
        }
    }
    if name == "range" {
        let total = args.len() + named.len();
        if total == 0 || total > 3 {
            checker.errors.push(error_at(
                format!("range expects 1, 2, or 3 arguments, found {total}"),
                span,
            ));
        } else {
            for arg in args {
                let at = checker.check_expr(arg);
                if let Err(e) = checker.unifier.unify(&at, &Type::Int) {
                    checker.report_mismatch(e, arg.span());
                }
            }
            for (_, val) in named {
                let at = checker.check_expr(val);
                if let Err(e) = checker.unifier.unify(&at, &Type::Int) {
                    checker.report_mismatch(e, val.span());
                }
            }
        }
    }
    checker.validate_bounds(sig, subs, span);
    Some(ret)
}

/// Smart suggestion for unknown qualified function calls with a one-edit
/// fixit when unambiguous. Returns `Some(Type::Unit)` when a suggestion was
/// reported, `None` otherwise.
pub(crate) fn check_call_unknown_suggestion(
    checker: &mut Checker,
    name: &str,
    callee: &Expr,
    span: Span,
) -> Option<Type> {
    let head = name.split('.').next().unwrap_or(name);
    let head_is_local = checker.env.iter().rev().any(|s| s.contains_key(head));
    if checker.funcs.contains_key(name)
        || !matches!(callee, Expr::Path { .. })
        || !name.starts_with("std.")
        || head_is_local
    {
        return None;
    }
    let candidates: Vec<String> = checker
        .funcs
        .keys()
        .flat_map(|k| {
            let mut v = vec![k.clone()];
            if let Some(bare) = k.rsplit('.').next() {
                if bare != k {
                    v.push(bare.to_string());
                }
            }
            v
        })
        .collect();
    let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
    let all = suggest_all(name, &refs);
    if all.is_empty() {
        return None;
    }
    let is_unambiguous = all.len() == 1 && all[0].1 <= 1;
    let callee_span = match callee {
        Expr::Path { span, .. } => *span,
        _ => span,
    };
    if is_unambiguous {
        checker.errors.push(
            error_at(format!("unknown function `{name}`"), callee_span)
                .with_note(format!("did you mean `{}`?", all[0].0))
                .with_fixit(FixIt::safe(
                    callee_span,
                    all[0].0.to_string(),
                    "replace with suggested function",
                )),
        );
    } else {
        checker.errors.push(
            error_at(format!("unknown function `{name}`"), callee_span)
                .with_note(format!("did you mean `{}`?", all[0].0)),
        );
    }
    Some(Type::Unit)
}

pub(crate) fn check_call_path_method(
    checker: &mut Checker,
    callee: &Expr,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    let Expr::Path { parts, span: pspan } = callee else {
        return None;
    };
    if parts.len() < 2 {
        return None;
    }
    let joined = parts.join(".");
    if let Some(t) = check_call_path_enum_construction(checker, parts, args, named, span) {
        return Some(t);
    }
    if parts.len() >= 3 {
        if let Some(t) = check_call_path_variant_method(checker, parts, args, named, *pspan, span) {
            return Some(t);
        }
    }
    if let Some(t) = check_call_path_local_var(checker, callee, &joined, args, named, span) {
        return Some(t);
    }
    let method = parts.last().unwrap();
    let recv_t = checker.lookup_path(&parts[..parts.len() - 1], *pspan);
    if let Some(t) =
        check_call_path_type_dispatch(checker, &recv_t, method, args, named, *pspan, span)
    {
        return Some(t);
    }
    None
}

/// Enum construction for a path callee: `Token.IntLit(1)` builds a value,
/// not a function invocation. Returns `Some(Type)` when handled, `None`
/// when it is not an enum construction or the name resolves to a value.
fn check_call_path_enum_construction(
    checker: &mut Checker,
    parts: &[String],
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    let enum_head = parts[..parts.len() - 1].join(".");
    let canonical_head = checker.canonical_enum_name(&enum_head);
    if !checker.enums.contains_key(&canonical_head)
        || checker.funcs.contains_key(&parts.join("."))
        || checker.lookup_opt(&enum_head).is_some()
    {
        return None;
    }
    let variant = parts.last().cloned().unwrap_or_default();
    checker.check_enum_construction(&canonical_head, &variant, args, named, span)
}

/// Method call on a constructed variant: `Token.IntLit(1).add(2)` — the
/// first arg fills the payload, the rest go to the method.
fn check_call_path_variant_method(
    checker: &mut Checker,
    parts: &[String],
    args: &[Expr],
    named: &[(Ident, Expr)],
    pspan: Span,
    span: Span,
) -> Option<Type> {
    let enum_head = parts[..parts.len() - 2].join(".");
    let canonical_head = checker.canonical_enum_name(&enum_head);
    if !checker.enums.contains_key(&canonical_head)
        || checker.funcs.contains_key(&parts.join("."))
        || checker.lookup_opt(&enum_head).is_some()
    {
        return None;
    }
    let variant = parts[parts.len() - 2].clone();
    let method = parts.last().cloned().unwrap_or_default();
    let _pv = checker.enum_variant_payload(&canonical_head, &variant, pspan);
    match _pv {
        Some(Some(pty)) => check_call_path_variant_payload(
            checker,
            &canonical_head,
            &variant,
            &method,
            pty,
            args,
            named,
            span,
        ),
        Some(None) => {
            let recv = Expr::Path {
                parts: parts[..parts.len() - 1].to_vec(),
                span: pspan,
            };
            let field_callee = Expr::Field {
                obj: Box::new(recv),
                name: parts.last().cloned().unwrap_or_default(),
                span: pspan,
            };
            Some(checker.check_call(&field_callee, args, named, span))
        }
        None => {
            let (gen_vars, _) = checker.fresh_enum_vars(&canonical_head);
            Some(Type::Enum(canonical_head.to_string(), gen_vars))
        }
    }
}

/// Variant method with a payload: first arg fills the payload, rest go to
/// the method.
#[allow(clippy::too_many_arguments)]
fn check_call_path_variant_payload(
    checker: &mut Checker,
    canonical_head: &str,
    variant: &str,
    method: &str,
    pty: Type,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    if args.is_empty() {
        checker.errors.push(error_at(
            format!(
                "variant `{canonical_head}.{variant}` holds a value: pass it before the method arguments (e.g. `{canonical_head}.{variant}(v).{method}(...)`)"
            ),
            span,
        ));
        let (gen_vars, _) = checker.fresh_enum_vars(canonical_head);
        return Some(Type::Enum(canonical_head.to_string(), gen_vars));
    }
    let (gen_vars, gen_map) = checker.fresh_enum_vars(canonical_head);
    let at = checker.check_expr(&args[0]);
    let exp = infer_subst(&pty, &gen_map);
    if let Err(e) = checker.unifier.unify(&at, &exp) {
        checker.report_mismatch(e, args[0].span());
    }
    let recv_args: Vec<Type> = gen_vars
        .iter()
        .map(|v| checker.unifier.resolve_deep(v))
        .collect();
    let recv_t = Type::Enum(canonical_head.to_string(), recv_args);
    let mut sig = checker
        .funcs
        .get(&format!("{canonical_head}.{method}"))
        .cloned();
    if sig.is_none() {
        if let Some((ns, _)) = canonical_head.rsplit_once('.') {
            sig = zz_checker_core::lookup_std(&checker.funcs, &format!("{ns}.{method}")).cloned();
        }
    }
    if let Some(sig) = sig {
        checker
            .used_names
            .insert(format!("{canonical_head}.{method}"));
        return Some(checker.check_method_call(&crate::MethodCall {
            sig: &sig,
            recv_t: &recv_t,
            promoted_recv: None,
            method,
            args: &args[1..],
            named,
            span,
        }));
    }
    checker.errors.push(error_at(
        format!("unknown method `{method}` for enum `{canonical_head}`"),
        span,
    ));
    let (gen_vars, _) = checker.fresh_enum_vars(canonical_head);
    Some(Type::Enum(canonical_head.to_string(), gen_vars))
}

/// Method call on a genuine local variable value: `db.exec(...)` on a
/// local `db`, never the seeded free function.
fn check_call_path_local_var(
    checker: &mut Checker,
    callee: &Expr,
    joined: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    if checker.method_shadow_call(callee, args, named) {
        return None;
    }
    let var_ty = checker.lookup_opt(joined)?;
    let callee_t = checker.unifier.resolve(&var_ty);
    match &callee_t {
        Type::Func(..) | Type::Named(..) => {
            let pnames: Vec<String> = (0..args.len()).map(|i| format!("_{i}")).collect();
            match callee_t {
                Type::Func(ps, ret) => {
                    checker.check_args_against(None, &pnames, &ps, &[], args, named, span);
                    return Some(*ret);
                }
                Type::Named(ref nname) => {
                    if let Some(sig) = zz_checker_core::lookup_std(&checker.funcs, nname).cloned() {
                        let (ps, ret, subs) = checker.instantiate(&sig);
                        let pnames: Vec<String> =
                            sig.params.iter().map(|(n, _)| n.clone()).collect();
                        checker.check_args_against(
                            Some(short_name(nname)),
                            &pnames,
                            &ps,
                            &sig.has_default,
                            args,
                            named,
                            span,
                        );
                        checker.validate_bounds(&sig, &subs, span);
                        return Some(ret);
                    }
                }
                _ => {}
            }
            None
        }
        Type::Var(_) => {
            let arg_types: Vec<Type> = args.iter().map(|a| checker.check_expr(a)).collect();
            let ret_var = checker.unifier.fresh_var();
            let func_ty = Type::Func(arg_types, Box::new(ret_var.clone()));
            if let Err(e) = checker.unifier.unify(&var_ty, &func_ty) {
                checker.report_mismatch(e, span);
            }
            Some(checker.unifier.resolve(&ret_var))
        }
        _ => None,
    }
}

/// Type-based stdlib dispatch for `recv.method(...)` on a path callee.
fn check_call_path_type_dispatch(
    checker: &mut Checker,
    recv_t: &Type,
    method: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    pspan: Span,
    span: Span,
) -> Option<Type> {
    let recv_t_resolved = checker.unifier.resolve(recv_t);
    let impl_owned = checker.receiver_impl_method(&recv_t_resolved, method);
    let free_sig = zz_checker_core::lookup_std(&checker.funcs, method).cloned();
    let mut sig = if impl_owned { None } else { free_sig.clone() };
    if sig.is_none() {
        sig = check_call_path_stdlib_ns(checker, &recv_t_resolved, method);
    }
    // Embedded promotion.
    let mut promoted_recv: Option<Type> = None;
    if sig.is_none() {
        if let Type::Struct(sname, sargs) = checker.unifier.resolve(recv_t) {
            if let Some((defining, psig)) = checker.find_struct_method(&sname, method) {
                promoted_recv = checker
                    .promoted_method_receiver(&sname, &sargs, &defining)
                    .or_else(|| Some(Type::Struct(defining, Vec::new())));
                sig = Some(psig);
            }
        }
    }
    if sig.is_none() {
        sig = free_sig;
    }
    let sig = sig?;
    // sqlz method form.
    if (method == "query" || method == "exec" || method == "transaction")
        && matches!(checker.unifier.resolve(recv_t), Type::Db)
    {
        return check_call_path_sqlz_method(
            checker, &sig, recv_t, method, args, named, pspan, span,
        );
    }
    // Generic method dispatch.
    let (ps, ret, subs) = checker.instantiate(&sig);
    if ps.is_empty() {
        checker.errors.push(error_at(
            format!("method `{method}` takes no arguments"),
            span,
        ));
        return Some(Type::Unit);
    }
    if let Some(promoted) = promoted_recv {
        if let Err(e) = checker.unifier.unify(&promoted, &ps[0]) {
            checker.report_mismatch(e, pspan);
        }
    } else if let Err(e) = checker.unifier.unify(recv_t, &ps[0]) {
        checker.report_mismatch(e, pspan);
    }
    checker.check_args_against(
        None,
        &sig.params[1..]
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>(),
        &ps[1..],
        &[],
        args,
        named,
        span,
    );
    checker.validate_bounds(&sig, &subs, span);
    Some(ret)
}

/// Look up a stdlib namespace method for a typed receiver (e.g. `str.`,
/// `vec.`, `option.`, etc.).
fn check_call_path_stdlib_ns(
    checker: &mut Checker,
    recv_t: &Type,
    method: &str,
) -> Option<zz_checker_core::FuncSig> {
    match recv_t {
        Type::Str => zz_checker_core::lookup_std(&checker.funcs, &format!("str.{method}")).cloned(),
        Type::Array(_) => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("vec.{method}")).cloned()
        }
        Type::Option(_) => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("option.{method}")).cloned()
        }
        Type::Result(_, _) => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("result.{method}")).cloned()
        }
        Type::Int => zz_checker_core::lookup_std(&checker.funcs, &format!("int.{method}")).cloned(),
        Type::Float => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("float.{method}")).cloned()
        }
        Type::Bool => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("bool.{method}")).cloned()
        }
        Type::Response => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("http.{method}")).cloned()
        }
        Type::HttpRequest => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("http.{method}")).cloned()
        }
        Type::TcpStream => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("net.{method}")).cloned()
        }
        Type::TcpListener => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("net.{method}")).cloned()
        }
        Type::HttpServer => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("http.{method}")).cloned()
        }
        Type::Json => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("json.{method}")).cloned()
        }
        Type::Db => zz_checker_core::lookup_std(&checker.funcs, &format!("sqlz.{method}")).cloned(),
        Type::Chan => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("chan.{method}")).cloned()
        }
        Type::TaskJoin => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("task.{method}")).cloned()
        }
        Type::Opaque(tag) => {
            zz_checker_core::lookup_std(&checker.funcs, &format!("{tag}.{method}")).cloned()
        }
        Type::Struct(sname, _) => {
            let mut sig =
                zz_checker_core::lookup_std(&checker.funcs, &format!("{sname}.{method}")).cloned();
            if sig.is_none() {
                if let Some((ns, _)) = sname.rsplit_once('.') {
                    sig = zz_checker_core::lookup_std(&checker.funcs, &format!("{ns}.{method}"))
                        .cloned();
                }
            }
            sig
        }
        Type::Enum(ename, _) => {
            let mut sig =
                zz_checker_core::lookup_std(&checker.funcs, &format!("{ename}.{method}")).cloned();
            if sig.is_none() {
                if let Some((ns, _)) = ename.rsplit_once('.') {
                    sig = zz_checker_core::lookup_std(&checker.funcs, &format!("{ns}.{method}"))
                        .cloned();
                }
            }
            sig
        }
        _ => None,
    }
}

/// sqlz method form: `db.exec(sql)`, `db.query(sql)`,
/// `db.transaction(fn(tx) { ... })` — receiver is implicit.
#[allow(clippy::too_many_arguments)]
fn check_call_path_sqlz_method(
    checker: &mut Checker,
    sig: &zz_checker_core::FuncSig,
    recv_t: &Type,
    method: &str,
    args: &[Expr],
    named: &[(Ident, Expr)],
    pspan: Span,
    span: Span,
) -> Option<Type> {
    let (ps, ret, subs) = checker.instantiate(sig);
    if !ps.is_empty() {
        if let Err(e) = checker.unifier.unify(recv_t, &ps[0]) {
            checker.report_mismatch(e, pspan);
        }
    }
    let expected = ps.len().saturating_sub(1);
    let actual = args.len() + named.len();
    if actual != expected {
        checker.errors.push(error_at(
            format!("expected {expected} argument(s), found {actual}"),
            span,
        ));
    } else if expected >= 1 {
        for (i, arg) in args.iter().enumerate() {
            if ps.len() >= 2 + i {
                if let Expr::Closure {
                    params,
                    ret_ty,
                    body,
                    span: cspan,
                } = arg
                {
                    let expected_t = checker.unifier.resolve(&ps[1 + i]);
                    let ep = if let Type::Func(ep, _) = &expected_t {
                        Some(ep.as_slice())
                    } else {
                        None
                    };
                    let at = checker.check_closure(params, ret_ty.as_ref(), body, *cspan, ep);
                    if let Err(e) = checker.unifier.unify(&at, &ps[1 + i]) {
                        checker.report_mismatch(e, arg.span());
                    }
                } else {
                    let at = checker.check_expr(arg);
                    if let Err(e) = checker.unifier.unify(&at, &ps[1 + i]) {
                        checker.report_mismatch(e, arg.span());
                    }
                }
            }
        }
        if method == "query" || method == "exec" {
            checker.verify_sql_params(&args[0], span);
        }
    }
    checker.validate_bounds(sig, &subs, span);
    Some(ret)
}

/// Check a callee whose type is already known (Func / Named / Var / Error /
/// other). Returns the result type.
pub(crate) fn check_call_callee_type(
    checker: &mut Checker,
    callee: &Expr,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Type {
    let callee_t = checker.check_expr(callee);
    let callee_t = checker.unifier.resolve(&callee_t);
    match callee_t {
        Type::Func(ps, ret) => {
            let pnames: Vec<String> = (0..ps.len()).map(|i| format!("_{i}")).collect();
            checker.check_args_against(None, &pnames, &ps, &[], args, named, span);
            *ret
        }
        Type::Named(name) => match zz_checker_core::lookup_std(&checker.funcs, &name).cloned() {
            Some(sig) => {
                let (ps, ret, subs) = checker.instantiate(&sig);
                let param_names: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
                checker.check_args_against(
                    Some(short_name(&name)),
                    &param_names,
                    &ps,
                    &sig.has_default,
                    args,
                    named,
                    span,
                );
                checker.validate_bounds(&sig, &subs, span);
                ret
            }
            None => {
                // Sherlock: suggest the closest known function so a typo
                // (`lenght`, `get_var`) points at the fix instead of a dead end.
                let mut diag = error_at(format!("unknown function `{name}`"), span);
                let candidates: Vec<String> = checker
                    .funcs
                    .keys()
                    .flat_map(|k| {
                        let mut v = vec![k.clone()];
                        if let Some(bare) = k.rsplit('.').next() {
                            if bare != k {
                                v.push(bare.to_string());
                            }
                        }
                        v
                    })
                    .collect();
                let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                let all = suggest_all(&name, &refs);
                if let Some((suggestion, _)) = all.first() {
                    diag = diag.with_note(format!(
                        "did you mean to call `{suggestion}` instead of calling a value of type `{name}`?"
                    ));
                }
                checker.errors.push(diag);
                Type::Unit
            }
        },
        Type::Var(_) => {
            if !checker.had_undefined_var {
                checker.errors.push(error_at(
                    "cannot call a value whose type could not be inferred",
                    span,
                ));
            }
            Type::Error
        }
        Type::Error => Type::Error,
        other => {
            if !checker.had_undefined_var {
                checker.errors.push(error_at(
                    format!("cannot call a value of type `{other}`"),
                    span,
                ));
            }
            Type::Error
        }
    }
}

/// Resolve a selective-import alias (e.g. bare `squared` from `import m(squared)`
/// becomes `m.squared`). Returns the (possibly rewritten) callee expression.
pub(crate) fn check_call_resolve_alias(checker: &mut Checker, callee: &Expr) -> Expr {
    let Expr::Ident { name, span } = callee else {
        return callee.clone();
    };
    if checker.funcs.contains_key(name) || checker.env.iter().any(|s| s.contains_key(name)) {
        return callee.clone();
    }
    let Some(qualified) = checker.import_aliases.get(name).cloned() else {
        return callee.clone();
    };
    checker.used_names.insert(name.clone());
    Expr::Path {
        parts: qualified.split('.').map(str::to_string).collect(),
        span: *span,
    }
}

/// Handle direct-name calls: removed APIs, math constants, sqlz,
/// print builtin, standard functions, unknown suggestions.
/// Returns `Some(Type)` when handled, `None` to fall through to path
/// method dispatch or callee-type checking.
pub(crate) fn check_call_direct_name(
    checker: &mut Checker,
    direct_name: &Option<String>,
    callee: &Expr,
    args: &[Expr],
    named: &[(Ident, Expr)],
    span: Span,
) -> Option<Type> {
    let Some(name) = direct_name else {
        return None;
    };
    // Mark the callee as used so the unused-private-function warning
    // doesn't fire on legitimately-called functions.
    if checker.funcs.contains_key(name) {
        checker.used_names.insert(name.clone());
    }
    if let Some(ret) = check_call_removed_api(checker, name, args, named, span) {
        return Some(ret);
    }
    if let Some(t) = check_call_math_const(checker, name, args, named, span) {
        return Some(t);
    }
    if let Some(t) = check_call_sqlz(checker, name, callee, args, named, span) {
        return Some(t);
    }
    if let Expr::Path { parts, .. } = callee {
        checker.lint_http_path_call(parts, args, span);
    }
    if let Some(t) = check_call_print_builtin(checker, name, args, named, span) {
        return Some(t);
    }
    if !checker.method_shadow_call(callee, args, named) {
        if let Some(sig) = zz_checker_core::lookup_std(&checker.funcs, name).cloned() {
            checker.used_names.insert(name.clone());
            let (ps, ret, subs) = checker.instantiate(&sig);
            if let Some(t) =
                check_call_input_range(checker, name, args, named, &sig, ret.clone(), &subs, span)
            {
                return Some(t);
            }
            let pnames: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
            checker.check_args_against(
                Some(short_name(name)),
                &pnames,
                &ps,
                &sig.has_default,
                args,
                named,
                span,
            );
            checker.validate_bounds(&sig, &subs, span);
            return Some(ret);
        }
    }
    check_call_unknown_suggestion(checker, name, callee, span)
}
