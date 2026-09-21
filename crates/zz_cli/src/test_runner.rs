//! `zz test` — discover and run `@test`-annotated functions.
//!
//! Design decisions (see docs/test-isolation.md):
//! - Each test runs in a **fresh `Interp`** (in-process isolation; no fork).
//! - Panic isolation via `std::panic::catch_unwind`.
//! - Timeout via cooperative budget (op counter), not wall-clock.
//! - `--nocapture` forces `--serial` (interleaved output is noise).
//! - `--jobs N` uses a thread pool with work-stealing (default: logical cores).

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zz_frontend::ast::Stmt;
use zz_frontend::diag::{render_to_string, Files};
use zz_frontend::span::Span;
use zz_frontend::test_attr::{self, TestMeta};
use zz_runtime::{Interp, Value};

use crate::loader;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

struct TestConfig {
    path: String,
    filter: Option<String>,
    exact: bool,
    tag: Option<String>,
    skip: Option<String>,
    fail_fast: bool,
    slow_threshold: Option<Duration>,
    nocapture: bool,
    serial: bool,
    jobs: usize,
    seed: u64,
    list_only: bool,
    json_output: bool,
    junit_path: Option<String>,
    repeat: usize,
    changed: bool,
}

impl TestConfig {
    fn parse(args: &[String]) -> Result<Self, String> {
        // Load zz.toml defaults first.
        let defaults = load_zz_toml_defaults();

        let mut path: Option<String> = None;
        let mut filter: Option<String> = None;
        let mut exact = false;
        let mut tag: Option<String> = None;
        let mut skip: Option<String> = None;
        let mut fail_fast = defaults.fail_fast;
        let mut slow_threshold = defaults.slow_threshold;
        let mut nocapture = false;
        let mut serial = defaults.serial;
        let mut jobs: Option<usize> = defaults.jobs;
        let mut seed: Option<u64> = defaults.seed;
        let mut list_only = false;
        let mut json_output = false;
        let mut junit_path: Option<String> = None;
        let mut repeat: usize = defaults.repeat;
        let mut changed = false;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--nocapture" | "--no-capture" => nocapture = true,
                "--serial" => serial = true,
                "--exact" => exact = true,
                "--fail-fast" => fail_fast = true,
                "--list" => list_only = true,
                "--json" => json_output = true,
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
                           --list           List discovered tests without running\n  \
                           --nocapture      Show stdout/stderr live (forces serial)\n  \
                           --slow-threshold Mark passes slower than N ms with ⚠️\n  \
                           --json           Structured JSON output to stdout\n  \
                           --junit <path>   JUnit XML to file\n  \
                           --repeat N       Run tests N times\n  \
                           --changed        Only run tests for files changed vs git HEAD\n  \
                           -h, --help       Show this help\n\n\
                         Without a path, discovers @test functions in the current directory."
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

        let path = path.unwrap_or_else(|| ".".to_string());

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
            slow_threshold,
            nocapture,
            serial,
            jobs,
            seed,
            list_only,
            json_output,
            junit_path,
            repeat,
            changed,
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
    seed: Option<u64>,
    repeat: usize,
}

impl Default for TestDefaults {
    fn default() -> Self {
        Self {
            jobs: None,
            serial: false,
            fail_fast: false,
            slow_threshold: None,
            seed: None,
            repeat: 1,
        }
    }
}

/// Load `[test]` section from `zz.toml` in the current directory.
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

    d
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

    let mut all_tests: Vec<TestInfo> = Vec::new();
    let mut load_errors = false;

    for file in &files {
        match discover_tests(file) {
            Ok(tests) => all_tests.extend(tests),
            Err(e) => {
                eprintln!("zz test: {e}");
                load_errors = true;
            }
        }
    }

    if load_errors {
        return Err("failed to load some test files".to_string());
    }

    apply_filters(&mut all_tests, &config);

    if all_tests.is_empty() {
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
        return Ok(());
    }

    if config.list_only {
        print_test_list(&all_tests, config.use_color());
        return Ok(());
    }

    // Deterministic shuffle
    let mut tests = all_tests;
    if !config.serial {
        let mut rng = Rng::new(config.seed);
        rng.shuffle(&mut tests);
    }

    let repeat = config.repeat;
    let mut all_results: Vec<TestResult> = Vec::new();
    let overall_start = Instant::now();

