//! Type checker: HM-lite inference, generics, patterns, exhaustiveness.
//!
//! Design (Phase 1 scope):
//! - Inference is unification-based; every unresolved inference variable left
//!   in a top-level binding is an error (monomorphic, no generalization).
//! - Generic parameters are explicit (`func id<T>(...)`); at call sites they
//!   are instantiated with fresh variables.
//! - `Option`/`Result` are built-in; constructors are `.ok/.err/.some/.none`,
//!   and `?` propagates through enclosing functions returning the same shape.
//! - `match`/`if let` patterns are checked against the scrutinee type with
//!   exhaustiveness enforcement for Option/Result/bool.

use std::collections::HashMap;

use zz_frontend::ast::{
    BinOp, Block, Expr, FmtPart, Lit, MatchArm, Param, Pattern, Program, Stmt, Ty, TyKind, UnOp,
};
use zz_frontend::diag::{error_at, RawDiag};
use zz_frontend::span::Span;

use crate::type_::Type;
use crate::unify::{Unifier, UnifyError};

/// A registered function signature.
#[derive(Debug, Clone)]
pub struct FuncSig {
    pub generics: Vec<String>,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
}

/// A registered struct definition: field names and their types.
#[derive(Debug, Clone)]
pub struct StructSig {
    pub fields: Vec<(String, Type)>,
}

pub struct CheckResult {
    pub errors: Vec<RawDiag>,
    /// Top-level `let` bindings and their types (fully resolved).
    pub bindings: HashMap<String, Type>,
    /// Top-level function signatures.
    pub funcs: HashMap<String, FuncSig>,
    /// Top-level struct definitions.
    pub structs: HashMap<String, StructSig>,
}

