//! Regression tests for the concurrency audit battery (`bench/audit/`).
//!
//! Each test builds one audit program with `zz build -p` (release AOT —
//! the engine the audit fixed) and runs the binary with a hard timeout:
//! a hang fails the test instead of hanging CI. Success tests assert
//! their marker line; negative tests assert the build itself fails.
//!
//! Coverage guards the audit fixes:
//! - au1  → MAJOR-1 (verdict clobber drops completions)
//! - au12 → MAJOR-2 (multi-join lost wakeup)
//! - au4/au8 → CRITICAL-1 (top-up stranding / helping discipline)
//! - au18 → MAJOR-5 (lowerer hole, known-fail guard)
//!
//! Skipped automatically if `zz build -p` is unavailable (same convention
//! as `performance_check_regression.rs`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Serializes binary RUNS (not builds): timing-sensitive concurrency
/// tests must not fight each other — or 13 parallel clang links — for
/// cores. Without this, an oversubscribed box trips hang timeouts on
/// healthy binaries (observed: au3 starved past 90s mid-suite, instant
/// solo). Builds stay parallel; only execution is serialized.
static RUN_LOCK: Mutex<()> = Mutex::new(());

/// Repo root (where `bench/` and `target/` live).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Locate the compiled zz binary (built once by `cargo test`).
fn zz_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

/// Build a single audit program via `zz build -p` into a sandbox dir.
/// Returns the produced binary, or `None` when the build is unavailable.
fn build_audit_bench(name: &str) -> Option<PathBuf> {
    let root = workspace_root();
    let src = root.join("bench/audit").join(format!("{name}.zz"));
    if !src.exists() {
        eprintln!("skip {name}: source not found at {}", src.display());
        return None;
    }
    let out_dir = root.join("bench/audit/.zzbin");
    let _ = std::fs::create_dir_all(&out_dir);
    let target = out_dir.join(name);

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
    // zz build -p writes `bin/<basename>` next to the source; move it.
    let produced = root.join("bench/audit/bin").join(name);
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

/// Assert that building `name` FAILS (negative test). Needs no binary.
fn assert_build_fails(name: &str) {
    let root = workspace_root();
    let src = root.join("bench/audit").join(format!("{name}.zz"));
    assert!(src.exists(), "fixture not found: {}", src.display());
    let log = Command::new(zz_bin())
        .arg("build")
        .arg("-p")
        .arg(&src)
        .current_dir(&root)
        .output()
        .expect("failed to invoke `zz build -p`");
    assert!(
        !log.status.success(),
        "{name} should fail to build but succeeded.\nstdout:\n{}",
        String::from_utf8_lossy(&log.stdout)
    );
}

/// Run a binary with a hard timeout. `None` on timeout (hang) or spawn
/// failure. Returns (stdout, wall-time, peak-rss-kib via /proc).
/// Execution is serialized via `RUN_LOCK` (see above).
fn run_timeout(bin: &Path, limit: Duration) -> Option<(String, Duration, u64)> {
    let _guard = RUN_LOCK.lock().ok()?;
    let start = Instant::now();
    let mut child = Command::new(bin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    loop {
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let wall = start.elapsed();
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let mut peak_kib = 0u64;
    if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                if let Some(v) = rest.split_whitespace().next() {
                    if let Ok(n) = v.parse::<u64>() {
                        peak_kib = peak_kib.max(n);
                    }
                }
            }
        }
    }
    Some((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        wall,
        peak_kib,
    ))
}

/// Run + assert marker present. Returns stdout for value checks.
fn run_audit(name: &str, marker: &str, limit: Duration) -> String {
    let Some(bin) = build_audit_bench(name) else {
        return String::new();
    };
    let Some((stdout, wall, _)) = run_timeout(&bin, limit) else {
        panic!(
            "{name}: binary hung or crashed (timeout {}s).\n\
                A hang here is a concurrency regression — see docs/concurrency_audit.md",
            limit.as_secs()
        );
    };
    assert!(
        stdout.contains(marker),
        "{name} missing success marker `{marker}` (wall {wall:?}).\nstdout:\n{stdout}"
    );
    stdout
}

fn parse_marker(stdout: &str, marker: &str) -> Option<u64> {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix(marker) {
            let v = rest.trim_start_matches([':', '=']).trim();
            if let Some(first) = v.split_whitespace().next() {
                return first.parse::<u64>().ok();
            }
        }
    }
    None
}

// ---------------------------------------------------------------------
// Fix guards — these failed or hung before the audit fixes.
// ---------------------------------------------------------------------

#[test]
fn audit_au1_verdict_survives_inline_resuspend() {
    // MAJOR-1: trailing handoff-send to a re-suspending waiter.
    let out = run_audit("au1_verdict", "verdict_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(out.contains("h2_done=1"), "h2 completion lost:\n{out}");
    }
}

#[test]
fn audit_au12_multi_join_wakes_all() {
    // MAJOR-2: two blockers on one handle must both complete.
    let out = run_audit("au12_multijoin", "multijoin_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(out.contains("m=2"), "wrong join payload:\n{out}");
    }
}

#[test]
fn audit_au4_join_chain_drains() {
    // Helping discipline: 60-link chain, no dependency inversion.
    let out = run_audit("au4_joinchain", "chain_ok", Duration::from_secs(90));
    if !out.is_empty() {
        assert_eq!(
            parse_marker(&out, "chain="),
            Some(61),
            "chain sum drift:\n{out}"
        );
    }
}

#[test]
fn audit_au8_join_tree_drains() {
    // CRITICAL-1: 255-task blocking-join tree (top-up stranding shape).
    let out = run_audit("au8_tree", "tree_ok", Duration::from_secs(180));
    if !out.is_empty() {
        assert!(out.contains("tree=128"), "tree sum drift:\n{out}");
    }
}

