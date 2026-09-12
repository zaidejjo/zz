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
pub mod zz_std;

pub use funcs::stdlib_funcs;
pub use natives::stdlib_natives;
pub use zz_std::zz_stdlib_programs;

/// Math constants registered as static float values (not zero-arg functions).
/// Keys are fully-qualified names like `"std.math.PI"`.
pub fn stdlib_consts() -> std::collections::HashMap<String, f64> {
    let mut m = std::collections::HashMap::new();
    // std.math constants
    m.insert("std.math.PI".into(), std::f64::consts::PI);
    m.insert("std.math.E".into(), std::f64::consts::E);
    m.insert("std.math.TAU".into(), std::f64::consts::TAU);
    m.insert("std.math.SQRT_2".into(), std::f64::consts::SQRT_2);
    m.insert("std.math.SQRT_1_2".into(), std::f64::consts::FRAC_1_SQRT_2);
    m.insert("std.math.LN_2".into(), std::f64::consts::LN_2);
    m.insert("std.math.LN_10".into(), std::f64::consts::LN_10);
    m.insert("std.math.LOG10_E".into(), std::f64::consts::LOG10_E);
    m.insert("std.math.LOG2_E".into(), std::f64::consts::LOG2_E);
    m.insert("std.math.INF".into(), f64::INFINITY);
    m.insert("std.math.NAN".into(), f64::NAN);
    m
}

/// The set of known `std.*` module names (second path component).
pub const STDLIB_MODULES: &[&str] = &[
    "io", "str", "vec", "json", "http", "fs", "env", "math", "time", "encoding", "net", "chan",
    "task",
];

/// Register a `std.*` module under a namespace name by copying its entries
/// from the `std.<module>.*` keys to `<ns>.*` keys in both registries.
///
/// Used by the loader and the REPL session so that `import std.io` makes
/// `io.println` (and friends) available. Returns an error message if the
/// module is unknown.
/// Build a non-generic zero-arg FuncSig (for constants).
fn const_sig(ret: zz_checker::Type) -> zz_checker::FuncSig {
    zz_checker::FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: Vec::new(),
        has_default: vec![],
        ret,
        is_extern: false,
    }
}

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
    // Also copy static constants.
    let std_consts = stdlib_consts();
    for k in std_consts.keys() {
        if let Some(rest) = k.strip_prefix(&prefix) {
            funcs.insert(format!("{ns}.{rest}"), const_sig(zz_checker::Type::Float));
        }
    }
    Ok(())
}

/// Register specific symbols from a `std.*` module directly into the current
/// namespace (no module prefix). Used for selective imports like
/// `import std.math(PI, sin)`.
///
/// `items` is a list of `(original_name, alias)`. If `alias` is `Some`, the
/// symbol is registered under the alias name; otherwise the original name is
/// used.
///
/// Returns a list of symbol names that were not found in the module.
pub fn register_selective_namespace(
    module: &str,
    items: &[(String, Option<String>)],
    funcs: &mut std::collections::HashMap<String, zz_checker::FuncSig>,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
) -> Result<Vec<String>, String> {
    if !STDLIB_MODULES.contains(&module) {
        return Err(format!("unknown stdlib module `std.{module}`"));
    }
    let prefix = format!("std.{module}.");
    let std_funcs = stdlib_funcs();
    let std_natives = stdlib_natives();
    let std_consts = stdlib_consts();
    let mut missing = Vec::new();
    for (name, alias) in items {
        let target = alias.as_ref().unwrap_or(name);
        let key = format!("{prefix}{name}");
        let mut found = false;
        if let Some(sig) = std_funcs.get(&key) {
            funcs.insert(target.clone(), sig.clone());
            found = true;
        }
        if let Some(entry) = std_natives.get(&key) {
            natives.insert(target.clone(), *entry);
            found = true;
        }
        if std_consts.contains_key(&key) {
            funcs.insert(target.clone(), const_sig(zz_checker::Type::Float));
            found = true;
        }
        if !found {
            missing.push(name.clone());
        }
    }
    Ok(missing)
}

/// Register all symbols from a `std.*` module directly into the current
/// namespace (no module prefix). Used for wildcard imports like
/// `import std.math(*)`.
pub fn register_wildcard_namespace(
    module: &str,
    funcs: &mut std::collections::HashMap<String, zz_checker::FuncSig>,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
) -> Result<(), String> {
    if !STDLIB_MODULES.contains(&module) {
        return Err(format!("unknown stdlib module `std.{module}`"));
    }
    let prefix = format!("std.{module}.");
    let std_funcs = stdlib_funcs();
    let std_natives = stdlib_natives();
    let std_consts = stdlib_consts();
    for (k, v) in std_funcs {
        if let Some(rest) = k.strip_prefix(&prefix) {
            funcs.insert(rest.to_string(), v);
        }
    }
    for (k, v) in std_natives {
        if let Some(rest) = k.strip_prefix(&prefix) {
            natives.insert(rest.to_string(), v);
        }
    }
    for k in std_consts.keys() {
        if let Some(rest) = k.strip_prefix(&prefix) {
            funcs.insert(rest.to_string(), const_sig(zz_checker::Type::Float));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
