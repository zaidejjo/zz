//! Precompiled C runtime archive cache.
//!
//! The C runtime (`RUNTIME_C`) is ~5 800 lines of C that gets recompiled on
//! every `zz build` / `zz run --native`. This module compiles it once into a
//! static library (`libzz_rt.a`) keyed by:
//!
//!   `{target}-{mode}-{src_hash}-{clang_id}-{pin}-{rt_version}`
//!
//! `pin` is the `zz toolchain` pin (`nopin` when unmanaged).
//! Subsequent builds reuse the cached archive in ~0ms. The archive is
//! invalidated when:
//! - the target triple changes (cross-compilation),
//! - the optimization mode changes (debug vs release, which gates LTO),
//! - any runtime `.c` or `.h` source file changes (detected via hash),
//! - the Clang binary or its version changes (bitcode compatibility).
//! - the managed-toolchain pin changes (`zz toolchain use`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{BuildError, BuildOptions, Clang};

/// Cache directory: the shared build cache root (`zz_paths::paths`,
/// honoring `ZZ_HOME` like every other ZZ path).
fn cache_dir() -> PathBuf {
    zz_paths::paths::build_cache_dir()
}

/// The cache root as a display string, for error hints (the cache path is
/// user-visible: it honors `ZZ_HOME` and appears in `zz cache clean`).
pub fn cache_dir_display() -> String {
    cache_dir().display().to_string()
}

/// SHA256-like fingerprint of the C runtime sources.
///
/// Concatenates `RUNTIME_H` + `RUNTIME_C` and hashes them. This covers all
/// `.c` and `.h` files that are embedded via `include_str!` in `lib.rs`.
fn runtime_src_hash() -> String {
    let combined = format!(
        "{}\n{}",
        zz_codegen_runtime::RUNTIME_H,
        zz_codegen_runtime::RUNTIME_C
    );
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

/// Compile-flag generation for the cached runtime archive. Bump on ANY
/// change to `build_compile_flags` (the key does not hash flags).
const RT_CACHE_VERSION: &str = "rt7";

/// Assemble the 8-field cache key.
///
/// Format: `{target}-{mode}-{src_hash}-{clang_id}-{pin}-{rt_version}-{san}-{deps}`
/// `pin` is the managed-toolchain pin (`nopin` when none): switching the
/// `zz toolchain` pin must never reuse an archive built by another
/// toolchain, even when the resolved `clang_id` hash collides in theory.
/// (`clang_id` already covers provider path + version output; the pin
/// additionally keeps cache dirs human-grepable per toolchain.)
/// `san` is the requested sanitizer set (`nosan` normally): a
/// sanitizer-instrumented archive must never satisfy a plain link (or
/// vice versa).
/// `deps` records whether the runtime was compiled with curl/sqlite3
/// headers enabled and with the optional staticlib backends referenced
/// (`c1s0p0f1`): the archive embeds those branches, so a program with
/// `http.fetch` must never link a no-curl archive, and an archive that
/// references `zz_pg_*` / `zz_float_format_raw` must never satisfy a
/// program that does not link `libzz_native_rt.a` (Darwin rejects an
/// undefined `weak_import` in an executable).
pub fn cache_key(
    target: Option<&str>,
    optimize: bool,
    clang: &Clang,
    curl: bool,
    sqlite: bool,
    pg: bool,
    float_fmt: bool,
) -> String {
    let fallback_triple = crate::host_triple();
    let triple = target.unwrap_or(&fallback_triple);
    let mode = if optimize { "rel" } else { "dev" };
    let pin = crate::toolchain_pin().unwrap_or_else(|| "nopin".to_string());
    let san = crate::sanitize_list().join("+");
    let san = if san.is_empty() {
        "nosan".to_string()
    } else {
        san
    };
    let deps = format!(
        "c{}s{}p{}f{}",
        u8::from(curl),
        u8::from(sqlite),
        u8::from(pg),
        u8::from(float_fmt)
    );
    format!(
        "{}-{}-{}-{}-{}-{}-{}-{}",
        triple,
        mode,
        runtime_src_hash(),
        clang_id(clang),
        pin,
        RT_CACHE_VERSION,
        san,
        deps,
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
    if opts.pgo != crate::PgoMode::None {
        return compile_rt_a(opts, clang, target, None);
    }

    // One retry on IO errors: a transient filesystem hiccup (a cache
    // directory removed by a parallel `zz cache clean`, a momentarily
    // unwritable temp dir) must not fail a native build. Compile failures
    // are *not* retried — they are deterministic.
    match ensure_rt_a_once(opts, clang, target) {
        Err(BuildError::Io(_)) => ensure_rt_a_once(opts, clang, target),
        other => other,
    }
}

fn ensure_rt_a_once(
    opts: &BuildOptions,
    clang: &Clang,
    target: Option<&str>,
) -> Result<PathBuf, BuildError> {
    let key = cache_key(
        target,
        opts.optimize,
        clang,
        opts.curl_link,
        opts.sqlite_link,
        links_pg(opts),
        links_float_fmt(opts),
    );
    // Per-key subdirectory with a fixed archive name (a readable,
    // human-grepable layout per cache key).
    let dir = cache_dir().join(&key);
    std::fs::create_dir_all(&dir).map_err(BuildError::Io)?;

    let path = dir.join("libzz_rt.a");

    // A usable archive is never tiny: parallel builds used to race on
    // shared temp paths and publish corrupt (observed: 8-byte) archives.
    // Treat those as missing so they rebuild instead of poisoning every
    // later link with `undefined reference to main`.
    const MIN_ARCHIVE_BYTES: u64 = 1024;
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() >= MIN_ARCHIVE_BYTES {
            return Ok(path);
        }
        let _ = std::fs::remove_file(&path);
    }

    // Cross-process build lock: cold parallel `zz run --native` runs
    // (e.g. the parity suite: N tests × clang -O3 full-runtime builds on
    // a small box) would otherwise each compile the runtime at once —
    // minutes wasted and OOM-killed clang processes under load. One
    // builder wins; the rest wait, re-check, and reuse its archive.
    // Lock file (not the archive itself) so a killed builder can never
    // leave a half-written artifact behind: publishing still goes
    // through the atomic rename in `compile_rt_a`.
    let _lock = acquire_rt_lock(&dir);
    // Double-checked: someone may have published while we waited.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() >= MIN_ARCHIVE_BYTES {
            return Ok(path);
        }
        let _ = std::fs::remove_file(&path);
    }

    compile_rt_a(opts, clang, target, Some(&path))
}

