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
//! store). `ZZ_CHECK_CACHE=0` disables entirely.

use std::collections::HashMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use zz_checker::{AliasSig, EnumSig, FuncSig, StructSig, Type};
use zz_frontend::ast::Program;
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
    /// Module's own aliases (== `CheckResult.aliases`).
    #[serde(borrow)]
    pub aliases: std::borrow::Cow<'a, HashMap<String, AliasSig>>,
    /// Resolved pub aliases (== `CheckResult.pub_aliases`).
    #[serde(borrow)]
    pub pub_aliases: std::borrow::Cow<'a, HashMap<String, AliasSig>>,
    /// Module's own enums (== `CheckResult.enums`).
    #[serde(borrow)]
    pub enums: std::borrow::Cow<'a, HashMap<String, EnumSig>>,
    /// Resolved pub enums (== `CheckResult.pub_enums`).
    #[serde(borrow)]
    pub pub_enums: std::borrow::Cow<'a, HashMap<String, EnumSig>>,
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
///
/// The build identity disambiguates same-version binaries, dirty working
/// trees included (issue #221): a rebuild that changed the checker but not
/// `HEAD` used to share a namespace with the previous binary's outcomes.
///
/// The stdlib seed fingerprint stays as an independent signal that same
/// binary re-emits carry different seeds (issue #371: release vs dev
/// stdlibs, or generated seeds) — a seed change invalidates via the
/// genesis even when the binary is byte-identical.
fn genesis(plugin_names: &[String]) -> String {
    let mut names = plugin_names.to_vec();
    names.sort();
    format!(
        "zz-check-cache-v7|cli={}|build={}|plugins={}|seed={}",
        env!("CARGO_PKG_VERSION"),
        zz_ir::build_stamp(),
        names.join(","),
        seed_fingerprint(),
    )
}

/// Hash of the sorted stdlib seed keys (checker function table): any
/// added/removed/renamed native changes the genesis, so stale check
/// outcomes (including cached failures) never replay across stdlib
/// changes. Sorted for determinism; SHA-256 hex like module keys.
/// Cached process-wide: the seed is baked per binary and never changes
/// mid-run; sorting ~1k keys once beats per-module recompute.
fn seed_fingerprint() -> String {
    static FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FINGERPRINT
        .get_or_init(|| {
            let mut keys: Vec<&String> = zz_stdlib::stdlib_funcs_cached().keys().collect();
            keys.sort();
            let mut h = Sha256::new();
            for k in keys {
                h.update(k.as_bytes());
                h.update(b"\0");
            }
            hex_of(h)
        })
        .clone()
}

/// Dependency-aware key for one module:
/// `H(genesis, namespace, source, dep_keys...)` with deps sorted by path.
/// Only TRUE dependents recheck on an edit (editing a leaf rechecks the
/// leaf + its importers, not everything after it in load order). Any change
/// anywhere in the transitive closure changes the key; a miss is exactly
/// today's behavior (always sound).
///
/// The namespace is part of the key (#290): cached pubs are stored
/// already-namespaced (`ar.foo` vs `area.foo`), so the same file checked
/// under two namespaces (e.g. `import area as ar` in one entry, plain
/// `import area` in another) must not share one cache entry — the second
/// entry would restore the winner's namespace and poison its own.
pub fn module_key(
    genesis: &str,
    source: &str,
    deps: &[(String, String)],
    namespace: &str,
) -> String {
    let mut sorted: Vec<(&String, &String)> = deps.iter().map(|(p, k)| (p, k)).collect();
    sorted.sort();
    let mut h = Sha256::new();
    h.update(genesis.as_bytes());
    h.update(b"|ns|");
    h.update(namespace.as_bytes());
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

/// In-process memo of check outcomes, keyed exactly like the disk cache
/// (same key, same value semantics). A multi-entry `zz check`/`zz test`
/// run restores shared dependencies once per entry from disk (JSON parse
/// of every pub table, every time); the memo serves repeats from memory.
/// Disk stays the cross-process cache: reads populate it, writes fill both.
///
/// Bounded by total retained bytes ([`MAX_MEM_BYTES`]): past the cap the
/// table resets (eviction only costs future hits, never correctness).
/// Mutex-guarded: cache tests run in parallel threads and share it; keys
/// are content hashes so identical content always agrees.
const MAX_MEM_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct MemTable {
    map: HashMap<String, CachedModule<'static>>,
    bytes: usize,
}

static MEM_CACHE: std::sync::OnceLock<std::sync::Mutex<MemTable>> = std::sync::OnceLock::new();

fn mem_cache() -> &'static std::sync::Mutex<MemTable> {
    MEM_CACHE.get_or_init(|| std::sync::Mutex::new(MemTable::default()))
}

