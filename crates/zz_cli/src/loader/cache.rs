//! Per-module check cache (S1 arena-scale).
//!
//! `zz check` on a project re-checks every module even when one file
//! changed. The cache stores each module's check outcome keyed by a hash
//! chain: `key_m = H(version, source_m, key_{m-1})`. Any change anywhere
//! upstream changes every downstream key (conservative, always sound;
//! a miss is exactly today's behavior).
//!
//! v1 scope: the `zz check` path only (`zz run`/`build`/`test` bypass and
//! recompute — they need span types and codegen inputs this cache does not
//! store). Parse still runs (import discovery needs the AST); only the
//! check is skipped on hit. `ZZ_CHECK_CACHE=0` disables entirely.

use std::collections::HashMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use zz_checker::{FuncSig, StructSig, Type};
use zz_frontend::diag::RawDiag;

/// What one module contributes on a cache hit: everything `finish()` needs
/// that does not require running the checker. Mirrors the `CheckResult`
/// fields the check path consumes; link/const/try data is run-path-only
/// and intentionally absent (run bypasses the cache).
///
/// Fields are `Cow`: the miss path serializes BORROWED maps (zero copy
/// before the tail moves them), while reads deserialize into `Owned`.
/// `into_owned()` on an `Owned` value is a no-op move.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedModule<'a> {
    /// Module's own functions (== `CheckResult.funcs`): privates + pubs.
    #[serde(borrow)]
    pub funcs: std::borrow::Cow<'a, HashMap<String, FuncSig>>,
    /// Resolved pub functions (== `CheckResult.pub_funcs`).
    #[serde(borrow)]
    pub pub_funcs: std::borrow::Cow<'a, HashMap<String, FuncSig>>,
    /// Module's own structs (== `CheckResult.structs`).
    #[serde(borrow)]
    pub structs: std::borrow::Cow<'a, HashMap<String, StructSig>>,
    /// Resolved pub structs (== `CheckResult.pub_structs`).
    #[serde(borrow)]
    pub pub_structs: std::borrow::Cow<'a, HashMap<String, StructSig>>,
    /// Module's new bindings (== `CheckResult.bindings`).
    #[serde(borrow)]
    pub bindings: std::borrow::Cow<'a, HashMap<String, Type>>,
    /// Resolved pub bindings (== `CheckResult.pub_bindings`).
    #[serde(borrow)]
    pub pub_bindings: std::borrow::Cow<'a, HashMap<String, Type>>,
    /// Errors AND warnings (== `CheckResult.errors`; warnings replay too).
    pub diags: Vec<RawDiag>,
}

/// Genesis material: anything that changes checker semantics globally.
/// Bumped automatically with the CLI version; extend if the checker gains
/// flags or the stdlib versions independently.
fn genesis(plugin_names: &[String]) -> String {
    let mut names = plugin_names.to_vec();
    names.sort();
    format!(
        "zz-check-cache-v1|cli={}|plugins={}",
        env!("CARGO_PKG_VERSION"),
        names.join(","),
    )
}

/// Dependency-aware key for one module: `H(genesis, source, dep_keys...)`
/// with deps sorted by path. Only TRUE dependents recheck on an edit
/// (editing a leaf rechecks the leaf + its importers, not everything
/// after it in load order). Any change anywhere in the transitive closure
/// changes the key; a miss is exactly today's behavior (always sound).
pub fn module_key(genesis: &str, source: &str, deps: &[(String, String)]) -> String {
    let mut sorted: Vec<(&String, &String)> = deps.iter().map(|(p, k)| (p, k)).collect();
    sorted.sort();
    let mut h = Sha256::new();
    h.update(genesis.as_bytes());
    h.update(b"|src|");
    h.update(source.as_bytes());
    for (path, key) in sorted {
        h.update(b"|dep|");
        h.update(path.as_bytes());
        h.update(b"=");
        h.update(key.as_bytes());
    }
    hex_of(h)
}

/// Genesis string for this invocation (exposed for tests).
pub fn genesis_key(plugin_names: &[String]) -> String {
    genesis(plugin_names)
}

fn hex_of(h: Sha256) -> String {
    let bytes = h.finalize();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
    }
    s
}

/// Cache directory: `$XDG_CACHE/zz/check/v1`, tempdir fallback (never fail
/// the compile when the cache is unusable — callers treat errors as miss).
fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("zz")
        .join("check")
        .join("v1")
}

fn cache_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.json"))
}

