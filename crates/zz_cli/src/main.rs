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
mod doctor;
mod loader;
mod pm;
mod repl;
mod session;
mod setup;
mod test_runner;
mod toolchain;
mod ui;
mod upgrade;

use zz_frontend::diag::{error_at, render_to_string, Files};
use zz_frontend::span::Span;
use zz_runtime::{Interp, Value};

use session::Session;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Enforce the project's `[package] zz` minimum-compiler requirement, if
/// any. No manifest or no `zz` key → pass. Called by the run/build/test
/// entries so an outdated compiler fails fast with an upgrade hint
/// instead of cryptic backend errors.
pub(crate) fn enforce_project_zz(start: &std::path::Path) -> Result<(), String> {
    let Some(root) = loader::find_project_root(start) else {
        return Ok(());
    };
    let manifest_path = root.join("zz.toml");
    if !manifest_path.exists() {
        return Ok(());
    }
    let Ok(manifest) = zz_pm::manifest::Manifest::load(&manifest_path) else {
        return Ok(()); // unloadable: the normal flow reports it better
    };
    manifest.check_zz_version(VERSION)
}

const USAGE: &str = "\
zz — the ZZ programming language

USAGE:
    zz                            start the interactive REPL
    zz eval <source>              evaluate source and print the result
    zz run [<file.zz>]            type-check and run a file (defaults to the
                                   project entry when omitted inside a project)
    zz run --bytecode [<file>]    run via .zzc bytecode (compiles .zz, or
                                  loads .zzc directly with no frontend)
    zz dis [<file.zzc|file.zz>]   disassemble bytecode to stable text
    zz test <file.zz | dir>       run @test-annotated functions
    zz check [FLAGS] [PATH]       scan for errors/warnings (file or directory)
    zz fix [FLAGS] [PATH]         apply auto-fixes (shortcut for check --fix)
    zz fmt [FLAGS] [PATH]         format ZZ source files in-place
    zz build [FLAGS] [<file.zz>]  compile a native binary (cached; defaults
                                   to the project entry when omitted inside
                                   a project)
    zz build --emit-ir -o <f.zzc> emit .zzc bytecode instead of a binary

PACKAGE MANAGER:
    zz init [--template T]        initialize zz.toml + src/main.zz in cwd
    zz new <name> [--template T]  create a new project (git init -b main)
                                  template: cli|lib|web, pkg:NAME, git URL, or PATH
                                  flags: --force (non-empty dir), --no-git
    zz add <pkg>[@ver]            add a dependency to zz.toml
    zz install, zz i              resolve deps, fetch into CAS, link
    zz install --path <dir|file|pkg>
                                  build from source (release) and install the
                                  binary into ~/.zz/bin; a bare package name
                                  is fetched from the registry first
    zz install --allow-source-builds
                                permit transitive native source builds
    zz install --allow-hooks    permit legacy [native] build hooks (direct only)
    zz remove <pkg>               remove a dependency
    zz update [pkg]               re-resolve floating versions
    zz outdated                   locked vs wanted vs latest per dep
    zz deps tree [--depth N]      print the dependency tree
    zz deps why <pkg>             show why a package is depended on
    zz audit                      verify pins (published? hash? licensed?)
    zz clean [--deps]             remove build outputs (plus vendor/ + lock)
    zz search <query>             search the package registry
    zz info <pkg>                 show package metadata and versions
    zz login [--browser]          authenticate for publishing
    zz publish [--dry-run]        validate, pack, and upload to the registry
    zz cache gc                   garbage-collect unused CAS entries
    zz cache clean                clear build cache
    zz setup [--yes]              create ~/.zz/bin, wire PATH + completions
    zz setup --check              verify shell integration (no changes)
    zz toolchain install          download a pinned Zig C backend into ~/.zz/toolchain
    zz toolchain status           installed versions, pin, active backend
    zz completion [shell]         print shell completion (bash|zsh|fish|powershell)
    zz upgrade [--check]           self-update from GitHub releases
    zz doctor [--fix]              audit the toolchain (binary, clang, shell, git, registry)

BUILD MODES (single Clang backend, always a native binary):
     zz build <file.zz>           static build (ThinLTO, DCE, stripped) — the default
     zz build --dynamic <file.zz> debug build (-O0 -g, fast, dynamic)
     zz build -p <file.zz>        release build (-O3 -flto=thin, dynamic, stripped)
     zz build -p --full <file.zz> max optimization (full LTO + DCE + strip); with `-- <args>` adds PGO training
     zz build --static <file.zz>  static build, explicit (same as the default; errors where static is impossible)
     zz build --pgo <file.zz>     PGO build (profile-guided, native host only)
     zz profile [<file.zz>] [-- args]
                                  PGO end to end: instrument → train → optimize
     zz build --target <triple> <file.zz>
                                  cross build via clang --target= (drops -march=native)

FLAGS:
    --check, -c        with fmt, check formatting without writing (exit 1 if changed)
    --stdin            with fmt, read source from stdin and write formatted to stdout
    --fix, -f          apply safe auto-fixes (typo replacements, field corrections)
    --hard             with --fix, apply ALL fixes including ambiguous ones (no prompts)
    --interactive, -i  with --fix, prompt for ambiguous fixes interactively
    --native           with run/test, use the native AOT compiler instead of the VM
                         (test: per-file dev build, one process per test)
    --embed <dir>      with run/build, serve (VM) or bake (native) a static asset
                       directory, readable at runtime via `fs.embedfs()`
    -p, --release      with build, full optimization (-O3 -flto=thin, dynamic, stripped)
    --static           with build, static self-contained binary (the default; explicit use errors where static is impossible)
    --dynamic          with build, dynamic debug build (-O0 -g, fast); falls back automatically where static is impossible
    --full             with build, max optimization: full LTO (-O3, DCE, stripped); with `-- <args>` runs PGO training first
    -o, --output <name> with build/run --native, name the output binary
                          (bare name stays in the project bin/ or standalone
                          CWD; path is used as-is)
    --pgo              with build, profile-guided optimization build (native host only)
    --target <triple>  with build, cross-compile via clang --target= (same flags as without -p, minus -march=native)
    --cc <clang|zig>   with build, select the Clang provider
    --chunk            with build, lower from the unified IR chunk instead of
                       HIR (dual-codegen gate; stdout+exit must match HIR)
    --allow-source-builds
                        with build/install, compile transitive native deps
                        from source when no prebuilt covers the host tag
    --allow-hooks       with build/install, run legacy [native] build hooks
                        (direct deps only; transitive hooks always error)
    --verbose          with build, print the exact clang command line
    --template <T>     with init/new, template: cli (default), lib, or web
    --git <url>        with add, git URL for dependency
    --rev <rev>        with add, git revision (branch, tag, or commit)
    --path <path>      with add, local path dependency
    --registry <url>   with add/install/search/info/login/publish/update,
                       registry base URL (default: ZZ_REGISTRY or the public registry)
    --limit <n>        with search, max results (default 20)
    --dry-run          with publish, validate + pack without uploading
    --artifact-dir <dir>
                        with publish, verify per-tag prebuilt tarballs and
                        print the [native.prebuilt] stanza
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
    zz install --path .               build this project and install it to ~/.zz/bin
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
    zz build --static hello.zz       static build, explicit (self-contained)
