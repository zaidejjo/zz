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
    run_zz_with_input_env(args, file, input, None)
}

/// `run_zz_with_input` plus an optional `ZZ_SWEEP_TOKEN` env value.
/// The sweep assigns a unique token per fixture task; fixtures that
/// touch shared mutable resources (/tmp scratch files, bind ports)
/// mix it into their paths/ports so parallel runners (macro tests,
/// sweep tasks, e2e) can never share them.
/// Bound on any single `zz` child (compile + run). A fixture that
/// spins forever (e.g. a retry loop over a native lowered to unit —
/// see `known_native_failure`) must fail the task, never the whole
/// sweep: `wait_with_output` below used to block indefinitely, wedging
/// the worker pool (`thread::scope` never joins) with zero output.
/// Generous on purpose: the `--native` leg pays a cold clang `-O3`
/// compile per fixture. Matches GNU `timeout`'s 124 convention.
const ZZ_CHILD_TIMEOUT_SECS: u64 = 300;
/// Sentinel exit for a timed-out child (GNU `timeout` convention).
const ZZ_CHILD_TIMEOUT_EXIT: i32 = 124;

fn run_zz_with_input_env(
    args: &[&str],
    file: &Path,
    input: &[u8],
    token: Option<&str>,
) -> (i32, String, String) {
    use std::io::Write as _;
    use std::process::Stdio;
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let mut cmd = Command::new(zz_bin);
    cmd.args(args)
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    if let Some(t) = token {
        cmd.env("ZZ_SWEEP_TOKEN", t);
        // Sweep fast paths (test-only, never production):
        // - `ZZ_CRYPTO_FAST=1`: minimal Argon2/bcrypt costs so the KDF
        //   fixtures take milliseconds, not tens of seconds. Same code
        //   on both engines → parity still proves the behavior.
        // - `ZZ_NATIVE_DEV=1`: `-O0 -g` clang per fixture (~4x faster
        //   than `-O3 -flto=thin`); separate cache entries, same C.
        cmd.env("ZZ_CRYPTO_FAST", "1");
        cmd.env("ZZ_NATIVE_DEV", "1");
    }
    let mut child = cmd
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
    // Bounded wait: poll, then kill. Same pattern as
    // `concurrency_audit_regression::run_timeout` and the
    // `build_e2e` network-guard (poll + `child.kill()`), so a hung
    // fixture can never wedge the sweep's worker pool.
    // NOTE: killing `zz run --native` may orphan its already-spawned
    // fixture binary (kill only reaches `zz`); that only happens on
    // timeout, i.e. for fixtures already failing as Unexpected/hangs.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(ZZ_CHILD_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let output = child
                        .wait_with_output()
                        .unwrap_or_else(|e| panic!("failed to reap `zz {args:?} {file:?}`: {e}"));
                    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                    stderr = format!(
                        "{stderr}\nPARITY TIMEOUT: `zz {args:?} {}` exceeded \
                         {ZZ_CHILD_TIMEOUT_SECS}s and was killed",
                        file.display()
                    );
                    return (
                        ZZ_CHILD_TIMEOUT_EXIT,
                        String::from_utf8_lossy(&output.stdout).to_string(),
                        stderr,
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("failed to wait `zz {args:?} {file:?}`: {e}"),
        }
    }
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

/// Sweep variants carrying the task's `ZZ_SWEEP_TOKEN`.
fn run_zz_vm_token(file: &Path, token: &str) -> (i32, String, String) {
    let input = stdin_for(file);
    run_zz_with_input_env(&["run"], file, &input, Some(token))
}

