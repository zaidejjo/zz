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
//!   AOT: child kill). `--timeout <ms>` (default 60s) is a hard per-test
//!   budget — overruns fail the test. `--soft-timeout` restores the old
//!   notice-only behavior.
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

use zz_build::{build_release, discover_plugin_manifests, BuildMode, ReleaseOptions};
use zz_checker::FuncSig;
use zz_frontend::ast::Stmt;
use zz_frontend::diag::{render_to_string, Files};
use zz_frontend::doctest;
use zz_frontend::span::Span;
use zz_frontend::test_attr::{self, TestMeta};
use zz_loader::rewrite::Rewriter;
use zz_loader::{find_project_root, load_program, LoadResult};
use zz_plugin::{load_c_plugin, load_manifest, load_plugin, LoadError, PluginLib};
use zz_pm::lock::Lockfile;
use zz_pm::manifest::Manifest;
use zz_runtime::{Interp, NativeEntry, Value};

/// Per-test time budget: overruns fail the test unless `--soft-timeout`
/// restores notice-only mode. Configurable via `--timeout <ms>`.
const DEFAULT_TIME_BUDGET: Duration = Duration::from_secs(60);

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
    paths: Vec<String>,
    filter: Option<String>,
    exact: bool,
    tag: Option<String>,
    skip: Option<String>,
    fail_fast: bool,
    fail_on_empty: bool,
    slow_threshold: Option<Duration>,
    time_budget: Duration,
    /// Notice-only budget overruns (legacy `--timeout` behavior).
    soft_timeout: bool,
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
    allow_source_builds: bool,
    allow_hooks: bool,
    update_snapshots: bool,
    no_doctests: bool,
}

