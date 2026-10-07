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

use zz_cli::fixture_meta;

#[path = "batch_lists.rs"]
mod batch_lists;

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

/// Returns `Some(reason)` when a `modules/` fixture cannot run standalone.
/// `import_alias.zz` references a missing `math/utils.zz` sibling — it
/// fails on the VM itself. Broken-fixture triage belongs to the M6
/// modules milestone, not to parity.
fn module_skip_reason(file: &Path) -> Option<&'static str> {
    let stem = file.file_stem()?.to_str()?;
    match stem {
        "import_alias" => Some("broken fixture: missing math/utils.zz sibling (fails on VM too)"),
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
        // NOTE: `frame_slots` was listed here (raw zz_value in scalar
        // comparison `(v0 > 0)`) but the unboxed-calls work now emits
        // verifiably-raw comparisons, so it runs as strict parity.
        // NOTE: `struct_impl` was listed here (unboxed struct
        // returned/fielded as zz_value and vice versa) but boxing
        // struct globals (issue #215) fixed it — sweep reports FIXED.
        // NOTE: `destructuring` was listed here (top-level tuple values
        // printed empty natively) but the sweep now reports FIXED — native
        // matches the VM, so it runs as strict parity (verified 2026-10-03).
        // NOTE: `struct_embedding` was listed here (global unboxed struct
        // passed raw to display builtins) but boxing struct globals
        // (issue #215) fixed it — sweep reports FIXED.

        // --- Output differences (native runs but output differs) ---
        "concurrency_panic_test" => Some("native: panic/fail inside task closures lowers to unit (no err plumbing through zz_call_closure); VM yields .err"),
        "encoding_test" => Some("native: different error message format for bad base64/hex/url"),
        "math_extended_test" => Some("native: float precision + error message differences"),
        "decorators" => Some("DEFERRED to HIR->IR lowering: only the final marker prints; decorator wrapper output missing"),
        "extension_methods" => {
            Some("DEFERRED to HIR->IR lowering: extension-method call results missing + spurious conflict diagnostics on stderr")
        }
        // NOTE: `selective_import`, `multi_selective`, `symbol_alias`,
        // and `wildcard_import` were listed here (imported const binding
        // printed empty natively) but the sweep reports FIXED — native
        // matches the VM, so they run as strict parity.

        // NOTE: `http_tls_cert`, `main_result_err`, and
        // `pg_connect_refused` used to be listed here (native exited 0
        // via silent-unit leniency); all three now fail loudly like the
        // VM and pass as strict error fixtures.

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

/// True when `stdout` carries comparable signal after normalization.
/// Purely-numeric output is invisible to `norm_stream` (every line is
/// stripped as a potential timestamp), so a fixture whose stdout is only
/// numbers passes parity vacuously — `m00=1` vs `m00=99` must differ,
/// but `1` vs `99` cannot. Fixtures must label numeric outputs.
fn has_parity_signal(stdout: &str) -> bool {
    stdout.trim().is_empty() || !norm_stream(stdout).trim().is_empty()
}

/// Assert `stdout` carries signal (see `has_parity_signal`); panics with
/// a fix directive otherwise. Applied to every strict comparison so a
/// blind fixture fails loudly instead of passing vacuously.
fn assert_parity_signal(file: &Path, who: &str, stdout: &str) {
    assert!(
        has_parity_signal(stdout),
        "PARITY BLIND [{0}]: {who} stdout has no signal after normalization.\n\
         Label numeric outputs (e.g. `m00={{x}}`) so diffs stay visible.\nstdout:\n{stdout}",
        file.display(),
    );
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
    assert_parity_signal(file, "VM", &vm_stdout);
    assert_parity_signal(file, "native", &native_stdout);

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

/// True when `ZZ_BATCH_NATIVE=1`: the `batched_parity` target owns
/// native legs (same programs, same flags, demuxed per file).
fn batch_native() -> bool {
    std::env::var("ZZ_BATCH_NATIVE").is_ok_and(|v| v == "1")
}

/// True when this fixture's native leg is covered by a batch (only
/// then may the individual test skip it in batch mode).
fn batch_covered(path: &Path) -> bool {
    let fixtures = fixtures_dir();
    let rel = path.strip_prefix(&fixtures).unwrap_or(path);
    let mut parts = rel.components();
    let (Some(cat), Some(file)) = (
        parts.next().and_then(|c| c.as_os_str().to_str()),
        parts.next().and_then(|c| c.as_os_str().to_str()),
    ) else {
        return false;
    };
    let key = format!("{cat}/{file}");
    batch_lists::ELIGIBLE
        .iter()
        .any(|(c, f, _)| format!("{c}/{f}") == key)
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
            // Batched mode (`ZZ_BATCH_NATIVE=1`): the batched_parity
            // target owns native legs for batchable fixtures (same
            // programs, same flags, demuxed per file). Quad, error, and
            // known-failure natives always stay individual.
            if vm_only() || (batch_native() && batch_covered(&path)) {
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
parity_strict!(parity_syntax_short_circuit, "syntax", "short_circuit.zz");
parity_strict!(parity_syntax_bitwise_ops, "syntax", "bitwise_ops.zz");
parity_strict!(parity_syntax_tuple_ops, "syntax", "tuple_ops.zz");
parity_strict!(parity_syntax_destructuring, "syntax", "destructuring.zz");
parity_strict!(
    parity_syntax_tuple_unboxed_struct,
    "syntax",
    "tuple_unboxed_struct.zz"
);
parity_strict!(
    parity_syntax_compound_assign,
    "syntax",
    "compound_assign.zz"
);
parity_strict!(
    parity_syntax_generic_structs,
    "syntax",
    "generic_structs.zz"
);
parity_strict!(parity_syntax_fstrings, "syntax", "fstrings.zz");
parity_strict!(parity_syntax_brace_escapes, "syntax", "brace_escapes.zz");
parity_strict!(
    parity_syntax_brace_escapes_multiline,
    "syntax",
    "brace_escapes_multiline.zz"
);
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
// Bug 8 regression: free-function method syntax (`p.bump()` === `bump(p)`)
// must lower the receiver on both engines.
parity_strict!(parity_syntax_method_free_fn, "syntax", "method_free_fn.zz");
parity_strict!(
    parity_syntax_method_chain_recv,
    "syntax",
    "method_chain_recv.zz"
);
parity_strict!(
    parity_syntax_for_annotated_decl,
    "syntax",
    "for_annotated_decl.zz"
);
parity_strict!(
    parity_regression_scalar_global_copy,
    "regression",
    "scalar_global_copy.zz"
);
parity_strict!(
    parity_regression_shadow_destructure_fn,
    "regression",
    "shadow_destructure_fn.zz"
);
parity_strict!(
    parity_regression_shadow_destructure,
    "regression",
    "shadow_destructure.zz"
);
parity_strict!(
    parity_regression_func_capture_destructure,
    "regression",
    "func_capture_destructure.zz"
);
parity_strict!(
    parity_regression_move_append_shapes,
    "regression",
    "move_append_shapes.zz"
);
parity_strict!(
    parity_regression_move_append_field,
    "regression",
    "move_append_field.zz"
);
parity_strict!(
    parity_regression_move_append_alias,
    "regression",
    "move_append_alias.zz"
);
parity_strict!(
    parity_regression_move_append_early_exit,
    "regression",
    "move_append_early_exit.zz"
);
parity_strict!(
    parity_regression_move_append_spawn,
    "regression",
    "move_append_spawn.zz"
);
parity_strict!(
    parity_regression_move_append_append,
    "regression",
    "move_append_append.zz"
);
parity_strict!(
    parity_regression_move_append_struct_copy,
    "regression",
    "move_append_struct_copy.zz"
);
parity_strict!(
    parity_regression_closure_capture_destructure,
    "regression",
    "closure_capture_destructure.zz"
);
parity_strict!(
    parity_regression_trailing_if_in_else,
    "regression",
    "trailing_if_in_else.zz"
);
parity_strict!(
    parity_regression_match_return_in_if,
    "regression",
    "match_return_in_if.zz"
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
// M0 edge corpus: strict-parity probes (both engines agree).
parity_strict!(
    parity_regression_edge_shift_mask,
    "regression",
    "edge_shift_mask.zz"
);
// Wrap-spec PR: overflow wraps on all legs (was VM-trap errors).
parity_strict!(
    parity_regression_edge_overflow_add,
    "regression",
    "edge_int_overflow_add.zz"
);
parity_strict!(
    parity_regression_edge_overflow_mul,
    "regression",
    "edge_int_overflow_mul.zz"
);
parity_strict!(
    parity_regression_edge_neg_min,
    "regression",
    "edge_int_neg_min.zz"
);
// Write-through family: strict on all legs (see above).
parity_strict!(
    parity_regression_edge_array_alias,
    "regression",
    "edge_array_alias.zz"
);
parity_strict!(
    parity_regression_edge_dict_alias,
    "regression",
    "edge_dict_alias.zz"
);
parity_strict!(
    parity_regression_edge_temp_index_drop,
    "regression",
    "edge_temp_index_drop.zz"
);
parity_strict!(
    parity_regression_edge_field_index_store,
    "regression",
    "edge_field_index_store.zz"
);
parity_strict!(
    parity_regression_edge_struct_path_store,
    "regression",
    "edge_struct_path_store.zz"
);
parity_strict!(
    parity_regression_edge_negative_index,
    "regression",
    "edge_negative_index.zz"
);
// Evaluation order (strict: both engines agree).
parity_strict!(
    parity_regression_edge_eval_order,
    "regression",
    "edge_eval_order.zz"
);
parity_strict!(
    parity_regression_edge_compound_index_eval,
    "regression",
    "edge_compound_index_eval.zz"
);
// Order divergence (fixed by the zzc-codec slice): VM and native both
// log source order [1, 2]. Strict.
parity_strict!(
    parity_regression_edge_index_store_order,
    "regression",
    "edge_index_store_order.zz"
);
parity_strict!(
    parity_regression_edge_cast_float_int,
    "regression",
    "edge_cast_float_int.zz"
);
parity_strict!(
    parity_regression_edge_cast_str_int,
    "regression",
    "edge_cast_str_int.zz"
);
// Fixed VM bug (write-through chained index stores): strict since the fix.
parity_strict!(
    parity_regression_edge_chained_store,
    "regression",
    "edge_chained_store.zz"
);
// NaN display is strict since canonical float formatting (spec §4).
parity_strict!(
    parity_regression_edge_float_nan_display,
    "regression",
    "edge_float_nan_display.zz"
);
// Canonical float formatting conformance (spec §4.1): exact pins + legs.
parity_strict!(
    parity_regression_edge_float_format,
    "regression",
    "edge_float_format.zz"
);
// edge_cast_float_nan is strict since the flag cleanup (-ffast-math
// removal + explicit isnan guard make int(NaN) deterministically 0).
parity_strict!(
    parity_regression_edge_cast_float_nan,
    "regression",
    "edge_cast_float_nan.zz"
);
parity_strict!(
    parity_regression_edge_slice_clamp,
    "regression",
    "edge_slice_clamp.zz"
);
parity_strict_error!(parity_err_edge_min_div_neg1, "edge_int_min_div_neg1.zz");
parity_strict_error!(parity_err_edge_min_rem_neg1, "edge_int_min_rem_neg1.zz");
parity_strict_error!(parity_err_edge_pow_neg, "edge_int_pow_neg.zz");
parity_strict_error!(parity_err_edge_index_oob, "edge_index_oob.zz");
// Modules fixtures: standalone-runnable files must match on both engines
// (`import_alias.zz` excluded — broken on the VM itself, see
// `module_skip_reason`).
parity_strict!(
    parity_modules_diamond_import,
    "modules",
    "diamond_import.zz"
);
parity_strict!(
    parity_modules_multi_level_pub,
    "modules",
    "multi_level_pub.zz"
);
parity_strict!(
    parity_modules_private_struct_field_access,
    "modules",
    "private_struct_field_access.zz"
);
parity_strict!(parity_modules_pub_access, "modules", "pub_access.zz");
parity_strict!(
    parity_modules_pub_no_unused_warning,
    "modules",
    "pub_no_unused_warning.zz"
);
parity_strict!(
    parity_modules_pub_reexport_alias,
    "modules",
    "pub_reexport_alias.zz"
);
parity_strict!(
    parity_modules_pub_struct_fields,
    "modules",
    "pub_struct_fields.zz"
);
parity_strict!(
    parity_modules_pub_struct_method,
    "modules",
    "pub_struct_method.zz"
);
parity_strict!(parity_modules_reexports, "modules", "reexports.zz");
parity_strict!(
    parity_modules_shadow_pub_var,
    "modules",
    "shadow_pub_var.zz"
);
parity_strict!(
    parity_regression_branch_call_returns,
    "regression",
    "branch_call_returns.zz"
);
parity_strict!(
    parity_regression_branch_call_tails,
    "regression",
    "branch_call_tails.zz"
);
parity_strict!(
    parity_regression_branch_early_return_calls,
    "regression",
    "branch_early_return_calls.zz"
);
parity_strict!(
    parity_regression_string_accum_loop,
    "regression",
    "string_accum_loop.zz"
);
parity_strict!(
    parity_regression_string_store_across_iter,
    "regression",
    "string_store_across_iter.zz"
);
parity_strict!(
    parity_regression_string_index_set_binop,
    "regression",
    "string_index_set_binop.zz"
);

// Types
parity_strict!(
    parity_regression_fuzz_smoke_00,
    "regression",
    "fuzz_smoke_00.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_01,
    "regression",
    "fuzz_smoke_01.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_02,
    "regression",
    "fuzz_smoke_02.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_03,
    "regression",
    "fuzz_smoke_03.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_04,
    "regression",
    "fuzz_smoke_04.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_05,
    "regression",
    "fuzz_smoke_05.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_06,
    "regression",
    "fuzz_smoke_06.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_07,
    "regression",
    "fuzz_smoke_07.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_08,
    "regression",
    "fuzz_smoke_08.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_09,
    "regression",
    "fuzz_smoke_09.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_10,
    "regression",
    "fuzz_smoke_10.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_11,
    "regression",
    "fuzz_smoke_11.zz"
);
// v4 fuzzer shapes (fixed seeds; all strict on both engines).
parity_strict!(
    parity_regression_fuzz_smoke_12,
    "regression",
    "fuzz_smoke_12.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_13,
    "regression",
    "fuzz_smoke_13.zz"
);
parity_strict!(
    parity_regression_fuzz_smoke_14,
    "regression",
    "fuzz_smoke_14.zz"
);
parity_strict!(parity_types_generics, "types", "generics.zz");
parity_strict!(parity_types_type_inference, "types", "type_inference.zz");

// Stdlib
parity_strict!(parity_stdlib_strings, "stdlib", "strings.zz");
parity_strict!(
    parity_stdlib_selective_calls,
    "stdlib",
    "selective_calls.zz"
);
parity_strict!(
    parity_stdlib_generic_selective,
    "stdlib",
    "generic_selective.zz"
);
parity_strict!(
    parity_stdlib_alias_module_calls,
    "stdlib",
    "alias_module_calls.zz"
);
parity_strict!(
    parity_stdlib_str_utf8_parity,
    "stdlib",
    "str_utf8_parity.zz"
);
parity_strict!(
    parity_stdlib_vec_nested_str_parity,
    "stdlib",
    "vec_nested_str_parity.zz"
);
parity_strict!(parity_stdlib_str_find_test, "stdlib", "str_find_test.zz");
parity_strict!(parity_stdlib_str_bytes_test, "stdlib", "str_bytes_test.zz");
parity_strict!(
    parity_stdlib_str_classify_test,
    "stdlib",
    "str_classify_test.zz"
);
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
parity_strict_error!(parity_err_alias_cycle, "alias_cycle.zz");
parity_strict_error!(parity_err_alias_dup, "alias_dup.zz");
parity_strict_error!(parity_err_alias_arity, "alias_arity.zz");
parity_strict_error!(parity_err_enum_nonexhaustive, "enum_nonexhaustive.zz");
parity_strict_error!(parity_err_enum_unknown_variant, "enum_unknown_variant.zz");
parity_strict_error!(parity_err_enum_missing_payload, "enum_missing_payload.zz");
parity_strict_error!(parity_err_enum_extra_arg, "enum_extra_arg.zz");
parity_strict_error!(parity_err_enum_dup, "enum_dup.zz");
parity_strict_error!(parity_err_enum_generic_mismatch, "enum_generic_mismatch.zz");
parity_strict_error!(parity_err_undefined_var, "undefined_var.zz");
parity_strict_error!(parity_err_arity, "arity.zz");
parity_strict_error!(parity_err_parse_error, "parse_error.zz");
parity_strict_error!(parity_err_div_by_zero, "div_by_zero.zz");
// M0 edge corpus: both engines fail (messages differ; error parity
// requires failure on both sides, not identical diagnostics).
parity_strict_error!(parity_err_edge_neg_shift, "edge_neg_shift_err.zz");
parity_strict_error!(parity_err_edge_rem_zero, "edge_rem_zero_err.zz");
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
parity_strict!(parity_syntax_match_assign, "syntax", "match_assign.zz");
parity_strict!(
    parity_syntax_return_in_loops,
    "syntax",
    "return_in_loops.zz"
);

// --- control_flow: fixed (member access on non-struct type) ---
parity_strict!(parity_syntax_control_flow, "syntax", "control_flow.zz");

// --- Fixed: nested field access boxing ---
parity_strict!(parity_types_structs, "types", "structs.zz");
parity_strict!(parity_types_aliases, "types", "aliases.zz");
parity_strict!(parity_types_alias_import, "types", "alias_import.zz");
parity_strict!(parity_types_enums, "types", "enums.zz");
parity_strict!(parity_types_enum_import, "types", "enum_import.zz");
parity_strict!(parity_types_enum_generics, "types", "enum_generics.zz");
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
parity_strict!(parity_stdlib_map_set, "stdlib", "map_set.zz");
parity_strict!(parity_stdlib_dec_ops, "stdlib", "dec_ops.zz");
parity_strict!(parity_stdlib_csv_test, "stdlib", "csv_test.zz");
parity_strict!(parity_stdlib_builders, "stdlib", "builders.zz");
parity_strict!(parity_stdlib_time_date, "stdlib", "time_date.zz");
parity_known_failure!(
    parity_stdlib_math_extended,
    "stdlib",
    "math_extended_test.zz"
);
parity_strict!(parity_stdlib_jsonmod, "stdlib", "jsonmod.zz");
parity_strict!(parity_stdlib_json_test, "stdlib", "json_test.zz");
parity_strict!(
    parity_stdlib_json_extended_test,
    "stdlib",
    "json_extended_test.zz"
);
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
// Quad matrix (pre-M1): VM-debug x VM-release x native-O0 x native-O3.
//
// The dual-engine macros above compare one VM (test-profile) against one
// native (release flags). The quad runs every `regression/` fixture plus
// every `errors/edge_*` probe through all four legs so profile- and
// opt-level-only divergences (overflow wrap-vs-trap, -ffast-math NaN
// folding) are pinned before the M1 spec decides them.
// ===========================================================================

/// Source stamp for the release-VM driver: current HEAD plus the dirty
/// state of `crates/`. The quad compares it against a stamp file next
/// to the binary and rebuilds on mismatch — otherwise a stale release
/// driver silently tests old code (e.g. a spec fix verified only on the
/// debug legs while `vm_rel` still runs yesterday's binary).
fn release_src_stamp(workspace: &Path) -> Option<String> {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    let dirty = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .arg("crates/zz_runtime/src")
        .arg("crates/zz_checker/src")
        .arg("crates/zz_frontend/src")
        .arg("crates/zz_hir/src")
        .arg("crates/zz_stdlib/src")
        .arg("crates/zz_codegen/src")
        .arg("crates/zz_cli/src")
        .arg("crates/zz_native_rt/src")
        .arg("Cargo.toml")
        .arg("Cargo.lock")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            o.stdout.hash(&mut h);
            format!("{:x}", h.finish())
        })
        .unwrap_or_default();
    Some(format!("{head}:{dirty}"))
}

/// Resolve the release-VM `zz` driver, building it once when missing.
///
/// `ZZ_VM_RELEASE_BIN` overrides; otherwise
/// `<workspace>/target/release/zz[.exe]`. A stamp file next to the
/// binary records the source revision it was built from; a mismatch
/// triggers a rebuild (a stale release driver silently testing old code
/// is a false-signal machine). Parallel tests serialize on a lock dir;
/// a failed build panics loudly (never a silent skip — the release-VM
/// leg is a required part of the matrix).
fn release_vm_bin() -> PathBuf {
    static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        if let Some(p) = std::env::var_os("ZZ_VM_RELEASE_BIN") {
            let p = PathBuf::from(p);
            assert!(p.is_file(), "ZZ_VM_RELEASE_BIN missing: {}", p.display());
            return p;
        }
        let exe = format!("zz{}", std::env::consts::EXE_SUFFIX);
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let p = workspace.join("target/release").join(&exe);
        let stamp_file = workspace.join("target/release/.zz-quad-src-stamp");
        let fresh = p.is_file()
            && match (
                release_src_stamp(&workspace),
                std::fs::read_to_string(&stamp_file),
            ) {
                (Some(cur), Ok(saved)) => cur == saved.trim(),
                // No git info (e.g. tarball builds): trust the binary.
                (None, _) => true,
                _ => false,
            };
        if fresh {
            return p;
        }
        // Serialized one-time build: the lock dir makes concurrent test
        // threads (and a concurrent `cargo build --release`) take turns.
        // Stale locks (crashed builder) older than 30 minutes are reaped
        // — otherwise one crash wedges every later run forever.
        let lock =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/.zz-quad-release-build.lock");
        loop {
            match std::fs::create_dir(&lock) {
                Ok(()) => break,
                Err(_) => {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .map(|t| {
                            t.elapsed().unwrap_or_default() > std::time::Duration::from_secs(1800)
                        })
                        .unwrap_or(true);
                    if stale {
                        let _ = std::fs::remove_dir_all(&lock);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
        let built = (|| {
            if p.is_file() {
                // Re-check under the lock: another waiter may have built
                // (and stamped) while we queued.
                if let (Some(cur), Ok(saved)) = (
                    release_src_stamp(&workspace),
                    std::fs::read_to_string(&stamp_file),
                ) {
                    if cur == saved.trim() {
                        return true;
                    }
                } else if release_src_stamp(&workspace).is_none() && p.is_file() {
                    return true;
                }
            }
            eprintln!("[quad] building release zz driver (one-time cost)...");
            let out = std::process::Command::new("cargo")
                .arg("build")
                .arg("--release")
                .arg("-p")
                .arg("zz_cli")
                .current_dir(&workspace)
                .output();
            match out {
                Ok(o) if o.status.success() && p.is_file() => {
                    if let Some(cur) = release_src_stamp(&workspace) {
                        let _ = std::fs::write(&stamp_file, &cur);
                    }
                    true
                }
                Ok(o) => {
                    eprintln!(
                        "[quad] release build failed:\n{}",
                        String::from_utf8_lossy(&o.stderr)
                    );
                    false
                }
                Err(e) => {
                    eprintln!("[quad] cannot run cargo: {e}");
                    false
                }
            }
        })();
        let _ = std::fs::remove_dir(&lock);
        assert!(
            built,
            "quad needs target/release/zz: run `cargo build --release -p zz_cli`"
        );
        p
    })
    .clone()
}

/// True when native legs must be skipped (Windows CI: no AOT backend).
fn quad_skip_native() -> bool {
    std::env::var("ZZ_SKIP_NATIVE").is_ok()
}

/// One matrix leg outcome: (exit, stdout, stderr).
type LegOut = (i32, String, String);

/// Run a fixture through one matrix leg. `driver` is the `zz` binary,
/// `native` selects `run --native`, `dev` selects `-O0 -g` clang output
/// (`ZZ_NATIVE_DEV=1`) instead of the default release flags.
fn run_quad_leg(
    driver: &Path,
    native: bool,
    dev: bool,
    file: &Path,
    input: &[u8],
    token: &str,
) -> LegOut {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(driver);
    cmd.arg("run");
    if native {
        cmd.arg("--native");
    }
    cmd.arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    cmd.env("ZZ_SWEEP_TOKEN", token);
    cmd.env("ZZ_CRYPTO_FAST", "1");
    if dev {
        cmd.env("ZZ_NATIVE_DEV", "1");
    }
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to exec quad leg {file:?}: {e}"));
    if !input.is_empty() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(input);
        }
    }
    drop(child.stdin.take());
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(ZZ_CHILD_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let output = child.wait_with_output().expect("reap quad leg");
                    return (
                        ZZ_CHILD_TIMEOUT_EXIT,
                        String::from_utf8_lossy(&output.stdout).to_string(),
                        format!(
                            "{}\nQUAD TIMEOUT: {} exceeded {ZZ_CHILD_TIMEOUT_SECS}s",
                            String::from_utf8_lossy(&output.stderr),
                            file.display()
                        ),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("failed to wait quad leg {file:?}: {e}"),
        }
    }
    let output = child.wait_with_output().expect("wait quad leg");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// All four legs for one fixture: (vm_debug, vm_release, native_dev, native_release).
struct QuadLegs {
    vm_dbg: LegOut,
    vm_rel: LegOut,
    nat_dev: Option<LegOut>,
    nat_rel: Option<LegOut>,
}

fn run_quad(file: &Path, token: &str) -> QuadLegs {
    let debug_bin = PathBuf::from(env!("CARGO_BIN_EXE_zz"));
    let input = stdin_for(file);
    let vm_dbg = run_quad_leg(&debug_bin, false, false, file, &input, token);
    let vm_rel = run_quad_leg(&release_vm_bin(), false, false, file, &input, token);
    let (nat_dev, nat_rel) = if quad_skip_native() || vm_only() {
        eprintln!(
            "SKIP native legs for {} (fast/Windows mode)",
            file.display()
        );
        (None, None)
    } else {
        (
            Some(run_quad_leg(&debug_bin, true, true, file, &input, token)),
            Some(run_quad_leg(&debug_bin, true, false, file, &input, token)),
        )
    };
    QuadLegs {
        vm_dbg,
        vm_rel,
        nat_dev,
        nat_rel,
    }
}

fn fmt_leg(name: &str, leg: &LegOut) -> String {
    format!(
        "--- {name} (exit {}) ---\nstdout:\n{}\nstderr:\n{}",
        leg.0, leg.1, leg.2
    )
}

fn fmt_quad(file: &Path, q: &QuadLegs) -> String {
    let mut s = format!("QUAD DUMP [{}]:\n", file.display());
    s.push_str(&fmt_leg("vm_dbg", &q.vm_dbg));
    s.push('\n');
    s.push_str(&fmt_leg("vm_rel", &q.vm_rel));
    if let Some(l) = &q.nat_dev {
        s.push('\n');
        s.push_str(&fmt_leg("nat_dev", l));
    }
    if let Some(l) = &q.nat_rel {
        s.push('\n');
        s.push_str(&fmt_leg("nat_rel", l));
    }
    s
}

/// Assert all available legs pairwise-match (exit + normalized streams).
fn assert_quad_agree(file: &Path, q: &QuadLegs) {
    let mut legs: Vec<(&str, &LegOut)> = vec![("vm_dbg", &q.vm_dbg), ("vm_rel", &q.vm_rel)];
    if let Some(l) = &q.nat_dev {
        legs.push(("nat_dev", l));
    }
    if let Some(l) = &q.nat_rel {
        legs.push(("nat_rel", l));
    }
    // Every leg must carry a parity signal: purely-numeric stdout is
    // invisible to normalization (see `strip_numeric_lines`), so a
    // fixture with no surviving signal passes vacuously. Force labeled
    // output instead.
    for (name, leg) in &legs {
        if !leg.1.trim().is_empty() && norm_stream(&leg.1).trim().is_empty() {
            panic!(
                "QUAD BLIND [{0}]: leg {name} stdout has no parity signal after normalization.\n\
                 Label numeric outputs (e.g. `m00={{x}}`) so diffs stay visible.\n{1}",
                file.display(),
                fmt_quad(file, q)
            );
        }
    }
    for (i, (an, a)) in legs.iter().enumerate() {
        for (bn, b) in &legs[i + 1..] {
            assert!(
                parity_match(a, b),
                "QUAD BUG [{0}]: legs {an} and {bn} differ.\n{1}",
                file.display(),
                fmt_quad(file, q)
            );
        }
    }
}

/// Assert every available leg failed.
fn assert_quad_all_fail(file: &Path, q: &QuadLegs) {
    let legs: Vec<(&str, &LegOut)> = {
        let mut v = vec![("vm_dbg", &q.vm_dbg), ("vm_rel", &q.vm_rel)];
        if let Some(l) = &q.nat_dev {
            v.push(("nat_dev", l));
        }
        if let Some(l) = &q.nat_rel {
            v.push(("nat_rel", l));
        }
        v
    };
    for (name, leg) in &legs {
        assert_ne!(
            leg.0,
            0,
            "QUAD BUG [{0}]: leg {name} should fail but exited 0.\n{1}",
            file.display(),
            fmt_quad(file, q)
        );
    }
}

/// Documented per-fixture splits: the M1 decision list in executable form.
/// The list is currently EMPTY — every prior split (overflow, MIN/-1,
/// pow-neg, OOB, NaN display, scalar-global) has been promoted to
/// AllAgree/AllFail by its fix PR. New divergences gain an arm here with
/// the exact leg behavior observed and decided; any leg that unexpectedly
/// agrees (a fix!) panics with FIXED so the entry is promoted.
fn assert_quad_split(_file: &Path, stem: &str, _q: &QuadLegs) {
    panic!("quad has no documented split for {stem} — add AllAgree or an arm");
}

/// Stems with a documented quad split (anything else in scope must agree
/// on all legs, or fail on all legs for `errors/`).
/// Documented per-fixture splits: currently EMPTY — every prior split
/// (overflow, MIN/-1, pow-neg, OOB, NaN display, scalar-global, and
/// index-store order) has been promoted to AllAgree/AllFail by its fix
/// PR. New divergences gain an arm here with the exact leg behavior
/// observed and decided; any leg that unexpectedly agrees (a fix!)
/// panics with FIXED so the entry is promoted.
fn quad_split_stem(_stem: &str) -> bool {
    false
}

/// Sorted `.zz` files directly under `dir` (non-recursive).
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

#[test]
fn quad_regression_and_edge_matrix() {
    let fixtures = fixtures_dir();
    let mut files: Vec<PathBuf> = Vec::new();
    for f in zz_files_sorted(&fixtures.join("regression")) {
        files.push(f);
    }
    for f in zz_files_sorted(&fixtures.join("errors")) {
        if f.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("edge_"))
        {
            files.push(f);
        }
    }
    files.sort();
    assert!(!files.is_empty(), "quad found no fixtures");

    // Parallel workers: this was one sequential loop (63 fixtures × up
    // to 4 process spawns = minutes idle cores). Each fixture runs in
    // its own child processes with a unique sweep token, so fixtures
    // are fully isolated and order-independent. Results fold in index
    // order, so the summary and verdicts match the sequential run.
    // `ZZ_QUAD_JOBS` overrides the default (kept modest: native legs
    // spawn clang, and RAM-thin hosts swap past ~4 concurrent builds).
    let n_workers: usize = std::env::var("ZZ_QUAD_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(4)
        .min(files.len().max(1));
    /// Per-fixture outcome for the ordered fold below.
    enum Outcome {
        StrictOk,
        SplitOk,
        AllFailOk,
        Failed(String),
    }
    let slots: Vec<std::sync::Mutex<Option<Outcome>>> = (0..files.len())
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..n_workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= files.len() {
                    break;
                }
                let file = &files[i];
                let token = format!("q{i}");
                let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                let in_errors = file
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    == Some("errors");
                let split_arm = quad_split_stem(stem);
                let q = run_quad(file, &token);
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if split_arm {
                        assert_quad_split(file, stem, &q);
                    } else if in_errors {
                        assert_quad_all_fail(file, &q);
                    } else {
                        assert_quad_agree(file, &q);
                    }
                }));
                let outcome = match r {
                    Ok(()) => {
                        if split_arm {
                            Outcome::SplitOk
                        } else if in_errors {
                            Outcome::AllFailOk
                        } else {
                            Outcome::StrictOk
                        }
                    }
                    Err(e) => Outcome::Failed(
                        e.downcast_ref::<String>()
                            .cloned()
                            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_else(|| "non-string panic".to_string()),
                    ),
                };
                *slots[i].lock().expect("quad slot") = Some(outcome);
            });
        }
    });
    // Ordered fold: identical summary and verdicts to the old loop.
    let mut failures: Vec<String> = Vec::new();
    let mut strict = 0u32;
    let mut split = 0u32;
    let mut allfail = 0u32;
    for slot in &slots {
        match slot.lock().expect("quad slot").take() {
            Some(Outcome::StrictOk) => strict += 1,
            Some(Outcome::SplitOk) => split += 1,
            Some(Outcome::AllFailOk) => allfail += 1,
            Some(Outcome::Failed(msg)) => failures.push(msg),
            None => failures.push("quad worker left fixture unrun".to_string()),
        }
    }
    println!("\n=== Quad Matrix Summary ===");
    println!("All-agree: {strict}");
    println!("Documented splits: {split}");
    println!("All-fail (errors): {allfail}");
    println!("Failures: {}", failures.len());
    for f in &failures {
        println!("---\n{f}");
    }
    assert!(failures.is_empty(), "{} quad failure(s)", failures.len());
}