/// Sweep variants carrying the task's `ZZ_SWEEP_TOKEN`.
fn run_zz_native_token(file: &Path, token: &str) -> (i32, String, String) {
    let input = stdin_for(file);
    run_zz_with_input_env(&["run", "--native"], file, &input, Some(token))
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
        "pg_connect_refused" => {
            Some("native: refused connect yields a null handle and exits 0 (AOT leniency, documented)")
        }
        // NOTE: `http_tls_cert` used to be listed here (native exited 0
        // via silent-unit leniency); since unimplemented natives abort,
        // native errors like the VM and it passes as a strict error
        // fixture.

        // --- Phase 2/3 HTTP: VM-only natives abort loudly on AOT ---
        // `listen_tls`, `listen_cfg`, `fetch_insecure`, `body_bytes`,
        // `hijack` et al have no C impl yet (tracked in
        // zz_codegen/tests.rs KNOWN_CODEGEN_GAPS as "P3 AOT"). Native
        // legs abort at first use via `zz_unimplemented_native` (exit 1
        // with `not implemented in AOT builds`) instead of the old
        // silent unit, which wedged retry loops forever.
        "http_tls_p3" => Some(
            "P3 AOT: listen_tls + fetch_insecure are VM-only; native aborts",
        ),
        "http_hijack_p3" | "http_bytes_p3" => {
            Some("P3 AOT: hijack / body_bytes are VM-only; native aborts")
        }
        "http_limits_p2" | "http_middleware_p2" | "http_static_p2" => {
            Some("P3 AOT: listen_cfg / serve_dir_at are VM-only; native aborts")
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parity assertion
// ---------------------------------------------------------------------------

/// Normalize an output stream for cross-engine comparison: ephemeral
/// `ip:port` pairs collapse, per-run scratch path segments (`/tmp/zz_*`)
/// mask out, and purely numeric lines (timestamps, addresses) drop.
/// Single choke point so the macros and the sweep below can never
/// disagree on what "equal" means.
fn norm_stream(s: &str) -> String {
    strip_numeric_lines(&mask_scratch_paths(&normalize_addrs(s)))
}

/// Mask per-run scratch segments in `/tmp/zz_*` paths: fixtures isolate
/// parallel runners with `..._<pid>_<token>` suffixes, and VM vs native
/// legs are always different processes, so these segments can never
/// match byte-for-byte. Only the trailing `_digits_digits` run is
/// masked; real output numbers elsewhere are untouched.
fn mask_scratch_paths(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Byte-slice prefix check (never `s[i..]`: `i` may sit inside a
        // multibyte char while scanning).
        if bytes[i..].starts_with(b"/tmp/zz_") {
            // Consume the stem: [A-Za-z0-9_]+, then strip up to two
            // trailing _<alnum-with-digit> groups (pid, sweep token),
            // which become _<run>. The digit requirement keeps real
            // name parts (`_test`, `_nonexist`) intact.
            let mut j = i + "/tmp/zz_".len();
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let mut k = j;
            for _ in 0..2 {
                let mut d = k;
                while d > i && bytes[d - 1].is_ascii_alphanumeric() {
                    d -= 1;
                }
                let run = &bytes[d..k];
                if !run.is_empty()
                    && d > i
                    && bytes[d - 1] == b'_'
                    && run.iter().any(|b| b.is_ascii_digit())
                {
                    k = d - 1;
                } else {
                    break;
                }
            }
            if k < j {
                out.extend_from_slice(&bytes[i..k]);
                out.extend_from_slice(b"_<run>");
            } else {
                out.extend_from_slice(&bytes[i..j]);
            }
            i = j;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
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
parity_strict!(
    parity_syntax_range_var_bounds,
    "syntax",
    "range_var_bounds.zz"
);
parity_strict!(
    parity_syntax_hex_escape_bounds,
    "syntax",
    "hex_escape_bounds.zz"
);
parity_strict!(
    parity_syntax_scope_collision,
    "syntax",
    "scope_collision.zz"
);
parity_strict!(
    parity_syntax_struct_scalar_fields,
    "syntax",
    "struct_scalar_fields.zz"
);
parity_strict!(
    parity_syntax_for_annotated_decl,
    "syntax",
    "for_annotated_decl.zz"
);
parity_strict!(
    parity_regression_tuple_destructure,
    "regression",
    "tuple_destructure.zz"
);
parity_strict!(
    parity_regression_neg_after_loop,
    "regression",
    "neg_after_loop.zz"
);

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
parity_strict!(parity_stdlib_math_consts, "stdlib", "math_consts.zz");
parity_strict!(parity_stdlib_enumerate_loop, "stdlib", "enumerate_loop.zz");
parity_strict!(parity_stdlib_path_join, "stdlib", "path_join.zz");
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
parity_strict!(
    parity_stdlib_option_interpolation,
    "stdlib",
    "option_interpolation.zz"
);
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
parity_strict!(
    parity_stdlib_http_fetch_test,
    "stdlib",
    "http_fetch_test.zz"
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
    // One task per fixture file. Tasks run on a bounded worker pool
    // (each spawns its own `zz` child processes, so they are fully
    // independent) with a unique ZZ_SWEEP_TOKEN each — fixtures that
    // touch shared mutable resources mix it into their scratch
    // paths/ports and can never collide across threads or processes.
    struct SweepTask {
        file: PathBuf,
        errors_bucket: bool,
        token: String,
    }
    enum SweepOutcome {
        StrictPass,
        KnownFailure,
        Skipped,
        ErrorStrict,
        ErrorKnown,
        Unexpected(String),
    }
    fn run_task(task: &SweepTask) -> SweepOutcome {
        // Legs run sequentially (VM, then native): concurrent legs were
        // tried and reverted (8 procs on 4 cores thrashed clang to ~140s
        // wall and a native fixture died by signal under the load).
        let file = &task.file;
        if task.errors_bucket {
            if let Some(bug) = known_native_failure(file) {
                if vm_only() {
                    return SweepOutcome::ErrorKnown;
                }
                let native = run_zz_native_token(file, &task.token);
                if native.0 != 0 {
                    return SweepOutcome::Unexpected(format!(
                        "FIXED! {} — native errors again; remove from known_native_failures() (was: {bug})",
                        file.display()
                    ));
                }
                return SweepOutcome::ErrorKnown;
            }
            if vm_only() {
                let vm = run_zz_vm_token(file, &task.token);
                if vm.0 == 0 {
                    return SweepOutcome::Unexpected(format!(
                        "ERROR FIXTURE {} exits 0 on VM (should fail)",
                        file.display()
                    ));
                }
                return SweepOutcome::ErrorStrict;
            }
            let vm = run_zz_vm_token(file, &task.token);
            let native = run_zz_native_token(file, &task.token);
            if vm.0 == 0 {
                return SweepOutcome::Unexpected(format!(
                    "ERROR FIXTURE {} exits 0 on VM (should fail)",
                    file.display()
                ));
            }
            if native.0 != 0 {
                SweepOutcome::ErrorStrict
            } else {
                SweepOutcome::Unexpected(format!(
                    "ERROR PARITY {}: vm_exit={} native_exit={}",
                    file.display(),
                    vm.0,
                    native.0
                ))
            }
        } else {
            if native_skip_reason(file).is_some() {
                return SweepOutcome::Skipped;
            }
            if vm_only() {
                let vm = run_zz_vm_token(file, &task.token);
                if vm.0 != 0 {
                    return SweepOutcome::Unexpected(format!(
                        "VM FAIL {}: {}",
                        file.display(),
                        vm.2
                    ));
                }
                return SweepOutcome::StrictPass;
            }
            if known_native_failure(file).is_some() {
                let vm = run_zz_vm_token(file, &task.token);
                if vm.0 != 0 {
                    return SweepOutcome::Unexpected(format!(
                        "VM FAIL {}: {}",
                        file.display(),
                        vm.2
                    ));
                }
                let native = run_zz_native_token(file, &task.token);
                let bug = known_native_failure(file).unwrap_or("tracked native bug");
                if parity_match(&vm, &native) {
                    return SweepOutcome::Unexpected(format!(
                        "FIXED! {} — remove from known_native_failures() (was: {bug})",
                        file.display()
                    ));
                }
                return SweepOutcome::KnownFailure;
            }
            let vm = run_zz_vm_token(file, &task.token);
            let native = run_zz_native_token(file, &task.token);
            if vm.0 != 0 {
                return SweepOutcome::Unexpected(format!("VM FAIL {}: {}", file.display(), vm.2));
            }
            if native.0 != 0 {
                return SweepOutcome::Unexpected(format!(
                    "NATIVE FAIL {}: {}",
                    file.display(),
                    native.2
                ));
            }
            if !parity_match(&vm, &native) {
                return SweepOutcome::Unexpected(format!(
                    "PARITY BUG {}\n--- exits vm={} native={} ---\n--- VM stdout ---\n{}\n--- NATIVE stdout ---\n{}\n--- VM stderr ---\n{}\n--- NATIVE stderr ---\n{}",
                    file.display(),
                    vm.0,
                    native.0,
                    vm.1,
                    native.1,
                    vm.2,
                    native.2
                ));
            }
            SweepOutcome::StrictPass
        }
    }

    fn zz_files_sorted(dir: &Path) -> Vec<PathBuf> {
        if !dir.is_dir() {
            return Vec::new();
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("cannot read dir {dir:?}: {e}"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("zz"))
            .collect();
        files.sort();
        files
    }

    let fixtures = fixtures_dir();
    let mut tasks: Vec<SweepTask> = Vec::new();
    for dir_name in ["syntax", "types", "stdlib"] {
        for file in zz_files_sorted(&fixtures.join(dir_name)) {
            let token = format!("t{}", tasks.len());
            tasks.push(SweepTask {
                file,
                errors_bucket: false,
                token,
            });
        }
    }
    for file in zz_files_sorted(&fixtures.join("errors")) {
        let token = format!("t{}", tasks.len());
        tasks.push(SweepTask {
            file,
            errors_bucket: true,
            token,
        });
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(tasks.len().max(1));
    let total = tasks.len();
    let done = std::sync::atomic::AtomicU32::new(0);
    let c_strict = std::sync::atomic::AtomicU32::new(0);
    let c_known = std::sync::atomic::AtomicU32::new(0);
    let c_skipped = std::sync::atomic::AtomicU32::new(0);
    let c_unexpected = std::sync::atomic::AtomicU32::new(0);
    eprintln!("[parity-sweep] 0/{total} (strict=0 known=0 skipped=0 unexpected=0)");
    let queue = std::sync::Mutex::new(std::collections::VecDeque::from(tasks));
    let outcomes = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let task = queue.lock().unwrap().pop_front();
                let Some(task) = task else { break };
                let outcome = run_task(&task);
                match &outcome {
                    SweepOutcome::StrictPass | SweepOutcome::ErrorStrict => {
                        c_strict.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    SweepOutcome::KnownFailure | SweepOutcome::ErrorKnown => {
                        c_known.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    SweepOutcome::Skipped => {
                        c_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    SweepOutcome::Unexpected(_) => {
                        c_unexpected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                outcomes.lock().unwrap().push(outcome);
                let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if d.is_multiple_of(10) || d as usize == total {
                    eprintln!(
                        "[parity-sweep] {d}/{total} (strict={} known={} skipped={} unexpected={})",
                        c_strict.load(std::sync::atomic::Ordering::Relaxed),
                        c_known.load(std::sync::atomic::Ordering::Relaxed),
                        c_skipped.load(std::sync::atomic::Ordering::Relaxed),
                        c_unexpected.load(std::sync::atomic::Ordering::Relaxed),
                    );
                }
            });
        }
    });
    let outcomes = outcomes.into_inner().unwrap();

    let mut strict_pass = 0u32;
    let mut known_failures = 0u32;
    let mut skipped = 0u32;
    let mut err_known = 0u32;
    let mut err_strict = 0u32;
    let mut unexpected = Vec::new();
    for outcome in outcomes {
        match outcome {
            SweepOutcome::StrictPass => strict_pass += 1,
            SweepOutcome::KnownFailure => known_failures += 1,
            SweepOutcome::Skipped => skipped += 1,
            SweepOutcome::ErrorStrict => err_strict += 1,
            SweepOutcome::ErrorKnown => err_known += 1,
            SweepOutcome::Unexpected(u) => unexpected.push(u),
        }
    }
    unexpected.sort();

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

#[test]
fn mask_scratch_paths_masks_pid_token_suffixes() {
    assert_eq!(
        mask_scratch_paths("err: /tmp/zz_fs_comprehensive_12345_3/missing.txt"),
        "err: /tmp/zz_fs_comprehensive_<run>/missing.txt"
    );
    // Multibyte content around the path must survive byte-wise scanning.
    assert_eq!(
        mask_scratch_paths("───\n/tmp/zz_x_1_0/a\n───"),
        "───\n/tmp/zz_x_<run>/a\n───"
    );
    // Letter-prefixed token (the sweep uses `t{n}`).
    assert_eq!(
        mask_scratch_paths("notfound: /tmp/zz_fs_comprehensive_322462_t74/missing.txt"),
        "notfound: /tmp/zz_fs_comprehensive_<run>/missing.txt"
    );
    // No suffix: untouched.
    assert_eq!(
        mask_scratch_paths("see /tmp/zz_phase4a_nonexist.txt"),
        "see /tmp/zz_phase4a_nonexist.txt"
    );
    // Unrelated numbers untouched.
    assert_eq!(
        mask_scratch_paths("files=6 lines=1893"),
        "files=6 lines=1893"
    );
}
