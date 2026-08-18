//! ZZ runtime: tree-walker interpreter (Phase 0).
//!
//! Powers the REPL and `zz run` until the bytecode VM lands in a later
//! phase. The frontend stays shared between all execution modes.

pub mod env;
pub mod interp;
pub mod json;
pub mod value;

pub use env::Env;
pub use interp::{EvalError, Interp, NativeEntry, NativeFn};
pub use value::{NativeFunc, Value};
pub use zz_frontend::span::Span;
