//! Best-effort per-file result cache for `zz fmt`.
//!
//! Repeats on unchanged files skip the whole pipeline (parse → lower →
//! render → verify): a cache hit proves the file is already formatted,
//! so `--check` reports clean and plain `fmt` skips the write without
//! touching the parser. First runs and edited files always pay the full
//! (now linear) pipeline, then become cached.
//!
//! Design:
//!
//! - Key = SHA-256 over cache version + crate version + `Debug` config +
//!   source bytes. Any source/config/toolchain change is a different key,
//!   so hits are sound by construction.
//! - Value = empty marker file: existence under a versioned key *is* the
//!   proof of clean. Dirty results are never stored (the next run sees
//!   new content and re-verifies anyway).
//! - Location honours `ZZ_HOME` exactly like `zz_pm::paths` (`$ZZ_HOME`
//!   else `~/.zz/`), under `cache/fmt-v1/`. Safe to delete wholesale.
//! - Best-effort throughout: missing/corrupt entries and all I/O errors
//!   degrade to a miss. Formatting can never fail because of the cache.
//! - Bounded: at most [`MAX_ENTRIES`] markers; beyond that new stores are
//!   skipped (reads keep hitting).
//! - Kill switch: `ZZ_FMT_NO_CACHE` set (to anything) disables both paths.

use crate::FmtConfig;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Bump when the format pipeline changes what "formatted" means.
const CACHE_VERSION: u32 = 1;
/// Marker-file ceiling (~a few hundred KB of empty files, worst case).
const MAX_ENTRIES: usize = 10_000;

fn disabled() -> bool {
    std::env::var("ZZ_FMT_NO_CACHE").is_ok()
}

fn cache_dir() -> Option<PathBuf> {
    if disabled() {
        return None;
    }
    let home = std::env::var("ZZ_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".zz"))
        })?;
    Some(home.join("cache").join("fmt-v1"))
}

fn key_for(source: &str, config: &FmtConfig) -> String {
    let mut h = Sha256::new();
    h.update(CACHE_VERSION.to_le_bytes());
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(b"\0");
    h.update(format!("{config:?}").as_bytes());
    h.update(b"\0");
    h.update(source.as_bytes());
    hex_of(&h.finalize())
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// True when `source` is proven already-formatted under `config`.
/// Any doubt returns false (caller runs the full pipeline).
pub fn is_clean(source: &str, config: &FmtConfig) -> bool {
    let Some(dir) = cache_dir() else {
        return false;
    };
    let key = key_for(source, config);
    // A valid marker is an empty regular file. Non-empty or unreadable
    // entries are treated as absent (never trusted).
    match std::fs::metadata(dir.join(format!("fmt-{key}"))) {
        Ok(m) => m.is_file() && m.len() == 0,
        Err(_) => false,
    }
}

/// Record that `source` formats to itself under `config` (clean).
/// No-op on any error or once the entry ceiling is reached.
pub fn mark_clean(source: &str, config: &FmtConfig) {
    let Some(dir) = cache_dir() else {
        return;
    };
    let path = dir.join(format!("fmt-{}", key_for(source, config)));
    if path.exists() {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // Bound growth: count markers before adding a new one. The scan is
    // O(entries) but runs at most once per newly-clean file.
    let count = std::fs::read_dir(&dir)
        .map(|it| {
            it.filter(|e| {
                e.as_ref().is_ok_and(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("fmt-") && !n.ends_with(".tmp"))
                })
            })
            .count()
        })
        .unwrap_or(usize::MAX);
    if count >= MAX_ENTRIES {
        return;
    }
    // Atomic publish: temp file + rename, so concurrent `fmt` processes
    // never observe a half-written marker.
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        key_for(source, config)
    ));
    if std::fs::write(&tmp, b"").is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Isolated ZZ_HOME per test (temp dir), restored afterwards.
    struct IsolatedHome {
        dir: PathBuf,
        prev: Option<String>,
    }

    impl IsolatedHome {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("zz-fmt-cache-test-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            let prev = std::env::var("ZZ_HOME").ok();
            // SAFETY: tests in this module serialize on ENV_LOCK.
            unsafe {
                std::env::set_var("ZZ_HOME", &dir);
            }
            std::env::remove_var("ZZ_FMT_NO_CACHE");
            IsolatedHome { dir, prev }
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            // SAFETY: serialized on ENV_LOCK like every other env touch here.
            unsafe {
                if let Some(p) = &self.prev {
                    std::env::set_var("ZZ_HOME", p);
                } else {
                    std::env::remove_var("ZZ_HOME");
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn cfg() -> FmtConfig {
        FmtConfig::default()
    }

    #[test]
    fn miss_then_hit_roundtrip() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _home = IsolatedHome::new("roundtrip");
        let src = "x := 1\n";
        assert!(!is_clean(src, &cfg()));
        mark_clean(src, &cfg());
        assert!(is_clean(src, &cfg()));
    }

    #[test]
    fn different_source_misses() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _home = IsolatedHome::new("source");
        mark_clean("x := 1\n", &cfg());
        assert!(!is_clean("x := 2\n", &cfg()));
    }

    #[test]
    fn corrupt_marker_is_miss() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _home = IsolatedHome::new("corrupt");
        let src = "x := 1\n";
        mark_clean(src, &cfg());
        assert!(is_clean(src, &cfg()));
        let dir = cache_dir().unwrap();
        let path = dir.join(format!("fmt-{}", key_for(src, &cfg())));
        std::fs::write(&path, b"garbage").unwrap();
        assert!(!is_clean(src, &cfg()), "non-empty marker must miss");
    }

    #[test]
    fn kill_switch_disables_cache() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _home = IsolatedHome::new("killswitch");
        // SAFETY: serialized on ENV_LOCK.
        unsafe {
            std::env::set_var("ZZ_FMT_NO_CACHE", "1");
        }
        let src = "x := 1\n";
        mark_clean(src, &cfg());
        assert!(!is_clean(src, &cfg()));
    }
}