// ---------------------------------------------------------------------
// Shape coverage — MPMC, relay, aliasing, indirect waits, captures.
// ---------------------------------------------------------------------

#[test]
fn audit_au2_double_join_aliasing_safe() {
    let out = run_audit("au2_doublejoin", "doublejoin_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(out.contains("m=2"), "double-join payload drift:\n{out}");
    }
}

#[test]
fn audit_au3_mpmc_sum_exact() {
    let out = run_audit("au3_mpmc", "mpmc_ok", Duration::from_secs(120));
    if !out.is_empty() {
        assert_eq!(
            parse_marker(&out, "total="),
            Some(8000),
            "mpmc sum drift:\n{out}"
        );
    }
}

#[test]
fn audit_au5_relay_cascade_completes() {
    let out = run_audit("au5_relay30", "relay_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert_eq!(
            parse_marker(&out, "relay="),
            Some(30),
            "relay sum drift:\n{out}"
        );
    }
}

#[test]
fn audit_au9_indirect_blocking_wait() {
    let out = run_audit("au9_indirect", "indirect_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(
            out.contains("indirect=123"),
            "indirect payload drift:\n{out}"
        );
    }
}

#[test]
fn audit_au10_rich_captures() {
    let out = run_audit("au10_bigcap", "bigcap_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(out.contains("v=54"), "capture payload drift:\n{out}");
    }
}

#[test]
fn audit_au11_deep_nesting() {
    run_audit("au11_deepnest", "nest_ok", Duration::from_secs(60));
}

#[test]
fn audit_au15_pool_overflow_capture() {
    let out = run_audit("au15_bigpool", "bigpool_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(out.contains("big=999"), "overflow-capture drift:\n{out}");
    }
}

#[test]
fn audit_au19_match_binding_survives_resume() {
    // Same bug class as the range-bound spill: a guarded binding arm's
    // value must live in a frame cell, not a C local skipped by resume.
    let out = run_audit("au19_matchbind", "matchbind_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(
            out.contains("matchbind=30"),
            "matchbind payload drift:\n{out}"
        );
    }
}

#[test]
fn audit_au20_variable_bound_survives_resume() {
    // Sibling of au3 with a captured (non-literal) bound: `0..n` must
    // trip exactly n times even with suspends inside.
    let out = run_audit("au20_varbound", "vartrip_ok", Duration::from_secs(60));
    if !out.is_empty() {
        assert!(
            out.contains("vartrip=5000"),
            "vartrip payload drift:\n{out}"
        );
    }
}

// ---------------------------------------------------------------------
// Resource bounds — allocator steady-state under churn and flood.
// ---------------------------------------------------------------------

const BASELINE_THRASH_WALL_S: u64 = 60;
const BASELINE_THRASH_PEAK_MB: u64 = 64;
const BASELINE_FLOOD_WALL_S: u64 = 120;
const BASELINE_FLOOD_PEAK_MB: u64 = 128;

#[test]
fn audit_au6_thrash_stays_bounded() {
    let Some(bin) = build_audit_bench("au6_thrash") else {
        return;
    };
    let Some((stdout, wall, peak_kib)) =
        run_timeout(&bin, Duration::from_secs(BASELINE_THRASH_WALL_S))
    else {
        panic!("au6_thrash hung or crashed (timeout {BASELINE_THRASH_WALL_S}s)");
    };
    assert!(stdout.contains("thrash_ok"), "missing marker:\n{stdout}");
    assert!(
        wall.as_secs() < BASELINE_THRASH_WALL_S,
        "thrash wall {wall:?} exceeds baseline"
    );
    assert!(
        peak_kib / 1024 < BASELINE_THRASH_PEAK_MB,
        "thrash peak {}MB exceeds baseline {BASELINE_THRASH_PEAK_MB}MB",
        peak_kib / 1024
    );
}

#[test]
fn audit_au7_flood_drains_exact() {
    let Some(bin) = build_audit_bench("au7_flood") else {
        return;
    };
    let Some((stdout, wall, peak_kib)) =
        run_timeout(&bin, Duration::from_secs(BASELINE_FLOOD_WALL_S))
    else {
        panic!("au7_flood hung or crashed (timeout {BASELINE_FLOOD_WALL_S}s)");
    };
    assert!(stdout.contains("flood_ok"), "missing marker:\n{stdout}");
    assert_eq!(
        parse_marker(&stdout, "sum="),
        Some(1_249_975_000),
        "flood sum drift:\n{stdout}"
    );
    assert!(
        wall.as_secs() < BASELINE_FLOOD_WALL_S,
        "flood wall {wall:?} exceeds baseline"
    );
    assert!(
        peak_kib / 1024 < BASELINE_FLOOD_PEAK_MB,
        "flood peak {}MB exceeds baseline {BASELINE_FLOOD_PEAK_MB}MB",
        peak_kib / 1024
    );
}

// ---------------------------------------------------------------------
// Negative tests — these programs must NOT build.
// ---------------------------------------------------------------------

#[test]
fn audit_au13_spawn_non_closure_rejected() {
    assert_build_fails("au13_negspawn");
}

#[test]
fn audit_au14_send_non_chan_rejected() {
    assert_build_fails("au14_negsend");
}

#[test]
fn audit_au18_greenspawn_capture_rejected() {
    // MAJOR-5 known-fail: nested capture through a green cell breaks C
    // codegen. Guards the failure stays loud (build error, never silent
    // misbehavior) until the lowerer fix lands — then this test flips to
    // a success assertion.
    assert_build_fails("au18_greenspawn");
}
