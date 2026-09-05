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
        let gen_names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
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
                params: sig_params,
                has_default,
                ret: sig_ret,
            },
        );
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
        let body_t = self.check_block(body);
        self.current_ret = prev_ret;
        self.current_generics = prev_gen;
        self.pop_scope();
        let _ = name;
        // If the body contains any `return` statement, the body's "natural"
        // type (Unit for loops, etc.) doesn't reflect the actual return path.
        // The `return` statements are already validated against current_ret,
        // so we only check the body type when there are no early returns.
        if !Self::block_has_return(body) {
            if let Err(e) = self.unifier.unify(&body_t, &sig.ret) {
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
