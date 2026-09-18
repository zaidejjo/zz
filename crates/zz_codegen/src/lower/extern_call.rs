//! Direct C calls for plugin `extern "C"` functions.
//!
//! Plugin manifests (`.zzi` files) contribute [`zz_checker::FuncSig`] entries
//! with `is_extern = true`, keyed by their ZZ-visible (possibly dotted) name
//! (e.g. `zimg.resize`). Each signature resolves to a C symbol via
//! [`zz_checker::FuncSig::c_symbol`]: the explicit `= "..."` override when
//! present, otherwise the ZZ name with `.` replaced by `_`.
//!
//! This module:
//! - [`extern_c_sig`] maps a checker signature to C declaration types.
//! - [`Lowerer::emit_extern_call`] lowers a call to a direct C invocation,
//!   unboxing scalar arguments and boxing the return value.
//!
//! Type-mapping contract (matches the lowerer's scalar conventions):
//! - `Int` ⇄ `int64_t` (pointer-sized, so opaque `int` handles round-trip)
//! - `Float` ⇄ `double`
//! - `Bool` ⇄ `bool`
//! - `Str` → `const char *` via `zz_str_cptr` (borrowed for the call only;
//!   the C side must not retain it)
//! - `*mut void` ⇄ `void *`, `*const void` ⇄ `const void *` (opaque only;
//!   handles cross as `(void *)(intptr_t)<int>`)
//! - `Void`/`Unit` returns lower to `(call, zz_unit())`
//! - Composite types are unsupported and fall back to `zz_unit()`
//!   (same silent-unit convention as natives without a C runtime impl).

use zz_frontend::ast::Expr;

use super::context::{scalar_operand_c, scalar_operand_type};
use super::{Lowerer, NameCtx};