impl TestConfig {
    fn parse(args: &[String]) -> Result<Self, String> {
        // Load zz.toml defaults first (optional; absence is fine).
        let defaults = load_zz_toml_defaults();

        let mut paths: Vec<String> = Vec::new();
        let mut filter: Option<String> = None;
        let mut exact = false;
        let mut tag: Option<String> = None;
        let mut skip: Option<String> = None;
        let mut fail_fast = defaults.fail_fast;
        let mut fail_on_empty = false;
        let mut slow_threshold = defaults.slow_threshold;
        let mut time_budget = defaults.time_budget;
        let mut soft_timeout = defaults.soft_timeout;
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
        let mut allow_source_builds = false;
        let mut allow_hooks = false;
        let mut update_snapshots = defaults.update_snapshots;
        let mut no_doctests = false;

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
                            time_budget = Duration::from_millis(ms);
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
                "--no-doctests" => no_doctests = true,
                "--list" => list_only = true,
                "--json" => json_output = true,
                "--native" | "--aot" => {
                    eprintln!("note: `--native` is an alias for `--engine native` (canonical)");
                    engine = TestEngine::Native;
                }
                "-p" | "--release" => native_release = true,
                "--allow-source-builds" => allow_source_builds = true,
                "--allow-hooks" => allow_hooks = true,
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
                        time_budget = Duration::from_millis(ms);
                    }
                }
                "--soft-timeout" => soft_timeout = true,
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
                "--update-snapshots" => update_snapshots = true,
                "--nocapture-impl" => {}
                "--help" | "-h" => {
                    return Err("usage: zz test [file.zz | directory]... [FLAGS]\n\n\
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
                           --timeout <ms>   Hard time budget per test (default 60000);\n  \
                           overruns fail unless --soft-timeout is passed\n  \
                           --soft-timeout   Budget overruns print a notice only (legacy mode)\n  \
                           --engine=vm|native  Select the test engine (default vm)\n  \
                           --native         Alias for --engine native (deprecated)\n  \
                           -p, --release    With --engine native: optimized build (default: dev)\n  \
                           --allow-source-builds  With --native: compile transitive native deps\n  \
                           from source when no prebuilt covers the host tag\n  \
                           --allow-hooks    With --native: run legacy [native] build hooks\n  \
                           (direct deps only; transitive hooks always error)\n  \
                           --json           Structured JSON output to stdout\n  \
                           --junit <path>   JUnit XML to file\n  \
                           --repeat N       Run tests N times\n  \
                           --changed        Only run tests for files changed vs git HEAD\n  \
                           --update-snapshots  Write assert_snapshot goldens instead of comparing\n  \
                           --no-doctests    Skip `/// ```zz` doc-tests\n  \
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
                    // Multiple positionals run together
                    // (`zz test a.zz b.zz dir/`); previously only the
                    // last one survived, silently dropping the rest.
                    paths.push(args[i].clone());
                }
            }
            i += 1;
        }

        if paths.is_empty() {
            paths.push(default_test_path());
        }
        let paths = paths;

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
            paths,
            filter,
            exact,
            tag,
            skip,
            fail_fast,
            fail_on_empty,
            slow_threshold,
            time_budget,
            soft_timeout,
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
            allow_source_builds,
            allow_hooks,
            update_snapshots,
            no_doctests,
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
    time_budget: Duration,
    /// Notice-only budget overruns (legacy `--timeout` behavior).
    soft_timeout: bool,
    seed: Option<u64>,
    repeat: usize,
    engine: TestEngine,
    update_snapshots: bool,
}

impl Default for TestDefaults {
    fn default() -> Self {
        Self {
            jobs: None,
            serial: false,
            fail_fast: false,
            slow_threshold: None,
            time_budget: DEFAULT_TIME_BUDGET,
            soft_timeout: false,
            seed: None,
            repeat: 1,
            engine: TestEngine::Vm,
            update_snapshots: false,
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
        d.time_budget = Duration::from_millis(v.max(0) as u64);
    }
    if let Some(v) = test_section.get("soft-timeout").and_then(|v| v.as_bool()) {
        d.soft_timeout = v;
    }
    if let Some(v) = test_section.get("engine").and_then(|v| v.as_str()) {
        d.engine = match v {
            "native" | "aot" => TestEngine::Native,
            _ => TestEngine::Vm,
        };
    }
    if let Some(v) = test_section
        .get("update-snapshots")
        .and_then(|v| v.as_bool())
    {
        d.update_snapshots = v;
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

// ---------------------------------------------------------------------------
// Snapshot store env (`assert_snapshot`)
// ---------------------------------------------------------------------------

/// Env passthrough for snapshot tests. The runner points it at
/// `<test file parent>/snapshots` per group; `zz run` (no runner) falls
/// back to `./snapshots`. Update mode writes instead of comparing.
const SNAPSHOT_ENV_DIR: &str = "ZZ_SNAPSHOTS_DIR";
const SNAPSHOT_ENV_UPDATE: &str = "ZZ_UPDATE_SNAPSHOTS";

/// Saves the ambient snapshot env on creation, restores it on drop so a
/// `zz test` process never leaks the per-group settings.
struct SnapshotEnv {
    prev_dir: Option<String>,
    prev_update: Option<String>,
}

impl SnapshotEnv {
    fn capture() -> Self {
        Self {
            prev_dir: std::env::var(SNAPSHOT_ENV_DIR).ok(),
            prev_update: std::env::var(SNAPSHOT_ENV_UPDATE).ok(),
        }
    }
}

impl Drop for SnapshotEnv {
    fn drop(&mut self) {
        restore_snapshot_env(SNAPSHOT_ENV_DIR, self.prev_dir.take());
        restore_snapshot_env(SNAPSHOT_ENV_UPDATE, self.prev_update.take());
    }
}

fn restore_snapshot_env(key: &str, prev: Option<String>) {
    // SAFETY: groups run sequentially on this thread; no concurrent
    // getenv races inside the runner. Same contract as `std.env.set`.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

/// Point snapshot natives at this group's store for the duration of the
/// group. AOT children inherit the process env; VM natives read it per
/// call — both stay consistent because groups run sequentially.
fn set_snapshot_env_for(file: &Path, config: &TestConfig) {
    let dir = file
        .parent()
        .map(|p| p.join("snapshots"))
        .unwrap_or_else(|| PathBuf::from("snapshots"));
    // SAFETY: see `restore_snapshot_env`.
    unsafe {
        std::env::set_var(SNAPSHOT_ENV_DIR, dir);
        if config.update_snapshots {
            std::env::set_var(SNAPSHOT_ENV_UPDATE, "1");
        } else {
            std::env::remove_var(SNAPSHOT_ENV_UPDATE);
        }
    }
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
    /// Duration exceeded the `--timeout` budget. Fails the test unless
    /// `--soft-timeout` restores notice-only mode.
    over_budget: bool,
    reason: Option<String>,
    duration: Duration,
    error_msg: Option<String>,
    /// Captured stdout (VM: per-test buffer; AOT: child stdout).
    /// Shown only for failed tests; always present in `--json`.
    stdout: String,
    retried: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct TestInfo {
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
    /// Doc-test that must fail to compile or run (inverts every error into
    /// a pass; a clean pass becomes a failure). Always `false` for
    /// ordinary tests.
    expect_compile_fail: bool,
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
    for root in &config.paths {
        enforce_project_zz(std::path::Path::new(root))?;
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for root in &config.paths {
        files.extend(collect_zz_files(root)?);
    }
    files.sort();
    files.dedup();
    let files = if config.changed {
        // When --changed, only load files that git says are modified.
        let changed = git_changed_files().unwrap_or_default();
        let changed_set: std::collections::HashSet<PathBuf> = changed.into_iter().collect();
        files
            .into_iter()
            .filter(|p| {
                changed_set.contains(p)
                    || std::fs::canonicalize(p)
                        .ok()
                        .map(|c| changed_set.contains(&c))
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>()
    } else {
        files
    };

    if files.is_empty() {
        let msg = if config.changed {
            "no changed .zz files found (git diff HEAD)".to_string()
        } else {
            format!(
                "no .zz files found in `{}`\n\n\
                 hint: ensure the path contains .zz source files",
                config.paths.join(", ")
            )
        };
        if config.json_output {
            println!("[]");
        } else {
            eprintln!("warning: zz test: {msg}");
            eprintln!("hint: no .zz files discovered — nothing ran, exit stays 0");
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
            eprintln!("warning: zz test: {msg}");
            if !config.fail_on_empty {
                eprintln!(
                    "hint: no tests ran — exit stays 0; use --fail-on-empty to fail CI on empty match"
                );
            }
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
            match build_aot_harness(file, tests, &config) {
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
    // Snapshot store env (restored on scope exit, even on early returns).
    let _snap_guard = SnapshotEnv::capture();

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
            if config.native_release && config.engine == TestEngine::Vm {
                eprintln!("note: -p/--release only affects --native (VM ignores it)");
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
            // Groups run sequentially, so one live snapshot setting is
            // race-free; AOT children inherit it; VM reads it per call.
            set_snapshot_env_for(file, &config);
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
        stdout: String::new(),
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
    // `tag = "serial"` lane: tests touching process-global state
    // (log level, env, ports) run sequentially up front, even in
    // parallel mode; the rest share the pool. Documented in testing.md.
    let (serial_lane, par_lane): (Vec<usize>, Vec<usize>) =
        (0..tests.len()).partition(|&i| tests[i].meta.tag.as_deref() == Some("serial"));
    let total = tests.len();
    let jobs = config.jobs.min(par_lane.len().max(1)).max(1);

    let queue: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new(par_lane.into_iter().collect()));
    let results: Arc<Mutex<Vec<Option<TestResult>>>> = Arc::new(Mutex::new(vec![None; total]));
    let fail_fast_flag = Arc::new(AtomicBool::new(false));
    // Serializes multi-line result blocks so parallel completions never
    // interleave mid-test.
    let print_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    let aot_bin = aot_bin.map(|p| p.to_path_buf());

    // Serial lane first, in file order (streams live, like serial mode).
    for idx in serial_lane {
        if config.fail_fast && fail_fast_flag.load(Ordering::Relaxed) {
            let s = skipped_result(&tests[idx], "skipped (--fail-fast)");
            if !config.json_output {
                let _guard = print_lock.lock().unwrap();
                print_test_line(&s, config);
            }
            results.lock().unwrap()[idx] = Some(s);
            continue;
        }
        let r = match aot_bin.as_deref() {
            Some(bin) => run_single_test_aot(&tests[idx], idx, bin, config),
            None => run_single_test_vm(&tests[idx], config),
        };
        if config.fail_fast && !r.passed && !r.ignored {
            fail_fast_flag.store(true, Ordering::Relaxed);
        }
        if !config.json_output {
            let _guard = print_lock.lock().unwrap();
            print_test_line(&r, config);
        }
        results.lock().unwrap()[idx] = Some(r);
    }

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
    // `--no-doctests`: drop synthetic doctest entries (grafting still
    // defines them, inert and never called — but malformed fences still
    // fail the file at load; see `graft_doctests`).
    if config.no_doctests {
        tests.retain(|t| !(t.func_name.len() == 1 && t.func_name[0].starts_with("__zz_doctest_")));
    }
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
    let mut loaded = load_program(path)?;

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

    // Doc-tests: graft `/// ```zz` fences as synthetic `@test` funcs.
    // Deterministic (same source → same funcs), so discovery and the
    // fresh loads at execution time always agree. Returns func-name →
    // display-name for the grafted tests.
    let doctest_names = graft_doctests(&mut loaded, path)?;

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

    // Synthetic doctest funcs get human display names (`doctest stem#n`)
    // and the compile_fail flag; `func_name` stays the real identifier.
    for test in tests.iter_mut() {
        if test.func_name.len() == 1 {
            if let Some((display, compile_fail)) = doctest_names.get(&test.func_name[0]) {
                test.name = display.clone();
                test.expect_compile_fail = *compile_fail;
            }
        }
    }

    Ok(tests)
}

/// Derive the namespacing context for doctest grafting from a loaded
/// (already rewritten) module: the consensus namespace of dotted top-level
/// definitions, their bare names, and import alias heads. Returns
/// `None` namespace for script-mode modules (all bare — no rewriting).
/// Same-file multi-namespace edges (#380) keep the majority head; alias
/// heads are left for the rewriter's import set instead.
fn doctest_rewrite_context(
    stmts: &[Stmt],
) -> (
    Option<String>,
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut heads: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut defs: Vec<String> = Vec::new();
    let mut decls: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut imports = std::collections::HashSet::new();
    for stmt in stmts {
        match stmt {
            Stmt::Func { name, .. }
            | Stmt::Struct { name, .. }
            | Stmt::TypeAlias { name, .. }
            | Stmt::Enum { name, .. } => {
                let full = name.join(".");
                defs.push(full.clone());
                if let Some((head, _)) = full.split_once('.') {
                    *heads.entry(head.to_string()).or_insert(0) += 1;
                }
            }
            Stmt::Decl { name, .. } => {
                defs.push(name.name.clone());
                decls.insert(name.name.clone());
                if let Some((head, _)) = name.name.split_once('.') {
                    *heads.entry(head.to_string()).or_insert(0) += 1;
                }
            }
            Stmt::Import { path, alias, .. } => {
                if let Some(a) = alias {
                    imports.insert(a.clone());
                } else if let Some(last) = path.last() {
                    imports.insert(last.clone());
                }
            }
            _ => {}
        }
    }
    let ns_opt = heads.into_iter().max_by_key(|(_, n)| *n).map(|(h, _)| h);
    let mut top = std::collections::HashSet::new();
    let mut top_bindings = std::collections::HashSet::new();
    if let Some(ns) = ns_opt.as_deref() {
        let prefix = format!("{ns}.");
        for full in &defs {
            top.insert(full.strip_prefix(&prefix).unwrap_or(full).to_string());
        }
        // Module-scope value bindings are receivers like any local: the
        // dot-call arm must not qualify their method names (#500). The
        // loader's `namespace_program` passes the same set.
        for decl in &decls {
            top_bindings.insert(decl.strip_prefix(&prefix).unwrap_or(decl).to_string());
        }
    } else {
        top.extend(defs);
        top_bindings.extend(decls);
    }
    (ns_opt, top, top_bindings, imports)
}

/// Graft doc-test fences as synthetic `@test` functions into every loaded
/// program. Pure + deterministic: extraction, padded parsing (line-exact
/// spans), and decorator construction depend only on file text, so the
/// discovery load and each execution load produce identical funcs.
/// Returns func-name → display-name for grafted tests. Hard errors
/// (bad fence, unparseable body) fail the file like load errors.
fn graft_doctests(
    loaded: &mut LoadResult,
    path: &Path,
) -> Result<std::collections::HashMap<String, (String, bool)>, String> {
    use zz_frontend::ast::{Block, Decorator, Expr, Ident};
    use zz_frontend::parser::parse;

    let mut names = std::collections::HashMap::new();
    for i in 0..loaded.programs.len() {
        let (name, source) = match loaded.files.get(i) {
            Some(f) => f.clone(),
            None => continue,
        };
        // Mirror the module's own namespacing for grafted bodies: derive
        // the namespace + bare top-level names from the loaded (rewritten)
        // siblings, then run the real rewriter over each synthetic func
        // body. Without this, bare references to sibling items fail at
        // runtime — the exact treatment user `@test` bodies received at
        // load. Script-mode modules (all bare) need no rewriting.
        let (ns_opt, top, top_bindings, imports) =
            doctest_rewrite_context(&loaded.programs[i].stmts);
        let stem = std::path::Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let (docs, mut diags) = doctest::extract(&source, &name);
        // File preludes (` ```zz,prelude `) compile once and prepend to
        // every test fence in this file (hidden shared setup). Parsed with
        // the same line-exact padding as bodies.
        let mut prelude_stmts = Vec::new();
        for doc in docs.iter().filter(|d| d.is_prelude) {
            let parsed = parse(&doc.padded_source());
            diags.extend(parsed.errors);
            if diags
                .iter()
                .any(|d| d.severity == zz_frontend::diag::Severity::Error)
            {
                let mut files = Files::new();
                let id = files.add(name.clone(), source.clone());
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err(format!("failed doctests in `{}`", path.display()));
            }
            prelude_stmts.extend(parsed.program.stmts);
        }
        // Parse each fence body with newline padding so spans sit on the
        // original file lines; render against the original source, whose
        // lines then match exactly.
        for doc in docs.iter().filter(|d| !d.is_prelude) {
            let padded = doc.padded_source();
            let parsed = parse(&padded);
            diags.extend(parsed.errors);
            // A `compile_fail` fence is *supposed* to break: a parse
            // failure becomes a runtime `fail()` stub (raised → inverted
            // to a pass at execution); anything else is still fatal.
            let parse_failed = diags
                .iter()
                .any(|d| d.severity == zz_frontend::diag::Severity::Error);
            if parse_failed && !doc.compile_fail {
                let mut files = Files::new();
                let id = files.add(name.clone(), source.clone());
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err(format!("failed doctests in `{}`", path.display()));
            }
            let mut named = Vec::new();
            if doc.ignore {
                named.push((
                    Ident {
                        name: "ignore".to_string(),
                        span: doc.header_span,
                    },
                    Expr::Bool {
                        value: true,
                        span: doc.header_span,
                    },
                ));
            }
            if doc.should_panic {
                named.push((
                    Ident {
                        name: "should_panic".to_string(),
                        span: doc.header_span,
                    },
                    Expr::Bool {
                        value: true,
                        span: doc.header_span,
                    },
                ));
            }
            if doc.no_run {
                // Check-only: reuse the ignore machinery (reported with a
                // reason, never executed) — uniform across VM, AOT, cover.
                named.push((
                    Ident {
                        name: "ignore".to_string(),
                        span: doc.header_span,
                    },
                    Expr::Bool {
                        value: true,
                        span: doc.header_span,
                    },
                ));
                named.push((
                    Ident {
                        name: "reason".to_string(),
                        span: doc.header_span,
                    },
                    Expr::Str {
                        value: "no_run doctest".to_string(),
                        span: doc.header_span,
                    },
                ));
            }
            let mut stmts = prelude_stmts.clone();
            if parse_failed {
                // `compile_fail` body didn't parse: stand in a runtime
                // failure so execution inverts it into a pass.
                stmts.push(Stmt::Expr(Expr::Call {
                    callee: Box::new(Expr::Ident {
                        name: "fail".to_string(),
                        span: doc.header_span,
                    }),
                    args: vec![Expr::Str {
                        value: "compile_fail fence".to_string(),
                        span: doc.header_span,
                    }],
                    named: Vec::new(),
                    span: doc.header_span,
                }));
            } else {
                stmts.extend(parsed.program.stmts);
            }
            let block_span = parsed.program.span;
            let mut func = Stmt::Func {
                name: vec![doc.func_name()],
                generics: Vec::new(),
                params: Vec::new(),
                ret: None,
                body: Block {
                    stmts,
                    span: block_span,
                },
                span: doc.header_span,
                pub_: false,
                decorators: vec![Decorator {
                    path: vec!["test".to_string()],
                    args: Vec::new(),
                    named,
                    span: doc.header_span,
                }],
            };
            // Same namespacing user bodies received: bare sibling references
            // become qualified paths (locals shadow correctly — the real
            // rewriter tracks scopes). Script-mode modules skip this.
            if let Some(ns) = ns_opt.as_deref() {
                let mut rw = Rewriter::new(ns, &top, &top_bindings);
                rw.imports = imports.clone();
                rw.push_scope();
                if let Stmt::Func { body, .. } = &mut func {
                    rw.rewrite_block(body);
                }
                rw.pop_scope();
            }
            loaded.programs[i].stmts.push(func);
            names.insert(doc.func_name(), (doc.display_name(&stem), doc.compile_fail));
        }
        if !diags.is_empty() {
            let mut files = Files::new();
            let id = files.add(name.clone(), source.clone());
            eprint!("{}", render_to_string(&files, id, &diags));
            return Err(format!("failed doctests in `{}`", path.display()));
        }
    }
    Ok(names)
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
                expect_compile_fail: false,
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
                        expect_compile_fail: false,
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
        apply_time_budget(&mut r, config);

        return r;
    }
}