/// Isolated cache dir + serialized env for cache-behavior tests.
///
/// Parallel tests share the process env, so XDG flips must be mutually
/// exclusive; isolation also makes storage assertions deterministic
/// (fresh dir per test — growth/contents prove hits, not luck).
/// Cleanup removes only the test's own dir.
/// Serializes tests that mutate process-global cache env. Parallel tests
/// otherwise observe each other's flips.
#[cfg(test)]
pub(crate) static CACHE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn with_isolated_cache<T>(f: impl FnOnce() -> T) -> T {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let _guard = CACHE_ENV_LOCK.lock().unwrap();
    let prev_xdg = std::env::var("XDG_CACHE_HOME").ok();
    let prev_flag = std::env::var("ZZ_CHECK_CACHE").ok();
    let dir = std::env::temp_dir().join(format!(
        "zz-cache-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::env::set_var("XDG_CACHE_HOME", &dir);
    let r = f();
    match prev_xdg {
        Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
        None => std::env::remove_var("XDG_CACHE_HOME"),
    }
    match prev_flag {
        Some(v) => std::env::set_var("ZZ_CHECK_CACHE", v),
        None => std::env::remove_var("ZZ_CHECK_CACHE"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// Entry count (test observability: proves hits happened).
#[cfg(test)]
pub(crate) fn entry_count() -> usize {
    std::fs::read_dir(cache_dir())
        .map(|it| it.count())
        .unwrap_or(0)
}

/// Disabled via `ZZ_CHECK_CACHE=0` (diagnosing weirdness, benchmarking).
pub fn cache_enabled() -> bool {
    std::env::var("ZZ_CHECK_CACHE")
        .map(|v| v != "0")
        .unwrap_or(true)
}

impl<'a> CachedModule<'a> {
    /// Freeze all fields to owned (no-op moves when already owned).
    pub fn into_owned(self) -> CachedModule<'static> {
        CachedModule {
            funcs: std::borrow::Cow::Owned(self.funcs.into_owned()),
            pub_funcs: std::borrow::Cow::Owned(self.pub_funcs.into_owned()),
            structs: std::borrow::Cow::Owned(self.structs.into_owned()),
            pub_structs: std::borrow::Cow::Owned(self.pub_structs.into_owned()),
            bindings: std::borrow::Cow::Owned(self.bindings.into_owned()),
            pub_bindings: std::borrow::Cow::Owned(self.pub_bindings.into_owned()),
            diags: self.diags,
        }
    }
}

/// Read a cached module. `None` on any failure (absent, corrupt, unreadable
/// dir) — the caller checks normally.
pub fn read_cached(key: &str) -> Option<CachedModule<'static>> {
    if !cache_enabled() {
        return None;
    }
    let data = std::fs::read(cache_path(key)).ok()?;
    // Deserialized maps are always `Owned` (HashMaps cannot borrow);
    // `into_owned` below is a no-op move in that case.
    let m: CachedModule<'_> = serde_json::from_slice(&data).ok()?;
    Some(m.into_owned())
}

/// Store a module outcome. Failures are silent (cache is best-effort).
pub fn write_cached(key: &str, module: &CachedModule) {
    if !cache_enabled() {
        return;
    }
    let Ok(data) = serde_json::to_vec(module) else {
        return;
    };
    let path = cache_path(key);
    if std::fs::create_dir_all(path.parent().expect("cache file has parent")).is_err() {
        return;
    }
    // Best-effort atomicity: temp file + rename (no torn reads).
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &data).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_keys_differ_by_source_and_deps() {
        let g = genesis_key(&[]);
        let leaf_a = module_key(&g, "pub base := 41", &[]);
        let leaf_a2 = module_key(&g, "pub base := 41", &[]);
        let leaf_b = module_key(&g, "pub base := 42", &[]);
        let mid_a = module_key(
            &g,
            "import config",
            &[("config.zz".to_string(), leaf_a.clone())],
        );
        let mid_b = module_key(
            &g,
            "import config",
            &[("config.zz".to_string(), leaf_b.clone())],
        );
        let mid_c = module_key(
            &g,
            "import config",
            &[("other.zz".to_string(), leaf_a.clone())],
        );
        assert_eq!(leaf_a, leaf_a2, "deterministic");
        assert_ne!(leaf_a, leaf_b, "source sensitivity");
        assert_ne!(mid_a, mid_b, "dep content sensitivity");
        assert_ne!(mid_a, mid_c, "dep path sensitivity");
        assert_eq!(leaf_a.len(), 64, "sha256 hex");
    }

    #[test]
    fn roundtrip_through_disk() {
        use std::borrow::Cow;
        super::with_isolated_cache(|| {
            // Unique key; cleanup removes only this test's own file.
            let key = format!("zz-unit-test-{}", std::process::id());
            let m = CachedModule {
                funcs: Cow::Owned(HashMap::new()),
                pub_funcs: Cow::Owned(HashMap::new()),
                structs: Cow::Owned(HashMap::new()),
                pub_structs: Cow::Owned(HashMap::new()),
                bindings: Cow::Owned(HashMap::new()),
                pub_bindings: Cow::Owned(HashMap::new()),
                diags: Vec::new(),
            };
            write_cached(&key, &m);
            let back = read_cached(&key);
            assert!(back.is_some(), "roundtrip");
            let _ = std::fs::remove_file(cache_path(&key));
        });
    }
}
