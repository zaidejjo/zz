//! Native AOT build / transient-run integration for the `zz` CLI.
//!
//! `zz build <file>`     — dev build (-O1)
//! `zz build -p <file>`  — release build (-O3 -flto -static, DCE)
//! `zz run --native`     — transient compile → exec → cleanup
//!
//! Binaries are cached under `~/.zz/cache` keyed by source-hash + build
//! mode, so unchanged files rebuild instantaneously.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use zz_codegen::BuildOptions;
use zz_frontend::diag::Files;
use zz_frontend::span::Span;
use zz_hir::TypedProgram;

use crate::loader;

/// Build mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Dev,
    Release,
}

/// The cache directory (`~/.zz/cache`).
pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".zz").join("cache")
}

/// Cache schema version — bump when codegen/runtime changes invalidate old
/// binaries.
const CACHE_VERSION: u32 = 3;

/// Hash of the source + mode, used as the cache key.
fn cache_key(src: &str, mode: BuildMode) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut hasher);
    mode_hash(mode).hash(&mut hasher);
    CACHE_VERSION.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn mode_hash(mode: BuildMode) -> u8 {
    match mode {
        BuildMode::Dev => 1,
        BuildMode::Release => 2,
    }
}

fn opts_for(mode: BuildMode) -> BuildOptions {
    match mode {
        BuildMode::Dev => BuildOptions::dev(),
        BuildMode::Release => BuildOptions::release(),
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

/// Build a native binary for `path`. Returns the output binary path.
pub fn build_native(path: &Path, mode: BuildMode) -> Result<PathBuf, String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pruned, reach, main_key) = typed_program_for(path, &entry_ns)?;

    // Cache: reuse when the same source + mode was built before.
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create cache: {e}"))?;
    let source = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let key = cache_key(&source, mode);
    let cached = dir.join(if mode == BuildMode::Release {
        format!("{key}-release")
    } else {
        format!("{key}-dev")
    });

    if cached.is_file() {
        // Reuse the cached binary.
        return Ok(cached);
    }

    let opts = opts_for(mode);
    zz_codegen::build_native(&pruned, &reach, &main_key, opts, &cached)
        .map_err(|e| format!("{e}"))?;
    Ok(cached)
}

/// Transient: compile to a temp path, return (binary path, cleanup fn).
/// Caller must invoke the closure to remove the artifact.
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
        .status()
        .map_err(|e| format!("cannot run binary: {e}"))?;
    Ok(status.code().unwrap_or(-1))
}
