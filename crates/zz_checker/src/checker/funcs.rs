//! Function registration and body checking.

use crate::checker::Checker;
use zz_diag_core::error_at;
use zz_infer::InferOps;
use zz_span_ast::ast::Stmt;
use zz_types::Type;

pub trait FuncOps {
    fn collect_func(&mut self, stmt: &Stmt);

    /// Register one `extern "C"` signature. No body is checked; parameter and
    /// return types must be C-compatible (int/float/bool/pointer/void/unit).
    fn collect_extern(
        &mut self,
        name: &zz_span_ast::ast::Ident,
        params: &[zz_span_ast::ast::Param],
        ret: &Option<zz_span_ast::ast::Ty>,
        c_symbol: Option<String>,
    );

    fn is_c_abi_type(ty: &Type) -> bool;

    fn is_c_abi_scalar(ty: &Type) -> bool;
}

impl FuncOps for Checker {
    fn collect_func(&mut self, stmt: &Stmt) {
        let (name, generics, params, ret) = match stmt {
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                ..
            } => (name, generics, params, ret),
            _ => {
                self.errors.push(error_at(
                    "internal: collect_func called with a non-Func statement",
                    stmt.span(),
                ));
                return;
            }
        };
        let gen_names: Vec<String> = generics.iter().map(|g| g.name.name.clone()).collect();
        let gen_bounds: Vec<(String, Vec<zz_span_ast::ast::TraitBound>)> = generics
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
    fn collect_extern(
        &mut self,
        name: &zz_span_ast::ast::Ident,
        params: &[zz_span_ast::ast::Param],
        ret: &Option<zz_span_ast::ast::Ty>,
        c_symbol: Option<String>,
    ) {
        let mut sig_params = Vec::with_capacity(params.len());
        for p in params {
            let ty = match &p.ty {
                Some(t) => {
                    let ct = self.ast_to_type(t, &[]);
                    if !Self::is_c_abi_type(&ct) {
                        self.errors.push(zz_diag_core::error_at(
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
                    self.errors.push(zz_diag_core::error_at(
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
                    self.errors.push(zz_diag_core::error_at(
                        format!(
                            "plugin extern function `{}` returns str, which is not yet supported — return an int status code and use a getter pattern instead",
                            name.name
                        ),
                        t.span,
                    ));
                } else if !Self::is_c_abi_type(&ct) {
                    self.errors.push(zz_diag_core::error_at(
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
            self.errors.push(zz_diag_core::error_at(
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
}
