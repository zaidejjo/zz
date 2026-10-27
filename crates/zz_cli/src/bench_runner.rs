//! `zz bench` — run `@bench`-annotated functions and report timings.
//!
//! Separate command from `zz test` by design: benches run serially in file
//! order (no shuffle, no parallelism — both would corrupt timings), each
//! bench runs `warmup` untimed iterations then `iters` timed ones, and the
//! output is a timing table, never pass/fail verdicts.
//!
//! Engines:
//! - VM (default): one interpreter setup per bench; iterations call the
//!   bench function in-process, timed with `Instant`.
//! - AOT (`--engine native`): each bench file is compiled once into a
//!   dispatch binary; every bench runs in its own process, timing itself
//!   with `time.monotonic_nanos` and printing one `BENCH` line.
//!
//! Bench functions must be zero-arg and non-generic (diagnosed at compile
//! time by `bench_attr`). `@setup`/`@teardown` run once around the
//! iterations, not per iteration: keep benches self-contained instead.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zz_frontend::ast::Stmt;
use zz_frontend::bench_attr::{self, BenchMeta};
use zz_frontend::diag::{render_to_string, Files};
use zz_frontend::span::Span;
use zz_runtime::{Interp, Value};

use crate::loader;

/// Default timed iterations / warmup when neither CLI nor decorator sets them.
const DEFAULT_ITERS: u64 = 1000;
const DEFAULT_WARMUP: u64 = 100;

/// Default total budget per bench (overruns abort the bench).
const DEFAULT_BENCH_BUDGET: Duration = Duration::from_secs(60);

/// Harness files generated for AOT mode. Discovery skips them so a
/// leftover harness is never picked up as a bench file.
const AOT_HARNESS_PREFIX: &str = "zz_bench_harness_";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchEngine {
    Vm,
    Native,
}

struct BenchConfig {
    paths: Vec<String>,
    filter: Option<String>,
    exact: bool,
    tag: Option<String>,
    skip: Option<String>,
    iters: u64,
    warmup: u64,
    time_budget: Duration,
    list_only: bool,
    json_output: bool,
    engine: BenchEngine,
    native_release: bool,
    allow_source_builds: bool,
    allow_hooks: bool,
}

