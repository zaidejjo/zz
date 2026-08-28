//! Shared runtime types extracted from the tree-walker interpreter.
//!
//! Contains the error type, control-flow enum, native function registry
//! types, and the [`RuntimeState`] struct that holds all mutable
//! interpreter state. Both the tree-walker and the bytecode VM operate
//! on this shared state.

pub mod format;
pub mod ops;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use zz_frontend::span::Span;

use crate::env::Env;
use crate::value::{FuncValue, Value};

// Re-exports for convenience.
pub use crate::env::Env as EnvReexport;
pub use crate::value::{FuncValue as FuncValueReexport, Value as ValueReexport};

#[derive(Debug)]
pub struct EvalError {
    pub message: String,
    pub span: Span,
}

impl EvalError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        EvalError {
            message: message.into(),
            span,
        }
    }
}

/// Result of evaluating an expression or statement. `Return` unwinds the
/// call stack until the enclosing function call catches it; `Break` and
/// `Continue` unwind to the enclosing loop.
#[derive(Debug)]
pub(crate) enum Flow {
    Value(Value),
    Return(Value),
    Break,
    Continue,
}

impl Flow {
    pub(crate) fn into_value(self) -> Result<Value, EvalError> {
        match self {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
            Flow::Break => Err(EvalError::new("`break` outside of a loop", Span::new(0, 0))),
            Flow::Continue => Err(EvalError::new(
                "`continue` outside of a loop",
                Span::new(0, 0),
            )),
        }
    }
}

/// A native function implementation. Receives the interpreter (so natives
/// can call back into ZZ, e.g. HTTP route handlers) and the argument vector
/// (a `Vec`, not a slice, because `std.vec.push` must grow it).
#[allow(clippy::ptr_arg)]
pub type NativeFn = fn(&mut crate::eval::Interp, &mut Vec<Value>) -> Result<Value, EvalError>;

/// A registered native function: its arity and Rust implementation.
#[derive(Debug, Clone, Copy)]
pub struct NativeEntry {
    pub arity: usize,
    pub f: NativeFn,
}

/// Shared mutable state for the ZZ runtime. Extracted from `Interp` so
/// both the tree-walker and the bytecode VM can operate on the same
/// underlying state.
pub struct RuntimeState {
    pub env: Rc<RefCell<Env>>,
    /// Named functions, kept separate from the environment so recursive
    /// bodies can resolve their own name without circular captured envs.
    pub funcs: HashMap<String, FuncValue>,
    /// Native (Rust-backed) functions, e.g. the standard library.
    pub natives: HashMap<String, NativeEntry>,
    /// Struct definitions: name → ordered field names.
    pub structs: HashMap<String, Vec<String>>,
    /// Command-line arguments passed to the running script (empty in the
    /// REPL). Exposed to scripts via `std.env.args`.
    pub args: Vec<String>,
    /// Deferred closures per function call level. `Stmt::Defer` pushes
    /// here; `call_func` pops and executes in LIFO order on return.
    pub defer_stacks: Vec<Vec<Value>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeState {
    pub fn new() -> Self {
        RuntimeState {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives: HashMap::new(),
            structs: HashMap::new(),
            args: Vec::new(),
            defer_stacks: Vec::new(),
        }
    }

    /// Create a runtime state with a native function registry.
    pub fn with_natives(natives: HashMap<String, NativeEntry>) -> Self {
        RuntimeState {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives,
            structs: HashMap::new(),
            args: Vec::new(),
            defer_stacks: Vec::new(),
        }
    }
}