";

fn main() -> ExitCode {
    // Self-heal ~/.zz/bin + PATH hint on every run (cheap, silent when piped).
    // No hint when already running setup/completion — that *is* the fix.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let early_cmd = args.first().map(String::as_str);
    let self_managing = matches!(early_cmd, Some("setup") | Some("completion"));
    setup::auto_heal(self_managing);

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
            let bytecode = rest.iter().any(|a| a == "--bytecode");
            let embed = parse_flag_value(rest, "--embed").map(std::path::PathBuf::from);
            // `-o` / `--output` names the published binary, `--native`
            // only: VM runs forward everything after the file to the
            // script untouched.
            let output = if native {
                parse_flag_value(rest, "--output")
                    .or_else(|| parse_flag_value(rest, "-o"))
                    .map(std::path::PathBuf::from)
            } else {
                None
            };
            // Strip `--embed <dir>` / `--embed=<dir>` (and `--native` /
            // `--bytecode`, plus `-o <name>` for native) so neither the
            // loader nor the script sees them as paths/args.
            let mut args: Vec<String> = strip_flag_value(rest, "--embed", "--native");
            args.retain(|a| a != "--bytecode");
            if native {
                args = strip_output_flag(&args);
            }
            let script_args = args.get(1..).unwrap_or(&[]).to_vec();
            let file = args.iter().find(|a| !a.starts_with('-'));
            if native && bytecode {
                eprintln!("zz: cannot combine `--native` and `--bytecode`");
                return ExitCode::FAILURE;
            }
            if native {
                match run_native(file, &script_args, embed, output) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("zz: {msg}");
                        ExitCode::FAILURE
                    }
                }
            } else if bytecode {
                match run_bytecode(file, &script_args, embed) {
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
        Some("dis") => {
            let file = rest.iter().find(|a| !a.starts_with('-'));
            match dis_file(file) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("zz: {msg}");
                    ExitCode::FAILURE
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
        Some("profile") => match profile_cmd(rest) {
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
            let has_stats = flags.contains(&"--stats".to_string());

            let interactive = has_fix && has_interactive && !has_hard;
            match check_or_fix_path(&path, has_fix, interactive, has_hard, has_stats) {
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
            match check_or_fix_path(&path, true, interactive, has_hard, false) {
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
        Some("outdated") => match pm::outdated(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("deps") => match pm::deps(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("audit") => match pm::audit(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("clean") => match pm::clean(rest) {
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
        Some("setup") => match setup::run(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("upgrade") => match upgrade::run(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("doctor") => match doctor::run(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("toolchain") => match toolchain::run(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("completion") => match setup::print_completion(rest) {
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
///
/// Returns the number of plugin libraries loaded (0 with no plugins, or
/// when every candidate failed — failures warn per-library above).
#[cfg(unix)]
fn load_vm_plugins(
    project_dir: &std::path::Path,
    natives: &mut std::collections::HashMap<String, zz_runtime::NativeEntry>,
    plugin_funcs: &[(String, zz_checker::FuncSig)],
) -> Result<usize, String> {
    use zz_pm::lock::Lockfile;

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
    let manifest = zz_pm::manifest::Manifest::load(&manifest_path).ok();

    let mut loaded_count = 0;
    for dep in &lock.deps {
        // Resolve package directory (canonicalized: a relative dep path
        // must never leak `..` into hook/dlsym paths downstream).
        let Some(pkg_dir) = crate::build::resolve_pkg_dir(&project_dir, dep, manifest.as_ref())
        else {
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
        let c_funcs: Option<std::collections::HashMap<String, zz_checker::FuncSig>> =
            match zz_plugin::load_manifest(&zzi_path) {
                Ok(manifest) if manifest.meta.c_abi.is_some() => Some(manifest.funcs),
                _ => None,
            };

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
            // Both paths retain the dlopen handle: dropping it unloads the
            // library, unmapping registered pointers (Rust entries and raw
            // C-registry symbol addresses alike).
            let loaded: Result<(), String> = match &c_funcs {
                Some(funcs) => match zz_plugin::load_c_plugin(lib_path, funcs) {
                    Ok(handle) => {
                        keep_plugin_alive(handle);
                        Ok(())
                    }
                    // Not a C plugin (e.g. a transitional Rust cdylib next
                    // to the C .so): try the next library silently.
                    Err(zz_plugin::LoadError::MissingSymbol { symbol, .. })
                        if symbol == "ZZ_C_PLUGIN_ABI_VERSION" =>
                    {
                        continue;
                    }
                    Err(e) => Err(e.to_string()),
                },
                None => zz_plugin::load_plugin(lib_path, natives)
                    .map(|handle| {
                        // The handle MUST stay alive (see keep_plugin_alive).
                        keep_plugin_alive(handle);
                    })
                    .map_err(|e| e.to_string()),
            };
            match loaded {
                Ok(()) => {
                    // Progress chatter only under ZZ_VERBOSE: everyday runs
                    // (and program output) stay clean; piped runs especially.
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
) -> Result<usize, String> {
    // dlopen not supported on this platform yet
    Ok(0)
}

/// A `.zz` program ready to execute: loaded modules, HIR types, and a
/// fully seeded interpreter (natives, consts, pure-ZZ stdlib, aliases).
struct PreparedRun {
    interp: Interp,
    programs: Vec<zz_frontend::ast::Program>,
    files: Vec<(String, String)>,
    types: std::sync::Arc<std::collections::HashMap<zz_checker::SpanKey, zz_checker::Type>>,
    structs: std::collections::HashMap<String, zz_checker::StructSig>,
    enums: std::collections::HashMap<String, zz_checker::EnumSig>,
    funcs: std::collections::HashMap<String, zz_checker::FuncSig>,
    entry_path: String,
}

/// Seed an interpreter: native dispatch, math constants, pure-ZZ
/// stdlib programs, and import-alias mirrors. Shared by `run` and the
/// `.zzc` loader (which supplies stdlib natives and empty maps).
#[allow(clippy::too_many_arguments)]
fn setup_interp(
    natives: std::collections::HashMap<String, zz_runtime::NativeEntry>,
    loaded_consts: &std::collections::HashMap<String, f64>,
    import_aliases: std::collections::HashMap<String, String>,
    stdlib_aliases: &[(String, String)],
    project_root: &std::path::Path,
    plugin_funcs: &[(String, zz_checker::FuncSig)],
    script_args: &[String],
    embed: Option<&std::path::Path>,
) -> Result<Interp, String> {
    let mut natives = natives;
    let first_try = match crate::load_vm_plugins(project_root, &mut natives, plugin_funcs) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("zz: warning: {e}");
            0
        }
    };
    if !plugin_funcs.is_empty() && first_try == 0 {
        crate::build::ensure_native_hooks(project_root);
        if let Err(e) = crate::load_vm_plugins(project_root, &mut natives, plugin_funcs) {
            eprintln!("zz: warning: {e}");
        }
    }

    let mut interp = Interp::with_natives(natives);
    interp.args = script_args.to_vec();
    // Selective-import aliases for bare generic-function calls (see
    // LoadResult::import_aliases): the VM compiler emits no code for
    // import statements, so the runtime map would otherwise stay empty.
    interp.import_aliases = import_aliases;

    // `--embed <dir>`: serve the asset tree to `fs.embedfs()` for this run.
    if let Some(dir) = embed {
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
    for (name, val) in loaded_consts {
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
            zz_prog.enums.clone(),
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
        for (module, ns) in stdlib_aliases {
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
    Ok(interp)
}

/// Load, check, and seed a `.zz` program for execution.
fn prepare_run(
    path: &str,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
) -> Result<PreparedRun, String> {
    let script_path = std::path::Path::new(path);
    // Project root: walk up from the script (entry files usually live in
    // `src/`; `zz.lock` sits at the root). Falls back to the script dir.
    let project_root = loader::find_project_root(script_path).unwrap_or_else(|| {
        script_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf()
    });
    // Fail fast on an unsatisfied `[package] zz` compiler requirement.
    enforce_project_zz(script_path)?;
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
    let crate::loader::LoadResult {
        programs,
        files,
        funcs,
        structs: _loaded_structs,
        aliases,
        enums: _loaded_enums,
        natives,
        consts,
        errors: _,
        stdlib_aliases,
        import_aliases,
        ..
    } = loaded;
    let typed = zz_hir::build_program(
        &merged,
        std::collections::HashMap::new(),
        funcs,
        _loaded_structs,
        aliases,
        _loaded_enums,
    );
    let interp = setup_interp(
        natives,
        &consts,
        import_aliases,
        &stdlib_aliases,
        &project_root,
        &plugin_funcs,
        script_args,
        embed.as_deref(),
    )?;
    Ok(PreparedRun {
        interp,
        programs,
        files,
        types: std::sync::Arc::new(typed.program.types),
        structs: typed.program.structs,
        enums: typed.program.enums,
        funcs: typed.program.funcs,
        entry_path: path.to_string(),
    })
}

/// Render an execution error against its source. Empty sources (`.zzc`
/// loads carry none) render plainly — the span renderer would panic.
fn render_eval_error(e: &zz_runtime::EvalError, name: &str, source: &str) -> String {
    if source.is_empty() {
        eprintln!(
            "error: {} (bytecode span {}..{})",
            e.message, e.span.start, e.span.end
        );
        for (fname, _) in &e.backtrace {
            if !fname.is_empty() {
                eprintln!("  at {fname}");
            }
        }
        for note in &e.notes {
            eprintln!("  note: {note}");
        }
        return "program failed".to_string();
    }
    let mut files = Files::new();
    let id = files.add(name.to_string(), source.to_string());
    let mut diag = error_at(e.message.clone(), e.span);
    for (fname, _span) in &e.backtrace {
        if !fname.is_empty() {
            diag = diag.with_note(format!("  at {fname}"));
        }
    }
    for note in &e.notes {
        diag = diag.with_note(note.clone());
    }
    let diags = vec![diag];
    eprint!("{}", render_to_string(&files, id, &diags));
    "program failed".to_string()
}

/// Print a non-unit program result, then auto-call `main()` when the
/// entry namespace defines it (with script args iff it takes params).
fn run_entry_main(
    interp: &mut Interp,
    last: Value,
    entry_path: &str,
    script_args: &[String],
    files: &[(String, String)],
) -> Result<(), String> {
    if last != Value::Unit {
        println!("{last}");
    }

    // Auto-call `main()` if defined in the entry file.
    // The entry file's namespace is its file stem (e.g. `myapp.zz` → `myapp`).
    let entry_ns = std::path::Path::new(entry_path)
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
                let (name, source) = files
                    .last()
                    .cloned()
                    .unwrap_or_else(|| (entry_path.to_string(), String::new()));
                return Err(render_eval_error(&e, &name, &source));
            }
        }
    }

    Ok(())
}

/// `zz run --bytecode <file>`: `.zz` compiles, round-trips through `.zzc`
/// bytes (decode + verify + raise), and executes with no AST-derived
/// structures on the execution path. `.zzc` loads straight from bytes
/// with no frontend at all.
fn run_bytecode(
    path: Option<&String>,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
) -> Result<(), String> {
    let path = resolve_run_entry(
        path,
        "zz run --bytecode [<file.zz|file.zzc>]",
        "provide a .zz file (round-trips through bytecode) or a .zzc file (loads directly)",
    )?;
    if path.ends_with(".zzc") {
        run_bytecode_file(&path, script_args)
    } else {
        run_bytecode_zz(&path, script_args, embed)
    }
}

fn run_bytecode_zz(
    path: &str,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
) -> Result<(), String> {
    let mut prep = prepare_run(path, script_args, embed)?;
    let mut last = Value::Unit;
    for (i, program) in prep.programs.iter().enumerate() {
        let native_names: std::sync::Arc<std::collections::HashSet<String>> =
            std::sync::Arc::new(prep.interp.natives.keys().cloned().collect());
        let chunk = std::sync::Arc::new(zz_runtime::vm::Compiler::compile_program_typed(
            program,
            prep.types.clone(),
            prep.structs.clone(),
            prep.enums.clone(),
            native_names,
        ));
        // Serialize, then drop every AST-derived structure: from here on
        // only bytes-derived data may flow into execution.
        let module = zz_ir::lower::lower_typed(&chunk, &prep.funcs)
            .map_err(|e| format!("zz: ir lower failed: {e}"))?;
        let bytes = zz_ir::codec::encode(&module);
        drop(chunk);
        drop(module);
        let loaded = zz_ir::codec::decode(&bytes).map_err(|e| format!("zz: invalid .zzc: {e}"))?;
        zz_ir::verify::verify(&loaded).map_err(|e| format!("zz: .zzc verify failed: {e}"))?;
        let chunk =
            zz_ir::raise::raise(&loaded).map_err(|e| format!("zz: ir raise failed: {e}"))?;
        match prep.interp.run_loaded_chunk(&chunk) {
            Ok(v) => last = v,
            Err(e) => {
                let (name, source) = prep
                    .files
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| (path.to_string(), String::new()));
                return Err(render_eval_error(&e, &name, &source));
            }
        }
    }
    run_entry_main(
        &mut prep.interp,
        last,
        &prep.entry_path,
        script_args,
        &prep.files,
    )
}

fn run_bytecode_file(path: &str, script_args: &[String]) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("zz: cannot read {path}: {e}"))?;
    let loaded = zz_ir::codec::decode(&bytes).map_err(|e| format!("zz: invalid .zzc: {e}"))?;
    zz_ir::verify::verify(&loaded).map_err(|e| format!("zz: .zzc verify failed: {e}"))?;
    let chunk = zz_ir::raise::raise(&loaded).map_err(|e| format!("zz: ir raise failed: {e}"))?;
    // Bare interpreter: stdlib natives, no project context. The module
    // is self-contained; imports were resolved at compile time. Every
    // stdlib module namespace is registered (the `.zz` path does this
    // per import via the loader): over-approximation is safe here for
    // the same reason injecting all bare const forms is — the checker
    // already gated names at compile time.
    let project_root = std::path::Path::new(path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let mut natives = zz_stdlib::natives::stdlib_natives();
    {
        let mut funcs = std::collections::HashMap::new();
        for module in zz_stdlib::STDLIB_MODULES {
            let ns = module.rsplit('.').next().unwrap_or(module);
            let _ = zz_stdlib::register_module_namespace(module, ns, &mut funcs, &mut natives);
        }
    }
    let empty_map = std::collections::HashMap::new();
    let mut interp = setup_interp(
        natives,
        &empty_map,
        std::collections::HashMap::new(),
        &[],
        &project_root,
        &[],
        script_args,
        None,
    )?;
    let last = match interp.run_loaded_chunk(&chunk) {
        Ok(v) => v,
        Err(e) => {
            return Err(render_eval_error(&e, path, ""));
        }
    };
    run_entry_main(
        &mut interp,
        last,
        path,
        script_args,
        &[(path.to_string(), String::new())],
    )
}

/// `zz dis <file>`: disassemble `.zzc` bytes (or a `.zz` program lowered
/// in memory) to stable text.
fn dis_file(path: Option<&String>) -> Result<(), String> {
    let path = resolve_run_entry(
        path,
        "zz dis [<file.zzc|file.zz>]",
        "provide a .zzc file or a .zz file to disassemble",
    )?;
    if path.ends_with(".zzc") {
        let bytes = std::fs::read(&path).map_err(|e| format!("zz: cannot read {path}: {e}"))?;
        let module = zz_ir::codec::decode(&bytes).map_err(|e| format!("zz: invalid .zzc: {e}"))?;
        print!("{}", zz_ir::dis::disassemble(&module));
        return Ok(());
    }
    let prep = prepare_run(&path, &[], None)?;
    for (i, program) in prep.programs.iter().enumerate() {
        let native_names: std::sync::Arc<std::collections::HashSet<String>> =
            std::sync::Arc::new(prep.interp.natives.keys().cloned().collect());
        let chunk = zz_runtime::vm::Compiler::compile_program_typed(
            program,
            prep.types.clone(),
            prep.structs.clone(),
            prep.enums.clone(),
            native_names,
        );
        let module = zz_ir::lower::lower_typed(&chunk, &prep.funcs)
            .map_err(|e| format!("zz: ir lower failed: {e}"))?;
        let name = prep
            .files
            .get(i)
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        println!("; module {name}");
        print!("{}", zz_ir::dis::disassemble(&module));
    }
    Ok(())
}

/// `zz build --emit-ir -o <file.zzc> <file.zz>`: type-check, lower one
/// module to `.zzc`, and write it. Multi-module programs are rejected:
/// one `.zzc` holds exactly one module (multi-entry is future work).
fn emit_ir_cmd(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--") {
        return Err("training args need `--full`\n\
             usage: zz build --emit-ir -o <file.zzc> <file.zz>"
            .to_string());
    }
    let output = parse_flag_value(args, "--output")
        .or_else(|| parse_flag_value(args, "-o"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            "missing output\n\n\
             usage: zz build --emit-ir -o <file.zzc> <file.zz>"
                .to_string()
        })?;
    // Positional path: first non-flag arg, skipping values consumed by
    // `--output` / `-o` / `--embed` / `--target` / `--cc` (space form).
    let mut skip_next = false;
    let path = args
        .iter()
        .find(|a| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if a.as_str() == "--output"
                || a.as_str() == "-o"
                || a.as_str() == "--embed"
                || a.as_str() == "--target"
                || a.as_str() == "--cc"
                || a.as_str() == "--emit-ir"
            {
                if !a.starts_with("--emit-ir") {
                    skip_next = true;
                }
                return false;
            }
            !a.starts_with('-')
        })
        .ok_or_else(|| {
            "missing file argument\n\n\
             usage: zz build --emit-ir -o <file.zzc> <file.zz>"
                .to_string()
        })?;
    let prep = prepare_run(path, &[], None)?;
    if prep.programs.len() != 1 {
        return Err(format!(
            "zz: --emit-ir needs a single-module program, found {} modules\n\
             hint: multi-module .zzc is future work; run each module through --bytecode instead",
            prep.programs.len()
        ));
    }
    let native_names: std::sync::Arc<std::collections::HashSet<String>> =
        std::sync::Arc::new(prep.interp.natives.keys().cloned().collect());
    let chunk = zz_runtime::vm::Compiler::compile_program_typed(
        &prep.programs[0],
        prep.types.clone(),
        prep.structs.clone(),
        prep.enums.clone(),
        native_names,
    );
    let module = zz_ir::lower::lower_typed(&chunk, &prep.funcs)
        .map_err(|e| format!("zz: ir lower failed: {e}"))?;
    let bytes = zz_ir::codec::encode(&module);
    std::fs::write(&output, &bytes)
        .map_err(|e| format!("zz: cannot write {}: {e}", output.display()))?;
    eprintln!(
        "zz: wrote {} ({} bytes, {} funcs)",
        output.display(),
        bytes.len(),
        module.funcs.len()
    );
    Ok(())
}

/// Resolve the entry file for `run` / `run --native` / `build` when the
/// file argument is omitted: the conventional project entry (`src/main.zz`,
/// then `main.zz`) under the project discovered from the current working
/// directory. An explicit argument is returned unchanged (its owning
/// project — resolved from the source path itself — decides the output
/// destination; see [`build::planned_dest_for`]).
fn resolve_default_entry(usage: &str, hint: &str) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let Some(root) = loader::find_project_root(&cwd) else {
        return Err(format!(
            "missing file argument\n\nusage: {usage}\nhint: {hint}"
        ));
    };
    // resolve_entry carries actionable hints for every failure shape.
    Ok(build::resolve_entry(&root)?.to_string_lossy().into_owned())
}

/// Resolve the entry file for `run` / `run --native`: explicit argument
/// wins, otherwise [`resolve_default_entry`].
fn resolve_run_entry(path: Option<&String>, usage: &str, hint: &str) -> Result<String, String> {
    match path {
        Some(p) => Ok(p.clone()),
        None => resolve_default_entry(usage, hint),
    }
}

fn run_file(
    path: Option<&String>,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
) -> Result<(), String> {
    let resolved = resolve_run_entry(
        path,
        "zz run [<file.zz>]",
        "provide the path to a .zz file to execute",
    )?;
    let path = &resolved;

    let mut prep = prepare_run(path, script_args, embed)?;
    let mut last = Value::Unit;
    for (i, program) in prep.programs.iter().enumerate() {
        match prep.interp.run_typed(
            program,
            prep.types.clone(),
            prep.structs.clone(),
            prep.enums.clone(),
        ) {
            Ok(v) => last = v,
            Err(e) => {
                let (name, source) = prep
                    .files
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| (prep.entry_path.clone(), String::new()));
                return Err(render_eval_error(&e, &name, &source));
            }
        }
    }
    run_entry_main(
        &mut prep.interp,
        last,
        &prep.entry_path,
        script_args,
        &prep.files,
    )
}

/// `zz run --native [<file>]`: build (cached, published to the
/// authoritative output destination, `-o` overrides like `build`) and
/// execute. The file argument defaults to the project entry inside a
/// project.
fn run_native(
    path: Option<&String>,
    script_args: &[String],
    embed: Option<std::path::PathBuf>,
    output: Option<std::path::PathBuf>,
) -> Result<(), String> {
    let resolved = resolve_run_entry(
        path,
        "zz run --native [<file.zz>]",
        "provide the path to a .zz file to compile and execute",
    )?;
    let path = &resolved;
    let p = std::path::Path::new(path);
    // Fail fast on an unsatisfied `[package] zz` compiler requirement.
    enforce_project_zz(p)?;
    // Release mode for true native speed — unless `ZZ_NATIVE_DEV=1`
    // (parity sweeps: `-O0 -g`, no LTO, ~4x faster clang per fixture;
    // same generated C, separate cache entries via the fingerprint).
    let mode = if std::env::var("ZZ_NATIVE_DEV").is_ok() {
        build::BuildMode::Dev
    } else {
        build::BuildMode::Release
    };
    let auto_output = output.is_none();
    let rel = build::ReleaseOptions {
        embed,
        output,
        ..Default::default()
    };
    // Publish through a private destination when the user gave no `-o`:
    // parallel same-named runs (e.g. the parity suites) shared one
    // published binary, so a build could stage a sibling's program between
    // publish and stage — rotating VM-vs-native mismatches. An explicit
    // `-o` keeps today's shared destination (user's choice, user's race).
    // auto_tmp tracks the private dir for best-effort cleanup below.
    let mut auto_tmp: Option<std::path::PathBuf> = None;
    let rel = if auto_output {
        static RUN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let uniq = RUN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("zz-run-native-{}-{uniq}", std::process::id()));
        auto_tmp = Some(tmp.clone());
        build::ReleaseOptions {
            output: Some(tmp.join("zz_out")),
            ..rel
        }
    } else {
        rel
    };
    // Publish through the normal destination, then execute a private
    // staged copy: concurrent same-named publishes can never swap the
    // binary mid-exec.
    let built = build::build_release(p, mode, &rel)?;
    let (bin, cleanup) = build::stage_exec_copy(&built)?;
    let code = match build::exec_binary(&bin, script_args) {
        Ok(code) => code,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };
    cleanup();
    // Best-effort removal of the private publish dir (never the user's
    // explicit `-o`): `run` must not litter temp or CWD.
    if let Some(dir) = auto_tmp {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if code != 0 {
        return Err(format!(
            "native program exited with code {code}\n\
             hint: the program may have panicked or returned a non-zero exit code"
        ));
    }
    Ok(())
}

/// `zz build [FLAGS] [<file>]`: always a native Clang binary.
///
/// Default (`zz build`): static self-contained binary (ThinLTO, DCE,
/// stripped). Falls back to dynamic with a note where static is
/// impossible (macOS targets, missing static system libraries);
/// explicit `--static` errors there instead. The Rust native runtime
/// links statically too (rlib, no RUNPATH — #303), so FFI programs
/// stay static.
/// `-p/--release/-O3` selects the dynamic optimized build; `--dynamic`
/// selects the fast dynamic debug build (`-O0 -g`).
/// Output (authoritative, no legacy duplicates): standalone builds place
/// `./<stem>` in the current working directory; project builds (file
/// omitted inside a project, or any source under a project) publish to
/// `<project-root>/bin/` named after `[package] name` (entry) or the
/// file stem (non-entry files). Bare `-o` renames inside that directory;
/// path-like `-o` is used as-is relative to the current directory.
/// (`zz run` without `--native` is the only command that executes
/// through the VM, leaving no build artifacts.)
fn build_cmd(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--emit-ir") {
        return emit_ir_cmd(args);
    }
    // Training args for `--full -- <program args>`: everything after the
    // first `--` belongs to the training run, never to flag parsing —
    // so the split happens before any flag is read. A bare `--` still
    // triggers the PGO pipeline (training with no args is valid).
    let dashdash = args.iter().position(|a| a == "--");
    let (flag_args, train_args) = split_train_args(args);
    let flag_args: Vec<String> = flag_args.to_vec();
    if flag_args.iter().any(|a| a == "--dev") {
        return Err(
            "`--dev` was removed: use `--dynamic` for the fast dynamic debug build\n\
             hint: drop --dev (default is static; -p/--release optimizes)"
                .to_string(),
        );
    }
    let release = flag_args
        .iter()
        .any(|a| a == "-p" || a == "--release" || a == "-O3");
    let is_static = flag_args.iter().any(|a| a == "--static");
    let is_dynamic = flag_args.iter().any(|a| a == "--dynamic");
    let is_full = flag_args.iter().any(|a| a == "--full");
    if is_static && is_dynamic {
        return Err("cannot combine `--static` and `--dynamic`\n\
             hint: drop one flag (default is static where possible)"
            .to_string());
    }
    if is_full && is_static {
        return Err("cannot combine `--full` and `--static`\n\
             hint: --full needs a dynamic link (full LTO + profile runtime); drop --static"
            .to_string());
    }
    if is_full && is_dynamic {
        return Err("cannot combine `--full` and `--dynamic`\n\
             hint: --full implies an optimized base; drop --dynamic"
            .to_string());
    }
    let is_pgo = flag_args.iter().any(|a| a == "--pgo");
    if is_full && is_pgo {
        return Err("cannot combine `--full` and `--pgo`\n\
             hint: --full runs its own instrument-train-optimize pipeline; pass training args after `--` instead"
            .to_string());
    }
    let verbose = flag_args.iter().any(|a| a == "--verbose");
    let allow_source_builds = flag_args.iter().any(|a| a == "--allow-source-builds");
    let allow_hooks = flag_args.iter().any(|a| a == "--allow-hooks");
    let target = parse_flag_value(&flag_args, "--target");
    let cc = parse_flag_value(&flag_args, "--cc");
    let embed = parse_flag_value(&flag_args, "--embed").map(std::path::PathBuf::from);
    let output = parse_flag_value(&flag_args, "--output")
        .or_else(|| parse_flag_value(&flag_args, "-o"))
        .map(std::path::PathBuf::from);
    if !train_args.is_empty() && !is_full {
        return Err("training args need `--full`\n\
             usage: zz build --release --full <file.zz> -- <program args>\n\
             hint: args after `--` run the PGO training workload"
            .to_string());
    }
    // Positional path: first non-flag arg, skipping values consumed by
    // `--target <triple>` / `--cc <name>` / `--embed <dir>` /
    // `-o <name>` (space form). Omitted inside a project: build the
    // conventional entry (`src/main.zz`, then `main.zz`).
    let mut skip_next = false;
    let path: String = match flag_args
        .iter()
        .find(|a| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if a.as_str() == "--target"
                || a.as_str() == "--cc"
                || a.as_str() == "--embed"
                || a.as_str() == "-o"
                || a.as_str() == "--output"
            {
                skip_next = true;
                return false;
            }
            !a.starts_with('-')
        })
        .cloned() {
        Some(p) => p,
        None => resolve_default_entry(
            "zz build [-p|--release|-O3|--static|--dynamic|--full|--pgo] [--target <triple>] [--cc <clang|zig>] [--embed <dir>] [-o <name>] [<file.zz>]",
            "provide the path to a .zz file to build (or run inside a project)",
        )?,
    };
    let path = path.as_str();
    let p = std::path::Path::new(path);

    // Fail fast on an unsatisfied `[package] zz` compiler requirement.
    crate::enforce_project_zz(p)?;

    // Default (no flags) is a static self-contained build; `--full`
    // selects max optimization (full LTO; plus PGO when training args
    // follow `--`); -p is the dynamic optimized build, --dynamic the
    // fast dynamic debug build. --static/--pgo select their own option
    // sets. Guards (PGO-cross, explicit-static-macOS) in validate()
    // apply uniformly; default-static downgrade paths (macOS, missing
    // static syslibs) fall back to dynamic with a note instead.
    let mode = if is_pgo {
        build::BuildMode::Pgo
    } else if is_static {
        build::BuildMode::Static
    } else if is_full {
        build::BuildMode::Full
    } else if release {
        build::BuildMode::Release
    } else if is_dynamic {
        build::BuildMode::Dev
    } else {
        build::BuildMode::Static
    };
    let allow_downgrade = mode == build::BuildMode::Static && !is_static;
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
        allow_source_builds,
        allow_hooks,
        allow_static_downgrade: allow_downgrade,
        output: output.clone(),
        chunk: flag_args.iter().any(|a| a == "--chunk") || std::env::var("ZZ_CHUNK_C").is_ok(),
    };
    let mode_str = match mode {
        build::BuildMode::Dev => "dev",
        build::BuildMode::Release => "release",
        build::BuildMode::Static => "static",
        build::BuildMode::Pgo | build::BuildMode::PgoUse => "pgo",
        build::BuildMode::Full | build::BuildMode::FullPgo => "full",
    };
    // `--full -- <train args>`: max optimization with PGO —
    // instrument, train, merge, rebuild optimized (mirrors `zz profile`
    // phases but lands full-LTO output). Without train args the single
    // Full build below is the whole story.
    if is_full && dashdash.is_some() {
        return build_full_with_training(path, &rel, &train_args);
    }
    crate::ui::header(&format!("building {path} ({mode_str})"));
    // The clang link step can run for minutes with no output — spin with
    // elapsed time so a big build never looks frozen. Cache hits finish
    // instantly, so the spinner is just one extra line there.
    let spinner = crate::ui::Spinner::start(&format!("Compiling {mode_str}"));
    let dest = match build::build_release(p, mode, &rel) {
        Ok(dest) => {
            let meta = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            spinner.finish(&format!(
                "built {} ({mode_str}, {})",
                dest.display(),
                crate::ui::human_bytes(meta)
            ));
            dest
        }
        Err(msg) => {
            drop(spinner);
            return Err(msg);
        }
    };
    println!("built {}", dest.display());
    Ok(())
}

/// `zz build --release --full <file.zz> -- <program args>`: max
/// optimization with PGO — instrument, train, merge, rebuild optimized
/// with full LTO. Mirrors the `zz profile` phases below (same
/// training-run contract and `default.profdata` handling) but lands
/// `FullPgo` output instead of plain `PgoUse`.
fn build_full_with_training(
    path: &str,
    rel: &build::ReleaseOptions,
    train_args: &[String],
) -> Result<(), String> {
    // Fail fast: merging needs llvm-profdata, and there is no point
    // spending a full instrumented build without it.
    if std::process::Command::new("llvm-profdata")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return Err("llvm-profdata not found\n\
            hint: install LLVM tools (apt: llvm, brew: llvm) to use `--full -- <args>`"
            .to_string());
    }
    let p = std::path::Path::new(path);
    crate::enforce_project_zz(p)?;

    crate::ui::header(&format!("building {path} (full+profile)"));
    crate::ui::step(1, 4, "Instrumented build");
    let spinner = crate::ui::Spinner::start("Compiling (instrumented)");
    let instrumented = match build::build_release(p, build::BuildMode::Pgo, rel) {
        Ok(bin) => {
            spinner.finish("instrumented build done");
            bin
        }
        Err(e) => {
            drop(spinner);
            return Err(e);
        }
    };

    crate::ui::step(2, 4, "Training run");
    let prof_dir = std::env::temp_dir().join(format!("zz-full-profile-{}", std::process::id()));
    if prof_dir.exists() {
        let _ = std::fs::remove_dir_all(&prof_dir);
    }
    std::fs::create_dir_all(&prof_dir).map_err(|e| format!("cannot create profile dir: {e}"))?;
    let profraw = prof_dir.join("zz.profraw");
    let status = std::process::Command::new(&instrumented)
        .args(train_args)
        .env("LLVM_PROFILE_FILE", &profraw)
        .status()
        .map_err(|e| format!("cannot run training binary: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&prof_dir);
        return Err(format!(
            "training run failed (exit {})\n\
              hint: the workload must succeed for profile data to be valid",
            status.code().unwrap_or(-1)
        ));
    }

    crate::ui::step(3, 4, "Merging profile");
    let mut raw_files: Vec<std::path::PathBuf> = std::fs::read_dir(&prof_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("profraw"))
                .collect()
        })
        .unwrap_or_default();
    raw_files.sort();
    if raw_files.is_empty() {
        let _ = std::fs::remove_dir_all(&prof_dir);
        return Err("no profile data collected\n\
            hint: the training run must execute instrumented code (check its args)"
            .to_string());
    }
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let profdata = cwd.join("default.profdata");
    let merge = std::process::Command::new("llvm-profdata")
        .arg("merge")
        .arg("-o")
        .arg(&profdata)
        .args(&raw_files)
        .output()
        .map_err(|e| format!("cannot run llvm-profdata: {e}"))?;
    let _ = std::fs::remove_dir_all(&prof_dir);
    if !merge.status.success() {
        return Err(format!(
            "llvm-profdata merge failed: {}\n\
              hint: inspect {} and retry",
            String::from_utf8_lossy(&merge.stderr).trim(),
            profdata.display()
        ));
    }

    crate::ui::step(4, 4, "Optimized build (full LTO + PGO)");
    let spinner = crate::ui::Spinner::start("Compiling (full+profile)");
    let dest = match build::build_release(p, build::BuildMode::FullPgo, rel) {
        Ok(dest) => {
            let meta = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            spinner.finish(&format!(
                "built {} (full, {})",
                dest.display(),
                crate::ui::human_bytes(meta)
            ));
            dest
        }
        Err(e) => {
            drop(spinner);
            return Err(format!(
                "{e}\nhint: {} left in place — fix and re-run to retry phase 2",
                profdata.display()
            ));
        }
    };
    let _ = std::fs::remove_file(&profdata);
    println!("built {}", dest.display());
    Ok(())
}

/// Split `profile` args at `--`: `(flag side, training args)`.
fn split_train_args(args: &[String]) -> (&[String], Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], args.get(i + 1..).unwrap_or(&[]).to_vec()),
        None => (args, Vec::new()),
    }
}

/// `zz profile [<file.zz>] [-- args]`: PGO end to end.
///
/// Phase 1 instruments (`-fprofile-generate`), the training run executes
/// with the given args, `llvm-profdata` merges coverage, and phase 2
/// rebuilds optimized (`-fprofile-use`). Profile files are cleaned up on
/// success; on failure they are left in place with a hint.
fn profile_cmd(args: &[String]) -> Result<(), String> {
    let (left, train_args) = split_train_args(args);
    let path: String = match left.iter().find(|a| !a.starts_with('-')).cloned() {
        Some(p) => p,
        None => resolve_default_entry(
            "zz profile [<file.zz>] [-- args]",
            "provide the path to a .zz file to profile (or run inside a project)",
        )?,
    };
    if left.iter().any(|a| a.starts_with('-')) {
        return Err("zz profile takes no build flags\n\
            hint: instrument + optimize modes are fixed; use `zz build` for custom flags"
            .to_string());
    }
    // Fail fast: merging needs llvm-profdata, and there is no point
    // spending a full instrumented build without it.
    if std::process::Command::new("llvm-profdata")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return Err("llvm-profdata not found\n\
            hint: install LLVM tools (apt: llvm, brew: llvm) to use `zz profile`"
            .to_string());
    }
    let p = std::path::Path::new(path.as_str());
    let rel = build::ReleaseOptions::default();

    crate::ui::header(&format!("profiling {path}"));
    crate::ui::step(1, 4, "Instrumented build");
    let spinner = crate::ui::Spinner::start("Compiling (instrumented)");
    let instrumented = match build::build_release(p, build::BuildMode::Pgo, &rel) {
        Ok(bin) => {
            spinner.finish("instrumented build done");
            bin
        }
        Err(e) => {
            drop(spinner);
            return Err(e);
        }
    };

    crate::ui::step(2, 4, "Training run");
    let prof_dir = std::env::temp_dir().join(format!("zz-profile-{}", std::process::id()));
    if prof_dir.exists() {
        let _ = std::fs::remove_dir_all(&prof_dir);
    }
    std::fs::create_dir_all(&prof_dir).map_err(|e| format!("cannot create profile dir: {e}"))?;
    let profraw = prof_dir.join("zz.profraw");
    let status = std::process::Command::new(&instrumented)
        .args(&train_args)
        .env("LLVM_PROFILE_FILE", &profraw)
        .status()
        .map_err(|e| format!("cannot run training binary: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&prof_dir);
        return Err(format!(
            "training run failed (exit {})\n\
              hint: the workload must succeed for profile data to be valid",
            status.code().unwrap_or(-1)
        ));
    }

    crate::ui::step(3, 4, "Merging profile");
    let mut raw_files: Vec<std::path::PathBuf> = std::fs::read_dir(&prof_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("profraw"))
                .collect()
        })
        .unwrap_or_default();
    raw_files.sort();
    if raw_files.is_empty() {
        let _ = std::fs::remove_dir_all(&prof_dir);
        return Err("no profile data collected\n\
            hint: the training run must execute instrumented code (check its args)"
            .to_string());
    }
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {e}"))?;
    let profdata = cwd.join("default.profdata");
    let merge = std::process::Command::new("llvm-profdata")
        .arg("merge")
        .arg("-o")
        .arg(&profdata)
        .args(&raw_files)
        .output()
        .map_err(|e| format!("cannot run llvm-profdata: {e}"))?;
    let _ = std::fs::remove_dir_all(&prof_dir);
    if !merge.status.success() {
        return Err(format!(
            "llvm-profdata merge failed: {}\n\
              hint: inspect {} and retry",
            String::from_utf8_lossy(&merge.stderr).trim(),
            profdata.display()
        ));
    }

    crate::ui::step(4, 4, "Optimized build");
    let spinner = crate::ui::Spinner::start("Compiling (optimized)");
    let dest = match build::build_release(p, build::BuildMode::PgoUse, &rel) {
        Ok(dest) => {
            let meta = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            spinner.finish(&format!(
                "built {} (pgo, {})",
                dest.display(),
                crate::ui::human_bytes(meta)
            ));
            dest
        }
        Err(e) => {
            drop(spinner);
            return Err(format!(
                "{e}\nhint: {} left in place — fix and re-run to retry phase 2",
                profdata.display()
            ));
        }
    };
    let _ = std::fs::remove_file(&profdata);
    println!("built {}", dest.display());
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

/// Strip `-o <name>` / `--output <name>` (and `=` forms): `run --native`
/// consumes the output flag itself, so the script and the positional
/// file search must never see it or its value.
fn strip_output_flag(args: &[String]) -> Vec<String> {
    const FLAGS: [&str; 2] = ["-o", "--output"];
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if FLAGS.contains(&a.as_str()) {
            skip_next = true;
            continue;
        }
        if FLAGS.iter().any(|f| a.starts_with(&format!("{f}="))) {
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
    show_stats: bool,
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
    // --stats accumulators (#248).
    let mut stat_files = 0usize;
    let mut stat_modules = 0usize;
    let mut stat_hits = 0usize;
    let mut stat_misses = 0usize;
    let mut stat_seed = 0usize;
    let stats_start = std::time::Instant::now();

    for path in &files {
        let path_str = path.display().to_string();
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path_str}`: {e}"))?;

        let file_start = std::time::Instant::now();
        let loaded = loader::load_program_check(path)?;
        let file_ms = file_start.elapsed().as_secs_f64() * 1000.0;
        if show_stats {
            stat_files += 1;
            stat_modules += loaded.stats.modules;
            stat_hits += loaded.stats.cache_hits;
            stat_misses += loaded.stats.cache_misses;
            stat_seed = stat_seed.max(loaded.stats.seed_funcs);
            eprintln!(
                "stats: {path_str}: {} modules ({} cached, {} checked), seed {} funcs, {file_ms:.1}ms",
                loaded.stats.modules,
                loaded.stats.cache_hits,
                loaded.stats.cache_misses,
                loaded.stats.seed_funcs,
            );
        }

        // Classify fixits by safety.
        let mut safe_fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
        let mut ambiguous_fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
        // Any error-severity diagnostic fails the check (#292): fixable
        // errors are still errors (warnings alone stay exit 0).
        let mut has_errors = false;

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
                        if d.severity == Severity::Error {
                            has_errors = true;
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
            if has_errors {
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
    if show_stats {
        let total_ms = stats_start.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "stats: {stat_files} files, {stat_modules} modules ({} cached, {} checked), seed {stat_seed} funcs, {total_ms:.1}ms total",
            stat_hits, stat_misses,
        );
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

    /// Run `zz check` (check-only) under the isolated cache env: sharing the
    /// process-global cache env with the loader's cache-counting tests
    /// otherwise pollutes their entry counts mid-flight.
    fn check_isolated(path: &str) -> Result<(), String> {
        super::loader::cache::with_isolated_cache(|| {
            check_or_fix_path(&Some(path.to_string()), false, false, false, false)
        })
    }

    #[test]
    fn check_ok_on_valid_file() {
        let path = write_temp("x := 1 + 2\nprintln(x)\n");
        let result = check_isolated(&path.to_string_lossy());
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_ok_on_phase4_features() {
        let path = write_temp(
            "scores := [10, 20, 30]\ny := scores[1]\nz := scores[1:3]\nfunc dbl(a: int, b: int) -> int { a * b }\nw := 5 |> dbl(3)\nt := typeof(w)\n",
        );
        let result = check_isolated(&path.to_string_lossy());
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_type_error() {
        let path = write_temp("x := 1 + \"a\"\n");
        let result = check_isolated(&path.to_string_lossy());
        assert!(result.is_err(), "expected type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_index_error() {
        let path = write_temp("x := 5\nx[0]\n");
        let result = check_isolated(&path.to_string_lossy());
        assert!(result.is_err(), "expected index type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_fixable_error() {
        // #292: an error carrying auto-fix suggestions (typo fixit) must
        // still fail `zz check` — only warnings exit 0.
        let path = write_temp("func main() {\n    count := 1\n    println(cout)\n}\n");
        let result = check_isolated(&path.to_string_lossy());
        assert!(result.is_err(), "expected fixable typo error to fail check");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_ok_on_warnings_only() {
        // #292: warnings alone (unused variable) must stay exit 0.
        let path = write_temp("func main() {\n    unused_xyz := 1\n}\n");
        let result = check_isolated(&path.to_string_lossy());
        assert!(
            result.is_ok(),
            "expected warnings-only to pass, got {result:?}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_missing_file_errors() {
        let result = check_isolated("/tmp/zz_no_such_file_zz.zz");
        assert!(result.is_err(), "expected error for missing file");
    }
    #[test]
    fn check_400_fn_file_stays_fast() {
        // Perf smoke guard (#248): 400 single-function definitions must
        // check in seconds, not minutes. The bound is deliberately generous
        // (100x the measured ~0.06s) — it catches catastrophic slowdowns
        // (e.g. quadratic seed handling), not 20% wobbles.
        let mut src = String::new();
        for i in 0..400 {
            src.push_str(&format!(
                "func zz_perf_fn_{i}(x: int) -> int {{ x + {i} }}\n"
            ));
        }
        src.push_str("func main() {\n    println(zz_perf_fn_0(1))\n}\n");
        let path = write_temp(&src);
        let start = std::time::Instant::now();
        let result = check_isolated(&path.to_string_lossy());
        let elapsed = start.elapsed();
        let _ = fs::remove_file(&path);
        assert!(result.is_ok(), "expected ok, got {result:?}");
        assert!(
            elapsed.as_secs() < 10,
            "400-fn check took {elapsed:?}, expected < 10s"
        );
    }
    #[test]
    fn check_no_arg_errors() {
        // NOTE: `examples/` is gitignored (personal scratch dir, absent in
        // CI checkouts) — scan the tracked fixtures instead. The point is
        // directory discovery finds files and does not panic/IO-error.
        let fixtures_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        assert!(fixtures_dir.is_dir(), "tests/fixtures dir should exist");
        let result = check_isolated(&fixtures_dir.display().to_string());
        // The function may fail type-check on fixtures; the point is it
        // should find files and not panic/IO-error.
        match &result {
            Err(msg) if msg.contains("does not exist") || msg.contains("no .zz files") => {
                panic!("scan should find files: {msg}");
            }
            _ => {} // either Ok or type-check errors — both prove scanning worked.
        }
    }
}

#[cfg(test)]
mod profile_tests {
    use super::split_train_args;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_training_args() {
        let a = args(&["app.zz", "--", "input.txt", "--fast"]);
        let (left, train) = split_train_args(&a);
        assert_eq!(left, &["app.zz".to_string()]);
        assert_eq!(train, vec!["input.txt".to_string(), "--fast".to_string()]);
    }

    #[test]
    fn no_separator_means_no_training_args() {
        let a = args(&["app.zz"]);
        let (left, train) = split_train_args(&a);
        assert_eq!(left, &["app.zz".to_string()]);
        assert!(train.is_empty());
    }

    #[test]
    fn trailing_separator_is_empty() {
        let a = args(&["app.zz", "--"]);
        let (left, train) = split_train_args(&a);
        assert_eq!(left, &["app.zz".to_string()]);
        assert!(train.is_empty());
    }
}
