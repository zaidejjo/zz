//! ZZ type checker (Phase 1).
//!
//! Unification-based inference for locals, explicit generics, pattern
//! matching with exhaustiveness, and spanned diagnostics for type errors.

pub mod checker;
pub use zz_types as type_;
pub use zz_unify as unify;

pub use checker::inference::subst as subst_type;
pub use checker::{
    canonical_std_name, check_program, check_program_typed, check_program_with_consts,
    check_program_with_module_ns, contains_std, is_std_module_head, lookup_std, AliasSig,
    CheckResult, ConvertImpl, EnumSig, FuncSig, SpanKey, StructSig, STD_MODULES, TOP_SCOPE,
};
pub use zz_types::Type;
pub use zz_unify::{Unifier, UnifyError};
