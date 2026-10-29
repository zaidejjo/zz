//! User enum registration and construction (`enum Token { ... }`).
//!
//! Enums erase to qualified `Object` values (`Token.IntLit`): the type
//! system tracks `Type::Enum`, but no engine allocates anything
//! enum-specific. Construction (`Token.IntLit(1)`) is a call-shaped
//! expression intercepted in `check_call`; patterns (`.IntLit(v)`)
//! resolve against this table in the match checker. Generic enums
//! (`enum Box<T>`) instantiate like generic structs: fresh variables
//! per parameter, filled by unifying the payload, arrears defaulted
//! like `Option`'s.

use std::collections::HashMap;

use zz_diag_core::error_at;
use zz_span_ast::span::Span;
use zz_suggest::suggest_all;

use super::Checker;
use zz_types::Type;

pub trait EnumOps {
    /// Fresh inference variables for an enum's parameters, with the
    /// substitution map (`T` → `?0`). Mirrors generic struct
    /// instantiation: the payload unifies against the substituted type,
    /// so `Box.V(1)` infers `Box<int>`.
    fn fresh_enum_vars(&mut self, enum_name: &str) -> (Vec<Type>, HashMap<String, Type>);

    /// Substitute an enum's parameters in a payload type using the
    /// scrutinee's arguments (`Box<int>` + `Named("T")` → `int`).
    /// Unknown-length args (shouldn't happen post-check) leave `Named`
    /// intact rather than mis-substituting.
    fn subst_enum_payload(&self, enum_name: &str, args: &[Type], payload: &Type) -> Type;

    /// Resolve a `.Variant` pattern against an enum scrutinee: returns
    /// the variant's payload type (`None` = unit variant, `Some` = the
    /// inner pattern must match the payload). Reports unknown variants.
    /// Generic parameters are already substituted by the caller.
    fn enum_variant_payload(
        &mut self,
        enum_name: &str,
        variant: &str,
        span: Span,
    ) -> Option<Option<Type>>;
}

impl EnumOps for Checker {
    /// Fresh inference variables for an enum's parameters, with the
    /// substitution map (`T` → `?0`). Mirrors generic struct
    /// instantiation: the payload unifies against the substituted type,
    /// so `Box.V(1)` infers `Box<int>`.
    fn fresh_enum_vars(&mut self, enum_name: &str) -> (Vec<Type>, HashMap<String, Type>) {
        let generics = self
            .enums
            .get(enum_name)
            .map(|s| s.generics.clone())
            .unwrap_or_default();
        let vars: Vec<Type> = generics.iter().map(|_| self.unifier.fresh_var()).collect();
        let map: HashMap<String, Type> = generics.into_iter().zip(vars.iter().cloned()).collect();
        (vars, map)
    }

    /// Substitute an enum's parameters in a payload type using the
    /// scrutinee's arguments (`Box<int>` + `Named("T")` → `int`).
    /// Unknown-length args (shouldn't happen post-check) leave `Named`
    /// intact rather than mis-substituting.
    fn subst_enum_payload(&self, enum_name: &str, args: &[Type], payload: &Type) -> Type {
        let generics = self
            .enums
            .get(enum_name)
            .map(|s| s.generics.clone())
            .unwrap_or_default();
        if generics.len() != args.len() {
            return payload.clone();
        }
        let map: HashMap<String, Type> = generics.into_iter().zip(args.iter().cloned()).collect();
        super::inference::subst(payload, &map)
    }

    /// Resolve a `.Variant` pattern against an enum scrutinee: returns
    /// the variant's payload type (`None` = unit variant, `Some` = the
    /// inner pattern must match the payload). Reports unknown variants.
    /// Generic parameters are already substituted by the caller.
    fn enum_variant_payload(
        &mut self,
        enum_name: &str,
        variant: &str,
        span: Span,
    ) -> Option<Option<Type>> {
        let sig = self.enums.get(enum_name).cloned()?;
        match sig.variants.iter().find(|(v, _)| v == variant) {
            Some((_, payload)) => Some(payload.clone()),
            None => {
                let known: Vec<&str> = sig.variants.iter().map(|(v, _)| v.as_str()).collect();
                let mut diag = error_at(
                    format!(
                        "unknown variant `{variant}` for enum `{enum_name}` (expected one of: {})",
                        known.join(", "),
                    ),
                    span,
                );
                // Sherlock: suggest the closest known variant (note only, no
                // fixit — the payload binding shape may differ).
                if let Some((suggestion, _)) = suggest_all(variant, &known).first() {
                    diag = diag.with_note(format!("did you mean `.{suggestion}`?"));
                }
                self.errors.push(diag);
                None
            }
        }
    }
}
