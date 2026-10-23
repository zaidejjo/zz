//! Function registration and body checking.

use crate::checker::Checker;
use crate::type_::Type;
use zz_frontend::ast::{Block, Expr, Stmt};
use zz_frontend::diag::{error_at, FixIt};
use zz_frontend::span::Span;

impl Checker {
    pub(crate) fn collect_func(&mut self, stmt: &Stmt) {
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
        let gen_names: Vec<String> = generics.iter().map(|g| g.name.name.clone()).collect();
        let gen_bounds: Vec<(String, Vec<zz_frontend::ast::TraitBound>)> = generics
            .iter()
            .map(|g| (g.name.name.clone(), g.bounds.clone()))
            .collect();
        let sig_params: Vec<(String, Type)> = params
            .iter()
            .map(|p| {
                let ty = match &p.ty {
                    Some(t) => self.ast_to_type(t, &gen_names),
                    None => self.unifier.fresh_var(),
                };
                (p.name.name.clone(), ty)
            })
            .collect();
        let has_default: Vec<bool> = params.iter().map(|p| p.default.is_some()).collect();
        let sig_ret = match ret {
            Some(t) => self.ast_to_type(t, &gen_names),
            None => self.unifier.fresh_var(),
        };
        let full_name = name.join(".");
        self.funcs.insert(
            full_name,
            crate::checker::FuncSig {
                generics: gen_names,
                bounds: gen_bounds,
                params: sig_params,
                has_default,
                ret: sig_ret,
                is_extern: false,
                extern_c_symbol: None,
            },
        );
    }

