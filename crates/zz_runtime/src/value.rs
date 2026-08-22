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
    /// A struct instance: its type name and insertion-ordered fields.
    Object {
        name: String,
        fields: Vec<(String, Value)>,
    },
    /// `a..b` or `a..b..step` — an integer range (used by `for` loops).
    Range(i64, i64, i64),
    /// `(v1, v2, ...)` — tuple value.
    Tuple(Vec<Value>),
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
/// captured at definition time (shared by reference).
#[derive(Debug, Clone, PartialEq)]
pub struct FuncValue {
    pub params: Vec<Param>,
    pub body: Expr,
    pub env: std::rc::Rc<std::cell::RefCell<Env>>,
    /// Pre-compiled bytecode body, when the function was defined through the
    /// Phase 6 compiler. `None` for tree-walker-created closures.
    pub chunk: Option<std::rc::Rc<crate::vm::Chunk>>,
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

    /// The runtime type name of this value (used by `typeof` and error
    /// messages). Struct instances report their type name.
    pub fn type_name(&self) -> String {
        match self {
            Value::Int(_) => "int".to_string(),
            Value::Float(_) => "float".to_string(),
            Value::Str(_) => "str".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Unit => "unit".to_string(),
            Value::Option(_) => "option".to_string(),
            Value::Result(_) => "result".to_string(),
            Value::Func(_) => "func".to_string(),
            Value::Array(_) => "array".to_string(),
            Value::Dict(_) => "dict".to_string(),
            Value::Native(_) => "native".to_string(),
            Value::Json(_) => "json".to_string(),
            Value::HttpServer(_) => "http.server".to_string(),
            Value::Object { name, .. } => name.clone(),
            Value::Range(..) => "range".to_string(),
            Value::Tuple(_) => "tuple".to_string(),
        }
    }

    /// The method namespace for this value type, used for method dispatch.
    /// Returns "str" for strings, "vec" for arrays, the struct namespace for
    /// objects, and None for types without method support.
    pub fn method_namespace(&self) -> Option<&'static str> {
        match self {
            Value::Str(_) => Some("str"),
            Value::Array(_) => Some("vec"),
            Value::Option(_) => Some("option"),
            Value::Result(_) => Some("result"),
            Value::Object { name, .. } => {
                // Extract namespace from struct name (e.g., "shapes.Point" -> "shapes")
                name.rsplit_once('.')
                    .map(|(ns, _)| Box::leak(ns.to_string().into_boxed_str()) as &str)
            }
            _ => None,
        }
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
            Value::Object { name, fields } => {
                write!(f, "{name}{{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Range(a, b, step) => {
                if *step == 1 {
                    write!(f, "{a}..{b}")
                } else {
                    write!(f, "{a}..{b}..{step}")
                }
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
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