/// Guard for the runtime-archive build lock; removes the lock file on
/// drop, but only when this guard created it — a guard that gave up
/// waiting must never unlink another builder's live lock. A panic or
/// kill between acquire and drop leaves the file — `acquire_rt_lock`
/// expires those by mtime.
struct RtBuildLock {
    path: std::path::PathBuf,
    owned: bool,
}

impl Drop for RtBuildLock {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Spin-acquire `<dir>/.lock` via atomic `create_new`. Cross-process
/// (each `zz run --native` is its own process, so in-process statics
/// cannot serialize this). Gives up spinning after a generous timeout
/// and proceeds to build anyway — a contended build beats a wedged one.
/// Stale locks (mtime older than the timeout window) are unlinked so a
/// killed builder cannot wedge later runs.
fn acquire_rt_lock(dir: &Path) -> RtBuildLock {
    use std::time::{Duration, Instant};
    // Cold -O3 runtime builds take ~1-3 min; budget generously, then
    // build through rather than fail the whole native leg.
    const LOCK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
    const LOCK_STALE_AFTER: Duration = Duration::from_secs(20 * 60);
    const POLL: Duration = Duration::from_millis(100);
    let path = dir.join(".lock");
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return RtBuildLock { path, owned: true },
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Reap locks from builders that died mid-build.
                let stale = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > LOCK_STALE_AFTER);
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if start.elapsed() >= LOCK_TIMEOUT {
                    // Wedged lock (clock trouble, pathological CI):
                    // build through instead of failing. Not owned, so
                    // drop never unlinks the live lock file.
                    return RtBuildLock { path, owned: false };
                }
                std::thread::sleep(POLL);
            }
            Err(_) => {
                // Lock dir missing/unwritable: build unlocked rather
                // than fail the native leg on a locking nicety.
                return RtBuildLock { path, owned: false };
            }
        }
    }
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
    let key = cache_key(
        target,
        opts.optimize,
        clang,
        opts.curl_link,
        opts.sqlite_link,
        links_pg(opts),
        links_float_fmt(opts),
    );

    // Assemble the runtime TU (same as the current single-TU approach).
    let raw = format!(
        "{}\n{}",
        zz_codegen_runtime::RUNTIME_H,
        zz_codegen_runtime::RUNTIME_C
    );
    let src = zz_codegen_runtime::strip_quoted_includes(&raw);

    // Unique temporaries per call: parallel test threads (and processes)
    // share the pid + key namespace, and identical `zz-rt-*` paths used to
    // make concurrent clang/ar invocations clobber each other into corrupt
    // (e.g. 8-byte) archives. The counter + thread id disambiguate; the
    // final `rename` below publishes atomically.
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmpbase = std::env::temp_dir().join(format!(
        "zz-rt-{}-{:?}-{}-{}",
        std::process::id(),
        std::thread::current().id(),
        uniq,
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
    cmd.arg(format!("-DZZ_HAS_CURL={}", u8::from(opts.curl_link)));
    cmd.arg(format!("-DZZ_HAS_SQLITE3={}", u8::from(opts.sqlite_link)));
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
    if let Some(parent) = archive_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Build the archive at a unique temp path, then atomically rename over
    // the cache entry: concurrent compilers never observe (or publish) a
    // half-written archive. The pid + counter keep parallel processes and
    // threads from colliding. The `.a` suffix is kept so linkers that sniff
    // by extension still treat the unpublished fallback as an archive.
    let tmp_archive =
        archive_path.with_file_name(format!(".libzz_rt.{}.{}.tmp.a", std::process::id(), uniq));
    let ar_out = Command::new("ar")
        .args([
            "rcs",
            tmp_archive.to_str().unwrap(),
            o_path.to_str().unwrap(),
        ])
        .output()
        .map_err(BuildError::Io)?;
    if !ar_out.status.success() {
        let _ = std::fs::remove_file(&c_path);
        let _ = std::fs::remove_file(&o_path);
        let _ = std::fs::remove_file(&tmp_archive);
        return Err(BuildError::CompileFailed {
            stderr: String::from_utf8_lossy(&ar_out.stderr).into_owned(),
        });
    }
    let published = std::fs::rename(&tmp_archive, archive_path).is_ok();
    // Cleanup temporaries.
    let _ = std::fs::remove_file(&c_path);
    let _ = std::fs::remove_file(&o_path);
    if published {
        return Ok(archive_path.to_path_buf());
    }
    if archive_path.exists() {
        // Lost a publish race with an identical archive: adopt the winner.
        let _ = std::fs::remove_file(&tmp_archive);
        return Ok(archive_path.to_path_buf());
    }
    // The cache directory vanished mid-build (e.g. a concurrent
    // `zz cache clean`). The archive we just built is still valid, and
    // callers link it by path — hand it back instead of a missing cache
    // entry. `zz cache clean` (or the cache's LRU reclaim) removes the
    // leftover temporary later.
    Ok(tmp_archive)
}

/// True when this link provides the Postgres backend from the Rust
/// staticlib. `pg_link` only force-extracts archive members inside the
/// `native_rt || float_link` block, so it is meaningless without it.
fn links_pg(opts: &BuildOptions) -> bool {
    opts.pg_link && (opts.native_rt || opts.float_link)
}

/// True when this link provides `zz_float_format_raw` (`float_link`
/// implies the staticlib link in `push_link_inputs`).
fn links_float_fmt(opts: &BuildOptions) -> bool {
    opts.float_link
}

/// Build the compile-only flags for the C runtime archive.
///
/// These are a subset of `clang_flags()`: optimization + target flags only,
/// no link flags (`-Wl,*`, `-l*`, `-static`, `-s`).
///
/// `-ffunction-sections -fdata-sections` are unconditional: the final
/// link already passes `--gc-sections`, so per-function sections let it
/// drop unreferenced runtime features (e.g. fetch/sql) wholesale.
///
/// `ZZ_LINK_PG` / `ZZ_LINK_FLOAT` tell the runtime whether the optional
/// staticlib backends are part of *this* link. They default to 1 (the
/// historical weak-reference shape); 0 compiles the references out, which
/// Darwin needs because ld64 treats an unresolved `weak_import` in an
/// executable as a hard error (ELF permits it).
/// If you change this list, bump `RT_CACHE_VERSION` below or stale
/// archives will poison the cache.
fn build_compile_flags(opts: &BuildOptions, target: Option<&str>) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    flags.push("-ffunction-sections".to_string());
    flags.push("-fdata-sections".to_string());
    if opts.optimize {
        flags.push("-O3".to_string());
        // NO `-flto=thin` here by design (rt6): the archive must hold
        // native objects, never LLVM bitcode. Bitcode members depend on
        // the gold plugin + ar/ld bitcode literacy at link time, which
        // varies by toolchain (CI's binutils 2.42 cannot index them and
        // its links fail with `undefined reference to main` while local
        // 2.47 works). Native objects link everywhere; ThinLTO still
        // applies to program code at the final link. Stale bitcode
        // archives are cut off by the rt6 key bump.
        // Parity contract (mirrors `clang_flags`): defined wrapping and
        // conservative aliasing; no host-specific and no unsafe-FP flags.
        // Keep in sync with `clang_flags()` and bump RT_CACHE_VERSION.
        flags.push("-fwrapv".to_string());
        flags.push("-fno-strict-aliasing".to_string());
        flags.push("-funroll-loops".to_string());
        flags.push("-fomit-frame-pointer".to_string());
    } else {
        flags.push("-O0".to_string());
        flags.push("-g".to_string());
    }
    if let Some(t) = target {
        flags.push(format!("--target={t}"));
    }
    // Sanitizer leg: the archive must be instrumented with the same
    // flags as the program, or ASan/UBSan miss every runtime bug.
    flags.extend(crate::sanitize_flags());
    // Optional staticlib backends: only reference their symbols when this
    // link actually provides them (see the doc comment above).
    flags.push(format!("-DZZ_LINK_PG={}", u8::from(links_pg(opts))));
    flags.push(format!(
        "-DZZ_LINK_FLOAT={}",
        u8::from(links_float_fmt(opts))
    ));
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_tmp(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("zz-rt-lock-{tag}-{}-{uniq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn optional_backend_defines_follow_the_link_gates() {
        // Default/dev link: no staticlib, so the archive must carry zero
        // references to the Rust backends. This is the Darwin fix: ld64
        // rejects unresolved weak imports in an executable.
        let mut opts = BuildOptions::dev();
        let flags = build_compile_flags(&opts, None);
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_PG=0"), "{flags:?}");
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_FLOAT=0"), "{flags:?}");

        // PG gate: staticlib linked and members force-extracted.
        opts.native_rt = true;
        opts.pg_link = true;
        let flags = build_compile_flags(&opts, None);
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_PG=1"), "{flags:?}");
        // float was not gated in this link.
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_FLOAT=0"), "{flags:?}");

        // `float_link` alone already links the staticlib.
        let mut float_only = BuildOptions::dev();
        float_only.float_link = true;
        let flags = build_compile_flags(&float_only, None);
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_FLOAT=1"), "{flags:?}");
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_PG=0"), "{flags:?}");

        // `pg_link` without a staticlib link cannot provide the symbols;
        // keep them compiled out rather than emit a dangling reference.
        let mut dangling = BuildOptions::dev();
        dangling.pg_link = true;
        let flags = build_compile_flags(&dangling, None);
        assert!(flags.iter().any(|f| f == "-DZZ_LINK_PG=0"), "{flags:?}");
    }

    #[test]
    fn rt_lock_releases_on_drop() {
        let dir = lock_tmp("drop");
        {
            let _guard = acquire_rt_lock(&dir);
            assert!(dir.join(".lock").exists(), "lock file present while held");
        }
        assert!(!dir.join(".lock").exists(), "lock file removed on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rt_lock_serializes_second_acquirer() {
        let dir = lock_tmp("serial");
        let guard = acquire_rt_lock(&dir);
        assert!(guard.owned, "first acquire owns the lock");
        let dir2 = dir.clone();
        let handle = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let guard2 = acquire_rt_lock(&dir2);
            assert!(guard2.owned, "second acquire owns after release");
            start.elapsed()
        });
        // Hold long enough to prove the spawned acquire blocks.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            !handle.is_finished(),
            "second acquire must wait for release"
        );
        drop(guard);
        let waited = handle.join().expect("acquirer thread");
        assert!(
            waited >= std::time::Duration::from_millis(400),
            "acquirer waited for release, only waited {waited:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
