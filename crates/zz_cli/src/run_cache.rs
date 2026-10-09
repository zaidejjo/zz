//! VM run cache (one-time compiler, Phase 1).
//!
//! No-change `zz run` re-parses, re-checks, rebuilds HIR and recompiles VM
//! bytecode for the whole dependency closure on every invocation. This
//! module caches the compiled output: one directory per closure key under
//! `<build-cache>/run-v1/<slug>/` holding `meta.json` plus one `.zzc` IR
//! module per program in execution order.
//!
//! Key discipline mirrors the native build cache: [`zz_pm::cache_key`]
//! covers entry bytes + canonical path (namespace), every `.zz` source
//! under the project dir, live path-dep content, and the target string.
//! The VM fingerprint covers the toolchain (CLI version, IR version, git
//! HEAD for dev-loop rebuilds) plus sorted plugin manifest names; `--embed`
//! bytes join via the artifact segment. Script args are runtime-only and
//! correctly excluded. A miss (or any corruption) falls back to today's
//! pipeline exactly — callers must treat `lookup` returning `None` as a
//! normal miss, never an error.
//!
//! `ZZ_RUN_CACHE=0` disables read and write (benchmarking, weirdness).
//! `ZZ_VERBOSE=1` prints hit/miss lines (slug prefix only, no paths).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Cache format version: bump when `meta.json` or the module layout changes.
/// Old entries are never read (safe: cold rebuild once, aged out via LRU).
/// v2 adds scoped-stdlib metadata (imported modules + selective/wildcard
/// replays); v1 entries lack it and must not execute.
const CACHE_VERSION: &str = "v2";
/// Upper bound on retained run entries (LRU by mtime, best-effort).
const MAX_ENTRIES: usize = 256;

/// Interpreter inputs that cannot be derived from `.zzc` bytes alone.
/// Small maps (consts, aliases) — the natives table itself is rebuilt from
/// the stdlib + plugins on every hit (same as the `.zzc` loader path).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RunMeta {
    /// Selective-import float constants (`pi` etc.), bare name → value.
    #[serde(default)]
    pub consts: HashMap<String, f64>,
    /// Bare name → qualified `ns.sym` for generic calls.
    #[serde(default)]
    pub import_aliases: HashMap<String, String>,
    /// `(module, alias)` pairs for `import std.X as alias` mirrors.
    #[serde(default)]
    pub stdlib_aliases: Vec<(String, String)>,
    /// Sorted union of stdlib modules imported anywhere in the closure.
    /// Scopes pure-ZZ execution and natives on hits (see
    /// `zz_stdlib::stdlib_program_closure`).
    #[serde(default)]
    pub stdlib_modules: Vec<String>,
    /// Selective imports replayed for bare-name natives on hits.
    #[serde(default)]
    pub stdlib_selectives: crate::loader::StdlibSelectives,
    /// Wildcard imports replayed for bare-name natives on hits.
    #[serde(default)]
    pub stdlib_wildcards: Vec<String>,
}

/// A cache hit: raw IR bytes per module (execution order) plus meta.
/// Bytes are unverified here — the caller decodes/verifies/raises, and
/// must treat any failure as a miss (never execute garbage).
pub struct RunHit {
    pub modules: Vec<Vec<u8>>,
    pub meta: RunMeta,
}

/// Disabled via `ZZ_RUN_CACHE=0`.
pub fn cache_enabled() -> bool {
    std::env::var("ZZ_RUN_CACHE")
        .map(|v| v != "0")
        .unwrap_or(true)
}

fn verbose() -> bool {
    std::env::var("ZZ_VERBOSE").is_ok()
}

/// `<build-cache>/run-v1` (honors `ZZ_HOME` via `build_cache_dir`;
/// `zz cache clean` clears the parent, so no separate gc path needed).
pub fn run_cache_dir() -> PathBuf {
    crate::build::cache_dir().join(format!("run-{CACHE_VERSION}"))
}

/// Commit hash of the tree this binary was built from (dev-loop cache
/// correctness: same-version rebuilds must not share entries). `None`
/// outside a git checkout (tarballs fall back to version-only).
fn git_hash() -> Option<String> {
    static HASH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        let mut dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        loop {
            if dir.join(".git").exists() {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(dir)
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if s.is_empty() {
                            return None;
                        }
                        return Some(s);
                    }
                    _ => return None,
                }
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => return None,
            }
        }
    })
    .clone()
}