fn mem_get(key: &str) -> Option<CachedModule<'static>> {
    mem_cache().lock().ok()?.map.get(key).cloned()
}

fn mem_insert(key: &str, module: CachedModule<'static>, serialized_len: usize) {
    if let Ok(mut table) = mem_cache().lock() {
        if table.bytes + serialized_len > MAX_MEM_BYTES {
            table.map.clear();
            table.bytes = 0;
        }
        table.bytes += serialized_len;
        table.map.insert(key.to_string(), module);
    }
}

/// In-process memo of parsed+expanded programs (pre-namespace), keyed by
/// (canonical path, source content hash). A multi-entry `zz check` run
/// re-parses every dependency closure per entry file; the memo serves
/// repeats from memory (one clone) instead of lex+parse+decorator
/// expansion. Namespace rewriting still runs per consumer on the clone,
/// so the same file under different aliases stays distinct (#290):
/// the memoized program is namespace-agnostic by construction.
///
/// Bounded by retained source bytes ([`MAX_PARSED_BYTES`]): past the cap
/// the table resets (eviction only costs future hits, never correctness).
/// Source length proxies AST size without measuring it.
/// Content-hash keys make stale hits impossible: edited files hash
/// differently (including across `--fix` writes inside one run).
/// Mutex-guarded like the outcome memo above.
const MAX_PARSED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct ParsedTable {
    map: HashMap<(PathBuf, String), Program>,
    bytes: usize,
}

static PARSED_PROGRAMS: std::sync::OnceLock<std::sync::Mutex<ParsedTable>> =
    std::sync::OnceLock::new();

fn parsed_table() -> &'static std::sync::Mutex<ParsedTable> {
    PARSED_PROGRAMS.get_or_init(|| std::sync::Mutex::new(ParsedTable::default()))
}

/// Content hash for program memo keys (SHA-256 hex, like module keys).
pub(crate) fn source_hash(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    hex_of(h)
}

/// Fetch a memoized program. Cloned per caller: consumers namespace-rewrite
/// in place, so sharing the stored value would corrupt later hits.
pub(crate) fn parsed_get(canon: &std::path::Path, hash: &str) -> Option<Program> {
    parsed_table()
        .lock()
        .ok()?
        .map
        .get(&(canon.to_path_buf(), hash.to_string()))
        .cloned()
}

/// Store a parsed+expanded (pre-namespace) program for reuse.
/// `source_len` accounts the entry against the byte cap.
pub(crate) fn parsed_insert(
    canon: &std::path::Path,
    hash: &str,
    program: &Program,
    source_len: usize,
) {
    if let Ok(mut table) = parsed_table().lock() {
        if table.bytes + source_len > MAX_PARSED_BYTES {
            table.map.clear();
            table.bytes = 0;
        }
        table.bytes += source_len;
        table
            .map
            .insert((canon.to_path_buf(), hash.to_string()), program.clone());
    }
}

/// Process-wide memos for import resolution (#233).
///
/// Every bare `import foo` otherwise re-walks ancestors (canonicalize +
/// `exists()` probes per level), re-parses `zz.toml` + `zz.lock` (TOML),
/// and re-stats candidate entries — per import, per entry file. These
/// memoize the pure lookups. Nothing they observe changes mid-run:
/// discovery enumerates files upfront and `--fix` only rewrites `.zz`
/// files (manifest/lock reads additionally revalidate mtimes, so even a
/// concurrent manifest edit can only cause a reload, never a stale hit).
///
/// All tables are clear-on-overflow capped (eviction costs hits only)
/// and mutex-guarded like the memos above.
const MAX_FS_ENTRIES: usize = 4096;
const MAX_MANIFEST_ENTRIES: usize = 1024;

