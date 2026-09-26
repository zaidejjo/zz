//! Type unification (HM-lite).
//!
//! Inference variables form a union-find structure: each `Var(id)` maps to
//! its resolved type, which may itself contain vars. `resolve` follows the
//! chain; `unify` binds vars and reports the offending pair on conflict.
//! An occurs check prevents infinite types (`let f = |x| f(x)` style).

use std::collections::HashMap;

use crate::type_::Type;

#[derive(Debug, Default, Clone)]
pub struct Unifier {
    vars: HashMap<u32, Type>,
    next_var: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnifyError {
    pub left: String,
    pub right: String,
    pub message: String,
}

impl Unifier {
    pub fn new() -> Self {
        Unifier::default()
    }

    /// Allocate a fresh inference variable.
    pub fn fresh_var(&mut self) -> Type {
        let id = self.next_var;
        self.next_var += 1;
        Type::Var(id)
    }

    /// Directly bind a variable to a type (no occurs check — use when the
    /// type cannot contain the variable).
    pub fn bind(&mut self, id: u32, ty: Type) {
        self.vars.insert(id, ty);
    }

    /// Bind a type (which must resolve to a variable) to another type.
    pub fn bind_var(&mut self, t: &Type, ty: Type) {
        if let Type::Var(id) = self.resolve(t) {
            self.vars.insert(id, ty);
        }
    }

    /// Follow variable chains to a concrete (or outermost var) type.
    pub fn resolve(&self, t: &Type) -> Type {
        match t {
            Type::Var(id) => match self.vars.get(id) {
                Some(inner) => self.resolve(inner),
                None => Type::Var(*id),
            },
            _ => t.clone(),
        }
    }

    /// If `t` resolves through the union-find chain to an *unbound*
    /// variable, return its id. Pure shared traversal — zero allocation.
    /// Returns `None` for concrete nodes (including Vars bound to a type).
    fn terminal_var(&self, t: &Type) -> Option<u32> {
        match t {
            Type::Var(id) => match self.vars.get(id) {
                Some(inner) => self.terminal_var(inner),
                None => Some(*id),
            },
            _ => None,
        }
    }

    /// Structural equality through variable chains, without cloning.
    ///
    /// Used by duplicate-type scans that previously compared
    /// `resolve(a) == resolve(b)` (two full-tree clones per comparison).
    pub fn eq_resolved(&self, a: &Type, b: &Type) -> bool {
        let (mut a, mut b) = (a, b);
        // Peel bound top-level Vars iteratively (shared borrows only).
        loop {
            let mut progressed = false;
            if let Type::Var(id) = a {
                if let Some(inner) = self.vars.get(id) {
                    a = inner;
                    progressed = true;
                }
            }
            if let Type::Var(id) = b {
                if let Some(inner) = self.vars.get(id) {
                    b = inner;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        match (a, b) {
            (Type::Var(x), Type::Var(y)) => x == y,
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::Str, Type::Str)
            | (Type::Unit, Type::Unit)
            | (Type::Void, Type::Void)
            | (Type::Json, Type::Json)
            | (Type::Db, Type::Db)
            | (Type::Bytes, Type::Bytes)
            | (Type::HttpServer, Type::HttpServer)
            | (Type::TcpStream, Type::TcpStream)
            | (Type::TcpListener, Type::TcpListener)
            | (Type::Response, Type::Response)
            | (Type::Chan, Type::Chan)
            | (Type::TaskJoin, Type::TaskJoin)
            | (Type::Error, _)
            | (_, Type::Error) => true,
            (Type::Named(x), Type::Named(y)) => x == y,
            (Type::Struct(x), Type::Struct(y)) => x == y,
            (Type::Opaque(x), Type::Opaque(y)) => x == y,
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| self.eq_resolved(x, y))
            }
            (Type::Option(x), Type::Option(y)) => self.eq_resolved(x, y),
            (Type::Result(a1, e1), Type::Result(a2, e2)) => {
                self.eq_resolved(a1, a2) && self.eq_resolved(e1, e2)
            }
            (Type::Func(p1, r1), Type::Func(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| self.eq_resolved(x, y))
                    && self.eq_resolved(r1, r2)
            }
            (Type::Array(x), Type::Array(y)) => self.eq_resolved(x, y),
            (
                Type::Ptr {
                    mutable: m1,
                    inner: x,
                },
                Type::Ptr {
                    mutable: m2,
                    inner: y,
                },
            ) => m1 == m2 && self.eq_resolved(x, y),
            (Type::Dict(k1, v1), Type::Dict(k2, v2)) => {
                self.eq_resolved(k1, k2) && self.eq_resolved(v1, v2)
            }
            (Type::Union(ms), Type::Union(ns)) => {
                ms.len() == ns.len()
                    && ms
                        .iter()
                        .zip(ns.iter())
                        .all(|(x, y)| self.eq_resolved(x, y))
            }
            (Type::Range(x), Type::Range(y)) => self.eq_resolved(x, y),
            _ => false,
        }
    }

