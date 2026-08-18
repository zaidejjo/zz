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
    BinOp, Block, Expr, Lit, MatchArm, Param, Pattern, Program, Stmt, Ty, TyKind, UnOp,
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

pub struct CheckResult {
    pub errors: Vec<RawDiag>,
    /// Top-level `let` bindings and their types (fully resolved).
    pub bindings: HashMap<String, Type>,
    /// Top-level function signatures.
    pub funcs: HashMap<String, FuncSig>,
}

/// Type-check a whole program, seeded with bindings/funcs from prior REPL
/// evals. Errors are collected (not fatal); the program should not run if
/// any are present.
pub fn check_program(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
) -> CheckResult {
    let mut checker = Checker::new(initial_bindings, initial_funcs);

    // Pass 1: register all function signatures so recursion and mutual
    // recursion resolve.
    let mut seen = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Func { name, .. } = stmt {
            if let Some(prev) = seen.insert(name.name.clone(), name.span) {
                checker.errors.push(error_at(
                    format!("duplicate definition of function `{}`", name.name),
                    name.span,
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
    }
}

fn contains_var(t: &Type) -> bool {
    match t {
        Type::Var(_) => true,
        Type::Tuple(ts) => ts.iter().any(contains_var),
        Type::Option(x) => contains_var(x),
        Type::Result(a, b) => contains_var(a) || contains_var(b),
        Type::Func(ps, r) => ps.iter().any(contains_var) || contains_var(r),
        _ => false,
    }
}

struct Checker {
    unifier: Unifier,
    errors: Vec<RawDiag>,
    funcs: HashMap<String, FuncSig>,
    env: Vec<HashMap<String, Type>>,
    /// Top-level let bindings discovered this run: name → (type, span).
    new_bindings: HashMap<String, Type>,
    current_ret: Option<Type>,
    current_generics: Vec<String>,
}

impl Checker {
    fn new(initial_bindings: HashMap<String, Type>, funcs: HashMap<String, FuncSig>) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: Unifier::new(),
            errors: Vec::new(),
            funcs,
            env,
            new_bindings: HashMap::new(),
            current_ret: None,
            current_generics: Vec::new(),
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
        for scope in self.env.iter().rev() {
            if let Some(t) = scope.get(name) {
                return t.clone();
            }
        }
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
            // Function used as a value: give its (uninstantiated) type. Call
            // sites handle generic instantiation via the Named path below.
            return Type::Named(name.to_string());
        }
        self.errors
            .push(error_at(format!("undefined variable `{name}`"), span));
        Type::Unit
    }

    // --- statements -------------------------------------------------------

    fn check_stmt(&mut self, stmt: &Stmt) -> Type {
        match stmt {
            Stmt::Let {
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
                if self.env.len() == 1 {
                    self.new_bindings.insert(name.name.clone(), vt.clone());
                }
                if contains_var(&rt) {
                    self.errors.push(error_at(
                        format!(
                            "cannot infer the type of `{}`; add a type annotation",
                            name.name
                        ),
                        name.span,
                    ));
                }
                self.define(&name.name, rt.clone());
                rt
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
        self.funcs.insert(
            name.name.clone(),
            FuncSig {
                generics: gen_names,
                params: sig_params,
                ret: sig_ret,
            },
        );
    }

    // --- expressions ------------------------------------------------------

    fn check_expr(&mut self, e: &Expr) -> Type {
        match e {
            Expr::Int { .. } => Type::Int,
            Expr::Float { .. } => Type::Float,
            Expr::Str { .. } => Type::Str,
            Expr::Bool { .. } => Type::Bool,
            Expr::Ident { name, span } => self.lookup(name, *span),
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
                self.check_block(body);
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
        if let Expr::Ident { name, .. } = callee {
            if let Some(sig) = self.funcs.get(name).cloned() {
                let (ps, ret) = self.instantiate(&sig);
                self.check_args_against(ps, args, span);
                return ret;
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
            TyKind::Named(name, args) => {
                if generics.iter().any(|g| g == name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("generic parameter `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Named(name.clone())
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
}

fn func_name(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Func { name, .. } => name.name.clone(),
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
        check_program(&parsed.program, HashMap::new(), HashMap::new())
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

    #[test]
    fn infers_int_from_literal() {
        let r = check_src("let x = 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn infers_float_from_promotion() {
        let r = check_src("let x = 1 + 2.5");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Float);
    }

    #[test]
    fn annotation_unifies() {
        let r = check_src("let x: float = 1 + 2.5");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Float);
    }

    #[test]
    fn annotation_mismatch_errors() {
        errors_contain("let x: str = 1 + 2", "type mismatch");
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
        let r = check_src("func add(a: int, b: int) -> int { return a + b }\nlet z = add(1, 2)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn func_return_type_inferred() {
        let r = check_src("func five() { return 5 }\nlet z = five()");
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
        let r = check_src("func id<T>(x: T) -> T { return x }\nlet a = id(1)\nlet b = id(\"s\")");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["a"], Type::Int);
        assert_eq!(r.bindings["b"], Type::Str);
    }

    #[test]
    fn generic_func_monomorphic_fail() {
        errors_contain(
            "func id<T>(x: T) -> T { x }\nlet f = id",
            "cannot use generic function `id` as a value",
        );
    }

    #[test]
    fn recursion_works() {
        let r = check_src("func fact(n: int) -> int { if n <= 1 { 1 } else { n * fact(n - 1) } }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn closure_inference() {
        let r = check_src("let f = |x: int| x + 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["f"],
            Type::Func(vec![Type::Int], Box::new(Type::Int))
        );
    }

    #[test]
    fn closure_ambiguous_errors() {
        errors_contain("let f = |x| x", "cannot infer the type of `f`");
    }

    #[test]
    fn calling_closure() {
        let r = check_src("let f = |x: int| x + 1\nlet y = f(5)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["y"], Type::Int);
    }

    #[test]
    fn match_option() {
        let r = check_src("let v = .some(1)\nlet x = match v { .some(n) => n, .none => 0 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn match_result() {
        let r = check_src(
            "let v: Result<int, str> = .ok(1)\nlet x = match v { .ok(n) => n, .err(_) => 0 }",
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn match_nonexhaustive_errors() {
        errors_contain(
            "let v = .some(1)\nmatch v { .some(n) => n }",
            "non-exhaustive",
        );
    }

    #[test]
    fn match_on_int_requires_wildcard() {
        errors_contain("match 5 { 1 => 1 }", "requires a `_` wildcard arm");
    }

    #[test]
    fn match_arm_type_mismatch_errors() {
        errors_contain(
            "let v = .some(1)\nmatch v { .some(n) => n, .none => \"x\" }",
            "type mismatch",
        );
    }

    #[test]
    fn if_let_binds() {
        let r = check_src("let v = .some(5)\nlet x = if let .some(n) = v { n } else { 0 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn try_propagates_result() {
        let r = check_src(
            "func div(a: int, b: int) -> Result<int, str> { if b == 0 { .err(\"z\") } else { .ok(a / b) } }\nfunc f(a: int, b: int) -> Result<int, str> { let q = div(a, b)?; .ok(q) }",
        );
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn try_on_option() {
        let r = check_src("func f() -> Option<int> { let x = .some(1)?; .some(x) }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn try_outside_function_errors() {
        errors_contain(".ok(1)?", "only be used inside a function");
    }

    #[test]
    fn try_on_plain_int_errors() {
        errors_contain(
            "func f() -> Result<int, str> { let x = 5?; .ok(x) }",
            "cannot use `?` on a value of type `int`",
        );
    }

    #[test]
    fn try_error_type_mismatch() {
        errors_contain(
            "func a() -> Result<int, str> { .ok(1) }\nfunc b() -> Result<int, int> { let x = a()?; .ok(x) }",
            "type mismatch",
        );
    }

    #[test]
    fn variant_type_inference() {
        let r = check_src("let a = .ok(1)\nlet b = .none");
        // `.none` alone is ambiguous → error
        let _ = r;
        errors_contain("let b = .none", "cannot infer the type of `b`");
    }

    #[test]
    fn return_outside_function_errors() {
        errors_contain("return 5", "`return` outside of a function");
    }

    #[test]
    fn if_else_type_unify() {
        let r = check_src("let x = if true { 1 } else { 2 }");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn if_else_mismatch_errors() {
        errors_contain("let x = if true { 1 } else { \"a\" }", "type mismatch");
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
        let r = check_src("let s = \"a\" + \"b\"");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["s"], Type::Str);
    }

    #[test]
    fn str_plus_int_errors() {
        errors_contain("let s = \"a\" + 1", "cannot apply `+`");
    }

    #[test]
    fn shadowing_allowed() {
        let r = check_src("let x = 1\nlet x = x + 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn duplicate_func_errors() {
        errors_contain("func f() { 1 }\nfunc f() { 2 }", "duplicate definition");
    }
}
