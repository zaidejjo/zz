//! Single declarative source of truth for the ZZ standard library.
//!
//! The signature table lives in [`table`]; `zz_stdlib` (checker signatures)
//! and, from PR 1b, the AOT FFI/`native_impl` tables derive from this crate.
//! Keeping the table in a leaf crate that depends only on `zz_checker`
//! avoids any dependency cycle with `zz_stdlib` / `zz_codegen_ffi`.
//!
//! Regenerate the derived files with:
//! `cargo run -p zz_stdlib_meta --bin regen` (or `-- --check` in CI).

use std::collections::HashMap;

use zz_checker::FuncSig;

pub mod table;

/// Build the complete stdlib signature table.
///
/// One fresh `HashMap` per call; callers (e.g. `zz_stdlib::stdlib_funcs_cached`)
/// cache it.
pub fn build() -> HashMap<String, FuncSig> {
    table::build()
}

#[cfg(test)]
mod tests {
    /// `table::build` panics on a duplicate qualified name (see `table::Table`).
    /// Calling it here makes that lockstep invariant part of `cargo test`.
    #[test]
    fn table_builds_without_duplicates() {
        let rows = crate::build();
        assert!(
            rows.len() > 300,
            "unexpectedly small stdlib table: {} rows",
            rows.len()
        );
        // Spot-check the three shapes the table must always carry:
        // bare builtin, canonical `std.*`, and a generic signature.
        assert!(rows.contains_key("print"));
        assert!(rows.contains_key("std.str.length"));
        assert_eq!(rows["std.vec.push"].generics, vec!["T"]);
    }
}
