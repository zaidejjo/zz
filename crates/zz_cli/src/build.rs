//! Native AOT build / transient-run integration for the `zz` CLI.
//!
//! Single-backend model (Clang-only), always a native binary:
//! `zz build <file>`            — debug: native Clang `-O0 -g` (fast, no LTO).
//! `zz build -p <file>`         — release: native Clang `-O3 -flto=thin`.
//! `zz build --static`          — static self-contained (ThinLTO, DCE).
//! `zz build --pgo`             — PGO instrumented build (native host only).
//! `zz build --target <triple>` — cross build via `clang --target=`
//!                                (same flags, minus `-march=native`).
//! `zz run --native`            — transient release compile → exec → cleanup.
//!
//! (`zz run` without `--native` is the only VM path.)
//!
//! All artifacts live under `bin/` next to the source file. Release binaries
//! are cached under `~/.zz/cache` keyed by source-hash + build options +
//! target triple, so unchanged files rebuild instantaneously.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use zz_codegen::{BuildOptions, ClangProvider};
use zz_frontend::span::Span;
use zz_hir::TypedProgram;

use crate::loader;

/// Build mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Dev,
    Release,
    Static,
    Pgo,
}

/// Release-build knobs: cross target, provider selection, verbosity.
#[derive(Debug, Clone, Default)]
pub struct ReleaseOptions {
    /// `--target=<triple>` cross triple, or `None` for a native host build.
    pub target: Option<String>,
    /// `--cc=` provider preference (clang vs `zig cc`).
    pub provider: ClangProvider,
    /// `--verbose`: print the exact clang command line.
    pub verbose: bool,
}

impl ReleaseOptions {
    /// Target as `Option<&str>` for the codegen API.
    pub fn target_opt(&self) -> Option<&str> {
        self.target.as_deref()
    }
}

/// The cache directory (`~/.zz/cache`).
pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".zz").join("cache")
}

