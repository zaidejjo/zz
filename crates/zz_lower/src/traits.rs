//! Static trait dispatch for the native backend.
//!
//! ZZ generics are checked and then **erased**: `func render<T: Draw>(s: T)`
//! compiles to one C function whose body calls `s.draw()` on a `zz_value`.
//! There is no monomorphization, so the call site cannot name a concrete
//! `zz_fn_<Type>_<method>` symbol.
//!
//! The bridge is one dispatcher per `(trait, method)`: it inspects the
//! receiver's runtime type name and forwards to the matching implementor.
//! The VM already does exactly this (`{type_name}.{method}` lookup in
//! `Op::CallMethod`), so both engines agree by construction.
//!
//! Generating a dispatcher (rather than a table) keeps the AOT output
//! self-contained: no data section, no function pointers, and every
//! implementor stays visible to `-ffunction-sections -gc-sections`.

use zz_frontend::ast::{Expr, Ident, TraitBound};

use super::*;

impl Lowerer {
    /// True when `trait_name` has at least one implementor in this program.
    /// A trait with no impls needs no dispatcher (nothing to dispatch to).
    fn trait_has_impls(&self, trait_name: &str) -> bool {
        self.tp
            .trait_impls
            .get(trait_name)
            .is_some_and(|types| !types.is_empty())
    }

