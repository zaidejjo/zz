//! Standard library type signatures, consumed by the checker.
//!
//! GENERATED FILE - do not edit by hand.
//!
//! The declarative source of truth lives in `zz_stdlib_meta::table`; this
//! shim only exposes the cached accessors that `zz_stdlib` and its consumers
//! rely on. Regenerate with:
//!
//! ```text
//! cargo run -p zz_stdlib_meta --bin regen
//! ```
//!
//! Keeping the table in the leaf crate `zz_stdlib_meta` lets the AOT FFI
//! tables derive from the same data without a dependency cycle.

use std::collections::HashMap;

use zz_checker::FuncSig;

/// All standard library function signatures, keyed by qualified name
/// (e.g. `std.str.length`). Console I/O lives here as bare builtins
/// (`print`, `println`, `input`).
///
/// The table is built once per process and cloned on each call: callers that
/// only need to look things up should use [`stdlib_funcs_cached`].
pub fn stdlib_funcs() -> HashMap<String, FuncSig> {
    stdlib_funcs_cached().clone()
}

/// Borrow the process-wide cached signature table.
///
/// Built lazily on first use from [`zz_stdlib_meta::build`], so the checker
/// pays for it at most once per process.
pub fn stdlib_funcs_cached() -> &'static HashMap<String, FuncSig> {
    static CACHED: std::sync::OnceLock<HashMap<String, FuncSig>> = std::sync::OnceLock::new();
    CACHED.get_or_init(zz_stdlib_meta::build)
}
