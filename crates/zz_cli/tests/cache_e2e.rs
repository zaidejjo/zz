//! `zz clean` / `zz cache` reclaim behavior.
//!
//! - `zz cache clean` clears the whole build-cache tree (native binary
//!   entries, precompiled runtime archives, per-module objects), not just
//!   one corner of it.
//! - `zz clean` reports the global cache with a reclaim hint instead of
//!   a bare "nothing to clean" while gigabytes sit elsewhere.
//! - native builds honor `ZZ_HOME`: the cache lands in the isolated home,
//!   never leaking into the real one.
//!
//! All tests isolate `ZZ_HOME` to a temp dir (per-process paths), so they
//! never touch the developer's real `~/.zz`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn zz() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("zz-cache-e2e-{tag}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmpdir");
    dir
}

fn run(dir: &Path, args: &[&str], zz_home: &Path) -> (i32, String, String) {
    let out = Command::new(zz())
        .args(args)
        .current_dir(dir)
        .env("ZZ_HOME", zz_home)
        .output()
        .expect("exec zz");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn seed_tree(home: &Path) {
    let cache = home.join("cache");
    std::fs::create_dir_all(&cache).expect("seed");
    // Native binary entry (flat file, the heavy one).
    std::fs::write(cache.join("abc123-Static-host"), vec![0u8; 1024 * 1024]).expect("seed");
    // Precompiled runtime archive (per-key dir).
    let rt = cache.join("x86_64-unknown-linux-gnu-rel-hash-clang-nopin-rt6-nosan");
    std::fs::create_dir_all(&rt).expect("seed");
    std::fs::write(rt.join("libzz_rt.a"), vec![0u8; 2048]).expect("seed");
    // Per-module objects.
    let objs = cache.join("objects").join("slug");
    std::fs::create_dir_all(&objs).expect("seed");
    std::fs::write(objs.join("out.o"), vec![0u8; 1024]).expect("seed");
}

#[test]
fn cache_clean_clears_whole_tree() {
    let home = fresh_dir("home");
    let work = fresh_dir("work");
    seed_tree(&home);
    let (code, stdout, stderr) = run(&work, &["cache", "clean"], &home);
    assert_eq!(
        code, 0,
        "cache clean must pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !home.join("cache").exists(),
        "entire cache tree must be gone"
    );
    assert!(
        stdout.contains("freed"),
        "must report freed size:\n{stdout}"
    );
    assert!(
        stdout.contains("1.0 MB"),
        "must report human size (1MiB + 3KiB):\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn cache_clean_empty_reports_plainly() {
    let home = fresh_dir("emptyhome");
    let work = fresh_dir("emptywork");
    let (code, stdout, _) = run(&work, &["cache", "clean"], &home);
    assert_eq!(code, 0);
    assert!(stdout.contains("no cache to clear"), "got:\n{stdout}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn clean_reports_global_cache_with_hint() {
    let home = fresh_dir("cleanhome");
    let work = fresh_dir("cleanwork");
    // Small but non-empty cache: the line must appear with exact size.
    std::fs::create_dir_all(home.join("cache")).expect("seed");
    std::fs::write(home.join("cache").join("e-Static-host"), vec![0u8; 2048]).expect("seed");
    let (code, stdout, stderr) = run(&work, &["clean"], &home);
    assert_eq!(
        code, 0,
        "clean must pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("global build cache: 2.0 KB in 1 entries"),
        "needs cache status line, got:\n{stdout}"
    );
    assert!(
        stdout.contains("zz cache clean"),
        "needs reclaim hint, got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn clean_stays_quiet_without_cache() {
    let home = fresh_dir("quhome");
    let work = fresh_dir("quwork");
    let (code, stdout, _) = run(&work, &["clean"], &home);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("global build cache"),
        "no cache line without a cache, got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn native_build_honors_zz_home() {
    let home = fresh_dir("buildhome");
    let work = fresh_dir("buildwork");
    std::fs::write(work.join("hello.zz"), "println(\"zzhome_ok\")\n").expect("fixture");
    let (code, _stdout, stderr) = run(&work, &["build", "hello.zz"], &home);
    if code != 0 && stderr.contains("no clang found") {
        eprintln!("SKIP: no clang on PATH");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&work);
        return;
    }
    assert_eq!(code, 0, "build must pass.\nstderr:\n{stderr}");
    // Standalone output still lands in the invocation dir ...
    assert!(work.join("hello").is_file(), "./hello missing");
    // ... while the cache lands in the isolated home, never ~/.zz.
    let entries: Vec<_> = std::fs::read_dir(home.join("cache"))
        .expect("ZZ_HOME cache must exist")
        .flatten()
        .collect();
    assert!(!entries.is_empty(), "isolated cache must hold entries");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&work);
}
