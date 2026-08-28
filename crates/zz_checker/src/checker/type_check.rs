//! Core type-checking logic: statements, expressions, patterns.

use crate::checker::inference::{contains_var, default_variant_vars};
use crate::checker::Checker;
use crate::type_::Type;
use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Lit, Param, Pattern, Stmt, UnOp};
use zz_frontend::diag::{error_at, FixIt};
use zz_frontend::levenshtein::suggest_all;
use zz_frontend::span::Span;

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
            } => {
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
                        self.errors.push(error_at(
                            format!(
                                "cannot infer the type of `{}`; add a type annotation",
                                name.name
                            ),
                            name.span,
                        ));
                    } else {
                        self.define_at(&name.name, d.clone(), name.span);
                        if self.env.len() == 1 {
                            self.new_bindings.insert(name.name.clone(), d.clone());
                        }
                        return d;
                    }
                }
                if self.env.len() == 1 {
                    self.new_bindings.insert(name.name.clone(), vt.clone());
                }
                self.define_at(&name.name, rt.clone(), name.span);
                rt
            }
            Stmt::Import { path, alias, span } => {
                let ns = alias
                    .as_ref()
                    .cloned()
                    .or_else(|| path.last().cloned())
                    .unwrap_or_default();
                self.imports.push((ns, *span));
                Type::Unit
            }
            Stmt::Func { .. } => {
                let sig = self.funcs.get(&Self::func_name(stmt)).unwrap().clone();
                self.check_func_body(stmt, &sig);
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
                var,
                iter,
                body,
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
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
                self.pop_scope();
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
            Stmt::Assign {
                target,
                value,
                span,
            } => {
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
                    Type::Struct(sname) => match self.structs.get(&sname) {
                        Some(sig) => match sig.fields.iter().find(|(n, _)| n == name) {
                            Some((_, ft)) => ft.clone(),
                            None => {
                                let field_names: Vec<&str> =
                                    sig.fields.iter().map(|(n, _)| n.as_str()).collect();
                                let mut diag = error_at(
                                    format!("struct `{sname}` has no field `{name}`"),
                                    *span,
                                );
                                let all = suggest_all(name, &field_names);
                                if let Some((suggestion, _)) = all.first() {
                                    diag = diag
                                        .with_note(format!("did you mean field `{suggestion}`?"));
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
                        None => {
                            self.errors
                                .push(error_at(format!("unknown struct `{sname}`"), *span));
                            Type::Unit
                        }
                    },
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

    pub(crate) fn check_expr(&mut self, e: &Expr) -> Type {
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
                    Type::Struct(sname) => match self.structs.get(&sname) {
                        Some(sig) => match sig.fields.iter().find(|(n, _)| n == name) {
                            Some((_, ft)) => ft.clone(),
                            None => {
                                let field_names: Vec<&str> =
                                    sig.fields.iter().map(|(n, _)| n.as_str()).collect();
                                let mut diag = error_at(
                                    format!("struct `{sname}` has no field `{name}`"),
                                    *span,
                                );
                                let all = suggest_all(name, &field_names);
                                if let Some((suggestion, _)) = all.first() {
                                    diag = diag
                                        .with_note(format!("did you mean field `{suggestion}`?"));
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
                        None => {
                            self.errors
                                .push(error_at(format!("unknown struct `{sname}`"), *span));
                            Type::Unit
                        }
                    },
                    other => {
                        let method = name.clone();
                        let ns = match &other {
                            Type::Str => Some("str"),
                            Type::Array(_) => Some("vec"),
                            Type::Option(_) => Some("option"),
                            Type::Result(_, _) => Some("result"),
                            _ => None,
                        };
                        if let Some(ns) = ns {
                            if let Some(sig) = self.funcs.get(&format!("{ns}.{method}")).cloned() {
                                let (ps, ret) = self.instantiate(&sig);
                                if !ps.is_empty() {
                                    if let Err(e) = self.unifier.unify(&other, &ps[0]) {
                                        self.report_mismatch(e, *span);
                                    }
                                }
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
                let Some(sig) = self.structs.get(name).cloned() else {
                    self.errors
                        .push(error_at(format!("unknown struct `{name}`"), *span));
                    return Type::Unit;
                };
                for (fname, fval) in fields {
                    let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == fname) else {
                        self.errors.push(error_at(
                            format!("struct `{name}` has no field `{fname}`"),
                            fval.span(),
                        ));
                        continue;
                    };
                    let vt = self.check_expr(fval);
                    if let Err(e) = self.unifier.unify(&vt, ft) {
                        self.report_mismatch(e, fval.span());
                    }
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
            Expr::Closure { params, body, span } => self.check_closure(params, body, *span),
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
                        if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                            self.report_mismatch(err, *span);
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
                        if let Err(e) = self.unifier.unify(&*inner, &rt_resolved) {
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
                let lt = self.check_expr(left);
                let rt = self.check_expr(right);
                if let Err(e) = self.unifier.unify(&rt, &lt) {
                    self.report_mismatch(e, span);
                }
                Type::Bool
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.check_arith(op, left, right, span)
            }
        }
    }

    pub(crate) fn check_arith(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
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
                        Type::Option(_) => {
                            sig = self.funcs.get(&format!("option.{method}")).cloned()
                        }
                        Type::Result(_, _) => {
                            sig = self.funcs.get(&format!("result.{method}")).cloned()
                        }
                        Type::Struct(sname) => {
                            if let Some((ns, _)) = sname.rsplit_once('.') {
                                sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(sig) = sig {
                    let (ps, ret) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
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
                    return ret;
                }
                None
            }
            _ => None,
        };
        if let Some(name) = &direct_name {
            if let Some(sig) = self.funcs.get(name).cloned() {
                self.used_names.insert(name.clone());
                let (ps, ret) = self.instantiate(&sig);
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
                return ret;
            }
        }
        // Method call: `p.dist()` resolves to `dist(p, ...)`.
        if let Expr::Path { parts, span: pspan } = callee {
            if parts.len() >= 2 {
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
                        Type::Struct(sname) => {
                            if let Some((ns, _)) = sname.rsplit_once('.') {
                                sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(sig) = sig {
                    let (ps, ret) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
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
                    let (ps, ret) = self.instantiate(&sig);
                    let param_names: Vec<String> =
                        sig.params.iter().map(|(n, _)| n.clone()).collect();
                    self.check_args_against(&param_names, &ps, &sig.has_default, args, named, span);
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
                let at = self.check_expr(arg);
                if let Err(e) = self.unifier.unify(&at, &ps[i]) {
                    self.report_mismatch(e, arg.span());
                }
            }
        }
    }

    pub(crate) fn check_closure(&mut self, params: &[Param], body: &Expr, _span: Span) -> Type {
        self.push_scope();
        let mut ptypes = Vec::new();
        for p in params {
            let ty = match &p.ty {
                Some(t) => {
                    let gens = self.current_generics.clone();
                    self.ast_to_type(t, &gens)
                }
                None => self.unifier.fresh_var(),
            };
            self.define(&p.name.name, ty.clone());
            ptypes.push(ty);
        }
        let bt = self.check_expr(body);
        self.pop_scope();
        Type::Func(ptypes, Box::new(bt))
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
            let bt = self.check_expr(&arm.body);
            self.pop_scope();
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
                    "`?` can only be used inside a function returning `Result` or `Option`",
                    span,
                ));
                return Type::Unit;
            }
        };
        match ot {
            Type::Option(t) => match &ret {
                Type::Option(_) => *t,
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Option(t.clone()));
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
                    if let Err(err) = self.unifier.unify(&e, ret_e) {
                        self.report_mismatch(err, span);
                    }
                    *t
                }
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Result(t.clone(), e.clone()));
                    *t
                }
                other => {
                    self.errors.push(error_at(
                        format!("`?` on `Result` cannot propagate through a function returning `{other}`"),
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
        }
    }

    pub(crate) fn check_exhaustive(
        &mut self,
        st: &Type,
        arms: &[zz_frontend::ast::MatchArm],
        span: Span,
    ) {
        if arms
            .iter()
            .any(|a| matches!(a.pat, Pattern::Wildcard { .. }))
        {
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
        let have: Vec<String> = arms
            .iter()
            .filter_map(|a| match &a.pat {
                Pattern::Variant { name, .. } => Some(name.clone()),
                Pattern::Literal {
                    value: Lit::Bool(b),
                    ..
                } => Some(if *b { "true" } else { "false" }.to_string()),
                _ => None,
            })
            .collect();
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
