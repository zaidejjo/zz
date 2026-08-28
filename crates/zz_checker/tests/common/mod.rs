//! Shared test helpers for zz_checker integration tests.

use std::collections::HashMap;
use zz_checker::{check_program, CheckResult, FuncSig, StructSig};
use zz_frontend::parse;

pub fn check_src(src: &str) -> CheckResult {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    check_program(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    )
}

pub fn errors_of(src: &str) -> Vec<String> {
    check_src(src)
        .errors
        .iter()
        .filter(|e| e.severity == zz_frontend::diag::Severity::Error)
        .map(|e| e.message.clone())
        .collect()
}

pub fn errors_contain(src: &str, needle: &str) {
    let errs = errors_of(src);
    assert!(
        errs.iter().any(|e| e.contains(needle)),
        "expected error containing `{needle}`, got: {errs:?}"
    );
}

/// True if `CheckResult` has any errors (severity=Error), ignoring warnings.
pub fn has_errors(r: &CheckResult) -> bool {
    r.errors
        .iter()
        .any(|e| e.severity == zz_frontend::diag::Severity::Error)
}

/// Check with a seeded function map (e.g. a generic builtin like `typeof`).
pub fn check_src_with_funcs(src: &str, funcs: HashMap<String, FuncSig>) -> CheckResult {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    check_program(&parsed.program, HashMap::new(), funcs, HashMap::new())
}

/// Check with seeded functions and structs (e.g. a namespaced struct
/// that only exists through module registration).
pub fn check_src_with_funcs_and_structs(
    src: &str,
    funcs: HashMap<String, FuncSig>,
    structs: HashMap<String, StructSig>,
) -> CheckResult {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    check_program(&parsed.program, HashMap::new(), funcs, structs)
}
