//! Runtime values for the Phase 1 tree-walker.

use std::fmt;

use zz_frontend::ast::{Expr, Param};

use crate::env::Env;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Unit,
    /// `.some(v)` / `.none`.
    Option(Option<Box<Value>>),
    /// `.ok(v)` / `.err(e)`.
    Result(Result<Box<Value>, Box<Value>>),
    /// A closure or named function, with its captured environment.
    Func(FuncValue),
    /// `[v1, v2, ...]`.
    Array(Vec<Value>),
    /// `{k1: v1, k2: v2, ...}` — insertion-ordered key/value pairs.
    Dict(Vec<(Value, Value)>),
    /// A native (Rust-backed) function from the standard library.
    Native(NativeFunc),
    /// A parsed JSON value (opaque to the type system).
    Json(JsonValue),
    /// An HTTP server handle with its registered routes.
    HttpServer(HttpServer),
}

/// A JSON value (see [`crate::json`]).
pub use crate::json::JsonValue;

/// An HTTP server: registered (method, path, handler) routes.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpServer {
    pub routes: Vec<(String, String, Value)>,
}

/// A native function reference: name + arity. The implementation lives in
/// the interpreter's native registry.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeFunc {
    pub name: String,
    pub arity: usize,
}

/// A callable value: parameter list, body expression, and the environment
/// captured at definition time.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncValue {
    pub params: Vec<Param>,
    pub body: Expr,
    pub env: Env,
}

impl Value {
    /// Promote a numeric value to float (used for mixed arithmetic).
    pub fn to_float(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_truthy(&self) -> bool {
        matches!(self, Value::Bool(true))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            // Always show a decimal point so floats are distinguishable.
            Value::Float(x) => {
                if x.is_finite() && x.fract() == 0.0 {
                    write!(f, "{x:.1}")
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Unit => write!(f, ""),
            Value::Option(Some(v)) => write!(f, ".some({v})"),
            Value::Option(None) => write!(f, ".none"),
            Value::Result(Ok(v)) => write!(f, ".ok({v})"),
            Value::Result(Err(e)) => write!(f, ".err({e})"),
            Value::Func(_) => write!(f, "<func>"),
            Value::Array(vs) => {
                write!(f, "[")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Dict(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Native(nf) => write!(f, "<native {}>", nf.name),
            Value::Json(j) => write!(f, "{j}"),
            Value::HttpServer(_) => write!(f, "<http server>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_displays_plain() {
        assert_eq!(Value::Int(42).to_string(), "42");
    }

    #[test]
    fn float_always_shows_decimal() {
        assert_eq!(Value::Float(3.0).to_string(), "3.0");
        assert_eq!(Value::Float(3.5).to_string(), "3.5");
    }

    #[test]
    fn unit_displays_empty() {
        assert_eq!(Value::Unit.to_string(), "");
    }

    #[test]
    fn array_displays() {
        assert_eq!(
            Value::Array(vec![Value::Int(1), Value::Int(2)]).to_string(),
            "[1, 2]"
        );
    }

    #[test]
    fn dict_displays() {
        assert_eq!(
            Value::Dict(vec![
                (Value::Str("a".into()), Value::Int(1)),
                (Value::Str("b".into()), Value::Int(2)),
            ])
            .to_string(),
            "{a: 1, b: 2}"
        );
    }

    #[test]
    fn variants_display() {
        assert_eq!(
            Value::Option(Some(Box::new(Value::Int(1)))).to_string(),
            ".some(1)"
        );
        assert_eq!(Value::Option(None).to_string(), ".none");
        assert_eq!(
            Value::Result(Ok(Box::new(Value::Int(1)))).to_string(),
            ".ok(1)"
        );
        assert_eq!(
            Value::Result(Err(Box::new(Value::Str("x".into())))).to_string(),
            ".err(x)"
        );
    }
}