static CANON_MEMO: std::sync::OnceLock<
    std::sync::Mutex<HashMap<PathBuf, Result<PathBuf, String>>>,
> = std::sync::OnceLock::new();
static ROOT_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Option<PathBuf>>>> =
    std::sync::OnceLock::new();
static ROOTS_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Vec<PathBuf>>>> =
    std::sync::OnceLock::new();

fn memo_capped_insert<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, V>, cap: usize, k: K, v: V) {
    if map.len() >= cap {
        map.clear();
    }
    map.insert(k, v);
}

/// Memoized `path.canonicalize()`. Errors memoize too (absent paths stay
/// absent within a run; discovery is upfront). The io error string is
/// retained so callers render byte-identical diagnostics.
pub(crate) fn canon(path: &std::path::Path) -> Result<PathBuf, String> {
    if let Some(hit) = CANON_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(path).cloned())
    {
        return hit;
    }
    let v = path.canonicalize().map_err(|e| e.to_string());
    if let Ok(mut m) = CANON_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        memo_capped_insert(&mut m, MAX_FS_ENTRIES, path.to_path_buf(), v.clone());
    }
    v
}

/// Memoized project-root walk (nearest ancestor holding `zz.toml`).
/// `walk` computes it on miss; `None` (outside any project) memoizes too.
pub(crate) fn project_root_memo(
    canon_path: &std::path::Path,
    walk: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(hit) = ROOT_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(canon_path).cloned())
    {
        return hit;
    }
    let v = walk();
    if let Ok(mut m) = ROOT_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        memo_capped_insert(&mut m, MAX_FS_ENTRIES, canon_path.to_path_buf(), v.clone());
    }
    v
}

/// Memoized ancestor-roots walk (nearest-first `zz.toml` holders).
pub(crate) fn ancestor_roots_memo(
    canon_path: &std::path::Path,
    walk: impl FnOnce() -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    if let Some(hit) = ROOTS_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(canon_path).cloned())
    {
        return hit;
    }
    let v = walk();
    if let Ok(mut m) = ROOTS_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        memo_capped_insert(&mut m, MAX_FS_ENTRIES, canon_path.to_path_buf(), v.clone());
    }
    v
}

/// Reverse dependency edges for import resolution (#420): canonical
/// package dir → project roots that loaded it as a dependency. A path
/// dependency (sibling dir, not under `vendor/`) has no ancestor link to
/// its consumer, so bare imports *inside* the dep (`import leaf` in
/// `mid/`) would otherwise never try the consumer's lock. Entries are
/// recorded as each dep entry resolves (parents always load before their
/// imports are processed), keyed by canonical dir so parallel tests on
/// unique temp dirs never cross-talk.
static DEP_CONSUMERS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Vec<PathBuf>>>> =
    std::sync::OnceLock::new();

/// Record that `root` loaded the package at `pkg_dir` as a dependency.
pub(crate) fn record_dep_consumer(pkg_dir: &std::path::Path, root: &std::path::Path) {
    let pkg = canon(pkg_dir).unwrap_or_else(|_| pkg_dir.to_path_buf());
    let root = canon(root).unwrap_or_else(|_| root.to_path_buf());
    if pkg == root {
        return;
    }
    if let Ok(mut m) = DEP_CONSUMERS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        let entry = m.entry(pkg).or_default();
        if !entry.contains(&root) {
            entry.push(root);
        }
    }
}

/// Consumer roots recorded for a package dir (empty when unknown).
pub(crate) fn dep_consumers(pkg_dir: &std::path::Path) -> Vec<PathBuf> {
    let pkg = canon(pkg_dir).unwrap_or_else(|_| pkg_dir.to_path_buf());
    DEP_CONSUMERS
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|m| m.get(&pkg).cloned())
        .unwrap_or_default()
}

