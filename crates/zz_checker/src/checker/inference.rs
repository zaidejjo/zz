//! Type inference helpers: unification, type variables, generics.

use crate::checker::Checker;
use crate::type_::Type;
use zz_frontend::ast::Ty;
use zz_frontend::diag::error_at;
use zz_frontend::span::Span;

impl Checker {
    /// Merge element types into a single type: identical types collapse to
    /// one; differing types form a union.
    pub(crate) fn merge_types(&mut self, types: Vec<Type>) -> Type {
        if types.is_empty() {
            return self.unifier.fresh_var();
        }
        let mut distinct: Vec<Type> = Vec::new();
        for t in types {
            let rt = self.unifier.resolve(&t);
            if let Some(existing) = distinct.iter().find(|d| self.unifier.resolve(d) == rt) {
                let _ = self.unifier.unify(existing, &t);
            } else {
                distinct.push(t);
            }
        }
        if distinct.len() == 1 {
            distinct.pop().unwrap()
        } else {
            Type::Union(distinct)
        }
    }

    // --- generics ---------------------------------------------------------

    /// Does the generic parameter currently in scope carry the given bound?
    pub(crate) fn has_bound(&self, name: &str, bound: zz_frontend::ast::TraitBound) -> bool {
        self.current_bounds
            .get(name)
            .is_some_and(|bs| bs.contains(&bound))
    }

    /// Does a concrete type satisfy a trait bound?
    pub(crate) fn satisfies_bound(&self, ty: &Type, bound: zz_frontend::ast::TraitBound) -> bool {
        use zz_frontend::ast::TraitBound as B;
        match ty {
            Type::Union(ms) => ms.iter().all(|m| self.satisfies_bound(m, bound)),
            Type::Var(_) | Type::Named(_) | Type::Error => false,
            Type::Func(..) => false,
            Type::Unit | Type::Void => false,
            _ => match bound {
                B::Num => matches!(ty, Type::Int | Type::Float),
                B::Ord => matches!(ty, Type::Int | Type::Float | Type::Str),
                B::Eq => true,
                B::Display => true,
            },
        }
    }

    /// Validate that every generic parameter with a bound was instantiated
    /// with a concrete type satisfying that bound. Called at call sites after
    /// argument unification. Unresolved generics (never pinned by the call)
    /// are skipped — the body's operator checks already validated them.
    pub(crate) fn validate_bounds(
        &mut self,
        sig: &crate::checker::FuncSig,
        subs: &std::collections::HashMap<String, Type>,
        span: Span,
    ) {
        for (gen, bounds) in &sig.bounds {
            let Some(sub) = subs.get(gen) else { continue };
            let rt = self.unifier.resolve(sub);
            if matches!(rt, Type::Var(_)) {
                continue;
            }
            for b in bounds {
                if !self.satisfies_bound(&rt, *b) {
                    self.errors.push(error_at(
                        format!(
                            "type `{rt}` does not satisfy bound `{}` for generic parameter `{gen}`",
                            b.name()
                        ),
                        span,
                    ));
                }
            }
        }
    }

    pub(crate) fn instantiate(
        &mut self,
        sig: &crate::checker::FuncSig,
    ) -> (Vec<Type>, Type, std::collections::HashMap<String, Type>) {
        let subs: std::collections::HashMap<String, Type> = sig
            .generics
            .iter()
            .map(|g| (g.clone(), self.unifier.fresh_var()))
            .collect();
        let params = sig.params.iter().map(|(_, t)| subst(t, &subs)).collect();
        let ret = subst(&sig.ret, &subs);
        (params, ret, subs)
    }

    // --- type annotations -------------------------------------------------

    pub(crate) fn ast_to_type(&mut self, ty: &Ty, generics: &[String]) -> Type {
        self.ast_to_type_inner(ty, generics)
    }

