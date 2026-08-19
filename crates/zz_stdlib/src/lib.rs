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
mod tests {
    use std::collections::HashMap;

    use zz_checker::check_program;
    use zz_frontend::parse;
    use zz_runtime::{Interp, Value};

    use super::{register_module_namespace, stdlib_funcs, stdlib_natives};

    /// Parse, type-check and run a source snippet with the full stdlib
    /// available under its namespace names (like the loader does).
    fn run(src: &str) -> Result<Value, String> {
        let parsed = parse(src);
        if !parsed.errors.is_empty() {
            return Err(format!("parse errors: {:?}", parsed.errors));
        }

        let mut funcs = stdlib_funcs();
        let mut natives = stdlib_natives();
        for module in ["io", "str", "vec", "json", "fs", "env", "math", "time"] {
            register_module_namespace(module, module, &mut funcs, &mut natives)
                .expect("known module");
        }

        let checked = check_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
        if !checked.errors.is_empty() {
            return Err(format!("check errors: {:?}", checked.errors));
        }

        let mut interp = Interp::with_natives(natives);
        interp
            .run(&parsed.program)
            .map_err(|e| e.message.to_string())
    }

    // --- typeof -------------------------------------------------------------

    #[test]
    fn typeof_builtin() {
        assert_eq!(run("typeof(1)").unwrap(), Value::Str("int".to_string()));
        assert_eq!(run("typeof(1.5)").unwrap(), Value::Str("float".to_string()));
        assert_eq!(run("typeof(\"x\")").unwrap(), Value::Str("str".to_string()));
        assert_eq!(run("typeof(true)").unwrap(), Value::Str("bool".to_string()));
        assert_eq!(
            run("typeof([1, 2])").unwrap(),
            Value::Str("array".to_string())
        );
        assert_eq!(
            run("typeof({\"a\": 1})").unwrap(),
            Value::Str("dict".to_string())
        );
        assert_eq!(
            run("typeof(.some(1))").unwrap(),
            Value::Str("option".to_string())
        );
        assert_eq!(
            run("typeof(.ok(1))").unwrap(),
            Value::Str("result".to_string())
        );
        assert_eq!(
            run("typeof(0..5)").unwrap(),
            Value::Str("range".to_string())
        );
    }

    #[test]
    fn typeof_struct_reports_name() {
        let v = run("struct Point { x: int }\np := Point{ x: 1 }\ntypeof(p)").unwrap();
        assert_eq!(v, Value::Str("Point".to_string()));
    }

    // --- pipeline with stdlib ----------------------------------------------

    #[test]
    fn pipe_with_stdlib() {
        let v = run("import std.str\nimport std.vec\n\"a,b,c\" |> str.split(\",\") |> vec.len()")
            .unwrap();
        assert_eq!(v, Value::Int(3));
    }

    // --- std.fs -------------------------------------------------------------