impl BenchConfig {
    fn parse(args: &[String]) -> Result<Self, String> {
        let defaults = load_zz_toml_defaults();

        let mut paths: Vec<String> = Vec::new();
        let mut filter: Option<String> = None;
        let mut exact = false;
        let mut tag: Option<String> = None;
        let mut skip: Option<String> = None;
        let mut iters = defaults.iters;
        let mut warmup = defaults.warmup;
        let mut time_budget = defaults.time_budget;
        let mut list_only = false;
        let mut json_output = false;
        let mut engine = defaults.engine;
        let mut native_release = false;
        let mut allow_source_builds = false;
        let mut allow_hooks = false;

        let mut i = 0;
        while i < args.len() {
            // `--flag=value` forms for value flags.
            if args[i].starts_with("--filter=")
                || args[i].starts_with("--tag=")
                || args[i].starts_with("--skip=")
                || args[i].starts_with("--iters=")
                || args[i].starts_with("--warmup=")
                || args[i].starts_with("--timeout=")
                || args[i].starts_with("--engine=")
            {
                let (flag, val) = args[i].split_once('=').unwrap();
                let val = val.to_string();
                match flag {
                    "--filter" => filter = Some(val),
                    "--tag" => tag = Some(val),
                    "--skip" => skip = Some(val),
                    "--iters" => {
                        if let Ok(n) = val.parse::<u64>() {
                            iters = n.max(1);
                        }
                    }
                    "--warmup" => {
                        if let Ok(n) = val.parse::<u64>() {
                            warmup = n;
                        }
                    }
                    "--timeout" => {
                        if let Ok(ms) = val.parse::<u64>() {
                            time_budget = Duration::from_millis(ms);
                        }
                    }
                    "--engine" => {
                        engine = match val.as_str() {
                            "vm" => BenchEngine::Vm,
                            "native" | "aot" => BenchEngine::Native,
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
                "--exact" => exact = true,
                "--list" => list_only = true,
                "--json" => json_output = true,
                "--native" | "--aot" => {
                    eprintln!("note: `--native` is an alias for `--engine native` (canonical)");
                    engine = BenchEngine::Native;
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
                "--iters" | "-n" => {
                    i += 1;
                    if let Some(n) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                        iters = n.max(1);
                    }
                }
                "--warmup" | "-w" => {
                    i += 1;
                    if let Some(n) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                        warmup = n;
                    }
                }
                "--timeout" => {
                    i += 1;
                    if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                        time_budget = Duration::from_millis(ms);
                    }
                }
                "--engine" => {
                    i += 1;
                    match args.get(i).map(String::as_str) {
                        Some("vm") => engine = BenchEngine::Vm,
                        Some("native") | Some("aot") => engine = BenchEngine::Native,
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
                "--help" | "-h" => {
                    return Err("usage: zz bench [file.zz | directory]... [FLAGS]\n\n\
                         FLAGS:\n  \
                           --exact          Exact match filter (not substring)\n  \
                           --filter, -f     Substring filter on bench name\n  \
                           --tag, -t <t>    Only benches with tag = t\n  \
                           --skip, -s <s>   Exclude benches matching substring s\n  \
                           --iters, -n N    Timed iterations per bench (default 1000)\n  \
                           --warmup, -w N   Untimed warmup iterations (default 100)\n  \
                           --timeout <ms>   Total budget per bench (default 60000)\n  \
                           --list           List discovered benches without running\n  \
                           --json           Structured JSON output to stdout\n  \
                           --engine=vm|native  Select the bench engine (default vm)\n  \
                           --native         Alias for --engine native (deprecated)\n  \
                           -p, --release    With --engine native: optimized build (default: dev)\n  \
                           --allow-source-builds  With --native: compile transitive native deps\n  \
                           from source when no prebuilt covers the host tag\n  \
                           --allow-hooks    With --native: run legacy [native] build hooks\n  \
                           (direct deps only; transitive hooks always error)\n  \
                           -h, --help       Show this help\n\n\
                         Benches run serially in file order (never shuffled or\n  \
                         parallelized). Each bench runs `warmup` untimed iterations\n  \
                         then `iters` timed ones. `@bench(iters = N, warmup = M)`\n  \
                         overrides the CLI defaults per bench.\n\n\
                         Without a path, uses ./bench/ when it holds .zz files,\n  \
                         else the current directory. No zz.toml required."
                        .to_string());
                }
                other if other.starts_with('-') => {
                    return Err(format!(
                        "unknown flag: {other}\n\nhint: use `zz bench --help` for usage"
                    ));
                }
                _ => {
                    paths.push(args[i].clone());
                }
            }
            i += 1;
        }

        if paths.is_empty() {
            paths.push(default_bench_path());
        }

        Ok(Self {
            paths,
            filter,
            exact,
            tag,
            skip,
            iters,
            warmup,
            time_budget,
            list_only,
            json_output,
            engine,
            native_release,
            allow_source_builds,
            allow_hooks,
        })
    }

    fn use_color(&self) -> bool {
        if self.json_output {
            return false;
        }
        std::io::stderr().is_terminal()
    }
}

// ---------------------------------------------------------------------------
// zz.toml config
// ---------------------------------------------------------------------------

struct BenchDefaults {
    iters: u64,
    warmup: u64,
    time_budget: Duration,
    engine: BenchEngine,
}

impl Default for BenchDefaults {
    fn default() -> Self {
        Self {
            iters: DEFAULT_ITERS,
            warmup: DEFAULT_WARMUP,
            time_budget: DEFAULT_BENCH_BUDGET,
            engine: BenchEngine::Vm,
        }
    }
}

/// Load `[bench]` section from `zz.toml` in the current directory.
/// Missing file / section is fine (zero-config); only overrides apply.
fn load_zz_toml_defaults() -> BenchDefaults {
    let Ok(content) = std::fs::read_to_string("zz.toml") else {
        return BenchDefaults::default();
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return BenchDefaults::default();
    };
    let Some(bench_section) = table.get("bench").and_then(|v| v.as_table()) else {
        return BenchDefaults::default();
    };

    let mut d = BenchDefaults::default();

    if let Some(v) = bench_section.get("iters").and_then(|v| v.as_integer()) {
        d.iters = (v.max(1)) as u64;
    }
    if let Some(v) = bench_section.get("warmup").and_then(|v| v.as_integer()) {
        d.warmup = (v.max(0)) as u64;
    }
    if let Some(v) = bench_section.get("timeout").and_then(|v| v.as_integer()) {
        d.time_budget = Duration::from_millis(v.max(0) as u64);
    }
    if let Some(v) = bench_section.get("engine").and_then(|v| v.as_str()) {
        d.engine = match v {
            "native" | "aot" => BenchEngine::Native,
            _ => BenchEngine::Vm,
        };
    }

    d
}

/// No-arg bench root: `./bench/` when it holds `.zz` files, else `.`.
/// Zero-config — no `zz.toml` needed.
fn default_bench_path() -> String {
    let bench = Path::new("bench");
    if bench.is_dir() && dir_has_zz(bench) {
        return "bench".to_string();
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
struct BenchInfo {
    name: String,
    file: PathBuf,
    meta: BenchMeta,
    module_index: usize,
    func_name: Vec<String>,
    setup_fn: Option<String>,
    teardown_fn: Option<String>,
    tag: Option<String>,
}

#[derive(Debug, Clone)]
struct BenchResult {
    name: String,
    file: PathBuf,
    iters: u64,
    warmup: u64,
    median_ns: u64,
    mean_ns: u64,
    min_ns: u64,
    max_ns: u64,
    error: Option<String>,
    over_budget: bool,
}

#[derive(Debug, Clone, Default)]
struct BenchStats {
    median_ns: u64,
    mean_ns: u64,
    min_ns: u64,
    max_ns: u64,
}

fn stats_of(samples: &mut [u64]) -> BenchStats {
    if samples.is_empty() {
        return BenchStats::default();
    }
    samples.sort_unstable();
    let min_ns = samples[0];
    let max_ns = samples[samples.len() - 1];
    let median_ns = samples[samples.len() / 2];
    let sum: u128 = samples.iter().map(|&s| s as u128).sum();
    let mean_ns = (sum / samples.len() as u128) as u64;
    BenchStats {
        median_ns,
        mean_ns,
        min_ns,
        max_ns,
    }
}

// ---------------------------------------------------------------------------
// Top-level entry
// ---------------------------------------------------------------------------

pub fn bench_command(args: &[String]) -> Result<(), String> {
    let config = BenchConfig::parse(args)?;

    // Fail fast on an unsatisfied `[package] zz` compiler requirement.
    for root in &config.paths {
        crate::enforce_project_zz(std::path::Path::new(root))?;
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for root in &config.paths {
        files.extend(collect_zz_files(root)?);
    }
    files.sort();
    files.dedup();
    // Never pick up generated AOT harnesses (either engine's).
    files.retain(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| !n.starts_with("zz_bench_harness_") && !n.starts_with("zz_test_harness_"))
            .unwrap_or(true)
    });

    if files.is_empty() {
        let msg = format!(
            "no .zz files found in `{}`\n\n\
             hint: ensure the path contains .zz source files",
            config.paths.join(", ")
        );
        if config.json_output {
            println!("[]");
        } else {
            eprintln!("warning: zz bench: {msg}");
            eprintln!("hint: no .zz files discovered — nothing ran, exit stays 0");
        }
        return Ok(());
    }

    // Discover per file (keeps file grouping for output).
    let mut groups: Vec<(PathBuf, Vec<BenchInfo>)> = Vec::new();
    let mut load_errors = false;
    for file in &files {
        match discover_benches(file) {
            Ok(benches) => {
                if !benches.is_empty() {
                    groups.push((file.clone(), benches));
                }
            }
            Err(e) => {
                eprintln!("zz bench: {e}");
                load_errors = true;
            }
        }
    }

    if load_errors {
        return Err("failed to load some bench files".to_string());
    }

    // Filter within each group; drop empty groups.
    for (_, benches) in groups.iter_mut() {
        let mut v = std::mem::take(benches);
        apply_filters(&mut v, &config);
        *benches = v;
    }
    groups.retain(|(_, b)| !b.is_empty());

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
                format!("0 benches matched ({})", parts.join(", "))
            }
            _ => "no benches found".to_string(),
        };
        if config.json_output {
            println!("[]");
        } else {
            eprintln!("warning: zz bench: {msg}");
            eprintln!("hint: mark functions with `@bench` to benchmark them");
        }
        return Ok(());
    }

    if config.list_only {
        print_bench_list_grouped(&groups, config.use_color());
        return Ok(());
    }

    // AOT: compile one dispatch binary per file up front (dev by default,
    // release with `-p/--release`). VM needs no setup.
    let mut aot_bins: BTreeMap<PathBuf, (PathBuf, PathBuf)> = BTreeMap::new();
    if config.engine == BenchEngine::Native {
        for (file, benches) in &groups {
            match build_bench_harness(file, benches, &config) {
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

    let use_color = config.use_color();
    let engine_label = match config.engine {
        BenchEngine::Vm => "vm",
        BenchEngine::Native => {
            if config.native_release {
                "aot (release)"
            } else {
                "aot (dev)"
            }
        }
    };
    if !config.json_output {
        eprintln!("engine: {engine_label}");
        if config.native_release && config.engine == BenchEngine::Vm {
            eprintln!("note: -p/--release only affects --engine native (VM ignores it)");
        }
    }

    let mut all_results: Vec<BenchResult> = Vec::new();
    let total_files = groups.len();
    for (gi, (file, benches)) in groups.iter().enumerate() {
        if !config.json_output {
            if gi > 0 {
                eprintln!();
            }
            eprintln!("Running {}", file.display());
            eprintln!("running {} benches", benches.len());
        }
        let bin = aot_bins.get(file).map(|(_, b)| b.clone());
        for (bi, bench) in benches.iter().enumerate() {
            let r = match bin.as_deref() {
                Some(bin) => run_single_bench_aot(bench, &format!("b{bi}"), bin, &config),
                None => run_single_bench_vm(bench, &config),
            };
            if !config.json_output {
                print_bench_line(&r, use_color);
            }
            all_results.push(r);
        }
    }

    cleanup_aot(&mut aot_bins);

    if config.json_output {
        print_json_results(&all_results)?;
    } else if total_files > 1 {
        let failed = all_results.iter().filter(|r| r.error.is_some()).count();
        eprintln!(
            "\nbench result: {}. {} ran across {} files.",
            if failed == 0 { "ok" } else { "FAILED" },
            all_results.len(),
            total_files
        );
    }

    if all_results.iter().any(|r| r.error.is_some()) {
        return Err("benches failed".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Filter pipeline
// ---------------------------------------------------------------------------

fn apply_filters(benches: &mut Vec<BenchInfo>, config: &BenchConfig) {
    if let Some(ref pat) = config.filter {
        if config.exact {
            benches.retain(|b| b.name == *pat);
        } else {
            let pat_lower = pat.to_lowercase();
            benches.retain(|b| b.name.to_lowercase().contains(&pat_lower));
        }
    }

    if let Some(ref tag) = config.tag {
        benches.retain(|b| b.tag.as_deref() == Some(tag.as_str()));
    }

    if let Some(ref skip_pat) = config.skip {
        let skip_lower = skip_pat.to_lowercase();
        benches.retain(|b| !b.name.to_lowercase().contains(&skip_lower));
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn discover_benches(path: &Path) -> Result<Vec<BenchInfo>, String> {
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
    // Bench files share the same setup/teardown decorators as test files.
    let mut setup_fn: Option<String> = None;
    let mut teardown_fn: Option<String> = None;

    for program in &loaded.programs {
        for stmt in &program.stmts {
            match stmt {
                Stmt::Func {
                    name, decorators, ..
                } => {
                    for dec in decorators {
                        if zz_frontend::test_attr::is_setup_decorator(dec) {
                            setup_fn = Some(name.join("."));
                        } else if zz_frontend::test_attr::is_teardown_decorator(dec) {
                            teardown_fn = Some(name.join("."));
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
                                if zz_frontend::test_attr::is_setup_decorator(dec) {
                                    setup_fn = Some(method_name.join("."));
                                } else if zz_frontend::test_attr::is_teardown_decorator(dec) {
                                    teardown_fn = Some(method_name.join("."));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Second pass: discover @bench functions, attaching setup/teardown.
    let mut benches = Vec::new();

    for (idx, program) in loaded.programs.iter().enumerate() {
        for stmt in &program.stmts {
            for mut bench in discover_in_stmt(stmt, path, idx) {
                bench.setup_fn = setup_fn.clone();
                bench.teardown_fn = teardown_fn.clone();
                benches.push(bench);
            }
        }
    }

    Ok(benches)
}

fn discover_in_stmt(stmt: &Stmt, file: &Path, module_index: usize) -> Vec<BenchInfo> {
    match stmt {
        Stmt::Func {
            name, decorators, ..
        } => {
            if decorators.is_empty() {
                return Vec::new();
            }
            let bench_dec = match decorators
                .iter()
                .find(|d| bench_attr::is_bench_decorator(d))
            {
                Some(d) => d,
                None => return Vec::new(),
            };
            let meta = bench_attr::parse_bench_meta(bench_dec, &mut Vec::new())
                .unwrap_or_else(|| BenchMeta::empty(bench_dec.span));

            let display_name = if name.len() > 1 {
                name.join(".")
            } else {
                name[0].clone()
            };

            vec![BenchInfo {
                name: display_name,
                file: file.to_path_buf(),
                meta: meta.clone(),
                module_index,
                func_name: name.clone(),
                setup_fn: None,
                teardown_fn: None,
                tag: meta.tag.clone(),
            }]
        }
        Stmt::Impl {
            name: impl_name,
            methods,
            ..
        } => {
            let mut benches = Vec::new();
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
                    let bench_dec = match decorators
                        .iter()
                        .find(|d| bench_attr::is_bench_decorator(d))
                    {
                        Some(d) => d,
                        None => continue,
                    };
                    let meta = bench_attr::parse_bench_meta(bench_dec, &mut Vec::new())
                        .unwrap_or_else(|| BenchMeta::empty(bench_dec.span));

                    let mut full_name = impl_name.clone();
                    full_name.extend(method_name.iter().cloned());

                    benches.push(BenchInfo {
                        name: full_name.join("."),
                        file: file.to_path_buf(),
                        meta: meta.clone(),
                        module_index,
                        func_name: full_name,
                        setup_fn: None,
                        teardown_fn: None,
                        tag: meta.tag.clone(),
                    });
                }
            }
            benches
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// VM execution: one interpreter setup, warmup + timed iterations
// ---------------------------------------------------------------------------

/// Build a ready interpreter for one bench file, mirroring
/// `test_runner::run_test_isolated` setup (load → HIR → stdlib init →
/// execute modules up to the bench's own). Callers then invoke bench
/// functions repeatedly via [`Interp::call`].
fn setup_bench_interp(bench: &BenchInfo) -> Result<(Interp, Value), String> {
    use std::sync::Arc;

    let loaded = loader::load_program(&bench.file)?;

    // Native plugins: same as `zz run` / `zz test`.
    let mut natives = loaded.natives.clone();
    {
        let project_root = loader::find_project_root(&bench.file).unwrap_or_else(|| {
            bench
                .file
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        });
        let plugin_funcs = crate::build::discover_plugin_manifests(&bench.file);
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
        loaded.funcs,
        loaded.structs,
        loaded.aliases,
        loaded.enums,
    );
    let types = Arc::new(typed.program.types);
    let structs = typed.program.structs;
    let enums = typed.program.enums;

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
        if let Err(e) = interp.run_typed(program, types.clone(), structs.clone(), enums.clone()) {
            return Err(format!("module error: {e:?}"));
        }
        if i == bench.module_index {
            break;
        }
    }

    let func_name = bench.func_name.join(".");
    let func_val = interp
        .env
        .get(&func_name)
        .ok_or_else(|| format!("bench function `{func_name}` not found in environment"))?;

    Ok((interp, func_val))
}

fn call_named(interp: &mut Interp, name: &str, span: Span) -> Result<(), String> {
    let func_val = interp
        .env
        .get(name)
        .ok_or_else(|| format!("`@setup`/`@teardown` function `{name}` not found"))?;
    match interp.call(func_val, vec![], span) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{e:?}")),
    }
}

fn run_single_bench_vm(bench: &BenchInfo, config: &BenchConfig) -> BenchResult {
    let iters = bench.meta.iters.unwrap_or(config.iters).max(1);
    let warmup = bench.meta.warmup.unwrap_or(config.warmup);
    let fail = |msg: String| BenchResult {
        name: bench.name.clone(),
        file: bench.file.clone(),
        iters,
        warmup,
        median_ns: 0,
        mean_ns: 0,
        min_ns: 0,
        max_ns: 0,
        error: Some(msg),
        over_budget: false,
    };

    // Bench stdout is captured and discarded: prints would pollute the
    // timing table and perturb measurements.
    zz_stdlib::natives::io::test_capture_start();
    let result = run_bench_vm_inner(bench, iters, warmup, config);
    let _ = zz_stdlib::natives::io::test_capture_take();
    result.unwrap_or_else(fail)
}

fn run_bench_vm_inner(
    bench: &BenchInfo,
    iters: u64,
    warmup: u64,
    config: &BenchConfig,
) -> Result<BenchResult, String> {
    let (mut interp, func_val) = setup_bench_interp(bench)?;

    if let Some(ref setup_name) = bench.setup_fn {
        call_named(&mut interp, setup_name, bench.meta.span)?;
    }

    for _ in 0..warmup {
        if let Err(e) = interp.call(func_val.clone(), vec![], bench.meta.span) {
            return Err(format!("warmup failed: {e:?}"));
        }
    }

    let start_all = Instant::now();
    let mut samples: Vec<u64> = Vec::with_capacity(iters.min(1_000_000) as usize);
    for i in 0..iters {
        let t0 = Instant::now();
        if let Err(e) = interp.call(func_val.clone(), vec![], bench.meta.span) {
            return Err(format!("iteration {i} failed: {e:?}"));
        }
        samples.push(t0.elapsed().as_nanos() as u64);
        // Budget check every 64 iterations: per-iter checks would perturb
        // fast benches, unbounded loops would hang the runner.
        if i % 64 == 63 && start_all.elapsed() > config.time_budget {
            if let Some(ref teardown_name) = bench.teardown_fn {
                let _ = call_named(&mut interp, teardown_name, bench.meta.span);
            }
            return Ok(BenchResult {
                name: bench.name.clone(),
                file: bench.file.clone(),
                iters: i + 1,
                warmup,
                median_ns: 0,
                mean_ns: 0,
                min_ns: 0,
                max_ns: 0,
                error: Some(format!(
                    "timeout: exceeded {} budget",
                    fmt_dur(config.time_budget)
                )),
                over_budget: true,
            });
        }
    }

    if let Some(ref teardown_name) = bench.teardown_fn {
        let _ = call_named(&mut interp, teardown_name, bench.meta.span);
    }

    let stats = stats_of(&mut samples);
    Ok(BenchResult {
        name: bench.name.clone(),
        file: bench.file.clone(),
        iters,
        warmup,
        median_ns: stats.median_ns,
        mean_ns: stats.mean_ns,
        min_ns: stats.min_ns,
        max_ns: stats.max_ns,
        error: None,
        over_budget: false,
    })
}

// ---------------------------------------------------------------------------
// AOT execution: per-file dispatch binary, one process per bench
// ---------------------------------------------------------------------------

fn build_bench_harness(
    file: &Path,
    benches: &[BenchInfo],
    config: &BenchConfig,
) -> Result<(PathBuf, PathBuf), String> {
    let source = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read bench file `{}`: {e}", file.display()))?;
    // A bench file with its own `func main` would collide with the harness
    // entrypoint; park it aside (it never runs under `zz bench`).
    let source = source
        .replace("func main(", "func __zz_user_main(")
        .replace("func main (", "func __zz_user_main (")
        .replace("pub func main(", "pub func __zz_user_main(")
        .replace("pub func main (", "pub func __zz_user_main (");
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

    // The harness times itself with `time.monotonic_nanos` (same clock
    // family on both engines) and prints one machine-readable line.
    // `import std.time` is injected: duplicate imports are accepted, so
    // files that already import it keep working.
    let mut arms = String::new();
    for (idx, bench) in benches.iter().enumerate() {
        let call = bare_name(&bench.func_name.join("."), &prefix);
        let setup = bench
            .setup_fn
            .as_deref()
            .map(|s| format!("        {}()\n", bare_name(s, &prefix)))
            .unwrap_or_default();
        let teardown = bench
            .teardown_fn
            .as_deref()
            .map(|s| format!("        {}()\n", bare_name(s, &prefix)))
            .unwrap_or_default();
        // Separate `if`s (never `else if`): ZZ unifies `if/else` arm types,
        // and bench functions may return anything.
        arms.push_str(&format!(
            "    if which == \"b{idx}\" {{\n{setup}        i := 0\n        while i < warmup {{\n            {call}()\n            i = i + 1\n        }}\n        start := time.monotonic_nanos()\n        i = 0\n        while i < iters {{\n            {call}()\n            i = i + 1\n        }}\n        total := time.monotonic_nanos() - start\n{teardown}        println(\"BENCH {{total}} {{iters}}\")\n        return\n    }}\n"
        ));
    }
    arms.push_str("    fail(\"unknown bench: \" + which)\n");

    let harness_src = format!(
        "import std.time\n{source}\n// __zz_bench_harness__: generated by `zz bench --native`, do not edit.\nfunc main(args: [str]) {{\n    if len(args) < 3 {{\n        fail(\"zz bench harness: want <id> <iters> <warmup>\")\n    }}\n    which := args[0]\n    iters := 0\n    match int(args[1]) {{\n        .some(v) => iters = v\n        .none => fail(\"zz bench harness: bad iters\")\n    }}\n    warmup := 0\n    match int(args[2]) {{\n        .some(v) => warmup = v\n        .none => fail(\"zz bench harness: bad warmup\")\n    }}\n{arms}}}\n"
    );

    let harness_name = format!("{AOT_HARNESS_PREFIX}{orig_ns}_{}.zz", std::process::id());
    let harness_path = file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&harness_name);
    std::fs::write(&harness_path, &harness_src)
        .map_err(|e| format!("cannot write AOT harness `{}`: {e}", harness_path.display()))?;

    let mode = if config.native_release {
        crate::build::BuildMode::Release
    } else {
        crate::build::BuildMode::Dev
    };
    let rel = crate::build::ReleaseOptions {
        allow_source_builds: config.allow_source_builds,
        allow_hooks: config.allow_hooks,
        ..Default::default()
    };
    let bin = match crate::build::build_release(&harness_path, mode, &rel) {
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

fn run_single_bench_aot(
    bench: &BenchInfo,
    arm: &str,
    bin: &Path,
    config: &BenchConfig,
) -> BenchResult {
    let iters = bench.meta.iters.unwrap_or(config.iters).max(1);
    let warmup = bench.meta.warmup.unwrap_or(config.warmup);
    let fail = |msg: String| BenchResult {
        name: bench.name.clone(),
        file: bench.file.clone(),
        iters,
        warmup,
        median_ns: 0,
        mean_ns: 0,
        min_ns: 0,
        max_ns: 0,
        error: Some(msg),
        over_budget: false,
    };

    let start = Instant::now();
    let mut child = match std::process::Command::new(bin)
        .arg(arm)
        .arg(iters.to_string())
        .arg(warmup.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot run AOT bench binary: {e}")),
    };

    // Drain pipes on threads: a chatty child must never block on a full pipe.
    let child_stdout = child.stdout.take();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut o) = child_stdout {
            use std::io::Read;
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let child_stderr = child.stderr.take();
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut e) = child_stderr {
            use std::io::Read;
            let _ = e.read_to_end(&mut buf);
        }
        buf
    });

    // Poll for exit; the `--timeout` budget kills the child on overrun.
    let deadline = start + config.time_budget;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                // Status query failed; block for the real exit instead.
                match child.wait() {
                    Ok(status) => break Some(status),
                    Err(e) => {
                        return fail(format!("AOT bench child status unknown: {e}"));
                    }
                }
            }
        }
    };

    let stdout = out.join().unwrap_or_default();
    let _ = err_handle.join();
    let stdout = String::from_utf8_lossy(&stdout);

    let Some(status) = outcome else {
        return BenchResult {
            name: bench.name.clone(),
            file: bench.file.clone(),
            iters,
            warmup,
            median_ns: 0,
            mean_ns: 0,
            min_ns: 0,
            max_ns: 0,
            error: Some(format!(
                "timeout: exceeded {} budget",
                fmt_dur(config.time_budget)
            )),
            over_budget: true,
        };
    };
    if !status.success() {
        let tail: Vec<&str> = stdout.lines().rev().take(5).collect();
        let detail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        return fail(format!(
            "AOT bench exited with code {}: {detail}",
            status.code().unwrap_or(-1)
        ));
    }
    // The child prints exactly one `BENCH <total_ns> <iters>` line with
    // per-iteration means; the parent divides back out. (AOT reports the
    // mean as median/min/max too: one sample cannot produce a spread.
    // Documented in bench.md.)
    for line in stdout.lines().rev() {
        if let Some(rest) = line.strip_prefix("BENCH ") {
            let mut parts = rest.split_whitespace();
            if let (Some(total), Some(n)) = (parts.next(), parts.next()) {
                if let (Ok(total_ns), Ok(ran)) = (total.parse::<u64>(), n.parse::<u64>()) {
                    if let Some(per) = total_ns.checked_div(ran) {
                        return BenchResult {
                            name: bench.name.clone(),
                            file: bench.file.clone(),
                            iters: ran,
                            warmup,
                            median_ns: per,
                            mean_ns: per,
                            min_ns: per,
                            max_ns: per,
                            error: None,
                            over_budget: false,
                        };
                    }
                }
            }
        }
    }
    fail("AOT bench produced no BENCH line".to_string())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Adaptive duration: `12ns`, `3.4µs`, `2.1ms`, `1.20s`.
fn fmt_nanos(ns: u64) -> String {
    if ns < 1_000 {
        format!("{ns}ns")
    } else if ns < 1_000_000 {
        format!("{:.1}µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.1}ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2}s", ns as f64 / 1_000_000_000.0)
    }
}

fn fmt_dur(d: Duration) -> String {
    if d.as_secs() > 0 {
        format!("{:.2}s", d.as_secs_f64())
    } else {
        format!("{}ms", d.as_millis())
    }
}

fn print_bench_line(r: &BenchResult, use_color: bool) {
    let name = &r.name;
    if let Some(ref e) = r.error {
        if use_color {
            eprintln!(
                "\x1b[1;31mbench {name} ... FAILED\x1b[0m ({} iters)",
                r.iters
            );
        } else {
            eprintln!("bench {name} ... FAILED ({} iters)", r.iters);
        }
        for line in e.lines() {
            eprintln!("  {line}");
        }
        return;
    }
    let line = format!(
        "bench {name} ... {}/iter ({} iters, mean {}, min {}, max {})",
        fmt_nanos(r.median_ns),
        r.iters,
        fmt_nanos(r.mean_ns),
        fmt_nanos(r.min_ns),
        fmt_nanos(r.max_ns),
    );
    if use_color {
        eprintln!("\x1b[32m{line}\x1b[0m");
    } else {
        eprintln!("{line}");
    }
}

fn print_bench_list_grouped(groups: &[(PathBuf, Vec<BenchInfo>)], use_color: bool) {
    for (file, benches) in groups {
        if use_color {
            eprintln!("\x1b[1m{}:\x1b[0m", file.display());
        } else {
            eprintln!("{}:", file.display());
        }
        for b in benches {
            eprintln!("  {}", b.name);
        }
    }
}

fn print_json_results(results: &[BenchResult]) -> Result<(), String> {
    let mut out = String::from("[\n");
    for (i, r) in results.iter().enumerate() {
        let err = match &r.error {
            Some(e) => format!("\"{}\"", json_escape(e)),
            None => "null".to_string(),
        };
        out.push_str(&format!(
            "  {{\"name\": \"{}\", \"file\": \"{}\", \"iters\": {}, \"warmup\": {}, \
             \"median_ns\": {}, \"mean_ns\": {}, \"min_ns\": {}, \"max_ns\": {}, \
             \"over_budget\": {}, \"error\": {}}}{}\n",
            json_escape(&r.name),
            json_escape(&r.file.to_string_lossy()),
            r.iters,
            r.warmup,
            r.median_ns,
            r.mean_ns,
            r.min_ns,
            r.max_ns,
            r.over_budget,
            err,
            if i + 1 < results.len() { "," } else { "" },
        ));
    }
    out.push_str("]\n");
    print!("{out}");
    std::io::stdout()
        .flush()
        .map_err(|e| format!("cannot write JSON: {e}"))?;
    Ok(())
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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
                if name.starts_with('.') || name == "target" || name == "vendor" || name == "build"
                {
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