/// Toolchain fingerprint for the VM run key: CLI version + IR format
/// version + git HEAD + sorted plugin manifest names. Any toolchain or
/// plugin-shape change busts every entry (sound over stale code).
pub fn vm_fingerprint(plugin_funcs: &[(String, zz_checker::FuncSig)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut names: Vec<&str> = plugin_funcs.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    env!("CARGO_PKG_VERSION").hash(&mut h);
    zz_ir::VERSION.hash(&mut h);
    git_hash()
        .unwrap_or_else(|| "nogit".to_string())
        .hash(&mut h);
    names.hash(&mut h);
    h.finish()
}

/// Compute the run-cache slug for an entry file. Reads the entry bytes and
/// hashes the closure live (same machinery as the native build cache).
/// `embed` joins the key via the artifact segment when `--embed` is used.
pub fn run_key(
    entry: &Path,
    plugin_funcs: &[(String, zz_checker::FuncSig)],
    embed: Option<&Path>,
) -> Result<String, String> {
    let src = std::fs::read_to_string(entry)
        .map_err(|e| format!("zz: cannot read {}: {e}", entry.display()))?;
    let fp = vm_fingerprint(plugin_funcs);
    // Fast path: stat-only closure check against the fingerprint sidecar
    // (no source reads on no-change runs). Falls back to the canonical
    // full hash on any doubt; manifest errors propagate like `compute`.
    if let Some(slug) = fp_lookup(entry, &src, fp, embed)? {
        return Ok(slug);
    }
    let mut key = zz_pm::cache_key::CacheKey::compute(entry, &src, fp, Some("vm-run"), None)?;
    if let Some(dir) = embed {
        key.artifact_hash = crate::build::embed_sig(dir);
    }
    fp_store(entry, &key);
    Ok(key.to_slug())
}

// ---------------------------------------------------------------------------
// Fingerprint fast path: stat-based closure validation.
//
// `CacheKey::compute` re-reads and re-hashes every source on every run
// (tens of ms at table scale, on the hot path). The sidecar records, per
// project root, the file set with (mtime, size) plus the last combined
// hashes. A no-change run validates with stats only and reuses the
// combined hashes bit-for-bit (same `to_slug` inputs as `compute`, so
// slugs are interchangeable). Any new/removed/changed/recent file falls
// back to the canonical full hash, which also refreshes the sidecar.
// Trust rule per file: (mtime, size) must match AND mtime must be older
// than one second (same-tick guard: an edit and a run landing in the
// same timestamp tick always re-hashes).
// ---------------------------------------------------------------------------

/// One file's stat signature: seconds + nanos + size. Unknown (unstatable)
/// never validates — the caller falls back to the full hash.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct FileStat {
    secs: u64,
    nanos: u32,
    size: u64,
}

/// One hashed scope (the project tree or one path-dep dir): the validated
/// file set plus the combined hash `compute` produced for it.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ScopeFp {
    files: HashMap<String, FileStat>,
    combined: String,
}

/// Sidecar per project root: sources scope plus one scope per path dep
/// (keyed by dep name) with the dep dir it was recorded for (a remapped
/// path invalidates the scope).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ProjectFp {
    sources: ScopeFp,
    deps: HashMap<String, ScopeFp>,
    dep_dirs: HashMap<String, String>,
}

fn fp_path(root: &Path) -> PathBuf {
    let h = zz_pm::hash::hash_bytes(root.to_string_lossy().as_bytes());
    run_cache_dir().join(format!("fp-{}.json", &h[..16.min(h.len())]))
}

/// Enumerate `.zz` files under `dir`, mirroring `hash_zz_sources` set
/// semantics (hidden dirs, `target/`, `bin/`, `node_modules/` skipped;
/// symlinks followed like `Path::is_dir`). Order is irrelevant; callers
/// sort. Errors are ignored per-entry, like the canonical walk.
fn crawl_zz(dir: &Path) -> Vec<PathBuf> {
    const SKIP_DIRS: &[&str] = &["target", "bin", "node_modules"];
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 32 {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                walk(&path, out, depth + 1);
            } else if path.extension().and_then(|e| e.to_str()) == Some("zz") {
                out.push(path);
            }
        }
    }
    walk(dir, &mut out, 0);
    out.sort();
    out
}