    for iteration in 0..repeat {
        if repeat > 1 && !config.json_output {
            eprintln!("\n--- iteration {}/{} ---", iteration + 1, repeat);
        }

        let start = Instant::now();
        let results = if config.parallel() {
            run_parallel(&tests, &config)?
        } else {
            run_serial(&tests, &config)?
        };

        let iter_failed = results.iter().filter(|r| !r.passed && !r.ignored).count();

        // Summary per iteration (non-json)
        if !config.json_output {
            let seed_note = if !config.serial {
                format!(" (seed={})", config.seed)
            } else {
                String::new()
            };
            if repeat > 1 {
                eprintln!("  iteration {}:", iteration + 1);
            }
            print_summary(&results, start.elapsed(), config.use_color(), &seed_note);
        }

        all_results.extend(results);

        // --fail-fast across iterations
        if config.fail_fast && iter_failed > 0 {
            break;
        }
    }

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
        let total_passed = all_results
            .iter()
            .filter(|r| r.passed && !r.ignored)
            .count();
        let total_failed = all_results
            .iter()
            .filter(|r| !r.passed && !r.ignored)
            .count();
        let total_ignored = all_results.iter().filter(|r| r.ignored).count();
        let total = all_results.len();
        eprintln!(
            "\n--- aggregate: {total_failed} failed, {total_passed} passed, {total_ignored} ignored, {total} total in {:.2?} ---",
            overall_start.elapsed()
        );
    }

    let failed = all_results
        .iter()
        .filter(|r| !r.passed && !r.ignored)
        .count();
    if failed > 0 {
        Err("tests failed".to_string())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Serial runner
// ---------------------------------------------------------------------------

fn run_serial(tests: &[TestInfo], config: &TestConfig) -> Result<Vec<TestResult>, String> {
    let mut results = Vec::new();
    let total = tests.len();
    let use_color = config.use_color();

    for (idx, test) in tests.iter().enumerate() {
        if !config.nocapture && !config.json_output && use_color {
            eprint!("\r\x1b[2m  running {}/{} ...\x1b[0m", idx + 1, total);
            let _ = std::io::stderr().flush();
        }

        let r = run_single_test(test, config);

        if !config.nocapture && !config.json_output && use_color {
            eprint!("\r\x1b[2K");
        }

        if !config.nocapture && !config.json_output {
            print_result_line(&r, use_color, config.slow_threshold);
        }

        let should_fail_fast = config.fail_fast && !r.passed && !r.ignored;
        results.push(r);

        if should_fail_fast && idx + 1 < total {
            eprintln!("  stopping after first failure (--fail-fast)");
            break;
        }
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Parallel runner
// ---------------------------------------------------------------------------

fn run_parallel(tests: &[TestInfo], config: &TestConfig) -> Result<Vec<TestResult>, String> {
    let total = tests.len();
    let jobs = config.jobs.min(total);
    let use_color = config.use_color();

    // Shared state
    let queue: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new((0..total).collect()));
    let results: Arc<Mutex<Vec<Option<TestResult>>>> = Arc::new(Mutex::new(vec![None; total]));
    let completed = Arc::new(AtomicUsize::new(0));
    let fail_fast_flag = Arc::new(AtomicBool::new(false));

    // Progress thread (only for TTY, non-json, non-nocapture)
    let show_progress = !config.nocapture && !config.json_output && use_color;
    let progress_handle = if show_progress {
        let completed = Arc::clone(&completed);
        Some(std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(100));
            let done = completed.load(Ordering::Relaxed);
            if done >= total {
                break;
            }
            eprint!("\r\x1b[2m  running {done}/{total} ...\x1b[0m");
            let _ = std::io::stderr().flush();
        }))
    } else {
        None
    };

    std::thread::scope(|s| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                let queue = Arc::clone(&queue);
                let results = Arc::clone(&results);
                let completed = Arc::clone(&completed);
                let fail_fast_flag = Arc::clone(&fail_fast_flag);
                let config_ref = config;

                s.spawn(move || {
                    loop {
                        // Pull next test from queue
                        let idx = {
                            let mut q = queue.lock().unwrap();
                            q.pop_front()
                        };

                        let idx = match idx {
                            Some(i) => i,
                            None => break, // queue empty
                        };

                        // Check fail-fast
                        if fail_fast_flag.load(Ordering::Relaxed) {
                            // Put it back so we don't lose it, but break
                            queue.lock().unwrap().push_front(idx);
                            break;
                        }

                        let test = &tests[idx];
                        let r = run_single_test(test, config_ref);

                        // Check fail-fast after run
                        if config_ref.fail_fast && !r.passed && !r.ignored {
                            fail_fast_flag.store(true, Ordering::Relaxed);
                        }

                        // Store result
                        {
                            let mut res = results.lock().unwrap();
                            res[idx] = Some(r);
                        }

                        let _done = completed.fetch_add(1, Ordering::Relaxed) + 1;

                        // Print result line (parallel mode: print as completed)
                        if !config_ref.nocapture && !config_ref.json_output {
                            let res = results.lock().unwrap();
                            if let Some(ref r) = res[idx] {
                                // Clear progress, print result
                                if use_color {
                                    eprint!("\r\x1b[2K");
                                }
                                print_result_line(r, use_color, config_ref.slow_threshold);
                            }
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    });

    // Wait for progress thread
    if let Some(h) = progress_handle {
        let _ = h.join();
    }

    // Clear progress line one last time
    if show_progress {
        eprint!("\r\x1b[2K");
    }

    // Collect results in order
    let results = Arc::try_unwrap(results)
        .map_err(|_| "results Arc still referenced".to_string())?
        .into_inner()
        .unwrap();

    let results: Vec<TestResult> = results
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| {
            r.or_else(|| {
                // Skipped due to fail-fast
                Some(TestResult {
                    name: tests[i].name.clone(),
                    file: tests[i].file.clone(),
                    passed: false,
                    ignored: false,
                    slow: false,
                    reason: None,
                    duration: Duration::ZERO,
                    error_msg: Some("skipped (--fail-fast)".to_string()),
                    retried: 0,
                })
            })
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Filter pipeline
// ---------------------------------------------------------------------------

fn apply_filters(tests: &mut Vec<TestInfo>, config: &TestConfig) {
    if let Some(ref pat) = config.filter {
        if config.exact {
            tests.retain(|t| t.name == *pat);
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

fn run_single_test(test: &TestInfo, config: &TestConfig) -> TestResult {
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

    let mut interp = Interp::with_natives(loaded.natives.clone());

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
// Output: --list
// ---------------------------------------------------------------------------

fn print_test_list(tests: &[TestInfo], use_color: bool) {
    for t in tests {
        let tag_info = t
            .meta
            .tag
            .as_deref()
            .map(|t| format!(" [tag={t}]"))
            .unwrap_or_default();
        if use_color {
            println!(
                "  \x1b[36m{}\x1b[0m :: {}{tag_info}",
                t.file.display(),
                t.name
            );
        } else {
            println!("  {} :: {}{tag_info}", t.file.display(), t.name);
        }
    }
    println!();
    println!("{} tests found", tests.len());
}

// ---------------------------------------------------------------------------
// Output: per-test result line
// ---------------------------------------------------------------------------

fn print_result_line(r: &TestResult, use_color: bool, _slow_threshold: Option<Duration>) {
    let (sym, color) = if r.passed {
        if r.ignored {
            ("\u{25CB}", "\x1b[33m") // ○ yellow
        } else {
            ("\u{2713}", "\x1b[32m") // ✓ green
        }
    } else {
        ("\u{2717}", "\x1b[31m") // ✗ red
    };

    let retry_info = if r.retried > 0 {
        format!(" (retried {}x)", r.retried)
    } else {
        String::new()
    };

    let reason_info = r
        .reason
        .as_deref()
        .map(|r| format!(" \u{2014} {r}"))
        .unwrap_or_default();

    let slow_info = if r.slow { " \u{26A0}\u{FE0F}" } else { "" };

    let dur_ms = r.duration.as_millis();

    if use_color {
        eprintln!(
            "  {color}{sym}\x1b[0m {} {color}{dur_ms}ms\x1b[0m{retry_info}{reason_info}{slow_info}",
            r.name,
        );
    } else {
        eprintln!(
            "  {sym} {} {dur_ms}ms{retry_info}{reason_info}{slow_info}",
            r.name,
        );
    }
}

// ---------------------------------------------------------------------------
// Output: summary
// ---------------------------------------------------------------------------

fn print_summary(results: &[TestResult], elapsed: Duration, use_color: bool, seed_note: &str) {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed && !r.ignored).count();
    let failed = results.iter().filter(|r| !r.passed && !r.ignored).count();
    let ignored = results.iter().filter(|r| r.ignored).count();
    let slow = results.iter().filter(|r| r.slow).count();

    eprintln!();

    if use_color {
        if failed == 0 {
            eprint!("\x1b[32m{passed} passed\x1b[0m");
        } else {
            eprint!("\x1b[31m{failed} failed\x1b[0m, \x1b[32m{passed} passed\x1b[0m");
        }
        if ignored > 0 {
            eprint!(", {ignored} ignored");
        }
        if slow > 0 {
            eprint!(", \x1b[33m{slow} slow\x1b[0m");
        }
        eprintln!(", {total} total{seed_note} ({elapsed:.2?})");
    } else {
        if failed == 0 {
            eprint!("{passed} passed");
        } else {
            eprint!("{failed} failed, {passed} passed");
        }
        if ignored > 0 {
            eprint!(", {ignored} ignored");
        }
        if slow > 0 {
            eprint!(", {slow} slow");
        }
        eprintln!(", {total} total{seed_note} ({elapsed:.2?})");
    }
}

// ---------------------------------------------------------------------------
// Output: JSON
// ---------------------------------------------------------------------------

fn print_json_results(results: &[TestResult]) -> Result<(), String> {
    let mut stdout = std::io::stdout();
    writeln!(stdout, "[").map_err(|e| format!("write error: {e}"))?;

    for (i, r) in results.iter().enumerate() {
        let status = if r.ignored {
            "ignored"
        } else if r.passed {
            "passed"
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
    let failures = results.iter().filter(|r| !r.passed && !r.ignored).count();
    let skipped = results.iter().filter(|r| r.ignored).count();
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

        if r.ignored {
            let message = r.reason.as_deref().unwrap_or("ignored");
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
                if name.starts_with('.') || name == "target" {
                    continue;
                }
            }
            collect_zz_recursive(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("zz") {
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// use std::io::IsTerminal
// ---------------------------------------------------------------------------

use std::io::IsTerminal;