/// Apply the `--timeout` budget to a finished attempt: mark `over_budget`,
/// and — unless `--soft-timeout` — fail tests that passed functionally but
/// overran the budget. Genuine failures, ignores, and fail-fast skips are
/// left untouched.
fn apply_time_budget(r: &mut TestResult, config: &TestConfig) {
    if r.ignored || is_failfast_skip(r) || r.duration <= config.time_budget {
        return;
    }
    r.over_budget = true;
    if !config.soft_timeout && r.passed {
        r.passed = false;
        r.error_msg = Some(format!(
            "timeout: exceeded {} budget (took {})",
            fmt_dur(config.time_budget),
            fmt_dur(r.duration),
        ));
    }
}

/// Env marker: this process IS a timeout worker (spawned by a parent
/// `zz test`), so timed tests run inline instead of spawning grandchildren.
const WORKER_ENV: &str = "ZZ_TEST_WORKER";

fn worker_mode() -> bool {
    std::env::var_os(WORKER_ENV).is_some()
}

/// Drain a child process's pipes on threads: a chatty child must never
/// block forever on a full pipe while the parent only polls for exit.
struct PipeDrains {
    out: std::thread::JoinHandle<Vec<u8>>,
    err: Option<std::thread::JoinHandle<Vec<u8>>>,
}

fn spawn_drains(child: &mut std::process::Child, with_stderr: bool) -> PipeDrains {
    let child_stdout = child.stdout.take();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut o) = child_stdout {
            use std::io::Read;
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let err = with_stderr.then(|| {
        let child_stderr = child.stderr.take();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut e) = child_stderr {
                use std::io::Read;
                let _ = e.read_to_end(&mut buf);
            }
            buf
        })
    });
    PipeDrains { out, err }
}

