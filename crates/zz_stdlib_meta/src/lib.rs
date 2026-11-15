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

pub mod c_impl;
pub mod ffi_impl;
pub mod inventory;
pub mod table;

pub use c_impl::native_impl;
pub use ffi_impl::{ffi_decl, ffi_impl};

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

    /// The inventory is the committed tier classification. Every signature
    /// must get exactly one tier; a missing entry means a new stdlib function
    /// landed without saying where (and how) it is implemented.
    #[test]
    fn inventory_covers_every_signature() {
        let mut missing: Vec<String> = crate::build()
            .keys()
            .filter(|name| crate::inventory::tier_of(name).is_none())
            .cloned()
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "unclassified stdlib entries (add to inventory): {missing:?}"
        );
    }
}
