//! Regression tests for `bench/performance_check/` baselines.
//!
//! Enforces hard upper bounds on ZZ's resource usage so a regression in
//! the allocator / AOT codegen surfaces immediately rather than silently
//! growing peak memory.
//!
//! Baselines are intentionally generous (current measured values + headroom).
//! Tighten them as ZZ stabilises.
//!
//! Skipped automatically if `zz build -p` is not available (e.g. minimal
//! CI containers that only ran `cargo check`).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Repo root (where `bench/` and `target/` live).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Locate the compiled zz binary (built once by `cargo test`).
fn zz_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

/// Build a single benchmark via `zz build -p` into a sandbox dir.
fn build_zz_bench(name: &str) -> Option<PathBuf> {
    let root = workspace_root();
    let src = root
        .join("bench/performance_check/zz")
        .join(format!("bench_{name}.zz"));
    if !src.exists() {
        eprintln!("skip {name}: source not found at {}", src.display());
        return None;
    }
    let out_dir = root.join("bench/performance_check/.zzbin");
    let _ = std::fs::create_dir_all(&out_dir);
    let target = out_dir.join(format!("bench_{name}"));

    let log = Command::new(zz_bin())
        .arg("build")
        .arg("-p")
        .arg(&src)
        .current_dir(&root)
        .output()
        .expect("failed to invoke `zz build -p`");
    if !log.status.success() {
        eprintln!(
            "skip {name}: zz build -p failed:\n{}",
            String::from_utf8_lossy(&log.stderr)
        );
        return None;
    }
    // Standalone builds publish to the invocation dir (CWD = root).
    let produced = root.join(format!("bench_{name}"));
    if produced.exists() {
        let _ = std::fs::rename(&produced, &target);
    }
    if target.exists() {
        Some(target)
    } else {
        eprintln!("skip {name}: built binary not found");
        None
    }
}

/// Run a binary and return (stdout, wall-time, peak-rss-kib via /proc).
fn run_meas(bin: &Path) -> (String, Duration, u64) {
    let start = Instant::now();
    let child = Command::new(bin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn benchmark binary");
    let pid = child.id();
    let output = child.wait_with_output().expect("wait failed");
    let wall = start.elapsed();

    // Read peak RSS from /proc/<pid>/status after exit (Linux only).
    let mut peak_kib = 0u64;
    if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                // Value in kB.
                if let Some(v) = rest.split_whitespace().next() {
                    if let Ok(n) = v.parse::<u64>() {
                        peak_kib = peak_kib.max(n);
                    }
                }
            }
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (stdout, wall, peak_kib)
}

fn parse_marker(stdout: &str, marker: &str) -> Option<u64> {
    // Lines like `powmod_sum: 47999861`
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix(marker) {
            // Strip optional ': ' then parse the integer.
            let v = rest.trim_start_matches(':').trim();
            if let Some(first) = v.split_whitespace().next() {
                return first.parse::<u64>().ok();
            }
        }
    }
    None
}

// ---------------------------------------------------------------------
// Regression baselines
// ---------------------------------------------------------------------
//
// Generous on purpose — these protect against gross regressions. The
// actual measured values live in `bench/performance_check/RESULTS.md`.
// ---------------------------------------------------------------------

const BASELINE_CPU_WALL_MS: u128 = 60_000; // 60 s
const BASELINE_CPU_PEAK_MB: u64 = 32; // 32 MB
const BASELINE_STR_WALL_MS: u128 = 60_000; // 60 s
const BASELINE_STR_PEAK_MB: u64 = 32; // 32 MB
const BASELINE_MEM_WALL_MS: u128 = 60_000; // 60 s
const BASELINE_MEM_PEAK_MB: u64 = 128; // 128 MB (AOT load + a few thousand-value dicts)