    #[test]
    fn fs_write_and_read_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("zz_stdlib_test_{}.txt", std::process::id()));
        let path_str = path.to_string_lossy().to_string();

        let v = run(&format!(
            "import std.fs\nfs.write_file(\"{path_str}\", \"hello fs\")"
        ))
        .unwrap();
        assert_eq!(v, Value::Result(Ok(Box::new(Value::Unit))));

        let v = run(&format!("import std.fs\nfs.read_file(\"{path_str}\")")).unwrap();
        assert_eq!(
            v,
            Value::Result(Ok(Box::new(Value::Str("hello fs".to_string()))))
        );

        let v = run(&format!("import std.fs\nfs.exists(\"{path_str}\")")).unwrap();
        assert_eq!(v, Value::Bool(true));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fs_read_missing_file_is_err() {
        let v = run("import std.fs\nfs.read_file(\"/tmp/zz_no_such_file_zz\")").unwrap();
        match v {
            Value::Result(Err(e)) => {
                assert!(
                    e.to_string().contains("No such file"),
                    "unexpected error: {e}"
                );
            }
            other => panic!("expected err result, got {other}"),
        }
    }

    #[test]
    fn fs_exists_missing_is_false() {
        let v = run("import std.fs\nfs.exists(\"/tmp/zz_no_such_file_zz\")").unwrap();
        assert_eq!(v, Value::Bool(false));
    }

    // --- std.env ------------------------------------------------------------

    #[test]
    fn env_get_var_found() {
        // PATH is set in essentially every environment.
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
        assert!(
            checked.errors.is_empty(),
            "check errors: {:?}",
            checked.errors
        );

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

    // --- conversions --------------------------------------------------------

    #[test]
    fn conv_str() {
        assert_eq!(run("str(42)").unwrap(), Value::Str("42".to_string()));
        assert_eq!(run("str(3.5)").unwrap(), Value::Str("3.5".to_string()));
        assert_eq!(run("str(true)").unwrap(), Value::Str("true".to_string()));
        assert_eq!(
            run("str([1, 2])").unwrap(),
            Value::Str("[1, 2]".to_string())
        );
        assert_eq!(
            run("str({\"a\": 1})").unwrap(),
            Value::Str("{a: 1}".to_string())
        );
    }

    #[test]
    fn conv_int() {
        assert_eq!(
            run("int(\"42\")").unwrap(),
            Value::Option(Some(Box::new(Value::Int(42))))
        );
        assert_eq!(
            run("int(\" 7 \")").unwrap(),
            Value::Option(Some(Box::new(Value::Int(7))))
        );
        assert_eq!(run("int(\"abc\")").unwrap(), Value::Option(None));
        assert_eq!(
            run("int(3.7)").unwrap(),
            Value::Option(Some(Box::new(Value::Int(3))))
        );
        assert_eq!(
            run("int(5)").unwrap(),
            Value::Option(Some(Box::new(Value::Int(5))))
        );
        assert_eq!(run("int(true)").unwrap(), Value::Option(None));
    }

    #[test]
    fn conv_float() {
        assert_eq!(
            run("float(\"2.5\")").unwrap(),
            Value::Option(Some(Box::new(Value::Float(2.5))))
        );
        assert_eq!(run("float(\"x\")").unwrap(), Value::Option(None));
        assert_eq!(
            run("float(3)").unwrap(),
            Value::Option(Some(Box::new(Value::Float(3.0))))
        );
        assert_eq!(
            run("float(1.5)").unwrap(),
            Value::Option(Some(Box::new(Value::Float(1.5))))
        );
    }

    // --- std.math -----------------------------------------------------------

    #[test]
    fn math_abs() {
        assert_eq!(run("import std.math\nmath.abs(-5)").unwrap(), Value::Int(5));
        assert_eq!(
            run("import std.math\nmath.abs(-5.5)").unwrap(),
            Value::Float(5.5)
        );
        assert_eq!(run("import std.math\nmath.abs(3)").unwrap(), Value::Int(3));
    }

    #[test]
    fn math_floor_ceil() {
        assert_eq!(
            run("import std.math\nmath.floor(3.7)").unwrap(),
            Value::Int(3)
        );
        assert_eq!(
            run("import std.math\nmath.floor(-3.2)").unwrap(),
            Value::Int(-4)
        );
        assert_eq!(
            run("import std.math\nmath.ceil(3.2)").unwrap(),
            Value::Int(4)
        );
        assert_eq!(
            run("import std.math\nmath.ceil(-3.7)").unwrap(),
            Value::Int(-3)
        );
    }

    #[test]
    fn math_sqrt_pow() {
        assert_eq!(
            run("import std.math\nmath.sqrt(9)").unwrap(),
            Value::Float(3.0)
        );
        assert_eq!(
            run("import std.math\nmath.sqrt(2.25)").unwrap(),
            Value::Float(1.5)
        );
        assert_eq!(
            run("import std.math\nmath.pow(2, 10)").unwrap(),
            Value::Float(1024.0)
        );
        assert_eq!(
            run("import std.math\nmath.pow(2.0, 3.0)").unwrap(),
            Value::Float(8.0)
        );
    }

    #[test]
    fn math_random_in_range() {
        let v = run("import std.math\nmath.random()").unwrap();
        match v {
            Value::Float(f) => assert!((0.0..1.0).contains(&f), "random out of range: {f}"),
            other => panic!("expected float, got {other}"),
        }
    }

    // --- std.time -----------------------------------------------------------

    #[test]
    fn time_now_ms() {
        let v = run("import std.time\ntime.now_ms()").unwrap();
        match v {
            Value::Int(ms) => assert!(ms > 0, "now_ms should be positive: {ms}"),
            other => panic!("expected int, got {other}"),
        }
    }

    #[test]
    fn time_sleep_ms() {
        let start = std::time::Instant::now();
        let v = run("import std.time\ntime.sleep_ms(20)").unwrap();
        assert_eq!(v, Value::Unit);
        assert!(
            start.elapsed().as_millis() >= 15,
            "sleep returned too early: {:?}",
            start.elapsed()
        );
    }
}
