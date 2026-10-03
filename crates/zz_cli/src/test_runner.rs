//! `zz test` — discover and run `@test`-annotated functions.
//!
//! Engines:
//! - VM (default): each test runs in a fresh `Interp` (in-process).
//! - AOT (`--native`): each test file is compiled once (dev profile) into
//!   a per-test dispatch binary; every test runs in its own process
//!   (exit-code isolation — a failing `assert` aborts only its process).
//!
//! Design decisions (see docs/test-isolation.md):
//! - VM: panic isolation via `std::panic::catch_unwind`.
//! - Timeout: `@test(timeout = ms)` is a hard timeout (VM: watcher thread,
//!   AOT: child kill). `--timeout <ms>` (default 60s) is a soft budget —
//!   overruns print a notice but never interrupt or fail the test.
//! - `--nocapture` forces `--serial` (interleaved output is noise).
//! - `--jobs N` uses a thread pool with work-stealing (default: logical cores).
//! - Files run sequentially so output stays grouped per file
//!   (`Running <file>` blocks); tests *within* a file run serial/parallel
//!   per `--serial`/`--jobs`.
//! - No-arg discovery prefers `./tests/` when it holds `.zz` files
//!   (zero-config; no `zz.toml` needed), else the current directory.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zz_frontend::ast::Stmt;
use zz_frontend::diag::{render_to_string, Files};
use zz_frontend::span::Span;
use zz_frontend::test_attr::{self, TestMeta};
use zz_runtime::{Interp, Value};

use crate::loader;

/// Soft per-test time budget: overruns print a notice but never
/// interrupt or fail the test. Configurable via `--timeout <ms>`.
const DEFAULT_SOFT_BUDGET: Duration = Duration::from_secs(60);

/// Harness files generated for AOT mode. Discovery skips them so a
/// leftover harness is never picked up as a test file.
const AOT_HARNESS_PREFIX: &str = "zz_test_harness_";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestEngine {
    Vm,
    Native,
}

struct TestConfig {
    path: String,
    filter: Option<String>,
    exact: bool,
    tag: Option<String>,
    skip: Option<String>,
    fail_fast: bool,
    fail_on_empty: bool,
    slow_threshold: Option<Duration>,
    soft_budget: Duration,
    nocapture: bool,
    serial: bool,
    jobs: usize,
    seed: u64,
    list_only: bool,
    json_output: bool,
    junit_path: Option<String>,
    repeat: usize,
    changed: bool,
    engine: TestEngine,
    native_release: bool,
}

impl TestConfig {
    fn parse(args: &[String]) -> Result<Self, String> {
        // Load zz.toml defaults first (optional; absence is fine).
        let defaults = load_zz_toml_defaults();

        let mut path: Option<String> = None;
        let mut filter: Option<String> = None;
        let mut exact = false;
        let mut tag: Option<String> = None;
        let mut skip: Option<String> = None;
        let mut fail_fast = defaults.fail_fast;
        let mut fail_on_empty = false;
        let mut slow_threshold = defaults.slow_threshold;
        let mut soft_budget = defaults.soft_budget;
        let mut nocapture = false;
        let mut serial = defaults.serial;
        let mut jobs: Option<usize> = defaults.jobs;
        let mut seed: Option<u64> = defaults.seed;
        let mut list_only = false;
        let mut json_output = false;
        let mut junit_path: Option<String> = None;
        let mut repeat: usize = defaults.repeat;
        let mut changed = false;
        let mut engine = defaults.engine;
        let mut native_release = false;

        let mut i = 0;
        while i < args.len() {
            // `--flag=value` forms for value flags.
            if args[i].starts_with("--filter=")
                || args[i].starts_with("--tag=")
                || args[i].starts_with("--skip=")
                || args[i].starts_with("--jobs=")
                || args[i].starts_with("--seed=")
                || args[i].starts_with("--slow-threshold=")
                || args[i].starts_with("--junit=")
                || args[i].starts_with("--repeat=")
                || args[i].starts_with("--timeout=")
                || args[i].starts_with("--engine=")
            {
                let (flag, val) = args[i].split_once('=').unwrap();
                let val = val.to_string();
                match flag {
                    "--filter" => filter = Some(val),
                    "--tag" => tag = Some(val),
                    "--skip" => skip = Some(val),
                    "--jobs" => jobs = val.parse::<usize>().ok(),
                    "--seed" => seed = val.parse::<u64>().ok(),
                    "--slow-threshold" => {
                        if let Ok(ms) = val.parse::<u64>() {
                            slow_threshold = Some(Duration::from_millis(ms));
                        }
                    }
                    "--junit" => junit_path = Some(val),
                    "--repeat" => {
                        if let Ok(n) = val.parse::<usize>() {
                            repeat = n.max(1);
                        }
                    }
                    "--timeout" => {
                        if let Ok(ms) = val.parse::<u64>() {
                            soft_budget = Duration::from_millis(ms);
                        }
                    }
                    "--engine" => {
                        engine = match val.as_str() {
                            "vm" => TestEngine::Vm,
                            "native" | "aot" => TestEngine::Native,
                            _ => {
                                return Err(format!(
                                    "unknown --engine `{val}`\n\nhint: use --engine vm|native"
                                ));
                            }
                        };
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            match args[i].as_str() {
                "--nocapture" | "--no-capture" => nocapture = true,
                "--serial" => serial = true,
                "--exact" => exact = true,
                "--fail-fast" => fail_fast = true,
                "--fail-on-empty" => fail_on_empty = true,
                "--list" => list_only = true,
                "--json" => json_output = true,
                "--native" | "--aot" => engine = TestEngine::Native,
                "-p" | "--release" => native_release = true,
                "--filter" | "-f" => {
                    i += 1;
                    filter = args.get(i).cloned();
                }
                "--tag" | "-t" => {
                    i += 1;
                    tag = args.get(i).cloned();
                }
                "--skip" | "-s" => {
                    i += 1;
                    skip = args.get(i).cloned();
                }
                "--jobs" | "-j" => {
                    i += 1;
                    jobs = args.get(i).and_then(|s| s.parse::<usize>().ok());
                }
                "--seed" => {
                    i += 1;
                    seed = args.get(i).and_then(|s| s.parse::<u64>().ok());
                }
                "--slow-threshold" => {
                    i += 1;
                    if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                        slow_threshold = Some(Duration::from_millis(ms));
                    }
                }
                "--timeout" => {
                    i += 1;
                    if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                        soft_budget = Duration::from_millis(ms);
                    }
                }
                "--engine" => {
                    i += 1;
                    match args.get(i).map(String::as_str) {
                        Some("vm") => engine = TestEngine::Vm,
                        Some("native") | Some("aot") => engine = TestEngine::Native,
                        Some(other) => {
                            return Err(format!(
                                "unknown --engine `{other}`\n\nhint: use --engine vm|native"
                            ));
                        }
                        None => {
                            return Err(
                                "missing value for --engine\n\nhint: use --engine vm|native"
                                    .to_string(),
                            );
                        }
                    }
                }
                "--junit" => {
                    i += 1;
                    junit_path = args.get(i).cloned();
                }
                "--repeat" => {
                    i += 1;
                    if let Some(n) = args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                        repeat = n.max(1);
                    }
                }
                "--changed" => changed = true,
                "--nocapture-impl" => {}
                "--help" | "-h" => {
                    return Err("usage: zz test [file.zz | directory] [FLAGS]\n\n\
                         FLAGS:\n  \
                           --exact          Exact match filter (not substring)\n  \
                           --tag <t>        Only tests with tag = t\n  \
                           --skip <s>       Exclude tests matching substring s\n  \
                           --filter, -f     Substring filter on test name\n  \
                           --jobs, -j N     Thread pool size (default: logical cores)\n  \
                           --serial         Run tests sequentially\n  \
                           --seed <n>       Shuffle seed for reproducible order\n  \
                           --fail-fast      Stop after first failure\n  \
                           --fail-on-empty  Exit 1 when no tests matched (default: exit 0)\n  \
                           --list           List discovered tests without running\n  \
                           --nocapture      Show stdout/stderr live (forces serial)\n  \
                           --slow-threshold Mark passes slower than N ms\n  \
                           --timeout <ms>   Soft time budget per test (default 60000);\n  \
                           overruns print a notice, never interrupt or fail\n  \
                           --native, --aot  Run tests as AOT binaries (dev build)\n  \
                           --engine=vm|native  Select the test engine (default vm)\n  \
                           -p, --release    With --native: optimized build (default: dev)\n  \
                           --json           Structured JSON output to stdout\n  \
                           --junit <path>   JUnit XML to file\n  \
                           --repeat N       Run tests N times\n  \
                           --changed        Only run tests for files changed vs git HEAD\n  \
                           -h, --help       Show this help\n\n\
                         ENGINES:\n  \
                           VM (default): in-process interpreter, fastest.\n  \
                           AOT (--native): each file compiled once (dev), each test\n  \
                           runs in its own process (exit-code isolation).\n\n\
                         Without a path, uses ./tests/ when it holds .zz files,\n  \
                         else the current directory. No zz.toml required."
                        .to_string());
                }
                other if other.starts_with('-') => {
                    return Err(format!(
                        "unknown flag: {other}\n\nhint: use `zz test --help` for usage"
                    ));
                }
                _ => {
                    path = Some(args[i].clone());
                }
            }
            i += 1;
        }

        let path = path.unwrap_or_else(default_test_path);

        // --nocapture forces --serial
        if nocapture {
            serial = true;
        }

        let jobs = jobs
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            })
            .max(1);
        let seed = seed.unwrap_or_else(|| {
            use std::collections::hash_map::RandomState;
            use std::hash::{BuildHasher, Hasher};
            let s = RandomState::new();
            let mut h = s.build_hasher();
            h.write_u64(0xDEAD_BEEF); // seed the hasher
            h.finish()
        });

        Ok(Self {
            path,
            filter,
            exact,
            tag,
            skip,
            fail_fast,
            fail_on_empty,
            slow_threshold,
            soft_budget,
            nocapture,
            serial,
            jobs,
            seed,
            list_only,
            json_output,
            junit_path,
            repeat,
            changed,
            engine,
            native_release,
        })
    }

    fn use_color(&self) -> bool {
        if self.json_output {
            return false;
        }
        std::io::stderr().is_terminal()
    }

    /// Whether to run tests in parallel.
    fn parallel(&self) -> bool {
        !self.serial && self.jobs > 1
    }
}

