//! ZZ_HOME resolution and directory layout.
//!
//! All paths respect the `ZZ_HOME` env var. Default:
//! - Linux/macOS: `$HOME/.zz/`
//! - Windows: `%USERPROFILE%\.zz\`

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

// Thread-local: `ENV_OVERRIDES` maps a variable name to an optional value
// (`Some(None)` = explicitly unset).
thread_local! {
    static ENV_OVERRIDES: RefCell<HashMap<String, Option<OsString>>> =
        RefCell::new(HashMap::new());
}

/// Guard returned by [`env_scope`]: restores the previous thread-local
/// values on drop (innermost scope wins, nesting is supported).
#[derive(Debug)]
#[must_use = "the override only lasts as long as the guard"]
pub struct EnvScope {
    prev: Vec<(String, Option<Option<OsString>>)>,
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        ENV_OVERRIDES.with(|cell| {
            let mut map = cell.borrow_mut();
            // Reverse order: repeated keys in one call restore correctly.
            for (key, prev) in self.prev.drain(..).rev() {
                match prev {
                    Some(v) => {
                        map.insert(key, v);
                    }
                    None => {
                        map.remove(&key);
                    }
                }
            }
        });
    }
}

/// Thread-scoped environment overrides (test hook).
///
/// Override `pairs` (`None` = explicitly unset) on the current thread.
///
/// `std::env::set_var` mutates process globals: concurrent readers in the
/// same test binary may observe a torn value — Rust 2024 makes the call
/// `unsafe` for exactly this reason. That is not theoretical here: a test
/// that redirected `ZZ_HOME` while a parallel test built an AOT binary sent
/// that build's runtime-archive cache into the other test's temp directory,
/// surfacing as `io error: Invalid argument (os error 22)` and a bogus
/// `Undefined symbols: _main` link error.
///
/// Tests therefore scope values here and read them through [`env_var_os`] /
/// [`env_home_dir`] / [`env_cache_dir`]. No process state is touched, so
/// parallel tests and the builds they run cannot observe each other's
/// values; spawned threads and child processes deliberately do not inherit
/// the override, so background builds keep the real environment. Callers
/// keep the returned guard alive for the scope that should see it.
#[doc(hidden)]
pub fn env_scope(pairs: &[(&str, Option<OsString>)]) -> EnvScope {
    let mut prev = Vec::with_capacity(pairs.len());
    ENV_OVERRIDES.with(|cell| {
        let mut map = cell.borrow_mut();
        for (key, value) in pairs {
            let key = (*key).to_string();
            prev.push((key.clone(), map.insert(key, value.clone())));
        }
    });
    EnvScope { prev }
}

/// `std::env::var_os` with the current thread's [`env_scope`] applied.
///
/// Production callers see the process environment unchanged (the override
/// map is empty); tests see their own thread's values.
pub fn env_var_os(key: &str) -> Option<OsString> {
    match ENV_OVERRIDES.with(|cell| cell.borrow().get(key).cloned()) {
        Some(value) => value,
        None => std::env::var_os(key),
    }
}

/// `dirs::home_dir` with the current thread's [`env_scope`] applied.
///
/// Only the override is consulted (`HOME`, then `USERPROFILE` on Windows);
/// without an override this is exactly `dirs::home_dir()`, so non-test
/// behavior — including Windows' known-folder resolution — is unchanged.
pub fn env_home_dir() -> Option<PathBuf> {
    let overridden = ENV_OVERRIDES.with(|cell| {
        let map = cell.borrow();
        map.get("HOME")
            .cloned()
            .or_else(|| map.get("USERPROFILE").cloned())
            .flatten()
    });
    match overridden {
        Some(home) => Some(PathBuf::from(home)),
        None => dirs::home_dir(),
    }
}

/// `dirs::cache_dir` with the current thread's [`env_scope`] applied.
///
/// On Linux `dirs::cache_dir` already honors `XDG_CACHE_HOME`, but macOS
/// (`~/Library/Caches`) and Windows (`%LOCALAPPDATA%`) ignore it — so tests
/// scoping `XDG_CACHE_HOME` were only isolated on Linux. The override is
/// platform-independent; without one this is exactly `dirs::cache_dir()`.
pub fn env_cache_dir() -> Option<PathBuf> {
    let overridden = ENV_OVERRIDES.with(|cell| cell.borrow().get("XDG_CACHE_HOME").cloned());
    match overridden {
        Some(dir) => dir.map(PathBuf::from),
        None => dirs::cache_dir(),
    }
}

/// Resolve the ZZ home directory.
///
/// Priority: `ZZ_HOME` env var > platform default (`~/.zz/`).
pub fn zz_home() -> PathBuf {
    if let Some(home) = env_var_os("ZZ_HOME") {
        return PathBuf::from(home);
    }
    let base = env_home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".zz")
}

/// `~/.zz/packages/` — CAS source storage.
pub fn packages_dir() -> PathBuf {
    zz_home().join("packages")
}

/// `~/.zz/cache/objects/` — per-module build artifact cache.
pub fn cache_objects_dir() -> PathBuf {
    zz_home().join("cache").join("objects")
}

/// `~/.zz/cache/` — everything rebuildable from a build: native binary
/// cache entries (flat `<key>-<mode>-<target>` files), precompiled
/// runtime archives (per-key dirs), and the per-module `objects/`
/// cache. Safe to delete wholesale; `zz cache clean` clears this tree.
pub fn build_cache_dir() -> PathBuf {
    zz_home().join("cache")
}