    /// Resolve every variable in a type (used to finalize inferred types).
    pub fn resolve_deep(&self, t: &Type) -> Type {
        let t = self.resolve(t);
        match t {
            Type::Tuple(ts) => Type::Tuple(ts.iter().map(|x| self.resolve_deep(x)).collect()),
            Type::Option(inner) => Type::Option(Box::new(self.resolve_deep(&inner))),
            Type::Result(a, b) => Type::Result(
                Box::new(self.resolve_deep(&a)),
                Box::new(self.resolve_deep(&b)),
            ),
            Type::Func(ps, r) => Type::Func(
                ps.iter().map(|x| self.resolve_deep(x)).collect(),
                Box::new(self.resolve_deep(&r)),
            ),
            Type::Array(t) => Type::Array(Box::new(self.resolve_deep(&t))),
            Type::Dict(k, v) => Type::Dict(
                Box::new(self.resolve_deep(&k)),
                Box::new(self.resolve_deep(&v)),
            ),
            Type::Union(ts) => Type::Union(ts.iter().map(|x| self.resolve_deep(x)).collect()),
            Type::Range(t) => Type::Range(Box::new(self.resolve_deep(&t))),
            Type::Ptr { mutable, inner } => Type::Ptr {
                mutable,
                inner: Box::new(self.resolve_deep(&inner)),
            },
            other => other,
        }
    }

    /// Unify two types, binding variables as needed.
    ///
    /// Fast paths (zero allocation): unbound-variable binding stores a
    /// single clone of the other side; concrete spines match by reference.
    /// Only a *bound* top-level `Var` pays one subtree clone (peeled into
    /// a frame-owned temporary — never holding `&self` across recursion).
    /// Union member trials roll back a bounded journal instead of cloning
    /// the whole union-find map per attempt.
    pub fn unify(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        let mut journal: Vec<(u32, Option<Type>)> = Vec::new();
        self.unify_inner(a, b, &mut journal)
    }

    /// Record a binding, journaling the previous value once per trial nest
    /// so speculative (union-member) attempts can roll back cheaply.
    fn journaled_insert(&mut self, journal: &mut Vec<(u32, Option<Type>)>, id: u32, ty: Type) {
        if !journal.iter().any(|(k, _)| *k == id) {
            journal.push((id, self.vars.get(&id).cloned()));
        }
        self.vars.insert(id, ty);
    }

    /// Undo journal entries back to `mark` (exclusive).
    fn rollback(&mut self, journal: &mut Vec<(u32, Option<Type>)>, mark: usize) {
        while journal.len() > mark {
            let (id, old) = journal.pop().expect("unify journal mark out of range");
            match old {
                Some(ty) => {
                    self.vars.insert(id, ty);
                }
                None => {
                    self.vars.remove(&id);
                }
            }
        }
    }

