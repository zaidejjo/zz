//! Type alias registration and expansion (`type Tokens = [Token]`).
//!
//! Aliases erase at check time: every use resolves to the target type,
//! so the runtime, VM, and native codegen never see alias names.
//! Collection is two-phase (raw targets recorded in pass 1a, converted
//! after all structs are known) so targets may reference any struct
//! regardless of order. Nested aliases expand recursively with cycle
//! detection (`type A = B`, `type B = A` reports instead of hanging).

use std::collections::HashMap;

use zz_frontend::ast::Ty;
use zz_frontend::diag::error_at;
use zz_frontend::span::Span;

use super::inference::subst;
use super::{AliasSig, Checker};
use crate::type_::Type;

impl Checker {
    /// Convert one raw alias target to a resolved [`AliasSig`].
    /// Idempotent: already-converted names return immediately (also
    /// breaks re-entrant conversion during cycle reporting).
    pub(crate) fn convert_alias(&mut self, name: &str) {
        if self.aliases.contains_key(name) {
            return;
        }
        if self.alias_expanding.iter().any(|n| n == name) {
            let span = self
                .alias_asts
                .get(name)
                .map(|(_, t)| t.span)
                .unwrap_or(Span::new(0, 0));
            self.errors.push(error_at(
                format!(
                    "cyclic type alias `{}`: {}",
                    name,
                    self.alias_expanding.join(" -> ")
                ),
                span,
            ));
            // Placeholder so the use site resolves instead of cascading
            // a second "unknown type" error.
            self.aliases.entry(name.to_string()).or_insert(AliasSig {
                generics: Vec::new(),
                target: Type::Unit,
            });
            return;
        }
        let Some((generics, target)) = self.alias_asts.get(name).cloned() else {
            return;
        };
        self.alias_expanding.push(name.to_string());
        let ty = self.ast_to_type_inner(&target, &generics);
        self.alias_expanding.pop();
        // A cycle during conversion leaves no entry: record the
        // (possibly partial) type anyway so uses don't cascade
        // "unknown type" on top of the cycle error.
        self.aliases.entry(name.to_string()).or_insert(AliasSig {
            generics,
            target: ty,
        });
    }

    /// Canonical alias name, mirroring [`Checker::canonical_struct_name`]:
    /// a selectively-imported bare name resolves to its qualified form.
    pub(crate) fn canonical_alias_name(&self, name: &str) -> String {
        if let Some(qualified) = self.import_aliases.get(name) {
            if self.aliases.contains_key(qualified) {
                return qualified.clone();
            }
        }
        name.to_string()
    }

    /// Expand an alias use `Name[args]` to its resolved [`Type`].
    /// Arity is checked like generic structs; mismatches report and
    /// resolve with the provided prefix (missing parameters stay
    /// `Named`, which surfaces as a generic — never a silent hole).
    pub(crate) fn expand_alias(
        &mut self,
        name: &str,
        args: &[Ty],
        outer_generics: &[String],
        span: Span,
    ) -> Type {
        // Lazily convert: seeded aliases (cross-module) arrive converted,
        // same-program aliases convert in pass 1a — but a use inside an
        // alias target converts on demand through `convert_alias`.
        self.convert_alias(name);
        let Some(sig) = self.aliases.get(name).cloned() else {
            self.errors
                .push(error_at(format!("unknown type `{name}`"), span));
            return Type::Unit;
        };
        if args.len() != sig.generics.len() {
            if sig.generics.is_empty() {
                self.errors.push(error_at(
                    format!(
                        "type alias `{name}` takes no type arguments but {} given",
                        args.len(),
                    ),
                    span,
                ));
            } else {
                self.errors.push(error_at(
                    format!(
                        "type alias `{name}` takes {} type argument{} but {} given",
                        sig.generics.len(),
                        if sig.generics.len() == 1 { "" } else { "s" },
                        args.len(),
                    ),
                    span,
                ));
            }
        }
        let mut map = HashMap::new();
        for (i, g) in sig.generics.iter().enumerate() {
            if let Some(a) = args.get(i) {
                map.insert(g.clone(), self.ast_to_type_inner(a, outer_generics));
            }
        }
        subst(&sig.target, &map)
    }
}
