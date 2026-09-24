//! ZZ CLI entry point.
//!
//! Phase 1:
//! - `zz`            → interactive REPL
//! - `zz eval <src>` → evaluate source once and print the result
//! - `zz run <file>` → type-check and run a `.zz` file
//! - `zz check <file>` → parse and type-check a `.zz` file without running it
//! - `zz --help`     → usage

use std::process::ExitCode;

mod build;
mod loader;
mod pm;
mod repl;
mod session;
mod test_runner;

use zz_frontend::diag::{error_at, render_to_string, Files};
use zz_frontend::span::Span;
use zz_runtime::{Interp, Value};

use session::Session;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
zz — the ZZ programming language

USAGE:
    zz                            start the interactive REPL
    zz eval <source>              evaluate source and print the result
    zz run <file.zz>              type-check and run a file
    zz test <file.zz | dir>       run @test-annotated functions
    zz check [FLAGS] [PATH]       scan for errors/warnings (file or directory)
    zz fix [FLAGS] [PATH]         apply auto-fixes (shortcut for check --fix)
    zz fmt [FLAGS] [PATH]         format ZZ source files in-place
    zz build [FLAGS] <file.zz>    compile a native binary (cached)

PACKAGE MANAGER:
    zz init [--template T]        initialize zz.toml + src/main.zz in cwd
    zz new <name> [--template T]  create a new project directory
    zz add <pkg>[@ver]            add a dependency to zz.toml
    zz install, zz i              resolve deps, fetch into CAS, link
    zz remove <pkg>               remove a dependency
    zz update [pkg]               re-resolve floating versions
    zz search <query>             search the package registry
    zz info <pkg>                 show package metadata and versions
    zz login [--browser]          authenticate for publishing
    zz publish [--dry-run]        validate, pack, and upload to the registry
    zz cache gc                   garbage-collect unused CAS entries
    zz cache clean                clear build cache

BUILD MODES (single Clang backend, always a native binary):
     zz build <file.zz>           debug build (-O0 -g, fast, dynamic) — the default
     zz build -p <file.zz>        release build (-O3 -flto=thin, dynamic, stripped)
     zz build --static <file.zz>  static build (ThinLTO, DCE, self-contained; not on macOS)
     zz build --pgo <file.zz>     PGO build (profile-guided, native host only)
     zz build --target <triple> <file.zz>
                                  cross build via clang --target= (drops -march=native)

FLAGS:
    --check, -c        with fmt, check formatting without writing (exit 1 if changed)
    --stdin            with fmt, read source from stdin and write formatted to stdout
    --fix, -f          apply safe auto-fixes (typo replacements, field corrections)
    --hard             with --fix, apply ALL fixes including ambiguous ones (no prompts)
    --interactive, -i  with --fix, prompt for ambiguous fixes interactively
    --native           with run, use the native AOT compiler instead of the VM
    --embed <dir>      with run/build, serve (VM) or bake (native) a static asset
                       directory, readable at runtime via `fs.embedfs()`
    -p, --release      with build, full optimization (-O3 -flto=thin, dynamic, stripped)
    --static           with build, static self-contained binary (ThinLTO, DCE; rejected on macOS)
    --pgo              with build, profile-guided optimization build (native host only)
    --target <triple>  with build, cross-compile via clang --target= (same flags as without -p, minus -march=native)
    --cc <clang|zig>   with build, select the Clang provider
    --verbose          with build, print the exact clang command line
    --template <T>     with init/new, template: cli (default), lib, or web
    --git <url>        with add, git URL for dependency
    --rev <rev>        with add, git revision (branch, tag, or commit)
    --path <path>      with add, local path dependency
    --registry <url>   with add/install/search/info/login/publish/update,
                       registry base URL (default: ZZ_REGISTRY or the public registry)
    --limit <n>        with search, max results (default 20)
    --dry-run          with publish, validate + pack without uploading
    --skip-tests       with publish, skip the `zz test` gate (native pkgs)
    --browser          with login, print the OAuth URL before prompting
    --author <name>    with init/new, package author (repeatable, comma-split)
    --description <t>  with init/new, package description
    --license <spdx>   with init/new, package license (e.g. MIT)
    --repo <url>       with init/new, package repository URL
    --help, -h         show this help
    --version, -V      show version

FIX SAFETY:
    Safe fixes (single unambiguous match) are applied automatically with --fix.
    Ambiguous fixes (multiple candidates) require --hard or --interactive (-i).

PATH can be a single .zz file or a directory (recursively scans all .zz files).
Defaults to `.` (current directory) if omitted.