/// Compute a cache key from source + build options + target triple.
/// Uses BuildOptions fingerprint (target-aware) + runtime file mtimes for
/// automatic cache invalidation.
fn cache_key(src: &str, opts: BuildOptions, target: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut hasher);
    opts.fingerprint_with(target).hash(&mut hasher);
    // Include runtime file mtimes for automatic cache invalidation
    if let Some(runtime_mtime) = runtime_mtime() {
        runtime_mtime.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Get modification time of every input that affects native output, for
/// cache invalidation: the C runtime + codegen (`zz_codegen`), the Rust
/// native runtime linked into FFI builds (`zz_native_rt`), the pure-ZZ
/// stdlib sources merged into every build (`zz_stdlib/zz/`), and this
/// build module itself.
fn runtime_mtime() -> Option<u64> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../");
    // Crate dirs and the subdirs to watch inside them (one level deep).
    let watched: &[(&str, &[&str])] = &[
        ("zz_codegen", &["src/lower", "src/runtime", "src"]),
        ("zz_native_rt", &["src"]),
        ("zz_stdlib", &["zz"]),
        ("zz_cli", &["src"]),
    ];
    let mut mtimes: Vec<u64> = Vec::new();
    for (krate, dirs) in watched {
        for dir in *dirs {
            let dir = crates.join(krate).join(dir);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Recurse one level (covers `zz_stdlib/zz/<mod>/` and
                // `zz_codegen/src/` files alongside the subdirs above).
                let candidates = if path.is_dir() {
                    std::fs::read_dir(&path)
                        .map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
                        .unwrap_or_default()
                } else {
                    vec![path]
                };
                for cand in candidates {
                    if let Ok(meta) = std::fs::metadata(&cand) {
                        if meta.is_file() {
                            if let Ok(m) = meta.modified() {
                                if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                                    mtimes.push(d.as_secs());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if mtimes.is_empty() {
        return None;
    }
    // Combine order-independently: sum of all file mtimes.
    Some(mtimes.iter().fold(0u64, |acc, m| acc.wrapping_add(*m)))
}

fn opts_for(mode: BuildMode) -> BuildOptions {
    match mode {
        BuildMode::Dev => BuildOptions::dev(),
        BuildMode::Release => BuildOptions::release(),
        BuildMode::Static => BuildOptions::static_lto(),
        BuildMode::Pgo => BuildOptions::pgo_generate(),
    }
}

/// Type-check + DCE all modules to a typed program, entry-main name, and
/// reachable set.
fn typed_program_for(
    path: &Path,
    entry_ns: &str,
) -> Result<(TypedProgram, zz_hir::ReachableSet, String), String> {
    let loaded = loader::load_program(path)?;
    let mut has_errors = false;
    for e in &loaded.errors {
        let mut files = zz_frontend::diag::Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!(
            "{}",
            zz_frontend::diag::render_to_string(&files, id, &e.diags)
        );
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            has_errors = true;
        }
    }
    if has_errors {
        return Err("program failed to type-check".into());
    }

    // Merge all module programs into one for HIR building / DCE. Modules
    // are namespaced by the loader (entry = file stem), so concat is safe.
    // Pure-ZZ stdlib sources come first (mirroring the VM, which executes
    // them before user code): without them AOT binaries silently lower
    // pure-ZZ helpers like `str.repeat` to unit. They contain only function
    // definitions, so merging cannot introduce top-level side effects.
    let mut merged_stmts = Vec::new();
    for zz_prog in zz_stdlib::zz_stdlib_programs() {
        merged_stmts.extend(zz_prog.program.stmts.iter().cloned());
    }
    let merged_span = loaded
        .programs
        .last()
        .map(|p| p.span)
        .unwrap_or(Span::new(0, 0));
    for p in &loaded.programs {
        merged_stmts.extend(p.stmts.iter().cloned());
    }
    let merged = zz_frontend::ast::Program {
        stmts: merged_stmts,
        span: merged_span,
    };

    let res = zz_hir::build_program(
        &merged,
        HashMap::new(),
        loaded.funcs.clone(),
        loaded.structs.clone(),
    );
    if !res.diagnostics.is_empty() {
        for d in &res.diagnostics {
            if d.severity == zz_frontend::diag::Severity::Error {
                eprintln!("zz: {}", d.message);
            }
        }
    }
    let main_key = format!("{entry_ns}.main");
    let (pruned, reach) = zz_hir::dce(&res.program, &main_key);
    Ok((pruned, reach, main_key))
}

/// Directory holding build artifacts: `bin/` next to the source file
/// (or `<cwd>/bin` when the source has no parent).
pub fn bin_dir_for(src: &Path) -> PathBuf {
    src.parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from("bin")
            } else {
                p.join("bin")
            }
        })
        .unwrap_or_else(|| PathBuf::from("bin"))
}

/// Output binary name: `<stem>`, `<stem>-<triple>` for cross builds,
/// plus `.exe` for Windows hosts/targets. Uses path components only —
/// never string-concatenated separators.
pub fn bin_name(stem: &str, target: Option<&str>) -> String {
    let mut name = stem.to_string();
    if let Some(t) = target {
        name.push('-');
        name.push_str(t);
    }
    let windows = match target {
        Some(t) => zz_codegen::is_windows_target(t),
        None => cfg!(windows),
    };
    if windows {
        name.push_str(".exe");
    }
    name
}

/// True when `p` is a file that can be executed: present, non-empty, and
/// (on unix) carrying at least one execute bit. Protects the cache from
/// stale artifacts left by interrupted builds, which clang may leave as
/// a complete-but-non-executable (0644) file.
fn is_usable_cache_binary(p: &Path) -> bool {
    let Ok(meta) = p.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Native Clang build (cached), published to `bin/` next to the source.
/// Returns the output binary path. `Dev` mode compiles with `-O0 -g`
/// (fast debug binary); `Release`/`Static`/`Pgo` use their option sets.
///
/// When no Clang provider is installed, the generated C + build scripts
/// are still emitted to `bin/` before the error is returned, so the user
/// can build manually on a machine with Clang.
pub fn build_release(
    path: &Path,
    mode: BuildMode,
    rel: &ReleaseOptions,
) -> Result<PathBuf, String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pruned, reach, main_key) = typed_program_for(path, &entry_ns)?;
    let opts = opts_for(mode);
    let target = rel.target_opt();

    // Early validation: exact CLI-contract errors for PGO-cross and
    // static-macOS, before any cache or toolchain work.
    if let Err(e) = zz_codegen::validate(&opts, target) {
        return Err(e.to_string());
    }

    // Resolve the provider now so a missing toolchain fails fast — but
    // still leave bin/app.c + scripts behind for manual builds.
    let clang = match zz_codegen::detect_clang_with(rel.provider) {
        Some(c) => c,
        None => {
            let lowered = zz_codegen::lower_only(&pruned, &reach, &main_key);
            let dir = bin_dir_for(path);
            let _ = zz_codegen::emit_c_plus_script(&lowered.source, &dir, target, &opts);
            return Err(zz_codegen::BuildError::NoClang.to_string());
        }
    };
    if rel.verbose {
        eprintln!(
            "zz: {} {}",
            clang.label,
            zz_codegen::compile::clang_flags(&opts, target).join(" ")
        );
    }

    // Cache: reuse when the same source + build options + target were
    // built before.
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create cache: {e}"))?;
    let source = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let key = cache_key(&source, opts, target);
    let target_slug = target.unwrap_or("host");
    let cached = dir.join(format!("{key}-{mode:?}-{target_slug}"));

    if is_usable_cache_binary(&cached) {
        // Reuse the cached binary.
        return publish_to_bin(&cached, path, target);
    }
    // Stale artifact (interrupted build, missing exec bit, empty file):
    // drop it so the fresh build below replaces it.
    let _ = std::fs::remove_file(&cached);

    // Build to a unique temp path in the same directory, then atomically
    // rename into place. Concurrent builds of the same key (parallel tests,
    // parallel `zz` invocations) each produce a complete, executable file;
    // observers never see a partially-written or non-executable binary.
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        "{key}-{mode:?}-{target_slug}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    if let Err(e) =
        zz_codegen::build_native_with(&pruned, &reach, &main_key, opts, target, &clang, &tmp)
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    // Some compilers create the output without the exec bit when writing a
    // fresh file; force it so the rename only ever publishes runnable code.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let res = std::fs::metadata(&tmp)
            .and_then(|m| {
                let mut perms = m.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(&tmp, perms)
            })
            .map_err(|e| format!("cannot set exec bit: {e}"));
        if let Err(e) = res {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    if let Err(e) = std::fs::rename(&tmp, &cached) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot publish cache entry: {e}"));
    }
    publish_to_bin(&cached, path, target)
}

/// Copy a cached binary into `bin/` next to the source with the
/// target-aware name. Returns the `bin/` path.
fn publish_to_bin(cached: &Path, src: &Path, target: Option<&str>) -> Result<PathBuf, String> {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".to_string());
    let dir = bin_dir_for(src);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create bin dir: {e}"))?;
    let dest = dir.join(bin_name(&stem, target));
    std::fs::copy(cached, &dest).map_err(|e| format!("cannot write binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dest) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(&dest, perms);
        }
    }
    Ok(dest)
}

/// Build a native binary for `path` in release `mode` with default options.
/// Convenience wrapper over [`build_release`] (native host, auto provider).
/// Returns the `bin/` output binary path.
pub fn build_native(path: &Path, mode: BuildMode) -> Result<PathBuf, String> {
    build_release(path, mode, &ReleaseOptions::default())
}

/// Transient: compile to a temp path (release opts), return (binary path,
/// cleanup fn). Caller must invoke the closure to remove the artifact.
#[allow(dead_code, clippy::type_complexity)]
pub fn transient_build(path: &Path) -> Result<(PathBuf, Box<dyn FnOnce()>), String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pruned, reach, main_key) = typed_program_for(path, &entry_ns)?;

    let tmp = std::env::temp_dir().join(format!("zz-native-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create tmp: {e}"))?;
    let bin = tmp.join("zz_out");
    zz_codegen::build_native(
        &pruned,
        &reach,
        &main_key,
        BuildOptions::release(),
        None,
        &bin,
    )
    .map_err(|e| format!("{e}"))?;

    let tmp_for_cleanup = tmp.clone();
    let cleanup = Box::new(move || {
        let _ = std::fs::remove_dir_all(&tmp_for_cleanup);
    });
    Ok((bin, cleanup))
}

