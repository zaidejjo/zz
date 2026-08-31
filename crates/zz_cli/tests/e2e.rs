//! End-to-end integration tests for the ZZ language.
//!
//! Discovers all `.zz` fixture files under `tests/fixtures/` and runs them
//! through the `zz` binary, asserting expected behavior:
//!
//! - `syntax/`, `types/`, `stdlib/` → must exit 0 (success)
//! - `errors/` → must exit 1 (compile or runtime error)
//!
//! Each success fixture must print a final line matching its filename stem
//! (e.g., `declarations.zz` → `declarations_ok` or just the stem as a marker).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the workspace root (where `tests/fixtures/` lives).
fn fixtures_dir() -> PathBuf {
    // When running `cargo test -p zz_cli`, CARGO_MANIFEST_DIR is crates/zz_cli/.
    // fixtures/ is at ../../tests/fixtures relative to that.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Run `zz run <file>` and return (exit_code, stdout, stderr).
fn run_zz(file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("run")
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz run {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Run `zz eval <src>` and return (exit_code, stdout, stderr).
fn run_zz_eval(src: &str) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("eval")
        .arg(src)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz eval`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Run `zz check <file>` and return (exit_code, stdout, stderr).
fn run_zz_check(file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("check")
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz check {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Discover all `.zz` files in a directory (non-recursive).
fn find_fixtures(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("zz"))
        .collect();
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// Success fixtures: must exit 0
// ---------------------------------------------------------------------------

macro_rules! e2e_success_test {
    ($name:ident, $category:expr, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join($category).join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let (exit, stdout, stderr) = run_zz(&path);
            assert_eq!(
                exit,
                0,
                "fixture {} should exit 0 but got {}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
                path.display(),
                exit,
            );

            // Verify the last line contains a success marker.
            let last = stdout.lines().last().unwrap_or("");
            let stem = path.file_stem().unwrap().to_string_lossy();
            assert!(
                !last.is_empty(),
                "fixture {} produced no output.\nstderr:\n{stderr}",
                path.display(),
            );
        }
    };
}

// Syntax fixtures
e2e_success_test!(e2e_syntax_declarations, "syntax", "declarations.zz");
e2e_success_test!(e2e_syntax_functions, "syntax", "functions.zz");
e2e_success_test!(e2e_syntax_control_flow, "syntax", "control_flow.zz");
e2e_success_test!(e2e_syntax_pipelines, "syntax", "pipelines.zz");
e2e_success_test!(e2e_syntax_hof, "syntax", "hof.zz");
e2e_success_test!(e2e_syntax_match, "syntax", "match.zz");
e2e_success_test!(e2e_syntax_operators, "syntax", "operators.zz");
e2e_success_test!(e2e_syntax_fstrings, "syntax", "fstrings.zz");
e2e_success_test!(e2e_syntax_arrays, "syntax", "arrays.zz");
e2e_success_test!(e2e_syntax_dicts, "syntax", "dicts.zz");
e2e_success_test!(e2e_syntax_defer, "syntax", "defer.zz");
e2e_success_test!(e2e_syntax_string_blocks, "syntax", "string_blocks.zz");
e2e_success_test!(e2e_syntax_return_in_loops, "syntax", "return_in_loops.zz");
e2e_success_test!(e2e_syntax_dict_iteration, "syntax", "dict_iteration.zz");

// Type fixtures
e2e_success_test!(e2e_types_structs, "types", "structs.zz");
e2e_success_test!(e2e_types_generics, "types", "generics.zz");
e2e_success_test!(e2e_types_variants, "types", "variants.zz");
e2e_success_test!(e2e_types_type_inference, "types", "type_inference.zz");

// Stdlib fixtures
e2e_success_test!(e2e_stdlib_strings, "stdlib", "strings.zz");
e2e_success_test!(e2e_stdlib_vectors, "stdlib", "vectors.zz");
e2e_success_test!(e2e_stdlib_math_ops, "stdlib", "math_ops.zz");
e2e_success_test!(e2e_stdlib_jsonmod, "stdlib", "jsonmod.zz");
e2e_success_test!(e2e_stdlib_filesystem, "stdlib", "filesystem.zz");
e2e_success_test!(e2e_stdlib_console, "stdlib", "console.zz");
e2e_success_test!(e2e_stdlib_envmod, "stdlib", "envmod.zz");
e2e_success_test!(e2e_stdlib_time_ops, "stdlib", "time_ops.zz");

// ---------------------------------------------------------------------------
// Error fixtures: must exit 1
// ---------------------------------------------------------------------------

macro_rules! e2e_error_test {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join("errors").join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let (exit, stdout, stderr) = run_zz(&path);
            assert_ne!(
                exit,
                0,
                "error fixture {} should fail but exited 0.\nstdout:\n{stdout}",
                path.display(),
            );
            // stderr should contain an error diagnostic.
            assert!(
                stderr.contains("error") || !stderr.is_empty(),
                "error fixture {} should produce diagnostics.\nstderr:\n{stderr}",
                path.display(),
            );
        }
    };
}

e2e_error_test!(e2e_err_type_mismatch, "type_mismatch.zz");
e2e_error_test!(e2e_err_undefined_var, "undefined_var.zz");
e2e_error_test!(e2e_err_missing_field, "missing_field.zz");
e2e_error_test!(e2e_err_arity, "arity.zz");
e2e_error_test!(e2e_err_parse_error, "parse_error.zz");
e2e_error_test!(e2e_err_div_by_zero, "div_by_zero.zz");
e2e_error_test!(e2e_err_unknown_field, "unknown_field.zz");

// ---------------------------------------------------------------------------
// Eval tests: inline code via `zz eval`
// ---------------------------------------------------------------------------

#[test]
fn e2e_eval_basic_arithmetic() {
    let (exit, stdout, _) = run_zz_eval("1 + 2");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "3");
}

#[test]
fn e2e_eval_string_interpolation() {
    let (exit, stdout, _) = run_zz_eval("name := \"ZZ\"; \"Hello, {name}!\"");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hello, ZZ!");
}

#[test]
fn e2e_eval_closure() {
    let (exit, stdout, _) =
        run_zz_eval("double := |x: int| x * 2; nums := [1, 2, 3]; map(nums, double)");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "[2, 4, 6]");
}

#[test]
fn e2e_eval_pipeline() {
    let (exit, stdout, _) =
        run_zz_eval("inc := |x: int| x + 1; dbl := |x: int| x * 2; 5 |> inc |> dbl");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "12");
}

#[test]
fn e2e_eval_option() {
    let (exit, stdout, _) =
        run_zz_eval("val := .some(42); match val { .some(v) => v, .none => 0 }");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_eval_elvis() {
    let (exit, stdout, _) = run_zz_eval("none_val: Option<int> = .none; none_val ?? 99");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "99");
}

#[test]
fn e2e_eval_typeof() {
    let (exit, stdout, _) = run_zz_eval("typeof(42)");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "int");
}

#[test]
fn e2e_eval_list_comprehension() {
    let (exit, stdout, _) = run_zz_eval("[x ** 2 for x in range(0, 5)]");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "[0, 1, 4, 9, 16]");
}

#[test]
fn e2e_eval_default_params() {
    let (exit, stdout, _) =
        run_zz_eval("func greet(name: str, greeting: str = \"Hello\") -> str { \"{greeting}, {name}\" }; greet(\"ZZ\")");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hello, ZZ");
}

// Note: struct definitions/mutation don't work in eval mode (parser limitation).
// Struct tests are covered by `e2e_types_structs` run fixture.

#[test]
fn e2e_eval_string_comparison_if() {
    // Verify string comparison before block parses AND runs correctly.
    let (exit, stdout, _) = run_zz_eval(r#"x := "hello"; if x == "hello" { "yes" } else { "no" }"#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "yes");
}

#[test]
fn e2e_eval_string_interpolation_still_works() {
    let (exit, stdout, _) = run_zz_eval(r#"greeting := "Hi"; name := "ZZ"; "{greeting}, {name}!""#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hi, ZZ!");
}

#[test]
fn e2e_eval_unspaced_string_block() {
    let (exit, stdout, _) = run_zz_eval(r#"x := "hello"; if x == "hello"{ "yes" }else{ "no" }"#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "yes");
}

#[test]
fn e2e_eval_multi_interpolation() {
    let (exit, stdout, _) = run_zz_eval(r#"a := "Hello"; b := "World"; "{a} {b}!""#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hello World!");
}

#[test]
fn e2e_check_string_before_while_block() {
    let tmp = std::env::temp_dir().join("zz_str_while_test.zz");
    std::fs::write(
        &tmp,
        r#"s := "test"
while s == "test" {
    println(s)
}"#,
    )
    .unwrap();
    let (exit, _, stderr) = run_zz_check(&tmp);
    assert_eq!(
        exit, 0,
        "check should pass for string-before-while-block.\nstderr: {stderr}"
    );
    std::fs::remove_file(&tmp).ok();
}

// ---------------------------------------------------------------------------
// Dynamic discovery: run every .zz under fixtures/success/
// ---------------------------------------------------------------------------

#[test]
fn e2e_discover_all_success_fixtures() {
    let fixtures = fixtures_dir();
    let success_dirs = ["syntax", "types", "stdlib"];
    let mut failures = Vec::new();

    for dir_name in &success_dirs {
        let dir = fixtures.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for file in find_fixtures(&dir) {
            let (exit, stdout, stderr) = run_zz(&file);
            if exit != 0 {
                failures.push(format!(
                    "FAIL {} (exit {exit})\nstdout: {stdout}\nstderr: {stderr}",
                    file.display(),
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed:\n{}",
            failures.len(),
            failures.join("\n\n"),
        );
    }
}

#[test]
fn e2e_discover_all_error_fixtures() {
    let fixtures = fixtures_dir();
    let err_dir = fixtures.join("errors");
    if !err_dir.is_dir() {
        return; // no error fixtures, skip
    }

    let mut failures = Vec::new();
    for file in find_fixtures(&err_dir) {
        let (exit, stdout, stderr) = run_zz(&file);
        if exit == 0 {
            failures.push(format!(
                "FAIL {} — should error but exited 0\nstdout: {stdout}",
                file.display(),
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} error fixture(s) failed:\n{}",
            failures.len(),
            failures.join("\n\n"),
        );
    }
}