// ---------------------------------------------------------------------------
// zz.toml config
// ---------------------------------------------------------------------------

struct TestDefaults {
    jobs: Option<usize>,
    serial: bool,
    fail_fast: bool,
    slow_threshold: Option<Duration>,
    soft_budget: Duration,
    seed: Option<u64>,
    repeat: usize,
    engine: TestEngine,
}

impl Default for TestDefaults {
    fn default() -> Self {
        Self {
            jobs: None,
            serial: false,
            fail_fast: false,
            slow_threshold: None,
            soft_budget: DEFAULT_SOFT_BUDGET,
            seed: None,
            repeat: 1,
            engine: TestEngine::Vm,
        }
    }
}

/// Load `[test]` section from `zz.toml` in the current directory.
/// Missing file / section is fine (zero-config); only overrides apply.
fn load_zz_toml_defaults() -> TestDefaults {
    let Ok(content) = std::fs::read_to_string("zz.toml") else {
        return TestDefaults::default();
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return TestDefaults::default();
    };
    let Some(test_section) = table.get("test").and_then(|v| v.as_table()) else {
        return TestDefaults::default();
    };

    let mut d = TestDefaults::default();

    if let Some(v) = test_section.get("jobs").and_then(|v| v.as_integer()) {
        d.jobs = Some(v.max(1) as usize);
    }
    if let Some(v) = test_section.get("serial").and_then(|v| v.as_bool()) {
        d.serial = v;
    }
    if let Some(v) = test_section.get("fail-fast").and_then(|v| v.as_bool()) {
        d.fail_fast = v;
    }
    if let Some(v) = test_section
        .get("slow-threshold")
        .and_then(|v| v.as_integer())
    {
        d.slow_threshold = Some(Duration::from_millis(v as u64));
    }
    if let Some(v) = test_section.get("seed").and_then(|v| v.as_integer()) {
        d.seed = Some(v as u64);
    }
    if let Some(v) = test_section.get("repeat").and_then(|v| v.as_integer()) {
        d.repeat = (v.max(1)) as usize;
    }
    if let Some(v) = test_section.get("timeout").and_then(|v| v.as_integer()) {
        d.soft_budget = Duration::from_millis(v.max(0) as u64);
    }
    if let Some(v) = test_section.get("engine").and_then(|v| v.as_str()) {
        d.engine = match v {
            "native" | "aot" => TestEngine::Native,
            _ => TestEngine::Vm,
        };
    }

    d
}

/// No-arg test root: `./tests/` when it holds `.zz` files, else `.`.
/// Zero-config — no `zz.toml` needed.
fn default_test_path() -> String {
    let tests = Path::new("tests");
    if tests.is_dir() && dir_has_zz(tests) {
        return "tests".to_string();
    }
    ".".to_string()
}

fn dir_has_zz(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "vendor" || name == "build" {
                continue;
            }
            if dir_has_zz(&p) {
                return true;
            }
        } else if p.extension().and_then(|e| e.to_str()) == Some("zz") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TestResult {
    name: String,
    file: PathBuf,
    passed: bool,
    ignored: bool,
    slow: bool,
    /// Duration exceeded the soft budget (notice only, never fails).
    over_budget: bool,
    reason: Option<String>,
    duration: Duration,
    error_msg: Option<String>,
    retried: u32,
}

#[derive(Debug, Clone)]
struct TestInfo {
    name: String,
    file: PathBuf,
    meta: TestMeta,
    module_index: usize,
    func_name: Vec<String>,
    /// Qualified name of the `@setup` function for this file, if any.
    setup_fn: Option<String>,
    /// Qualified name of the `@teardown` function for this file, if any.
    teardown_fn: Option<String>,
    /// If this is a parameterized case, the runtime values for this invocation.
    case_values: Option<Vec<Value>>,
}

// ---------------------------------------------------------------------------
// Deterministic shuffle (XorShift64)
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1)) // avoid zero state
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.next_u64() as usize % (i + 1);
            v.swap(i, j);
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level entry
// ---------------------------------------------------------------------------