/// Execute a binary, forwarding args; waits for completion.
pub fn exec_binary(bin: &Path, script_args: &[String]) -> Result<i32, String> {
    let status = Command::new(bin)
        .args(script_args)
        .stdin(Stdio::inherit())
        .status()
        .map_err(|e| format!("cannot run binary: {e}"))?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_naming_host_and_cross() {
        // Host naming follows the cfg target (this suite runs on unix).
        let host = bin_name("app", None);
        assert_eq!(host, if cfg!(windows) { "app.exe" } else { "app" });
        // Cross builds tag the triple; Windows triples add .exe.
        assert_eq!(
            bin_name("app", Some("aarch64-unknown-linux-gnu")),
            "app-aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            bin_name("app", Some("x86_64-pc-windows-gnu")),
            "app-x86_64-pc-windows-gnu.exe"
        );
        assert_eq!(
            bin_name("demo", Some("x86_64-apple-darwin")),
            "demo-x86_64-apple-darwin"
        );
    }

    #[test]
    fn bin_dir_is_pathbuf_joined() {
        // Never string-concatenated separators: always parent + "bin".
        assert_eq!(
            bin_dir_for(Path::new("src/main.zz")),
            PathBuf::from("src/bin")
        );
        assert_eq!(bin_dir_for(Path::new("main.zz")), PathBuf::from("bin"));
    }
}