EXAMPLES:
    zz init                           initialize project in current directory
    zz new myapp                      create new project 'myapp'
    zz new mylib --template lib       create new library project
    zz add foo@^1.2.0                 add semver-range dependency
    zz add bar --git URL --rev main   add git dependency
    zz add baz --path ../baz          add path dependency
    zz add qux                        add via local registry alias (~/.zz/registry.toml)
    zz registry add qux --path ../qux  register a local alias (no server; share the file via dotfiles)
    zz registry list                  list local aliases
    zz install                        resolve and fetch all dependencies
    zz remove foo                     remove a dependency
    zz check .                       scan current directory
    zz check src/ --fix             fix all safe issues in src/
    zz fix hello.zz                  fix a single file
    zz check --fix --hard src/       force-apply all fixes, no prompts
    zz check --fix -i src/           interactive mode for ambiguous fixes
    zz fmt .                         format all .zz files in current directory
    zz fmt -c src/                   check formatting without writing
    zz fmt --stdin < file.zz         format a single file via stdin/stdout
    zz build hello.zz                dev build (dynamic)
    zz build -p hello.zz             release build (dynamic, optimized)
    zz build --static hello.zz       static build (self-contained)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Separate the subcommand from flags and path.
    let cmd = args.first().map(String::as_str);
    let rest = args.get(1..).unwrap_or(&[]);

    match cmd {
        None => match repl::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("zz: repl error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("eval") => {
            let src = rest.join(" ");
            let mut session = Session::new("<eval>");
            let output = session.eval_to_console(&src);
            if !output.is_empty() {
                println!("{output}");
            }
            if session.last_eval_had_errors() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Some("run") => {
            let native = rest.iter().any(|a| a == "--native");
            let embed = parse_flag_value(rest, "--embed").map(std::path::PathBuf::from);
            // Strip `--embed <dir>` / `--embed=<dir>` (and `--native`) so
            // neither the loader nor the script sees them as paths/args.
            let args: Vec<String> = strip_flag_value(rest, "--embed", "--native");
            let script_args = args.get(1..).unwrap_or(&[]).to_vec();
            let file = args.iter().find(|a| !a.starts_with('-'));
            if native {
                match run_native(file, &script_args, embed) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("zz: {msg}");
                        ExitCode::FAILURE
                    }
                }
            } else {
                match run_file(file, &script_args, embed) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("zz: {msg}");
                        ExitCode::FAILURE
                    }
                }
            }
        }
        Some("build") => match build_cmd(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("check") => {
            let (path, flags) = parse_path_and_flags(rest);
            let has_fix = flags.contains(&"--fix".to_string());
            let has_hard = flags.contains(&"--hard".to_string());
            let has_interactive = flags.contains(&"--interactive".to_string());

            let interactive = has_fix && has_interactive && !has_hard;
            match check_or_fix_path(&path, has_fix, interactive, has_hard) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("zz: {msg}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("fix") => {
            let (path, flags) = parse_path_and_flags(rest);
            let has_hard = flags.contains(&"--hard".to_string());
            let has_interactive = flags.contains(&"--interactive".to_string());
            let interactive = has_interactive && !has_hard;
            match check_or_fix_path(&path, true, interactive, has_hard) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("zz: {msg}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("fmt") => {
            let (path, flags) = parse_path_and_flags(rest);
            let check_only =
                flags.contains(&"--check".to_string()) || flags.contains(&"-c".to_string());
            let stdin = flags.contains(&"--stdin".to_string());
            match fmt_command(path, check_only, stdin) {
                Ok(changed) => {
                    if check_only && changed {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                Err(msg) => {
                    eprintln!("zz: {msg}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("test") => match test_runner::test_command(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        // Package manager commands
        Some("init") => match pm::init(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("new") => match pm::new(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("add") => match pm::add(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("install") | Some("i") => match pm::install(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("remove") | Some("uninstall") => match pm::remove(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("update") => match pm::update(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("search") => match pm::search(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("info") => match pm::info(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("registry") => match pm::registry(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("login") => match pm::login(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("publish") => match pm::publish(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("cache") => match pm::cache(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("--version") | Some("-V") => {
            println!("zz {VERSION}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("zz: unknown command `{other}`\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Split args into flags (--flag items) and path (last non-flag arg).
/// Flags must precede the path: `zz check --fix src/`.
/// Short aliases are normalized: `-i` → `--interactive`, `-f` → `--fix`, `-c` → `--check`.
fn parse_path_and_flags(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut path = None;
    let mut flags = Vec::new();
    for a in args {
        if a == "-i" {
            flags.push("--interactive".to_string());
        } else if a == "-f" {
            flags.push("--fix".to_string());
        } else if a == "-c" {
            flags.push("--check".to_string());
        } else if a.starts_with("--") || a.starts_with('-') {
            flags.push(a.clone());
        } else {
            // Last non-flag wins as path.
            path = Some(a.clone());
        }
    }
    (path, flags)
}

/// Load plugin shared libraries for VM-based native dispatch.
///
/// Reads `zz.lock` and `zz.toml` from the project directory, finds dependencies
/// with `plugin.zzi` and shared libraries in their `build/` directory, loads
/// them via dlopen, and registers their native functions.
///
/// `plugin_funcs` carries the manifest signatures keyed by ZZ-visible name;
/// after loading, C-symbol registrations are aliased to those ZZ names so
/// VM dispatch and AOT lowering resolve identically.
#[cfg(unix)]
fn load_vm_plugins(
    project_dir: &std::path::Path,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
    plugin_funcs: &[(String, zz_checker::FuncSig)],
) -> Result<(), String> {
    use zz_pm::lock::Lockfile;

    let lock_path = project_dir.join("zz.lock");
    let lock = match Lockfile::load(&lock_path) {
        Ok(l) => l,
        Err(_) => return Ok(()), // no lock file, no plugins
    };

    // Load manifest to resolve path deps
    let manifest_path = project_dir.join("zz.toml");
    let manifest = zz_pm::manifest::Manifest::load(&manifest_path).ok();

    for dep in &lock.deps {
        // Resolve package directory: path deps use local path, git deps use CAS
        let pkg_dir = if dep.source == "path" {
            if let Some(ref m) = manifest {
                if let Some(zz_pm::manifest::DepSpec::Path(ref path_dep)) =
                    m.dependencies.get(&dep.name)
                {
                    project_dir.join(&path_dep.path)
                } else {
                    continue;
                }
            } else {
                continue;
            }
        } else {
            zz_pm::paths::cas_entry(&dep.hash)
        };

        // Only load plugins that have a plugin.zzi manifest
        if !pkg_dir.join("plugin.zzi").exists() {
            continue;
        }

        // Look for shared library in build/ directory
        let build_dir = pkg_dir.join("build");
        if !build_dir.exists() {
            continue;
        }

        // Try common shared library names, then any .so/.dylib the
        // build hook actually produced (e.g. libzimg_native.so — never
        // assume lib<name>.so).
        let mut lib_paths: Vec<std::path::PathBuf> = Vec::new();
        let lib_names = [
            format!("lib{}.so", dep.name.replace('-', "_")),
            format!("lib{}.dylib", dep.name.replace('-', "_")),
            format!("{}.so", dep.name.replace('-', "_")),
            format!("{}.dylib", dep.name.replace('-', "_")),
        ];

        for lib_name in &lib_names {
            let lib_path = build_dir.join(lib_name);
            if lib_path.exists() && !lib_paths.contains(&lib_path) {
                lib_paths.push(lib_path);
            }
        }
        if let Ok(entries) = std::fs::read_dir(&build_dir) {
            let mut scanned: Vec<std::path::PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    matches!(p.extension().and_then(|e| e.to_str()), Some("so" | "dylib"))
                        && !lib_paths.contains(p)
                })
                .collect();
            scanned.sort();
            lib_paths.extend(scanned);
        }

        for lib_path in &lib_paths {
            match zz_plugin::load_plugin(lib_path, natives) {
                Ok(handle) => {
                    // The handle MUST stay alive: dropping it unloads the
                    // library, unmapping the registered function pointers.
                    keep_plugin_alive(handle);
                    eprintln!("zz: loaded plugin `{}`", dep.name);
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

    Ok(())
}

/// Loaded plugin libraries, kept alive for the process lifetime.
/// Dropping a `PluginLib` unloads its `.so`, unmapping every registered
/// function pointer — so handles are never released once loaded.
static PLUGIN_LIBS: std::sync::OnceLock<std::sync::Mutex<Vec<zz_plugin::PluginLib>>> =
    std::sync::OnceLock::new();

/// Retain a loaded plugin library for the rest of the process.
fn keep_plugin_alive(handle: zz_plugin::PluginLib) {
    PLUGIN_LIBS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .expect("plugin registry lock")
        .push(handle);
}

/// Non-Unix stub for VM plugin loading.
#[cfg(not(unix))]
fn load_vm_plugins(
    _project_dir: &std::path::Path,
    _natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
    _plugin_funcs: &[(String, zz_checker::FuncSig)],
) -> Result<(), String> {
    // dlopen not supported on this platform yet
    Ok(())
}

fn run_file(
    path: Option<&String>,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
) -> Result<(), String> {
    let path = path.ok_or_else(|| {
        "missing file argument\n\n\
             usage: zz run <file.zz>\n\
             hint: provide the path to a .zz file to execute"
            .to_string()
    })?;

    let script_path = std::path::Path::new(path);
    // Project root: walk up from the script (entry files usually live in
    // `src/`; `zz.lock` sits at the root). Falls back to the script dir.
    let project_root = loader::find_project_root(script_path).unwrap_or_else(|| {
        script_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf()
    });
    // Discover plugin manifest signatures so `zz run` type-checks the
    // same dotted names the AOT path merges.
    let plugin_funcs = crate::build::discover_plugin_manifests(script_path);

    let loaded = if plugin_funcs.is_empty() {
        loader::load_program(script_path)?
    } else {
        loader::load_program_with_plugins(script_path, &plugin_funcs)?
    };
    let mut has_errors = false;
    for e in &loaded.errors {
        let mut files = Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!("{}", render_to_string(&files, id, &e.diags));
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            has_errors = true;
        }
    }
    if has_errors {
        return Err("program failed\n\n\
                   hint: fix the errors shown above and try again"
            .to_string());
    }

    // Load plugin shared libraries for VM-based native dispatch.
    let mut natives = loaded.natives.clone();
    if let Err(e) = crate::load_vm_plugins(&project_root, &mut natives, &plugin_funcs) {
        eprintln!("zz: warning: {e}");
    }

    // Build the typed program (HIR) to get the resolved type map.
    // The merged program is only used for type checking; execution still
    // runs each module's original program so top-level side effects
    // (imports, struct registrations) happen in dependency order.
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
    let types = std::sync::Arc::new(typed.program.types);
    let structs = typed.program.structs;

    let mut interp = Interp::with_natives(natives);
    interp.args = script_args.to_vec();
    // Selective-import aliases for bare generic-function calls (see
    // LoadResult::import_aliases): the VM compiler emits no code for
    // import statements, so the runtime map would otherwise stay empty.
    interp.import_aliases = loaded.import_aliases.clone();

    // `--embed <dir>`: serve the asset tree to `fs.embedfs()` for this run.
    if let Some(dir) = embed.as_deref() {
        let files = build::collect_embed(dir)?;
        zz_stdlib::natives::fs::vfs::set_embed(
            files
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>(),
        );
    }

    // Inject math constants as static float values in the runtime env.
    // This avoids the zero-arg native function indirection — `PI` resolves
    // directly to `Value::Float(3.14159…)` without a function call.
    // Three forms are injected so all reference styles work:
    //   - `std.math.PI`  — fully qualified
    //   - `math.PI`      — module namespace (import std.math)
    //   - `PI`           — bare (import std.math(PI))
    // The checker gates which names are actually accessible per-module,
    // so injecting all bare forms here is safe.
    for (key, val) in zz_stdlib::stdlib_consts() {
        interp.env.define(&key, Value::Float(val));
        if let Some(rest) = key.strip_prefix("std.") {
            interp.env.define(rest, Value::Float(val));
        }
        // Bare name: `std.math.PI` → `PI`
        if let Some(bare) = key.rsplit('.').next() {
            interp.env.define(bare, Value::Float(val));
        }
    }
    // Also inject any aliased constants from selective imports
    // (e.g. `import std.math(PI as pi)` → inject `pi`).
    for (name, val) in &loaded.consts {
        interp.env.define(name, Value::Float(*val));
    }

    // Run compiled pure-ZZ stdlib programs. These populate the environment
    // with functions written in ZZ (e.g. vec.map, math.sum) that extend
    // the native stdlib. Must happen before user code so the functions are
    // available when user modules reference them.
    for zz_prog in zz_stdlib::zz_stdlib_programs() {
        if let Err(e) = interp.run_typed(
            &zz_prog.program,
            std::sync::Arc::new(zz_prog.types.clone()),
            zz_prog.structs.clone(),
        ) {
            eprintln!("zz: pure-ZZ stdlib error: {e:?}");
            return Err("stdlib initialization failed".to_string());
        }
    }
    // Canonical `std.*` aliases for pure-ZZ helpers: sources declare short
    // names (`json.is_null`) while the checker advertises both spellings.
    zz_stdlib::define_canonical_purezz_aliases(&mut interp.env, &mut interp.funcs);
    // Mirror pure-ZZ Env bindings for `import std.X as alias` renames
    // (e.g. `colors.red` → `cl.red`). Natives are already aliased via
    // `loaded.natives`; pure-ZZ funcs live in Env and need the same.
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

    let mut last = Value::Unit;
    for (i, program) in loaded.programs.iter().enumerate() {
        match interp.run_typed(program, types.clone(), structs.clone()) {
            Ok(v) => last = v,
            Err(e) => {
                let (name, source) = loaded
                    .files
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| (path.clone(), String::new()));
                let mut files = Files::new();
                let id = files.add(name, source);
                let mut diag = error_at(e.message.clone(), e.span);
                for (name, _span) in &e.backtrace {
                    if !name.is_empty() {
                        diag = diag.with_note(format!("  at {name}"));
                    }
                }
                for note in &e.notes {
                    diag = diag.with_note(note.clone());
                }
                let diags = vec![diag];
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err("program failed".to_string());
            }
        }
    }
    if last != Value::Unit {
        println!("{last}");
    }

    // Auto-call `main()` if defined in the entry file.
    // The entry file's namespace is its file stem (e.g. `myapp.zz` → `myapp`).
    let entry_ns = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let main_key = format!("{entry_ns}.main");
    if let Some(fv) = interp.funcs.get(&main_key).cloned() {
        let span = Span::new(0, 0);
        // Pass script args only if main() accepts parameters.
        let call_args = if fv.params.is_empty() {
            vec![]
        } else {
            vec![Value::Array(Box::new(
                script_args
                    .iter()
                    .map(|a| Value::Str(a.clone().into()))
                    .collect(),
            ))]
        };
        match interp.call(Value::Func(Box::new(fv)), call_args, span) {
            Ok(v) => {
                // `func main() -> Result<(), E>`: `Err(e)` prints to stderr
                // and fails the run (exit 1). `Ok`/`Unit` are success.
                if let Value::Result(r) = v {
                    if let Err(e) = &*r {
                        eprintln!("{e}");
                        return Err("program failed".to_string());
                    }
                }
            }
            Err(e) => {
                // Render against the entry file's real source (entry is
                // last in load order). An empty source would panic the
                // renderer on any non-empty error span.
                let (name, source) = loaded
                    .files
                    .last()
                    .cloned()
                    .unwrap_or_else(|| (path.clone(), String::new()));
                let mut files = Files::new();
                let id = files.add(name, source);
                let mut diag = error_at(e.message.clone(), e.span);
                for (name, _) in &e.backtrace {
                    if !name.is_empty() {
                        diag = diag.with_note(format!("  at {name}"));
                    }
                }
                for note in &e.notes {
                    diag = diag.with_note(note.clone());
                }
                let diags = vec![diag];
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err("program failed".to_string());
            }
        }
    }

    Ok(())
}

/// `zz run --native <file>`: compile to a temp location, execute, cleanup.
fn run_native(
    path: Option<&String>,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
) -> Result<(), String> {
    let path = path.ok_or_else(|| {
        "missing file argument\n\n\
             usage: zz run --native <file.zz>\n\
             hint: provide the path to a .zz file to compile and execute"
            .to_string()
    })?;
    let p = std::path::Path::new(path);
    // Use release mode for native runs to get -O3 optimization (true native speed).
    let rel = build::ReleaseOptions {
        embed,
        ..Default::default()
    };
    let cached = build::build_release(p, build::BuildMode::Release, &rel)?;
    let code = build::exec_binary(&cached, script_args)?;
    if code != 0 {
        return Err(format!(
            "native program exited with code {code}\n\
             hint: the program may have panicked or returned a non-zero exit code"
        ));
    }
    Ok(())
}

/// `zz build [FLAGS] <file>`: always a native Clang binary.
///
/// Default (`zz build`): fast native debug build (`-O0 -g`, no LTO).
/// `-p/--release/-O3` upgrades to the optimized build (`-O3 -flto=thin`).
/// Both paths are real binaries in `bin/` — never VM execution.
/// (`zz run` is the only command that executes through the VM.)
fn build_cmd(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--dev") {
        return Err(
            "`--dev` was removed: `zz build` is a debug build by default\n\
             hint: drop --dev (use -p/--release for the optimized build)"
                .to_string(),
        );
    }
    let release = args
        .iter()
        .any(|a| a == "-p" || a == "--release" || a == "-O3");
    let is_static = args.iter().any(|a| a == "--static");
    let is_pgo = args.iter().any(|a| a == "--pgo");
    let verbose = args.iter().any(|a| a == "--verbose");
    let target = parse_flag_value(args, "--target");
    let cc = parse_flag_value(args, "--cc");
    let embed = parse_flag_value(args, "--embed").map(std::path::PathBuf::from);
    // Positional path: first non-flag arg, skipping values consumed by
    // `--target <triple>` / `--cc <name>` / `--embed <dir>` (space form).
    let mut skip_next = false;
    let path = args
        .iter()
        .find(|a| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if a.as_str() == "--target" || a.as_str() == "--cc" || a.as_str() == "--embed" {
                skip_next = true;
                return false;
            }
            !a.starts_with('-')
        })
        .ok_or_else(|| {
            "missing file argument\n\n\
             usage: zz build [-p|--release|-O3|--static|--pgo] [--target <triple>] [--cc <clang|zig>] [--embed <dir>] <file.zz>\n\
             hint: provide the path to a .zz file to build"
                .to_string()
        })?;
    let p = std::path::Path::new(path);

    // Default (no flags) is a fast native debug build; -p upgrades to
    // optimized. --static/--pgo select their own option sets. Guards
    // (PGO-cross, static-macOS) in validate() apply uniformly.
    let mode = if is_pgo {
        build::BuildMode::Pgo
    } else if is_static {
        build::BuildMode::Static
    } else if release {
        build::BuildMode::Release
    } else {
        build::BuildMode::Dev
    };
    let provider = match cc.as_deref() {
        None => zz_codegen::ClangProvider::Any,
        Some(name) => zz_codegen::ClangProvider::parse(name).ok_or_else(|| {
            format!(
                "unknown --cc provider `{name}`\n\
                 hint: use --cc clang or --cc zig"
            )
        })?,
    };
    let rel = build::ReleaseOptions {
        target,
        provider,
        verbose,
        embed,
    };
    let dest = build::build_release(p, mode, &rel)?;
    let meta = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    let mode_str = match mode {
        build::BuildMode::Dev => "dev",
        build::BuildMode::Release => "release",
        build::BuildMode::Static => "static",
        build::BuildMode::Pgo => "pgo",
    };
    println!(
        "built {} ({}, {:.1} KB)",
        dest.display(),
        mode_str,
        meta as f64 / 1024.0
    );
    Ok(())
}

/// Value of a `--flag value` or `--flag=value` CLI flag.
fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
        if a == flag {
            if let Some(v) = iter.next() {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Strip value-flags (`--embed <dir>` / `--embed=<dir>`) plus any bare
/// flags in `bare` from an arg list (for `run`: the loader and the script
/// must never see CLI-only flags).
fn strip_flag_value(args: &[String], flag: &str, bare: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == flag {
            skip_next = true;
            continue;
        }
        if a.starts_with(&format!("{flag}=")) {
            continue;
        }
        if a == bare {
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Top-level entry for `zz fmt`.
///
/// Modes:
/// - `stdin=true`: read source from stdin, write formatted output to
///   stdout. Honors `--check` by exiting 1 if stdin needs formatting
///   without producing output.
/// - `stdin=false`: format every `.zz` file under `path_arg` (or
///   `.` if not given) in place, or print a unified diff for files
///   that need formatting under `--check`.
///
/// Uses `zz_fmt::discover` for gitignore-aware file discovery and
/// `zz_fmt::format_paths_parallel` for concurrent formatting via rayon.
///
/// Returns `Ok(true)` when at least one file (or stdin) needed
/// formatting — the caller uses this to choose the exit code.
fn fmt_command(path_arg: Option<String>, check_only: bool, stdin: bool) -> Result<bool, String> {
    use std::io::Read;

    if stdin {
        let mut source = String::new();
        std::io::stdin()
            .read_to_string(&mut source)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        let config = zz_fmt::FmtConfig::default();
        return match zz_fmt::format_source(&source, &config) {
            Ok(formatted) => {
                if formatted != source {
                    if check_only {
                        // Emit the diff so callers can see what would change.
                        let diff = zz_fmt::diff::unified_diff_plain("<stdin>", &source, &formatted)
                            .unwrap_or_default();
                        eprint!("{diff}");
                    } else {
                        print!("{formatted}");
                    }
                    Ok(true)
                } else {
                    if !check_only {
                        print!("{formatted}");
                    }
                    Ok(false)
                }
            }
            Err(e) => Err(format!("format error: {e}")),
        };
    }

    // Discover .zz files using gitignore-aware walker.
    let raw = path_arg.as_deref().unwrap_or(".");
    let base = std::path::PathBuf::from(raw);
    let files = zz_fmt::discover(&[base]).map_err(|e| format!("file discovery failed: {e}"))?;

    if files.is_empty() {
        return Err(format!("no .zz files found in `{raw}`"));
    }

    let config = zz_fmt::FmtConfig::default();

    // Read all files into memory, then format in parallel (pure, no disk I/O).
    // This avoids writing files when --check is active.
    let sources: Vec<(std::path::PathBuf, String)> = files
        .iter()
        .filter_map(|p| {
            std::fs::read_to_string(p)
                .ok()
                .map(|s| (p.clone(), s))
                .filter(|(_, s)| !s.is_empty())
        })
        .collect();

    let src_refs: Vec<(&std::path::PathBuf, &str)> =
        sources.iter().map(|(p, s)| (p, s.as_str())).collect();
    let results = zz_fmt::format_sources_parallel(&src_refs, &config);

    let mut changed_any = false;
    let mut errors: Vec<String> = Vec::new();

    for (i, result) in results.into_iter().enumerate() {
        let formatted = match result {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("{e}"));
                continue;
            }
        };

        let (path, original) = &sources[i];
        let path_str = path.display().to_string();

        if formatted == *original {
            continue;
        }
        changed_any = true;

        if check_only {
            eprintln!("--- {path_str} (would reformat) ---");
            let diff = zz_fmt::diff::unified_diff_plain(&path_str, original, &formatted)
                .unwrap_or_default();
            print!("{diff}");
        } else {
            if let Err(e) = std::fs::write(path, &formatted) {
                errors.push(format!("cannot write `{path_str}`: {e}"));
                continue;
            }
            eprintln!("reformatted: {path_str}");
        }
    }

    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }

    if check_only && changed_any {
        eprintln!(
            "\nzz: {} file(s) need formatting (run `zz fmt` to fix)",
            files.len()
        );
    } else if !changed_any && !check_only {
        eprintln!("zz: all files already formatted");
    }

    Ok(changed_any)
}

/// Parse, type-check, and optionally auto-fix files.
fn check_or_fix_path(
    path_arg: &Option<String>,
    do_fix: bool,
    interactive: bool,
    force: bool,
) -> Result<(), String> {
    use zz_frontend::diag::{FixSafety, Severity};

    let raw = path_arg.as_deref().unwrap_or(".");
    let base = std::path::PathBuf::from(raw);
    let files = zz_fmt::discover(&[base]).map_err(|e| format!("file discovery failed: {e}"))?;

    if files.is_empty() {
        return Err(format!("no .zz files found in `{raw}`"));
    }

    let mut total_errors = 0u32;
    let mut total_fixes = 0u32;
    let mut any_safe_fixits = false;
    let mut any_ambiguous = false;

    for path in &files {
        let path_str = path.display().to_string();
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path_str}`: {e}"))?;

        let loaded = loader::load_program(path)?;

        // Classify fixits by safety.
        let mut safe_fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
        let mut ambiguous_fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
        let mut has_hard_errors = false;

        for e in &loaded.errors {
            for d in &e.diags {
                match d.severity {
                    Severity::Error | Severity::Warning => {
                        for fixit in &d.fixits {
                            match fixit.safety {
                                FixSafety::Safe => safe_fixits.push(fixit.clone()),
                                FixSafety::Ambiguous => ambiguous_fixits.push(fixit.clone()),
                            }
                        }
                        if d.severity == Severity::Error && d.fixits.is_empty() {
                            has_hard_errors = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        if !do_fix {
            // Check-only mode: print diagnostics, track what's fixable.
            for e in &loaded.errors {
                let mut files_ctx = Files::new();
                let id = files_ctx.add(e.name.clone(), e.source.clone());
                eprint!("{}", render_to_string(&files_ctx, id, &e.diags));
            }
            if !safe_fixits.is_empty() {
                any_safe_fixits = true;
            }
            if !ambiguous_fixits.is_empty() {
                any_ambiguous = true;
            }
            if has_hard_errors {
                total_errors += 1;
            }
            continue;
        }

        // Fix mode.
        // Collect all approved fixits (safe + accepted ambiguous) into one list.
        let mut approved: Vec<zz_frontend::diag::FixIt> = safe_fixits;

        // Handle ambiguous fixes based on mode.
        if !ambiguous_fixits.is_empty() {
            if force {
                // --hard: auto-apply all ambiguous fixes.
                approved.extend(ambiguous_fixits);
            } else if interactive {
                // -i / --interactive: prompt user for each ambiguous fix.
                for fixit in &ambiguous_fixits {
                    let start = fixit.span.start as usize;
                    let end = fixit.span.end as usize;
                    if end > source.len() || start >= end {
                        continue;
                    }
                    let original = &source[start..end];
                    let line_num = source[..start].matches('\n').count() + 1;

                    if fixit.alternatives.len() > 1 {
                        // Multiple candidates: show numbered menu.
                        eprintln!("  Ambiguous field `{original}` at {path_str}:{line_num}:");
                        for (i, alt) in fixit.alternatives.iter().enumerate() {
                            eprintln!("    [{}] {}", i + 1, alt);
                        }
                        eprintln!("    [s] Skip");
                        eprint!("  Choice [1-{}]: ", fixit.alternatives.len());

                        let mut input = String::new();
                        std::io::stdin()
                            .read_line(&mut input)
                            .map_err(|e| format!("stdin error: {e}"))?;
                        let trimmed = input.trim();

                        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("s") {
                            eprintln!("    skipped");
                            continue;
                        }

                        // Parse number.
                        match trimmed.parse::<usize>() {
                            Ok(n) if n >= 1 && n <= fixit.alternatives.len() => {
                                let mut chosen_fixit = fixit.clone();
                                chosen_fixit.replacement = fixit.alternatives[n - 1].clone();
                                approved.push(chosen_fixit);
                                eprintln!(
                                    "    applied: `{original}` → `{}`",
                                    fixit.alternatives[n - 1]
                                );
                            }
                            _ => {
                                eprintln!("    invalid choice, skipped");
                            }
                        }
                    } else {
                        // Single candidate: simple Y/n prompt.
                        eprint!(
                            "  ambiguous fix: `{original}` at {path_str}:{} — apply `{}`? [Y/n]: ",
                            fixit.span.start, fixit.replacement,
                        );
                        let mut input = String::new();
                        std::io::stdin()
                            .read_line(&mut input)
                            .map_err(|e| format!("stdin error: {e}"))?;
                        let trimmed = input.trim();
                        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") {
                            approved.push(fixit.clone());
                            eprintln!("    applied: `{original}` → `{}`", fixit.replacement);
                        } else {
                            eprintln!("    skipped");
                        }
                    }
                }
            } else {
                // Default --fix: skip ambiguous, hint user.
                any_ambiguous = true;
            }
        }

        // Apply all approved fixits in one pass, right-to-left to avoid offset shifts.
        let mut applied = 0u32;
        if !approved.is_empty() {
            let mut new_source = source.clone();
            approved.sort_by_key(|b| std::cmp::Reverse(b.span.start));
            for fixit in &approved {
                let start = fixit.span.start as usize;
                let end = fixit.span.end as usize;
                if end <= new_source.len() && start < end {
                    let original = source[start..end].to_string();
                    new_source.replace_range(start..end, &fixit.replacement);
                    applied += 1;
                    let label = if fixit.safety == zz_frontend::diag::FixSafety::Safe {
                        "fixed"
                    } else {
                        "fixed (force)"
                    };
                    eprintln!(
                        "  {label}: `{original}` → `{}` at {path_str}:{}",
                        fixit.replacement, fixit.span.start,
                    );
                }
            }

            if applied > 0 {
                std::fs::write(path, &new_source)
                    .map_err(|e| format!("cannot write `{path_str}`: {e}"))?;
                eprintln!("zz: applied {applied} fix(es) to `{path_str}`");
                total_fixes += applied;
            }
        }
    }

    if !do_fix && total_errors > 0 {
        return Err(format!("{total_errors} file(s) failed type-check"));
    }
    if do_fix && total_fixes == 0 {
        eprintln!("zz: no fixable diagnostics in `{raw}`");
    }

    // Footer hints in check-only mode.
    if !do_fix && (any_safe_fixits || any_ambiguous) {
        if any_safe_fixits {
            eprintln!("help: run `zz check --fix {raw}` to automatically apply safe fixes");
        }
        if any_ambiguous {
            eprintln!(
                "help: run `zz check --fix -i {raw}` to review ambiguous fixes interactively"
            );
            eprintln!("help: run `zz check --fix --hard {raw}` to force-apply all fixes");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::check_or_fix_path;

    fn write_temp(src: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "zz_check_test_{}_{}.zz",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, src).unwrap();
        path
    }

    #[test]
    fn check_ok_on_valid_file() {
        let path = write_temp("x := 1 + 2\nprintln(x)\n");
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_ok_on_phase4_features() {
        let path = write_temp(
            "scores := [10, 20, 30]\ny := scores[1]\nz := scores[1:3]\nfunc dbl(a: int, b: int) -> int { a * b }\nw := 5 |> dbl(3)\nt := typeof(w)\n",
        );
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_type_error() {
        let path = write_temp("x := 1 + \"a\"\n");
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_err(), "expected type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_index_error() {
        let path = write_temp("x := 5\nx[0]\n");
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_err(), "expected index type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_missing_file_errors() {
        let result = check_or_fix_path(
            &Some("/tmp/zz_no_such_file_zz.zz".to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_err(), "expected error for missing file");
    }
    #[test]
    fn check_no_arg_errors() {
        let examples_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        assert!(examples_dir.is_dir(), "examples dir should exist");
        let result = check_or_fix_path(
            &Some(examples_dir.display().to_string()),
            false,
            false,
            false,
        );
        // The function may fail type-check on examples; the point is it
        // should find files and not panic/IO-error.
        match &result {
            Err(msg) if msg.contains("does not exist") || msg.contains("no .zz files") => {
                panic!("scan should find files: {msg}");
            }
            _ => {} // either Ok or type-check errors — both prove scanning worked.
        }
    }
}
