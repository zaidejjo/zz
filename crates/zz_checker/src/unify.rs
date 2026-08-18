//! Type unification (HM-lite).
//!
//! Inference variables form a union-find structure: each `Var(id)` maps to
//! its resolved type, which may itself contain vars. `resolve` follows the
//! chain; `unify` binds vars and reports the offending pair on conflict.
//! An occurs check prevents infinite types (`let f = |x| f(x)` style).

use std::collections::HashMap;

use crate::type_::Type;

#[derive(Debug, Default)]
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
            other => other,
        }
    }

    /// Unify two types, binding variables as needed.
    pub fn unify(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError> {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (a, b) {
            (Type::Var(x), Type::Var(y)) if x == y => Ok(()),
            (Type::Var(x), t) | (t, Type::Var(x)) => {
                if self.occurs(x, &t) {
                    Err(UnifyError {
                        left: format!("var{x}"),
                        right: t.to_string(),
                        message: "infinite type".into(),
                    })
                } else {
                    self.vars.insert(x, t);
                    Ok(())
                }
            }
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::Str, Type::Str)
            | (Type::Unit, Type::Unit) => Ok(()),
            (Type::Named(a), Type::Named(b)) if a == b => Ok(()),
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
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Type::Option(x), Type::Option(y)) => self.unify(&x, &y),
            (Type::Result(a1, e1), Type::Result(a2, e2)) => {
                self.unify(&a1, &a2)?;
                self.unify(&e1, &e2)
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
                    self.unify(x, y)?;
                }
                self.unify(&r1, &r2)
            }
            (a, b) => Err(UnifyError {
                left: a.to_string(),
                right: b.to_string(),
                message: "type mismatch".into(),
            }),
        }
    }

    fn occurs(&self, id: u32, t: &Type) -> bool {
        match self.resolve(t) {
            Type::Var(other) => other == id,
            Type::Tuple(ts) => ts.iter().any(|x| self.occurs(id, x)),
            Type::Option(x) => self.occurs(id, &x),
            Type::Result(a, b) => self.occurs(id, &a) || self.occurs(id, &b),
            Type::Func(ps, r) => ps.iter().any(|x| self.occurs(id, x)) || self.occurs(id, &r),
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
