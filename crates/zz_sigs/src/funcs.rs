//! Function registration and body checking.

use zz_checker_core::{Checker, FuncSig};
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
        let (name, generics, params, ret, decorators) = match stmt {
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                decorators,
                ..
            } => (name, generics, params, ret, decorators),
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
        let readonly = decorators.iter().any(is_readonly_decorator);
        self.funcs.insert(
            full_name,
            FuncSig {
                generics: gen_names,
                bounds: gen_bounds,
                params: sig_params,
                has_default,
                ret: sig_ret,
                is_extern: false,
                readonly,
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
                        self.errors.push(error_at(
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
                    self.errors.push(error_at(
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
                    self.errors.push(error_at(
                        format!(
                            "plugin extern function `{}` returns str, which is not yet supported — return an int status code and use a getter pattern instead",
                            name.name
                        ),
                        t.span,
                    ));
                } else if !Self::is_c_abi_type(&ct) {
                    self.errors.push(error_at(
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
            self.errors.push(error_at(
                format!("duplicate definition of function `{full_name}`"),
                name.span,
            ));
        }
        // Derive and validate the C symbol: explicit override when present,
        // otherwise the ZZ name with `.` replaced by `_`. Validate it is a
        // legal C identifier and that no other `extern "C"` function declares
        // the same symbol — duplicate symbols surface here as check-time
        // errors instead of linker errors. Full symbol-existence verification
        // (does the symbol exist in a linked library) requires the C
        // toolchain and is intentionally left to link time.
        let c_sym = c_symbol
            .clone()
            .unwrap_or_else(|| full_name.replace('.', "_"));
        if !is_valid_c_ident(&c_sym) {
            self.errors.push(error_at(
                format!(
                    "extern function `{full_name}`: C symbol `{c_sym}` is not a valid C identifier"
                ),
                name.span,
            ));
        } else if let Some(prev_span) = self.extern_symbols.get(&c_sym).copied() {
            self.errors.push(error_at(
                format!(
                    "duplicate extern `extern \"C\"` symbol `{c_sym}`: declared more than once",
                ),
                name.span,
            ));
            self.errors.push(error_at(
                "previous extern symbol declaration here",
                prev_span,
            ));
        } else {
            self.extern_symbols.insert(c_sym, name.span);
        }

        self.funcs.insert(
            full_name,
            FuncSig {
                generics: Vec::new(),
                bounds: Vec::new(),
                params: sig_params,
                has_default: vec![false; params.len()],
                ret: sig_ret,
                is_extern: true,
                readonly: false,
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

/// Returns `true` if `d` is `@readonly` (path `["readonly"]`, no args).
///
/// `@readonly` marks a function as not retaining its arguments — arguments
/// passed to a readonly call do not escape through the callee, so they can
/// stay arena-allocated instead of being forced to ARC heap.
pub(crate) fn is_readonly_decorator(d: &zz_span_ast::ast::Decorator) -> bool {
    d.path == ["readonly"] && d.args.is_empty() && d.named.is_empty()
}

/// True when `name` is usable as a C identifier (an extern symbol): starts
/// with an alphabetic or underscore, rest alphanumeric or underscore. Mirrors
/// [`c_ident_valid`] in `zz_lower` so the checker can validate declared
/// extern symbols without pulling in the lowering crate.
pub(crate) fn is_valid_c_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
