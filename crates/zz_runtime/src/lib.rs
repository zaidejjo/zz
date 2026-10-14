//! ZZ runtime: tree-walker interpreter (Phase 0).
//!
//! Powers the REPL and `zz run` until the bytecode VM lands in a later
//! phase. The frontend stays shared between all execution modes.

pub mod c_abi;
pub mod coverage;
pub mod env;
pub mod eval;
pub mod json;
pub mod lf_chan;
pub mod runtime;
pub mod value;
pub mod vm;

pub use coverage::{CoverageState, COV_UNMAPPED};
pub use env::Env;
pub use env::EnvLink;
pub use eval::{EvalError, Interp, NativeEntry, NativeFn, SpawnHook, SPAWN_HOOK};
pub use value::{deep_clone, BytesData, FuncValue, NativeFunc, ObjectValue, RangeValue, Value};
pub use vm::Chunk;
pub use zz_frontend::ast::{Expr, Param};
pub use zz_frontend::span::Span;