fn finish_drains(d: PipeDrains) -> (Vec<u8>, Vec<u8>) {
    (
        d.out.join().unwrap_or_default(),
        d.err
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default(),
    )
}

/// One attempt with per-test stdout capture (VM engine).
///
/// Unless `--nocapture` (live output), `print`/`println` during the
/// attempt go to a thread-local buffer instead of the terminal. The
/// buffer lands on [`TestResult::stdout`]: shown only for failed tests,
/// always present in `--json`. Passing suites stay quiet — and piped
/// JSON is never polluted by test prints.
fn run_test_attempt(test: &TestInfo, nocapture: bool) -> TestResult {
    if !nocapture {
        zz_stdlib::natives::io::test_capture_start();
    }
    let mut r = run_test_attempt_inner(test, nocapture);
    if !nocapture {
        r.stdout = zz_stdlib::natives::io::test_capture_take();
    }
    r
}

fn run_test_attempt_inner(test: &TestInfo, nocapture: bool) -> TestResult {
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
            stdout: String::new(),
            retried: 0,
        };
    }

    // A hard `@test(timeout = ms)` runs the test in a worker subprocess
    // (re-executed `zz test` for just this test) so the deadline is a
    // true wall-clock kill: the child is killed on overrun, leaving no
    // abandoned thread behind (threads can't be killed in Rust) and
    // preempting even blocking natives. Untimed tests stay in-process.
    let result = if let Some(timeout_ms) = test.meta.timeout_ms {
        if worker_mode() {
            // Worker child itself: run inline (never spawn grandchildren).
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_test_isolated(test)))
        } else {
            return run_test_in_worker(test, timeout_ms, nocapture);
        }
    } else {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_test_isolated(test)))
    };

    match result {
        Ok(Ok(())) => {
            // A `compile_fail` doctest that compiled AND ran clean is a
            // failure: the snippet was supposed to break.
            let passed = !test.expect_compile_fail;
            let error_msg = if test.expect_compile_fail {
                if nocapture {
                    eprintln!("  FAILED: {}: expected compile failure", test.name);
                }
                Some("expected compile failure, but the snippet compiled and ran".to_string())
            } else {
                None
            };
            TestResult {
                name: test.name.clone(),
                file: test.file.clone(),
                passed,
                ignored: false,
                slow: false,
                over_budget: false,
                reason: None,
                duration: start.elapsed(),
                error_msg,
                stdout: String::new(),
                retried: 0,
            }
        }
        Ok(Err(e)) => {
            let passed = test.meta.should_panic || test.expect_compile_fail;
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
                } else if test.expect_compile_fail {
                    Some("compile_fail".to_string())
                } else {
                    None
                },
                duration: start.elapsed(),
                error_msg,
                stdout: String::new(),
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

            let passed = test.meta.should_panic || test.expect_compile_fail;
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
                } else if test.expect_compile_fail {
                    Some("compile_fail".to_string())
                } else {
                    None
                },
                duration: start.elapsed(),
                error_msg: Some(msg),
                stdout: String::new(),
                retried: 0,
            }
        }
    }
}

/// Run a timed VM test in a worker subprocess (same `zz` binary, single
/// test, JSON report) with a true wall-clock deadline.
///
/// The child re-discovers just this test (`--exact` on its qualified
/// name, scoped to its file) and prints one JSON object; the parent maps
/// it back onto [`TestResult`]. On overrun the child is killed — nothing
/// leaks, and even blocking natives are preempted. Only timed tests pay
/// the spawn cost; everything else stays in-process.
fn run_test_in_worker(test: &TestInfo, timeout_ms: u64, nocapture: bool) -> TestResult {
    let start = Instant::now();
    let fail = |msg: String| TestResult {
        name: test.name.clone(),
        file: test.file.clone(),
        passed: false,
        ignored: false,
        slow: false,
        over_budget: false,
        reason: None,
        duration: start.elapsed(),
        error_msg: Some(msg),
        stdout: String::new(),
        retried: 0,
    };

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        // No worker possible (embedded binary): run inline without
        // timeout enforcement rather than refusing the test.
        Err(_) => {
            return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_test_isolated(test)
            })) {
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
                    stdout: String::new(),
                    retried: 0,
                },
                Ok(Err(e)) => fail(e),
                Err(_) => fail("worker fallback panicked".to_string()),
            };
        }
    };
    let file = match std::fs::canonicalize(&test.file) {
        Ok(p) => p,
        Err(e) => {
            return fail(format!(
                "cannot resolve test file `{}`: {e}",
                test.file.display()
            ));
        }
    };

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("test")
        .arg(&file)
        .arg("--exact")
        .arg("--filter")
        .arg(&test.name)
        .arg("--serial")
        .arg("--json")
        .env(WORKER_ENV, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(if nocapture {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::piped()
        });
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot spawn test worker: {e}")),
    };
    let drains = spawn_drains(&mut child, !nocapture);

    let deadline = start + Duration::from_millis(timeout_ms);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break true,
        }
    };
    let (out_bytes, err_bytes) = finish_drains(drains);

    if timed_out {
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
            stdout: String::new(),
            retried: 0,
        };
    }

    map_worker_report(test, &out_bytes, &err_bytes, start.elapsed())
}

/// Map a worker child's JSON report back onto [`TestResult`].
/// Unparseable output is a loud failure, never a silent pass.
fn map_worker_report(
    test: &TestInfo,
    out_bytes: &[u8],
    err_bytes: &[u8],
    duration: Duration,
) -> TestResult {
    let fail = |msg: String| TestResult {
        name: test.name.clone(),
        file: test.file.clone(),
        passed: false,
        ignored: false,
        slow: false,
        over_budget: false,
        reason: None,
        duration,
        error_msg: Some(msg),
        stdout: String::new(),
        retried: 0,
    };
    let text = String::from_utf8_lossy(out_bytes);
    let entry = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.as_array()?.first().cloned())
        .and_then(|v| v.as_object().cloned());
    let Some(entry) = entry else {
        let err_text = String::from_utf8_lossy(err_bytes).into_owned();
        let tail: Vec<&str> = err_text.lines().rev().take(10).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        let tail = tail.trim();
        let msg = if tail.is_empty() {
            "test worker produced no report".to_string()
        } else {
            format!("test worker produced no report:\n{tail}")
        };
        return fail(msg);
    };
    let status = entry
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("failed");
    let opt_str = |k: &str| entry.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let child_duration = entry
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .map(Duration::from_millis)
        .unwrap_or(duration);
    let stdout = opt_str("stdout").unwrap_or_default();
    match status {
        "passed" => TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            ignored: false,
            slow: false,
            over_budget: false,
            reason: opt_str("reason"),
            duration: child_duration,
            error_msg: opt_str("error"),
            stdout,
            retried: 0,
        },
        "ignored" => TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            ignored: true,
            slow: false,
            over_budget: false,
            reason: opt_str("reason"),
            duration: child_duration,
            error_msg: None,
            stdout,
            retried: 0,
        },
        _ => TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            ignored: false,
            slow: false,
            over_budget: false,
            reason: opt_str("reason"),
            duration: child_duration,
            error_msg: opt_str("error"),
            stdout,
            retried: 0,
        },
    }
}

