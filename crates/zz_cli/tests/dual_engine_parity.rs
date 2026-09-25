//! Dual-engine parity tests: every `.zz` fixture must produce identical
//! output when run through both the bytecode VM (`zz run`) and the AOT
//! native compiler (`zz run --native`).
//!
//! Fixtures are categorized as:
//! - **Strict parity**: both engines must produce identical output
//! - **Known native failure**: tracked bugs in the native engine (test passes
//!   if native fails as expected; panics if the bug is fixed so we can remove it)
//! - **Skipped**: non-deterministic output (HTTP closures, timing)
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

/// Stdin bytes for a fixture run: `<stem>.stdin` sitting next to the
/// `.zz` file when present, otherwise empty.
///
/// Piping explicitly (instead of inheriting) keeps runs deterministic:
/// a fixture calling `input()` sees EOF rather than hanging on a TTY
/// or inheriting CI's /dev/null unpredictably. Both engines get the
/// identical bytes, so stdin itself is parity-covered. (Convention:
/// keep `.stdin` files small — bytes are written before output is
/// drained, like the existing e2e piped-input test.)
fn stdin_for(file: &Path) -> Vec<u8> {
    let stdin_path = file.with_extension("stdin");
    std::fs::read(stdin_path).unwrap_or_default()
}

/// Run `zz` with `args` + `file`, feeding `input` on stdin.
/// Write errors are ignored: fixtures that exit early (e.g. error
/// fixtures that never read stdin) close the pipe first (EPIPE).
fn run_zz_with_input(args: &[&str], file: &Path, input: &[u8]) -> (i32, String, String) {
    use std::io::Write as _;
    use std::process::Stdio;
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let mut child = Command::new(zz_bin)
        .args(args)
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .spawn()
        .unwrap_or_else(|e| panic!("failed to exec `zz {args:?} {file:?}`: {e}"));
    if !input.is_empty() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(input);
        }
    }
    // Always close the pipe: the child sees EOF instead of blocking
    // forever on a held-open stdin (fixtures without a `.stdin` file
    // read empty input, deterministically, on both engines).
    drop(child.stdin.take());
    let output = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("failed to wait `zz {args:?} {file:?}`: {e}"));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Run `zz run <file>` (bytecode VM engine).
fn run_zz_vm(file: &Path) -> (i32, String, String) {
    let input = stdin_for(file);
    run_zz_with_input(&["run"], file, &input)
}