    pub(crate) fn ast_to_type_inner(&mut self, ty: &Ty, generics: &[String]) -> Type {
        use zz_frontend::ast::TyKind;
        match &ty.kind {
            TyKind::Int => Type::Int,
            TyKind::Float => Type::Float,
            TyKind::Bool => Type::Bool,
            TyKind::Str => Type::Str,
            TyKind::Unit => Type::Unit,
            TyKind::Void => Type::Void,
            TyKind::Ptr { mutable, inner } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.ast_to_type_inner(inner, generics)),
            },
            TyKind::Tuple(ts) => Type::Tuple(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Option(t) => Type::Option(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Result(t, e) => Type::Result(
                Box::new(self.ast_to_type_inner(t, generics)),
                Box::new(self.ast_to_type_inner(e, generics)),
            ),
            TyKind::Func(ps, r) => Type::Func(
                ps.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
                Box::new(self.ast_to_type_inner(r, generics)),
            ),
            TyKind::Array(t) => Type::Array(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Dict(k, v) => Type::Dict(
                Box::new(self.ast_to_type_inner(k, generics)),
                Box::new(self.ast_to_type_inner(v, generics)),
            ),
            TyKind::Union(ts) => Type::Union(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Named(name, args) => {
                if generics.iter().any(|g| g == name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("generic parameter `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Named(name.clone())
                } else if self.structs.contains_key(name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("struct `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Struct(name.clone())
                } else if name == "json" {
                    Type::Json
                } else {
                    self.errors
                        .push(error_at(format!("unknown type `{name}`"), ty.span));
                    Type::Unit
                }
            }
        }
    }
}

/// Check if a type contains any unresolved inference variables.
pub(crate) fn contains_var(t: &Type) -> bool {
    match t {
        Type::Var(_) => true,
        Type::Void => false,
        Type::Tuple(ts) => ts.iter().any(contains_var),
        Type::Option(x) => contains_var(x),
        Type::Result(a, b) => contains_var(a) || contains_var(b),
        Type::Func(ps, r) => ps.iter().any(contains_var) || contains_var(r),
        Type::Array(x) => contains_var(x),
        Type::Dict(k, v) => contains_var(k) || contains_var(v),
        Type::Union(ts) => ts.iter().any(contains_var),
        Type::Range(x) => contains_var(x),
        Type::Ptr { inner, .. } => contains_var(inner),
        _ => false,
    }
}

/// Replace unresolved type variables inside `Option`/`Result` with `unit`
/// so `.none`/`.ok`/`.err` bindings type-check without annotations.
pub(crate) fn default_variant_vars(t: &mut Type) {
    match t {
        Type::Option(inner) => {
            if contains_var(inner) {
                default_variant_vars(inner);
                if contains_var(inner) {
                    **inner = Type::Unit;
                }
            }
        }
        Type::Result(ok, err) => {
            if contains_var(ok) {
                default_variant_vars(ok);
                if contains_var(ok) {
                    **ok = Type::Unit;
                }
            }
            if contains_var(err) {
                default_variant_vars(err);
                if contains_var(err) {
                    **err = Type::Unit;
                }
            }
        }
        _ => {}
    }
}

/// Substitute generic type parameters in a type.
pub(crate) fn subst(t: &Type, subs: &std::collections::HashMap<String, Type>) -> Type {
    match t {
        Type::Named(name) => subs
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Named(name.clone())),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Option(x) => Type::Option(Box::new(subst(x, subs))),
        Type::Result(a, b) => Type::Result(Box::new(subst(a, subs)), Box::new(subst(b, subs))),
        Type::Func(ps, r) => Type::Func(
            ps.iter().map(|x| subst(x, subs)).collect(),
            Box::new(subst(r, subs)),
        ),
        Type::Array(x) => Type::Array(Box::new(subst(x, subs))),
        Type::Dict(k, v) => Type::Dict(Box::new(subst(k, subs)), Box::new(subst(v, subs))),
        Type::Union(ts) => Type::Union(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Range(x) => Type::Range(Box::new(subst(x, subs))),
        Type::Ptr { mutable, inner } => Type::Ptr {
            mutable: *mutable,
            inner: Box::new(subst(inner, subs)),
        },
        other => other.clone(),
    }
}