    /// Register one `extern "C"` signature. No body is checked; parameter and
    /// return types must be C-compatible (int/float/bool/pointer/void/unit).
    pub(crate) fn collect_extern(
        &mut self,
        name: &zz_frontend::ast::Ident,
        params: &[zz_frontend::ast::Param],
        ret: &Option<zz_frontend::ast::Ty>,
        c_symbol: Option<String>,
    ) {
        let mut sig_params = Vec::with_capacity(params.len());
        for p in params {
            let ty = match &p.ty {
                Some(t) => {
                    let ct = self.ast_to_type(t, &[]);
                    if !Self::is_c_abi_type(&ct) {
                        self.errors.push(zz_frontend::diag::error_at(
                            format!(
                                "extern function `{}`: parameter `{}` has non-C type `{}` (allowed: int, float, bool, str, bytes, *const T, *mut T, void, ())",
                                name.name, p.name.name, ct
                            ),
                            p.span,
                        ));
                    }
                    ct
                }
                None => {
                    self.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "extern function `{}`: parameter `{}` needs an explicit C type",
                            name.name, p.name.name
                        ),
                        p.span,
                    ));
                    Type::Error
                }
            };
            sig_params.push((p.name.name.clone(), ty));
        }
        let sig_ret = match ret {
            Some(t) => {
                let ct = self.ast_to_type(t, &[]);
                if matches!(ct, Type::Str) {
                    self.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "plugin extern function `{}` returns str, which is not yet supported — return an int status code and use a getter pattern instead",
                            name.name
                        ),
                        t.span,
                    ));
                } else if !Self::is_c_abi_type(&ct) {
                    self.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "extern function `{}` has non-C return type `{}` (allowed: int, float, bool, str, bytes, *const T, *mut T, void, ())",
                            name.name, ct
                        ),
                        t.span,
                    ));
                }
                ct
            }
            None => Type::Unit,
        };
        let full_name = name.name.clone();
        if self.funcs.contains_key(&full_name) {
            self.errors.push(zz_frontend::diag::error_at(
                format!("duplicate definition of function `{full_name}`"),
                name.span,
            ));
        }
        self.funcs.insert(
            full_name,
            crate::checker::FuncSig {
                generics: Vec::new(),
                bounds: Vec::new(),
                params: sig_params,
                has_default: vec![false; params.len()],
                ret: sig_ret,
                is_extern: true,
                extern_c_symbol: c_symbol,
            },
        );
    }

    fn is_c_abi_type(ty: &Type) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Bool | Type::Unit | Type::Void | Type::Error => true,
            Type::Str => true,
            // Borrowed byte views (#234).
            Type::Bytes => true,
            Type::Ptr { inner, .. } => Self::is_c_abi_scalar(inner),
            _ => false,
        }
    }

    fn is_c_abi_scalar(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Int | Type::Float | Type::Bool | Type::Unit | Type::Void
        )
    }

    pub(crate) fn check_func_body(&mut self, stmt: &Stmt, sig: &crate::checker::FuncSig) {
        let (name, params, body) = match stmt {
            Stmt::Func {
                name, params, body, ..
            } => (name, params, body),
            _ => unreachable!(),
        };
        self.push_scope();
        for (i, (pname, pty)) in sig.params.iter().enumerate() {
            // Bind with the source span when available (powers precise
            // diagnostics, e.g. the module-namespace shadow lint); the
            // signature table itself carries no spans.
            match params.get(i) {
                Some(p) => self.define_var_at(&p.name.name, pty.clone(), p.name.span, false),
                None => self.define(pname, pty.clone()),
            }
        }
        let prev_ret = self.current_ret.replace(sig.ret.clone());
        let prev_gen = std::mem::replace(&mut self.current_generics, sig.generics.clone());
        let prev_bounds = std::mem::replace(
            &mut self.current_bounds,
            sig.bounds.iter().cloned().collect(),
        );
        let body_t = self.check_named_fn_body(body);
        self.current_ret = prev_ret;
        self.current_generics = prev_gen;
        self.current_bounds = prev_bounds;
        self.pop_scope();
        // Dead-mutation lint (#278): value-typed params mutated without
        // ever being observed. Runs on the AST after the body checks so
        // it sees the final program even when type errors exist.
        super::value_lint::lint_dead_param_mutation(&sig.params, body, &mut self.errors);
        let _ = name;
        // A named function yields a value only through `return`, so its
        // fall-through type is `unit` unless every path diverges (`Never`,
        // e.g. ending in `if c { return 1 } else { return 2 }`). The body's
        // tail expression type is irrelevant here: it was discarded.
        //
        // A non-unit signature plus a fall-through is the missing-`return`
        // error — with an insertion fixit so `zz fix` can migrate the
        // corpus. An unresolved return (no annotation) is not "non-unit":
        // unifying it with `unit` is how the signature becomes `unit`.
        let resolved_body = self.unifier.resolve(&body_t);
        if matches!(resolved_body, Type::Error) {
            // The body already reported a type error: don't cascade a
            // return-type complaint on top of it.
            let _ = self.unifier.unify(&body_t, &sig.ret);
            return;
        }
        let resolved_ret = self.unifier.resolve(&sig.ret);
        let ret_takes_value = !matches!(
            resolved_ret,
            Type::Unit | Type::Void | Type::Error | Type::Var(_)
        );
        let diverges = matches!(resolved_body, Type::Never);
        if ret_takes_value && !diverges {
            self.report_missing_return(body, &resolved_ret);
            return;
        }
        let fallthrough = if diverges { Type::Never } else { Type::Unit };
        if let Err(e) = self.unifier.unify(&fallthrough, &sig.ret) {
            self.report_mismatch(e, body.span);
        }
    }

    /// `func f() -> int { 1 }`: the tail expression is discarded now, so
    /// point at it and offer the insertion `zz fix` applies. Tails that
    /// cannot take a prefix (`return`, declarations, empty bodies) get the
    /// message only.
    fn report_missing_return(&mut self, body: &Block, ret_ty: &Type) {
        let last = body.stmts.last();
        let span = last.map(|s| s.span()).unwrap_or(body.span);
        // A tail that already yields `unit` (e.g. `println(...)`) cannot be
        // fixed by prefixing `return`: the value would still be unit.
        let unit_tail = match (last, &self.last_block_tail) {
            (Some(stmt), Some((tail_span, ty))) if *tail_span == stmt.span() => {
                matches!(self.unifier.resolve(ty), Type::Unit | Type::Void)
            }
            _ => false,
        };
        let value_tail = match last {
            // `return if c { 1 }` would still fall through: an `if` without
            // an `else` yields unit, so an insertion fixit would trade one
            // error for another.
            Some(Stmt::Expr(Expr::If { els: None, .. })) => None,
            Some(Stmt::Expr(e)) if !unit_tail => {
                let s = e.span();
                Some(Span::new(s.start, s.start))
            }
            _ => None,
        };
        let mut diag = error_at(
            format!("function returns `{ret_ty}` but its body can fall through; add `return`"),
            span,
        )
        .with_note("named functions return a value only through `return`; the last expression is discarded");
        if let Some(insert_at) = value_tail {
            diag = diag.with_fixit(FixIt::safe(insert_at, "return ", "add `return`"));
        }
        self.errors.push(diag);
    }

    /// Recursively check whether a block contains a `return` statement.
    pub(crate) fn block_has_return(block: &Block) -> bool {
        block.stmts.iter().any(Self::stmt_has_return)
    }

    fn stmt_has_return(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Return { .. } => true,
            Stmt::For { body, .. } => Self::block_has_return(body),
            Stmt::Expr(Expr::While { body, .. }) => Self::block_has_return(body),
            Stmt::Expr(Expr::If { then, els, .. }) => {
                Self::block_has_return(then)
                    || els.as_ref().is_some_and(|e| Self::expr_has_return(e))
            }
            _ => false,
        }
    }

    fn expr_has_return(expr: &Expr) -> bool {
        match expr {
            Expr::Block(b) => Self::block_has_return(b),
            Expr::If { then, els, .. } => {
                Self::block_has_return(then)
                    || els.as_ref().is_some_and(|e| Self::expr_has_return(e))
            }
            _ => false,
        }
    }
}