/// Memoized manifest+lock loads per project root, validated by both
/// files' mtimes on every hit. Partial states memoize too (manifest
/// without lock is the common lockless-project shape): any file
/// appearing, disappearing, or changing busts the mtime match and
/// reloads. Miss cost only, never stale data.
type ManifestEntry = (
    Option<zz_pm::manifest::Manifest>,
    Option<zz_pm::lock::Lockfile>,
    Option<std::time::SystemTime>,
    Option<std::time::SystemTime>,
);

static MANIFEST_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, ManifestEntry>>> =
    std::sync::OnceLock::new();

/// Memoized manifest alone (no lockfile required). `package_of` resolves
/// package names for projects that never generated a lock.
pub(crate) fn manifest_of(root: &std::path::Path) -> Option<zz_pm::manifest::Manifest> {
    manifest_entry(root).0
}

/// Shared manifest+lock load with mtime validation.
fn manifest_entry(
    root: &std::path::Path,
) -> (
    Option<zz_pm::manifest::Manifest>,
    Option<zz_pm::lock::Lockfile>,
) {
    let toml = root.join("zz.toml");
    let lock = root.join("zz.lock");
    let mt_toml = file_mtime(&toml);
    let mt_lock = file_mtime(&lock);
    if let Some((man, lck, h_toml, h_lock)) = MANIFEST_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(root).cloned())
    {
        if h_toml == mt_toml && h_lock == mt_lock {
            return (man, lck);
        }
        // Changed under us: fall through and reload.
    }
    let man = zz_pm::manifest::Manifest::load(&toml).ok();
    let lck = zz_pm::lock::Lockfile::load(&lock).ok();
    if let Ok(mut m) = MANIFEST_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        memo_capped_insert(
            &mut m,
            MAX_MANIFEST_ENTRIES,
            root.to_path_buf(),
            (man.clone(), lck.clone(), mt_toml, mt_lock),
        );
    }
    (man, lck)
}

fn file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Memoized `(Manifest, Lockfile)` per project root. Both files must
/// parse; entries revalidate both files' mtimes on every hit (a
/// concurrent manifest/lock edit reloads instead of serving stale deps:
/// miss cost, never wrong data). Callers needing only the manifest
/// (no lock required) use [`manifest_of`].
pub(crate) fn manifest_lock(
    root: &std::path::Path,
) -> Option<(zz_pm::manifest::Manifest, zz_pm::lock::Lockfile)> {
    let (man, lck) = manifest_entry(root);
    Some((man?, lck?))
}

/// Memoized dependency file probes: `entry_in` (3 `is_file` stats per
/// dep) and `submodule_in` (2 stats) otherwise re-stat the same
/// `vendor/`/CAS/path-dep candidates for every importer (#233, item 3).
/// Dependency directories never change mid-run (discovery is upfront;
/// `--fix` only rewrites project `.zz` files), so results memoize for
/// the process lifetime like [`canon`] — including negatives. Eviction
/// only costs future hits, never correctness.
/// Probe memo table: (package dir, probe key) → resolved file (or `None`
/// when no candidate exists). Shared shape for [`ENTRY_MEMO`] and
/// [`SUBMODULE_MEMO`] (clippy `type_complexity` would fire on repeats).
type ProbeMemo = std::sync::Mutex<HashMap<(PathBuf, String), Option<PathBuf>>>;

static ENTRY_MEMO: std::sync::OnceLock<ProbeMemo> = std::sync::OnceLock::new();
static SUBMODULE_MEMO: std::sync::OnceLock<ProbeMemo> = std::sync::OnceLock::new();

/// Memoized dependency entry-file probe (`<pkgdir>/src/main.zz`,
/// `src/<dep>.zz`, `<dep>.zz`). `probe` stats the filesystem on miss.
pub(crate) fn entry_in_memo(
    pkg_dir: &std::path::Path,
    dep_name: &str,
    probe: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let key = (pkg_dir.to_path_buf(), dep_name.to_string());
    if let Some(hit) = ENTRY_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(&key).cloned())
    {
        return hit;
    }
    let v = probe();
    if let Ok(mut m) = ENTRY_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        memo_capped_insert(&mut m, MAX_FS_ENTRIES, key, v.clone());
    }
    v
}