pub fn test_command(args: &[String]) -> Result<(), String> {
    let config = TestConfig::parse(args)?;

    // Fail fast on an unsatisfied `[package] zz` compiler requirement.
    crate::enforce_project_zz(std::path::Path::new(&config.path))?;

    let files = if config.changed {
        // When --changed, only load files that git says are modified.
        let changed = git_changed_files().unwrap_or_default();
        let changed_set: std::collections::HashSet<PathBuf> = changed.into_iter().collect();
        let all = collect_zz_files(&config.path)?;
        all.into_iter()
            .filter(|p| {
                changed_set.contains(p)
                    || std::fs::canonicalize(p)
                        .ok()
                        .map(|c| changed_set.contains(&c))
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>()
    } else {
        collect_zz_files(&config.path)?
    };

    if files.is_empty() {
        let msg = if config.changed {
            "no changed .zz files found (git diff HEAD)".to_string()
        } else {
            format!(
                "no .zz files found in `{}`\n\n\
                 hint: ensure the path contains .zz source files",
                config.path
            )
        };
        if config.json_output {
            println!("[]");
        } else {
            eprintln!("zz test: {msg}");
        }
        return Ok(());
    }

    // Discover per file (keeps file grouping for output).
    let mut groups: Vec<(PathBuf, Vec<TestInfo>)> = Vec::new();
    let mut load_errors = false;
    for file in &files {
        match discover_tests(file) {
            Ok(tests) => {
                if !tests.is_empty() {
                    groups.push((file.clone(), tests));
                }
            }
            Err(e) => {
                eprintln!("zz test: {e}");
                load_errors = true;
            }
        }
    }

    if load_errors {
        return Err("failed to load some test files".to_string());
    }

    // Filter within each group; drop empty groups.
    for (_, tests) in groups.iter_mut() {
        let mut v = std::mem::take(tests);
        apply_filters(&mut v, &config);
        *tests = v;
    }
    groups.retain(|(_, t)| !t.is_empty());

    if groups.is_empty() {
        let msg = match (&config.filter, &config.tag, &config.skip) {
            (f, t, s) if f.is_some() || t.is_some() || s.is_some() => {
                let mut parts = Vec::new();
                if let Some(f) = &config.filter {
                    parts.push(format!("filter: {f:?}"));
                }
                if let Some(t) = &config.tag {
                    parts.push(format!("tag: {t:?}"));
                }
                if let Some(s) = &config.skip {
                    parts.push(format!("skip: {s:?}"));
                }
                format!("0 tests matched ({})", parts.join(", "))
            }
            _ => "no tests found".to_string(),
        };
        if config.json_output {
            println!("[]");
        } else {
            eprintln!("zz test: {msg}");
        }
        if config.fail_on_empty {
            return Err(msg);
        }
        return Ok(());
    }

    if config.list_only {
        print_test_list_grouped(&groups, config.use_color());
        return Ok(());
    }

    // Deterministic shuffle *within* each file (seeded; files stay sorted
    // so output blocks are stable).
    if !config.serial {
        let mut rng = Rng::new(config.seed);
        for (_, tests) in groups.iter_mut() {
            rng.shuffle(tests);
        }
    }

    // AOT: compile one dispatch binary per file up front (dev by default,
    // release with `-p/--release`). VM needs no setup.
    let mut aot_bins: BTreeMap<PathBuf, (PathBuf, PathBuf)> = BTreeMap::new();
    if config.engine == TestEngine::Native {
        for (file, tests) in &groups {
            match build_aot_harness(file, tests, config.native_release) {
                Ok(b) => {
                    aot_bins.insert(file.clone(), b);
                }
                Err(e) => {
                    cleanup_aot(&mut aot_bins);
                    return Err(e);
                }
            }
        }
    }

    let repeat = config.repeat;
    let mut all_results: Vec<TestResult> = Vec::new();
    let overall_start = Instant::now();
    let use_color = config.use_color();
    let total_files = groups.len();

    for iteration in 0..repeat {
        if repeat > 1 && !config.json_output {
            eprintln!("\n--- iteration {}/{} ---", iteration + 1, repeat);
        }
        let iter_start = Instant::now();
        let mut iter_results: Vec<TestResult> = Vec::new();
        let mut failed_seen = false;
        let engine_label = match config.engine {
            TestEngine::Vm => "vm",
            TestEngine::Native => {
                if config.native_release {
                    "aot (release)"
                } else {
                    "aot (dev)"
                }
            }
        };
        if !config.json_output {
            if config.serial {
                eprintln!("engine: {engine_label}");
            } else {
                eprintln!("engine: {engine_label} (seed={})", config.seed);
            }
        }

        for (gi, (file, tests)) in groups.iter().enumerate() {
            if config.fail_fast && failed_seen {
                // Remaining files skipped without running (streamed too).
                for t in tests {
                    let s = skipped_result(t, "skipped (--fail-fast)");
                    if !config.json_output {
                        print_test_line(&s, &config);
                    }
                    iter_results.push(s);
                }
                continue;
            }
            if !config.json_output {
                if gi > 0 {
                    eprintln!();
                }
                eprintln!("Running {}", file.display());
                eprintln!("running {} tests", tests.len());
            }
            let file_start = Instant::now();
            let bin = aot_bins.get(file).map(|(_, b)| b.clone());
            // Streams each result test-by-test; the file footer follows.
            let results = match run_group(tests, &config, bin.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    cleanup_aot(&mut aot_bins);
                    return Err(e);
                }
            };
            let elapsed = file_start.elapsed();
            if !config.json_output {
                print_file_footer(&results, elapsed, use_color, &config);
            }
            if results.iter().any(|r| !r.passed && !r.ignored) {
                failed_seen = true;
            }
            if config.fail_fast && failed_seen && gi + 1 < groups.len() {
                eprintln!("stopping after first failure (--fail-fast)");
            }
            iter_results.extend(results);
        }

        if !config.json_output && total_files > 1 {
            print_global_footer(&iter_results, iter_start.elapsed(), total_files, use_color);
        }

        all_results.extend(iter_results);

        // --fail-fast across iterations
        if config.fail_fast && all_results.iter().any(|r| !r.passed && !r.ignored) {
            break;
        }
    }

    cleanup_aot(&mut aot_bins);

    // --junit (aggregate across all iterations)
    if let Some(ref path) = config.junit_path {
        write_junit(path, &all_results)?;
    }

    // --json: emit single valid JSON array (all iterations combined)
    if config.json_output {
        print_json_results(&all_results)?;
    }

    // Aggregate summary for repeat mode
    if repeat > 1 && !config.json_output {
        let (total_passed, total_failed, total_ignored, total_skipped) = {
            let mut p = 0;
            let mut f = 0;
            let mut ig = 0;
            let mut sk = 0;
            for r in &all_results {
                if r.ignored {
                    ig += 1;
                } else if r.passed {
                    p += 1;
                } else if is_failfast_skip(r) {
                    sk += 1;
                } else {
                    f += 1;
                }
            }
            (p, f, ig, sk)
        };
        let total = all_results.len();
        let skipped_info = if total_skipped > 0 {
            format!("; {total_skipped} skipped")
        } else {
            String::new()
        };
        eprintln!(
            "\naggregate: {total_failed} failed, {total_passed} passed, {total_ignored} ignored{skipped_info}, {total} total in {}",
            fmt_dur(overall_start.elapsed())
        );
    }

    let failed = all_results
        .iter()
        .filter(|r| !r.passed && !r.ignored && !is_failfast_skip(r))
        .count();
    if failed > 0 {
        Err("tests failed".to_string())
    } else {
        Ok(())
    }
}

fn skipped_result(test: &TestInfo, msg: &str) -> TestResult {
    TestResult {
        name: test.name.clone(),
        file: test.file.clone(),
        passed: false,
        ignored: false,
        slow: false,
        over_budget: false,
        reason: None,
        duration: Duration::ZERO,
        error_msg: Some(msg.to_string()),
        retried: 0,
    }
}

// ---------------------------------------------------------------------------
// Group runner (one file): streams each result test-by-test as it finishes
// ---------------------------------------------------------------------------

/// Run one file's tests with the selected engine (`aot_bin = None` → VM).
/// Every result prints live, test-by-test: serially in order, parallel in
/// completion order. Files still run sequentially so blocks stay grouped.
fn run_group(
    tests: &[TestInfo],
    config: &TestConfig,
    aot_bin: Option<&Path>,
) -> Result<Vec<TestResult>, String> {
    if config.parallel() {
        run_group_parallel(tests, config, aot_bin)
    } else {
        run_group_serial(tests, config, aot_bin)
    }
}

fn run_group_serial(
    tests: &[TestInfo],
    config: &TestConfig,
    aot_bin: Option<&Path>,
) -> Result<Vec<TestResult>, String> {
    let mut results = Vec::with_capacity(tests.len());
    for (idx, test) in tests.iter().enumerate() {
        let r = match aot_bin {
            Some(bin) => run_single_test_aot(test, idx, bin, config),
            None => run_single_test_vm(test, config),
        };
        let stop = config.fail_fast && !r.passed && !r.ignored;
        if !config.json_output {
            print_test_line(&r, config);
        }
        results.push(r);
        if stop {
            // Mark the rest skipped so totals stay accurate.
            for t in &tests[results.len()..] {
                let s = skipped_result(t, "skipped (--fail-fast)");
                if !config.json_output {
                    print_test_line(&s, config);
                }
                results.push(s);
            }
            break;
        }
    }
    Ok(results)
}

fn run_group_parallel(
    tests: &[TestInfo],
    config: &TestConfig,
    aot_bin: Option<&Path>,
) -> Result<Vec<TestResult>, String> {
    let total = tests.len();
    let jobs = config.jobs.min(total).max(1);

    let queue: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new((0..total).collect()));
    let results: Arc<Mutex<Vec<Option<TestResult>>>> = Arc::new(Mutex::new(vec![None; total]));
    let fail_fast_flag = Arc::new(AtomicBool::new(false));
    // Serializes multi-line result blocks so parallel completions never
    // interleave mid-test.
    let print_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    let aot_bin = aot_bin.map(|p| p.to_path_buf());

    std::thread::scope(|s| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                let queue = Arc::clone(&queue);
                let results = Arc::clone(&results);
                let fail_fast_flag = Arc::clone(&fail_fast_flag);
                let print_lock = Arc::clone(&print_lock);
                let config_ref = config;
                let bin_clone = aot_bin.clone();

                s.spawn(move || loop {
                    let idx = {
                        let mut q = queue.lock().unwrap();
                        q.pop_front()
                    };
                    let idx = match idx {
                        Some(i) => i,
                        None => break,
                    };
                    if fail_fast_flag.load(Ordering::Relaxed) {
                        queue.lock().unwrap().push_front(idx);
                        break;
                    }
                    let test = &tests[idx];
                    let r = match bin_clone.as_deref() {
                        Some(bin) => run_single_test_aot(test, idx, bin, config_ref),
                        None => run_single_test_vm(test, config_ref),
                    };
                    if config_ref.fail_fast && !r.passed && !r.ignored {
                        fail_fast_flag.store(true, Ordering::Relaxed);
                    }
                    // Stream test-by-test in completion order.
                    if !config_ref.json_output {
                        let _guard = print_lock.lock().unwrap();
                        print_test_line(&r, config_ref);
                    }
                    results.lock().unwrap()[idx] = Some(r);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    });

    let results = Arc::try_unwrap(results)
        .map_err(|_| "results Arc still referenced".to_string())?
        .into_inner()
        .unwrap();

    Ok(results
        .into_iter()
        .enumerate()
        .map(|(i, r)| match r {
            Some(r) => r,
            // Never ran (fail-fast): stream its marker in file order.
            None => {
                let s = skipped_result(&tests[i], "skipped (--fail-fast)");
                if !config.json_output {
                    let _guard = print_lock.lock().unwrap();
                    print_test_line(&s, config);
                }
                s
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Filter pipeline
// ---------------------------------------------------------------------------

fn apply_filters(tests: &mut Vec<TestInfo>, config: &TestConfig) {
    if let Some(ref pat) = config.filter {
        if config.exact {
            tests.retain(|t| t.name == *pat || display_name(&t.name, &t.file) == *pat);
        } else {
            let pat_lower = pat.to_lowercase();
            tests.retain(|t| t.name.to_lowercase().contains(&pat_lower));
        }
    }

    if let Some(ref tag) = config.tag {
        tests.retain(|t| t.meta.tag.as_deref() == Some(tag.as_str()));
    }

    if let Some(ref skip_pat) = config.skip {
        let skip_lower = skip_pat.to_lowercase();
        tests.retain(|t| !t.name.to_lowercase().contains(&skip_lower));
    }
}

/// Get list of .zz files changed vs HEAD using `git diff`.
fn git_changed_files() -> Result<Vec<PathBuf>, String> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if !output.status.success() {
        // Try staged changes as fallback.
        let output2 = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .output()
            .map_err(|e| format!("failed to run git: {e}"))?;

        if !output2.status.success() {
            return Err("git diff failed (not a git repo?)".to_string());
        }

        return parse_git_output(&output2.stdout);
    }

    parse_git_output(&output.stdout)
}

fn parse_git_output(stdout: &[u8]) -> Result<Vec<PathBuf>, String> {
    let text = String::from_utf8_lossy(stdout);
    let files: Vec<PathBuf> = text
        .lines()
        .filter(|l| l.ends_with(".zz"))
        .map(PathBuf::from)
        .collect();
    Ok(files)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn discover_tests(path: &Path) -> Result<Vec<TestInfo>, String> {
    let loaded = loader::load_program(path)?;

    let mut errors = false;
    for e in &loaded.errors {
        let mut files = Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!("{}", render_to_string(&files, id, &e.diags));
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            errors = true;
        }
    }
    if errors {
        return Err(format!("failed to load `{}`", path.display()));
    }

    // First pass: find @setup / @teardown functions (file-scoped, last wins).
    let mut setup_fn: Option<String> = None;
    let mut teardown_fn: Option<String> = None;

    for program in &loaded.programs {
        for stmt in &program.stmts {
            match stmt {
                Stmt::Func {
                    name, decorators, ..
                } => {
                    for dec in decorators {
                        if test_attr::is_setup_decorator(dec) {
                            let qname = name.join(".");
                            setup_fn = Some(qname);
                        } else if test_attr::is_teardown_decorator(dec) {
                            let qname = name.join(".");
                            teardown_fn = Some(qname);
                        }
                    }
                }
                Stmt::Impl { methods, .. } => {
                    for method in methods {
                        if let Stmt::Func {
                            name: method_name,
                            decorators,
                            ..
                        } = method
                        {
                            for dec in decorators {
                                if test_attr::is_setup_decorator(dec) {
                                    let qname = method_name.join(".");
                                    setup_fn = Some(qname);
                                } else if test_attr::is_teardown_decorator(dec) {
                                    let qname = method_name.join(".");
                                    teardown_fn = Some(qname);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Second pass: discover @test functions, attaching setup/teardown.
    let mut tests = Vec::new();

    for (idx, program) in loaded.programs.iter().enumerate() {
        for stmt in &program.stmts {
            for mut test in discover_in_stmt(stmt, path, idx) {
                test.setup_fn = setup_fn.clone();
                test.teardown_fn = teardown_fn.clone();

                // Expand parameterized cases into individual test entries.
                if let Some(ref case_exprs) = test.meta.cases.clone() {
                    for (i, case_expr) in case_exprs.iter().enumerate() {
                        let case_vals = match eval_case_literal(case_expr) {
                            Some(v) => v,
                            None => {
                                // Non-literal case: skip with diagnostic-like name.
                                let mut expanded = test.clone();
                                expanded.name =
                                    format!("{}[case {}] <non-literal, skipped>", test.name, i);
                                expanded.case_values = Some(vec![]);
                                tests.push(expanded);
                                continue;
                            }
                        };
                        let mut expanded = test.clone();
                        expanded.name = format!("{}[{}]", test.name, i);
                        expanded.case_values = Some(case_vals);
                        tests.push(expanded);
                    }
                } else {
                    tests.push(test);
                }
            }
        }
    }

    Ok(tests)
}

/// Evaluate a literal `Expr` into a runtime `Value`. Returns `None` for
/// non-literal expressions (variables, calls, etc.).
fn eval_case_literal(expr: &zz_frontend::ast::Expr) -> Option<Vec<Value>> {
    use zz_frontend::ast::Expr;
    match expr {
        Expr::Array { elems, .. } => {
            let mut vals = Vec::with_capacity(elems.len());
            for e in elems {
                vals.push(eval_single_literal(e)?);
            }
            Some(vals)
        }
        _ => None,
    }
}

fn eval_single_literal(expr: &zz_frontend::ast::Expr) -> Option<Value> {
    use zz_frontend::ast::Expr;
    match expr {
        Expr::Int { value, .. } => Some(Value::Int(*value)),
        Expr::Float { value, .. } => Some(Value::Float(*value)),
        Expr::Str { value, .. } => Some(Value::Str(value.clone().into())),
        Expr::Bool { value, .. } => Some(Value::Bool(*value)),
        Expr::Paren { expr, .. } => eval_single_literal(expr),
        // Handle unary minus: -(literal)
        Expr::Unary {
            op, expr: inner, ..
        } => {
            if *op == zz_frontend::ast::UnOp::Neg {
                match eval_single_literal(inner)? {
                    Value::Int(v) => Some(Value::Int(-v)),
                    Value::Float(v) => Some(Value::Float(-v)),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn discover_in_stmt(stmt: &Stmt, file: &Path, module_index: usize) -> Vec<TestInfo> {
    match stmt {
        Stmt::Func {
            name, decorators, ..
        } => {
            if decorators.is_empty() {
                return Vec::new();
            }
            let test_dec = match decorators.iter().find(|d| test_attr::is_test_decorator(d)) {
                Some(d) => d,
                None => return Vec::new(),
            };
            let meta = test_attr::parse_test_meta(test_dec, &mut Vec::new())
                .unwrap_or_else(|| TestMeta::empty(test_dec.span));

            let display_name = if name.len() > 1 {
                name.join(".")
            } else {
                name[0].clone()
            };

            vec![TestInfo {
                name: display_name,
                file: file.to_path_buf(),
                meta,
                module_index,
                func_name: name.clone(),
                setup_fn: None,
                teardown_fn: None,
                case_values: None,
            }]
        }
        Stmt::Impl {
            name: impl_name,
            methods,
            ..
        } => {
            let mut tests = Vec::new();
            for method in methods {
                if let Stmt::Func {
                    name: method_name,
                    decorators,
                    ..
                } = method
                {
                    if decorators.is_empty() {
                        continue;
                    }
                    // Skip non-test decorators (e.g. @setup/@teardown on impl methods).
                    let test_dec = match decorators.iter().find(|d| test_attr::is_test_decorator(d))
                    {
                        Some(d) => d,
                        None => continue,
                    };
                    let meta = test_attr::parse_test_meta(test_dec, &mut Vec::new())
                        .unwrap_or_else(|| TestMeta::empty(test_dec.span));

                    let mut full_name = impl_name.clone();
                    full_name.extend(method_name.iter().cloned());

                    tests.push(TestInfo {
                        name: full_name.join("."),
                        file: file.to_path_buf(),
                        meta,
                        module_index,
                        func_name: full_name,
                        setup_fn: None,
                        teardown_fn: None,
                        case_values: None,
                    });
                }
            }
            tests
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn run_single_test_vm(test: &TestInfo, config: &TestConfig) -> TestResult {
    let max_retries = test.meta.retry.unwrap_or(0);
    let mut attempt = 0;

    loop {
        let r = run_test_attempt(test, config.nocapture);
        let should_retry = !r.passed && !r.ignored && attempt < max_retries;
        if should_retry {
            attempt += 1;
            continue;
        }
        let mut r = r;
        r.retried = attempt;

        if let Some(threshold) = config.slow_threshold {
            if r.passed && !r.ignored && r.duration >= threshold {
                r.slow = true;
            }
        }
        if !r.ignored && r.duration > config.soft_budget {
            r.over_budget = true;
        }

        return r;
    }
}

fn run_test_attempt(test: &TestInfo, nocapture: bool) -> TestResult {
    let start = Instant::now();

    if test.meta.ignore {
        return TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            ignored: true,
            slow: false,
            over_budget: false,
            reason: test.meta.reason.clone(),
            duration: start.elapsed(),
            error_msg: None,
            retried: 0,
        };
    }

    // If timeout is set, run in a thread and join with deadline.
    let result = if let Some(timeout_ms) = test.meta.timeout_ms {
        let test_clone = TestInfo {
            name: test.name.clone(),
            file: test.file.clone(),
            meta: test.meta.clone(),
            module_index: test.module_index,
            func_name: test.func_name.clone(),
            setup_fn: test.setup_fn.clone(),
            teardown_fn: test.teardown_fn.clone(),
            case_values: test.case_values.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_test_isolated(&test_clone)
            }));
            let _ = tx.send(outcome);
        });
        match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(outcome) => outcome,
            Err(_) => {
                return TestResult {
                    name: test.name.clone(),
                    file: test.file.clone(),
                    passed: false,
                    ignored: false,
                    slow: false,
                    over_budget: false,
                    reason: None,
                    duration: start.elapsed(),
                    error_msg: Some(format!("timeout: exceeded {timeout_ms}ms limit")),
                    retried: 0,
                };
            }
        }
    } else {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_test_isolated(test)))
    };

    match result {
        Ok(Ok(())) => TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            ignored: false,
            slow: false,
            over_budget: false,
            reason: None,
            duration: start.elapsed(),
            error_msg: None,
            retried: 0,
        },
        Ok(Err(e)) => {
            let passed = test.meta.should_panic;
            let error_msg = Some(e.clone());
            if !passed && nocapture {
                eprintln!("  FAILED: {}: {e}", test.name);
            } else if passed && nocapture {
                eprintln!("  {}: panicked as expected: {e}", test.name);
            }
            TestResult {
                name: test.name.clone(),
                file: test.file.clone(),
                passed,
                ignored: false,
                slow: false,
                over_budget: false,
                reason: if test.meta.should_panic {
                    Some("should_panic".to_string())
                } else {
                    None
                },
                duration: start.elapsed(),
                error_msg,
                retried: 0,
            }
        }
        Err(panic) => {
            let msg = if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "unknown panic".to_string()
            };

            let passed = test.meta.should_panic;
            if !passed && nocapture {
                eprintln!("  FAILED: {}: panic: {msg}", test.name);
            } else if passed && nocapture {
                eprintln!("  {}: panicked as expected: {msg}", test.name);
            }

            TestResult {
                name: test.name.clone(),
                file: test.file.clone(),
                passed,
                ignored: false,
                slow: false,
                over_budget: false,
                reason: if test.meta.should_panic {
                    Some("should_panic".to_string())
                } else {
                    None
                },
                duration: start.elapsed(),
                error_msg: Some(msg),
                retried: 0,
            }
        }
    }
}

fn run_test_isolated(test: &TestInfo) -> Result<(), String> {
    let loaded = loader::load_program(&test.file)?;

    // Native plugins: dlopen each dependency's build/*.so so `@test`
    // functions can call plugin natives — same as `zz run` (previously the
    // runner type-checked plugin imports but never loaded them, failing
    // at runtime with "undefined variable").
    let mut natives = loaded.natives.clone();
    {
        let project_root = loader::find_project_root(&test.file).unwrap_or_else(|| {
            test.file
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        });
        let plugin_funcs = crate::build::discover_plugin_manifests(&test.file);
        crate::load_vm_plugins(&project_root, &mut natives, &plugin_funcs).map(|_| ())?;
    }

    let merged_stmts: Vec<_> = loaded
        .programs
        .iter()
        .flat_map(|p| p.stmts.iter().cloned())
        .collect();
    let merged_span = loaded
        .programs
        .last()
        .map(|p| p.span)
        .unwrap_or(Span::new(0, 0));
    let merged = zz_frontend::ast::Program {
        stmts: merged_stmts,
        span: merged_span,
    };
    let typed = zz_hir::build_program(
        &merged,
        std::collections::HashMap::new(),
        loaded.funcs.clone(),
        loaded.structs.clone(),
    );
    let types = Arc::new(typed.program.types);
    let structs = typed.program.structs;

    let mut interp = Interp::with_natives(natives);

    for (key, val) in zz_stdlib::stdlib_consts() {
        interp.env.define(&key, Value::Float(val));
        if let Some(rest) = key.strip_prefix("std.") {
            interp.env.define(rest, Value::Float(val));
        }
        if let Some(bare) = key.rsplit('.').next() {
            interp.env.define(bare, Value::Float(val));
        }
    }
    for (name, val) in &loaded.consts {
        interp.env.define(name, Value::Float(*val));
    }

    for zz_prog in zz_stdlib::zz_stdlib_programs() {
        if let Err(e) = interp.run_typed(
            &zz_prog.program,
            Arc::new(zz_prog.types.clone()),
            zz_prog.structs.clone(),
        ) {
            return Err(format!("stdlib init error: {e:?}"));
        }
    }
    // Canonical `std.*` aliases for pure-ZZ helpers.
    zz_stdlib::define_canonical_purezz_aliases(&mut interp.env, &mut interp.funcs);

    {
        let snap = interp.env.flatten();
        for (module, ns) in &loaded.stdlib_aliases {
            let src_prefix = module.rsplit('.').next().unwrap_or(module);
            if ns == src_prefix {
                continue;
            }
            for (k, v) in &snap {
                if k == src_prefix || k.starts_with(&format!("{src_prefix}.")) {
                    let alias_key = if k == src_prefix {
                        ns.clone()
                    } else {
                        format!("{ns}{}", &k[src_prefix.len()..])
                    };
                    interp.env.define(&alias_key, v.clone());
                }
            }
        }
    }

    for (i, program) in loaded.programs.iter().enumerate() {
        if let Err(e) = interp.run_typed(program, types.clone(), structs.clone()) {
            return Err(format!("module error: {e:?}"));
        }
        if i == test.module_index {
            break;
        }
    }

    let func_name = test.func_name.join(".");
    let func_val = interp
        .env
        .get(&func_name)
        .ok_or_else(|| format!("test function `{func_name}` not found in environment"))?;

    // Call @setup if present.
    if let Some(ref setup_name) = test.setup_fn {
        call_named_fn(&mut interp, setup_name, test.meta.span)?;
    }

    // Call the test function (with case arguments if parameterized).
    let args = test.case_values.clone().unwrap_or_default();
    let test_result = match interp.call(func_val, args, test.meta.span) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{e:?}")),
    };

    // Call @teardown if present (always runs, even on test failure).
    if let Some(ref teardown_name) = test.teardown_fn {
        let _ = call_named_fn(&mut interp, teardown_name, test.meta.span);
    }

    test_result
}

/// Call a named zero-arg function in the interpreter. Returns Err on failure.
fn call_named_fn(interp: &mut Interp, name: &str, span: Span) -> Result<(), String> {
    let func_val = interp
        .env
        .get(name)
        .ok_or_else(|| format!("`@setup`/`@teardown` function `{name}` not found"))?;

    match interp.call(func_val, vec![], span) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{name}: {e:?}")),
    }
}

// ---------------------------------------------------------------------------
// AOT engine: per-file dispatch binary, one process per test
// ---------------------------------------------------------------------------

/// Compile one dispatch binary for a test file (dev by default, release
/// with `-p/--release`). The harness inlines the test file's source plus a
/// generated `main(args)` that runs a single test selected by argv[0].
/// Returns `(harness_source_path, binary_path)`; both are cleaned up by
/// [`cleanup_aot`] (the build cache entry is kept).
fn build_aot_harness(
    file: &Path,
    tests: &[TestInfo],
    release: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let source = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read test file `{}`: {e}", file.display()))?;
    // A test file with its own `func main` would collide with the harness
    // entrypoint; park it aside (it never runs under `zz test`).
    let source = source
        .replace("func main(", "func __zz_user_main(")
        .replace("pub func main(", "pub func __zz_user_main(");
    let orig_ns = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{orig_ns}.");

    fn bare_name(qualified: &str, prefix: &str) -> String {
        qualified
            .strip_prefix(prefix)
            .unwrap_or(qualified)
            .to_string()
    }

    fn zz_literal(v: &Value) -> String {
        match v {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => {
                if f.is_finite() && f.fract() == 0.0 {
                    format!("{f:.1}")
                } else {
                    format!("{f:?}")
                }
            }
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => {
                let e: String = s
                    .chars()
                    .flat_map(|c| match c {
                        '\\' => vec!['\\', '\\'],
                        '"' => vec!['\\', '"'],
                        '\n' => vec!['\\', 'n'],
                        '\r' => vec!['\\', 'r'],
                        '\t' => vec!['\\', 't'],
                        c => vec![c],
                    })
                    .collect();
                format!("\"{e}\"")
            }
            _ => "0".to_string(),
        }
    }

    let mut arms = String::new();
    for (idx, test) in tests.iter().enumerate() {
        let call = bare_name(&test.func_name.join("."), &prefix);
        let args = test
            .case_values
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(zz_literal)
            .collect::<Vec<_>>()
            .join(", ");
        let setup = test
            .setup_fn
            .as_deref()
            .map(|s| format!("        {}()\n", bare_name(s, &prefix)))
            .unwrap_or_default();
        let teardown = test
            .teardown_fn
            .as_deref()
            .map(|s| format!("        {}()\n", bare_name(s, &prefix)))
            .unwrap_or_default();
        // Separate `if`s (never `else if`): ZZ unifies `if/else` arm
        // types, and test functions may return anything. The trailing
        // bare `return` both normalizes every arm to unit (a no-else
        // `if` requires a unit arm) and stops dispatch after a match.
        // NOTE: a failing `assert` aborts the process, so `@teardown`
        // is skipped on AOT failure (the VM still runs it). Documented.
        arms.push_str(&format!(
            "    if which == \"t{idx}\" {{\n{setup}        {call}({args})\n{teardown}        return\n    }}\n"
        ));
    }
    arms.push_str("    fail(\"unknown test: \" + which)\n");

    let harness_src = format!(
        "{source}\n// __zz_test_harness__: generated by `zz test --native`, do not edit.\nfunc main(args: [str]) {{\n    if len(args) == 0 {{\n        fail(\"zz test harness: missing test id\")\n    }}\n    which := args[0]\n{arms}}}\n"
    );

    let harness_name = format!("{AOT_HARNESS_PREFIX}{orig_ns}_{}.zz", std::process::id());
    let harness_path = file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&harness_name);
    std::fs::write(&harness_path, &harness_src)
        .map_err(|e| format!("cannot write AOT harness `{}`: {e}", harness_path.display()))?;

    let mode = if release {
        crate::build::BuildMode::Release
    } else {
        crate::build::BuildMode::Dev
    };
    let bin = match crate::build::build_release(
        &harness_path,
        mode,
        &crate::build::ReleaseOptions::default(),
    ) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&harness_path);
            return Err(format!("AOT build failed for `{}`: {e}", file.display()));
        }
    };
    Ok((harness_path, bin))
}

/// Remove harness sources and their published `bin/` copies.
/// The content cache under `~/.zz/cache` is kept for fast reruns.
fn cleanup_aot(bins: &mut BTreeMap<PathBuf, (PathBuf, PathBuf)>) {
    for (_, (harness, bin)) in std::mem::take(bins) {
        let _ = std::fs::remove_file(&harness);
        // Only the `bin/` copy next to the harness goes away; the cache
        // entry stays. Never delete anything outside the harness dir.
        if let Some(name) = harness.file_stem().and_then(|s| s.to_str()) {
            if name.starts_with(AOT_HARNESS_PREFIX) {
                let _ = std::fs::remove_file(&bin);
                // Drop the now-empty `bin/` dir; keep it if others use it.
                if let Some(dir) = bin.parent() {
                    let _ = std::fs::remove_dir(dir);
                }
            }
        }
    }
}

/// Run one test as its own AOT process (`t<idx>` selects the dispatch
/// arm). Exit 0 = pass; anything else = fail (inverted for `should_panic`).
/// `@test(timeout = ms)` kills the child; the soft `--timeout` budget only
/// marks `over_budget` (never interrupts).
fn run_single_test_aot(test: &TestInfo, idx: usize, bin: &Path, config: &TestConfig) -> TestResult {
    let max_retries = test.meta.retry.unwrap_or(0);
    let mut attempt = 0;
    loop {
        let r = run_aot_attempt(test, idx, bin, config);
        if !r.passed && !r.ignored && attempt < max_retries {
            attempt += 1;
            continue;
        }
        let mut r = r;
        r.retried = attempt;
        if let Some(threshold) = config.slow_threshold {
            if r.passed && !r.ignored && r.duration >= threshold {
                r.slow = true;
            }
        }
        if !r.ignored && r.duration > config.soft_budget {
            r.over_budget = true;
        }
        return r;
    }
}

fn run_aot_attempt(test: &TestInfo, idx: usize, bin: &Path, config: &TestConfig) -> TestResult {
    let start = Instant::now();
    if test.meta.ignore {
        return TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            ignored: true,
            slow: false,
            over_budget: false,
            reason: test.meta.reason.clone(),
            duration: start.elapsed(),
            error_msg: None,
            retried: 0,
        };
    }

    let id = format!("t{idx}");
    let mut child = match std::process::Command::new(bin)
        .arg(&id)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return TestResult {
                name: test.name.clone(),
                file: test.file.clone(),
                passed: false,
                ignored: false,
                slow: false,
                over_budget: false,
                reason: None,
                duration: start.elapsed(),
                error_msg: Some(format!("cannot run AOT test binary: {e}")),
                retried: 0,
            };
        }
    };

    // Drain pipes on threads: a chatty test (large stdout) must never
    // block forever on a full pipe while we only poll for exit.
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();
    let out_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = child_stdout {
            use std::io::Read;
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let err_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut err) = child_stderr {
            use std::io::Read;
            let _ = err.read_to_end(&mut buf);
        }
        buf
    });

    // Poll for exit so `@test(timeout = ms)` can kill a hung child.
    let hard_deadline = test
        .meta
        .timeout_ms
        .map(|ms| start + Duration::from_millis(ms));
    enum WaitOutcome {
        Exited(std::process::ExitStatus),
        TimedOut,
        WaitError,
    }
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break WaitOutcome::Exited(status),
            Ok(None) => {
                if let Some(deadline) = hard_deadline {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break WaitOutcome::TimedOut;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break WaitOutcome::WaitError,
        }
    };

    let message_of = |status: std::process::ExitStatus, stderr: &str| {
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        let tail = tail.trim();
        if tail.is_empty() {
            match status.code() {
                Some(code) => format!("AOT test exited with code {code}"),
                None => "AOT test killed by signal".to_string(),
            }
        } else {
            tail.to_string()
        }
    };

    match outcome {
        WaitOutcome::TimedOut => {
            // Drains end at kill-time EOF; join so no reader outlives us.
            let _ = out_drain.join();
            let _ = err_drain.join();
            let ms = test.meta.timeout_ms.unwrap_or(0);
            TestResult {
                name: test.name.clone(),
                file: test.file.clone(),
                passed: false,
                ignored: false,
                slow: false,
                over_budget: false,
                reason: None,
                duration: start.elapsed(),
                error_msg: Some(format!("timeout: exceeded {ms}ms limit")),
                retried: 0,
            }
        }
        WaitOutcome::WaitError => {
            let _ = out_drain.join();
            let _ = err_drain.join();
            TestResult {
                name: test.name.clone(),
                file: test.file.clone(),
                passed: false,
                ignored: false,
                slow: false,
                over_budget: false,
                reason: None,
                duration: start.elapsed(),
                error_msg: Some("AOT test child status unknown".to_string()),
                retried: 0,
            }
        }
        WaitOutcome::Exited(status) => {
            // Cap the failure message: last 20 lines of stderr.
            let stderr = err_drain
                .join()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let _ = out_drain.join();
            if status.success() {
                if test.meta.should_panic {
                    TestResult {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        passed: false,
                        ignored: false,
                        slow: false,
                        over_budget: false,
                        reason: Some("should_panic".to_string()),
                        duration: start.elapsed(),
                        error_msg: Some("expected panic, test passed".to_string()),
                        retried: 0,
                    }
                } else {
                    TestResult {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        passed: true,
                        ignored: false,
                        slow: false,
                        over_budget: false,
                        reason: None,
                        duration: start.elapsed(),
                        error_msg: None,
                        retried: 0,
                    }
                }
            } else {
                let msg = message_of(status, &stderr);
                if test.meta.should_panic {
                    TestResult {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        passed: true,
                        ignored: false,
                        slow: false,
                        over_budget: false,
                        reason: Some("should_panic".to_string()),
                        duration: start.elapsed(),
                        error_msg: Some(msg),
                        retried: 0,
                    }
                } else {
                    if config.nocapture {
                        eprintln!("  FAILED: {}: {msg}", test.name);
                    }
                    TestResult {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        passed: false,
                        ignored: false,
                        slow: false,
                        over_budget: false,
                        reason: None,
                        duration: start.elapsed(),
                        error_msg: Some(msg),
                        retried: 0,
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Output: --list (grouped per file)
// ---------------------------------------------------------------------------

fn print_test_list_grouped(groups: &[(PathBuf, Vec<TestInfo>)], _use_color: bool) {
    let mut total = 0;
    for (file, tests) in groups {
        println!("Running {}", file.display());
        println!("running {} tests", tests.len());
        for t in tests {
            let tag_info = t
                .meta
                .tag
                .as_deref()
                .map(|t| format!(" [tag={t}]"))
                .unwrap_or_default();
            println!("test {}{tag_info}", display_name(&t.name, &t.file));
            total += 1;
        }
        println!();
    }
    println!("{total} tests found");
}

// ---------------------------------------------------------------------------
// Output: cargo-style per-file blocks, no symbols
// ---------------------------------------------------------------------------

/// Bare test name for display: strip the `<file-stem>.` namespace prefix
/// (`basic_assertions.test_x` → `test_x`). Group headers already show the
/// file, so the prefix is noise. Filtering still uses the qualified name.
fn display_name(name: &str, file: &Path) -> String {
    if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
        let prefix = format!("{stem}.");
        if let Some(bare) = name.strip_prefix(&prefix) {
            return bare.to_string();
        }
    }
    name.to_string()
}
/// `898ms` below a second, `1.20s` at/above it.
fn fmt_dur(d: Duration) -> String {
    if d.as_millis() < 1000 {
        format!("{}ms", d.as_millis())
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

fn status_word(r: &TestResult) -> &'static str {
    if r.ignored {
        "ignored"
    } else if r.passed {
        "ok"
    } else if is_failfast_skip(r) {
        "skipped"
    } else {
        "FAILED"
    }
}

/// A `--fail-fast` leftover: never ran, not a real failure.
fn is_failfast_skip(r: &TestResult) -> bool {
    !r.passed && !r.ignored && r.error_msg.as_deref() == Some("skipped (--fail-fast)")
}

/// `test <name> ... <ok|FAILED|ignored> (<dur>)` — word first, status last.
/// Colors: `ok` green, `FAILED` red, `ignored` yellow. No symbols.
fn print_test_line(r: &TestResult, config: &TestConfig) {
    let use_color = config.use_color();
    let status = status_word(r);
    let colored = if use_color {
        match status {
            "ok" => format!("\x1b[32m{status}\x1b[0m"),
            "FAILED" => format!("\x1b[31m{status}\x1b[0m"),
            _ => format!("\x1b[33m{status}\x1b[0m"),
        }
    } else {
        status.to_string()
    };

    let mut extra = String::new();
    if r.retried > 0 {
        extra.push_str(&format!(" (retried {}x)", r.retried));
    }
    if let Some(reason) = r.reason.as_deref() {
        // `should_panic` is engine bookkeeping, not worth a suffix.
        if reason != "should_panic" {
            extra.push_str(&format!(" -- {reason}"));
        }
    }
    if r.slow {
        extra.push_str(" (slow)");
    }
    if r.over_budget {
        extra.push_str(&format!(
            " (took {}; over {} soft budget, not interrupted)",
            fmt_dur(r.duration),
            fmt_dur(config.soft_budget),
        ));
    }

    eprintln!(
        "test {} ... {} ({}){extra}",
        display_name(&r.name, &r.file),
        colored,
        fmt_dur(r.duration)
    );

    if !r.passed && !r.ignored {
        if let Some(msg) = r.error_msg.as_deref() {
            for line in msg.lines() {
                let line = line.trim_end();
                if !line.is_empty() {
                    eprintln!("  {line}");
                }
            }
        }
    }
}

/// `test result: <ok|FAILED>. X passed; Y failed; Z ignored; finished in <dur>`
fn print_file_footer(
    results: &[TestResult],
    elapsed: Duration,
    use_color: bool,
    config: &TestConfig,
) {
    let passed = results.iter().filter(|r| r.passed && !r.ignored).count();
    let failed = results
        .iter()
        .filter(|r| !r.passed && !r.ignored && !is_failfast_skip(r))
        .count();
    let ignored = results.iter().filter(|r| r.ignored).count();
    let skipped = results.iter().filter(|r| is_failfast_skip(r)).count();
    let over = results.iter().filter(|r| r.over_budget).count();

    eprintln!();
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    let skipped_info = if skipped > 0 {
        format!("; {skipped} skipped")
    } else {
        String::new()
    };
    if use_color {
        let v = if failed == 0 {
            format!("\x1b[32m{verdict}\x1b[0m")
        } else {
            format!("\x1b[31m{verdict}\x1b[0m")
        };
        eprintln!(
            "test result: {v}. {passed} passed; {failed} failed; {ignored} ignored{skipped_info}; finished in {}",
            fmt_dur(elapsed),
        );
    } else {
        eprintln!(
            "test result: {verdict}. {passed} passed; {failed} failed; {ignored} ignored{skipped_info}; finished in {}",
            fmt_dur(elapsed),
        );
    }
    if over > 0 {
        eprintln!(
            "note: {over} test(s) exceeded the {} soft budget (not interrupted)",
            fmt_dur(config.soft_budget),
        );
    }
}

fn print_global_footer(results: &[TestResult], elapsed: Duration, files: usize, use_color: bool) {
    let passed = results.iter().filter(|r| r.passed && !r.ignored).count();
    let failed = results
        .iter()
        .filter(|r| !r.passed && !r.ignored && !is_failfast_skip(r))
        .count();
    let ignored = results.iter().filter(|r| r.ignored).count();
    let skipped = results.iter().filter(|r| is_failfast_skip(r)).count();
    let total = results.len();
    eprintln!();
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    let skipped_info = if skipped > 0 {
        format!("; {skipped} skipped")
    } else {
        String::new()
    };
    if use_color {
        let v = if failed == 0 {
            format!("\x1b[32m{verdict}\x1b[0m")
        } else {
            format!("\x1b[31m{verdict}\x1b[0m")
        };
        eprintln!(
            "test result: {v}. {passed} passed; {failed} failed; {ignored} ignored{skipped_info}; {total} total across {files} files; finished in {}",
            fmt_dur(elapsed),
        );
    } else {
        eprintln!(
            "test result: {verdict}. {passed} passed; {failed} failed; {ignored} ignored{skipped_info}; {total} total across {files} files; finished in {}",
            fmt_dur(elapsed),
        );
    }
}

// ---------------------------------------------------------------------------
// Output: JSON
// ---------------------------------------------------------------------------

fn print_json_results(results: &[TestResult]) -> Result<(), String> {
    let mut stdout = std::io::stdout();
    writeln!(stdout, "[").map_err(|e| format!("write error: {e}"))?;

    for (i, r) in results.iter().enumerate() {
        // Fail-fast leftovers never ran: report `skipped`, not `failed`.
        let status = if r.ignored {
            "ignored"
        } else if r.passed {
            "passed"
        } else if is_failfast_skip(r) {
            "skipped"
        } else {
            "failed"
        };

        let file = r.file.display().to_string().replace('\\', "/");
        let error = r
            .error_msg
            .as_deref()
            .map(|e| format!("\"{}\"", json_escape(e)))
            .unwrap_or_else(|| "null".to_string());
        let reason = r
            .reason
            .as_deref()
            .map(|r| format!("\"{}\"", json_escape(r)))
            .unwrap_or_else(|| "null".to_string());

        let comma = if i + 1 < results.len() { "," } else { "" };

        writeln!(
            stdout,
            "  {{\"name\": \"{}\", \"file\": \"{}\", \"status\": \"{}\", \
             \"duration_ms\": {}, \"attempts\": {}, \"slow\": {}, \
             \"error\": {}, \"reason\": {}}}{comma}",
            json_escape(&r.name),
            json_escape(&file),
            status,
            r.duration.as_millis(),
            r.retried + 1,
            r.slow,
            error,
            reason,
        )
        .map_err(|e| format!("write error: {e}"))?;
    }

    writeln!(stdout, "]").map_err(|e| format!("write error: {e}"))?;
    Ok(())
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

// ---------------------------------------------------------------------------
// Output: JUnit XML
// ---------------------------------------------------------------------------

fn write_junit(path: &str, results: &[TestResult]) -> Result<(), String> {
    let total = results.len();
    let failures = results
        .iter()
        .filter(|r| !r.passed && !r.ignored && !is_failfast_skip(r))
        .count();
    let skipped = results
        .iter()
        .filter(|r| r.ignored || is_failfast_skip(r))
        .count();
    let time_s: f64 = results.iter().map(|r| r.duration.as_secs_f64()).sum();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"zz\" tests=\"{total}\" failures=\"{failures}\" skipped=\"{skipped}\" time=\"{time_s:.3}\">\n"
    ));

    for r in results {
        let classname = r.file.display().to_string().replace('\\', "/");
        xml.push_str(&format!(
            "  <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\">",
            xml_escape(&r.name),
            xml_escape(&classname),
            r.duration.as_secs_f64(),
        ));

        if r.ignored || is_failfast_skip(r) {
            let message = r
                .reason
                .as_deref()
                .or(r.error_msg.as_deref())
                .unwrap_or("skipped");
            xml.push_str(&format!("<skipped message=\"{}\"/>", xml_escape(message),));
        } else if !r.passed {
            let message = r.error_msg.as_deref().unwrap_or("test failed");
            xml.push_str(&format!(
                "<failure message=\"{}\">{}</failure>",
                xml_escape(message),
                xml_escape(message),
            ));
        }

        xml.push_str("</testcase>\n");
    }

    xml.push_str("</testsuite>\n");

    std::fs::write(path, &xml)
        .map_err(|e| format!("failed to write JUnit XML to `{path}`: {e}"))?;

    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ---------------------------------------------------------------------------
// File collection
// ---------------------------------------------------------------------------

fn collect_zz_files(path: &str) -> Result<Vec<PathBuf>, String> {
    let p = Path::new(path);
    if p.is_file() {
        if p.extension().and_then(|e| e.to_str()) == Some("zz") {
            return Ok(vec![p.to_path_buf()]);
        }
        return Err(format!(
            "`{path}` is not a .zz file\n\n\
             hint: provide a .zz file or a directory"
        ));
    }
    if p.is_dir() {
        let mut files = Vec::new();
        collect_zz_recursive(p, &mut files)?;
        files.sort();
        return Ok(files);
    }
    Err(format!(
        "`{path}` does not exist\n\n\
         hint: check the path and try again"
    ))
}

fn collect_zz_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("failed to read `{}`: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Skip VCS, build outputs, and linked dependencies: `vendor/`
                // holds symlinks into the CAS (often self-referential via
                // path deps), which would recurse forever. Matches the
                // skip sets in `zz_pm::hash` / `zz_pm::cas` and the
                // scaffolded `.gitignore` (vendor/, build/, src/bin/).
                // `bin/` holds published AOT artifacts next to sources.
                if name.starts_with('.')
                    || name == "target"
                    || name == "vendor"
                    || name == "build"
                    || name == "bin"
                {
                    continue;
                }
            }
            collect_zz_recursive(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("zz") {
            // Never pick up generated AOT harnesses (also removed after
            // the run; the skip guards leftovers from killed runs).
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem.starts_with(AOT_HARNESS_PREFIX) {
                    continue;
                }
            }
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// use std::io::IsTerminal
// ---------------------------------------------------------------------------

use std::io::IsTerminal;
