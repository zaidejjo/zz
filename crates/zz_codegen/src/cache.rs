//! Precompiled C runtime archive cache.
//!
//! The C runtime (`RUNTIME_C`) is ~5 800 lines of C that gets recompiled on
//! every `zz build` / `zz run --native`. This module compiles it once into a
//! static library (`libzz_rt.a`) keyed by:
//!
//!   `{target}-{mode}-{src_hash}-{clang_id}`
//!
//! Subsequent builds reuse the cached archive in ~0ms. The archive is
//! invalidated when:
//! - the target triple changes (cross-compilation),
//! - the optimization mode changes (debug vs release, which gates LTO),
//! - any runtime `.c` or `.h` source file changes (detected via hash),
//! - the Clang binary or its version changes (bitcode compatibility).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compile::{BuildError, BuildOptions, Clang};

/// Cache directory: `$HOME/.zz/cache/`.
fn cache_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("zz"))
        .join(".zz")
        .join("cache")
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
}

/// SHA256-like fingerprint of the C runtime sources.
///
/// Concatenates `RUNTIME_H` + `RUNTIME_C` and hashes them. This covers all
/// `.c` and `.h` files that are embedded via `include_str!` in `lib.rs`.
fn runtime_src_hash() -> String {
    let combined = format!("{}\n{}", crate::RUNTIME_H, crate::RUNTIME_C);
    let mut hasher = DefaultHasher::new();
    combined.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Fingerprint of the Clang provider (path + `--version` output).
///
/// Ensures that switching Clang versions or providers (system clang → zig cc)
/// invalidates stale archives with potentially incompatible bitcode.
fn clang_id(clang: &Clang) -> String {
    let mut hasher = DefaultHasher::new();
    clang.path.to_string_lossy().hash(&mut hasher);
    // Probe the version (fast, cached by OS).
    if let Ok(out) = Command::new(&clang.path).arg("--version").output() {
        out.stdout.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Assemble the 4-field cache key.
///
/// Format: `{target}-{mode}-{src_hash}-{clang_id}`
pub fn cache_key(target: Option<&str>, optimize: bool, clang: &Clang) -> String {
    let fallback_triple = crate::compile::host_triple();
    let triple = target.unwrap_or(&fallback_triple);
    let mode = if optimize { "rel" } else { "dev" };
    format!(
        "{}-{}-{}-{}",
        triple,
        mode,
        runtime_src_hash(),
        clang_id(clang)
    )
}

/// Return the path to the cached C runtime archive for the given build
/// configuration, compiling it if necessary.
///
/// The archive contains the C runtime objects (core.c, strings.c,
/// collections.c, json.c, memory.c) compiled with flags matching the
/// caller's `BuildOptions`. Cross-compilation correctness is guaranteed
/// because the cache key includes the target triple and the compilation
/// command includes `--target=<triple>` when applicable.
pub fn ensure_rt_a(
    opts: &BuildOptions,
    clang: &Clang,
    target: Option<&str>,
) -> Result<PathBuf, BuildError> {
    // PGO builds are non-reusable — skip the cache entirely.
    if opts.pgo != crate::compile::PgoMode::None {
        return compile_rt_a(opts, clang, target, None);
    }

    let key = cache_key(target, opts.optimize, clang);
    // Per-key subdirectory with a fixed archive name — allows `-lzz_rt`.
    let dir = cache_dir().join(&key);
    std::fs::create_dir_all(&dir).map_err(BuildError::Io)?;

    let path = dir.join("libzz_rt.a");

    if path.exists() {
        return Ok(path);
    }

    compile_rt_a(opts, clang, target, Some(&path))
}

/// Compile the C runtime into a static library and optionally save to `dest`.
///
/// The runtime TU is assembled identically to the current single-TU approach:
/// `RUNTIME_H` + `RUNTIME_C` concatenated, with quoted includes stripped.
fn compile_rt_a(
    opts: &BuildOptions,
    clang: &Clang,
    target: Option<&str>,
    dest: Option<&Path>,
) -> Result<PathBuf, BuildError> {
    let key = cache_key(target, opts.optimize, clang);

    // Assemble the runtime TU (same as the current single-TU approach).
    let raw = format!("{}\n{}", crate::RUNTIME_H, crate::RUNTIME_C);
    let src = crate::lower::strip_quoted_includes(&raw);

    let tmpbase = std::env::temp_dir().join(format!(
        "zz-rt-{}-{}",
        std::process::id(),
        &key[..16.min(key.len())]
    ));
    let c_path = tmpbase.with_extension("c");
    let o_path = tmpbase.with_extension("o");

    std::fs::write(&c_path, &src).map_err(BuildError::Io)?;

    // Build the compilation flags (subset of clang_flags — compile-only).
    let compile_flags = build_compile_flags(opts, target);

    let mut cmd = Command::new(&clang.path);
    if clang.zig {
        cmd.arg("cc");
        if let Some(t) = target {
            cmd.arg("-target").arg(t);
        }
    }
    for flag in &compile_flags {
        // For zig the target travels via `-target`, not `--target=`.
        if clang.zig && flag.starts_with("--target=") {
            continue;
        }
        cmd.arg(flag);
    }
    cmd.arg("-DZZ_HAS_SQLITE3");
    cmd.arg("-c");
    cmd.arg(&c_path);
    cmd.arg("-o");
    cmd.arg(&o_path);

    let out = cmd.output().map_err(BuildError::Io)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let _ = std::fs::remove_file(&c_path);
        let _ = std::fs::remove_file(&o_path);
        return Err(BuildError::CompileFailed { stderr });
    }

    // Archive into .a
    let default_dir = cache_dir().join(&key);
    let default_path = default_dir.join("libzz_rt.a");
    let archive_path = dest.unwrap_or(&default_path);
    let ar_out = Command::new("ar")
        .args([
            "rcs",
            archive_path.to_str().unwrap(),
            o_path.to_str().unwrap(),
        ])
        .output()
        .map_err(BuildError::Io)?;
    if !ar_out.status.success() {
        let _ = std::fs::remove_file(&c_path);
        let _ = std::fs::remove_file(&o_path);
        return Err(BuildError::CompileFailed {
            stderr: String::from_utf8_lossy(&ar_out.stderr).into_owned(),
        });
    }

    // Cleanup temporaries.
    let _ = std::fs::remove_file(&c_path);
    let _ = std::fs::remove_file(&o_path);

    Ok(archive_path.to_path_buf())
}

/// Build the compile-only flags for the C runtime archive.
///
/// These are a subset of `clang_flags()`: optimization + target flags only,
/// no link flags (`-Wl,*`, `-l*`, `-static`, `-s`).
fn build_compile_flags(opts: &BuildOptions, target: Option<&str>) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    if opts.optimize {
        flags.push("-O3".to_string());
        flags.push("-flto=thin".to_string());
        // Host-only: reads the build machine's CPU; illegal on cross targets.
        if target.is_none() {
            flags.push("-march=native".to_string());
        }
        flags.push("-ffast-math".to_string());
        flags.push("-funroll-loops".to_string());
        flags.push("-fomit-frame-pointer".to_string());
    } else {
        flags.push("-O0".to_string());
        flags.push("-g".to_string());
    }
    if let Some(t) = target {
        flags.push(format!("--target={t}"));
    }
    flags
}
