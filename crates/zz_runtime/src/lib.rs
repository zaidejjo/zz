//! ZZ runtime: tree-walker interpreter (Phase 0).
//!
//! Powers the REPL and `zz run` until the bytecode VM lands in a later
//! phase. The frontend stays shared between all execution modes.

pub mod c_abi;
pub mod coverage;
pub mod debug;
pub mod env;
pub mod eval;
pub mod json;
pub mod lf_chan;
pub mod runtime;
pub mod state;
pub mod value;
pub mod vm;

pub use coverage::{CoverageState, COV_UNMAPPED};
pub use env::Env;
pub use env::EnvLink;
pub use eval::{EvalError, Interp, NativeEntry, NativeFn, SpawnHook, SPAWN_HOOK};
pub use value::{
    deep_clone, upgrade, weak, BytesData, DictData, FuncValue, NativeFunc, ObjectValue, RangeValue,
    Value,
};
pub use vm::runtime::{state_batch_depth_debug, state_lock_depth_debug};
pub use vm::Chunk;
pub use zz_frontend::ast::{Expr, Param};
pub use zz_frontend::span::Span;
