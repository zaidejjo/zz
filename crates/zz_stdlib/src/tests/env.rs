use std::collections::HashMap;

use super::run;
use super::{register_module_namespace, stdlib_funcs, stdlib_natives};
use zz_checker::check_program;
use zz_frontend::parse;
use zz_runtime::{Interp, Value};

#[test]
fn env_get_var_found() {
    let v = run("import std.env\nenv.get_var(\"PATH\")").unwrap();
    match v {
        Value::Option(Some(s)) => assert!(!s.to_string().is_empty()),
        other => panic!("expected some str, got {other}"),
    }
}

#[test]
fn env_get_var_missing_is_none() {
    let v = run("import std.env\nenv.get_var(\"ZZ_NO_SUCH_VAR_12345\")").unwrap();
    assert_eq!(v, Value::Option(None));
}

#[test]
fn env_args_returns_script_args() {
    let parsed = parse("import std.env\nenv.args()");
    assert!(parsed.errors.is_empty());
    let mut funcs = stdlib_funcs();
    let mut natives = stdlib_natives();
    register_module_namespace("env", "env", &mut funcs, &mut natives).expect("known module");
    let checked = check_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
    let has_errors = checked
        .errors
        .iter()
        .any(|e| e.severity == zz_frontend::diag::Severity::Error);
    assert!(!has_errors, "check errors: {:?}", checked.errors);

    let mut interp = Interp::with_natives(natives);
    interp.args = vec!["one".to_string(), "two".to_string()];
    let v = interp.run(&parsed.program).unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Str("one".to_string()),
            Value::Str("two".to_string())
        ])
    );
}
