//! ZZ runtime: tree-walker interpreter (Phase 0).
//!
//! Powers the REPL and `zz run` until the bytecode VM lands in a later
//! phase. The frontend stays shared between all execution modes.

pub mod env;
pub mod eval;
pub mod json;
pub mod runtime;
pub mod value;
pub mod vm;

pub use env::Env;
pub use eval::{EvalError, Interp, NativeEntry, NativeFn};
pub use value::{NativeFunc, Value};
pub use zz_frontend::span::Span;
