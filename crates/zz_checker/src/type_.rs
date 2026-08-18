//! Checker types.
//!
//! `Type` is the inferred type language. `Var` are inference variables
//! (mutable through the `Unifier`); `Named` are generic parameters in
//! function signatures (substituted with fresh vars at call sites).

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Tuple(Vec<Type>),
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Func(Vec<Type>, Box<Type>),
    /// `[T]` — array type.
    Array(Box<Type>),
    /// `{K: V}` — dictionary type.
    Dict(Box<Type>, Box<Type>),
    /// `A | B` — union type.
    Union(Vec<Type>),
    /// Inference variable.
    Var(u32),
    /// Named type: a generic parameter (e.g. `T` in `func id<T>`).
    Named(String),
}

impl Type {
    /// Is this (resolved) type numeric?
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Bool => write!(f, "bool"),
            Type::Str => write!(f, "str"),
            Type::Unit => write!(f, "unit"),
            Type::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Type::Option(t) => write!(f, "Option<{t}>"),
            Type::Result(t, e) => write!(f, "Result<{t}, {e}>"),
            Type::Func(ps, r) => {
                write!(f, "func(")?;
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {r}")
            }
            Type::Array(t) => write!(f, "[{t}]"),
            Type::Dict(k, v) => write!(f, "{{{k}: {v}}}"),
            Type::Union(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{t}")?;
                }
                Ok(())
            }
            Type::Var(_) => write!(f, "_"),
            Type::Named(n) => write!(f, "{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_nested() {
        let t = Type::Result(Box::new(Type::Int), Box::new(Type::Str));
        assert_eq!(t.to_string(), "Result<int, str>");
        let t2 = Type::Option(Box::new(Type::Func(vec![Type::Int], Box::new(Type::Bool))));
        assert_eq!(t2.to_string(), "Option<func(int) -> bool>");
    }
}
