use std::collections::HashMap;

use zz_checker::check_program;
use zz_frontend::parse;
use zz_runtime::{Interp, Value};

use super::{
    canonical_module, register_module_namespace, register_wildcard_namespace, stdlib_funcs,
    stdlib_natives, STDLIB_MODULES,
};

pub(super) fn run(src: &str) -> Result<Value, String> {
    let parsed = parse(src);
    if !parsed.errors.is_empty() {
        return Err(format!("parse errors: {:?}", parsed.errors));
    }

    let mut funcs = stdlib_funcs();
    let mut natives = stdlib_natives();
    for module in ["str", "vec", "json", "fs", "env", "math", "time"] {
        let _ = register_module_namespace(module, module, &mut funcs, &mut natives)
            .expect("known module");
    }

    let checked = check_program(
        &parsed.program,
        HashMap::new(),
        funcs,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let has_errors = checked
        .errors
        .iter()
        .any(|e| e.severity == zz_frontend::diag::Severity::Error);
    if has_errors {
        return Err(format!("check errors: {:?}", checked.errors));
    }

    let mut interp = Interp::with_natives(natives);
    interp
        .run(&parsed.program)
        .map_err(|e| e.message.to_string())
}

mod control_flow;
mod conversions;
mod env;
mod fs;
mod functions;
mod math;
mod methods;
mod operators;
mod pipeline;
mod time;
mod typeof_tests;
mod types;

/// The per-module member cache (#233) must copy exactly what the old
/// full-table scan copied: same keys, same values, for every module —
/// including the `db`→`sqlz` alias and the `sqlz`/`sqlz.postgres` split.
#[test]
fn module_namespace_matches_full_scan_oracle() {
    use super::{stdlib_consts_cached, stdlib_funcs_cached, stdlib_natives_cached};
    use std::collections::HashSet;
    let std_funcs = stdlib_funcs_cached();
    let std_natives = stdlib_natives_cached();
    let std_consts = stdlib_consts_cached();
    for module in STDLIB_MODULES {
        let canon = canonical_module(module);
        let prefix = format!("std.{canon}.");
        // Oracle: the old scan rules (funcs/natives skip dotted rests;
        // the full-module const copy had no dot filter).
        let mut want_funcs: HashSet<String> = HashSet::new();
        for k in std_funcs.keys() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.contains('.') {
                    want_funcs.insert(format!("ns.{rest}"));
                }
            }
        }
        for k in std_consts.keys() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                want_funcs.insert(format!("ns.{rest}"));
            }
        }
        let mut want_natives: HashSet<String> = HashSet::new();
        for k in std_natives.keys() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.contains('.') {
                    want_natives.insert(format!("ns.{rest}"));
                }
            }
        }
        let mut funcs = HashMap::new();
        let mut natives = super::stdlib_natives();
        let inserted = register_module_namespace(module, "ns", &mut funcs, &mut natives)
            .unwrap_or_else(|e| panic!("known module {module}: {e}"));
        let got_funcs: HashSet<String> = funcs.keys().cloned().collect();
        let got_natives: HashSet<String> = natives
            .keys()
            .filter(|k| k.starts_with("ns."))
            .cloned()
            .collect();
        assert_eq!(got_funcs, want_funcs, "func keys for std.{module}");
        assert_eq!(got_natives, want_natives, "native keys for std.{module}");
        let got_inserted: HashSet<String> = inserted.into_iter().collect();
        assert_eq!(got_inserted, want_funcs, "inserted set for std.{module}");
        // Values agree with the source tables (catches mis-attribution
        // like `sqlz.postgres.*` landing under `sqlz`).
        for (key, sig) in &funcs {
            let short = key.strip_prefix("ns.").unwrap();
            let src = format!("{prefix}{short}");
            if let Some(want) = std_funcs.get(&src) {
                assert_eq!(
                    format!("{sig:?}"),
                    format!("{want:?}"),
                    "sig mismatch for {key}"
                );
            } else {
                assert!(
                    std_consts.contains_key(&src),
                    "{key} is neither a func nor a const of std.{module}"
                );
            }
        }
    }
}

/// Same oracle for wildcard imports (bare names, dot filter everywhere).
#[test]
fn wildcard_namespace_matches_full_scan_oracle() {
    use super::{stdlib_funcs_cached, stdlib_natives_cached};
    use std::collections::HashSet;
    let std_funcs = stdlib_funcs_cached();
    let std_natives = stdlib_natives_cached();
    for module in STDLIB_MODULES {
        let canon = canonical_module(module);
        let prefix = format!("std.{canon}.");
        let mut want: HashSet<String> = HashSet::new();
        for k in std_funcs.keys().chain(std_natives.keys()) {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.contains('.') {
                    want.insert(rest.to_string());
                }
            }
        }
        // Consts: wildcard scan had the dot filter (unlike full-module).
        for k in super::stdlib_consts_cached().keys() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.contains('.') {
                    want.insert(rest.to_string());
                }
            }
        }
        let mut funcs = HashMap::new();
        let mut natives = super::stdlib_natives();
        let inserted = register_wildcard_namespace(module, &mut funcs, &mut natives)
            .unwrap_or_else(|e| panic!("known module {module}: {e}"));
        let got: HashSet<String> = inserted.into_iter().collect();
        assert_eq!(got, want, "wildcard set for std.{module}");
    }
}