fn run_test_isolated(test: &TestInfo) -> Result<(), String> {
    let (mut interp, func_val, args) = setup_test_interp(test, false)?;

    // Call @setup if present.
    if let Some(ref setup_name) = test.setup_fn {
        call_named_fn(&mut interp, setup_name, test.meta.span)?;
    }

    // Call the test function (with case arguments if parameterized).
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

/// Shared setup for isolated test execution: load, graft doctests,
/// plugins, HIR build, interpreter seed, stdlib + module execution up to
/// the test's own module. Returns the interpreter plus the resolved test
/// function value and its case arguments; callers run setup/test/teardown
/// calls themselves. With `cover`, the recorder is enabled and every user
/// module is labeled from `loaded.files` (stdlib runs stay unlabeled).
fn setup_test_interp(test: &TestInfo, cover: bool) -> Result<(Interp, Value, Vec<Value>), String> {
    let mut loaded = load_program(&test.file)?;

    // Same doctest graft as discovery (deterministic): without it the
    // synthetic `__zz_doctest_*` functions would not exist at execution.
    // Diagnostics were already shown at discovery; discard the names here.
    // A file whose fences break between discovery and execution fails here
    // instead of running a stale test list — correct behavior.
    let _ = graft_doctests(&mut loaded, &test.file)?;

    // Native plugins: dlopen each dependency's build/*.so so `@test`
    // functions can call plugin natives — same as `zz run` (previously the
    // runner type-checked plugin imports but never loaded them, failing
    // at runtime with "undefined variable").
    let mut natives = loaded.natives.clone();
    {
        let project_root = find_project_root(&test.file).unwrap_or_else(|| {
            test.file
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        });
        let plugin_funcs = discover_plugin_manifests(&test.file);
        load_vm_plugins(&project_root, &mut natives, &plugin_funcs).map(|_| ())?;
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
        loaded.funcs,
        loaded.structs,
        loaded.aliases,
        loaded.enums,
    );
    let types = Arc::new(typed.program.types);
    let structs = typed.program.structs;
    let enums = typed.program.enums;

    let mut interp = Interp::with_natives(natives);

    if cover {
        interp.enable_coverage();
    }

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
        let zz_prog = match zz_prog {
            Ok(p) => p,
            Err(e) => return Err(format!("stdlib init error: {e:?}")),
        };
        if let Err(e) = interp.run_typed(
            &zz_prog.program,
            Arc::new(zz_prog.types.clone()),
            zz_prog.structs.clone(),
            zz_prog.enums.clone(),
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
        if cover {
            if let Some((name, source)) = loaded.files.get(i) {
                interp.set_coverage_source(name, source);
            }
        }
        if let Err(e) = interp.run_typed(program, types.clone(), structs.clone(), enums.clone()) {
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

    let args = test.case_values.clone().unwrap_or_default();
    Ok((interp, func_val, args))
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

/// Run one test with the coverage recorder enabled, merging-friendly:
/// returns the test's pass/fail plus its coverage state. Setup and
/// teardown run exactly like [`run_test_isolated`] (inline — no timeout
/// worker subprocesses; a hanging test hangs coverage, documented in
/// `docs/coverage.md`).
pub(crate) fn run_test_covered(
    test: &TestInfo,
) -> Result<(bool, zz_runtime::CoverageState), String> {
    let (mut interp, func_val, args) = setup_test_interp(test, true)?;

    if let Some(ref setup_name) = test.setup_fn {
        call_named_fn(&mut interp, setup_name, test.meta.span)?;
    }
    let res = interp.call(func_val, args, test.meta.span);
    if let Some(ref teardown_name) = test.teardown_fn {
        let _ = call_named_fn(&mut interp, teardown_name, test.meta.span);
    }
    // `should_panic` inversion mirrors `run_test_attempt_inner`: an
    // expected failure still counts the executed lines.
    let passed = match res {
        Ok(_) => !test.meta.should_panic,
        Err(_) => test.meta.should_panic,
    };
    let cov = interp
        .coverage
        .clone()
        .ok_or_else(|| "coverage recorder missing after covered run".to_string())?;
    Ok((passed, cov))
}

/// Discover every test under `root` (file or directory; `None` selects
/// the same default as `zz test`) and run each with coverage, merging
/// all states. Ignored tests are skipped like the runner skips them.
/// Returns the merged state plus the failing test names (empty = all
/// passed); failures still contribute their partial coverage.
pub fn cover_tests(root: Option<&str>) -> Result<(zz_runtime::CoverageState, Vec<String>), String> {
    let roots = match root {
        Some(r) => vec![r.to_string()],
        None => vec![default_test_path()],
    };
    for r in &roots {
        enforce_project_zz(std::path::Path::new(r))?;
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for r in &roots {
        files.extend(collect_zz_files(r)?);
    }
    files.sort();
    files.dedup();

    let mut total = zz_runtime::CoverageState::default();
    let mut failed = Vec::new();
    for file in &files {
        let tests = discover_tests(file)?;
        for test in &tests {
            if test.meta.ignore {
                continue;
            }
            match run_test_covered(test) {
                Ok((passed, cov)) => {
                    if !passed {
                        failed.push(test.name.clone());
                    }
                    total.merge(&cov);
                }
                Err(e) => {
                    failed.push(format!("{} ({e})", test.name));
                }
            }
        }
    }
    Ok((total, failed))
}

// ---------------------------------------------------------------------------
// AOT engine: per-file dispatch binary, one process per test
// ---------------------------------------------------------------------------

/// Compile one dispatch binary for a test file (dev by default, release
/// Alias bare `*.main` imports in AOT harness sources (#401).
///
/// The harness appends its own `func main` entrypoint to the inlined test
/// file, so a test `import nrepro.main` (bare local name `main`) collides:
/// value scope shadows the module and `main.answer()` fails to check with
/// "cannot access field `answer` ...". The VM never injects `func main`,
/// hence the VM/native split (plus a spurious `unused import` warning).
///
/// Fix: rewrite the import with a file-local alias — the documented `as`
/// workaround, applied by the harness — and retarget qualified `main.`
/// references through it. Token-span surgery (lexer tokens, edits applied
/// back-to-front), never string search: strings, comments, and `main(`
/// call sites are untouched. Imports that already alias, and selective
/// `import m(sym)` lists (which bind values, never a `main.` qualifier),
/// are left alone.
fn alias_main_imports(source: &str) -> String {
    use zz_frontend::lexer::lex;
    use zz_frontend::token::TokenKind;

    const ALIAS_BASE: &str = "__zz_harness_main_mod";

    let lexed = lex(source);
    if !lexed.errors.is_empty() {
        return source.to_string();
    }
    let toks = &lexed.tokens;

    // Significant tokens: everything but statement separators (a `main` /
    // `.` pair split across lines is still one qualified access — such
    // code type-checks in the VM today, so the harness must see it too).
    let sig: Vec<usize> = toks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind != TokenKind::StmtEnd)
        .map(|(i, _)| i)
        .collect();

    // First pass: locate bare `*.main` imports. Records (a) insertion of
    // ` as <alias>` after the import path, and (b) the import statement's
    // token range, whose path idents are never reference candidates.
    struct ImportEdit {
        /// Byte offset just past the import path: insert ` as <alias>`.
        insert_at: usize,
        /// Token index range of the whole import statement.
        range: (usize, usize),
    }
    let mut imports: Vec<ImportEdit> = Vec::new();
    let mut s = 0;
    while s < sig.len() {
        if toks[sig[s]].kind != TokenKind::Import {
            s += 1;
            continue;
        }
        // Import path: Ident (Dot Ident)*.
        let mut p = s + 1;
        let mut path: Vec<usize> = Vec::new();
        while p < sig.len() && toks[sig[p]].kind == TokenKind::Ident {
            path.push(sig[p]);
            p += 1;
            if p + 1 < sig.len()
                && toks[sig[p]].kind == TokenKind::Dot
                && toks[sig[p + 1]].kind == TokenKind::Ident
            {
                p += 1; // consume the Dot, loop takes the Ident
            } else {
                break;
            }
        }
        if path.is_empty() {
            s += 1;
            continue;
        }
        // Optional `as` alias.
        let mut aliased = false;
        if p + 1 < sig.len()
            && toks[sig[p]].kind == TokenKind::As
            && toks[sig[p + 1]].kind == TokenKind::Ident
        {
            aliased = true;
            p += 2;
        }
        // Optional selective list: balanced parens (idents/commas/stars).
        let mut selective = false;
        if p < sig.len() && toks[sig[p]].kind == TokenKind::LParen {
            selective = true;
            let mut depth = 0usize;
            while p < sig.len() {
                match toks[sig[p]].kind {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            p += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                p += 1;
            }
        }
        let stmt_end = p; // exclusive end of the import statement
        let last = *path.last().expect("path is non-empty");
        if !aliased && !selective && toks[last].text == "main" {
            let insert_at = toks[last].span.end as usize;
            imports.push(ImportEdit {
                insert_at,
                range: (sig[s], sig[stmt_end.saturating_sub(1)]),
            });
        }
        s = stmt_end.max(s + 1);
    }
    if imports.is_empty() {
        return source.to_string();
    }

    // Collision-free alias: the dunder prefix makes this near-impossible,
    // but a test file could still declare it — suffix until it is absent.
    let taken = |cand: &str| {
        toks.iter()
            .any(|t| (t.kind == TokenKind::Ident || t.kind == TokenKind::Str) && t.text == cand)
    };
    let mut alias = ALIAS_BASE.to_string();
    let mut n = 2u32;
    while taken(&alias) {
        alias = format!("{ALIAS_BASE}_{n}");
        n += 1;
    }

    // Second pass: `main` idents followed by `.`, outside import
    // statements, name the imported module — retarget through the alias.
    // Everything else (`main(` calls, strings, comments, the import paths
    // themselves) is byte-identical.
    let in_import = |idx: usize| {
        imports
            .iter()
            .any(|im| idx >= im.range.0 && idx <= im.range.1)
    };
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for (k, &ti) in sig.iter().enumerate() {
        if toks[ti].kind != TokenKind::Ident || toks[ti].text != "main" {
            continue;
        }
        if in_import(ti) {
            continue;
        }
        let is_qualified = k + 1 < sig.len() && toks[sig[k + 1]].kind == TokenKind::Dot;
        if !is_qualified {
            continue;
        }
        let span = toks[ti].span;
        edits.push((span.start as usize, span.end as usize, alias.clone()));
    }
    for im in &imports {
        edits.push((im.insert_at, im.insert_at, format!(" as {alias}")));
    }
    // Back-to-front so earlier spans stay valid.
    edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out = source.to_string();
    for (start, end, text) in edits {
        out.replace_range(start..end, &text);
    }
    out
}

/// with `-p/--release`). The harness inlines the test file's source plus a
/// generated `main(args)` that runs a single test selected by argv[0].
/// Returns `(harness_source_path, binary_path)`; both are cleaned up by
/// [`cleanup_aot`] (the build cache entry is kept).
fn build_aot_harness(
    file: &Path,
    tests: &[TestInfo],
    config: &TestConfig,
) -> Result<(PathBuf, PathBuf), String> {
    let raw = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read test file `{}`: {e}", file.display()))?;
    // Doc-test source twins, extracted BEFORE main-parking / alias
    // transforms so the VM graft (parsed ASTs) and this harness text derive
    // from identical input (same order → same `__zz_doctest_N` names).
    // Skipped with --no-doctests. Discovery already failed the file on bad
    // fences; the check here only guards TOCTOU drift.
    let doctests = if config.no_doctests {
        Vec::new()
    } else {
        let (docs, diags) = doctest::extract(&raw, &file.display().to_string());
        if diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            let mut files = Files::new();
            let id = files.add(file.display().to_string(), raw.clone());
            eprint!("{}", render_to_string(&files, id, &diags));
            return Err(format!("failed doctests in `{}`", file.display()));
        }
        docs
    };
    // A test file with its own `func main` would collide with the harness
    // entrypoint; park it aside (it never runs under `zz test`).
    let source = raw
        .replace("func main(", "func __zz_user_main(")
        .replace("func main (", "func __zz_user_main (")
        .replace("pub func main(", "pub func __zz_user_main(")
        .replace("pub func main (", "pub func __zz_user_main (");
    // A bare `*.main` import would collide the same way: the harness
    // entrypoint occupies value scope as `main`, shadowing the module, so
    // `main.answer()` fails to check (native only — the VM never injects
    // `func main`). Retarget such imports through a file-local alias (#401).
    let source = alias_main_imports(&source);
    // Append the doctest twins after all transforms (generated code has no
    // mains or imports to rewrite, and must not be rewritten itself).
    // File preludes inline at the top of every twin body. `compile_fail`
    // fences append a `fail()` stub: the real body may not compile, which
    // would break the whole harness build — and the AOT run path reports
    // them as ignored before any arm runs, so the stub never executes.
    let mut source = source;
    let prelude_text = doctests
        .iter()
        .filter(|d| d.is_prelude)
        .map(|d| d.body_text())
        .collect::<Vec<_>>()
        .join("\n");
    for doc in doctests.iter().filter(|d| !d.is_prelude) {
        if doc.compile_fail {
            source.push_str(&format!(
                "@test func {}() {{\n    fail(\"compile_fail fence\")\n}}\n",
                doc.func_name()
            ));
        } else {
            source.push_str(&doc.harness_func(&prelude_text));
        }
    }
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
            Value::Float(f) => float_literal(*f),
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
        // Dispatch arms: `t<idx>` runs `@setup` + the test, `d<idx>`
        // runs `@teardown` alone. The runner always executes the
        // teardown arm afterwards in a fresh process — even when the
        // test aborts the first process via `exit(1)` — mirroring the
        // VM, which drains defers and calls teardown on failure.
        // (Caveat: setup-established *in-memory* state is not visible
        // to teardown across processes; share cross-phase state via
        // the filesystem. Documented.)
        // Separate `if`s (never `else if`): ZZ unifies `if/else` arm
        // types, and test functions may return anything. The trailing
        // bare `return` both normalizes every arm to unit (a no-else
        // `if` requires a unit arm) and stops dispatch after a match.
        arms.push_str(&format!(
            "    if which == \"t{idx}\" {{\n{setup}        {call}({args})\n        return\n    }}\n"
        ));
        if test.teardown_fn.is_some() {
            let td = bare_name(test.teardown_fn.as_deref().unwrap_or(""), &prefix);
            arms.push_str(&format!(
                "    if which == \"d{idx}\" {{\n        {td}()\n        return\n    }}\n"
            ));
        }
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

    let mode = if config.native_release {
        BuildMode::Release
    } else {
        BuildMode::Dev
    };
    let rel = ReleaseOptions {
        allow_source_builds: config.allow_source_builds,
        allow_hooks: config.allow_hooks,
        ..Default::default()
    };
    let bin = match build_release(&harness_path, mode, &rel) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&harness_path);
            return Err(format!("AOT build failed for `{}`: {e}", file.display()));
        }
    };
    Ok((harness_path, bin))
}

/// Remove harness sources and their published binaries.
/// The content cache under `~/.zz/cache` is kept for fast reruns.
fn cleanup_aot(bins: &mut BTreeMap<PathBuf, (PathBuf, PathBuf)>) {
    for (_, (harness, bin)) in std::mem::take(bins) {
        let _ = std::fs::remove_file(&harness);
        // Only the published copy goes away; the cache
        // entry stays. Never delete anything outside the harness dir.
        if let Some(name) = harness.file_stem().and_then(|s| s.to_str()) {
            if name.starts_with(AOT_HARNESS_PREFIX) {
                let _ = std::fs::remove_file(&bin);
                // Drop the now-empty output dir; keep it if others use it.
                if let Some(dir) = bin.parent() {
                    let _ = std::fs::remove_dir(dir);
                }
            }
        }
    }
}

/// Run one test as its own AOT process (`t<idx>` selects the dispatch
/// arm). Exit 0 = pass; anything else = fail (inverted for `should_panic`).
/// `@test(timeout = ms)` kills the child; the `--timeout` budget marks
/// `over_budget` and fails the test unless `--soft-timeout` is passed.
fn run_single_test_aot(test: &TestInfo, idx: usize, bin: &Path, config: &TestConfig) -> TestResult {
    let max_retries = test.meta.retry.unwrap_or(0);
    let mut attempt = 0;
    loop {
        // Test arm always runs first ...
        let r = run_aot_attempt(test, &format!("t{idx}"), bin, config, true);
        // ... then teardown runs in a fresh process even when the test
        // aborted the first one. Best-effort, like the VM's `let _ =`
        // teardown call: failures are discarded, the side effects stand.
        if test.teardown_fn.is_some() {
            let _ = run_aot_attempt(test, &format!("d{idx}"), bin, config, false);
        }
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
        apply_time_budget(&mut r, config);
        return r;
    }
}

fn run_aot_attempt(
    test: &TestInfo,
    arm: &str,
    bin: &Path,
    config: &TestConfig,
    hard_timeout: bool,
) -> TestResult {
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
            stdout: String::new(),
            retried: 0,
        };
    }
    // `compile_fail` doctests are VM-only: AOT compiles the whole file
    // upfront, so a negative-compile test cannot be isolated per test.
    // Reported as ignored (never silently skipped).
    if test.expect_compile_fail {
        return TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            ignored: true,
            slow: false,
            over_budget: false,
            reason: Some("compile_fail is VM-only".to_string()),
            duration: start.elapsed(),
            error_msg: None,
            stdout: String::new(),
            retried: 0,
        };
    }

    let id = arm.to_string();
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
                stdout: String::new(),
                retried: 0,
            };
        }
    };

    let drains = spawn_drains(&mut child, true);

    // Poll for exit; the test arm honors `@test(timeout = ms)` with a
    // kill, the teardown arm waits unbounded (mirrors the VM, which
    // never times out teardown either).
    let hard_deadline = if hard_timeout {
        test.meta
            .timeout_ms
            .map(|ms| start + Duration::from_millis(ms))
    } else {
        None
    };
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

    /// Last 15 lines of a stream, for failure detail.
    fn tail15(s: &str) -> String {
        let tail: Vec<&str> = s.lines().rev().take(15).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
    // Assert diagnostics arrive on stderr; test prints ride on stdout
    // (shown separately with `| `). Keep them apart, no duplication.
    let message_of = |status: std::process::ExitStatus, stderr: &str| {
        let err = tail15(stderr).trim().to_string();
        if err.is_empty() {
            match status.code() {
                Some(code) => format!("AOT test exited with code {code}"),
                None => "AOT test killed by signal".to_string(),
            }
        } else {
            err
        }
    };

    match outcome {
        WaitOutcome::TimedOut => {
            // Drains end at kill-time EOF; join so no reader outlives us.
            let _ = finish_drains(drains);
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
                stdout: String::new(),
                retried: 0,
            }
        }
        WaitOutcome::WaitError => {
            let _ = finish_drains(drains);
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
                stdout: String::new(),
                retried: 0,
            }
        }
        WaitOutcome::Exited(status) => {
            // Cap the failure detail: child stdout tail, then stderr.
            let (out_bytes, err_bytes) = finish_drains(drains);
            let stdout = String::from_utf8_lossy(&out_bytes).into_owned();
            let stderr = String::from_utf8_lossy(&err_bytes).into_owned();
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
                        stdout: String::new(),
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
                        stdout: String::new(),
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
                        stdout,
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
                        stdout,
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

/// Plain-decimal float literal the ZZ lexer accepts (it has no exponent
/// notation, and Rust `{:?}` emits `1e300` past ~1e16). Expansion is
/// digit-exact, so the literal parses back to the same `f64`. Non-finite
/// values can't come from source literals (only via 300+ digit monsters);
/// stay loud instead of substituting silently.
fn float_literal(f: f64) -> String {
    if !f.is_finite() {
        return "1.0/0.0".to_string();
    }
    let s = format!("{f:?}");
    if !(s.contains('e') || s.contains('E')) {
        if f.fract() == 0.0 && !s.contains('.') {
            return format!("{s}.0");
        }
        return s;
    }
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.as_str()),
    };
    let (mant, exp): (&str, i32) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse().unwrap_or(0)),
        None => (rest, 0),
    };
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let frac_len = mant
        .split('.')
        .nth(1)
        .map(|fr| fr.len() as i32)
        .unwrap_or(0);
    let point = digits.len() as i32 - frac_len + exp; // digits before '.'
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if point <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-point) as usize));
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            out.push('0');
        } else {
            out.push_str(trimmed);
        }
    } else if point as usize >= digits.len() {
        out.push_str(&digits);
        out.push_str(&"0".repeat(point as usize - digits.len()));
        out.push_str(".0");
    } else {
        out.push_str(&digits[..point as usize]);
        out.push('.');
        out.push_str(&digits[point as usize..]);
    }
    out
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
        if config.soft_timeout {
            extra.push_str(&format!(
                " (took {}; over {} soft budget, not interrupted)",
                fmt_dur(r.duration),
                fmt_dur(config.time_budget),
            ));
        } else {
            extra.push_str(&format!(
                " (took {}; exceeded {} budget)",
                fmt_dur(r.duration),
                fmt_dur(config.time_budget),
            ));
        }
    }

    eprintln!(
        "test {} ... {} ({}){extra}",
        display_name(&r.name, &r.file),
        colored,
        fmt_dur(r.duration)
    );

    // Captured stdout prints only on failure (cargo-style); passing
    // tests stay quiet unless `--nocapture` streams live.
    if !r.passed && !r.ignored && !r.stdout.trim().is_empty() {
        for line in r.stdout.lines() {
            eprintln!("  | {line}");
        }
    }

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
        if config.soft_timeout {
            eprintln!(
                "note: {over} test(s) exceeded the {} soft budget (not interrupted)",
                fmt_dur(config.time_budget),
            );
        } else {
            eprintln!(
                "note: {over} test(s) exceeded the {} budget (failed; use --soft-timeout for notice-only)",
                fmt_dur(config.time_budget),
            );
        }
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
        // Doctest identity rides the display-name convention
        // (`doctest <stem>#<n>`, set at discovery); no struct churn.
        let kind = if r.name.starts_with("doctest ") {
            "doctest"
        } else {
            "test"
        };

        writeln!(
            stdout,
            "  {{\"name\": \"{}\", \"file\": \"{}\", \"kind\": \"{}\", \"status\": \"{}\", \
             \"duration_ms\": {}, \"attempts\": {}, \"slow\": {}, \
             \"error\": {}, \"reason\": {}, \"stdout\": \"{}\"}}{comma}",
            json_escape(&r.name),
            json_escape(&file),
            kind,
            status,
            r.duration.as_millis(),
            r.retried + 1,
            r.slow,
            error,
            reason,
            json_escape(&r.stdout),
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
        if !r.stdout.trim().is_empty() {
            // CDATA can't contain `]]>`; split it across sections.
            let safe = r.stdout.replace("]]>", "]]]]><![CDATA[>");
            xml.push_str(&format!("<system-out><![CDATA[{safe}]]></system-out>"));
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
                // scaffolded `.gitignore` (vendor/, build/, bin/).
                // `bin/` holds published AOT artifacts at the project root.
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

// Loaded plugin libraries, kept alive for the process lifetime.
// Dropping a `PluginLib` unloads its `.so`, unmapping every registered
// function pointer — so handles are never released once loaded.
// Capped at `MAX_PLUGIN_LIBS` so a single-process test suite cannot grow
// RSS without bound.
static PLUGIN_LIBS: std::sync::OnceLock<std::sync::Mutex<Vec<PluginLib>>> =
    std::sync::OnceLock::new();

/// Cap on retained plugin handles; oldest are dropped when full.
const MAX_PLUGIN_LIBS: usize = 64;

/// Retain a loaded plugin library for the rest of the process.
pub fn keep_plugin_alive(handle: PluginLib) {
    let mut g = PLUGIN_LIBS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .expect("plugin registry lock");
    if g.len() >= MAX_PLUGIN_LIBS {
        g.remove(0);
    }
    g.push(handle);
}

/// Check `zz.toml` compiler version requirement for the project at `start`.
pub fn enforce_project_zz(start: &Path) -> Result<(), String> {
    let Some(root) = find_project_root(start) else {
        return Ok(());
    };
    let manifest_path = root.join("zz.toml");
    if !manifest_path.exists() {
        return Ok(());
    }
    let Ok(manifest) = Manifest::load(&manifest_path) else {
        return Ok(()); // unloadable: the normal flow reports it better
    };
    manifest.check_zz_version(env!("CARGO_PKG_VERSION"))
}

/// Load VM plugins from the project's lockfile.
#[cfg(unix)]
pub fn load_vm_plugins(
    project_dir: &Path,
    natives: &mut std::collections::HashMap<String, NativeEntry>,
    plugin_funcs: &[(String, FuncSig)],
) -> Result<usize, String> {
    // Canonicalize: callers pass roots derived from relative script paths
    // (possibly empty = CWD); every join below must be unambiguous.
    let project_dir =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());

    let lock_path = project_dir.join("zz.lock");
    let lock = match Lockfile::load(&lock_path) {
        Ok(l) => l,
        Err(_) => return Ok(0), // no lock file, no plugins
    };

    // Load manifest to resolve path deps
    let manifest_path = project_dir.join("zz.toml");
    let manifest = Manifest::load(&manifest_path).ok();

    let mut loaded_count = 0;
    for dep in &lock.deps {
        // Resolve package directory (canonicalized: a relative dep path
        // must never leak `..` into hook/dlsym paths downstream).
        let Some(pkg_dir) = zz_build::resolve_pkg_dir(&project_dir, dep, manifest.as_ref()) else {
            continue;
        };

        // Only load plugins that have a plugin.zzi manifest
        let zzi_path = pkg_dir.join("plugin.zzi");
        if !zzi_path.exists() {
            continue;
        }

        // C-only plugins (`// C-ABI: 1` header) resolve symbols directly
        // with dlsym — no Rust shim. Their manifest functions register in
        // the runtime C-ABI registry under ZZ-visible names.
        let c_funcs: Option<std::collections::HashMap<String, FuncSig>> =
            match load_manifest(&zzi_path) {
                Ok(manifest) if manifest.meta.c_abi.is_some() => Some(manifest.funcs),
                _ => None,
            };

        // Look for shared library in build/ directory
        let build_dir = pkg_dir.join("build");
        if !build_dir.exists() {
            continue;
        }

        // Try the platform's canonical shared-library names, then any
        // `.so`/`.dylib`/`.dll` the build hook actually produced.
        let mut lib_paths: Vec<PathBuf> = Vec::new();
        let base = dep.name.replace('-', "_");
        let lib_names = [
            format!(
                "{}{base}{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ),
            format!("{base}{}", std::env::consts::DLL_SUFFIX),
        ];

        for lib_name in &lib_names {
            let lib_path = build_dir.join(lib_name);
            if lib_path.exists() && !lib_paths.contains(&lib_path) {
                lib_paths.push(lib_path);
            }
        }
        if let Ok(entries) = std::fs::read_dir(&build_dir) {
            let is_shared_lib = |p: &Path| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some(ext) if ext == "so" || ext == "dylib" || ext == "dll"
                )
            };
            let mut scanned: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_shared_lib(p) && !lib_paths.contains(p))
                .collect();
            scanned.sort();
            lib_paths.extend(scanned);
        }

        for lib_path in &lib_paths {
            let loaded: Result<(), String> = match &c_funcs {
                Some(funcs) => match load_c_plugin(lib_path, funcs) {
                    Ok(handle) => {
                        keep_plugin_alive(handle);
                        Ok(())
                    }
                    Err(LoadError::MissingSymbol { symbol, .. })
                        if symbol == "ZZ_C_PLUGIN_ABI_VERSION" =>
                    {
                        continue;
                    }
                    Err(e) => Err(e.to_string()),
                },
                None => load_plugin(lib_path, natives)
                    .map(|handle| {
                        // The handle MUST stay alive (see keep_plugin_alive).
                        keep_plugin_alive(handle);
                    })
                    .map_err(|e| e.to_string()),
            };
            match loaded {
                Ok(()) => {
                    if std::env::var("ZZ_VERBOSE").is_ok() {
                        eprintln!("zz: loaded plugin `{}`", dep.name);
                    }
                    loaded_count += 1;
                    break;
                }
                Err(e) => {
                    eprintln!("zz: warning: failed to load plugin `{}`: {e}", dep.name);
                }
            }
        }
    }

    // Alias C-symbol registrations to ZZ-visible dotted names so VM
    // dispatch resolves exactly what AOT lowering calls.
    for (zz_name, sig) in plugin_funcs {
        if natives.contains_key(zz_name) {
            continue;
        }
        let c_sym = sig.c_symbol(zz_name);
        if let Some(entry) = natives.get(&c_sym).cloned() {
            natives.insert(zz_name.clone(), entry);
        }
    }

    Ok(loaded_count)
}

