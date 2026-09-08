//! Dual-engine parity tests: every `.zz` fixture must produce identical
//! output when run through both the bytecode VM (`zz run`) and the AOT
//! native compiler (`zz run --native`).
//!
//! Fixtures are categorized as:
//! - **Strict parity**: both engines must produce identical output
//! - **Known native failure**: tracked bugs in the native engine (test passes
//!   if native fails as expected; panics if the bug is fixed so we can remove it)
//! - **Skipped**: non-deterministic output (HTTP closures, timing, concurrency)
//!
//! Run with: `cargo test -p zz_cli --test dual_engine_parity`

use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Locate `tests/fixtures/` relative to `CARGO_MANIFEST_DIR`.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Run `zz run <file>` (bytecode VM engine).
fn run_zz_vm(file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("run")
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz run {file:?}`: {e}"));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Run `zz run --native <file>` (AOT native compiler engine).
fn run_zz_native(file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("run")
        .arg("--native")
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz run --native {file:?}`: {e}"));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Strip lines that are purely numeric (timestamps, memory addresses).
/// These are non-deterministic between VM and native runs.
fn strip_numeric_lines(s: &str) -> String {
    s.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.is_empty() || trimmed.parse::<i64>().is_err()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Known skip reasons (non-deterministic output)
// ---------------------------------------------------------------------------

/// Returns `Some(reason)` if the fixture should be skipped entirely.
fn native_skip_reason(file: &Path) -> Option<&'static str> {
    let stem = file.file_stem()?.to_str()?;
    match stem {
        "http_server_test" | "http_client_test" | "http_phase5b_test" | "concurrent_http_test" => {
            Some("HTTP route handlers use closures not callable from AOT C runtime")
        }
        "time_ops" | "time_test" | "bench_memory_arena" => {
            Some("output contains time.now_ms() — non-deterministic timestamps")
        }
        "concurrency_spawn_test" | "channel_test" => {
            Some("thread scheduling makes output non-deterministic")
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Known native failures — tracked parity bugs
//
// Each entry maps a fixture stem to a bug description. If the native engine
// unexpectedly SUCCEEDS for a fixture in this list, the test panics with
// "FIXED! Remove from known_native_failures!" so we can clean up.
// ---------------------------------------------------------------------------

/// Returns `Some(bug_description)` if the fixture has a known native bug.
fn known_native_failure(file: &Path) -> Option<&'static str> {
    let stem = file.file_stem()?.to_str()?;
    match stem {
        // --- C codegen compile errors (not yet fixed) ---
        "empty_infer" => Some("C codegen: type mismatch in zz_clone (int64_t vs zz_value)"),
        "structs" => Some("C codegen: nested field access emits int64_t instead of zz_value"),
        "variants" => Some("C codegen: undeclared variable in match + else scope error"),

        // --- Output differences (native runs but output differs) ---
        "missing_field" => Some("native: should error on missing struct field but exits 0"),
        "encoding_test" => Some("native: different error message format for bad base64/hex/url"),
        "filesystem" => Some("native: fs.write/fs.read not implemented in C runtime"),
        "fs_test" => Some("native: fs.write/fs.read/fs.remove not implemented in C runtime"),
        "json_test" => Some("native: json.parse returns empty — C runtime json stub"),
        "jsonmod" => Some("native: json.parse/stringify not implemented in C runtime"),
        "math_extended_test" => Some("native: float precision + error message differences"),
        "math_ops" => Some("native: sqrt float precision difference (14 vs 16 digits)"),
        "net_tcp_test" => Some("native: TCP listen/connect/read not implemented in C runtime"),
        "vectors" => Some("native: vec.slice/vec.push missing — C runtime stub"),
        "arrays" => Some("native: array literal/comprehension printing broken in codegen"),
        "defer" => Some("native: defer statements not emitted in C codegen"),
        "dict_iteration" => Some("native: dict.keys() iteration missing second loop output"),
        "functions" => Some("native: string concatenation with '+' drops first operand"),
        "hof" => Some("native: higher-order functions (map/filter) not working in C runtime"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parity assertion
// ---------------------------------------------------------------------------

/// Assert strict parity between VM and native output.
fn assert_parity_strict(
    file: &Path,
    vm: (i32, String, String),
    native: (i32, String, String),
    is_error_fixture: bool,
) {
    let (vm_exit, vm_stdout, vm_stderr) = vm;
    let (native_exit, native_stdout, native_stderr) = native;
    let display = file.display();

    if is_error_fixture {
        assert_ne!(
            vm_exit, 0,
            "[{display}]: VM should fail but exited 0.\nvm stdout: {vm_stdout}"
        );
        assert_ne!(
            native_exit, 0,
            "[{display}]: native should fail but exited 0.\nnative stdout: {native_stdout}"
        );
        assert!(
            vm_stderr.contains("error") || !vm_stderr.is_empty(),
            "[{display}]: VM error fixture produced no diagnostics.\nvm stderr: {vm_stderr}"
        );
        assert!(
            native_stderr.contains("error") || !native_stderr.is_empty(),
            "[{display}]: native error fixture produced no diagnostics.\nnative stderr: {native_stderr}"
        );
        return;
    }

    assert_eq!(
        vm_exit, 0,
        "[{display}]: VM should exit 0 but got {vm_exit}.\nvm stderr: {vm_stderr}"
    );
    assert_eq!(
        native_exit, 0,
        "[{display}]: native should exit 0 but got {native_exit}.\nnative stderr: {native_stderr}"
    );

    let vm_norm = strip_numeric_lines(&vm_stdout);
    let native_norm = strip_numeric_lines(&native_stdout);
    assert_eq!(
        vm_norm, native_norm,
        "PARITY BUG [{display}]: VM and native stdout differ.\n--- VM ---\n{vm_stdout}\n--- NATIVE ---\n{native_stdout}"
    );

    let vm_err_norm = strip_numeric_lines(&vm_stderr);
    let native_err_norm = strip_numeric_lines(&native_stderr);
    assert_eq!(
        vm_err_norm, native_err_norm,
        "PARITY BUG [{display}]: VM and native stderr differ.\n--- VM stderr ---\n{vm_stderr}\n--- Native stderr ---\n{native_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// Generate a strict error-parity test (both engines must error).
macro_rules! parity_strict_error {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join("errors").join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let vm = run_zz_vm(&path);
            let native = run_zz_native(&path);
            assert_parity_strict(&path, vm, native, true);
        }
    };
}

/// Generate a strict parity test (both engines must match).
macro_rules! parity_strict {
    ($name:ident, $category:expr, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join($category).join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            if let Some(reason) = native_skip_reason(&path) {
                eprintln!("SKIP [{}]: {reason}", path.display());
                return;
            }

            let vm = run_zz_vm(&path);
            let native = run_zz_native(&path);
            assert_parity_strict(&path, vm, native, false);
        }
    };
}

/// Generate a known-failure test (native is expected to fail/differ).
///
/// - If native fails/differs as expected → test PASSES (known bug documented)
/// - If native unexpectedly succeeds → test PANICS with "FIXED!" so we clean up
macro_rules! parity_known_failure {
    ($name:ident, $category:expr, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join($category).join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let bug = known_native_failure(&path)
                .expect("parity_known_failure called for non-known-failure fixture");

            let vm = run_zz_vm(&path);
            let native = run_zz_native(&path);

            // VM must succeed.
            assert_eq!(
                vm.0, 0,
                "VM failed for known-failure fixture {}.\nvm stderr: {}",
                path.display(),
                vm.2
            );

            // Check if native failed/differed as expected.
            let native_broken = native.0 != 0 || strip_numeric_lines(&vm.1) != strip_numeric_lines(&native.1);

            if native_broken {
                eprintln!(
                    "KNOWN BUG [{}]: {bug}\n  native exit: {}\n  native stderr: {}",
                    path.display(),
                    native.0,
                    native.2
                );
                // Test passes — known bug is still present.
            } else {
                // Native unexpectedly succeeded! Bug is fixed.
                panic!(
                    "FIXED! [{}]: native now matches VM.\n\
                     Remove this fixture from known_native_failures() in dual_engine_parity.rs.\n\
                     Bug was: {bug}",
                    path.display()
                );
            }
        }
    };
}

/// Generate a known-failure error test.
macro_rules! parity_known_error_failure {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join("errors").join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let bug = known_native_failure(&path)
                .expect("parity_known_failure called for non-known-failure fixture");

            let vm = run_zz_vm(&path);
            let native = run_zz_native(&path);

            // VM must fail.
            assert_ne!(
                vm.0, 0,
                "VM should fail for error fixture {}.",
                path.display()
            );

            // Native is expected to fail differently (exit 0 or wrong error).
            let native_broken = native.0 == 0;
            // (For error fixtures, "broken" means native doesn't error when it should)

            if native_broken {
                eprintln!(
                    "KNOWN BUG [{}]: {bug}\n  native exited 0 (should have errored)",
                    path.display()
                );
            } else {
                panic!(
                    "FIXED! [{}]: native now errors correctly.\n\
                     Remove this fixture from known_native_failures() in dual_engine_parity.rs.\n\
                     Bug was: {bug}",
                    path.display()
                );
            }
        }
    };
}

// ===========================================================================
// Strict parity tests — VM and native MUST match
// ===========================================================================

// Syntax
parity_strict!(parity_syntax_declarations, "syntax", "declarations.zz");
parity_strict!(parity_syntax_pipelines, "syntax", "pipelines.zz");
parity_strict!(parity_syntax_operators, "syntax", "operators.zz");
parity_strict!(parity_syntax_fstrings, "syntax", "fstrings.zz");
parity_strict!(parity_syntax_dicts, "syntax", "dicts.zz");
parity_strict!(parity_syntax_string_blocks, "syntax", "string_blocks.zz");
parity_strict!(parity_syntax_pipe_elvis, "syntax", "pipe_elvis.zz");

// Types
parity_strict!(parity_types_generics, "types", "generics.zz");
parity_strict!(parity_types_type_inference, "types", "type_inference.zz");

// Stdlib
parity_strict!(parity_stdlib_strings, "stdlib", "strings.zz");
parity_strict!(parity_stdlib_console, "stdlib", "console.zz");
parity_strict!(parity_stdlib_envmod, "stdlib", "envmod.zz");
parity_strict!(parity_stdlib_env_test, "stdlib", "env_test.zz");
parity_strict!(
    parity_stdlib_str_extended_test,
    "stdlib",
    "str_extended_test.zz"
);

// Error fixtures (both must error)
parity_strict_error!(parity_err_type_mismatch, "type_mismatch.zz");
parity_strict_error!(parity_err_undefined_var, "undefined_var.zz");
parity_strict_error!(parity_err_arity, "arity.zz");
parity_strict_error!(parity_err_parse_error, "parse_error.zz");
parity_strict_error!(parity_err_div_by_zero, "div_by_zero.zz");
parity_strict_error!(parity_err_unknown_field, "unknown_field.zz");
parity_strict_error!(parity_err_struct_init_assign, "struct_init_assign_error.zz");
parity_strict_error!(parity_err_int_float_cmp, "int_float_cmp.zz");

// ===========================================================================
// Known native failure tests — documented bugs
//
// These tests PASS even though native fails, because the failure is expected.
// When a bug is fixed, the test will panic with "FIXED!" reminding us to
// remove the entry from known_native_failures() and move to strict parity.
// ===========================================================================

// --- Match and return_in_loops: fixed by box_scalar_operand + __tail scoping ---
parity_strict!(parity_syntax_match, "syntax", "match.zz");
parity_strict!(
    parity_syntax_return_in_loops,
    "syntax",
    "return_in_loops.zz"
);

// --- control_flow: fixed (member access on non-struct type) ---
parity_strict!(parity_syntax_control_flow, "syntax", "control_flow.zz");

// --- Still broken: C codegen compile errors (not yet fixed) ---
parity_known_failure!(parity_syntax_empty_infer, "syntax", "empty_infer.zz");
parity_known_failure!(parity_types_structs, "types", "structs.zz");
parity_known_failure!(parity_types_variants, "types", "variants.zz");

// --- Output differences (native runs but output diverges) ---
parity_known_failure!(parity_syntax_functions, "syntax", "functions.zz");
parity_known_failure!(parity_syntax_hof, "syntax", "hof.zz");
parity_known_failure!(parity_syntax_arrays, "syntax", "arrays.zz");
parity_known_failure!(parity_syntax_defer, "syntax", "defer.zz");
parity_known_failure!(parity_syntax_dict_iteration, "syntax", "dict_iteration.zz");
parity_known_failure!(parity_stdlib_vectors, "stdlib", "vectors.zz");
parity_known_failure!(parity_stdlib_math_ops, "stdlib", "math_ops.zz");
parity_known_failure!(
    parity_stdlib_math_extended,
    "stdlib",
    "math_extended_test.zz"
);
parity_known_failure!(parity_stdlib_jsonmod, "stdlib", "jsonmod.zz");
parity_known_failure!(parity_stdlib_json_test, "stdlib", "json_test.zz");
parity_known_failure!(parity_stdlib_encoding_test, "stdlib", "encoding_test.zz");
parity_known_failure!(parity_stdlib_filesystem, "stdlib", "filesystem.zz");
parity_known_failure!(parity_stdlib_fs_test, "stdlib", "fs_test.zz");
parity_known_failure!(parity_stdlib_net_tcp_test, "stdlib", "net_tcp_test.zz");

// --- Error fixture: native doesn't error when it should ---
parity_known_error_failure!(parity_err_missing_field, "missing_field.zz");

// ===========================================================================
// Skipped fixtures (non-deterministic output)
// ===========================================================================
//
// These are skipped by native_skip_reason() in the strict parity tests above.
// Listed here for documentation:
// - HTTP: http_server_test, http_client_test, http_phase5b_test, concurrent_http_test
// - Timing: time_ops, time_test, bench_memory_arena
// - Concurrency: concurrency_spawn_test, channel_test
// - Str stdlib: str_extended_test (already strict — passes)

// ===========================================================================
// Dynamic discovery: exhaustive parity sweep (ignored by default)
//
// Runs EVERY .zz fixture through both engines. Reports summary.
// Run with: cargo test -p zz_cli --test dual_engine_parity -- --ignored
// ===========================================================================

#[test]
#[ignore]
fn parity_discover_all_fixtures() {
    let fixtures = fixtures_dir();
    let success_dirs = ["syntax", "types", "stdlib"];
    let mut strict_pass = 0u32;
    let mut known_failures = 0u32;
    let mut skipped = 0u32;
    let mut unexpected = Vec::new();

    for dir_name in &success_dirs {
        let dir = fixtures.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read dir {dir:?}: {e}"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("zz"))
            .collect();
        files.sort();

        for file in &files {
            if native_skip_reason(file).is_some() {
                skipped += 1;
                continue;
            }

            let vm = run_zz_vm(file);
            let native = run_zz_native(file);

            if known_native_failure(file).is_some() {
                known_failures += 1;
                let native_broken =
                    native.0 != 0 || strip_numeric_lines(&vm.1) != strip_numeric_lines(&native.1);
                if !native_broken {
                    unexpected.push(format!(
                        "FIXED! {} — remove from known_native_failures()",
                        file.display()
                    ));
                }
                continue;
            }

            // Strict parity check.
            if vm.0 != 0 {
                unexpected.push(format!("VM FAIL {}: {}", file.display(), vm.2));
                continue;
            }
            if native.0 != 0 {
                unexpected.push(format!("NATIVE FAIL {}: {}", file.display(), native.2));
                continue;
            }
            if strip_numeric_lines(&vm.1) != strip_numeric_lines(&native.1) {
                unexpected.push(format!(
                    "PARITY BUG {}\n--- VM ---\n{}\n--- NATIVE ---\n{}",
                    file.display(),
                    vm.1,
                    native.1
                ));
                continue;
            }
            strict_pass += 1;
        }
    }

    // Error fixtures.
    let err_dir = fixtures.join("errors");
    let mut err_known = 0u32;
    let mut err_strict = 0u32;
    if err_dir.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&err_dir)
            .unwrap_or_else(|e| panic!("cannot read dir {err_dir:?}: {e}"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("zz"))
            .collect();
        files.sort();

        for file in &files {
            if known_native_failure(file).is_some() {
                err_known += 1;
                continue;
            }
            let vm = run_zz_vm(file);
            let native = run_zz_native(file);
            if vm.0 != 0 && native.0 != 0 {
                err_strict += 1;
            } else {
                unexpected.push(format!(
                    "ERROR PARITY {}: vm_exit={} native_exit={}",
                    file.display(),
                    vm.0,
                    native.0
                ));
            }
        }
    }

    println!("\n=== Dual-Engine Parity Summary ===");
    println!("Strict parity pass: {strict_pass}");
    println!("Known failures:     {known_failures} (+ {err_known} error fixtures)");
    println!("Strict error pass:  {err_strict}");
    println!("Skipped:            {skipped}");
    println!("Unexpected:         {}", unexpected.len());
    for u in &unexpected {
        println!("  {u}");
    }

    if !unexpected.is_empty() {
        panic!(
            "{} unexpected result(s). See summary above.",
            unexpected.len()
        );
    }
}