/// Memoized dependency submodule probe (`<depdir>/src/<mod...>.zz`,
/// `<depdir>/<mod...>.zz`). `rest` is the dotted path below the dep.
pub(crate) fn submodule_in_memo(
    pkg_dir: &std::path::Path,
    rest: &[String],
    probe: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let key = (pkg_dir.to_path_buf(), rest.join("/"));
    if let Some(hit) = SUBMODULE_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(&key).cloned())
    {
        return hit;
    }
    let v = probe();
    if let Ok(mut m) = SUBMODULE_MEMO
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
    {
        memo_capped_insert(&mut m, MAX_FS_ENTRIES, key, v.clone());
    }
    v
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
    // Catch panics so process env is always restored and the lock never
    // poisons: one failing test must not cascade into opaque `PoisonError`
    // failures in every other cache test.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    match prev_xdg {
        Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
        None => std::env::remove_var("XDG_CACHE_HOME"),
    }
    match prev_flag {
        Some(v) => std::env::set_var("ZZ_CHECK_CACHE", v),
        None => std::env::remove_var("ZZ_CHECK_CACHE"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    match r {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e),
    }
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
            aliases: std::borrow::Cow::Owned(self.aliases.into_owned()),
            pub_aliases: std::borrow::Cow::Owned(self.pub_aliases.into_owned()),
            enums: std::borrow::Cow::Owned(self.enums.into_owned()),
            pub_enums: std::borrow::Cow::Owned(self.pub_enums.into_owned()),
            bindings: std::borrow::Cow::Owned(self.bindings.into_owned()),
            pub_bindings: std::borrow::Cow::Owned(self.pub_bindings.into_owned()),
            diags: self.diags,
        }
    }
}

/// Read a cached module. Memory first, then disk (which populates
/// memory). `None` on any failure (absent, corrupt, unreadable
/// dir) — the caller checks normally.
pub fn read_cached(key: &str) -> Option<CachedModule<'static>> {
    if !cache_enabled() {
        return None;
    }
    if let Some(m) = mem_get(key) {
        return Some(m);
    }
    let data = std::fs::read(cache_path(key)).ok()?;
    // Deserialized maps are always `Owned` (HashMaps cannot borrow);
    // `into_owned` below is a no-op move in that case.
    let m: CachedModule<'_> = serde_json::from_slice(&data).ok()?;
    let m = m.into_owned();
    mem_insert(key, m.clone(), data.len());
    Some(m)
}

