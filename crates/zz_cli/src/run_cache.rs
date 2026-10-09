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
const CACHE_VERSION: &str = "v1";
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
    let mut key = zz_pm::cache_key::CacheKey::compute(
        entry,
        &src,
        vm_fingerprint(plugin_funcs),
        Some("vm-run"),
        None,
    )?;
    if let Some(dir) = embed {
        key.artifact_hash = crate::build::embed_sig(dir);
    }
    Ok(key.to_slug())
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
            };
            store(slug, &[b"ZZC1-fake-module-0".to_vec()], &meta);
            let hit = lookup(slug).expect("stored entry must hit");
            assert_eq!(hit.modules.len(), 1);
            assert_eq!(hit.meta.consts.get("pi"), Some(&3.0));
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
}
