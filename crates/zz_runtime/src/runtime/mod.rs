//! Shared runtime types extracted from the tree-walker interpreter.
//!
//! Contains the error type, control-flow enum, native function registry
//! types, and the [`RuntimeState`] struct that holds all mutable
//! interpreter state. Both the tree-walker and the bytecode VM operate
//! on this shared state.

pub mod format;
pub mod ops;

use std::collections::HashMap;

use zz_frontend::span::Span;

use crate::value::{FuncValue, Value};

// Re-exports for convenience.
pub use crate::env::Env as EnvReexport;
pub use crate::value::{FuncValue as FuncValueReexport, Value as ValueReexport};

#[derive(Debug)]
pub struct EvalError {
    pub message: String,
    pub span: Span,
    /// Call stack at the time of the error: (function_name, call_site_span).
    pub backtrace: Vec<(String, Span)>,
    /// Extra `= ...` note lines rendered under the error (e.g. hints).
    pub notes: Vec<String>,
}

impl EvalError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        EvalError {
            message: message.into(),
            span,
            backtrace: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Loud internal error for a green-thread `Yield` that reached code
    /// which cannot suspend (interpreter frames live on the Rust call
    /// stack). Blocking natives park the thread instead whenever such
    /// frames are above them, so this is always a bug.
    pub fn yield_escape() -> Self {
        EvalError::new(
            "internal error: green-thread yield across interpreter frames",
            Span::new(0, 0),
        )
    }

    pub fn with_backtrace(mut self, bt: Vec<(String, Span)>) -> Self {
        self.backtrace = bt;
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes.extend(notes);
        self
    }
}

/// Result of evaluating an expression or statement. `Return` unwinds the
/// call stack until the enclosing function call catches it; `Break` and
/// `Continue` unwind to the enclosing loop. `Yield` suspends a green-thread
/// task at a blocking call (`chan.recv`/`task.join` on an unready object);
/// only the executor produces or consumes it.
#[derive(Debug)]
pub enum Flow {
    Value(Value),
    Return(Value),
    Break(Span),
    Continue(Span),
    Yield(crate::value::YieldReason),
}

impl Flow {
    pub(crate) fn into_value(self) -> Result<Value, EvalError> {
        match self {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
            Flow::Break(span) => Err(EvalError::new("`break` outside of a loop", span)),
            Flow::Continue(span) => Err(EvalError::new("`continue` outside of a loop", span)),
            Flow::Yield(_) => Err(EvalError::new(
                "internal error: green-thread yield escaped its executor",
                Span::new(0, 0),
            )),
        }
    }
}

/// A native function implementation. Receives the interpreter (so natives
/// can call back into ZZ, e.g. HTTP route handlers), the argument vector
/// (a `Vec`, not a slice, because `std.vec.push` must grow it), and the
/// call-site span for accurate error reporting.
#[allow(clippy::ptr_arg)]
pub type NativeFn = fn(&mut crate::eval::Interp, &mut Vec<Value>, Span) -> Result<Value, EvalError>;

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
    pub env: crate::env::EnvLink,
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
            env: crate::env::EnvLink::new(),
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
            env: crate::env::EnvLink::new(),
            funcs: HashMap::new(),
            natives,
            structs: HashMap::new(),
            args: Vec::new(),
            defer_stacks: Vec::new(),
        }
    }
}