/// Type-check a whole program, seeded with bindings/funcs/structs from prior
/// REPL evals. Errors are collected (not fatal); the program should not run
/// if any are present.
pub fn check_program(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
) -> CheckResult {
    let mut checker = Checker::new(initial_bindings, initial_funcs, initial_structs);

    // Pass 1: register struct definitions (fields are resolved against the
    // struct registry, so structs may reference earlier structs). Structs
    // must be registered before functions so `func f(p: Point)` resolves.
    let mut seen_structs = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Struct { name, span, .. } = stmt {
            let full_name = name.join(".");
            if let Some(prev) = seen_structs.insert(full_name.clone(), *span) {
                checker.errors.push(error_at(
                    format!("duplicate definition of struct `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(error_at("previous definition here", prev));
            }
            checker.collect_struct(stmt);
        }
    }

    // Pass 1b: register all function signatures so recursion and mutual
    // recursion resolve.
    let mut seen = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Func { name, span, .. } = stmt {
            let full_name = name.join(".");
            if let Some(prev) = seen.insert(full_name.clone(), *span) {
                checker.errors.push(error_at(
                    format!("duplicate definition of function `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(error_at("previous definition here", prev));
            }
            checker.collect_func(stmt);
        }
    }

    // Pass 2: check top-level statements in order.
    for stmt in &program.stmts {
        checker.check_stmt(stmt);
    }

    // Finalize: bindings that still contain inference variables were already
    // reported inline (see check_stmt `Let`); skip them so the session never
    // seeds an unresolved type.
    let mut bindings = HashMap::new();
    for (name, ty) in checker.new_bindings {
        let rt = checker.unifier.resolve_deep(&ty);
        if !contains_var(&rt) {
            bindings.insert(name, rt);
        }
    }

    CheckResult {
        errors: checker.errors,
        bindings,
        funcs: checker.funcs,
        structs: checker.structs,
    }
}

fn contains_var(t: &Type) -> bool {
    match t {
        Type::Var(_) => true,
        Type::Tuple(ts) => ts.iter().any(contains_var),
        Type::Option(x) => contains_var(x),
        Type::Result(a, b) => contains_var(a) || contains_var(b),
        Type::Func(ps, r) => ps.iter().any(contains_var) || contains_var(r),
        Type::Array(x) => contains_var(x),
        Type::Dict(k, v) => contains_var(k) || contains_var(v),
        Type::Union(ts) => ts.iter().any(contains_var),
        Type::Range(x) => contains_var(x),
        _ => false,
    }
}

/// Replace unresolved type variables inside `Option`/`Result` with `unit`
/// so `.none`/`.ok`/`.err` bindings type-check without annotations.
fn default_variant_vars(t: &mut Type) {
    match t {
        Type::Option(inner) => {
            if contains_var(inner) {
                default_variant_vars(inner);
                if contains_var(inner) {
                    **inner = Type::Unit;
                }
            }
        }
        Type::Result(ok, err) => {
            if contains_var(ok) {
                default_variant_vars(ok);
                if contains_var(ok) {
                    **ok = Type::Unit;
                }
            }
            if contains_var(err) {
                default_variant_vars(err);
                if contains_var(err) {
                    **err = Type::Unit;
                }
            }
        }
        _ => {}
    }
}

struct Checker {
    unifier: Unifier,
    errors: Vec<RawDiag>,
    funcs: HashMap<String, FuncSig>,
    structs: HashMap<String, StructSig>,
    env: Vec<HashMap<String, Type>>,
    /// Top-level let bindings discovered this run: name → (type, span).
    new_bindings: HashMap<String, Type>,
    current_ret: Option<Type>,
    current_generics: Vec<String>,
    /// Nesting depth of `for`/`while` loops (for `break`/`continue`).
    loop_depth: usize,
}

impl Checker {
    fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
    ) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: Unifier::new(),
            errors: Vec::new(),
            funcs,
            structs,
            env,
            new_bindings: HashMap::new(),
            current_ret: None,
            current_generics: Vec::new(),
            loop_depth: 0,
        }
    }

    // --- environments -----------------------------------------------------

    fn push_scope(&mut self) {
        self.env.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.env.pop();
    }

    fn define(&mut self, name: &str, ty: Type) {
        self.env.last_mut().unwrap().insert(name.to_string(), ty);
    }

    fn lookup(&mut self, name: &str, span: Span) -> Type {
        match self.lookup_opt(name) {
            Some(t) => t,
            None => {
                if let Some(sig) = self.funcs.get(name) {
                    if !sig.generics.is_empty() {
                        self.errors.push(error_at(
                            format!(
                                "cannot use generic function `{name}` as a value; call it with arguments"
                            ),
                            span,
                        ));
                        return Type::Unit;
                    }
                }
                self.errors
                    .push(error_at(format!("undefined variable `{name}`"), span));
                Type::Unit
            }
        }
    }

    /// Like [`Checker::lookup`] but without the error: returns `None` when
    /// the name is not bound anywhere.
    fn lookup_opt(&mut self, name: &str) -> Option<Type> {
        for scope in self.env.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        if let Some(sig) = self.funcs.get(name) {
            if !sig.generics.is_empty() {
                return None;
            }
            // Function used as a value: give its (uninstantiated) type. Call
            // sites handle generic instantiation via the Named path below.
            return Some(Type::Named(name.to_string()));
        }
        None
    }

    /// Resolve a dotted path: first as a single qualified name (module
    /// bindings/functions), then as a struct-field walk from the first part.
    fn lookup_path(&mut self, parts: &[String], span: Span) -> Type {
        let joined = parts.join(".");
        if let Some(t) = self.lookup_opt(&joined) {
            return t;
        }
        let Some(root) = self.lookup_opt(&parts[0]) else {
            self.errors
                .push(error_at(format!("undefined variable `{joined}`"), span));
            return Type::Unit;
        };
        let mut ty = root;
        for field in &parts[1..] {
            match self.unifier.resolve(&ty) {
                Type::Struct(name) => match self.structs.get(&name) {
                    Some(sig) => match sig.fields.iter().find(|(n, _)| n == field) {
                        Some((_, ft)) => ty = ft.clone(),
                        None => {
                            self.errors.push(error_at(
                                format!("struct `{name}` has no field `{field}`"),
                                span,
                            ));
                            return Type::Unit;
                        }
                    },
                    None => {
                        self.errors
                            .push(error_at(format!("unknown struct `{name}`"), span));
                        return Type::Unit;
                    }
                },
                other => {
                    self.errors.push(error_at(
                        format!("cannot access field `{field}` on a value of type `{other}`"),
                        span,
                    ));
                    return Type::Unit;
                }
            }
        }
        ty
    }

    // --- statements -------------------------------------------------------

    fn check_stmt(&mut self, stmt: &Stmt) -> Type {
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
                    // `.none`/`.ok`/`.err` bindings have an unconstrained
                    // variant parameter; default it to `unit` so they can be
                    // stored without an annotation.
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
                        self.define(&name.name, d.clone());
                        if self.env.len() == 1 {
                            self.new_bindings.insert(name.name.clone(), d.clone());
                        }
                        return d;
                    }
                }
                if self.env.len() == 1 {
                    self.new_bindings.insert(name.name.clone(), vt.clone());
                }
                self.define(&name.name, rt.clone());
                rt
            }
            Stmt::Import { .. } => {
                // Imports are resolved in a later phase; type-checking treats
                // them as a no-op.
                Type::Unit
            }
            Stmt::Func { .. } => {
                // Signature registered in pass 1; check the body against it.
                let sig = self.funcs.get(&func_name(stmt)).unwrap().clone();
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
            Stmt::Struct { .. } => {
                // Registered in pass 1b; nothing to check in the body.
                Type::Unit
            }
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
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let errors_before = self.errors.len();
                let tt = self.check_assign_target(target);
                let vt = self.check_expr(value);
                // Skip the unify when the target itself was rejected — the
                // target error is the real diagnosis; don't pile on a type
                // mismatch against the placeholder `unit`.
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
    fn check_assign_target(&mut self, target: &Expr) -> Type {
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
                                self.errors.push(error_at(
                                    format!("struct `{sname}` has no field `{name}`"),
                                    *span,
                                ));
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

    fn check_func_body(&mut self, stmt: &Stmt, sig: &FuncSig) {
        let (name, body) = match stmt {
            Stmt::Func { name, body, .. } => (name, body),
            _ => unreachable!(),
        };
        self.push_scope();
        for (pname, pty) in &sig.params {
            self.define(pname, pty.clone());
        }
        let prev_ret = self.current_ret.replace(sig.ret.clone());
        let prev_gen = std::mem::replace(&mut self.current_generics, sig.generics.clone());
        let body_t = self.check_block(body);
        self.current_ret = prev_ret;
        self.current_generics = prev_gen;
        self.pop_scope();
        let _ = name;
        if let Err(e) = self.unifier.unify(&body_t, &sig.ret) {
            self.report_mismatch(e, body.span);
        }
    }

    fn check_block(&mut self, block: &Block) -> Type {
        self.push_scope();
        let mut result = Type::Unit;
        for stmt in &block.stmts {
            result = self.check_stmt(stmt);
        }
        self.pop_scope();
        result
    }

    fn collect_struct(&mut self, stmt: &Stmt) {
        let (name, fields) = match stmt {
            Stmt::Struct { name, fields, .. } => (name, fields),
            _ => unreachable!(),
        };
        let gens = self.current_generics.clone();
        let sig_fields = fields
            .iter()
            .map(|(fname, fty)| (fname.name.clone(), self.ast_to_type(fty, &gens)))
            .collect();
        let full_name = name.join(".");
        self.structs
            .insert(full_name, StructSig { fields: sig_fields });
    }

    fn collect_func(&mut self, stmt: &Stmt) {
        let (name, generics, params, ret) = match stmt {
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                ..
            } => (name, generics, params, ret),
            _ => unreachable!(),
        };
        let gen_names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
        let sig_params = params
            .iter()
            .map(|p| {
                let ty = match &p.ty {
                    Some(t) => self.ast_to_type(t, &gen_names),
                    None => self.unifier.fresh_var(),
                };
                (p.name.name.clone(), ty)
            })
            .collect();
        let sig_ret = match ret {
            Some(t) => self.ast_to_type(t, &gen_names),
            None => self.unifier.fresh_var(),
        };
        let full_name = name.join(".");
        self.funcs.insert(
            full_name,
            FuncSig {
                generics: gen_names,
                params: sig_params,
                ret: sig_ret,
            },
        );
    }

    // --- expressions ------------------------------------------------------

    /// Merge element types into a single type: identical types collapse to
    /// one; differing types form a union.
    fn merge_types(&mut self, types: Vec<Type>) -> Type {
        if types.is_empty() {
            return self.unifier.fresh_var();
        }
        let mut distinct: Vec<Type> = Vec::new();
        for t in types {
            let rt = self.unifier.resolve(&t);
            if let Some(existing) = distinct.iter().find(|d| self.unifier.resolve(d) == rt) {
                let _ = self.unifier.unify(existing, &t);
            } else {
                distinct.push(t);
            }
        }
        if distinct.len() == 1 {
            distinct.pop().unwrap()
        } else {
            Type::Union(distinct)
        }
    }

    fn check_expr(&mut self, e: &Expr) -> Type {
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
                                self.errors.push(error_at(
                                    format!("struct `{sname}` has no field `{name}`"),
                                    *span,
                                ));
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
                // Interpolated strings are `str`; embedded expressions are
                // checked for validity (their Display form is used at runtime).
                for part in parts {
                    if let FmtPart::Expr(e) = part {
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
            Expr::Call { callee, args, span } => self.check_call(callee, args, *span),
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

    fn check_unary(&mut self, op: UnOp, expr: &Expr, span: Span) -> Type {
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

    fn check_binary(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        match op {
            BinOp::And | BinOp::Or => {
                let lt = self.check_expr(left);
                self.ensure_bool(lt, left.span());
                let rt = self.check_expr(right);
                self.ensure_bool(rt, right.span());
                Type::Bool
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let lt = self.check_expr(left);
                let rt = self.check_expr(right);
                if let Err(e) = self.unifier.unify(&rt, &lt) {
                    self.report_mismatch(e, span);
                }
                Type::Bool
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                self.check_arith(op, left, right, span)
            }
        }
    }

    fn check_arith(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        let lt = self.check_expr(left);

        let lt = self.unifier.resolve(&lt);
        let rt = self.check_expr(right);

        let rt = self.unifier.resolve(&rt);
        let result = match (&lt, &rt) {
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
                self.errors.push(error_at(
                    format!("cannot apply `{}` to `{}` and `{}`", op.symbol(), a, b),
                    span,
                ));
                Type::Int
            }
        };
        result
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Type {
        // Direct call of a named function: bypass `lookup` so generic
        // functions are instantiated here rather than rejected as values.
        let direct_name = match callee {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            _ => None,
        };
        if let Some(name) = &direct_name {
            if let Some(sig) = self.funcs.get(name).cloned() {
                let (ps, ret) = self.instantiate(&sig);
                // Special case: `input` accepts 0 or 1 string argument (optional prompt)
                if name == "input" {
                    if args.len() > 1 {
                        self.errors.push(error_at(
                            format!("expected 0 or 1 arguments, found {}", args.len()),
                            span,
                        ));
                    } else if args.len() == 1 {
                        let at = self.check_expr(&args[0]);
                        if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                            self.report_mismatch(e, args[0].span());
                        }
                    }
                    return ret;
                }
                self.check_args_against(ps, args, span);
                return ret;
            }
        }
        // Method call: `p.dist()` resolves to `dist(p, ...)` when the full
        // path isn't a known function. The receiver is the path minus its
        // last component; the method is looked up by name — first bare, then
        // namespaced by the receiver's struct type (`shapes.Point` → tries
        // `shapes.dist`).
        if let Expr::Path { parts, span: pspan } = callee {
            if parts.len() >= 2 {
                let method = parts.last().unwrap();
                let recv_t = self.lookup_path(&parts[..parts.len() - 1], *pspan);
                let mut sig = self.funcs.get(method).cloned();
                if sig.is_none() {
                    if let Type::Struct(sname) = self.unifier.resolve(&recv_t) {
                        if let Some((ns, _)) = sname.rsplit_once('.') {
                            sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                        }
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
                    self.check_args_against(ps[1..].to_vec(), args, span);
                    return ret;
                }
            }
        }
        let callee_t = self.check_expr(callee);

        let callee_t = self.unifier.resolve(&callee_t);
        match callee_t {
            Type::Func(ps, ret) => {
                self.check_args_against(ps, args, span);
                *ret
            }
            Type::Named(name) => match self.funcs.get(&name).cloned() {
                Some(sig) => {
                    let (ps, ret) = self.instantiate(&sig);
                    self.check_args_against(ps, args, span);
                    ret
                }
                None => {
                    self.errors
                        .push(error_at(format!("unknown function `{name}`"), span));
                    Type::Unit
                }
            },
            Type::Var(_) => {
                self.errors.push(error_at(
                    "cannot call a value whose type could not be inferred",
                    span,
                ));
                Type::Unit
            }
            other => {
                self.errors.push(error_at(
                    format!("cannot call a value of type `{other}`"),
                    span,
                ));
                Type::Unit
            }
        }
    }

    fn check_args_against(&mut self, ps: Vec<Type>, args: &[Expr], span: Span) {
        if ps.len() != args.len() {
            self.errors.push(error_at(
                format!("expected {} arguments, found {}", ps.len(), args.len()),
                span,
            ));
            return;
        }
        for (arg, p) in args.iter().zip(ps.iter()) {
            let at = self.check_expr(arg);
            if let Err(e) = self.unifier.unify(&at, p) {
                self.report_mismatch(e, arg.span());
            }
        }
    }

    fn check_closure(&mut self, params: &[Param], body: &Expr, _span: Span) -> Type {
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

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Type {
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

    fn check_try(&mut self, expr: &Expr, span: Span) -> Type {
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

    fn bind_pattern(&mut self, pat: &Pattern, ty: &Type) {
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
                    (Type::Var(_), _) => {
                        // Unknown scrutinee type: bind optimistically.
                        arg.as_ref()
                            .map(|p| (p.as_ref().clone(), self.unifier.fresh_var()))
                    }
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

    fn check_exhaustive(&mut self, st: &Type, arms: &[MatchArm], span: Span) {
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
            _ => return, // Var, Named, Tuple, Func: skip
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

    // --- generics ---------------------------------------------------------

    fn instantiate(&mut self, sig: &FuncSig) -> (Vec<Type>, Type) {
        let subs: HashMap<String, Type> = sig
            .generics
            .iter()
            .map(|g| (g.clone(), self.unifier.fresh_var()))
            .collect();
        let params = sig.params.iter().map(|(_, t)| subst(t, &subs)).collect();
        let ret = subst(&sig.ret, &subs);
        (params, ret)
    }

    // --- type annotations -------------------------------------------------

    fn ast_to_type(&mut self, ty: &Ty, generics: &[String]) -> Type {
        self.ast_to_type_inner(ty, generics)
    }

    fn ast_to_type_inner(&mut self, ty: &Ty, generics: &[String]) -> Type {
        match &ty.kind {
            TyKind::Int => Type::Int,
            TyKind::Float => Type::Float,
            TyKind::Bool => Type::Bool,
            TyKind::Str => Type::Str,
            TyKind::Unit => Type::Unit,
            TyKind::Tuple(ts) => Type::Tuple(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Option(t) => Type::Option(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Result(t, e) => Type::Result(
                Box::new(self.ast_to_type_inner(t, generics)),
                Box::new(self.ast_to_type_inner(e, generics)),
            ),
            TyKind::Func(ps, r) => Type::Func(
                ps.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
                Box::new(self.ast_to_type_inner(r, generics)),
            ),
            TyKind::Array(t) => Type::Array(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Dict(k, v) => Type::Dict(
                Box::new(self.ast_to_type_inner(k, generics)),
                Box::new(self.ast_to_type_inner(v, generics)),
            ),
            TyKind::Union(ts) => Type::Union(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Named(name, args) => {
                if generics.iter().any(|g| g == name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("generic parameter `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Named(name.clone())
                } else if self.structs.contains_key(name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("struct `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Struct(name.clone())
                } else {
                    self.errors
                        .push(error_at(format!("unknown type `{name}`"), ty.span));
                    Type::Unit
                }
            }
        }
    }

    // --- errors -----------------------------------------------------------

    fn report_mismatch(&mut self, err: UnifyError, span: Span) {
        let msg = match err.message.as_str() {
            "type mismatch" => {
                format!(
                    "type mismatch: expected `{}`, found `{}`",
                    err.right, err.left
                )
            }
            "function arity mismatch" => "function arity mismatch".to_string(),
            "tuple arity mismatch" => "tuple arity mismatch".to_string(),
            other => other.to_string(),
        };
        self.errors.push(error_at(msg, span));
    }

    fn ensure_bool(&mut self, t: Type, span: Span) {
        match self.unifier.resolve(&t) {
            Type::Bool => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Bool);
            }
            other => {
                self.errors
                    .push(error_at(format!("expected `bool`, found `{other}`"), span));
            }
        }
    }

    fn ensure_int(&mut self, t: Type, span: Span) {
        match self.unifier.resolve(&t) {
            Type::Int => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Int);
            }
            other => {
                self.errors.push(error_at(
                    format!("index must be `int`, found `{other}`"),
                    span,
                ));
            }
        }
    }
}

fn func_name(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Func { name, .. } => name.join("."),
        _ => unreachable!(),
    }
}

fn subst(t: &Type, subs: &HashMap<String, Type>) -> Type {
    match t {
        Type::Named(name) => subs
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Named(name.clone())),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Option(x) => Type::Option(Box::new(subst(x, subs))),
        Type::Result(a, b) => Type::Result(Box::new(subst(a, subs)), Box::new(subst(b, subs))),
        Type::Func(ps, r) => Type::Func(
            ps.iter().map(|x| subst(x, subs)).collect(),
            Box::new(subst(r, subs)),
        ),
        Type::Array(x) => Type::Array(Box::new(subst(x, subs))),
        Type::Dict(k, v) => Type::Dict(Box::new(subst(k, subs)), Box::new(subst(v, subs))),
        Type::Union(ts) => Type::Union(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Range(x) => Type::Range(Box::new(subst(x, subs))),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    fn check_src(src: &str) -> CheckResult {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn errors_of(src: &str) -> Vec<String> {
        check_src(src)
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect()
    }

    fn errors_contain(src: &str, needle: &str) {
        let errs = errors_of(src);
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "expected error containing `{needle}`, got: {errs:?}"
        );
    }

    /// Check with a seeded function map (e.g. a generic builtin like `typeof`).
    fn check_src_with_funcs(src: &str, funcs: HashMap<String, FuncSig>) -> CheckResult {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        check_program(&parsed.program, HashMap::new(), funcs, HashMap::new())
    }

    /// Check with seeded functions and structs (e.g. a namespaced struct
    /// that only exists through module registration).
    fn check_src_with_funcs_and_structs(
        src: &str,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
    ) -> CheckResult {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        check_program(&parsed.program, HashMap::new(), funcs, structs)
    }

    #[test]
    fn infers_int_from_literal() {
        let r = check_src("x := 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn infers_float_from_promotion() {
        let r = check_src("x := 1 + 2.5");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Float);
    }

    #[test]
    fn annotation_unifies() {
        let r = check_src("float x = 1 + 2.5");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Float);
    }

    #[test]
    fn annotation_mismatch_errors() {
        errors_contain("str x = 1 + 2", "type mismatch");
    }

    #[test]
    fn type_mismatch_arithmetic() {
        errors_contain("1 + \"a\"", "cannot apply `+`");
    }

    #[test]
    fn bool_ops_require_bool() {
        errors_contain("1 && true", "expected `bool`, found `int`");
    }

    #[test]
    fn comparison_requires_same_type() {
        errors_contain("1 == \"a\"", "type mismatch");
    }

    #[test]
    fn undefined_variable_errors() {
        errors_contain("nope + 1", "undefined variable `nope`");
    }

    #[test]
    fn func_signature_and_body() {
        let r = check_src("func add(a: int, b: int) -> int { return a + b }\nz := add(1, 2)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn func_return_type_inferred() {
        let r = check_src("func five() { return 5 }\nz := five()");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn func_wrong_return_type_errors() {
        errors_contain("func f() -> int { return \"a\" }", "type mismatch");
    }

    #[test]
    fn wrong_arg_count_errors() {
        errors_contain(
            "func f(a: int) -> int { a }\nf(1, 2)",
            "expected 1 arguments, found 2",
        );
    }

    #[test]
    fn wrong_arg_type_errors() {
        errors_contain("func f(a: int) -> int { a }\nf(\"x\")", "type mismatch");
    }

    #[test]
    fn generic_func_instantiates() {
        let r = check_src("func id<T>(x: T) -> T { return x }\na := id(1)\nb := id(\"s\")");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["a"], Type::Int);
        assert_eq!(r.bindings["b"], Type::Str);
    }

    #[test]
    fn generic_func_monomorphic_fail() {
        errors_contain(
            "func id<T>(x: T) -> T { x }\nf := id",
            "cannot use generic function `id` as a value",
        );
    }

    #[test]
    fn recursion_works() {
        let r = check_src("func fact(n: int) -> int { if n <= 1 { 1 } else { n * fact(n - 1) } }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    // --- structs -----------------------------------------------------------

    #[test]
    fn struct_def_and_init() {
        let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["p"], Type::Struct("Point".into()));
        assert_eq!(r.structs["Point"].fields.len(), 2);
    }

    #[test]
    fn struct_field_access() {
        let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nz := p.x");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn struct_field_mutation() {
        let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn struct_field_mutation_type_mismatch_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = \"a\"",
            "type mismatch",
        );
    }

    #[test]
    fn struct_unknown_field_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.z",
            "has no field `z`",
        );
    }

    #[test]
    fn struct_unknown_field_in_init_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\np := Point{ x: 1, z: 2 }",
            "has no field `z`",
        );
    }

    #[test]
    fn struct_unknown_type_errors() {
        errors_contain("p := Nope{ x: 1 }", "unknown struct `Nope`");
    }

    #[test]
    fn struct_in_func_signature() {
        let r = check_src(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\nz := dist(Point{ x: 1, y: 2 })",
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn struct_wrong_arg_type_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x }\ndist(5)",
            "type mismatch",
        );
    }

    #[test]
    fn struct_field_on_non_struct_errors() {
        errors_contain("x := 5\nx.y", "cannot access field `y`");
    }

    #[test]
    fn struct_duplicate_definition_errors() {
        errors_contain(
            "struct A { x: int }\nstruct A { y: int }",
            "duplicate definition of struct `A`",
        );
    }

    #[test]
    fn struct_type_annotation_resolves() {
        let r = check_src("struct Point { x: int, y: int }\nPoint p = Point{ x: 1, y: 2 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["p"], Type::Struct("Point".into()));
    }

    // --- for loops ---------------------------------------------------------

    #[test]
    fn for_over_range() {
        let r = check_src("sum := 0\nfor i in 0..5 { sum = sum + i }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn for_over_array() {
        let r = check_src("total := 0\nfor n in [10, 20, 30] { total = total + n }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn for_loop_var_typed() {
        let r = check_src("for i in 0..5 { i }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn for_over_non_iterable_errors() {
        errors_contain("for i in 5 { i }", "cannot iterate a value of type `int`");
    }

    #[test]
    fn for_loop_var_scope_does_not_leak() {
        errors_contain("for i in 0..5 { i }\ni", "undefined variable `i`");
    }

    #[test]
    fn break_outside_loop_errors() {
        errors_contain("break", "`break` outside of a loop");
    }

    #[test]
    fn continue_outside_loop_errors() {
        errors_contain("continue", "`continue` outside of a loop");
    }

    #[test]
    fn break_inside_loop_ok() {
        let r = check_src("for i in 0..5 { if i == 2 { break } }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn break_inside_while_ok() {
        let r = check_src("x := 0\nwhile x < 5 { x = x + 1; break }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn range_bounds_must_be_int() {
        errors_contain("for i in 0.5..2.5 { i }", "range bounds must be `int`");
    }

    #[test]
    fn assignment_to_undefined_errors() {
        errors_contain("nope = 5", "undefined variable `nope`");
    }

    #[test]
    fn closure_inference() {
        let r = check_src("f := |x: int| x + 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["f"],
            Type::Func(vec![Type::Int], Box::new(Type::Int))
        );
    }

    #[test]
    fn closure_ambiguous_errors() {
        errors_contain("f := |x| x", "cannot infer the type of `f`");
    }

    #[test]
    fn calling_closure() {
        let r = check_src("f := |x: int| x + 1\ny := f(5)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["y"], Type::Int);
    }

    #[test]
    fn match_option() {
        let r = check_src("v := .some(1)\nx := match v { .some(n) => n, .none => 0 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn match_result() {
        let r =
            check_src("Result<int, str> v = .ok(1)\nx := match v { .ok(n) => n, .err(_) => 0 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn match_nonexhaustive_errors() {
        errors_contain("v := .some(1)\nmatch v { .some(n) => n }", "non-exhaustive");
    }

    #[test]
    fn match_on_int_requires_wildcard() {
        errors_contain("match 5 { 1 => 1 }", "requires a `_` wildcard arm");
    }

    #[test]
    fn match_arm_type_mismatch_errors() {
        errors_contain(
            "v := .some(1)\nmatch v { .some(n) => n, .none => \"x\" }",
            "type mismatch",
        );
    }

    #[test]
    fn if_let_binds() {
        let r = check_src("v := .some(5)\nx := if let .some(n) = v { n } else { 0 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn try_propagates_result() {
        let r = check_src(
            "func div(a: int, b: int) -> Result<int, str> { if b == 0 { .err(\"z\") } else { .ok(a / b) } }\nfunc f(a: int, b: int) -> Result<int, str> { q := div(a, b)?; .ok(q) }",
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn try_on_option() {
        let r = check_src("func f() -> Option<int> { x := .some(1)?; .some(x) }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn try_outside_function_errors() {
        errors_contain(".ok(1)?", "only be used inside a function");
    }

    #[test]
    fn try_on_plain_int_errors() {
        errors_contain(
            "func f() -> Result<int, str> { x := 5?; .ok(x) }",
            "cannot use `?` on a value of type `int`",
        );
    }

    #[test]
    fn try_error_type_mismatch() {
        errors_contain(
            "func a() -> Result<int, str> { .ok(1) }\nfunc b() -> Result<int, int> { x := a()?; .ok(x) }",
            "type mismatch",
        );
    }

    #[test]
    fn variant_type_inference() {
        // `.none`/`.ok`/`.err` default their unknown variant parameter to
        // `unit`; `.some`/`.ok` with a concrete argument infer fully.
        let r = check_src("a := .ok(1)\nb := .none\nc := .err(\"boom\")");
        assert!(
            r.errors.is_empty(),
            "expected no errors, got {:?}",
            r.errors
        );
        // A binding whose type still has a var after defaulting still errors.
        errors_contain("f := |x| x", "cannot infer the type of `f`");
    }

    #[test]
    fn return_outside_function_errors() {
        errors_contain("return 5", "`return` outside of a function");
    }

    #[test]
    fn if_else_type_unify() {
        let r = check_src("x := if true { 1 } else { 2 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn if_else_mismatch_errors() {
        errors_contain("x := if true { 1 } else { \"a\" }", "type mismatch");
    }

    #[test]
    fn if_condition_must_be_bool() {
        errors_contain("if 1 { 1 } else { 2 }", "expected `bool`");
    }

    #[test]
    fn while_condition_must_be_bool() {
        errors_contain("while 1 { f() }", "expected `bool`");
    }

    #[test]
    fn str_concat() {
        let r = check_src("s := \"a\" + \"b\"");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["s"], Type::Str);
    }

    #[test]
    fn str_plus_int_errors() {
        errors_contain("s := \"a\" + 1", "cannot apply `+`");
    }

    #[test]
    fn shadowing_allowed() {
        let r = check_src("x := 1\nx := x + 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn duplicate_func_errors() {
        errors_contain("func f() { 1 }\nfunc f() { 2 }", "duplicate definition");
    }

    #[test]
    fn array_literal_infers() {
        let r = check_src("scores := [10, 20, 30]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["scores"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn array_explicit_decl() {
        let r = check_src("[int] scores = [10, 20, 30]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["scores"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn array_mixed_types_form_union() {
        let r = check_src("v := [1, \"a\"]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["v"],
            Type::Array(Box::new(Type::Union(vec![Type::Int, Type::Str])))
        );
    }

    #[test]
    fn array_annotation_unifies_with_union() {
        let r = check_src("[int] v = [1, 2]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["v"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn array_type_mismatch_errors() {
        errors_contain("[int] v = [\"a\"]", "type mismatch");
    }

    #[test]
    fn array_union_member_accepted() {
        // Union semantics: a value matches a declared type if any member
        // matches. `[1, "a"]` has element type `int | str`, which contains
        // `int`, so the annotation is accepted.
        let r = check_src("[int] v = [1, \"a\"]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn empty_array_ambiguous() {
        errors_contain("v := []", "cannot infer the type of `v`");
    }

    #[test]
    fn dict_literal_infers() {
        let r = check_src("ages := {\"Zaid\": 20}");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["ages"],
            Type::Dict(Box::new(Type::Str), Box::new(Type::Int))
        );
    }

    #[test]
    fn dict_explicit_decl() {
        let r = check_src("{str: int} ages = {\"a\": 1}");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["ages"],
            Type::Dict(Box::new(Type::Str), Box::new(Type::Int))
        );
    }

    #[test]
    fn dict_union_value_type() {
        let r = check_src("{str: str | int} user = {\"name\": \"Zaid\", \"age\": 20}");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["user"],
            Type::Dict(
                Box::new(Type::Str),
                Box::new(Type::Union(vec![Type::Str, Type::Int]))
            )
        );
    }

    #[test]
    fn dict_key_mismatch_errors() {
        errors_contain("{str: int} m = {1: 2}", "type mismatch");
    }

    #[test]
    fn empty_dict_ambiguous() {
        errors_contain("m := {}", "cannot infer the type of `m`");
    }

    #[test]
    fn import_is_noop() {
        let r = check_src("import std.io\nx := 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn union_annotation_accepts_member() {
        let r = check_src("str | int v = 5");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        // Binding stores the value type (int), which unifies with the union.
        assert_eq!(r.bindings["v"], Type::Int);
    }

    #[test]
    fn union_mismatch_errors() {
        errors_contain("str | int v = true", "type mismatch");
    }

    // --- indexing & slicing -------------------------------------------------

    #[test]
    fn array_index_type() {
        let r = check_src("scores := [10, 20]\nx := scores[0]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn dict_index_type() {
        let r = check_src("ages := {\"a\": 1}\nx := ages[\"a\"]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn str_index_type() {
        let r = check_src("x := \"hello\"[1]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Str);
    }

    #[test]
    fn array_slice_type() {
        let r = check_src("scores := [10, 20, 30]\nx := scores[1:3]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn str_slice_type() {
        let r = check_src("x := \"hello\"[1:3]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Str);
    }

    #[test]
    fn index_non_indexable_errors() {
        errors_contain("x := 5\nx[0]", "cannot index a value of type `int`");
    }

    #[test]
    fn index_non_int_errors() {
        errors_contain("scores := [1, 2]\nscores[\"a\"]", "index must be `int`");
    }

    #[test]
    fn slice_non_sliceable_errors() {
        errors_contain("x := 5\nx[1:2]", "cannot slice a value of type `int`");
    }

    #[test]
    fn index_assign_type_checked() {
        let r = check_src("scores := [1, 2]\nscores[0] = 5");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn index_assign_wrong_type_errors() {
        errors_contain("scores := [1, 2]\nscores[0] = \"x\"", "type mismatch");
    }

    #[test]
    fn str_index_assign_errors() {
        errors_contain(
            "s := \"abc\"\ns[0] = \"x\"",
            "cannot assign to an index of a string",
        );
    }

    #[test]
    fn dict_index_assign_ok() {
        let r = check_src("ages := {\"a\": 1}\nages[\"b\"] = 2");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    // --- pipeline -----------------------------------------------------------

    #[test]
    fn pipe_type_checks() {
        let r = check_src("func dbl(a: int, b: int) -> int { a * b }\nx := 5 |> dbl(3)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn pipe_bare_name_type_checks() {
        let r = check_src("func inc(n: int) -> int { n + 1 }\nx := 5 |> inc");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn pipe_type_mismatch_errors() {
        errors_contain(
            "func dbl(a: int, b: int) -> int { a * b }\nx := \"s\" |> dbl(3)",
            "type mismatch",
        );
    }

    #[test]
    fn pipe_chain_type_checks() {
        let r = check_src(
            "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\nx := 5 |> inc |> dbl",
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    // --- typeof -------------------------------------------------------------

    #[test]
    fn typeof_any_value() {
        // `typeof` is a generic builtin: `typeof(v: T) -> str`.
        let mut funcs = HashMap::new();
        funcs.insert(
            "typeof".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), Type::Named("T".to_string()))],
                ret: Type::Str,
            },
        );
        for src in [
            "x := typeof(1)",
            "x := typeof(\"s\")",
            "x := typeof([1, 2])",
            "x := typeof({\"a\": 1})",
            "x := typeof(.some(1))",
        ] {
            let r = check_src_with_funcs(src, funcs.clone());
            assert!(r.errors.is_empty(), "errors for `{src}`: {:?}", r.errors);
            assert_eq!(r.bindings["x"], Type::Str, "for `{src}`");
        }
    }

    // --- method calls -------------------------------------------------------

    fn method_funcs() -> HashMap<String, FuncSig> {
        let mut funcs = HashMap::new();
        funcs.insert(
            "dist".to_string(),
            FuncSig {
                generics: Vec::new(),
                params: vec![
                    ("p".to_string(), Type::Struct("Point".to_string())),
                    ("scale".to_string(), Type::Int),
                ],
                ret: Type::Int,
            },
        );
        funcs
    }

    #[test]
    fn method_call_type_checks() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\np := Point{ x: 3 }\nz := p.dist(2)",
            method_funcs(),
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn method_call_receiver_mismatch_errors() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\nstruct Line { a: int }\nl := Line{ a: 1 }\nz := l.dist(2)",
            method_funcs(),
        );
        assert!(
            r.errors.iter().any(|e| e.message.contains("type mismatch")),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn method_call_arg_mismatch_errors() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\np := Point{ x: 3 }\nz := p.dist(\"s\")",
            method_funcs(),
        );
        assert!(
            r.errors.iter().any(|e| e.message.contains("type mismatch")),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn method_call_unknown_method_errors() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\np := Point{ x: 3 }\nz := p.nope()",
            method_funcs(),
        );
        assert!(
            r.errors
                .iter()
                .any(|e| e.message.contains("no field `nope`")),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn method_call_namespaced_by_struct_type() {
        // `shapes.Point` receiver resolves `dist` → `shapes.dist`. The
        // namespaced struct exists only through module registration, so it
        // is seeded rather than defined in source.
        let mut funcs = HashMap::new();
        funcs.insert(
            "shapes.dist".to_string(),
            FuncSig {
                generics: Vec::new(),
                params: vec![("p".to_string(), Type::Struct("shapes.Point".to_string()))],
                ret: Type::Int,
            },
        );
        let mut structs = HashMap::new();
        structs.insert(
            "shapes.Point".to_string(),
            StructSig {
                fields: vec![("x".to_string(), Type::Int)],
            },
        );
        let r = check_src_with_funcs_and_structs(
            "p := shapes.Point{ x: 3 }\nz := p.dist()",
            funcs,
            structs,
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    // --- conversions --------------------------------------------------------

    fn conv_funcs() -> HashMap<String, FuncSig> {
        let t = Type::Named("T".to_string());
        let mut funcs = HashMap::new();
        funcs.insert(
            "str".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), t.clone())],
                ret: Type::Str,
            },
        );
        funcs.insert(
            "int".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), t.clone())],
                ret: Type::Option(Box::new(Type::Int)),
            },
        );
        funcs.insert(
            "float".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), t.clone())],
                ret: Type::Option(Box::new(Type::Float)),
            },
        );
        funcs
    }

    #[test]
    fn conversion_sigs() {
        let r = check_src_with_funcs("a := str(1)\nb := int(\"42\")\nc := float(3)", conv_funcs());
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["a"], Type::Str);
        assert_eq!(r.bindings["b"], Type::Option(Box::new(Type::Int)));
        assert_eq!(r.bindings["c"], Type::Option(Box::new(Type::Float)));
    }

    #[test]
    fn conversion_any_value() {
        for src in ["a := str([1, 2])", "a := int(3.7)", "a := float(\"2.5\")"] {
            let r = check_src_with_funcs(src, conv_funcs());
            assert!(r.errors.is_empty(), "errors for `{src}`: {:?}", r.errors);
        }
    }
}