// ===========================================================================
// Exhaustive parity sweep: EVERY fixture through BOTH engines.
//
// This is the strict gate — not documentation. Any `.zz` file under
// tests/fixtures/{syntax,types,stdlib,regression,modules,errors} runs
// here with no registration needed (modules/import_alias excluded via
// `module_skip_reason`), so a new fixture (or a regression in an old
// one) cannot slip past the per-file macros above. Stdin comes from the
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
            if module_skip_reason(file).is_some() {
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
            if !has_parity_signal(&vm.1) || !has_parity_signal(&native.1) {
                return SweepOutcome::Unexpected(format!(
                    "PARITY BLIND {}: stdout has no signal after normalization — label numeric outputs",
                    file.display()
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

    let fixtures = fixtures_dir();
    let mut tasks: Vec<SweepTask> = Vec::new();
    // M0: all `zz run` fixture dirs. `modules/` stays out: most fixtures
    // there are multi-file/import-layout cases that do not run standalone
    // (`import_alias.zz` fails on the VM itself); they are tagged for the
    // M6 modules milestone instead. `test/` stays out: it needs the
    // `zz test` runner, not `zz run` (covered by e2e_test_success!).
    for dir_name in ["syntax", "types", "stdlib", "regression", "modules"] {
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

// ---------------------------------------------------------------------------
// Normalizer audit (pre-M1 item 2): what normalization must and must not do
// ---------------------------------------------------------------------------

/// Negative tests: these pairs must NEVER compare equal. Each is a past
/// or plausible divergence that normalization must preserve.
#[test]
fn norm_keeps_value_differences() {
    // The chained-store class: labeled values differ.
    assert!(!parity_match(
        &(0, "m00=1\nedge_ok\n".to_string(), String::new()),
        &(0, "m00=99\nedge_ok\n".to_string(), String::new()),
    ));
    // Float spelling: NaN vs nan.
    assert!(!parity_match(
        &(0, "NaN\nok\n".to_string(), String::new()),
        &(0, "nan\nok\n".to_string(), String::new()),
    ));
    // Exit codes always matter, even with identical streams.
    assert!(!parity_match(
        &(1, String::new(), "error".to_string()),
        &(0, String::new(), "error".to_string()),
    ));
    // stderr participates: same stdout, different diagnostics.
    assert!(!parity_match(
        &(0, "ok\n".to_string(), "warn a".to_string()),
        &(0, "ok\n".to_string(), "warn b".to_string()),
    ));
}

/// Positive tests: benign run-to-run variation must still normalize away.
#[test]
fn norm_still_normalizes() {
    // Scratch paths with different pid/token runs.
    assert!(parity_match(
        &(
            0,
            "err: /tmp/zz_fs_x_123_4/missing\nok\n".to_string(),
            String::new()
        ),
        &(
            0,
            "err: /tmp/zz_fs_x_999_t7/missing\nok\n".to_string(),
            String::new()
        ),
    ));
    // Ephemeral ports.
    assert!(parity_match(
        &(0, "listen 127.0.0.1:54321\nok\n".to_string(), String::new()),
        &(0, "listen 127.0.0.1:12345\nok\n".to_string(), String::new()),
    ));
    // Purely numeric lines (timestamps) drop on both sides equally.
    assert!(parity_match(
        &(0, "1728000000000\nok\n".to_string(), String::new()),
        &(0, "1728000000001\nok\n".to_string(), String::new()),
    ));
}

/// The parity-signal gate: numeric-only stdout is blind and must be
/// rejected so fixtures label their outputs.
#[test]
fn signal_gate_catches_numeric_only() {
    assert!(has_parity_signal(""));
    assert!(has_parity_signal("m00=1\nedge_ok\n"));
    assert!(!has_parity_signal("1\n99\n"));
    assert!(!has_parity_signal("42\n"));
}

/// Harness/tag consistency: the known-failure + skip lists and the
/// `known-divergence` / `vm-only` / `nondeterministic` tags must agree.
/// A divergence tracked in only one place is a lie in the other.
#[test]
fn known_lists_match_fixture_tags() {
    let fixtures = fixtures_dir();
    let mut problems = Vec::new();
    for dir_name in [
        "syntax",
        "types",
        "stdlib",
        "regression",
        "modules",
        "errors",
    ] {
        let dir = fixtures.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read fixture dir")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("zz"))
            .collect();
        files.sort();
        for file in &files {
            let tags = fixture_meta::fixture_features(file).unwrap_or_else(|e| panic!("{e}"));
            let known = known_native_failure(file).is_some();
            let skipped = native_skip_reason(file).is_some() || module_skip_reason(file).is_some();
            let tagged_diverged = tags
                .iter()
                .any(|t| t == "known-divergence" || t == "vm-only");
            let tagged_nondet = tags.iter().any(|t| t == "nondeterministic");
            if tagged_diverged && !known {
                problems.push(format!(
                    "{}: tagged diverged but missing from known_native_failure",
                    file.display()
                ));
            }
            if known && !tagged_diverged {
                problems.push(format!(
                    "{}: in known_native_failure but missing known-divergence/vm-only tag",
                    file.display()
                ));
            }
            if tagged_nondet && !skipped {
                problems.push(format!(
                    "{}: tagged nondeterministic but not skipped by the harness",
                    file.display()
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "tag/harness drift:\n{}",
        problems.join("\n")
    );
}
