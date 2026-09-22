//! ZZ type checker (Phase 1).
//!
//! Unification-based inference for locals, explicit generics, pattern
//! matching with exhaustiveness, and spanned diagnostics for type errors.

pub mod checker;
pub mod type_;
pub mod unify;

pub use checker::{check_program, CheckResult, FuncSig, StructSig};
pub use type_::Type;
pub use unify::{Unifier, UnifyError};
