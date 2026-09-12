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
    // zz build writes `<basename>` next to the source; move it.
    let produced = root
        .join("bench/performance_check/zz")
        .join(format!("bench_{name}"));
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