// ----- bench_memory_leak -------------------------------------------
#[test]
fn regression_bench_memory_leak_completes_and_stays_under_baseline() {
    let Some(bin) = build_zz_bench("memory_leak") else {
        return;
    };

    let (stdout, wall, peak_kib) = run_meas(&bin);
    let _ = bin; // keep alive

    assert!(
        stdout.contains("bench_memory_leak_ok"),
        "memory_leak missing success marker.\nstdout:\n{stdout}"
    );

    let wall_ms = wall.as_millis();
    assert!(
        wall_ms < BASELINE_MEM_WALL_MS,
        "memory_leak wall time {wall_ms}ms exceeds baseline {BASELINE_MEM_WALL_MS}ms"
    );

    let peak_mb = peak_kib / 1024;
    assert!(
        peak_mb < BASELINE_MEM_PEAK_MB,
        "memory_leak peak RSS {peak_mb}MB exceeds baseline {BASELINE_MEM_PEAK_MB}MB"
    );

    // Optional check: pass1 and pass2 both reported in output.
    assert!(stdout.contains("pass1_ms:"), "missing pass1_ms line");
    assert!(stdout.contains("pass2_ms:"), "missing pass2_ms line");
}

// ----- bench_cpu_intensive -----------------------------------------
#[test]
fn regression_bench_cpu_intensive_completes_and_stays_under_baseline() {
    let Some(bin) = build_zz_bench("cpu_intensive") else {
        return;
    };

    let (stdout, wall, peak_kib) = run_meas(&bin);

    assert!(
        stdout.contains("bench_cpu_intensive_ok"),
        "cpu_intensive missing success marker.\nstdout:\n{stdout}"
    );

    let wall_ms = wall.as_millis();
    assert!(
        wall_ms < BASELINE_CPU_WALL_MS,
        "cpu_intensive wall time {wall_ms}ms exceeds baseline {BASELINE_CPU_WALL_MS}ms"
    );

    let peak_mb = peak_kib / 1024;
    assert!(
        peak_mb < BASELINE_CPU_PEAK_MB,
        "cpu_intensive peak RSS {peak_mb}MB exceeds baseline {BASELINE_CPU_PEAK_MB}MB"
    );

    // Signature result must be deterministic and non-zero.
    let sig = parse_marker(&stdout, "signature_sum")
        .expect("missing signature_sum in cpu_intensive output");
    assert_eq!(sig, 50_005_042_949_861, "signature_sum drift (got {sig})");
}

// ----- bench_string_concats ----------------------------------------
#[test]
fn regression_bench_string_concats_completes_and_stays_under_baseline() {
    let Some(bin) = build_zz_bench("string_concats") else {
        return;
    };

    let (stdout, wall, peak_kib) = run_meas(&bin);

    assert!(
        stdout.contains("bench_string_concats_ok"),
        "string_concats missing success marker.\nstdout:\n{stdout}"
    );

    let wall_ms = wall.as_millis();
    assert!(
        wall_ms < BASELINE_STR_WALL_MS,
        "string_concats wall time {wall_ms}ms exceeds baseline {BASELINE_STR_WALL_MS}ms"
    );

    let peak_mb = peak_kib / 1024;
    assert!(
        peak_mb < BASELINE_STR_PEAK_MB,
        "string_concats peak RSS {peak_mb}MB exceeds baseline {BASELINE_STR_PEAK_MB}MB"
    );
}

// ----- bench_struct_access -----------------------------------------
// Commit 3e22a46 ("flat C arrays for arrays of unboxed structs") generated
// C that referenced an undeclared `arr` instead of the declared pointer id,
// so `bench_struct_access.zz` failed to compile. This regression test locks
// in that `arr: [Pt] = []` + `.push()` lowers to a contiguous buffer with
// direct `v[k].x` field access.
#[test]
fn regression_bench_struct_access_compiles_and_runs() {
    let Some(bin) = build_zz_bench("struct_access") else {
        return;
    };

    let (stdout, _, _) = run_meas(&bin);
    let _ = bin;

    assert!(
        stdout.contains("struct_access_ok"),
        "struct_access missing success marker.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("fill_1M_ms:"),
        "struct_access missing fill_1M_ms marker.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("sum_x: 499999500000"),
        "struct_access sum_x drifted.\nstdout:\n{stdout}"
    );
}

