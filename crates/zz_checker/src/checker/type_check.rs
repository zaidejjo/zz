//! Core type-checking logic: statements, expressions, patterns.

use crate::checker::inference::{contains_var, default_variant_vars};
use crate::checker::Checker;
use crate::type_::Type;
use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Lit, Param, Pattern, Stmt, Ty, UnOp};
use zz_frontend::diag::{error_at, FixIt};
use zz_frontend::levenshtein::suggest_all;
use zz_frontend::span::Span;

/// Cheap static sanity check over the literal SQL text: balanced
/// single/double quotes and parens. Returns an error message when the
/// text is clearly malformed; `None` means "looks plausible".
fn check_sql_static(text: &str) -> Option<String> {
    let mut single = false;
    let mut double = false;
    let mut depth: i32 = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '(' if !single && !double => depth += 1,
            ')' if !single && !double => {
                depth -= 1;
                if depth < 0 {
                    return Some("unbalanced `)` in SQL string".to_string());
                }
            }
            '-' if !single && !double && chars.peek() == Some(&'-') => {
                // `--` line comment: skip to end of line.
                for c2 in chars.by_ref() {
                    if c2 == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if single {
        return Some("unbalanced `'` in SQL string".to_string());
    }
    if double {
        return Some("unbalanced `\"` in SQL string".to_string());
    }
    if depth != 0 {
        return Some("unbalanced `(` in SQL string".to_string());
    }
    None
}

impl Checker {
    // --- statements -------------------------------------------------------

    pub(crate) fn check_stmt(&mut self, stmt: &Stmt) -> Type {
        self.had_undefined_var = false;
        match stmt {
            Stmt::Decl {
                name,
                ty,
                value,
                span: _,
                pub_: _,
                is_const,
            } => {
                // Pre-bind closures so recursive references resolve.
                if matches!(value, Expr::Closure { .. }) {
                    let fv = self.unifier.fresh_var();
                    self.define_var_at(&name.name, fv, name.span, *is_const);
                }
                let vt = self.check_expr(value);
                if let Some(ann) = ty {
                    let gens = self.current_generics.clone();
                    let at = self.ast_to_type(ann, &gens);
                    if let Err(e) = self.unifier.unify(&vt, &at) {
                        self.report_mismatch(e, ann.span);
                    }
                }
                let rt = self.unifier.resolve_deep(&vt);
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
                            self.errors.push(error_at(
                                format!(
                                    "cannot infer the type of `{}`; add a type annotation",
                                    name.name
                                ),
                                name.span,
                            ));
                        }
                    } else {
                        self.define_var_at(&name.name, d.clone(), name.span, *is_const);
                        if self.env.len() == 1 {
                            self.new_bindings.insert(name.name.clone(), d.clone());
                        }
                        return d;
                    }
                }
                if self.env.len() == 1 {
                    self.new_bindings.insert(name.name.clone(), vt.clone());
                }
                self.define_var_at(&name.name, rt.clone(), name.span, *is_const);
                rt
            }
            Stmt::Import {
                path,
                alias,
                items,
                span,
                pub_: _,
            } => {
                if items.is_empty() {
                    // Full module import: track namespace for unused-import warnings.
                    let ns = alias
                        .as_ref()
                        .cloned()
                        .or_else(|| path.last().cloned())
                        .unwrap_or_default();
                    self.imports.push((ns, *span));
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
                            zz_frontend::ast::ImportItem::Wildcard { .. } => {
                                // Wildcard: track the module namespace.
                                for k in self.funcs.keys().cloned().collect::<Vec<_>>() {
                                    if let Some(bare) = k.strip_prefix(&format!("{eff_ns}.")) {
                                        if !bare.is_empty() && !bare.contains('.') {
                                            self.import_aliases
                                                .entry(bare.to_string())
                                                .or_insert(k);
                                        }
                                    }
                                }
                                eff_ns.clone()
                            }
                            zz_frontend::ast::ImportItem::Named {
                                name,
                                alias: item_alias,
                                ..
                            } => {
                                let target = item_alias.clone().unwrap_or_else(|| name.clone());
                                self.import_aliases
                                    .entry(target.clone())
                                    .or_insert_with(|| format!("{eff_ns}.{name}"));
                                target
                            }
                        };
                        self.imports.push((name, *span));
                    }
                }
                Type::Unit
            }
            Stmt::Func { span, .. } => {
                let fname = Self::func_name(stmt);
                let sig = match self.funcs.get(&fname) {
                    Some(s) => s.clone(),
                    None => {
                        self.errors.push(zz_frontend::diag::error_at(
                            format!(
                                "nested function `{}` is not supported; use a closure (`|params| body`) instead",
                                fname
                            ),
                            *span,
                        ));
                        return Type::Unit;
                    }
                };
                // Extern functions have no ZZ body — already registered in Pass 1d.
                if sig.is_extern {
                    return Type::Unit;
                }
                self.check_func_body(stmt, &sig);
                Type::Unit
            }
            Stmt::ExternBlock { .. } => Type::Unit,
            Stmt::Link { lib, span } => {
                if lib.trim().is_empty() {
                    self.errors
                        .push(error_at("`@link` requires a non-empty library name", *span));
                } else if !self.link_libs.iter().any(|l| l == lib) {
                    self.link_libs.push(lib.clone());
                }
                Type::Unit
            }
            Stmt::Impl { name, methods, .. } => {
                let type_name = name.join(".");
                for method in methods {
                    if let Stmt::Func { .. } = method {
                        let method_name = Self::func_name(method);
                        let full_name = format!("{}.{}", type_name, method_name);
                        let sig = self.funcs.get(&full_name).unwrap().clone();
                        self.check_func_body(method, &sig);
                    }
                }
                Type::Unit
            }
            Stmt::Return { value, span } => {
                let ret = match self.current_ret.clone() {
                    Some(r) => r,
                    None => {
                        self.errors
                            .push(error_at("`return` outside of a function", *span));
                        Type::Unit
                    }
                };
                match value {
                    Some(v) => {
                        let vt = self.check_expr(v);
                        if let Err(e) = self.unifier.unify(&vt, &ret) {
                            self.report_mismatch(e, v.span());
                        }
                        vt
                    }
                    None => {
                        if let Err(e) = self.unifier.unify(&Type::Unit, &ret) {
                            self.report_mismatch(e, *span);
                        }
                        Type::Unit
                    }
                }
            }
            Stmt::Expr(e) => self.check_expr(e),
            Stmt::Struct { .. } => Type::Unit,
            Stmt::For {
                vars,
                iter,
                body,
                span,
            } => {
                let it = self.check_expr(iter);
                let it = self.unifier.resolve(&it);
                match vars.len() {
                    0 => unreachable!(),
                    1 => {
                        // `for x in collection` — single variable
                        let elem = match it {
                            Type::Array(elem) => *elem,
                            Type::Bytes => Type::Int,
                            Type::Range(elem) => *elem,
                            Type::Dict(_k, _v) => {
                                // for x in dict → iterates keys
                                *_k
                            }
                            Type::Var(_) => {
                                self.errors.push(error_at(
                                    "cannot iterate a value whose type could not be inferred",
                                    *span,
                                ));
                                Type::Unit
                            }
                            other => {
                                self.errors.push(error_at(
                                    format!("cannot iterate a value of type `{other}`"),
                                    *span,
                                ));
                                Type::Unit
                            }
                        };
                        self.push_scope();
                        self.define(&vars[0].name, elem);
                        self.loop_depth += 1;
                        self.check_block(body);
                        self.loop_depth -= 1;
                        self.pop_scope();
                    }
                    2 => {
                        // `for k, v in dict` — key-value pair — or
                        // `for i, x in xs.enumerate()` — index + element
                        // over an array of 2-tuples.
                        match it {
                            Type::Dict(k, v) => {
                                self.push_scope();
                                self.define(&vars[0].name, *k);
                                self.define(&vars[1].name, *v);
                                self.loop_depth += 1;
                                self.check_block(body);
                                self.loop_depth -= 1;
                                self.pop_scope();
                            }
                            Type::Array(elem) => {
                                match self.unifier.resolve(&elem) {
                                    Type::Tuple(pair) if pair.len() == 2 => {
                                        self.push_scope();
                                        self.define(&vars[0].name, pair[0].clone());
                                        self.define(&vars[1].name, pair[1].clone());
                                        self.loop_depth += 1;
                                        self.check_block(body);
                                        self.loop_depth -= 1;
                                        self.pop_scope();
                                    }
                                    Type::Var(_) => {
                                        // Element type not yet inferred
                                        // (e.g. generic): fresh vars; body
                                        // usage constrains them later.
                                        let k_var = self.unifier.fresh_var();
                                        let v_var = self.unifier.fresh_var();
                                        self.push_scope();
                                        self.define(&vars[0].name, k_var);
                                        self.define(&vars[1].name, v_var);
                                        self.loop_depth += 1;
                                        self.check_block(body);
                                        self.loop_depth -= 1;
                                        self.pop_scope();
                                    }
                                    other => {
                                        self.errors.push(error_at(
                                            format!(
                                                "expected a dictionary or an array of tuples for `for k, v in ...`, got `{other}`"
                                            ),
                                            *span,
                                        ));
                                        self.push_scope();
                                        self.define(&vars[0].name, Type::Unit);
                                        self.define(&vars[1].name, Type::Unit);
                                        self.loop_depth += 1;
                                        self.check_block(body);
                                        self.loop_depth -= 1;
                                        self.pop_scope();
                                    }
                                }
                            }
                            Type::Var(_) => {
                                self.errors.push(error_at(
                                    "cannot iterate a value whose type could not be inferred",
                                    *span,
                                ));
                                self.push_scope();
                                self.define(&vars[0].name, Type::Unit);
                                self.define(&vars[1].name, Type::Unit);
                                self.loop_depth += 1;
                                self.check_block(body);
                                self.loop_depth -= 1;
                                self.pop_scope();
                            }
                            other => {
                                self.errors.push(error_at(
                                    format!(
                                        "expected a dictionary for `for k, v in ...`, got `{other}`"
                                    ),
                                    *span,
                                ));
                                self.push_scope();
                                self.define(&vars[0].name, Type::Unit);
                                self.define(&vars[1].name, Type::Unit);
                                self.loop_depth += 1;
                                self.check_block(body);
                                self.loop_depth -= 1;
                                self.pop_scope();
                            }
                        }
                    }
                    _ => {
                        self.errors.push(error_at(
                            "for loop supports at most 2 variables (e.g. `for k, v in dict` or `for i, x in xs.enumerate()`)",
                            *span,
                        ));
                    }
                }
                Type::Unit
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`break` outside of a loop", *span));
                }
                Type::Unit
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`continue` outside of a loop", *span));
                }
                Type::Unit
            }
            Stmt::Defer { expr, span } => {
                if self.current_ret.is_none() {
                    self.errors
                        .push(error_at("`defer` outside of a function", *span));
                }
                self.check_expr(expr);
                Type::Unit
            }
            Stmt::Destructure {
                pat,
                value,
                span: _,
            } => {
                fn has_or(pat: &Pattern) -> bool {
                    match pat {
                        Pattern::Or { .. } => true,
                        Pattern::Variant { arg: Some(a), .. } => has_or(a),
                        Pattern::Tuple { pats, .. } => pats.iter().any(has_or),
                        _ => false,
                    }
                }
                if has_or(pat) {
                    self.errors.push(error_at(
                        "or-patterns (`|`) are only allowed in match arms",
                        pat.span(),
                    ));
                }
                let vt = self.check_expr(value);
                let vt = self.unifier.resolve(&vt);
                self.bind_pattern(pat, &vt);
                Type::Unit
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                // Reject assignment to immutable (`const`) variables. The target is an
                // `Ident` in plain programs and a `Path` (e.g. `ns.x`) after
                // the loader namespaces top-level bindings.
                let tname: Option<String> = match target {
                    Expr::Ident { name, .. } => Some(name.clone()),
                    Expr::Path { parts, .. } => Some(parts.join(".")),
                    _ => None,
                };
                if let Some(tname) = tname {
                    if let Some(def_span) = self.lookup_const_span(&tname) {
                        let display = Self::display_name(&tname);
                        self.errors.push(
                            error_at(
                                format!("cannot assign to immutable variable `{}`", display),
                                target.span(),
                            )
                            .with_secondary(zz_frontend::diag::SecondaryLabel {
                                span: def_span,
                                message: "variable defined as immutable here".to_string(),
                            })
                            .with_note(format!(
                                "hint: remove `const` to make `{}` mutable",
                                display
                            )),
                        );
                    }
                }
                let errors_before = self.errors.len();
                let tt = self.check_assign_target(target);
                let vt = self.check_expr(value);
                if self.errors.len() == errors_before {
                    if let Err(e) = self.unifier.unify(&vt, &tt) {
                        self.report_mismatch(e, *span);
                    }
                }
                Type::Unit
            }
        }
    }

    /// Type of an assignment target: a variable, a qualified name, or a
    /// struct field path.
    pub(crate) fn check_assign_target(&mut self, target: &Expr) -> Type {
        match target {
            Expr::Ident { name, span } => self.lookup(name, *span),
            Expr::Path { parts, span } => self.lookup_path(parts, *span),
            Expr::Field { obj, name, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                match ot {
                    Type::Struct(sname) => match self.structs.get(&sname).cloned() {
                        Some(sig) => match sig.fields.iter().find(|(n, _)| n == name) {
                            Some((_, ft)) => ft.clone(),
                            None => match self.resolve_struct_field(&sname, name) {
                                // Promoted through an embedded struct.
                                Some(ft) => ft,
                                None => {
                                    let visible = self.all_visible_fields(&sname);
                                    let field_names: Vec<&str> =
                                        visible.iter().map(|n| n.as_str()).collect();
                                    let mut diag = error_at(
                                        format!("struct `{sname}` has no field `{name}`"),
                                        *span,
                                    );
                                    let all = suggest_all(name, &field_names);
                                    if let Some((suggestion, _)) = all.first() {
                                        diag = diag.with_note(format!(
                                            "did you mean field `{suggestion}`?"
                                        ));
                                        let field_span =
                                            Span::new(span.end - name.len() as u32, span.end);
                                        let alts: Vec<String> =
                                            all.iter().map(|(s, _)| s.to_string()).collect();
                                        let fixit = if all.len() == 1 {
                                            FixIt::safe(
                                                field_span,
                                                suggestion.to_string(),
                                                "replace field",
                                            )
                                        } else {
                                            FixIt::ambiguous(
                                                field_span,
                                                suggestion.to_string(),
                                                "replace field",
                                                alts,
                                            )
                                        };
                                        diag = diag.with_fixit(fixit);
                                    }
                                    self.errors.push(diag);
                                    Type::Unit
                                }
                            },
                        },
                        None => {
                            self.errors
                                .push(error_at(format!("unknown struct `{sname}`"), *span));
                            Type::Unit
                        }
                    },
                    Type::Dict(k, v) => {
                        // Dict field access: req.body returns the value type
                        if let Err(e) = self.unifier.unify(&Type::Str, &k) {
                            self.report_mismatch(e, *span);
                        }
                        *v
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot assign to field `{name}` of a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Index { obj, index, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                let it = self.check_expr(index);
                match ot {
                    Type::Array(elem) => {
                        self.ensure_int(it, index.span());
                        *elem
                    }
                    Type::Dict(k, v) => {
                        if let Err(e) = self.unifier.unify(&it, &k) {
                            self.report_mismatch(e, index.span());
                        }
                        *v
                    }
                    Type::Str => {
                        self.errors
                            .push(error_at("cannot assign to an index of a string", *span));
                        Type::Unit
                    }
                    Type::Bytes => {
                        self.errors
                            .push(error_at("cannot assign to an index of bytes", *span));
                        Type::Unit
                    }
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot assign to an index of a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot assign to an index of a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            other => {
                self.errors.push(error_at(
                    "cannot assign to this expression".to_string(),
                    other.span(),
                ));
                Type::Unit
            }
        }
    }

    pub(crate) fn check_block(&mut self, block: &Block) -> Type {
        self.push_scope();
        let mut result = Type::Unit;
        for stmt in &block.stmts {
            result = self.check_stmt(stmt);
        }
        self.pop_scope();
        result
    }

    // --- expressions ------------------------------------------------------

    /// Check an expression, recording its resolved type keyed by span so the
    /// HIR could bindings can be built deterministically afterward.
    pub(crate) fn check_expr(&mut self, e: &Expr) -> Type {
        let ty = self.check_expr_impl(e);
        // Unresolved types stay as `Var`s during the walk; the typed map is
        // deep-resolved at the end (see `check_program_typed`).
        self.span_types.insert(e.span(), ty.clone());
        ty
    }

    fn check_expr_impl(&mut self, e: &Expr) -> Type {
        match e {
            Expr::Int { .. } => Type::Int,
            Expr::Float { .. } => Type::Float,
            Expr::Str { .. } => Type::Str,
            Expr::Bool { .. } => Type::Bool,
            Expr::Ident { name, span } => self.lookup(name, *span),
            Expr::Path { parts, span } => self.lookup_path(parts, *span),
            Expr::Field { obj, name, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                match ot {
                    Type::Struct(sname) => match self.structs.get(&sname).cloned() {
                        Some(sig) => match sig.fields.iter().find(|(n, _)| n == name) {
                            Some((_, ft)) => ft.clone(),
                            None => match self.resolve_struct_field(&sname, name) {
                                // Promoted through an embedded struct.
                                Some(ft) => ft,
                                None => {
                                    let visible = self.all_visible_fields(&sname);
                                    let field_names: Vec<&str> =
                                        visible.iter().map(|n| n.as_str()).collect();
                                    let mut diag = error_at(
                                        format!("struct `{sname}` has no field `{name}`"),
                                        *span,
                                    );
                                    let all = suggest_all(name, &field_names);
                                    if let Some((suggestion, _)) = all.first() {
                                        diag = diag.with_note(format!(
                                            "did you mean field `{suggestion}`?"
                                        ));
                                        let field_span =
                                            Span::new(span.end - name.len() as u32, span.end);
                                        let alts: Vec<String> =
                                            all.iter().map(|(s, _)| s.to_string()).collect();
                                        let fixit = if all.len() == 1 {
                                            FixIt::safe(
                                                field_span,
                                                suggestion.to_string(),
                                                "replace field",
                                            )
                                        } else {
                                            FixIt::ambiguous(
                                                field_span,
                                                suggestion.to_string(),
                                                "replace field",
                                                alts,
                                            )
                                        };
                                        diag = diag.with_fixit(fixit);
                                    }
                                    self.errors.push(diag);
                                    Type::Unit
                                }
                            },
                        },
                        None => {
                            self.errors
                                .push(error_at(format!("unknown struct `{sname}`"), *span));
                            Type::Unit
                        }
                    },
                    Type::Dict(k, v) => {
                        // Dict field access: req.body returns the value type
                        if let Err(e) = self.unifier.unify(&Type::Str, &k) {
                            self.report_mismatch(e, *span);
                        }
                        *v
                    }
                    Type::HttpRequest => {
                        // Typed request field access: `req.method/path/body`
                        // are `str`; `req.headers/query/params` are `{str: str}`.
                        match name.as_str() {
                            "method" | "path" | "body" => Type::Str,
                            "headers" | "query" | "params" => {
                                Type::Dict(Box::new(Type::Str), Box::new(Type::Str))
                            }
                            _ => {
                                self.errors.push(error_at(
                                    format!("http.request has no field `{name}`"),
                                    *span,
                                ));
                                Type::Unit
                            }
                        }
                    }
                    Type::Var(_id) => {
                        // Inference variable — not yet resolved (e.g. untyped closure param).
                        // Return a fresh var; unification will catch real mismatches later.
                        self.unifier.fresh_var()
                    }
                    other => {
                        let method = name.clone();
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
                            if let Some(sig) = self.funcs.get(&format!("{ns}.{method}")).cloned() {
                                let (ps, ret, subs) = self.instantiate(&sig);
                                if !ps.is_empty() {
                                    if let Err(e) = self.unifier.unify(&other, &ps[0]) {
                                        self.report_mismatch(e, *span);
                                    }
                                }
                                self.validate_bounds(&sig, &subs, *span);
                                return ret;
                            }
                        }
                        self.errors.push(error_at(
                            format!("cannot access field `{name}` on a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Range { start, end, .. } => {
                let st = self.check_expr(start);
                let et = self.check_expr(end);
                for (t, s) in [(st, start.span()), (et, end.span())] {
                    match self.unifier.resolve(&t) {
                        Type::Int => {}
                        Type::Var(id) => {
                            self.unifier.bind(id, Type::Int);
                        }
                        other => {
                            self.errors.push(error_at(
                                format!("range bounds must be `int`, found `{other}`"),
                                s,
                            ));
                        }
                    }
                }
                Type::Range(Box::new(Type::Int))
            }
            Expr::StructInit { name, fields, span } => {
                self.used_names.insert(name.clone());
                let Some(sig) = self.structs.get(name).cloned() else {
                    self.errors
                        .push(error_at(format!("unknown struct `{name}`"), *span));
                    return Type::Unit;
                };
                // Each given field maps to a concrete path: direct fields to
                // `[name]`, promoted (flattened) fields to their embedded
                // prefix + `[name]` (e.g. `id` in `User{id: 1, ...}` maps to
                // `[Base, id]`).
                let mut given_paths: Vec<Vec<String>> = Vec::new();
                for (fname, fval) in fields {
                    if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == fname) {
                        let vt = self.check_expr(fval);
                        if let Err(e) = self.unifier.unify(&vt, ft) {
                            self.report_mismatch(e, fval.span());
                        }
                        given_paths.push(vec![fname.clone()]);
                    } else if let Some((prefix, pft)) = self.resolve_struct_field_path(name, fname)
                    {
                        if prefix.is_empty() {
                            // Unreachable: direct fields are handled above.
                            continue;
                        }
                        let vt = self.check_expr(fval);
                        if let Err(e) = self.unifier.unify(&vt, &pft) {
                            self.report_mismatch(e, fval.span());
                        }
                        let mut full = prefix;
                        full.push(fname.clone());
                        given_paths.push(full);
                    } else {
                        self.errors.push(error_at(
                            format!("struct `{name}` has no field `{fname}`"),
                            fval.span(),
                        ));
                    }
                }
                // An explicit embedded value and flattened leaves inside the
                // same subtree are ambiguous: reject rather than guess.
                for (i, a) in given_paths.iter().enumerate() {
                    for b in given_paths.iter().skip(i + 1) {
                        let conflict = (a.len() < b.len() && b.starts_with(a))
                            || (b.len() < a.len() && a.starts_with(b));
                        if conflict {
                            let (outer, inner) = if a.len() < b.len() { (a, b) } else { (b, a) };
                            self.errors.push(error_at(
                                format!(
                                    "field `{}` conflicts with embedded value `{}` in struct literal `{name}` (provide one or the other)",
                                    inner.last().unwrap_or(&String::new()),
                                    outer.last().unwrap_or(&String::new()),
                                ),
                                *span,
                            ));
                        }
                    }
                }
                // Verify all required fields are provided (flattened leaves
                // count toward their embedded subtree).
                if let Some(leaf) = self.first_uncovered_leaf(name, &[], &given_paths, 0) {
                    self.errors.push(error_at(
                        format!(
                            "missing field `{}` in struct literal `{name}`",
                            leaf.join("."),
                        ),
                        *span,
                    ));
                }
                Type::Struct(name.clone())
            }
            Expr::Index { obj, index, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                let it = self.check_expr(index);
                match ot {
                    Type::Array(elem) => {
                        self.ensure_int(it, index.span());
                        *elem
                    }
                    Type::Bytes => {
                        self.ensure_int(it, index.span());
                        Type::Int
                    }
                    Type::Dict(k, v) => {
                        if let Err(e) = self.unifier.unify(&it, &k) {
                            self.report_mismatch(e, index.span());
                        }
                        *v
                    }
                    Type::Str => {
                        self.ensure_int(it, index.span());
                        Type::Str
                    }
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot index a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot index a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Slice {
                obj,
                start,
                end,
                span,
            } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                for bound in [start.as_deref(), end.as_deref()].into_iter().flatten() {
                    let bt = self.check_expr(bound);
                    self.ensure_int(bt, bound.span());
                }
                match ot {
                    Type::Array(elem) => Type::Array(elem),
                    Type::Bytes => Type::Bytes,
                    Type::Str => Type::Str,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot slice a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot slice a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let FmtPart::Expr(e, _) = part {
                        let _ = self.check_expr(e);
                    }
                }
                Type::Str
            }
            Expr::Paren { expr, .. } => self.check_expr(expr),
            Expr::Unary { op, expr, span } => self.check_unary(*op, expr, *span),
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => self.check_binary(*op, left, right, *span),
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => self.check_call(callee, args, named, *span),
            Expr::Closure {
                params,
                ret_ty,
                body,
                span,
            } => self.check_closure(params, ret_ty.as_ref(), body, *span, None),
            Expr::If {
                cond,
                then,
                els,
                span,
            } => {
                let ct = self.check_expr(cond);
                self.ensure_bool(ct, cond.span());
                let tt = self.check_block(then);
                match els {
                    Some(e) => {
                        let et = self.check_expr(e);
                        if let Err(err) = self.unifier.unify(&et, &tt) {
                            self.report_mismatch(err, e.span());
                        }
                    }
                    None => {
                        // If the then-block contains a `return`, its type
                        // may not be Unit (e.g. `if x { return 5 }`), but
                        // that's fine — the return short-circuits.
                        if !Self::block_has_return(then) {
                            if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                                self.report_mismatch(err, *span);
                            }
                        }
                    }
                }
                tt
            }
            Expr::While { cond, body, .. } => {
                let ct = self.check_expr(cond);
                self.ensure_bool(ct, cond.span());
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
                Type::Unit
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.check_match(scrutinee, arms, *span),
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                span,
            } => {
                let vt = self.check_expr(value);
                self.push_scope();
                self.bind_pattern(pat, &vt);
                let tt = self.check_block(then);
                self.pop_scope();
                match els {
                    Some(e) => {
                        let et = self.check_expr(e);
                        if let Err(err) = self.unifier.unify(&et, &tt) {
                            self.report_mismatch(err, e.span());
                        }
                    }
                    None => {
                        if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                            self.report_mismatch(err, *span);
                        }
                    }
                }
                tt
            }
            Expr::Try { expr, span } => self.check_try(expr, *span),
            Expr::Block(b) => self.check_block(b),
            Expr::Array { elems, span: _ } => {
                let types: Vec<Type> = elems.iter().map(|e| self.check_expr(e)).collect();
                let elem_t = self.merge_types(types);
                Type::Array(Box::new(elem_t))
            }
            Expr::Tuple { items, .. } => {
                let types: Vec<Type> = items.iter().map(|e| self.check_expr(e)).collect();
                Type::Tuple(types)
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                span,
            } => {
                let it = self.check_expr(iter);
                let it = self.unifier.resolve(&it);
                let elem = match it {
                    Type::Array(elem) => *elem,
                    Type::Range(elem) => *elem,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot iterate a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot iterate a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                };
                self.push_scope();
                self.define(&var.name, elem);
                if let Some(f) = filter {
                    let ft = self.check_expr(f);
                    let ft = self.unifier.resolve(&ft);
                    if let Err(e) = self.unifier.unify(&ft, &Type::Bool) {
                        self.report_mismatch(e, f.span());
                    }
                }
                let body_t = self.check_expr(body);
                self.pop_scope();
                Type::Array(Box::new(body_t))
            }
            Expr::Dict { entries, span: _ } => {
                let mut key_types = Vec::new();
                let mut val_types = Vec::new();
                for (k, v) in entries {
                    key_types.push(self.check_expr(k));
                    val_types.push(self.check_expr(v));
                }
                let key_t = self.merge_types(key_types);
                let val_t = self.merge_types(val_types);
                Type::Dict(Box::new(key_t), Box::new(val_t))
            }
            Expr::Break { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`break` outside of a loop", *span));
                }
                Type::Unit
            }
            Expr::Continue { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`continue` outside of a loop", *span));
                }
                Type::Unit
            }
            Expr::Variant { name, arg, span } => {
                let arg_t = arg.as_ref().map(|a| self.check_expr(a));
                match (name.as_str(), arg_t) {
                    ("ok", Some(t)) => {
                        Type::Result(Box::new(t), Box::new(self.unifier.fresh_var()))
                    }
                    ("ok", None) => {
                        self.errors
                            .push(error_at("`.ok` requires an argument", *span));
                        Type::Result(Box::new(Type::Unit), Box::new(self.unifier.fresh_var()))
                    }
                    ("err", Some(e)) => {
                        Type::Result(Box::new(self.unifier.fresh_var()), Box::new(e))
                    }
                    ("err", None) => {
                        self.errors
                            .push(error_at("`.err` requires an argument", *span));
                        Type::Result(Box::new(self.unifier.fresh_var()), Box::new(Type::Unit))
                    }
                    ("some", Some(t)) => Type::Option(Box::new(t)),
                    ("some", None) => {
                        self.errors
                            .push(error_at("`.some` requires an argument", *span));
                        Type::Option(Box::new(Type::Unit))
                    }
                    ("none", None) => Type::Option(Box::new(self.unifier.fresh_var())),
                    ("none", Some(_)) => {
                        self.errors
                            .push(error_at("`.none` takes no argument", *span));
                        Type::Option(Box::new(self.unifier.fresh_var()))
                    }
                    (other, _) => {
                        self.errors
                            .push(error_at(format!("unknown variant `.{other}`"), *span));
                        Type::Unit
                    }
                }
            }
        }
    }

    pub(crate) fn check_unary(&mut self, op: UnOp, expr: &Expr, span: Span) -> Type {
        let t = self.check_expr(expr);
        let t = self.unifier.resolve(&t);
        match op {
            UnOp::Not => match t {
                Type::Bool => Type::Bool,
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Bool);
                    Type::Bool
                }
                other => {
                    self.errors
                        .push(error_at(format!("expected `bool`, found `{other}`"), span));
                    Type::Bool
                }
            },
            UnOp::Pos | UnOp::Neg => match t {
                Type::Int => Type::Int,
                Type::Float => Type::Float,
                Type::Named(n) => {
                    if self.has_bound(&n, zz_frontend::ast::TraitBound::Num) {
                        Type::Named(n)
                    } else {
                        self.errors.push(error_at(
                            format!(
                                "generic parameter `{n}` needs a `Num` bound for `{}`; write `func ...<{n}: Num>`",
                                op.symbol()
                            ),
                            span,
                        ));
                        Type::Int
                    }
                }
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Int);
                    Type::Int
                }
                other => {
                    self.errors.push(error_at(
                        format!("cannot negate a value of type `{other}`"),
                        span,
                    ));
                    Type::Int
                }
            },
        }
    }

    pub(crate) fn check_binary(
        &mut self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Type {
        match op {
            BinOp::And | BinOp::Or => {
                let lt = self.check_expr(left);
                self.ensure_bool(lt, left.span());
                let rt = self.check_expr(right);
                self.ensure_bool(rt, right.span());
                Type::Bool
            }
            BinOp::Elvis => {
                let lt = self.check_expr(left);
                let lt_resolved = self.unifier.resolve(&lt);
                let rt = self.check_expr(right);
                let rt_resolved = self.unifier.resolve(&rt);
                match lt_resolved {
                    Type::Option(inner) => {
                        if let Err(e) = self.unifier.unify(&inner, &rt_resolved) {
                            self.report_mismatch(e, span);
                        }
                        *inner
                    }
                    Type::Result(inner, _err) => {
                        if let Err(e) = self.unifier.unify(&inner, &rt_resolved) {
                            self.report_mismatch(e, span);
                        }
                        *inner
                    }
                    _ => {
                        if let Err(e) = self.unifier.unify(&lt, &rt) {
                            self.report_mismatch(e, span);
                        }
                        lt
                    }
                }
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                use zz_frontend::ast::TraitBound;
                let lt = self.check_expr(left);
                let lt = self.unifier.resolve(&lt);
                let rt = self.check_expr(right);
                let rt = self.unifier.resolve(&rt);
                // Which bound does this comparison require?
                let needed = match op {
                    BinOp::Eq | BinOp::Ne => TraitBound::Eq,
                    _ => TraitBound::Ord,
                };
                // Reject int/float mixed comparisons with a helpful message.
                match (&lt, &rt) {
                    (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
                        self.errors.push(error_at(
                            format!(
                                "cannot compare `{}` with `{}`. Use `float(x)` to cast",
                                lt, rt
                            ),
                            span,
                        ));
                        Type::Bool
                    }
                    // Generic operands: allowed only when the parameter
                    // carries the required bound (`Eq` for ==/!=, `Ord` for
                    // ordering). The concrete type is pinned at the call site.
                    (Type::Named(a), Type::Named(b)) if a == b => {
                        if self.has_bound(a, needed) {
                            Type::Bool
                        } else {
                            self.errors.push(error_at(
                                format!(
                                    "generic parameter `{a}` needs a `{}` bound for `{}`; write `func ...<{a}: {}>`",
                                    needed.name(),
                                    op.symbol(),
                                    needed.name()
                                ),
                                span,
                            ));
                            Type::Bool
                        }
                    }
                    (Type::Named(n), _) | (_, Type::Named(n)) => {
                        if self.has_bound(n, needed) {
                            Type::Bool
                        } else {
                            self.errors.push(error_at(
                                format!(
                                    "generic parameter `{n}` needs a `{}` bound for `{}`; write `func ...<{n}: {}>`",
                                    needed.name(),
                                    op.symbol(),
                                    needed.name()
                                ),
                                span,
                            ));
                            Type::Bool
                        }
                    }
                    (Type::Var(_), t) => {
                        self.unifier.bind_var(&lt, t.clone());
                        Type::Bool
                    }
                    (t, Type::Var(_)) => {
                        self.unifier.bind_var(&rt, t.clone());
                        Type::Bool
                    }
                    _ => {
                        if let Err(e) = self.unifier.unify(&rt, &lt) {
                            self.report_mismatch(e, span);
                        }
                        Type::Bool
                    }
                }
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.check_arith(op, left, right, span)
            }
        }
    }

    pub(crate) fn check_arith(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        use zz_frontend::ast::TraitBound;
        let lt = self.check_expr(left);
        let lt = self.unifier.resolve(&lt);
        let rt = self.check_expr(right);
        let rt = self.unifier.resolve(&rt);
        match (&lt, &rt) {
            (Type::Int, Type::Int) => Type::Int,
            (Type::Str, Type::Str) if op == BinOp::Add => Type::Str,
            (Type::Int, Type::Float) | (Type::Float, Type::Int) | (Type::Float, Type::Float) => {
                Type::Float
            }
            // Generic operands: allowed only when the parameter carries a
            // `Num` bound. The result keeps the generic type; the concrete
            // type is pinned at the call site via instantiation.
            (Type::Named(a), Type::Named(b)) if a == b => {
                if self.has_bound(a, TraitBound::Num) {
                    Type::Named(a.clone())
                } else {
                    self.errors.push(error_at(
                        format!(
                            "generic parameter `{a}` needs a `Num` bound for `{}`; write `func ...<{a}: Num>`",
                            op.symbol()
                        ),
                        span,
                    ));
                    Type::Error
                }
            }
            (Type::Named(a), Type::Named(b)) => {
                self.errors.push(error_at(
                    format!(
                        "cannot apply `{}` to `{a}` and `{b}`: distinct generic parameters\n\
                         hint: use the same type parameter (`x: T, y: T`) or a concrete type",
                        op.symbol()
                    ),
                    span,
                ));
                Type::Error
            }
            (Type::Named(n), t) | (t, Type::Named(n))
                if self.has_bound(n, TraitBound::Num) && matches!(t, Type::Int | Type::Float) =>
            {
                Type::Named(n.clone())
            }
            (Type::Named(n), Type::Var(_)) | (Type::Var(_), Type::Named(n)) => {
                if self.has_bound(n, TraitBound::Num) {
                    Type::Named(n.clone())
                } else {
                    self.errors.push(error_at(
                        format!(
                            "generic parameter `{n}` needs a `Num` bound for `{}`; write `func ...<{n}: Num>`",
                            op.symbol()
                        ),
                        span,
                    ));
                    Type::Error
                }
            }
            (Type::Var(_), t) => {
                self.unifier.bind_var(&lt, t.clone());
                t.clone()
            }
            (t, Type::Var(_)) => {
                self.unifier.bind_var(&rt, t.clone());
                t.clone()
            }
            (a, b) => {
                if !matches!((&a, &b), (Type::Error, _) | (_, Type::Error)) {
                    self.errors.push(error_at(
                        format!("cannot apply `{}` to `{}` and `{}`", op.symbol(), a, b),
                        span,
                    ));
                }
                Type::Error
            }
        }
    }

    pub(crate) fn check_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        named: &[(String, Expr)],
        span: Span,
    ) -> Type {
        // Selective-import alias, resolved FIRST so the result flows
        // through the direct generic-instantiation path below: a bare
        // `squared` from `import m(squared)` becomes `m.squared` — but
        // ONLY on total miss. Locals, seed entries and synthetic Decls
        // all take precedence; this path exists for generic functions,
        // which have no value binding to find. Bare *uses* still error.
        let rewritten;
        let callee: &Expr = match callee {
            Expr::Ident { name, span }
                if !self.funcs.contains_key(name)
                    && !self.env.iter().any(|s| s.contains_key(name)) =>
            {
                match self.import_aliases.get(name).cloned() {
                    Some(qualified) => {
                        self.used_names.insert(name.clone());
                        rewritten = Expr::Path {
                            parts: qualified.split('.').map(str::to_string).collect(),
                            span: *span,
                        };
                        &rewritten
                    }
                    None => callee,
                }
            }
            c => c,
        };
        // Direct call of a named function: bypass `lookup` so generic
        // functions are instantiated here rather than rejected as values.
        let direct_name = match callee {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            Expr::Field { obj, name, .. } => {
                let recv_t = self.check_expr(obj);
                let recv_t = self.unifier.resolve(&recv_t);
                let method = name.clone();
                let mut sig = self.funcs.get(&method).cloned();
                if sig.is_none() {
                    match &self.unifier.resolve(&recv_t) {
                        Type::Str => sig = self.funcs.get(&format!("str.{method}")).cloned(),
                        Type::Array(_) => sig = self.funcs.get(&format!("vec.{method}")).cloned(),
                        Type::Bytes => sig = self.funcs.get(&format!("bytes.{method}")).cloned(),
                        Type::Option(_) => {
                            sig = self.funcs.get(&format!("option.{method}")).cloned()
                        }
                        Type::Result(_, _) => {
                            sig = self.funcs.get(&format!("result.{method}")).cloned()
                        }
                        // Scalar extensions (`impl int/float/bool`) live in
                        // the same merged table; no stdlib namespace here.
                        Type::Int => sig = self.funcs.get(&format!("int.{method}")).cloned(),
                        Type::Float => sig = self.funcs.get(&format!("float.{method}")).cloned(),
                        Type::Bool => sig = self.funcs.get(&format!("bool.{method}")).cloned(),
                        Type::Response => sig = self.funcs.get(&format!("http.{method}")).cloned(),
                        Type::HttpRequest => {
                            sig = self.funcs.get(&format!("http.{method}")).cloned()
                        }
                        Type::TcpStream => sig = self.funcs.get(&format!("net.{method}")).cloned(),
                        Type::TcpListener => {
                            sig = self.funcs.get(&format!("net.{method}")).cloned()
                        }
                        Type::HttpServer => {
                            sig = self.funcs.get(&format!("http.{method}")).cloned()
                        }
                        Type::Json => sig = self.funcs.get(&format!("json.{method}")).cloned(),
                        Type::Db => {
                            // Canonical `sqlz.*` first, `db.*` alias fallback.
                            sig = self.funcs.get(&format!("sqlz.{method}")).cloned();
                            if sig.is_none() {
                                sig = self.funcs.get(&format!("db.{method}")).cloned();
                            }
                        }
                        Type::Chan => sig = self.funcs.get(&format!("chan.{method}")).cloned(),
                        Type::TaskJoin => sig = self.funcs.get(&format!("task.{method}")).cloned(),
                        // Opaque handles dispatch on their module tag, e.g.
                        // an `Opaque("regex")` receiver resolves
                        // `regex.is_match`.
                        Type::Opaque(tag) => {
                            sig = self.funcs.get(&format!("{tag}.{method}")).cloned()
                        }
                        Type::Struct(sname) => {
                            // Try TypeName.method (impl block methods)
                            sig = self.funcs.get(&format!("{sname}.{method}")).cloned();
                            if sig.is_none() {
                                // Try namespace.method (cross-module)
                                if let Some((ns, _)) = sname.rsplit_once('.') {
                                    sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Embedded promotion: `u.area()` dispatches to `Base.area`
                // when `User` embeds `Base`. The runtime passes the embedded
                // value as the receiver, so the outer type is not unified
                // against the method's receiver below.
                let mut promoted_recv: Option<Type> = None;
                if sig.is_none() {
                    if let Type::Struct(sname) = self.unifier.resolve(&recv_t) {
                        if let Some((defining, psig)) = self.find_struct_method(&sname, &method) {
                            promoted_recv = Some(Type::Struct(defining));
                            sig = Some(psig);
                        }
                    }
                }
                if let Some(sig) = sig {
                    let (ps, ret, subs) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Some(promoted) = promoted_recv {
                        if let Err(e) = self.unifier.unify(&promoted, &ps[0]) {
                            self.report_mismatch(e, span);
                        }
                    } else if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                        self.report_mismatch(e, span);
                    }
                    self.check_args_against(
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
                    self.validate_bounds(&sig, &subs, span);
                    return ret;
                }
                None
            }
            _ => None,
        };
        if let Some(name) = &direct_name {
            // Math constants are values, not functions: `math.PI()` is
            // always an error — use bare `math.PI`.
            if Self::is_math_const(name) && self.funcs.contains_key(name) {
                self.used_names.insert(name.clone());
                // Still check the args so nested errors inside them surface.
                for arg in args {
                    self.check_expr(arg);
                }
                for (_, val) in named {
                    self.check_expr(val);
                }
                self.errors.push(error_at(
                    format!(
                        "cannot call `{name}`: it is a numeric constant value, remove the `()`"
                    ),
                    span,
                ));
                return Type::Float;
            }
            // sqlz foundation: `sqlz.query(sql)` / `sqlz.exec(sql)` (canonical;
            // `db.*` / `std.db.*` are zero-overhead aliases) take an
            // interpolated SQL string. `{expr}` segments are extracted as
            // bound parameters (never inlined), so verification here is:
            // receiver is `db`, SQL arg is `str`, each bound expr is a
            // scalar (int/float/str/bool). Return type unifies with the
            // caller's annotation (`let users: [User] = sqlz.query(...)`).
            //
            // `pg.query(db, sql)` / `pg.exec(db, sql)` (+ `postgres.*` and
            // `std.sqlz.postgres.*` spellings) and `my.query(db, sql)` /
            // `my.exec(db, sql)` (+ `mysql.*`, `std.sqlz.mysql.*`) are the
            // explicit-receiver free-function forms: the SQL is the SECOND
            // user arg.
            //
            // NOTE: this direct-name path only fires for qualified calls
            // where the leading component is the module namespace (not a
            // local). The common method form (`mydb.query(...)` on a
            // handle) is handled in the Path-method branch below, which
            // treats the receiver as implicit.
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
            if name == "sqlz.query"
                || name == "std.sqlz.query"
                || name == "sqlz.exec"
                || name == "std.sqlz.exec"
                || name == "db.query"
                || name == "std.db.query"
                || name == "db.exec"
                || name == "std.db.exec"
                || name == "sqlz.transaction"
                || name == "std.sqlz.transaction"
                || name == "db.transaction"
                || name == "std.db.transaction"
                || is_pg_call
            {
                if let Some(sig) = self.funcs.get(name).cloned() {
                    // Only take this path when the leading component really
                    // is the module namespace (NOT a local variable).
                    // Otherwise fall through to method dispatch.
                    let recv_is_module = match callee {
                        Expr::Path { parts, .. } => {
                            parts.len() >= 2 && self.lookup_opt(&parts[0]).is_none()
                        }
                        _ => false,
                    };
                    if recv_is_module {
                        self.used_names.insert(name.clone());
                        let (ps, ret, subs) = self.instantiate(&sig);
                        let pnames: Vec<String> =
                            sig.params.iter().map(|(n, _)| n.clone()).collect();
                        self.check_args_against(&pnames, &ps, &sig.has_default, args, named, span);
                        // Explicit-receiver forms (`pg.query(db, sql)`,
                        // `sqlz.query(db, sql)`) carry the SQL second;
                        // the bare method-namespace form carries it first.
                        // Skip for transaction — second arg is a closure, not SQL.
                        let is_tx_call = name == "sqlz.transaction"
                            || name == "std.sqlz.transaction"
                            || name == "db.transaction"
                            || name == "std.db.transaction";
                        if !is_tx_call {
                            let sql_idx = if is_pg_call || args.len() >= 2 { 1 } else { 0 };
                            if let Some(sql_arg) = args.get(sql_idx) {
                                self.verify_sql_params(sql_arg, span);
                            }
                        }
                        self.validate_bounds(&sig, &subs, span);
                        return ret;
                    }
                }
            }
            if let Some(sig) = self.funcs.get(name).cloned() {
                self.used_names.insert(name.clone());
                let (ps, ret, subs) = self.instantiate(&sig);
                if name == "input" {
                    if args.len() + named.len() > 1 {
                        self.errors.push(error_at(
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
                        let at = self.check_expr(arg_expr);
                        if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                            self.report_mismatch(e, arg_expr.span());
                        }
                    }
                    return ret;
                }
                if name == "range" {
                    let total = args.len() + named.len();
                    if total == 0 || total > 3 {
                        self.errors.push(error_at(
                            format!("range expects 1, 2, or 3 arguments, found {total}"),
                            span,
                        ));
                    } else {
                        for arg in args {
                            let at = self.check_expr(arg);
                            if let Err(e) = self.unifier.unify(&at, &Type::Int) {
                                self.report_mismatch(e, arg.span());
                            }
                        }
                        for (_, val) in named {
                            let at = self.check_expr(val);
                            if let Err(e) = self.unifier.unify(&at, &Type::Int) {
                                self.report_mismatch(e, val.span());
                            }
                        }
                    }
                    return ret;
                }
                let pnames: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
                self.check_args_against(&pnames, &ps, &sig.has_default, args, named, span);
                self.validate_bounds(&sig, &subs, span);
                // A bare function value as a print argument is always a
                // missing `()` (`println(env.os)` would print the function
                // itself instead of calling it). Catch it here — with the
                // name attached — rather than letting each engine render
                // `<func>` / `<native ...>` / empty output.
                if name == "print" || name == "println" {
                    if let Some(first) = args.first() {
                        // Resolve silently (lookup_opt, never check_expr:
                        // the argument was already checked above and a
                        // second pass would duplicate diagnostics).
                        let (arg_t, fname) = match first {
                            Expr::Ident { name: n, .. } => (self.lookup_opt(n), Some(n.clone())),
                            Expr::Path { parts, .. } => {
                                let joined = parts.join(".");
                                (self.lookup_opt(&joined), Some(joined))
                            }
                            _ => (None, None),
                        };
                        let is_func = matches!(
                            arg_t.as_ref().map(|t| self.unifier.resolve(t)),
                            Some(Type::Func(_, _))
                        ) || matches!(fname.as_deref(), Some(n)
                            if self.funcs.contains_key(n) && !Self::is_math_const(n));
                        if is_func {
                            if let Some(fname) = fname {
                                let mut diag = error_at(
                                    format!(
                                        "cannot print function `{fname}`: call it with arguments"
                                    ),
                                    first.span(),
                                );
                                diag = diag.with_note(format!("did you mean `{fname}()`?"));
                                diag = diag.with_fixit(FixIt::safe(
                                    Span::new(first.span().end, first.span().end),
                                    "()".to_string(),
                                    "call function",
                                ));
                                self.errors.push(diag);
                            }
                        }
                    }
                }
                return ret;
            }
        }
        // Method call: `p.dist()` resolves to `dist(p, ...)`.
        if let Expr::Path { parts, span: pspan } = callee {
            if parts.len() >= 2 {
                // First check if the full path resolves as a variable
                // (e.g. module-level closure `ns.f`). If so, treat it as
                // a regular call, not a method call.
                let joined = parts.join(".");
                if let Some(var_ty) = self.lookup_opt(&joined) {
                    self.used_names.insert(joined.clone());
                    let callee_t = self.unifier.resolve(&var_ty);
                    // If the var is still an unresolved inference var, or is
                    // already known to be a Func/Named, treat as a variable
                    // call — not a method call on the first path component.
                    match &callee_t {
                        Type::Func(..) | Type::Named(..) => {
                            let pnames: Vec<String> =
                                (0..args.len()).map(|i| format!("_{i}")).collect();
                            match callee_t {
                                Type::Func(ps, ret) => {
                                    self.check_args_against(&pnames, &ps, &[], args, named, span);
                                    return *ret;
                                }
                                Type::Named(ref nname) => {
                                    if let Some(sig) = self.funcs.get(nname).cloned() {
                                        let (ps, ret, subs) = self.instantiate(&sig);
                                        let pnames: Vec<String> =
                                            sig.params.iter().map(|(n, _)| n.clone()).collect();
                                        self.check_args_against(
                                            &pnames,
                                            &ps,
                                            &sig.has_default,
                                            args,
                                            named,
                                            span,
                                        );
                                        self.validate_bounds(&sig, &subs, span);
                                        return ret;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Type::Var(_) => {
                            // Fresh var from recursive closure pre-binding.
                            // Build a Func type from the args and unify.
                            let arg_types: Vec<Type> =
                                args.iter().map(|a| self.check_expr(a)).collect();
                            let ret_var = self.unifier.fresh_var();
                            let func_ty = Type::Func(arg_types, Box::new(ret_var.clone()));
                            if let Err(e) = self.unifier.unify(&var_ty, &func_ty) {
                                self.report_mismatch(e, span);
                            }
                            return self.unifier.resolve(&ret_var);
                        }
                        _ => {}
                    }
                }
                let method = parts.last().unwrap();
                let recv_t = self.lookup_path(&parts[..parts.len() - 1], *pspan);
                let mut sig = self.funcs.get(method).cloned();
                if sig.is_none() {
                    let recv_t_resolved = self.unifier.resolve(&recv_t);
                    match &recv_t_resolved {
                        Type::Str => {
                            sig = self.funcs.get(&format!("str.{method}")).cloned();
                        }
                        Type::Array(_) => {
                            sig = self.funcs.get(&format!("vec.{method}")).cloned();
                        }
                        Type::Option(_) => {
                            sig = self.funcs.get(&format!("option.{method}")).cloned();
                        }
                        Type::Result(_, _) => {
                            sig = self.funcs.get(&format!("result.{method}")).cloned();
                        }
                        Type::Int => {
                            sig = self.funcs.get(&format!("int.{method}")).cloned();
                        }
                        Type::Float => {
                            sig = self.funcs.get(&format!("float.{method}")).cloned();
                        }
                        Type::Bool => {
                            sig = self.funcs.get(&format!("bool.{method}")).cloned();
                        }
                        Type::Response => {
                            sig = self.funcs.get(&format!("http.{method}")).cloned();
                        }
                        Type::HttpRequest => {
                            sig = self.funcs.get(&format!("http.{method}")).cloned();
                        }
                        Type::TcpStream => {
                            sig = self.funcs.get(&format!("net.{method}")).cloned();
                        }
                        Type::TcpListener => {
                            sig = self.funcs.get(&format!("net.{method}")).cloned();
                        }
                        Type::HttpServer => {
                            sig = self.funcs.get(&format!("http.{method}")).cloned();
                        }
                        Type::Json => {
                            sig = self.funcs.get(&format!("json.{method}")).cloned();
                        }
                        Type::Db => {
                            // Canonical `sqlz.*` first, `db.*` alias fallback.
                            sig = self.funcs.get(&format!("sqlz.{method}")).cloned();
                            if sig.is_none() {
                                sig = self.funcs.get(&format!("db.{method}")).cloned();
                            }
                        }
                        Type::Chan => {
                            sig = self.funcs.get(&format!("chan.{method}")).cloned();
                        }
                        Type::TaskJoin => {
                            sig = self.funcs.get(&format!("task.{method}")).cloned();
                        }
                        Type::Opaque(tag) => {
                            sig = self.funcs.get(&format!("{tag}.{method}")).cloned();
                        }
                        Type::Struct(sname) => {
                            // Try TypeName.method (impl block methods)
                            sig = self.funcs.get(&format!("{sname}.{method}")).cloned();
                            if sig.is_none() {
                                // Try namespace.method (cross-module)
                                if let Some((ns, _)) = sname.rsplit_once('.') {
                                    sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Embedded promotion (see the `Field`-callee branch above).
                let mut promoted_recv: Option<Type> = None;
                if sig.is_none() {
                    if let Type::Struct(sname) = self.unifier.resolve(&recv_t) {
                        if let Some((defining, psig)) = self.find_struct_method(&sname, method) {
                            promoted_recv = Some(Type::Struct(defining));
                            sig = Some(psig);
                        }
                    }
                }
                if let Some(sig) = sig {
                    // sqlz method form: `db.exec(sql)`, `db.query(sql)`,
                    // `db.transaction(fn(tx) { ... })` — receiver is
                    // implicit, so user args are matched against sig[1..].
                    if (*method == "query" || *method == "exec" || *method == "transaction")
                        && matches!(self.unifier.resolve(&recv_t), Type::Db)
                    {
                        let (ps, ret, subs) = self.instantiate(&sig);
                        if !ps.is_empty() {
                            if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                                self.report_mismatch(e, *pspan);
                            }
                        }
                        let expected = ps.len().saturating_sub(1);
                        let actual = args.len() + named.len();
                        if actual != expected {
                            self.errors.push(error_at(
                                format!("expected {expected} argument(s), found {actual}"),
                                span,
                            ));
                        } else if expected >= 1 {
                            // Verify each user arg against sig[1..].
                            for (i, arg) in args.iter().enumerate() {
                                if ps.len() >= 2 + i {
                                    // Propagate expected types into
                                    // closures so their bodies can
                                    // resolve method calls on known
                                    // param types (e.g. `tx.exec`
                                    // when `tx: Db`).
                                    if let Expr::Closure {
                                        params,
                                        ret_ty,
                                        body,
                                        span: cspan,
                                    } = arg
                                    {
                                        let expected_t = self.unifier.resolve(&ps[1 + i]);
                                        let ep = if let Type::Func(ep, _) = &expected_t {
                                            Some(ep.as_slice())
                                        } else {
                                            None
                                        };
                                        let at = self.check_closure(
                                            params,
                                            ret_ty.as_ref(),
                                            body,
                                            *cspan,
                                            ep,
                                        );
                                        if let Err(e) = self.unifier.unify(&at, &ps[1 + i]) {
                                            self.report_mismatch(e, arg.span());
                                        }
                                    } else {
                                        let at = self.check_expr(arg);
                                        if let Err(e) = self.unifier.unify(&at, &ps[1 + i]) {
                                            self.report_mismatch(e, arg.span());
                                        }
                                    }
                                }
                            }
                            if *method == "query" || *method == "exec" {
                                self.verify_sql_params(&args[0], span);
                            }
                        }
                        self.validate_bounds(&sig, &subs, span);
                        return ret;
                    }
                    let (ps, ret, subs) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Some(promoted) = promoted_recv {
                        if let Err(e) = self.unifier.unify(&promoted, &ps[0]) {
                            self.report_mismatch(e, *pspan);
                        }
                    } else if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                        self.report_mismatch(e, *pspan);
                    }
                    self.check_args_against(
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
                    self.validate_bounds(&sig, &subs, span);
                    return ret;
                }
            }
        }
        let callee_t = self.check_expr(callee);
        let callee_t = self.unifier.resolve(&callee_t);
        match callee_t {
            Type::Func(ps, ret) => {
                let pnames: Vec<String> = (0..ps.len()).map(|i| format!("_{i}")).collect();
                self.check_args_against(&pnames, &ps, &[], args, named, span);
                *ret
            }
            Type::Named(name) => match self.funcs.get(&name).cloned() {
                Some(sig) => {
                    let (ps, ret, subs) = self.instantiate(&sig);
                    let param_names: Vec<String> =
                        sig.params.iter().map(|(n, _)| n.clone()).collect();
                    self.check_args_against(&param_names, &ps, &sig.has_default, args, named, span);
                    self.validate_bounds(&sig, &subs, span);
                    ret
                }
                None => {
                    self.errors
                        .push(error_at(format!("unknown function `{name}`"), span));
                    Type::Unit
                }
            },
            Type::Var(_) => {
                if !self.had_undefined_var {
                    self.errors.push(error_at(
                        "cannot call a value whose type could not be inferred",
                        span,
                    ));
                }
                Type::Error
            }
            Type::Error => Type::Error,
            other => {
                if !self.had_undefined_var {
                    self.errors.push(error_at(
                        format!("cannot call a value of type `{other}`"),
                        span,
                    ));
                }
                Type::Error
            }
        }
    }

    /// Check that the given positional and named arguments match the parameter
    /// types.  `has_default` indicates which trailing parameters have defaults;
    /// callers may omit those.
    pub(crate) fn check_args_against(
        &mut self,
        param_names: &[String],
        ps: &[Type],
        has_default: &[bool],
        args: &[Expr],
        named: &[(String, Expr)],
        span: Span,
    ) {
        let total_provided = args.len() + named.len();
        let total_params = ps.len();
        let allowed_min = total_params - has_default.iter().filter(|&&d| d).count();

        if total_provided < allowed_min || total_provided > total_params {
            self.errors.push(error_at(
                format!(
                    "expected {} to {} arguments, found {}",
                    allowed_min, total_params, total_provided
                ),
                span,
            ));
            return;
        }

        let mut slots: Vec<Option<&Expr>> = vec![None; total_params];

        for (i, arg) in args.iter().enumerate() {
            if i >= total_params {
                self.errors.push(error_at(
                    format!("too many positional arguments (max {})", total_params),
                    arg.span(),
                ));
                return;
            }
            if slots[i].is_some() {
                self.errors.push(error_at(
                    format!("positional argument `{}` conflicts with named argument", i),
                    arg.span(),
                ));
                return;
            }
            slots[i] = Some(arg);
        }

        for (name, val) in named {
            let pos = param_names.iter().position(|pn| pn == name);
            match pos {
                Some(i) => {
                    if slots[i].is_some() {
                        self.errors.push(error_at(
                            format!("argument `{name}` already provided positionally"),
                            val.span(),
                        ));
                        return;
                    }
                    slots[i] = Some(val);
                }
                None => {
                    self.errors
                        .push(error_at(format!("unknown parameter `{name}`"), val.span()));
                    return;
                }
            }
        }

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
                    let expected = self.unifier.resolve(&ps[i]);
                    let ep = if let Type::Func(ep, _) = &expected {
                        Some(ep.as_slice())
                    } else {
                        None
                    };
                    let at = self.check_closure(params, ret_ty.as_ref(), body, *span, ep);
                    if let Err(e) = self.unifier.unify(&at, &ps[i]) {
                        self.report_mismatch(e, arg.span());
                    }
                } else {
                    let at = self.check_expr(arg);
                    let at = self.unifier.resolve(&at);
                    let expected = self.unifier.resolve(&ps[i]);
                    // A named function passed where a `func(...)` value is
                    // expected (e.g. decorator application `dec(target)` or
                    // HOFs like `map(xs, double)`): expand the signature so
                    // structural unification applies instead of failing on
                    // `Named` vs `Func`.
                    let at = match (&at, &expected) {
                        (Type::Named(n), Type::Func(_, _)) => match self.funcs.get(n).cloned() {
                            Some(sig) => {
                                let (sps, sret, _) = self.instantiate(&sig);
                                Type::Func(sps, Box::new(sret))
                            }
                            None => at,
                        },
                        _ => at,
                    };
                    if let Err(e) = self.unifier.unify(&at, &expected) {
                        self.report_mismatch(e, arg.span());
                    }
                }
            }
        }
    }

    /// Compile-time SQL verification for `db.query(sql)` / `db.exec(sql)`.
    ///
    /// The SQL arg is normally an interpolated string (`Fmt`): static text
    /// segments stay in the prepared statement, `{expr}` segments become
    /// `?N` bound parameters. This check ensures every bound expr is a
    /// bindable scalar (int/float/str/bool) and rejects format specs
    /// (`{x:.2f}` would render client-side, breaking parameterization).
    /// Static text is scanned for balanced quotes/parens as a cheap
    /// syntax sanity check; full schema verification happens at runtime
    /// against the live connection.
    pub(crate) fn verify_sql_params(&mut self, sql_arg: &Expr, span: Span) {
        let parts = match sql_arg {
            Expr::Fmt { parts, .. } => Some(parts.clone()),
            Expr::Str { .. } => None,
            Expr::Paren { expr, .. } => match expr.as_ref() {
                Expr::Fmt { parts, .. } => Some(parts.clone()),
                Expr::Str { .. } => None,
                _ => {
                    let at = self.check_expr(sql_arg);
                    if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                        self.report_mismatch(e, sql_arg.span());
                    }
                    return;
                }
            },
            _ => {
                let at = self.check_expr(sql_arg);
                if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                    self.report_mismatch(e, sql_arg.span());
                }
                return;
            }
        };
        let Some(parts) = parts else { return };
        let mut static_text = String::new();
        for part in &parts {
            match part {
                FmtPart::Text(t) => static_text.push_str(t),
                FmtPart::Expr(e, spec) => {
                    if let Some(s) = spec {
                        self.errors.push(error_at(
                            format!(
                                "format spec `:{s}` not allowed in SQL interpolation; use plain `{{...}}` so the value is bound as a parameter"
                            ),
                            e.span(),
                        ));
                    }
                    let bt = self.check_expr(e);
                    let rt = self.unifier.resolve(&bt);
                    match rt {
                        Type::Int
                        | Type::Float
                        | Type::Str
                        | Type::Bool
                        | Type::Var(_)
                        | Type::Error => {}
                        other => {
                            self.errors.push(error_at(
                                format!(
                                    "SQL parameter must be int, float, str, or bool, found `{other}`"
                                ),
                                e.span(),
                            ));
                        }
                    }
                }
            }
        }
        if let Some(msg) = check_sql_static(&static_text) {
            self.errors.push(error_at(msg, span));
        }
    }

    /// `expected_params`: when a closure is passed to a function with a
    /// known signature, the caller can provide the expected parameter types
    /// here. For each unannotated param, if an expected type exists, the
    /// fresh var is unified with it *before* checking the body — so the
    /// body sees known param types and can resolve method calls (e.g.
    /// `tx.exec(...)` when `tx: Db`).
    pub(crate) fn check_closure(
        &mut self,
        params: &[Param],
        ret_ty: Option<&Ty>,
        body: &Expr,
        _span: Span,
        expected_params: Option<&[Type]>,
    ) -> Type {
        self.push_scope();
        let mut ptypes = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let ty = match &p.ty {
                Some(t) => {
                    let gens = self.current_generics.clone();
                    self.ast_to_type(t, &gens)
                }
                None => {
                    if let Some(ep) = expected_params.and_then(|eps| eps.get(i)) {
                        // Bind the fresh var to the expected type so the
                        // body can resolve method calls on known types.
                        let fv = self.unifier.fresh_var();
                        if let Err(e) = self.unifier.unify(&fv, ep) {
                            self.report_mismatch(e, p.span);
                        }
                        fv
                    } else {
                        self.unifier.fresh_var()
                    }
                }
            };
            self.define(&p.name.name, ty.clone());
            ptypes.push(ty);
        }
        // Allow `return` inside closures — same semantics as named functions.
        // `try` inside a closure without an explicit `-> Result/Option`
        // annotation is a compile error (innermost enclosing scope rule).
        if ret_ty.is_none() && Self::expr_contains_try(body) {
            self.errors.push(error_at(
                "closure must declare `-> Result<T, E>` to use `try`",
                body.span(),
            ));
        }
        let ret_var = self.unifier.fresh_var();
        let prev_ret = self.current_ret.replace(ret_var.clone());
        let bt = self.check_expr(body);
        self.current_ret = prev_ret;
        self.pop_scope();
        // Unify body type with the return var (from any `return` statements).
        let _ = self.unifier.unify(&ret_var, &bt);
        let mut resolved_ret = self.unifier.resolve(&ret_var);
        // If a return type annotation is provided, unify it with the inferred return type.
        if let Some(ann) = ret_ty {
            let gens = self.current_generics.clone();
            let ann_type = self.ast_to_type(ann, &gens);
            if let Err(e) = self.unifier.unify(&ann_type, &resolved_ret) {
                self.report_mismatch(e, ann.span);
            }
            resolved_ret = self.unifier.resolve(&ann_type);
        }
        Type::Func(ptypes, Box::new(resolved_ret))
    }

    pub(crate) fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[zz_frontend::ast::MatchArm],
        span: Span,
    ) -> Type {
        let st = self.check_expr(scrutinee);
        let st = self.unifier.resolve(&st);
        self.check_exhaustive(&st, arms, span);
        let mut result: Option<Type> = None;
        for arm in arms {
            self.push_scope();
            self.bind_pattern(&arm.pat, &st);
            // Check match guard: must resolve to bool
            if let Some(ref guard) = arm.guard {
                let gt = self.check_expr(guard);
                if let Err(e) = self.unifier.unify(&gt, &Type::Bool) {
                    self.report_mismatch(e, guard.span());
                }
            }
            let bt = self.check_expr(&arm.body);
            self.pop_scope();
            // `break`/`continue` arms diverge (never produce a value),
            // so they don't constrain the match's result type.
            if matches!(arm.body, Expr::Break { .. } | Expr::Continue { .. }) {
                continue;
            }
            match &result {
                Some(r) => {
                    if let Err(e) = self.unifier.unify(&bt, r) {
                        self.report_mismatch(e, arm.body.span());
                    }
                }
                None => result = Some(bt),
            }
        }
        result.unwrap_or(Type::Unit)
    }

    pub(crate) fn check_try(&mut self, expr: &Expr, span: Span) -> Type {
        let ot = self.check_expr(expr);
        let ot = self.unifier.resolve(&ot);
        let ret = match &self.current_ret {
            Some(r) => self.unifier.resolve(r),
            None => {
                self.errors.push(error_at(
                    "`?`/`try` can only be used inside a function returning `Result` or `Option`",
                    span,
                ));
                return Type::Unit;
            }
        };
        match ot {
            Type::Option(t) => match &ret {
                Type::Option(_) => {
                    self.try_resolutions.insert(span, None);
                    *t
                }
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Option(t.clone()));
                    self.try_resolutions.insert(span, None);
                    *t
                }
                other => {
                    self.errors.push(error_at(
                        format!("`?` on `Option` cannot propagate through a function returning `{other}`"),
                        span,
                    ));
                    *t
                }
            },
            Type::Result(t, e) => match &ret {
                Type::Result(_, ret_e) => {
                    let ein = self.unifier.resolve(&e);
                    let eout = self.unifier.resolve(ret_e);
                    if ein == eout {
                        // Identity conversion: zero-cost, inlined away.
                        self.try_resolutions.insert(span, None);
                        if let Err(err) = self.unifier.unify(&e, ret_e) {
                            self.report_mismatch(err, span);
                        }
                    } else if matches!(ein, Type::Var(_)) || matches!(eout, Type::Var(_)) {
                        if let Err(err) = self.unifier.unify(&e, ret_e) {
                            self.report_mismatch(err, span);
                        }
                        self.try_resolutions.insert(span, None);
                    } else {
                        // Different error types: need a `convert_to_` impl.
                        let hits = self.find_converts(&ein, &eout);
                        match hits.len() {
                            1 => {
                                self.try_resolutions
                                    .insert(span, hits.into_iter().next().unwrap_or(None));
                            }
                            0 => {
                                self.errors.push(error_at(
                                    format!(
                                        "no conversion path for `try`: error type `{ein}` cannot convert to `{eout}`\n\
                                         hint: add `impl {ein} {{ func convert_to_{eout}(self) -> {eout} {{ ... }} }}`",
                                    ),
                                    span,
                                ));
                            }
                            _ => {
                                // Defensive: V1 registration rejects a second
                                // convert from the same source type, and the
                                // seeded-funcs fallback stops at the first hit,
                                // so multiple hits are currently unconstructible.
                                // Kept so a future relaxed registry still fails
                                // loudly at the `try` site per spec.
                                self.errors.push(error_at(
                                    format!(
                                        "ambiguous conversion for `try`: {} impls convert `{ein}` to `{eout}`",
                                        hits.len()
                                    ),
                                    span,
                                ));
                            }
                        }
                    }
                    *t
                }
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Result(t.clone(), e.clone()));
                    self.try_resolutions.insert(span, None);
                    *t
                }
                other => {
                    self.errors.push(error_at(
                        format!("`?` on `Result` cannot propagate through a function returning `{other}`\nhelp: enclosing function must return `Result<T, E>` to use `try`"),
                        span,
                    ));
                    *t
                }
            },
            Type::Var(_) => {
                self.errors.push(error_at(
                    "cannot use `?` on a value whose type could not be inferred",
                    span,
                ));
                Type::Unit
            }
            other => {
                self.errors.push(error_at(
                    format!("cannot use `?` on a value of type `{other}`"),
                    span,
                ));
                Type::Unit
            }
        }
    }

    /// True when an expression tree contains a `Try` (`?` / `try`) node.
    fn expr_contains_try(e: &Expr) -> bool {
        match e {
            Expr::Try { .. } => true,
            Expr::Binary { left, right, .. } => {
                Self::expr_contains_try(left) || Self::expr_contains_try(right)
            }
            Expr::Unary { expr, .. } => Self::expr_contains_try(expr),
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                Self::expr_contains_try(callee)
                    || args.iter().any(Self::expr_contains_try)
                    || named.iter().any(|(_, v)| Self::expr_contains_try(v))
            }
            Expr::Field { obj, .. } => Self::expr_contains_try(obj),
            Expr::Index { obj, index, .. } => {
                Self::expr_contains_try(obj) || Self::expr_contains_try(index)
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                Self::expr_contains_try(obj)
                    || start.as_ref().is_some_and(|s| Self::expr_contains_try(s))
                    || end.as_ref().is_some_and(|s| Self::expr_contains_try(s))
            }
            Expr::Array { elems, .. } => elems.iter().any(Self::expr_contains_try),
            Expr::Dict { entries, .. } => entries
                .iter()
                .any(|(k, v)| Self::expr_contains_try(k) || Self::expr_contains_try(v)),
            Expr::Tuple { items, .. } => items.iter().any(Self::expr_contains_try),
            Expr::If {
                cond, then, els, ..
            } => {
                Self::expr_contains_try(cond)
                    || then.stmts.iter().any(Self::stmt_contains_try)
                    || els.as_ref().is_some_and(|x| Self::expr_contains_try(x))
            }
            Expr::While { cond, body, .. } => {
                Self::expr_contains_try(cond) || body.stmts.iter().any(Self::stmt_contains_try)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                Self::expr_contains_try(scrutinee)
                    || arms.iter().any(|a| {
                        Self::expr_contains_try(&a.body)
                            || a.guard.as_ref().is_some_and(Self::expr_contains_try)
                    })
            }
            Expr::IfLet {
                value, then, els, ..
            } => {
                Self::expr_contains_try(value)
                    || then.stmts.iter().any(Self::stmt_contains_try)
                    || els.as_ref().is_some_and(|x| Self::expr_contains_try(x))
            }
            Expr::Block(b) => b.stmts.iter().any(Self::stmt_contains_try),
            // Nested closures/fns establish their own return scope and get
            // their own `check_closure` diagnostic — don't attribute their
            // `try` to the enclosing closure.
            Expr::Closure { .. } => false,
            Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
                FmtPart::Expr(x, _) => Self::expr_contains_try(x),
                _ => false,
            }),
            Expr::Paren { expr, .. } => Self::expr_contains_try(expr),
            Expr::StructInit { fields, .. } => {
                fields.iter().any(|(_, v)| Self::expr_contains_try(v))
            }
            Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| Self::expr_contains_try(a)),
            Expr::ListComp {
                body, iter, filter, ..
            } => {
                Self::expr_contains_try(body)
                    || Self::expr_contains_try(iter)
                    || filter.as_ref().is_some_and(|f| Self::expr_contains_try(f))
            }
            Expr::Range { start, end, .. } => {
                Self::expr_contains_try(start) || Self::expr_contains_try(end)
            }
            _ => false,
        }
    }

    fn stmt_contains_try(s: &Stmt) -> bool {
        match s {
            Stmt::Expr(e) => Self::expr_contains_try(e),
            Stmt::Decl { value, .. } => Self::expr_contains_try(value),
            Stmt::Assign { value, target, .. } => {
                Self::expr_contains_try(value) || Self::expr_contains_try(target)
            }
            Stmt::Destructure { value, .. } => Self::expr_contains_try(value),
            Stmt::Return { value, .. } => value.as_ref().is_some_and(Self::expr_contains_try),
            Stmt::For { iter, body, .. } => {
                Self::expr_contains_try(iter) || body.stmts.iter().any(Self::stmt_contains_try)
            }
            Stmt::Defer { expr, .. } => Self::expr_contains_try(expr),
            // Nested named functions/methods own their return scope.
            Stmt::Func { .. } | Stmt::Impl { .. } => false,
            _ => false,
        }
    }

    /// Find conversion impl spans from `from` to `to`.
    /// Checks the local `convert_impls` registry plus seeded `ext_methods` /
    /// `funcs` (cross-module `convert_to_` methods) so multi-file programs work.
    fn find_converts(&self, from: &Type, to: &Type) -> Vec<Option<Span>> {
        let mut out = Vec::new();
        let from_s = from.to_string();
        let to_s = to.to_string();
        for c in &self.convert_impls {
            if c.from == *from && c.to == *to {
                out.push(Some(c.span));
            }
        }
        if !out.is_empty() {
            return out;
        }
        // Fallback: scan merged registries for `X.convert_to_Y` with matching sig.
        let mut seen_fn = std::collections::HashSet::new();
        for ((tkey, mname), (sig, mspan)) in &self.ext_methods {
            if !mname.starts_with("convert_to_") || sig.params.is_empty() {
                continue;
            }
            if sig.params[0].1 == *from && sig.ret == *to {
                let _ = tkey;
                if seen_fn.insert(sig.ret.to_string() + &from_s + &to_s) {
                    out.push(Some(*mspan));
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
        for (fname, sig) in &self.funcs {
            if let Some((tkey, mname)) = fname.rsplit_once('.') {
                if !mname.starts_with("convert_to_") || sig.params.is_empty() {
                    continue;
                }
                let _ = tkey;
                if sig.params[0].1 == *from && sig.ret == *to {
                    out.push(None);
                    break;
                }
            }
        }
        out
    }

    // --- patterns ---------------------------------------------------------

    pub(crate) fn bind_pattern(&mut self, pat: &Pattern, ty: &Type) {
        match pat {
            Pattern::Wildcard { .. } => {}
            Pattern::Binding { name } => {
                self.define(&name.name, ty.clone());
            }
            Pattern::Literal { value, span } => {
                let lit_t = match value {
                    Lit::Int(_) => Type::Int,
                    Lit::Float(_) => Type::Float,
                    Lit::Str(_) => Type::Str,
                    Lit::Bool(_) => Type::Bool,
                };
                if let Err(e) = self.unifier.unify(&lit_t, ty) {
                    self.report_mismatch(e, *span);
                }
            }
            Pattern::Variant { name, arg, span } => {
                let rt = self.unifier.resolve(ty);
                let inner = match (&rt, name.as_str()) {
                    (Type::Option(inner), "some") => match arg {
                        Some(p) => Some((p.as_ref().clone(), (**inner).clone())),
                        None => {
                            self.errors
                                .push(error_at("`.some` pattern requires an argument", *span));
                            None
                        }
                    },
                    (Type::Option(_), "none") => {
                        if arg.is_some() {
                            self.errors
                                .push(error_at("`.none` pattern takes no argument", *span));
                        }
                        None
                    }
                    (Type::Result(t, _), "ok") => {
                        arg.as_ref().map(|p| (p.as_ref().clone(), (**t).clone()))
                    }
                    (Type::Result(_, e), "err") => {
                        arg.as_ref().map(|p| (p.as_ref().clone(), (**e).clone()))
                    }
                    (Type::Var(_), _) => arg
                        .as_ref()
                        .map(|p| (p.as_ref().clone(), self.unifier.fresh_var())),
                    (other, vname) => {
                        self.errors.push(error_at(
                            format!("pattern `.{vname}` does not match a value of type `{other}`"),
                            *span,
                        ));
                        None
                    }
                };
                if let Some((p, inner)) = inner {
                    self.bind_pattern(&p, &inner);
                }
            }
            Pattern::Or { pats, span } => {
                if pats.is_empty() {
                    return;
                }
                // Snapshot current scope keys so we can diff what the
                // first alternative binds.
                let before: std::collections::HashSet<String> = self
                    .env
                    .last()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                self.bind_pattern(&pats[0], ty);
                let first_new: std::collections::HashMap<String, Type> = self
                    .env
                    .last()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(k, _)| !before.contains(k))
                    .collect();
                let first_names: std::collections::HashSet<String> =
                    first_new.keys().cloned().collect();
                for alt in &pats[1..] {
                    // Bind alternative in a throwaway scope (no unused
                    // warnings) to collect its bindings for comparison.
                    self.env.push(std::collections::HashMap::new());
                    self.defined_names.push(std::collections::HashMap::new());
                    self.const_env.push(std::collections::HashMap::new());
                    self.bind_pattern(alt, ty);
                    let alt_map = self.env.pop().unwrap_or_default();
                    self.defined_names.pop();
                    self.const_env.pop();
                    let alt_names: std::collections::HashSet<String> =
                        alt_map.keys().cloned().collect();
                    if alt_names != first_names {
                        self.errors.push(error_at(
                            "or-pattern alternatives must bind the same names",
                            *span,
                        ));
                        continue;
                    }
                    for (name, alt_ty) in alt_map {
                        if let Some(first_ty) = first_new.get(&name) {
                            if let Err(e) = self.unifier.unify(&alt_ty, first_ty) {
                                self.report_mismatch(e, alt.span());
                            }
                        }
                    }
                }
            }
            Pattern::Tuple { pats, span } => {
                let rt = self.unifier.resolve(ty);
                match rt {
                    Type::Tuple(inner_types) => {
                        if pats.len() != inner_types.len() {
                            self.errors.push(error_at(
                                format!(
                                    "expected tuple with {} elements, found {}",
                                    inner_types.len(),
                                    pats.len()
                                ),
                                *span,
                            ));
                        } else {
                            for (pat, inner_ty) in pats.iter().zip(inner_types.iter()) {
                                self.bind_pattern(pat, inner_ty);
                            }
                        }
                    }
                    Type::Var(_) => {
                        // If type is unknown, bind all patterns to fresh vars
                        let fv = self.unifier.fresh_var();
                        for pat in pats {
                            self.bind_pattern(pat, &fv);
                        }
                    }
                    other => {
                        self.errors.push(error_at(
                            format!(
                                "cannot destructure a value of type `{other}` into a tuple pattern"
                            ),
                            *span,
                        ));
                    }
                }
            }
        }
    }

    pub(crate) fn check_exhaustive(
        &mut self,
        st: &Type,
        arms: &[zz_frontend::ast::MatchArm],
        span: Span,
    ) {
        fn pat_is_wildcard(pat: &Pattern) -> bool {
            match pat {
                Pattern::Wildcard { .. } => true,
                Pattern::Or { pats, .. } => pats.iter().any(pat_is_wildcard),
                _ => false,
            }
        }
        if arms.iter().any(|a| pat_is_wildcard(&a.pat)) {
            return;
        }
        let needs: Option<Vec<&str>> = match st {
            Type::Option(_) => Some(vec!["some", "none"]),
            Type::Result(_, _) => Some(vec!["ok", "err"]),
            Type::Bool => Some(vec!["true", "false"]),
            Type::Int | Type::Float | Type::Str | Type::Unit => {
                self.errors.push(error_at(
                    format!("match on `{st}` requires a `_` wildcard arm"),
                    span,
                ));
                return;
            }
            _ => return,
        };
        let Some(needs) = needs else { return };
        fn pat_tags(pat: &Pattern, out: &mut Vec<String>) {
            match pat {
                Pattern::Variant { name, .. } => out.push(name.clone()),
                Pattern::Literal {
                    value: Lit::Bool(b),
                    ..
                } => out.push(if *b { "true" } else { "false" }.to_string()),
                Pattern::Or { pats, .. } => {
                    for p in pats {
                        pat_tags(p, out);
                    }
                }
                _ => {}
            }
        }
        let mut have: Vec<String> = Vec::new();
        for a in arms {
            pat_tags(&a.pat, &mut have);
        }
        let missing: Vec<&str> = needs
            .iter()
            .filter(|n| !have.iter().any(|h| h == *n))
            .copied()
            .collect();
        if !missing.is_empty() {
            let missing = missing
                .iter()
                .map(|m| format!("`.{m}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            self.errors.push(error_at(
                format!("non-exhaustive match: missing {missing} (or add a `_` arm)"),
                span,
            ));
        }
    }
}