/// `~/.zz/bin/` — installed compiler binaries and `zz install --path` tools.
///
/// This directory is the canonical user-tool location: the install scripts
/// place `zz`/`zz-lsp` here, and `zz install --path <dir>` drops built
/// project binaries here. It should be on `PATH` (see `zz setup`).
pub fn bin_dir() -> PathBuf {
    zz_home().join("bin")
}

/// `~/.zz/credentials.toml` — auth tokens (chmod 0600).
pub fn credentials_path() -> PathBuf {
    zz_home().join("credentials.toml")
}

/// `~/.zz/reverse-refs.json` — GC reference index.
pub fn reverse_refs_path() -> PathBuf {
    zz_home().join("reverse-refs.json")
}

/// `~/.zz/known-projects.json` — machine-wide project registry for safe GC.
pub fn known_projects_path() -> PathBuf {
    zz_home().join("known-projects.json")
}

/// CAS entry path for a given content hash/commit.
///
/// Returns `~/.zz/packages/<hash>` — the directory where extracted
/// source content lives after fetching into the CAS.
pub fn cas_entry(hash: &str) -> PathBuf {
    packages_dir().join(hash)
}

/// `(entries, bytes)` under `dir`, best effort: unreadable subtrees count
/// as empty. Cache and clean-size calculations share this.
///
/// Hardlinked files (the build cache aliases its probe entry, #629) are
/// counted once: on unix each `(device, inode)` contributes its size a
/// single time, so `zz cache clean` / `zz doctor` never inflate the total
/// by the number of aliases.
pub fn dir_usage(dir: &std::path::Path) -> (usize, u64) {
    let mut entries = 0usize;
    let mut bytes = 0u64;
    #[cfg(unix)]
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                entries += 1;
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    // `nlink > 1` covers hardlinks inside the tree; the set
                    // also collapses links shared with files outside it.
                    if meta.nlink() > 1 && !seen.insert((meta.dev(), meta.ino())) {
                        continue;
                    }
                }
                bytes += meta.len();
            }
        }
    }
    (entries, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zz_home_respects_env_var() {
        let tmp = std::env::temp_dir().join("zz_paths_test_home");
        let _scope = env_scope(&[("ZZ_HOME", Some(tmp.clone().into_os_string()))]);
        assert_eq!(zz_home(), tmp);
    }

    #[test]
    fn zz_home_default() {
        let _scope = env_scope(&[("ZZ_HOME", None)]);
        let home = zz_home();
        assert!(home.ends_with(".zz"), "should end with .zz: {home:?}");
    }

    #[test]
    fn packages_dir_under_home() {
        let _scope = env_scope(&[("ZZ_HOME", None)]);
        assert!(packages_dir().ends_with("packages"));
    }

    #[test]
    fn cache_objects_dir_under_home() {
        let _scope = env_scope(&[("ZZ_HOME", None)]);
        let p = cache_objects_dir();
        assert!(p.ends_with("objects"), "should end with objects: {p:?}");
    }

    #[test]
    fn build_cache_dir_under_home() {
        let _scope = env_scope(&[("ZZ_HOME", None)]);
        assert!(build_cache_dir().ends_with("cache"));
        let tmp = std::env::temp_dir().join("zz_paths_test_home_cache");
        let _tmp_scope = env_scope(&[("ZZ_HOME", Some(tmp.clone().into_os_string()))]);
        assert_eq!(build_cache_dir(), tmp.join("cache"));
    }

    #[test]
    fn env_scope_is_thread_local_and_nests() {
        let a = std::env::temp_dir().join("zz_paths_scope_a");
        let b = std::env::temp_dir().join("zz_paths_scope_b");
        let outer = env_scope(&[("ZZ_HOME", Some(a.clone().into_os_string()))]);
        assert_eq!(zz_home(), a);
        {
            let _inner = env_scope(&[("ZZ_HOME", Some(b.clone().into_os_string()))]);
            assert_eq!(zz_home(), b, "innermost scope wins");
        }
        assert_eq!(zz_home(), a, "inner drop restores the outer value");
        drop(outer);
        // Other threads never see the override: a spawned thread resolves
        // the process environment instead (the property that keeps parallel
        // builds out of a test's temp home).
        let scoped = env_scope(&[("ZZ_HOME", Some(a.clone().into_os_string()))]);
        let seen = std::thread::spawn(zz_home).join().unwrap();
        assert_ne!(seen, a, "override must not leak to other threads");
        drop(scoped);
        assert_eq!(zz_home(), seen, "after drop the base env is back");
    }

    #[test]
    fn dir_usage_counts() {
        let tmp = std::env::temp_dir().join("zz_paths_test_dusage");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a"), "1234").unwrap();
        std::fs::write(tmp.join("sub").join("b"), "12").unwrap();
        assert_eq!(dir_usage(&tmp), (2, 6));
        assert_eq!(dir_usage(&tmp.join("missing")), (0, 0));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn dir_usage_counts_hardlinks_once() {
        // Build-cache aliases are hardlinks (#629): the same inode must not
        // inflate the reported size, while the entry count stays honest.
        let tmp = std::env::temp_dir().join("zz_paths_test_dusage_link");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a"), "12345678").unwrap();
        std::fs::hard_link(tmp.join("a"), tmp.join("b")).unwrap();
        assert_eq!(dir_usage(&tmp), (2, 8));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bin_dir_under_home() {
        let _scope = env_scope(&[("ZZ_HOME", None)]);
        assert!(bin_dir().ends_with("bin"));
    }
}