    /// Every `(trait, method)` pair that needs a dispatcher, sorted so the
    /// generated C is deterministic across runs and map iteration orders.
    pub(crate) fn trait_dispatch_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (tname, tsig) in &self.tp.traits {
            if !self.trait_has_impls(tname) {
                continue;
            }
            for method in tsig.method_names() {
                pairs.push((tname.clone(), method.clone()));
            }
        }
        pairs.sort();
        pairs.dedup();
        pairs
    }

    /// C symbol of the dispatcher for `(trait, method)`.
    ///
    /// `mangle` maps `.` to `_` and `_` to `__`, so the name cannot collide
    /// with a user function (`zz_fn_…`) or a native (`zz_<ns>_<method>`).
    pub(crate) fn trait_dispatcher_name(trait_name: &str, method: &str) -> String {
        format!("zz_trait_{}", mangle(&format!("{trait_name}.{method}")))
    }

    /// Implementors of `trait_name` that actually emit `method`.
    ///
    /// Filtered by [`Lowerer::reachable_funcs`] — the same gate `lower()`
    /// uses before emitting a function body — so a dispatcher branch never
    /// references a symbol the backend dropped. The callgraph keeps every
    /// `*.{method}` edge for a generic receiver, so a legal call always
    /// finds its implementor here.
    fn trait_impl_targets(&self, trait_name: &str, method: &str) -> Vec<String> {
        let mut types: Vec<String> = self
            .tp
            .trait_impls
            .get(trait_name)
            .cloned()
            .unwrap_or_default();
        types.sort();
        types.dedup();
        types.retain(|ty| {
            let full = format!("{ty}.{method}");
            self.reachable_funcs.contains(&full) && self.tp.funcs.contains_key(&full)
        });
        types
    }

    /// Forward declarations for every trait dispatcher. Emitted into a header
    /// section visible to every translation unit (dispatchers are called
    /// from whichever unit holds a bounded generic function).
    pub(crate) fn lower_trait_dispatch_prototypes(&self) -> String {
        let mut out = String::new();
        for (tname, method) in self.trait_dispatch_pairs() {
            out.push_str(&format!(
                "{}zz_value {}(zz_value *args, size_t argc);\n",
                self.link_kw(),
                Self::trait_dispatcher_name(&tname, &method)
            ));
        }
        out
    }

    /// Definitions for every trait dispatcher, in deterministic order.
    ///
    /// Each branch tests the receiver's runtime type name with
    /// `zz_trait_type_is` (exact struct match or enum-variant prefix) and
    /// forwards. The receiver convention differs by implementor kind:
    ///
    /// - struct: `zz_fn_<Type>_<method>(zz_value *self, zz_value *args,
    ///   size_t argc)` — trait implementors are always boxed, so `self` is a
    ///   `zz_value *` and the dispatcher passes `&args[0]`.
    /// - enum: `zz_fn_<Type>_<method>(zz_value *args, size_t argc)` — plain
    ///   function shape with the receiver in `args[0]`.
    pub(crate) fn lower_trait_dispatchers(&self) -> String {
        let mut out = String::new();
        for (tname, method) in self.trait_dispatch_pairs() {
            let targets = self.trait_impl_targets(&tname, &method);
            let cname = Self::trait_dispatcher_name(&tname, &method);
            out.push_str(&format!(
                "{}zz_value {cname}(zz_value *args, size_t argc) {{\n",
                self.link_kw()
            ));
            if !targets.is_empty() {
                out.push_str("    if (argc >= 1) {\n");
                for ty in &targets {
                    let callee = format!("zz_fn_{}", mangle(&format!("{ty}.{method}")));
                    let forward = if self.tp.structs.contains_key(ty) {
                        format!("{callee}(&args[0], argc > 1 ? &args[1] : NULL, argc - 1)")
                    } else {
                        format!("{callee}(args, argc)")
                    };
                    out.push_str(&format!(
                        "        if (zz_trait_type_is(&args[0], \"{ty}\")) return {forward};\n"
                    ));
                }
                out.push_str("    }\n");
            }
            out.push_str(&format!(
                "    return zz_trait_missing_impl(\"{tname}\", \"{method}\", argc >= 1 ? args[0] : zz_unit());\n"
            ));
            out.push_str("}\n\n");
        }
        out
    }

    /// Split a method-call callee into `(receiver, method)` for the shapes a
    /// generic-receiver call can take: `item.draw()` (a 2-segment path — the
    /// AST shape the parser produces for a local receiver) and the final
    /// `Field` step of a longer receiver chain. Deeper paths keep the regular
    /// resolution: a generic parameter is never dotted.
    pub(crate) fn split_method_callee(callee: &Expr) -> Option<(Expr, String)> {
        match callee {
            Expr::Field { obj, name, .. } => Some(((**obj).clone(), name.clone())),
            Expr::Path { parts, span } if parts.len() == 2 => Some((
                Expr::Ident {
                    name: parts[0].clone(),
                    span: *span,
                },
                parts[1].clone(),
            )),
            _ => None,
        }
    }

    /// Resolve a method call on a bounded generic receiver to its dispatcher.
    ///
    /// Returns `(trait name, dispatcher symbol)` when `recv` is a generic
    /// parameter [`zz_checker::Type::Named`] of the enclosing function and
    /// one of that parameter's user-trait bounds declares `method`. The
    /// checker rejects a call no bound provides (and an ambiguous one that
    /// two bounds provide), so the first match is the right one.
    fn trait_dispatcher_for(
        &self,
        recv: &Expr,
        method: &str,
        names: &NameCtx,
    ) -> Option<(String, String)> {
        // Path receivers carry no per-segment span, so the recorded
        // expression type may be missing; the binding table holds the same
        // information for parameters and typed locals.
        let recv_ty = self.ty_at(names, recv.span()).or_else(|| match recv {
            Expr::Ident { name, .. } => names.checker_types.get(name),
            _ => None,
        })?;
        let zz_checker::Type::Named(generic) = recv_ty else {
            return None;
        };
        let sig = self.tp.funcs.get(&names.current_scope)?;
        let (_, bounds) = sig.bounds.iter().find(|(name, _)| name == generic)?;
        bounds.iter().find_map(|bound| {
            let TraitBound::User(trait_name) = bound else {
                return None;
            };
            let tsig = self.tp.traits.get(trait_name)?;
            if !tsig.methods.contains_key(method) || !self.trait_has_impls(trait_name) {
                return None;
            }
            Some((
                trait_name.clone(),
                Self::trait_dispatcher_name(trait_name, method),
            ))
        })
    }

    /// Lower a method call on a bounded generic receiver.
    ///
    /// `args`/`named` are the call's user arguments (the receiver is
    /// separate); named arguments are reordered against the trait method's
    /// declared parameter names, mirroring how direct calls reorder.
    /// Returns `None` when the receiver is not a bounded generic parameter
    /// or no bound provides `method`, leaving the call to the regular
    /// resolution path.
    pub(crate) fn lower_trait_call(
        &self,
        recv: &Expr,
        method: &str,
        args: &[Expr],
        named: &[(Ident, Expr)],
        names: &mut NameCtx,
        out: &mut String,
    ) -> Option<String> {
        let (trait_name, dispatcher) = self.trait_dispatcher_for(recv, method, names)?;
        let positional: Vec<&Expr> = if named.is_empty() {
            args.iter().collect()
        } else {
            self.reorder_trait_args(&trait_name, method, args, named)
        };
        // Emit the receiver first so its side effects precede the arguments
        // (the array initializer itself has no evaluation-order guarantee).
        // The call may retain or mutate the receiver, so drop any literal
        // length/index knowledge recorded for it — same invalidation the
        // regular call path performs.
        if let Expr::Ident { name, .. } = recv {
            names.invalidate_array_len(name);
            names.invalidate_dict_len(name);
            names.invalidate_stack_array_elems(name);
        }
        let recv_c = self.emit_expr(recv, names, out);
        let mut items: Vec<String> = Vec::with_capacity(positional.len() + 1);
        items.push(recv_c);
        for arg in positional {
            items.push(self.emit_expr(arg, names, out));
        }
        Some(format!(
            "({dispatcher}((zz_value[]){{ {joined} }}, {n}))",
            joined = items.join(", "),
            n = items.len()
        ))
    }

    /// Reorder named arguments to the trait method's declared positions.
    ///
    /// The trait signature's `params[0]` is the receiver (`Self`), so the
    /// user-facing parameters start at index 1. Unknown names keep their
    /// encounter order at the end; the checker has already validated names
    /// and arity, so this only affects well-formed calls.
    fn reorder_trait_args<'a>(
        &self,
        trait_name: &str,
        method: &str,
        args: &'a [Expr],
        named: &'a [(Ident, Expr)],
    ) -> Vec<&'a Expr> {
        let Some(tsig) = self.tp.traits.get(trait_name) else {
            return args.iter().chain(named.iter().map(|(_, v)| v)).collect();
        };
        let Some(msig) = tsig.methods.get(method) else {
            return args.iter().chain(named.iter().map(|(_, v)| v)).collect();
        };
        let params: Vec<&str> = msig
            .params
            .iter()
            .skip(1)
            .map(|(name, _)| name.as_str())
            .collect();
        let mut slots: Vec<Option<&'a Expr>> = vec![None; params.len()];
        for (i, arg) in args.iter().enumerate() {
            if i < slots.len() && slots[i].is_none() {
                slots[i] = Some(arg);
            }
        }
        for (name, value) in named {
            if let Some(idx) = params
                .iter()
                .position(|param| *param == name.name)
                .filter(|idx| slots[*idx].is_none())
            {
                slots[idx] = Some(value);
            }
        }
        slots.into_iter().flatten().collect()
    }
}