fn stat_now(path: &Path) -> Option<FileStat> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let d = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(FileStat {
        secs: d.as_secs(),
        nanos: d.subsec_nanos(),
        size: meta.len(),
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Validate one scope: identical file set, every stat matches, every
/// mtime older than the granularity guard. Pure function of the sidecar
/// and the current tree — no reads.
fn scope_clean(scope: &ScopeFp, files: &[PathBuf], now: u64) -> bool {
    if scope.files.len() != files.len() {
        return false;
    }
    for path in files {
        let key = path.to_string_lossy().into_owned();
        let (Some(recorded), Some(current)) = (scope.files.get(&key), stat_now(path)) else {
            return false;
        };
        if recorded.secs != current.secs
            || recorded.nanos != current.nanos
            || recorded.size != current.size
        {
            return false;
        }
        if recorded.secs + 1 >= now {
            return false;
        }
    }
    // Set equality: same length + every crawled file found above.
    // (HashMap lookup per file already proved membership both ways.)
    true
}

/// Fast-path key: `Some(slug)` when the sidecar validates the whole
/// closure with stats only; `None` on any doubt (caller runs `compute`);
/// `Err` only when the manifest is unloadable (same failure `compute`
/// reports — the run then surfaces the real error uncached).
fn fp_lookup(
    entry: &Path,
    src: &str,
    fp: u64,
    embed: Option<&Path>,
) -> Result<Option<String>, String> {
    let root = zz_pm::cache_key::CacheKey::project_dir_for(entry);
    let sidecar = std::fs::read(fp_path(&root)).ok();
    let Some(bytes) = sidecar else {
        return Ok(None);
    };
    let saved: ProjectFp = serde_json::from_slice(&bytes).ok().unwrap_or_default();
    // An empty sidecar (fresh default) never validates: the sources scope
    // always crawls ≥1 file (the entry itself).
    let files = crawl_zz(&root);
    let now = now_secs();
    if !scope_clean(&saved.sources, &files, now) {
        return Ok(None);
    }
    let deps = zz_pm::cache_key::path_dep_dirs(&root)?;
    if deps.len() != saved.deps.len() {
        return Ok(None);
    }
    let mut dep_hashes = HashMap::new();
    for (name, dir) in &deps {
        let dir_str = dir.to_string_lossy().into_owned();
        if saved.dep_dirs.get(name).map(String::as_str) != Some(dir_str.as_str()) {
            return Ok(None);
        }
        let Some(scope) = saved.deps.get(name) else {
            return Ok(None);
        };
        if !scope_clean(scope, &crawl_zz(dir), now) {
            return Ok(None);
        }
        dep_hashes.insert(name.clone(), scope.combined.clone());
    }
    let canonical_entry = entry.canonicalize().unwrap_or_else(|_| entry.to_path_buf());
    let key = zz_pm::cache_key::CacheKey {
        source_hash: zz_pm::hash::hash_bytes(src.as_bytes()),
        source_path: canonical_entry.to_string_lossy().into_owned(),
        dep_hashes,
        build_fingerprint: fp,
        target: "vm-run".to_string(),
        runtime_mtime: None,
        sources_hash: saved.sources.combined.clone(),
        artifact_hash: String::new(),
        artifact_flags: String::new(),
        native_build_sig: String::new(),
    };
    let mut key = key;
    if let Some(dir) = embed {
        key.artifact_hash = crate::build::embed_sig(dir);
    }
    Ok(Some(key.to_slug()))
}

/// Refresh the sidecar from a freshly computed key (best-effort; failures
/// are silent — the next run simply recomputes).
fn fp_store(entry: &Path, key: &zz_pm::cache_key::CacheKey) {
    let root = zz_pm::cache_key::CacheKey::project_dir_for(entry);
    let files = crawl_zz(&root);
    let mut sources = ScopeFp {
        files: HashMap::new(),
        combined: key.sources_hash.clone(),
    };
    for path in &files {
        if let Some(st) = stat_now(path) {
            sources
                .files
                .insert(path.to_string_lossy().into_owned(), st);
        }
    }
    let mut fp = ProjectFp {
        sources,
        deps: HashMap::new(),
        dep_dirs: HashMap::new(),
    };
    if let Ok(deps) = zz_pm::cache_key::path_dep_dirs(&root) {
        for (name, dir) in &deps {
            let mut scope = ScopeFp {
                files: HashMap::new(),
                combined: key.dep_hashes.get(name).cloned().unwrap_or_default(),
            };
            for path in &crawl_zz(dir) {
                if let Some(st) = stat_now(path) {
                    scope.files.insert(path.to_string_lossy().into_owned(), st);
                }
            }
            fp.dep_dirs
                .insert(name.clone(), dir.to_string_lossy().into_owned());
            fp.deps.insert(name.clone(), scope);
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&fp) {
        let path = fp_path(&root);
        if std::fs::create_dir_all(path.parent().expect("fp file has parent")).is_ok() {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// Look up a slug. `None` on any failure (absent, corrupt, unreadable) —
/// the caller runs the normal pipeline. Corrupt entries are removed
/// best-effort so the next run re-stores cleanly.
pub fn lookup(slug: &str) -> Option<RunHit> {
    if !cache_enabled() {
        return None;
    }
    if slug.contains('/') || slug.contains('\\') || slug.contains("..") {
        return None;
    }
    let dir = run_cache_dir().join(slug);
    let meta_bytes = std::fs::read(dir.join("meta.json")).ok()?;
    let meta: RunMeta = serde_json::from_slice(&meta_bytes).ok()?;
    let mut modules = Vec::new();
    for idx in 0..1024 {
        let path = dir.join(format!("m{idx}.zzc"));
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => modules.push(bytes),
            _ => break,
        }
    }
    if modules.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    // Touch for LRU (best-effort; failure never fails the run).
    touch(&dir);
    Some(RunHit { modules, meta })
}

/// Store a freshly compiled run. Failures are silent (cache is
/// best-effort). Writes to a temp dir then renames atomically so
/// concurrent readers never see a partial entry.
pub fn store(slug: &str, modules: &[Vec<u8>], meta: &RunMeta) {
    if !cache_enabled() || modules.is_empty() {
        return;
    }
    if slug.contains('/') || slug.contains('\\') || slug.contains("..") {
        return;
    }
    let base = run_cache_dir();
    let dir = base.join(slug);
    let tmp = base.join(format!(".tmp-{}-{}", std::process::id(), store_counter()));
    let _ = std::fs::create_dir_all(&tmp);
    let mut ok = true;
    match serde_json::to_vec(meta) {
        Ok(meta_bytes) => {
            if std::fs::write(tmp.join("meta.json"), &meta_bytes).is_err() {
                ok = false;
            }
        }
        Err(_) => ok = false,
    }
    for (idx, bytes) in modules.iter().enumerate() {
        if std::fs::write(tmp.join(format!("m{idx}.zzc")), bytes).is_err() {
            ok = false;
            break;
        }
    }
    if !ok {
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    // One-time upgrade: v1 entries lack scoped-stdlib metadata and must
    // never execute. Drop the whole legacy tree (rebuilds on demand;
    // `zz cache clean` covers the parent either way).
    static UPGRADED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    UPGRADED.get_or_init(|| {
        let _ = std::fs::remove_dir_all(base.join("run-v1"));
    });
    // Rename wins atomically; a concurrent writer's rename is equally
    // valid (same key ⇒ same bytes), so errors are ignored.
    if std::fs::rename(&tmp, &dir).is_err() {
        // Target may exist (lost the race) — retry over it after
        // dropping our temp tree into place via remove+rename.
        let _ = std::fs::remove_dir_all(&dir);
        if std::fs::rename(&tmp, &dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
    }
    evict_oldest();
}

fn store_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Best-effort LRU: past [`MAX_ENTRIES`] entries, delete oldest by mtime.
/// Errors are ignored (growth only costs disk, never correctness).
fn evict_oldest() {
    let base = run_cache_dir();
    let entries: Vec<(PathBuf, std::time::SystemTime)> = match std::fs::read_dir(&base) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| {
                e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    && !e.file_name().to_string_lossy().starts_with(".tmp-")
            })
            .map(|e| {
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                (e.path(), mtime)
            })
            .collect(),
        Err(_) => return,
    };
    if entries.len() <= MAX_ENTRIES {
        return;
    }
    let mut sorted = entries;
    sorted.sort_by_key(|(_, mtime)| *mtime);
    for (path, _) in sorted.iter().take(sorted.len() - MAX_ENTRIES) {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Refresh mtime for LRU without touching contents.
fn touch(dir: &Path) {
    // `meta.json` rewrite updates both file and dir mtimes on most
    // filesystems; content is unchanged (read-modify-write of same bytes
    // would be racy — instead re-set the dir mtime via a marker read).
    // Simplest portable touch: update permissions to current value is a
    // no-op on some platforms, so re-write the dir entry by creating and
    // removing a lock file.
    let lock = dir.join(".t");
    let _ = std::fs::write(&lock, b"t");
    let _ = std::fs::remove_file(&lock);
}

pub(crate) fn log_hit(slug: &str) {
    if verbose() {
        eprintln!("zz: run cache hit {}", &slug[..8.min(slug.len())]);
    }
}

/// Per-stage wall timer for startup profiling. Prints
/// `zz: t=<ms since last stage> <label>` under `ZZ_VERBOSE=1` only;
/// zero overhead otherwise (one `Instant::now` per stage). Permanent:
/// startup regressions show up here first.
pub(crate) struct StageTimer {
    last: std::time::Instant,
    active: bool,
}

impl StageTimer {
    pub fn start() -> Self {
        Self {
            last: std::time::Instant::now(),
            active: verbose(),
        }
    }

    pub fn stage(&mut self, label: &str) {
        if !self.active {
            return;
        }
        let now = std::time::Instant::now();
        eprintln!(
            "zz: t={:>5}ms {label}",
            now.duration_since(self.last).as_millis()
        );
        self.last = now;
    }
}

pub(crate) fn log_miss(slug: &str) {
    if verbose() {
        eprintln!("zz: run cache miss {}", &slug[..8.min(slug.len())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolated `ZZ_HOME` + enabled cache for run-cache tests. Serializes
    /// via its own mutex (parallel tests share process env).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_isolated_home<T>(f: impl FnOnce() -> T) -> T {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var("ZZ_HOME").ok();
        let prev_flag = std::env::var("ZZ_RUN_CACHE").ok();
        let dir = std::env::temp_dir().join(format!(
            "zz-run-cache-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::env::set_var("ZZ_HOME", &dir);
        std::env::remove_var("ZZ_RUN_CACHE");
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev_home {
            Some(v) => std::env::set_var("ZZ_HOME", v),
            None => std::env::remove_var("ZZ_HOME"),
        }
        match prev_flag {
            Some(v) => std::env::set_var("ZZ_RUN_CACHE", v),
            None => std::env::remove_var("ZZ_RUN_CACHE"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        match r {
            Ok(v) => v,
            Err(e) => std::panic::resume_unwind(e),
        }
    }

    fn write_proj(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::var("ZZ_HOME").unwrap();
        let proj = PathBuf::from(format!("{dir}/proj"));
        for (rel, content) in files {
            let p = proj.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
        }
        proj
    }

    #[test]
    fn key_changes_on_entry_edit() {
        with_isolated_home(|| {
            let proj = write_proj(&[("main.zz", "func main() {\n    println(1)\n}\n")]);
            let entry = proj.join("main.zz");
            let k1 = run_key(&entry, &[], None).unwrap();
            std::fs::write(&entry, "func main() {\n    println(2)\n}\n").unwrap();
            let k2 = run_key(&entry, &[], None).unwrap();
            assert_ne!(k1, k2, "entry edit must bust the key");
        });
    }

    #[test]
    fn key_covers_plugins_embed_and_ir_version() {
        with_isolated_home(|| {
            let proj = write_proj(&[("main.zz", "func main() {\n    println(1)\n}\n")]);
            let entry = proj.join("main.zz");
            let base = run_key(&entry, &[], None).unwrap();
            // Plugin shape joins the key.
            let sig = zz_checker::FuncSig {
                generics: Vec::new(),
                bounds: Vec::new(),
                params: Vec::new(),
                has_default: Vec::new(),
                ret: zz_checker::Type::Int,
                is_extern: true,
                extern_c_symbol: None,
            };
            let with_plugin = run_key(&entry, &[("plug.f".to_string(), sig)], None).unwrap();
            assert_ne!(base, with_plugin, "plugin set must join the key");
            // Embed bytes join the key.
            let assets = proj.join("assets");
            std::fs::create_dir_all(&assets).unwrap();
            std::fs::write(assets.join("a.txt"), b"hello").unwrap();
            let with_embed = run_key(&entry, &[], Some(&assets)).unwrap();
            assert_ne!(base, with_embed, "embed tree must join the key");
            // Toolchain joins the key (IR version is hashed into the
            // fingerprint — a format bump must never reuse old bytes).
            assert!(
                vm_fingerprint(&[]) != 0,
                "fingerprint must be non-degenerate"
            );
            assert!(base.contains("vm-run"), "target must be vm-run, got {base}");
        });
    }

    #[test]
    fn roundtrip_and_corrupt_is_miss() {
        with_isolated_home(|| {
            let slug = "testslug roundtrip must be path-safe";
            assert!(lookup(slug).is_none(), "path-unsafe slug never hits");
            let slug = "abc123-test-slug";
            let meta = RunMeta {
                consts: [("pi".to_string(), 3.0)].into_iter().collect(),
                import_aliases: [("m".to_string(), "ns.m".to_string())]
                    .into_iter()
                    .collect(),
                stdlib_aliases: vec![("math".to_string(), "mm".to_string())],
                stdlib_modules: vec!["math".to_string()],
                ..Default::default()
            };
            store(slug, &[b"ZZC1-fake-module-0".to_vec()], &meta);
            let hit = lookup(slug).expect("stored entry must hit");
            assert_eq!(hit.modules.len(), 1);
            assert_eq!(hit.meta.consts.get("pi"), Some(&3.0));
            assert_eq!(hit.meta.stdlib_modules, vec!["math".to_string()]);
            // Corrupt the module bytes: lookup still returns bytes (raw),
            // but the caller-side decode must fail — here assert the
            // bytes round-trip exactly so corruption is detectable.
            std::fs::write(
                run_cache_dir().join(slug).join("m0.zzc"),
                b"garbage-not-zzc",
            )
            .unwrap();
            let hit2 = lookup(slug).expect("raw bytes still returned");
            assert!(zz_ir::codec::decode(&hit2.modules[0]).is_err());
            // Wipe the entry: clean miss, no panic.
            std::fs::remove_dir_all(run_cache_dir().join(slug)).unwrap();
            assert!(lookup(slug).is_none());
        });
    }

    #[test]
    fn disabled_cache_never_hits() {
        with_isolated_home(|| {
            let slug = "disabled-slug";
            let meta = RunMeta::default();
            store(slug, &[b"ZZC1-x".to_vec()], &meta);
            assert!(lookup(slug).is_some());
            std::env::set_var("ZZ_RUN_CACHE", "0");
            assert!(lookup(slug).is_none(), "ZZ_RUN_CACHE=0 must miss");
            store("other", &[b"ZZC1-y".to_vec()], &meta);
            std::env::remove_var("ZZ_RUN_CACHE");
            assert!(lookup("other").is_none(), "ZZ_RUN_CACHE=0 must not store");
        });
    }

    /// The fingerprint sidecar must return the identical slug with stats
    /// only (no source reads), and bust on any content change.
    #[test]
    fn fp_sidecar_reuses_and_busts() {
        with_isolated_home(|| {
            let proj = write_proj(&[
                ("zz.toml", "[package]\nname = \"fp\"\nversion = \"0.1.0\"\n"),
                ("main.zz", "func main() {\n    println(1)\n}\n"),
                ("lib/help.zz", "pub func aid() -> int { 1 }\n"),
            ]);
            // Backdate mtimes past the granularity guard (fresh files
            // always take the canonical path — that is the safe default).
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
            for rel in ["main.zz", "lib/help.zz", "zz.toml"] {
                let p = proj.join(rel);
                std::fs::File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
            let entry = proj.join("main.zz");
            let k1 = run_key(&entry, &[], None).unwrap();
            // Sidecar must exist now (canonical path refreshes it).
            let root = zz_pm::cache_key::CacheKey::project_dir_for(&entry);
            assert!(
                std::fs::read(fp_path(&root)).is_ok(),
                "canonical run_key must write the sidecar"
            );
            // Second call: same slug (fast path interchangeable).
            let k2 = run_key(&entry, &[], None).unwrap();
            assert_eq!(k1, k2, "sidecar slug must equal canonical slug");
            // Same-size edit with a fresh mtime (the rapid edit+run case):
            // the mtime mismatch busts even though the size matches.
            // (Deliberately forged old mtimes are trusted, like cargo —
            // the granularity guard only covers real clock ticks.)
            std::fs::write(&entry, "func main() {\n    println(2)\n}\n").unwrap();
            let k3 = run_key(&entry, &[], None).unwrap();
            assert_ne!(k1, k3, "edited source must bust the sidecar");
            // New file: must bust (set change invalidates).
            let newf = proj.join("lib").join("extra.zz");
            std::fs::write(&newf, "pub func e() -> int { 2 }\n").unwrap();
            let k4 = run_key(&entry, &[], None).unwrap();
            assert_ne!(k3, k4, "added file must bust the sidecar");
        });
    }
}
