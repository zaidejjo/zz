//! ZZ standard library (Phase 2).
//!
//! Two registries, kept in lockstep:
//! - [`stdlib_funcs`]: type signatures consumed by the checker.
//! - [`stdlib_natives`]: Rust implementations consumed by the interpreter.
//!
//! Modules:
//! - console I/O is builtin — `print`, `println`, `input` (no import)
//! - `std.str`  — `length`, `split`, `contains`
//! - `std.vec`  — `push`, `pop`, `len`
//! - `std.json` — `parse`, `stringify`, `get`, `as_str`, `as_int`, `as_float`, `as_bool`
//! - `std.http` — `server`, `get`, `post`, `handle`, `listen`

pub mod funcs;
pub mod natives;
pub mod zz_std;

pub use funcs::{stdlib_funcs, stdlib_funcs_cached};
pub use natives::{stdlib_natives, stdlib_natives_cached};
pub use zz_std::{
    define_canonical_purezz_aliases, stdlib_program_closure, zz_stdlib_program_at,
    zz_stdlib_program_count, zz_stdlib_programs,
};

/// Math constants registered as static float values (not zero-arg functions).
/// Keys are fully-qualified names like `"std.math.PI"`.
///
/// Cached process-wide; [`stdlib_consts`] clones, hot paths borrow
/// [`stdlib_consts_cached`] directly.
pub fn stdlib_consts() -> std::collections::HashMap<String, f64> {
    stdlib_consts_cached().clone()
}

/// Borrow the process-wide cached constants table.
pub fn stdlib_consts_cached() -> &'static std::collections::HashMap<String, f64> {
    static CACHED: std::sync::OnceLock<std::collections::HashMap<String, f64>> =
        std::sync::OnceLock::new();
    CACHED.get_or_init(build_stdlib_consts)
}

/// Uncached constructor (runs once via [`stdlib_consts_cached`]).
fn build_stdlib_consts() -> std::collections::HashMap<String, f64> {
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
/// Single-sourced from `zz_checker::STD_MODULES` — see there for the
/// contract. Nested modules (`sqlz.postgres`) included; `sqlz` is the
/// only SQLite module. Nested `postgres`/`mysql` are reachable only via
/// `import std.sqlz.postgres` / `import std.sqlz.mysql` (optionally
/// `as pg` / `as my`); bare `pg.*` / `my.*` do not resolve.
pub use zz_checker::STD_MODULES as STDLIB_MODULES;

/// Per-module direct members, precomputed once per process (#233).
/// [`register_module_namespace`] used to scan the ENTIRE std func/native
/// tables per import (O(all symbols) per importer); with ~1k seed entries
/// and N importers that is O(N × seed) prefix checks. The table below
/// narrows each import to its own module's members (typically 3–30).
struct ModuleMembers {
    funcs: Vec<(String, zz_checker::FuncSig)>,
    natives: Vec<(String, zz_runtime::NativeEntry)>,
    consts: Vec<String>,
}

/// Short names in `members` are bare (`length`, not `str.length`).
fn module_members_table() -> &'static std::collections::HashMap<String, ModuleMembers> {
    static MEMBERS: std::sync::OnceLock<std::collections::HashMap<String, ModuleMembers>> =
        std::sync::OnceLock::new();
    MEMBERS.get_or_init(|| {
        let std_funcs = crate::funcs::stdlib_funcs_cached();
        let std_natives = crate::natives::stdlib_natives_cached();
        let std_consts = stdlib_consts_cached();
        let mut table: std::collections::HashMap<String, ModuleMembers> =
            std::collections::HashMap::new();
        // Direct members only (mirrors the old full-scan filter):
        // `import std.sqlz` must not leak nested `std.sqlz.postgres.*` keys.
        for (k, v) in std_funcs {
            if let Some((module, rest)) = split_std_key(k) {
                if !rest.contains('.') {
                    members_entry(&mut table, module)
                        .funcs
                        .push((rest.to_string(), v.clone()));
                }
            }
        }
        for (k, v) in std_natives {
            if let Some((module, rest)) = split_std_key(k) {
                if !rest.contains('.') {
                    members_entry(&mut table, module)
                        .natives
                        .push((rest.to_string(), *v));
                }
            }
        }
        for k in std_consts.keys() {
            if let Some((module, rest)) = split_std_key(k) {
                // The old full-module copy had no dot filter for consts;
                // all shipped consts are single-level (`std.math.PI`), so
                // the filter is a no-op today and keeps nested consts
                // with their own submodule like funcs/natives.
                if !rest.contains('.') {
                    members_entry(&mut table, module)
                        .consts
                        .push(rest.to_string());
                }
            }
        }
        table
    })
}

/// Vacant-or-present entry for the member table build.
fn members_entry<'a>(
    table: &'a mut std::collections::HashMap<String, ModuleMembers>,
    module: &str,
) -> &'a mut ModuleMembers {
    table
        .entry(module.to_string())
        .or_insert_with(|| ModuleMembers {
            funcs: Vec::new(),
            natives: Vec::new(),
            consts: Vec::new(),
        })
}

/// Split a `std.<module>.<rest>` key (module may itself be dotted, e.g.
/// `sqlz.postgres`) into (canonical module, rest). Non-std keys (bare
/// builtins like `print`, method-dispatch aliases like `vec.push`)
/// return `None`.
fn split_std_key(key: &str) -> Option<(&str, &str)> {
    let without_std = key.strip_prefix("std.")?;
    // Longest module match first: `sqlz.postgres.x` belongs to
    // `sqlz.postgres`, not `sqlz`.
    let mut best: Option<(&str, &str)> = None;
    for module in STDLIB_MODULES {
        let canonical = module;
        if let Some(rest) = without_std.strip_prefix(canonical) {
            if let Some(rest) = rest.strip_prefix('.') {
                if rest.is_empty() {
                    continue;
                }
                match best {
                    Some((prev, _)) if prev.len() >= canonical.len() => {}
                    _ => best = Some((canonical, rest)),
                }
            }
        }
    }
    best
}

