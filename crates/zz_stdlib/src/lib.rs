//! ZZ standard library (Phase 2).
//!
//! Two registries, kept in lockstep:
//! - [`stdlib_funcs`]: type signatures consumed by the checker.
//! - [`stdlib_natives`]: Rust implementations consumed by the interpreter.
//!
//! Modules:
//! - `std.io`   — `printz`, `println`, `read_line`
//! - `std.str`  — `length`, `split`, `contains`
//! - `std.vec`  — `push`, `pop`, `len`
//! - `std.json` — `parse`, `stringify`, `get`, `as_str`, `as_int`, `as_float`, `as_bool`
//! - `std.http` — `server`, `get`, `post`, `handle`, `listen`

pub mod funcs;
pub mod natives;

pub use funcs::stdlib_funcs;
pub use natives::stdlib_natives;

/// The set of known `std.*` module names (second path component).
pub const STDLIB_MODULES: &[&str] = &[
    "io", "str", "vec", "json", "http", "fs", "env", "math", "time",
];

/// Register a `std.*` module under a namespace name by copying its entries
/// from the `std.<module>.*` keys to `<ns>.*` keys in both registries.
///
/// Used by the loader and the REPL session so that `import std.io` makes
/// `io.println` (and friends) available. Returns an error message if the
/// module is unknown.
pub fn register_module_namespace(
    module: &str,
    ns: &str,
    funcs: &mut std::collections::HashMap<String, zz_checker::FuncSig>,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
) -> Result<(), String> {
    if !STDLIB_MODULES.contains(&module) {
        return Err(format!("unknown stdlib module `std.{module}`"));
    }
    let prefix = format!("std.{module}.");
    let std_funcs = stdlib_funcs();
    let std_natives = stdlib_natives();
    for (k, v) in std_funcs {
        if let Some(rest) = k.strip_prefix(&prefix) {
            funcs.insert(format!("{ns}.{rest}"), v);
        }
    }
    for (k, v) in std_natives {
        if let Some(rest) = k.strip_prefix(&prefix) {
            natives.insert(format!("{ns}.{rest}"), v);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
