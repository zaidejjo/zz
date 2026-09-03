//! Runtime values for the Phase 1 tree-walker.

use std::fmt;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use zz_frontend::ast::{Expr, Param};

use crate::env::Env;

/// A struct instance payload (boxed so `Value` stays small).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectValue {
    pub name: String,
    pub fields: Vec<(String, Value)>,
}

/// An integer range `a..b` / `a..b..step` (boxed so `Value` stays small).
#[derive(Debug, Clone, PartialEq)]
pub struct RangeValue {
    pub start: i64,
    pub end: i64,
    pub step: i64,
}

/// A runtime value.
///
/// Heap-allocates (boxes) every payload larger than a word so the enum fits
/// in 16 bytes (including its discriminant byte and padding). This keeps
/// `LoadSlot`/`StoreSlot`/stack-push copies to a single 16-byte memcpy
/// instead of copying large strings, vectors, or environments inline.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    /// A heap-allocated string (thin pointer).
    Str(Box<String>),
    Bool(bool),
    Unit,
    /// `.some(v)` / `.none`.
    Option(Option<Box<Value>>),
    /// `.ok(v)` / `.err(e)`.
    Result(Box<Result<Value, Value>>),
    /// A closure or named function, with its captured environment.
    Func(Box<FuncValue>),
    /// `[v1, v2, ...]`.
    Array(Box<Vec<Value>>),
    /// `{k1: v1, k2: v2, ...}` — insertion-ordered key/value pairs.
    Dict(Box<Vec<(Value, Value)>>),
    /// A native (Rust-backed) function from the standard library.
    Native(Box<NativeFunc>),
    /// A parsed JSON value (opaque to the type system).
    Json(Box<JsonValue>),
    /// An HTTP server handle with its registered routes.
    HttpServer(Box<HttpServer>),
    /// A TCP stream (opaque, wrapped in Arc<Mutex> for clone safety).
    TcpStream(Arc<Mutex<TcpStream>>),
    /// A TCP listener (opaque, wrapped in Arc<Mutex> for clone safety).
    TcpListener(Arc<Mutex<TcpListener>>),
    /// An HTTP response (status + body + headers).
    Response(Box<Response>),
    /// A struct instance: its type name and insertion-ordered fields.
    Object(Box<ObjectValue>),
    /// `a..b` or `a..b..step` — an integer range (used by `for` loops).
    Range(Box<RangeValue>),
    /// `(v1, v2, ...)` — tuple value.
    Tuple(Box<Vec<Value>>),
}

/// A JSON value (see [`crate::json`]).
pub use crate::json::JsonValue;

/// An HTTP server: registered (method, path, handler) routes.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpServer {
    pub routes: Vec<(String, String, Value)>,
    pub middlewares: Vec<Value>,
    pub log_enabled: bool,
    pub static_dir: Option<String>,
}

/// An HTTP response: status code, body, and headers.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
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

    #[inline(always)]
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
            Value::TcpStream(_) => "tcp.stream".to_string(),
            Value::TcpListener(_) => "tcp.listener".to_string(),
            Value::Response(_) => "http.response".to_string(),
            Value::Object(o) => o.name.clone(),
            Value::Range(_) => "range".to_string(),
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
            Value::TcpStream(_) => Some("net"),
            Value::TcpListener(_) => Some("net"),
            Value::Response(_) => Some("http"),
            Value::Object(o) => {
                // Extract namespace from struct name (e.g., "shapes.Point" -> "shapes")
                o.name
                    .rsplit_once('.')
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
            Value::Result(r) => match &**r {
                Ok(v) => write!(f, ".ok({v})"),
                Err(e) => write!(f, ".err({e})"),
            },
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
            Value::TcpStream(_) => write!(f, "<tcp stream>"),
            Value::TcpListener(_) => write!(f, "<tcp listener>"),
            Value::Response(res) => write!(f, "<http response {}>", res.status),
            Value::Object(o) => {
                write!(f, "{}{{", o.name)?;
                for (i, (k, v)) in o.fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Range(r) => {
                if r.step == 1 {
                    write!(f, "{}..{}", r.start, r.end)
                } else {
                    write!(f, "{}..{}..{}", r.start, r.end, r.step)
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

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Option(a), Value::Option(b)) => a == b,
            (Value::Result(a), Value::Result(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Json(a), Value::Json(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Response(a), Value::Response(b)) => a == b,
            (Value::HttpServer(_), Value::HttpServer(_)) => std::ptr::eq(self, other),
            (Value::Object(a), Value::Object(b)) => a == b,
            // Opaque types: compare by Arc pointer (identity, not deep equality)
            (Value::TcpStream(a), Value::TcpStream(b)) => Arc::ptr_eq(a, b),
            (Value::TcpListener(a), Value::TcpListener(b)) => Arc::ptr_eq(a, b),
            // Func and Native: compare by reference identity (not deep equality)
            (Value::Func(_), Value::Func(_)) => std::ptr::eq(self, other),
            (Value::Native(_), Value::Native(_)) => std::ptr::eq(self, other),
            (Value::Range(a), Value::Range(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod size_assert {
    use super::*;
    #[test]
    fn value_fits_in_16_bytes() {
        assert!(
            std::mem::size_of::<Value>() <= 16,
            "Value grew to {} bytes",
            std::mem::size_of::<Value>()
        );
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
            Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)])).to_string(),
            "[1, 2]"
        );
    }

    #[test]
    fn dict_displays() {
        assert_eq!(
            Value::Dict(Box::new(vec![
                (Value::Str("a".to_string().into()), Value::Int(1)),
                (Value::Str("b".to_string().into()), Value::Int(2)),
            ]))
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
            Value::Result(Box::new(Ok(Value::Int(1)))).to_string(),
            ".ok(1)"
        );
        assert_eq!(
            Value::Result(Box::new(Err(Value::Str("x".to_string().into())))).to_string(),
            ".err(x)"
        );
    }
}
