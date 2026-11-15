//! Regenerate derived files from the single declarative stdlib table.
//!
//! Usage:
//!   cargo run -p zz_stdlib_meta --bin regen            # write files
//!   cargo run -p zz_stdlib_meta --bin regen -- --check # CI gate (exit 1 on drift)
//!
//! The declarative source of truth is `zz_stdlib_meta` (`table`, `c_impl`,
//! `ffi_impl`, `inventory`). This binary only renders thin shims and the
//! inventory report; no proc macro and no `build.rs`.

use std::fs;
use std::path::PathBuf;

/// Generated `crates/zz_stdlib/src/funcs.rs`.
///
/// Kept byte-exact so `regen --check` is a reliable drift gate. Any change
/// here must be mirrored by running the generator.
const FUNCS_RS: &str = r##"//! Standard library type signatures, consumed by the checker.
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
"##;

/// Generated `crates/zz_codegen_ffi/src/generated.rs`.
const GENERATED_FFI_RS: &str = r##"//! GENERATED FILE - do not edit by hand.
//!
//! Thin shims over the single declarative source in `zz_stdlib_meta`.
//! Regenerate with `cargo run -p zz_stdlib_meta --bin regen`.

/// Map a zz native qualified name to its C runtime implementation name.
pub fn native_impl(name: &str) -> Option<&'static str> {
    zz_stdlib_meta::native_impl(name)
}

/// Map a zz native qualified name to its Rust-staticlib C symbol.
pub fn ffi_impl(name: &str) -> Option<&'static str> {
    zz_stdlib_meta::ffi_impl(name)
}

/// C declaration for a staticlib symbol.
pub fn ffi_decl(symbol: &str) -> Option<&'static str> {
    zz_stdlib_meta::ffi_decl(symbol)
}
"##;

/// `(path relative to the workspace root, generated contents)`.
fn derived() -> Vec<(&'static str, String)> {
    vec![
        ("crates/zz_stdlib/src/funcs.rs", FUNCS_RS.to_string()),
        (
            "crates/zz_codegen_ffi/src/generated.rs",
            GENERATED_FFI_RS.to_string(),
        ),
        (
            "crates/zz_stdlib_meta/src/inventory.md",
            zz_stdlib_meta::inventory::markdown(),
        ),
    ]
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/crates/zz_stdlib_meta
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root is two levels above the crate manifest")
        .to_path_buf()
}

fn main() {
    let check = std::env::args().skip(1).any(|arg| arg == "--check");
    let root = workspace_root();
    let mut drift = false;

    for (rel, contents) in derived() {
        let path = root.join(rel);
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current == contents {
            continue;
        }
        if check {
            eprintln!(
                "regen: {} is out of date with the zz_stdlib_meta table",
                path.display()
            );
            drift = true;
        } else {
            fs::write(&path, &contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            eprintln!("regen: wrote {}", path.display());
        }
    }

    if drift {
        eprintln!("regen: run `cargo run -p zz_stdlib_meta --bin regen` and commit the result");
        std::process::exit(1);
    }
}