/// Register a `std.*` module under a namespace name by copying its entries
/// from the `std.<module>.*` keys to `<ns>.*` keys in both registries.
///
/// Used by the loader and the REPL session so that `import std.str` makes
/// `str.length` (and friends) available. Returns an error message if the
/// module is unknown. On success returns the inserted `<ns>.*` keys, so
/// the loader can mark them seed-owned (per-module check partitioning
/// restores seed keys instead of consuming them — issue #214).
/// Build a non-generic zero-arg FuncSig (for constants).
fn const_sig(ret: zz_checker::Type) -> zz_checker::FuncSig {
    zz_checker::FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: Vec::new(),
        has_default: vec![],
        ret,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    }
}

pub fn register_module_namespace(
    module: &str,
    ns: &str,
    funcs: &mut std::collections::HashMap<String, zz_checker::FuncSig>,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
) -> Result<Vec<String>, String> {
    if !STDLIB_MODULES.contains(&module) {
        return Err(format!("unknown stdlib module `std.{module}`"));
    }
    // Members are precomputed once per process (#233): per import we walk
    // only this module's members instead of scanning the whole tables.
    let source = module;
    let Some(members) = module_members_table().get(source) else {
        return Ok(Vec::new());
    };
    let mut inserted = Vec::with_capacity(members.funcs.len() + members.consts.len());
    for (short, sig) in &members.funcs {
        let key = format!("{ns}.{short}");
        funcs.insert(key.clone(), sig.clone());
        inserted.push(key);
    }
    for (short, entry) in &members.natives {
        natives.insert(format!("{ns}.{short}"), *entry);
    }
    // Also copy static constants.
    for short in &members.consts {
        let key = format!("{ns}.{short}");
        funcs.insert(key.clone(), const_sig(zz_checker::Type::Float));
        inserted.push(key);
    }
    Ok(inserted)
}

/// Register specific symbols from a `std.*` module directly into the current
/// namespace (no module prefix). Used for selective imports like
/// `import std.math(PI, sin)`.
///
/// `items` is a list of `(original_name, alias)`. If `alias` is `Some`, the
/// symbol is registered under the alias name; otherwise the original name is
/// used.
///
/// Returns `(missing, inserted)`: symbols not found in the module, plus
/// the registered target names (for loader seed-ownership, issue #214).
pub fn register_selective_namespace(
    module: &str,
    items: &[(String, Option<String>)],
    funcs: &mut std::collections::HashMap<String, zz_checker::FuncSig>,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
) -> Result<(Vec<String>, Vec<String>), String> {
    if !STDLIB_MODULES.contains(&module) {
        return Err(format!("unknown stdlib module `std.{module}`"));
    }
    // Borrow cached tables (no per-import rebuild); lookups only.
    let prefix = format!("std.{module}.");
    let std_funcs = crate::funcs::stdlib_funcs_cached();
    let std_natives = crate::natives::stdlib_natives_cached();
    let std_consts = stdlib_consts_cached();
    let mut missing = Vec::new();
    let mut inserted = Vec::new();
    for (name, alias) in items {
        let target = alias.as_ref().unwrap_or(name);
        let key = format!("{prefix}{name}");
        let mut found = false;
        if let Some(sig) = std_funcs.get(&key) {
            funcs.insert(target.clone(), sig.clone());
            inserted.push(target.clone());
            found = true;
        }
        if let Some(entry) = std_natives.get(&key) {
            natives.insert(target.clone(), *entry);
            found = true;
        }
        if std_consts.contains_key(&key) {
            funcs.insert(target.clone(), const_sig(zz_checker::Type::Float));
            if !found {
                inserted.push(target.clone());
            }
            found = true;
        }
        if !found {
            missing.push(name.clone());
        }
    }
    Ok((missing, inserted))
}

/// Register all symbols from a `std.*` module directly into the current
/// namespace (no module prefix). Used for wildcard imports like
/// `import std.math(*)`. Returns the inserted bare names (for loader
/// seed-ownership, issue #214).
pub fn register_wildcard_namespace(
    module: &str,
    funcs: &mut std::collections::HashMap<String, zz_checker::FuncSig>,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
) -> Result<Vec<String>, String> {
    if !STDLIB_MODULES.contains(&module) {
        return Err(format!("unknown stdlib module `std.{module}`"));
    }
    // Members precomputed once per process (#233), same set as the old
    // full-table scan (direct members only — see `register_module_namespace`).
    let Some(members) = module_members_table().get(module) else {
        return Ok(Vec::new());
    };
    let mut inserted = Vec::with_capacity(members.funcs.len() + members.consts.len());
    for (short, sig) in &members.funcs {
        funcs.insert(short.clone(), sig.clone());
        inserted.push(short.clone());
    }
    for (short, entry) in &members.natives {
        natives.insert(short.clone(), *entry);
    }
    for short in &members.consts {
        funcs.insert(short.clone(), const_sig(zz_checker::Type::Float));
        inserted.push(short.clone());
    }
    inserted.sort();
    inserted.dedup();
    Ok(inserted)
}

#[cfg(test)]
mod tests;