/// Run `zz run --native <file>` (AOT native compiler engine).
fn run_zz_native(file: &Path) -> (i32, String, String) {
    let input = stdin_for(file);
    run_zz_with_input(&["run", "--native"], file, &input)
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

/// Replace `ip:port` substrings with `<addr>`. Ephemeral local ports differ
/// between VM and native runs even for identical programs.
fn normalize_addrs(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b':')
            {
                i += 1;
            }
            let cand = &s[start..i];
            let dots = cand.matches('.').count();
            if dots == 3 && cand.contains(':') {
                if let Some((prefix, port)) = cand.rsplit_once(':') {
                    let port_ok = !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit());
                    let prefix_ok = prefix.bytes().all(|b| b.is_ascii_digit() || b == b'.');
                    if port_ok && prefix_ok {
                        out.extend_from_slice(b"<addr>");
                        continue;
                    }
                }
            }
            out.extend_from_slice(&bytes[start..i]);
        } else {
            let b = bytes[i];
            if b.is_ascii() {
                out.push(b);
            } else {
                // Copy a whole UTF-8 sequence.
                let len = utf8_len(b);
                let end = (i + len).min(bytes.len());
                out.extend_from_slice(&bytes[i..end]);
                i = end - 1;
            }
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn utf8_len(first: u8) -> usize {
    if first & 0x80 == 0 {
        1
    } else if first & 0xE0 == 0xC0 {
        2
    } else if first & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// Known skip reasons (non-deterministic output)
// ---------------------------------------------------------------------------

/// Returns `Some(reason)` if the fixture should be skipped entirely.
fn native_skip_reason(file: &Path) -> Option<&'static str> {
    let stem = file.file_stem()?.to_str()?;
    match stem {
        "http_server_test" | "http_client_test" | "http_phase5b_test" | "concurrent_http_test" => {
            Some("live sockets / http.log timing output are non-deterministic between engines")
        }
        "time_ops" | "time_test" | "bench_memory_arena" => {
            Some("output contains time.now_ms() — non-deterministic timestamps")
        }
        "log_test" => {
            Some("log output embeds unix timestamps and span durations — non-deterministic")
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
        // --- C codegen compile errors (scalar boxing class) ---
        "frame_slots" => Some("C codegen: raw zz_value in scalar comparison `(v0 > 0)`"),
        "question_operator_newline" => {
            Some("C codegen: raw int64_t global assigned into zz_value temp")
        }
        "struct_impl" => {
            Some("C codegen: unboxed struct returned/fielded as zz_value and vice versa")
        }
        "local_wildcard" => {
            Some("C codegen: imported scalar global unboxed twice (`(zz_global_PI).i` on int64_t)")
        }

        // --- Output differences (native runs but output differs) ---
        "concurrency_panic_test" => Some("native: panic/fail inside task closures lowers to unit (no err plumbing through zz_call_closure); VM yields .err"),
        "encoding_test" => Some("native: different error message format for bad base64/hex/url"),
        "math_extended_test" => Some("native: float precision + error message differences"),
        "closure_annotations" => Some("native: top-level closure-call results print empty"),
        "decorators" => Some("native: only the final marker prints; decorator wrapper output missing"),
        "destructuring" => Some("native: top-level tuple-destructured values print empty"),
        "extension_methods" => {
            Some("native: extension-method call results missing + spurious conflict diagnostics on stderr")
        }
        "selective_import" | "multi_selective" | "symbol_alias" | "wildcard_import" => {
            Some("native: imported const binding prints empty (call results are fine)")
        }
        "generic_selective" => {
            Some("native: local-module generic fn call results print empty")
        }

        // --- Error fixtures where native leniency exits 0 ---
        "main_result_err" => Some("native: main returning .err exits 0 (no propagation)"),
        "pg_connect_refused" | "mysql_connect_refused" => {
            Some("native: refused connect yields a null handle and exits 0 (AOT leniency, documented)")
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parity assertion
// ---------------------------------------------------------------------------

/// Normalize an output stream for cross-engine comparison: ephemeral
/// `ip:port` pairs collapse and purely numeric lines (timestamps,
/// addresses) drop. Single choke point so the macros and the sweep
/// below can never disagree on what "equal" means.
fn norm_stream(s: &str) -> String {
    strip_numeric_lines(&normalize_addrs(s))
}

/// True when two runs match byte-for-byte after normalization:
/// exit codes equal and stdout + stderr equal.
fn parity_match(vm: &(i32, String, String), native: &(i32, String, String)) -> bool {
    vm.0 == native.0
        && norm_stream(&vm.1) == norm_stream(&native.1)
        && norm_stream(&vm.2) == norm_stream(&native.2)
}

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

    let vm_norm = norm_stream(&vm_stdout);
    let native_norm = norm_stream(&native_stdout);
    assert_eq!(
        vm_norm, native_norm,
        "PARITY BUG [{display}]: VM and native stdout differ (exits vm={vm_exit} native={native_exit}).\n--- VM stdout ---\n{vm_stdout}\n--- NATIVE stdout ---\n{native_stdout}\n--- VM stderr ---\n{vm_stderr}\n--- NATIVE stderr ---\n{native_stderr}"
    );

    let vm_err_norm = norm_stream(&vm_stderr);
    let native_err_norm = norm_stream(&native_stderr);
    assert_eq!(
        vm_err_norm, native_err_norm,
        "PARITY BUG [{display}]: VM and native stderr differ.\n--- VM stderr ---\n{vm_stderr}\n--- Native stderr ---\n{native_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// True when `ZZ_PARITY_VM_ONLY=1`: skip the `--native` leg (slow C
/// compile+link per fixture) and assert the VM leg only. For fast
/// iteration (`scripts/test-fast.sh`); CI always runs both legs.
fn vm_only() -> bool {
    std::env::var("ZZ_PARITY_VM_ONLY").is_ok()
}

/// Generate a strict error-parity test (both engines must error).
macro_rules! parity_strict_error {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join("errors").join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let vm = run_zz_vm(&path);
            if vm_only() {
                assert_ne!(
                    vm.0,
                    0,
                    "[{}]: VM should fail but exited 0.",
                    path.display()
                );
                return;
            }
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
            if vm_only() {
                assert_eq!(
                    vm.0,
                    0,
                    "[{}]: VM should exit 0 but got {}.\nvm stderr: {}",
                    path.display(),
                    vm.0,
                    vm.2
                );
                return;
            }
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
            // VM must succeed.
            assert_eq!(
                vm.0, 0,
                "VM failed for known-failure fixture {}.\nvm stderr: {}",
                path.display(),
                vm.2
            );

            if vm_only() {
                // Known bugs live on the native leg, which is skipped.
                eprintln!("SKIP KNOWN BUG [{}]: {bug} (VM leg ok)", path.display());
                return;
            }
            let native = run_zz_native(&path);

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
#[allow(unused_macros)]
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
parity_strict!(parity_syntax_scalar_copy, "syntax", "scalar_copy.zz");
parity_strict!(parity_syntax_elif_chain, "syntax", "elif_chain.zz");
parity_strict!(parity_syntax_top_level_elif, "syntax", "top_level_elif.zz");
parity_strict!(parity_syntax_chained_calls, "syntax", "chained_calls.zz");

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
parity_strict!(
    parity_stdlib_concurrency_spawn_test,
    "stdlib",
    "concurrency_spawn_test.zz"
);
parity_strict!(parity_stdlib_channel_test, "stdlib", "channel_test.zz");
parity_strict!(
    parity_stdlib_concurrency_tasks_test,
    "stdlib",
    "concurrency_tasks_test.zz"
);
parity_strict!(
    parity_stdlib_concurrency_stress_test,
    "stdlib",
    "concurrency_stress_test.zz"
);
parity_strict!(
    parity_stdlib_concurrency_try_join_test,
    "stdlib",
    "concurrency_try_join_test.zz"
);
parity_strict!(
    parity_stdlib_concurrency_capture_test,
    "stdlib",
    "concurrency_capture_test.zz"
);
parity_strict!(
    parity_stdlib_concurrency_recall_test,
    "stdlib",
    "concurrency_recall_test.zz"
);
parity_known_failure!(
    parity_stdlib_concurrency_panic_test,
    "stdlib",
    "concurrency_panic_test.zz"
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

// --- Fixed: nested field access boxing ---
parity_strict!(parity_types_structs, "types", "structs.zz");
// --- Fixed: nested variant patterns + if-let desugaring ---
parity_strict!(parity_types_variants, "types", "variants.zz");

// --- Fixed: empty_infer type inference ---
parity_strict!(parity_syntax_empty_infer, "syntax", "empty_infer.zz");

// --- Output differences (native runs but output diverges) ---
parity_strict!(parity_syntax_functions, "syntax", "functions.zz");
parity_strict!(parity_syntax_hof, "syntax", "hof.zz");
parity_strict!(parity_syntax_arrays, "syntax", "arrays.zz");
parity_strict!(parity_syntax_defer, "syntax", "defer.zz");
parity_strict!(parity_syntax_dict_iteration, "syntax", "dict_iteration.zz");
parity_strict!(parity_stdlib_vectors, "stdlib", "vectors.zz");
parity_strict!(parity_stdlib_math_ops, "stdlib", "math_ops.zz");
parity_known_failure!(
    parity_stdlib_math_extended,
    "stdlib",
    "math_extended_test.zz"
);
parity_strict!(parity_stdlib_jsonmod, "stdlib", "jsonmod.zz");
parity_strict!(parity_stdlib_json_test, "stdlib", "json_test.zz");
parity_known_failure!(parity_stdlib_encoding_test, "stdlib", "encoding_test.zz");
parity_strict!(parity_stdlib_filesystem, "stdlib", "filesystem.zz");
parity_strict!(parity_stdlib_fs_test, "stdlib", "fs_test.zz");
parity_strict!(
    parity_stdlib_fs_comprehensive,
    "stdlib",
    "fs_comprehensive.zz"
);
parity_strict!(parity_stdlib_result_print, "stdlib", "result_print.zz");
parity_strict!(parity_stdlib_import_alias, "stdlib", "import_alias.zz");
parity_strict!(parity_stdlib_fs_path, "stdlib", "fs_path.zz");
parity_strict!(parity_stdlib_fs_vfs, "stdlib", "fs_vfs.zz");
parity_strict!(parity_stdlib_bytes, "stdlib", "bytes.zz");
parity_strict!(parity_stdlib_env_full, "stdlib", "env_full.zz");
parity_strict!(parity_stdlib_net_tcp_test, "stdlib", "net_tcp_test.zz");
parity_strict!(parity_stdlib_input_chained, "stdlib", "input_chained.zz");
parity_strict!(
    parity_stdlib_http_request_response,
    "stdlib",
    "http_request_response.zz"
);

// --- Error fixture: both engines must error on missing struct field ---
parity_strict_error!(parity_err_missing_field, "missing_field.zz");

// ===========================================================================
// Skipped fixtures (non-deterministic output)
// ===========================================================================
//
// These are skipped by native_skip_reason() in the strict parity tests above.
// Listed here for documentation:
// - HTTP: http_server_test, http_client_test, http_phase5b_test, concurrent_http_test
// - Timing: time_ops, time_test, bench_memory_arena
// - Logging: log_test (embedded timestamps/durations)
// - Str stdlib: str_extended_test (already strict — passes)

// ===========================================================================
// Exhaustive parity sweep: EVERY fixture through BOTH engines.
//
// This is the strict gate — not documentation. Any `.zz` file under
// tests/fixtures/{syntax,types,stdlib,errors} runs here with no
// registration needed, so a new fixture (or a regression in an old one)
// cannot slip past the per-file macros above. Stdin comes from the
// `<stem>.stdin` sibling when present (see `stdin_for`), closed
// otherwise, identically for both engines.
//
// Buckets: strict pass / known failure (tracked bug, still broken) /
// skipped (non-deterministic output) / unexpected (CI-red). Under
// `ZZ_PARITY_VM_ONLY=1` only the VM leg runs (fast iteration).
// ===========================================================================

#[test]
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
            if vm.0 != 0 {
                unexpected.push(format!("VM FAIL {}: {}", file.display(), vm.2));
                continue;
            }
            if vm_only() {
                strict_pass += 1;
                continue;
            }
            let native = run_zz_native(file);

            if let Some(bug) = known_native_failure(file) {
                known_failures += 1;
                if parity_match(&vm, &native) {
                    unexpected.push(format!(
                        "FIXED! {} — remove from known_native_failures() (was: {bug})",
                        file.display()
                    ));
                }
                continue;
            }

            // Strict parity check (exit + stdout + stderr).
            if native.0 != 0 {
                unexpected.push(format!("NATIVE FAIL {}: {}", file.display(), native.2));
                continue;
            }
            if !parity_match(&vm, &native) {
                unexpected.push(format!(
                    "PARITY BUG {}\n--- exits vm={} native={} ---\n--- VM stdout ---\n{}\n--- NATIVE stdout ---\n{}\n--- VM stderr ---\n{}\n--- NATIVE stderr ---\n{}",
                    file.display(),
                    vm.0,
                    native.0,
                    vm.1,
                    native.1,
                    vm.2,
                    native.2
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
            if let Some(bug) = known_native_failure(file) {
                err_known += 1;
                // A "fixed" error fixture fails on native again: surface
                // it instead of silently counting.
                let native = if vm_only() {
                    continue;
                } else {
                    run_zz_native(file)
                };
                if native.0 != 0 {
                    unexpected.push(format!(
                        "FIXED! {} — native errors again; remove from known_native_failures() (was: {bug})",
                        file.display()
                    ));
                }
                continue;
            }
            let vm = run_zz_vm(file);
            if vm.0 == 0 {
                unexpected.push(format!(
                    "ERROR FIXTURE {} exits 0 on VM (should fail)",
                    file.display()
                ));
                continue;
            }
            if vm_only() {
                err_strict += 1;
                continue;
            }
            let native = run_zz_native(file);
            if native.0 != 0 {
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
