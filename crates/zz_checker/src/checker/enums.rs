//! User enum registration and construction (`enum Token { ... }`).
//!
//! Enums erase to qualified `Object` values (`Token.IntLit`): the type
//! system tracks `Type::Enum`, but no engine allocates anything
//! enum-specific. Construction (`Token.IntLit(1)`) is a call-shaped
//! expression intercepted in `check_call`; patterns (`.IntLit(v)`)
//! resolve against this table in the match checker.

use zz_frontend::diag::error_at;
use zz_frontend::span::Span;

use super::Checker;
use crate::type_::Type;

impl Checker {
    /// Canonical enum name, mirroring
    /// [`Checker::canonical_struct_name`]: a selectively-imported bare
    /// name resolves to its qualified form.
    pub(crate) fn canonical_enum_name(&self, name: &str) -> String {
        if let Some(qualified) = self.import_aliases.get(name) {
            if self.enums.contains_key(qualified) {
                return qualified.clone();
            }
        }
        name.to_string()
    }

    /// Check an enum construction `Enum.Variant(args)` where `enum_name`
    /// is the already-canonicalized enum and `variant` the trailing call
    /// path component. Returns the enum type on success.
    pub(crate) fn check_enum_construction(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[zz_frontend::ast::Expr],
        named: &[(String, zz_frontend::ast::Expr)],
        span: Span,
    ) -> Option<Type> {
        let sig = self.enums.get(enum_name).cloned()?;
        let Some((_, payload)) = sig.variants.iter().find(|(v, _)| v == variant) else {
            let known: Vec<&str> = sig.variants.iter().map(|(v, _)| v.as_str()).collect();
            self.errors.push(error_at(
                format!(
                    "unknown variant `{variant}` for enum `{enum_name}` (expected one of: {})",
                    known.join(", "),
                ),
                span,
            ));
            return Some(Type::Enum(enum_name.to_string()));
        };
        match payload {
            Some(pty) => {
                if !named.is_empty() {
                    self.errors.push(error_at(
                        format!(
                            "variant `{enum_name}.{variant}` takes one positional argument, not named arguments"
                        ),
                        span,
                    ));
                }
                if args.len() != 1 {
                    self.errors.push(error_at(
                        format!(
                            "variant `{enum_name}.{variant}` takes 1 argument but {} given",
                            args.len(),
                        ),
                        span,
                    ));
                    // Still check the args so nested errors surface.
                    for arg in args {
                        self.check_expr(arg);
                    }
                } else {
                    let at = self.check_expr(&args[0]);
                    if let Err(e) = self.unifier.unify(&at, pty) {
                        self.report_mismatch(e, args[0].span());
                    }
                }
            }
            None => {
                if !args.is_empty() || !named.is_empty() {
                    self.errors.push(error_at(
                        format!(
                            "variant `{enum_name}.{variant}` takes no arguments but {} given",
                            args.len() + named.len(),
                        ),
                        span,
                    ));
                }
            }
        }
        // A value construction counts as using the enum's name (marks
        // imports used when the path is qualified).
        if enum_name.contains('.') {
            self.used_names.insert(enum_name.to_string());
        }
        Some(Type::Enum(enum_name.to_string()))
    }

    /// Resolve a `.Variant` pattern against an enum scrutinee: returns
    /// the variant's payload type (`None` = unit variant, `Some` = the
    /// inner pattern must match the payload). Reports unknown variants.
    pub(crate) fn enum_variant_payload(
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
                self.errors.push(error_at(
                    format!(
                        "unknown variant `{variant}` for enum `{enum_name}` (expected one of: {})",
                        known.join(", "),
                    ),
                    span,
                ));
                None
            }
        }
    }
}