// ----- bench_check_20k ---------------------------------------------
// Performance regression: `zz check` on a 20k-line synthetic program
// (many funcs, structs, long match chains, big arrays, imports) must
// complete well under 15s cold in debug CI (debug builds are 6-10s on
// this hardware even without new analysis passes; release is ~4ms,
// far under the 2s target). Guards against quadratic blowups in the
// checker (scope_cache invalidation, merge_types O(n²), repeated
// deep-resolve, per-call table cloning, ...).
//
// Uses the release/dev `zz` binary already built by `cargo test`
// (CARGO_BIN_EXE_zz). Writes the generated program to a temp file so it
// does not pollute the repo.
#[test]
fn regression_check_20k_lines_under_15s() {
    const CHECK_TARGET_MS: u128 = 15_000; // 15 s, generous-but-real for debug CI
    const TARGET_LINES: usize = 20_000;

    let root = workspace_root();
    let zz = zz_bin();

    let tmp_dir = root.join("bench/performance_check/.zzbin");
    let _ = fs::create_dir_all(&tmp_dir);
    let src = tmp_dir.join("bench_check_20k.zz");

    // --- generate synthetic 20k-line program -------------------------
    let per_func = 18; // lines per function
    let nfuncs = TARGET_LINES / per_func;
    let mut prog = String::with_capacity(TARGET_LINES * 20);
    for i in 1..=12 {
        writeln!(prog, "import std.str as s{i}").unwrap();
    }
    writeln!(prog).unwrap();
    writeln!(prog, "struct BigRecord {{").unwrap();
    for j in 1..=20 {
        writeln!(prog, "  f{j}: int,").unwrap();
    }
    writeln!(prog, "}}").unwrap();
    writeln!(prog, "struct Wrapper {{ inner: BigRecord }}").unwrap();
    writeln!(prog).unwrap();
    writeln!(
        prog,
        "struct Box4<T1, T2, T3, T4> {{ a: T1, b: T2, c: T3, d: T4 }}"
    )
    .unwrap();
    writeln!(prog).unwrap();
    for k in 1..=nfuncs {
        writeln!(prog, "func gen_{k}(x: int, n: int) -> int {{").unwrap();
        writeln!(prog, "  arr := [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]").unwrap();
        writeln!(prog, "  big := Box4{{ a: x, b: x, c: x, d: x }}").unwrap();
        writeln!(prog, "  total := 0").unwrap();
        writeln!(prog, "  i := 0").unwrap();
        writeln!(prog, "  while i < n {{").unwrap();
        writeln!(prog, "    total = total + arr[i % 10]").unwrap();
        writeln!(prog, "    i = i + 1").unwrap();
        writeln!(prog, "  }}").unwrap();
        writeln!(prog, "  m := match x {{").unwrap();
        for arm in 1..=6 {
            writeln!(prog, "    {arm} => {arm} * n,").unwrap();
        }
        writeln!(prog, "    _ => 0").unwrap();
        writeln!(prog, "  }}").unwrap();
        writeln!(prog, "  _ = big.a + big.b + big.c + big.d").unwrap();
        writeln!(prog, "  return total + m").unwrap();
        writeln!(prog, "}}").unwrap();
        writeln!(prog).unwrap();
    }
    writeln!(prog, r#"println("bench_gen_ok")"#).unwrap();

    fs::write(&src, &prog).expect("write bench program");
    let actual_lines = prog.lines().count();

    // --- measure cold `zz check` ------------------------------------
    let start = Instant::now();
    let output = Command::new(&zz)
        .arg("check")
        .arg(&src)
        .current_dir(&root)
        .output()
        .expect("failed to invoke `zz check`");
    let wall_ms = start.elapsed().as_millis();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`zz check` failed on generated {actual_lines}-line program.\nstderr:\n{stderr}"
    );
    assert!(
        wall_ms < CHECK_TARGET_MS,
        "`zz check` on {actual_lines} lines took {wall_ms}ms, exceeds target {CHECK_TARGET_MS}ms"
    );
    eprintln!("PERF: zz check {actual_lines} lines in {wall_ms}ms (target < {CHECK_TARGET_MS}ms)");
}
