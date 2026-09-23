//! ZZ standard library (Phase 2).
//!
//! Two registries, kept in lockstep:
//! - [`stdlib_funcs`]: type signatures consumed by the checker.
//! - [`stdlib_natives`]: Rust implementations consumed by the interpreter.
//!
//! Modules:
//! - console I/O is builtin — `print`, `println`, `input` (no import, no `std.io`)
//! - `std.str`  — `length`, `split`, `contains`
//! - `std.vec`  — `push`, `pop`, `len`
//! - `std.json` — `parse`, `stringify`, `get`, `as_str`, `as_int`, `as_float`, `as_bool`
//! - `std.http` — `server`, `get`, `post`, `handle`, `listen`

pub mod funcs;
pub mod natives;
pub mod zz_std;

pub use funcs::stdlib_funcs;
pub use natives::stdlib_natives;
pub use zz_std::{define_canonical_purezz_aliases, zz_stdlib_programs};

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
/// `sqlz` is the canonical SQLite module; `db` is a zero-overhead alias
/// pointing directly at `std.sqlz` (see [`canonical_module`]).
/// `sqlz.postgres` is the nested PostgreSQL wire-protocol submodule and
/// `sqlz.mysql` the nested MySQL wire-protocol submodule
/// (dotted keys; see the loader's multi-component handling).
pub const STDLIB_MODULES: &[&str] = &[
    "str",
    "vec",
    "json",
    "http",
    "fs",
    "env",
    "math",
    "time",
    "encoding",
    "net",
    "chan",
    "task",
    "regexp",
    "crypto",
    "log",
    "sys",
    "args",
    "process",
    "uuid",
    "sqlz",
    "db",
    "sqlz.postgres",
    "sqlz.mysql",
    "colors",
    "test",
];

/// Resolve a module name to its canonical backing module.
/// Currently `db` is an alias for the canonical `sqlz` module; every
/// other module is its own canonical name.
pub fn canonical_module(module: &str) -> &str {
    match module {
        "db" => "sqlz",
        _ => module,
    }
}

/// Register a `std.*` module under a namespace name by copying its entries
/// from the `std.<module>.*` keys to `<ns>.*` keys in both registries.
///
/// Used by the loader and the REPL session so that `import std.str` makes
/// `str.length` (and friends) available. Returns an error message if the
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
        extern_c_symbol: None,
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
    // `std.db` is a zero-overhead alias: importing it copies the canonical
    // `std.sqlz.*` entries (identical sigs + fn pointers, no extra layer).
    let source = canonical_module(module);
    let prefix = format!("std.{source}.");
    let std_funcs = stdlib_funcs();
    let std_natives = stdlib_natives();
    for (k, v) in std_funcs {
        // Direct members only: `import std.sqlz` must not leak the nested
        // `std.sqlz.postgres.*` keys (those belong to `sqlz.postgres`).
        if let Some(rest) = k.strip_prefix(&prefix) {
            if !rest.contains('.') {
                funcs.insert(format!("{ns}.{rest}"), v);
            }
        }
    }
    for (k, v) in std_natives {
        if let Some(rest) = k.strip_prefix(&prefix) {
            if !rest.contains('.') {
                natives.insert(format!("{ns}.{rest}"), v);
            }
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
    // Alias: selective `import std.db(open)` resolves against `std.sqlz.*`.
    let prefix = format!("std.{}.", canonical_module(module));
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
    // Alias: wildcard `import std.db(*)` resolves against `std.sqlz.*`.
    let prefix = format!("std.{}.", canonical_module(module));
    let std_funcs = stdlib_funcs();
    let std_natives = stdlib_natives();
    let std_consts = stdlib_consts();
    for (k, v) in std_funcs {
        // Direct members only (see `register_module_namespace`).
        if let Some(rest) = k.strip_prefix(&prefix) {
            if !rest.contains('.') {
                funcs.insert(rest.to_string(), v);
            }
        }
    }
    for (k, v) in std_natives {
        if let Some(rest) = k.strip_prefix(&prefix) {
            if !rest.contains('.') {
                natives.insert(rest.to_string(), v);
            }
        }
    }
    for k in std_consts.keys() {
        if let Some(rest) = k.strip_prefix(&prefix) {
            if !rest.contains('.') {
                funcs.insert(rest.to_string(), const_sig(zz_checker::Type::Float));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