/// Non-Unix stub for VM plugin loading.
#[cfg(not(unix))]
pub fn load_vm_plugins(
    _project_dir: &Path,
    _natives: &mut std::collections::HashMap<String, NativeEntry>,
    _plugin_funcs: &[(String, FuncSig)],
) -> Result<usize, String> {
    // dlopen not supported on this platform yet
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_literal_has_no_exponents_and_roundtrips() {
        for f in [
            0.0, 1.0, -2.5, 0.1, 3.141_62, 123456.789, 1e300, 1.5e-7, -1.25e-10, 5e-324,
        ] {
            let lit = float_literal(f);
            assert!(
                !lit.contains(['e', 'E']),
                "exponent leaked into {lit} for {f}"
            );
            let back: f64 = lit.parse().expect("literal must parse");
            assert_eq!(back, f, "roundtrip failed for {lit}");
        }
        assert_eq!(float_literal(f64::INFINITY), "1.0/0.0");
    }

    #[test]
    fn alias_main_imports_retargets_qualified_uses() {
        // #401: bare `*.main` import + qualified use → aliased import +
        // retargeted uses. Strings, comments, and `main(` calls untouched.
        let src = "import nrepro.main\n@test\nfunc t() {\n    assert_eq(main.answer(), 42)\n    println(\"main.answer() in a string\")\n    // main.answer() in a comment\n}\n";
        let out = alias_main_imports(src);
        assert!(
            out.contains("import nrepro.main as __zz_harness_main_mod"),
            "import not aliased: {out}"
        );
        assert!(
            out.contains("assert_eq(__zz_harness_main_mod.answer(), 42)"),
            "qualified use not retargeted: {out}"
        );
        assert!(
            out.contains("\"main.answer() in a string\""),
            "string literal rewritten: {out}"
        );
        assert!(
            out.contains("// main.answer() in a comment"),
            "comment rewritten: {out}"
        );
    }

    #[test]
    fn alias_main_imports_leaves_clean_sources_alone() {
        // Already-aliased, selective, and unrelated imports are byte-identical.
        for src in [
            "import nrepro.main as app\nprintln(app.answer())\n",
            "import nrepro.main(answer)\nprintln(answer())\n",
            "import redis.client\nprintln(client.ping())\n",
            "println(\"no imports here\")\n",
        ] {
            assert_eq!(alias_main_imports(src), src, "rewrote clean source: {src}");
        }
    }

    #[test]
    fn alias_main_imports_ignores_main_call_sites() {
        // `main(` calls name the harness entrypoint, not the module.
        let src = "import nrepro.main\nfunc t() {\n    main()\n    println(main.answer())\n}\n";
        let out = alias_main_imports(src);
        assert!(out.contains("\n    main()\n"), "call site rewritten: {out}");
        assert!(
            out.contains("println(__zz_harness_main_mod.answer())"),
            "qualified use missed: {out}"
        );
    }
}