/// True when `name` is usable as a C identifier (an extern symbol).
pub(super) fn c_ident_valid(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Map one checker type to its C declaration type for extern calls.
/// Returns `None` for types that cannot cross the boundary.
fn c_abi_type(ty: &zz_checker::Type) -> Option<&'static str> {
    match ty {
        zz_checker::Type::Int => Some("int64_t"),
        zz_checker::Type::Float => Some("double"),
        zz_checker::Type::Bool => Some("bool"),
        zz_checker::Type::Str => Some("const char *"),
        zz_checker::Type::Void | zz_checker::Type::Unit => Some("void"),
        zz_checker::Type::Ptr { mutable, inner } => match inner.as_ref() {
            zz_checker::Type::Void => {
                if *mutable {
                    Some("void *")
                } else {
                    Some("const void *")
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// Map an extern signature to C declaration types: `(return, params)`.
/// Returns `None` when any type cannot cross the boundary; callers fall
/// back to `zz_unit()` in that case.
pub(super) fn extern_c_sig(sig: &zz_checker::FuncSig) -> Option<(String, Vec<String>)> {
    // `Str` returns need length-aware boxing the boundary does not
    // provide: reject them here (calls degrade to `zz_unit()`).
    if matches!(sig.ret, zz_checker::Type::Str) {
        return None;
    }
    let ret = c_abi_type(&sig.ret)?.to_string();
    let mut params = Vec::with_capacity(sig.params.len());
    for (_, ty) in &sig.params {
        // `void` is not a valid parameter type.
        match ty {
            zz_checker::Type::Void | zz_checker::Type::Unit => return None,
            _ => {}
        }
        params.push(c_abi_type(ty)?.to_string());
    }
    Some((ret, params))
}

/// Emit the declaration line for one reachable extern function, e.g.
/// `int64_t zimg_resize(int64_t, double);` for ZZ `zimg.resize`.
/// Returns `None` when the symbol or signature cannot cross the boundary
/// (caller skips the declaration; calls to it fall back to `zz_unit()`).
pub(super) fn extern_decl(zz_name: &str, sig: &zz_checker::FuncSig) -> Option<String> {
    let c_sym = sig.c_symbol(zz_name);
    if !c_ident_valid(&c_sym) {
        return None;
    }
    let (ret, params) = extern_c_sig(sig)?;
    let param_list = if params.is_empty() {
        "(void)".to_string()
    } else {
        format!("({})", params.join(", "))
    };
    Some(format!("{ret} {c_sym}{param_list};\n"))
}

impl Lowerer {
    /// Lower a call to an `is_extern` plugin function as a direct C call.
    ///
    /// Arguments are unboxed to their C scalar forms (locals stay raw,
    /// boxed values field-extract), pointer arguments cross as
    /// `(void *)(intptr_t)<int>`, and the return value is boxed back into
    /// a `zz_value`. Anything unsupported degrades to `zz_unit()`.
    pub(super) fn emit_extern_call(
        &self,
        cname: &str,
        sig: &zz_checker::FuncSig,
        args: &[&Expr],
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        // Resolve the C symbol from the ZZ-visible (possibly dotted) name.
        let c_sym = sig.c_symbol(cname);
        if !c_ident_valid(&c_sym) {
            return "zz_unit()".to_string();
        }
        if args.len() != sig.params.len() {
            return "zz_unit()".to_string();
        }
        // Argument values are always consumed: keep void-context-sensitive
        // shapes (e.g. vec.push → vec.append) out of argument position,
        // mirroring the generic call path.
        let saved_void = *self.void_context.borrow();
        *self.void_context.borrow_mut() = false;
        let mut parts: Vec<String> = Vec::with_capacity(args.len());
        for (arg, (_, pty)) in args.iter().zip(sig.params.iter()) {
            let emitted = self.emit_expr(arg, names, out);
            let Some(conv) = self.extern_arg_conv(arg, &emitted, names, pty) else {
                *self.void_context.borrow_mut() = saved_void;
                return "zz_unit()".to_string();
            };
            parts.push(conv);
        }
        *self.void_context.borrow_mut() = saved_void;

        let call = format!("{c_sym}({})", parts.join(", "));
        match &sig.ret {
            zz_checker::Type::Int => format!("zz_int({call})"),
            zz_checker::Type::Float => format!("zz_float({call})"),
            zz_checker::Type::Bool => format!("zz_bool({call})"),
            // Pointers have no boxed form: handles cross back as ints.
            zz_checker::Type::Ptr { .. } => format!("zz_int((int64_t)(intptr_t)({call}))"),
            zz_checker::Type::Void | zz_checker::Type::Unit => format!("({call}, zz_unit())"),
            _ => "zz_unit()".to_string(),
        }
    }

    /// Convert one already-emitted argument to its C parameter type.
    /// Returns `None` when the parameter type cannot cross the boundary.
    fn extern_arg_conv(
        &self,
        arg: &Expr,
        emitted: &str,
        names: &NameCtx,
        pty: &zz_checker::Type,
    ) -> Option<String> {
        match pty {
            zz_checker::Type::Int => Some(self.extern_unbox(arg, emitted, names, "int64_t")),
            zz_checker::Type::Float => Some(self.extern_unbox(arg, emitted, names, "double")),
            zz_checker::Type::Bool => Some(self.extern_unbox(arg, emitted, names, "bool")),
            // Strings cross borrowed for the call only, reusing the
            // native convention (`zz_str_cptr(v.s)`); the C side must
            // not retain the pointer. Non-boxed shapes (literals lower
            // as `zz_str_static(...)`) and boxed values both expose `.s`.
            zz_checker::Type::Str => Some(format!("zz_str_cptr(({emitted}).s)")),
            zz_checker::Type::Ptr { mutable, inner } => {
                if !matches!(inner.as_ref(), zz_checker::Type::Void) {
                    return None;
                }
                let raw = self.extern_unbox(arg, emitted, names, "int64_t");
                if *mutable {
                    Some(format!("(void *)(intptr_t)({raw})"))
                } else {
                    Some(format!("(const void *)(intptr_t)({raw})"))
                }
            }
            _ => None,
        }
    }

    /// Produce a raw C scalar expression of type `want`
    /// (`"int64_t"` / `"double"` / `"bool"`) from an emitted argument.
    ///
    /// Scalar-shaped expressions (literals, scalar locals, folded
    /// arithmetic) stay raw with a cast when the type differs; everything
    /// else is a boxed `zz_value` (guaranteed by the type checker, since
    /// the argument's ZZ type is scalar) and field-extracts.
    fn extern_unbox(&self, arg: &Expr, emitted: &str, names: &NameCtx, want: &str) -> String {
        if let Some(t) = self.extern_raw_type(arg, names) {
            let raw = self
                .extern_raw_expr(arg, emitted, names)
                .unwrap_or_else(|| emitted.to_string());
            if t == want {
                return raw;
            }
            return format!("({want})({raw})");
        }
        let field = match want {
            "double" => 'f',
            "bool" => 'b',
            _ => 'i',
        };
        format!("({emitted}).{field}")
    }

    /// Classify an argument's emitted form as a raw C scalar type, or
    /// `None` when it is a boxed `zz_value`.
    fn extern_raw_type(&self, arg: &Expr, names: &NameCtx) -> Option<&'static str> {
        match arg {
            Expr::Ident { name, .. } => match names.lookup_type(name) {
                Some("int64_t") => Some("int64_t"),
                Some("double") => Some("double"),
                Some("bool") => Some("bool"),
                _ => None,
            },
            Expr::Path { parts, .. } => {
                // Struct field access (p.x) is not a scalar variable even
                // when the base name resolves; mirror scalar_operand_type.
                if parts.len() == 2 {
                    if let Some(base_ty) = names.lookup_type(&parts[0]) {
                        if base_ty.starts_with("zz_struct_") {
                            return self.struct_field_scalar(base_ty, &parts[1]);
                        }
                    }
                }
                match names.lookup_type(&parts.join(".")) {
                    Some("int64_t") => Some("int64_t"),
                    Some("double") => Some("double"),
                    Some("bool") => Some("bool"),
                    _ => None,
                }
            }
            Expr::Field { obj, name, .. } => {
                // Unboxed-struct field access lowers to a raw scalar.
                let base_ty = match obj.as_ref() {
                    Expr::Ident { name: b, .. } => names.lookup_type(b)?,
                    _ => return None,
                };
                self.struct_field_scalar(base_ty, name)
            }
            _ => scalar_operand_type(arg, names),
        }
    }

    /// Raw C expression for an argument already classified scalar by
    /// [`Lowerer::extern_raw_type`]. Returns `None` to fall back to the
    /// `emit_expr` output (already raw in that case).
    fn extern_raw_expr(&self, arg: &Expr, emitted: &str, names: &NameCtx) -> Option<String> {
        // Struct field access has no scalar_operand_c shape; the
        // emit_expr output is already the raw `base.field` expression.
        if matches!(arg, Expr::Field { .. }) {
            return None;
        }
        if let Expr::Path { parts, .. } = arg {
            if parts.len() == 2 {
                if let Some(base_ty) = names.lookup_type(&parts[0]) {
                    if base_ty.starts_with("zz_struct_") {
                        return None;
                    }
                }
            }
        }
        // Careful: the caller's `emitted` was produced by emit_expr, which
        // may add retains/clones around idents. Prefer the side-effect-free
        // raw shape when available.
        match scalar_operand_c(arg, names) {
            Some(raw) => Some(raw),
            None => Some(emitted.to_string()),
        }
    }

    /// Scalar C type of an unboxed-struct field, or `None` for boxed or
    /// unknown fields.
    fn struct_field_scalar(&self, base_c_type: &str, field: &str) -> Option<&'static str> {
        match self.field_type_from_struct(base_c_type, field) {
            Some("int64_t") => Some("int64_t"),
            Some("double") => Some("double"),
            Some("bool") => Some("bool"),
            _ => None,
        }
    }
    /// Emit `extern` declarations for every reachable `is_extern` plugin
    /// function. Empty when none, keeping generated C for existing
    /// programs byte-identical.
    pub(super) fn extern_prelude(&self) -> String {
        let mut names: Vec<&String> = self
            .reachable_funcs
            .iter()
            .filter(|f| {
                self.tp
                    .funcs
                    .get(*f)
                    .map(|sig| sig.is_extern)
                    .unwrap_or(false)
            })
            .collect();
        names.sort();
        let mut out = String::new();
        for name in names {
            if let Some(sig) = self.tp.funcs.get(name) {
                if let Some(decl) = extern_decl(name, sig) {
                    out.push_str(&decl);
                }
            }
        }
        if out.is_empty() {
            return String::new();
        }
        // `intptr_t` is used for int↔pointer conversions below.
        format!("#include <stdint.h>\n{out}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(params: Vec<(&str, zz_checker::Type)>, ret: zz_checker::Type) -> zz_checker::FuncSig {
        zz_checker::FuncSig {
            generics: Vec::new(),
            bounds: Vec::new(),
            params: params
                .into_iter()
                .map(|(n, t)| (n.to_string(), t))
                .collect(),
            has_default: Vec::new(),
            ret,
            is_extern: true,
            extern_c_symbol: None,
        }
    }

    fn void_ptr(mutable: bool) -> zz_checker::Type {
        zz_checker::Type::Ptr {
            mutable,
            inner: Box::new(zz_checker::Type::Void),
        }
    }

    #[test]
    fn scalar_sig_maps_to_c_types() {
        let s = sig(
            vec![
                ("img", zz_checker::Type::Int),
                ("scale", zz_checker::Type::Float),
            ],
            zz_checker::Type::Int,
        );
        let (ret, params) = extern_c_sig(&s).unwrap();
        assert_eq!(ret, "int64_t");
        assert_eq!(params, vec!["int64_t", "double"]);
    }

    #[test]
    fn opaque_pointers_map_to_void_star() {
        let s = sig(
            vec![("img", void_ptr(true)), ("path", void_ptr(false))],
            zz_checker::Type::Void,
        );
        let (ret, params) = extern_c_sig(&s).unwrap();
        assert_eq!(ret, "void");
        assert_eq!(params, vec!["void *", "const void *"]);
    }

    #[test]
    fn str_params_map_to_const_char_star() {
        let s = sig(vec![("p", zz_checker::Type::Str)], zz_checker::Type::Int);
        let (ret, params) = extern_c_sig(&s).unwrap();
        assert_eq!(ret, "int64_t");
        assert_eq!(params, vec!["const char *"]);
    }

    #[test]
    fn str_returns_are_unsupported() {
        let s = sig(vec![("p", zz_checker::Type::Int)], zz_checker::Type::Str);
        assert!(extern_c_sig(&s).is_none());
    }

    #[test]
    fn composites_are_unsupported() {
        let s = sig(
            vec![("p", zz_checker::Type::Int)],
            zz_checker::Type::Array(Box::new(zz_checker::Type::Int)),
        );
        assert!(extern_c_sig(&s).is_none());
    }

    #[test]
    fn void_is_not_a_valid_param_type() {
        let s = sig(vec![("p", zz_checker::Type::Void)], zz_checker::Type::Void);
        assert!(extern_c_sig(&s).is_none());
    }

    #[test]
    fn decl_emits_symbol_with_param_types() {
        let s = sig(vec![("img", zz_checker::Type::Int)], zz_checker::Type::Int);
        assert_eq!(
            extern_decl("zimg_width", &s).unwrap(),
            "int64_t zimg_width(int64_t);\n"
        );
    }

    #[test]
    fn decl_empty_params_become_void() {
        let s = sig(vec![], zz_checker::Type::Int);
        assert_eq!(
            extern_decl("zimg_init", &s).unwrap(),
            "int64_t zimg_init(void);\n"
        );
    }

    #[test]
    fn decl_rejects_non_identifiers() {
        let s = sig(vec![], zz_checker::Type::Int);
        // Dots are fine on the ZZ side — the derived C symbol is checked.
        assert_eq!(
            extern_decl("zimg.width", &s).unwrap(),
            "int64_t zimg_width(void);\n"
        );
        assert!(extern_decl("9lives", &s).is_none());
        assert!(c_ident_valid("zimg_get_result"));
        assert!(!c_ident_valid(""));
    }

    #[test]
    fn decl_honors_explicit_c_symbol() {
        let mut s = sig(vec![("a", zz_checker::Type::Int)], zz_checker::Type::Int);
        s.extern_c_symbol = Some("custom_impl".to_string());
        assert_eq!(
            extern_decl("toy.add", &s).unwrap(),
            "int64_t custom_impl(int64_t);\n"
        );
        assert_eq!(s.c_symbol("toy.add"), "custom_impl");
    }

    #[test]
    fn c_symbol_derives_from_dotted_name() {
        let s = sig(vec![], zz_checker::Type::Int);
        assert_eq!(s.c_symbol("zimg.resize"), "zimg_resize");
        assert_eq!(s.c_symbol("add"), "add");
    }
}
