//! Native AOT build / transient-run integration for the `zz` CLI.
//!
//! `zz build <file>`        — dev build (-O1, dynamic)
//! `zz build -p <file>`     — release build (-O3 -flto, dynamic)
//! `zz build --static`      — static self-contained (ThinLTO, DCE)
//! `zz build --pgo`         — PGO instrumented build
//! `zz run --native`        — transient compile → exec → cleanup
//!
//! Binaries are cached under `~/.zz/cache` keyed by source-hash + build
//! options, so unchanged files rebuild instantaneously.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use zz_codegen::BuildOptions;
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

/// The cache directory (`~/.zz/cache`).
pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".zz").join("cache")
}

/// Compute a cache key from source + build options.
/// Uses BuildOptions fingerprint + runtime file mtimes for automatic cache invalidation.
fn cache_key(src: &str, opts: BuildOptions) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut hasher);
    opts.fingerprint().hash(&mut hasher);
    // Include runtime file mtimes for automatic cache invalidation
    if let Some(runtime_mtime) = runtime_mtime() {
        runtime_mtime.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Get modification time of the C runtime + codegen files for cache invalidation.
/// The runtime and codegen live in the zz_codegen crate, one level up from zz_cli.
fn runtime_mtime() -> Option<u64> {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../zz_codegen");
    let rt_c = base.join("src/runtime.c");
    let rt_h = base.join("src/runtime.h");
    let lower_rs = base.join("src/lower.rs");
    let c_mtime = rt_c.metadata().and_then(|m| m.modified()).ok();
    let h_mtime = rt_h.metadata().and_then(|m| m.modified()).ok();
    let lr_mtime = lower_rs.metadata().and_then(|m| m.modified()).ok();
    match (c_mtime, h_mtime, lr_mtime) {
        (Some(ct), Some(ht), Some(lr)) => {
            let ct_sys = ct
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let ht_sys = ht
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let lr_sys = lr
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            ct_sys.and_then(|c| {
                ht_sys.and_then(|h| lr_sys.map(|lr| c.wrapping_mul(h).wrapping_add(lr)))
            })
        }
        _ => None,
    }
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
    let mut merged_stmts = Vec::new();
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

/// Build a native binary for `path`. Returns the output binary path.
pub fn build_native(path: &Path, mode: BuildMode) -> Result<PathBuf, String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pruned, reach, main_key) = typed_program_for(path, &entry_ns)?;

    // Cache: reuse when the same source + build options were built before.
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create cache: {e}"))?;
    let source = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let opts = opts_for(mode);
    let key = cache_key(&source, opts);
    let cached = dir.join(format!("{key}-{mode:?}"));

    if is_usable_cache_binary(&cached) {
        // Reuse the cached binary.
        return Ok(cached);
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
        "{key}-{mode:?}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    if let Err(e) = zz_codegen::build_native(&pruned, &reach, &main_key, opts, &tmp) {
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
    Ok(cached)
}

/// Transient: compile to a temp path, return (binary path, cleanup fn).
/// Caller must invoke the closure to remove the artifact.
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
    zz_codegen::build_native(&pruned, &reach, &main_key, BuildOptions::dev(), &bin)
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