    fn unify_inner(
        &mut self,
        a: &Type,
        b: &Type,
        journal: &mut Vec<(u32, Option<Type>)>,
    ) -> Result<(), UnifyError> {
        // Fast path: unbound terminal variables bind immediately.
        match (self.terminal_var(a), self.terminal_var(b)) {
            (Some(x), Some(y)) if x == y => return Ok(()),
            (Some(x), _) => {
                if self.occurs(x, b) {
                    return Err(UnifyError {
                        left: format!("var{x}"),
                        right: b.to_string(),
                        message: "infinite type".into(),
                    });
                }
                self.journaled_insert(journal, x, b.clone());
                return Ok(());
            }
            (_, Some(y)) => {
                if self.occurs(y, a) {
                    return Err(UnifyError {
                        left: a.to_string(),
                        right: format!("var{y}"),
                        message: "infinite type".into(),
                    });
                }
                self.journaled_insert(journal, y, a.clone());
                return Ok(());
            }
            (None, None) => {}
        }
        // Both tops are concrete-or-bound. Peel one bound-`Var` level per
        // side into frame-owned temporaries (one subtree clone per bound
        // node; concrete nodes cost nothing). The peeled refs borrow the
        // temporaries/inputs — never `self` — so recursion stays mutable.
        let a_owned: Option<Type>;
        let mut a: &Type = a;
        if let Type::Var(id) = a {
            if let Some(bound) = self.vars.get(id) {
                a_owned = Some(bound.clone());
                a = a_owned.as_ref().expect("just assigned");
            }
        }
        let b_owned: Option<Type>;
        let mut b: &Type = b;
        if let Type::Var(id) = b {
            if let Some(bound) = self.vars.get(id) {
                b_owned = Some(bound.clone());
                b = b_owned.as_ref().expect("just assigned");
            }
        }
        match (a, b) {
            (Type::Var(x), Type::Var(y)) if x == y => Ok(()),
            (Type::Var(x), t) | (t, Type::Var(x)) => {
                let x = *x;
                if self.occurs(x, t) {
                    Err(UnifyError {
                        left: format!("var{x}"),
                        right: t.to_string(),
                        message: "infinite type".into(),
                    })
                } else {
                    self.journaled_insert(journal, x, t.clone());
                    Ok(())
                }
            }
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::Str, Type::Str)
            | (Type::Unit, Type::Unit)
            | (Type::Void, Type::Void) => Ok(()),
            // Error is an absorbing type: unify with anything without binding,
            // suppressing cascading type errors from earlier undefined symbols.
            (Type::Error, _) | (_, Type::Error) => Ok(()),
            (Type::Named(a), Type::Named(b)) if a == b => Ok(()),
            (Type::Struct(a), Type::Struct(b)) if a == b => Ok(()),
            // Opaque handles unify only within the same module tag.
            (Type::Opaque(a), Type::Opaque(b)) if a == b => Ok(()),
            (Type::Range(x), Type::Range(y)) => self.unify_inner(x, y, journal),
            (Type::Json, Type::Json)
            | (Type::Db, Type::Db)
            | (Type::Bytes, Type::Bytes)
            | (Type::HttpServer, Type::HttpServer)
            | (Type::TcpStream, Type::TcpStream)
            | (Type::TcpListener, Type::TcpListener)
            | (Type::Response, Type::Response)
            | (Type::Chan, Type::Chan)
            | (Type::TaskJoin, Type::TaskJoin) => Ok(()),
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return Err(UnifyError {
                        left: format!(
                            "({})",
                            xs.iter()
                                .map(|t| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        right: format!(
                            "({})",
                            ys.iter()
                                .map(|t| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        message: "tuple arity mismatch".into(),
                    });
                }
                for (x, y) in xs.iter().zip(ys.iter()) {
                    self.unify_inner(x, y, journal)?;
                }
                Ok(())
            }
            (Type::Option(x), Type::Option(y)) => self.unify_inner(x, y, journal),
            (Type::Result(a1, e1), Type::Result(a2, e2)) => {
                self.unify_inner(a1, a2, journal)?;
                self.unify_inner(e1, e2, journal)
            }
            (Type::Func(p1, r1), Type::Func(p2, r2)) => {
                if p1.len() != p2.len() {
                    return Err(UnifyError {
                        left: format!(
                            "func({})",
                            p1.iter()
                                .map(|t| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        right: format!(
                            "func({})",
                            p2.iter()
                                .map(|t| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        message: "function arity mismatch".into(),
                    });
                }
                for (x, y) in p1.iter().zip(p2.iter()) {
                    self.unify_inner(x, y, journal)?;
                }
                self.unify_inner(r1, r2, journal)
            }
            (Type::Array(x), Type::Array(y)) => self.unify_inner(x, y, journal),
            (
                Type::Ptr {
                    mutable: m1,
                    inner: x,
                },
                Type::Ptr {
                    mutable: m2,
                    inner: y,
                },
            ) => {
                if m1 != m2 {
                    return Err(UnifyError {
                        left: a.to_string(),
                        right: b.to_string(),
                        message: "pointer mutability mismatch (`*const` vs `*mut`)".into(),
                    });
                }
                self.unify_inner(x, y, journal)
            }
            (Type::Dict(k1, v1), Type::Dict(k2, v2)) => {
                self.unify_inner(k1, k2, journal)?;
                self.unify_inner(v1, v2, journal)
            }
            (Type::Union(ms), Type::Union(ns)) => {
                // Every member of `ms` must match the `ns` union. Member
                // trials share the journal (rollback per failed member).
                for m in ms {
                    self.unify_against_union(m, ns, journal)?;
                }
                Ok(())
            }
            (Type::Union(ms), t) | (t, Type::Union(ms)) => {
                // A value matches a union if it matches any member. Try each
                // member with journal rollback and commit the first success
                // (replaces the old full-unifier clone per attempt).
                self.unify_against_union(t, ms, journal)
            }
            (a, b) => Err(UnifyError {
                left: a.to_string(),
                right: b.to_string(),
                message: "type mismatch".into(),
            }),
        }
    }

    /// Unify `t` against a union's members, committing the first success.
    /// Failed member attempts roll back to the entry mark, so only the
    /// winning attempt's bindings survive.
    fn unify_against_union(
        &mut self,
        t: &Type,
        members: &[Type],
        journal: &mut Vec<(u32, Option<Type>)>,
    ) -> Result<(), UnifyError> {
        let mut last_err = None;
        for m in members {
            let mark = journal.len();
            match self.unify_inner(m, t, journal) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    self.rollback(journal, mark);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| UnifyError {
            left: t.to_string(),
            right: members
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(" | "),
            message: "type mismatch".into(),
        }))
    }

    /// Occurs check: does variable `id` appear in `t` (through chains)?
    /// Pure shared traversal — the old version cloned via `resolve` at
    /// every level.
    fn occurs(&self, id: u32, t: &Type) -> bool {
        match t {
            Type::Var(other) => match self.vars.get(other) {
                Some(inner) => *other == id || self.occurs(id, inner),
                None => *other == id,
            },
            Type::Tuple(ts) => ts.iter().any(|x| self.occurs(id, x)),
            Type::Option(x) => self.occurs(id, x),
            Type::Result(a, b) => self.occurs(id, a) || self.occurs(id, b),
            Type::Func(ps, r) => ps.iter().any(|x| self.occurs(id, x)) || self.occurs(id, r),
            Type::Array(x) => self.occurs(id, x),
            Type::Dict(k, v) => self.occurs(id, k) || self.occurs(id, v),
            Type::Union(ts) => ts.iter().any(|x| self.occurs(id, x)),
            Type::Range(x) => self.occurs(id, x),
            Type::Ptr { inner, .. } => self.occurs(id, inner),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_and_resolves() {
        let mut u = Unifier::new();
        let a = u.fresh_var();
        let b = u.fresh_var();
        u.unify(&a, &Type::Int).unwrap();
        u.unify(&b, &a).unwrap();
        assert_eq!(u.resolve(&b), Type::Int);
    }

    #[test]
    fn mismatch_reports_types() {
        let mut u = Unifier::new();
        let err = u.unify(&Type::Int, &Type::Str).unwrap_err();
        assert_eq!(err.left, "int");
        assert_eq!(err.right, "str");
    }

    #[test]
    fn nested_types_unify() {
        let mut u = Unifier::new();
        let a = u.fresh_var();
        let lhs = Type::Result(Box::new(Type::Int), Box::new(a.clone()));
        let rhs = Type::Result(Box::new(Type::Int), Box::new(Type::Str));
        u.unify(&lhs, &rhs).unwrap();
        assert_eq!(u.resolve(&a), Type::Str);
    }

    #[test]
    fn arity_mismatch_reports() {
        let mut u = Unifier::new();
        let err = u
            .unify(
                &Type::Func(vec![Type::Int], Box::new(Type::Int)),
                &Type::Func(vec![Type::Int, Type::Int], Box::new(Type::Int)),
            )
            .unwrap_err();
        assert_eq!(err.message, "function arity mismatch");
    }

    #[test]
    fn occurs_check_catches_infinite() {
        let mut u = Unifier::new();
        let a = u.fresh_var();
        let bad = Type::Func(vec![a.clone()], Box::new(Type::Int));
        let err = u.unify(&a, &bad).unwrap_err();
        assert_eq!(err.message, "infinite type");
    }
}
