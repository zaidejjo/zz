//! ZZ type checker (Phase 1).
//!
//! Unification-based inference for locals, explicit generics, pattern
//! matching with exhaustiveness, and spanned diagnostics for type errors.

pub mod checker;
pub mod type_;
pub mod unify;

pub use checker::inference::subst as subst_type;
pub use checker::{
    check_program, check_program_typed, check_program_with_consts, AliasSig, CheckResult,
    ConvertImpl, EnumSig, FuncSig, SpanKey, StructSig, TOP_SCOPE,
};
pub use type_::Type;
pub use unify::{Unifier, UnifyError};