/// Store a module outcome. Failures are silent (cache is best-effort).
pub fn write_cached(key: &str, module: &CachedModule) {
    if !cache_enabled() {
        return;
    }
    let Ok(data) = serde_json::to_vec(module) else {
        return;
    };
    mem_insert(key, module.clone().into_owned(), data.len());
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
    use std::borrow::Cow;

    #[test]
    fn dep_keys_differ_by_source_and_deps() {
        let g = genesis_key(&[]);
        let leaf_a = module_key(&g, "pub base := 41", &[], "config");
        let leaf_a2 = module_key(&g, "pub base := 41", &[], "config");
        let leaf_b = module_key(&g, "pub base := 42", &[], "config");
        let leaf_ns = module_key(&g, "pub base := 41", &[], "cfg");
        let mid_a = module_key(
            &g,
            "import config",
            &[("config.zz".to_string(), leaf_a.clone())],
            "main",
        );
        let mid_b = module_key(
            &g,
            "import config",
            &[("config.zz".to_string(), leaf_b.clone())],
            "main",
        );
        let mid_c = module_key(
            &g,
            "import config",
            &[("other.zz".to_string(), leaf_a.clone())],
            "main",
        );
        assert_eq!(leaf_a, leaf_a2, "deterministic");
        assert_ne!(leaf_a, leaf_b, "source sensitivity");
        assert_ne!(leaf_a, leaf_ns, "namespace sensitivity (#290)");
        assert_ne!(mid_a, mid_b, "dep content sensitivity");
        assert_ne!(mid_a, mid_c, "dep path sensitivity");
        assert_eq!(leaf_a.len(), 64, "sha256 hex");
    }

    #[test]
    fn genesis_folds_seed_fingerprint() {
        // #371: the S1 genesis must cover the stdlib seed content, so any
        // seed change (added/removed/renamed native) invalidates cached
        // outcomes — including stale failures recorded under an older
        // toolchain.
        let fp = seed_fingerprint();
        assert_eq!(fp.len(), 64, "sha256 hex");
        assert_eq!(fp, seed_fingerprint(), "deterministic");
        let g = genesis_key(&[]);
        assert!(
            g.contains(&format!("seed={fp}")),
            "genesis must embed the seed fingerprint, got: {g}"
        );
        // Different geneses key the same source differently: a seed change
        // misses instead of replaying the old entry.
        let other = format!("{g}|extra-seed-change");
        let k1 = module_key(&g, "println(term.poll())", &[], "main");
        let k2 = module_key(&other, "println(term.poll())", &[], "main");
        assert_ne!(k1, k2, "seed change must invalidate module keys");
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
                aliases: Cow::Owned(HashMap::new()),
                pub_aliases: Cow::Owned(HashMap::new()),
                enums: Cow::Owned(HashMap::new()),
                pub_enums: Cow::Owned(HashMap::new()),
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

    fn empty_module() -> CachedModule<'static> {
        CachedModule {
            funcs: Cow::Owned(HashMap::new()),
            pub_funcs: Cow::Owned(HashMap::new()),
            structs: Cow::Owned(HashMap::new()),
            pub_structs: Cow::Owned(HashMap::new()),
            aliases: Cow::Owned(HashMap::new()),
            pub_aliases: Cow::Owned(HashMap::new()),
            enums: Cow::Owned(HashMap::new()),
            pub_enums: Cow::Owned(HashMap::new()),
            bindings: Cow::Owned(HashMap::new()),
            pub_bindings: Cow::Owned(HashMap::new()),
            diags: Vec::new(),
        }
    }

    #[test]
    fn mem_memo_serves_after_disk_delete() {
        super::with_isolated_cache(|| {
            // Unique key: the memory memo is process-global (shared by
            // parallel tests), only the disk dir is isolated.
            let key = format!("zz-unit-mem-{}", std::process::id());
            let m = empty_module();
            write_cached(&key, &m);
            assert!(read_cached(&key).is_some(), "disk hit");
            std::fs::remove_file(cache_path(&key)).unwrap();
            assert!(
                read_cached(&key).is_some(),
                "memory memo must serve after the disk entry is gone"
            );
            let _ = std::fs::remove_file(cache_path(&key));
        });
    }

    #[test]
    fn mem_memo_serves_big_entries_after_disk_delete() {
        // The memo is bounded by total bytes (not per-entry size): even a
        // 2250-pub monster entry must serve from memory.
        super::with_isolated_cache(|| {
            let key = format!("zz-unit-big-{}", std::process::id());
            let mut m = empty_module();
            for i in 0..20000 {
                m.bindings
                    .to_mut()
                    .insert(format!("var_{i}_padding_padding"), Type::Int);
            }
            write_cached(&key, &m);
            std::fs::remove_file(cache_path(&key)).unwrap();
            assert!(
                read_cached(&key).is_some(),
                "big entries must serve from memory after the disk entry is gone"
            );
            let _ = std::fs::remove_file(cache_path(&key));
        });
    }

    fn parse_program(src: &str) -> Program {
        let parsed = zz_frontend::parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        parsed.program
    }

    #[test]
    fn parsed_memo_roundtrip() {
        // Process-global like the outcome memo: unique path per test only
        // needs unique content (keys are content hashes).
        let canon = PathBuf::from(format!("/tmp/zz-unit-parsed-{}", std::process::id()));
        let src = "pub func answer() -> int {\n return 41\n}\n";
        let hash = source_hash(src);
        assert!(parsed_get(&canon, &hash).is_none(), "cold miss");
        let program = parse_program(src);
        parsed_insert(&canon, &hash, &program, src.len());
        let back = parsed_get(&canon, &hash).expect("warm hit");
        assert_eq!(back, program, "memo must serve identical AST");
    }

    #[test]
    fn parsed_memo_content_sensitive() {
        let canon = PathBuf::from(format!(
            "/tmp/zz-unit-parsed-sensitive-{}",
            std::process::id()
        ));
        let program = parse_program("pub func v() -> int {\n return 1\n}\n");
        let h1 = source_hash("pub func v() -> int {\n return 1\n}\n");
        parsed_insert(&canon, &h1, &program, 28);
        let h2 = source_hash("pub func v() -> int {\n return 2\n}\n");
        assert!(
            parsed_get(&canon, &h2).is_none(),
            "edited content must miss"
        );
        let other = PathBuf::from(format!("/tmp/zz-unit-parsed-other-{}", std::process::id()));
        assert!(
            parsed_get(&other, &h1).is_none(),
            "different path must miss"
        );
    }

    #[test]
    fn canon_memo_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zz-unit-canon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.zz");
        std::fs::write(&file, b"x := 1\n").unwrap();
        let direct = file.canonicalize().expect("setup");
        assert_eq!(canon(&file), Ok(direct.clone()), "hit must equal raw");
        assert_eq!(canon(&file), Ok(direct), "repeat must serve memoized");
        let missing = dir.join("nope.zz");
        assert!(canon(&missing).is_err(), "absent path errors");
        assert!(canon(&missing).is_err(), "negative hit stays error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_and_submodule_memos_cache_probes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Process-global tables: unique dirs per test (pids collide
        // across parallel tests only via the same dir — include a
        // per-test tag).
        let dir = std::env::temp_dir().join(format!(
            "zz-unit-entrymemo-{}-{}",
            std::process::id(),
            "233a"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src").join("main.zz"),
            b"pub func f() -> int { return 1 }\n",
        )
        .unwrap();
        let calls = AtomicUsize::new(0);
        let probe = || {
            calls.fetch_add(1, Ordering::Relaxed);
            Some(dir.join("src").join("main.zz"))
        };
        let first = entry_in_memo(&dir, "some-dep-233a", probe);
        assert!(first.is_some(), "positive probe");
        let calls2 = AtomicUsize::new(0);
        let second = entry_in_memo(&dir, "some-dep-233a", || {
            calls2.fetch_add(1, Ordering::Relaxed);
            None
        });
        assert_eq!(first, second, "repeat must serve memoized");
        assert_eq!(calls.load(Ordering::Relaxed), 1, "probe ran once");
        assert_eq!(calls2.load(Ordering::Relaxed), 0, "repeat must not probe");
        // Different dep name misses (same dir).
        assert!(
            entry_in_memo(&dir, "other-dep-233a", || None).is_none(),
            "negative probe"
        );
        assert!(
            entry_in_memo(&dir, "other-dep-233a", || {
                panic!("negative hit must not re-probe")
            })
            .is_none(),
            "negative hit stands"
        );
        // Submodule memo keyed by (dir, rest path).
        let rest = vec!["mod0".to_string()];
        let sub = submodule_in_memo(&dir, &rest, || Some(dir.join("src").join("main.zz")));
        assert!(sub.is_some());
        let sub2 = submodule_in_memo(&dir, &rest, || panic!("submodule hit must not re-probe"));
        assert_eq!(sub, sub2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_lock_negative_memo() {
        let dir = std::env::temp_dir().join(format!("zz-unit-manneg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(manifest_lock(&dir).is_none(), "no manifest/lock -> None");
        assert!(manifest_lock(&dir).is_none(), "negative hit stands");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_lock_positive_memo() {
        let dir = std::env::temp_dir().join(format!("zz-unit-manpos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("zz.toml"),
            "[package]\nname = \"memo-test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("zz.lock"),
            "version = 1\ndeps = []\nchecksum = \"\"\n",
        )
        .unwrap();
        let first = manifest_lock(&dir).expect("both files present");
        assert_eq!(first.0.package.name, "memo-test");
        assert!(first.1.deps.is_empty());
        let second = manifest_lock(&dir).expect("mtime-validated hit");
        assert_eq!(second.0.package.name, "memo-test");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
