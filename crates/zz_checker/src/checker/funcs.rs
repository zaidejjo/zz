//! Function registration and body checking.

use crate::checker::Checker;
use crate::type_::Type;
use zz_frontend::ast::{Block, Expr, Stmt};

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
                                "extern function `{}`: parameter `{}` has non-C type `{}` (allowed: int, float, bool, str, *const T, *mut T, void, ())",
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
                            "extern function `{}` has non-C return type `{}` (allowed: int, float, bool, str, *const T, *mut T, void, ())",
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
        let prev_bounds = std::mem::replace(
            &mut self.current_bounds,
            sig.bounds.iter().cloned().collect(),
        );
        let body_t = self.check_fn_body_block(body);
        self.current_ret = prev_ret;
        self.current_generics = prev_gen;
        self.current_bounds = prev_bounds;
        self.pop_scope();
        let _ = name;
        // The body's fall-through type must match the declared return
        // type. Divergent arms contribute `Never`, which vanishes from
        // joins, so a body that always returns/diverges checks against
        // anything — while any value path must produce `sig.ret`. There
        // is deliberately no `block_has_return` bypass here: skipping the
        // check whenever a `return` exists masked genuine mismatches
        // (e.g. an `else { "hi" }` fall-through in a `-> int` function)
        // and missing-return paths (`if c { return 1 }` with no else,
        // which falls through with unit).
        if let Err(e) = self.unifier.unify(&body_t, &sig.ret) {
            // Sherlock: unwrapped Result at the tail value gets a
            // value-site error with fixes instead of the generic
            // return-type-span mismatch (#245).
            let tail_span = body.stmts.last().map(|s| s.span()).unwrap_or(body.span);
            let resolved_body = self.unifier.resolve(&body_t);
            let resolved_ret = self.unifier.resolve(&sig.ret);
            if !self.report_result_return_hint(&resolved_body, &resolved_ret, tail_span) {
                self.report_mismatch(e, body.span);
            }
        }
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
