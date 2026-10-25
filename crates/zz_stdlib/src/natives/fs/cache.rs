//! Process-wide, stat-validated content cache for `std.fs` reads.
//!
//! # Why
//!
//! Compiler-shaped workloads read the same file several times in one run:
//! `fs.scan_counts` (read + classify), then `fs.is_generated` (read +
//! heuristic), then maybe `fs.read` (read + UTF-8 validation). Without a
//! cache every call pays `open` + `fstat` + `read` + `close` + allocation,
//! so the total cost scales with *calls × file size*.
//!
//! This cache turns the second and later reads of an unchanged file into a
//! single `metadata()` probe plus an `Arc` bump: O(1) in file size and no
//! allocation. Cached buffers are shared by `read`, `read_bytes`,
//! `scan_counts`, and `is_generated` (including the `*_at` OS provider
//! path), and UTF-8 / NUL checks are memoized alongside the bytes.
//!
//! # Freshness
//!
//! Entries are keyed by path and validated against `len`, `mtime`, `ctime`,
//! `ino`, and `dev` on Unix (`len` + `mtime` elsewhere) on **every** hit, so
//! writes made outside ZZ are observed. `std.fs` mutators additionally
//! invalidate eagerly, which also covers coarse timestamp filesystems.
//!
//! # Bounds
//!
//! The cache is sharded (32 independent `Mutex` shards), FIFO-evicts per
//! shard, skips empty files and files larger than 8 MiB, and defaults to a
//! 96 MiB total budget. `ZZ_FS_CACHE=0` disables it; `ZZ_FS_CACHE_MB=<n>`
//! changes the budget (`0` disables).

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const SHARDS: usize = 32;
const DEFAULT_BUDGET_MB: usize = 96;
const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENTRIES_PER_SHARD: usize = 512;

/// Structural identity of a file. Two probes with equal fingerprints may
/// share cached bytes; any difference forces a fresh read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fingerprint {
    len: u64,
    mtime_sec: i64,
    mtime_nsec: u32,
    #[cfg(unix)]
    ctime_sec: i64,
    #[cfg(unix)]
    ctime_nsec: u32,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

struct Config {
    enabled: bool,
    shard_budget: usize,
    max_entry: usize,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Whether the content cache is active for this process.
pub(crate) fn enabled() -> bool {
    config().enabled
}

fn config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let enabled = std::env::var_os("ZZ_FS_CACHE")
            .map(|v| v != "0")
            .unwrap_or(true);
        // Parse as signed so a negative value disables the cache exactly
        // like the AOT runtime's `strtol` check (`n <= 0`).
        let mb = std::env::var("ZZ_FS_CACHE_MB")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(DEFAULT_BUDGET_MB as i64)
            .min(1_048_576); // 1 TiB ceiling (same as the AOT runtime)
        let budget = usize::try_from(mb).unwrap_or(0).saturating_mul(1024 * 1024);
        Config {
            enabled: enabled && budget > 0,
            shard_budget: (budget / SHARDS).max(64 * 1024),
            max_entry: MAX_ENTRY_BYTES.min(budget.max(1)),
        }
    })
}

/// A cached file: immutable bytes plus lazily computed text/UTF-8/NUL views.
pub(crate) struct CachedFile {
    fingerprint: Fingerprint,
    seq: u64,
    bytes: Arc<Vec<u8>>,
    text: OnceLock<Option<Arc<String>>>,
    has_nul: OnceLock<bool>,
    utf8_valid: OnceLock<bool>,
}

impl CachedFile {
    /// Shared raw contents (`read_bytes` / `scan_counts` / `is_generated`).
    pub(crate) fn bytes(&self) -> &Arc<Vec<u8>> {
        &self.bytes
    }

    /// Shared UTF-8 text, or `None` when the bytes are not valid UTF-8.
    /// Computed once; repeated `fs.read` calls are an `Arc` clone.
    pub(crate) fn text(&self) -> Option<Arc<String>> {
        // A prior `is_utf8`/`scan_counts` may already have rejected the
        // buffer; do not scan a large invalid file again just to fail.
        if self.utf8_valid.get() == Some(&false) {
            return None;
        }
        self.text
            .get_or_init(|| {
                let text = std::str::from_utf8(&self.bytes)
                    .ok()
                    .map(|s| Arc::new(s.to_owned()));
                // Reuse this validation for `is_utf8` (a `read` followed by
                // `scan_counts` must not scan the buffer twice).
                let _ = self.utf8_valid.set(text.is_some());
                text
            })
            .clone()
    }

    /// Memoized NUL scan (binary sniff).
    pub(crate) fn has_nul(&self) -> bool {
        *self
            .has_nul
            .get_or_init(|| memchr::memchr(0, &self.bytes).is_some())
    }

    /// Memoized strict UTF-8 validity.
    pub(crate) fn is_utf8(&self) -> bool {
        if let Some(text) = self.text.get() {
            return text.is_some();
        }
        *self
            .utf8_valid
            .get_or_init(|| std::str::from_utf8(&self.bytes).is_ok())
    }
}

struct Slot {
    key: Box<str>,
    seq: u64,
}

#[derive(Default)]
struct Shard {
    map: HashMap<Box<str>, Arc<CachedFile>>,
    /// Insertion order per shard. Stale slots (replaced or evicted) are
    /// skipped by matching `seq`, which keeps lookups and inserts O(1).
    order: VecDeque<Slot>,
    bytes: usize,
}

static SHARDS_DATA: OnceLock<[Mutex<Shard>; SHARDS]> = OnceLock::new();
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

fn shards() -> &'static [Mutex<Shard>; SHARDS] {
    SHARDS_DATA.get_or_init(|| std::array::from_fn(|_| Mutex::new(Shard::default())))
}

fn shard_index(path: &str) -> usize {
    // FNV-1a: cheap and well distributed for path-shaped keys.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h as usize) & (SHARDS - 1)
}

fn fingerprint(meta: &std::fs::Metadata) -> Option<Fingerprint> {
    // Only regular, non-empty, bounded files are worth caching. Pipes,
    // procfs, directories, and empty files either change without a
    // metadata update or cost more to track than to read.
    if !meta.is_file() || meta.len() == 0 || meta.len() > config().max_entry as u64 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(Fingerprint {
            len: meta.len(),
            mtime_sec: meta.mtime(),
            mtime_nsec: meta.mtime_nsec() as u32,
            ctime_sec: meta.ctime(),
            ctime_nsec: meta.ctime_nsec() as u32,
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(Fingerprint {
            len: meta.len(),
            mtime_sec: mtime.as_secs() as i64,
            mtime_nsec: mtime.subsec_nanos(),
        })
    }
}

/// Look up `path` and validate its fingerprint against the live filesystem.
/// Returns `None` on a miss, a stale entry (which is dropped), or a
/// non-cacheable path. Costs one `stat` syscall.
pub(crate) fn probe(path: &str) -> Option<Arc<CachedFile>> {
    if !config().enabled {
        return None;
    }
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
            // Deleted/renamed away: do not let an entry survive a later
            // inode reuse and look valid.
            remove(path);
            return None;
        }
    };
    let Some(fp) = fingerprint(&meta) else {
        remove(path);
        return None;
    };
    let idx = shard_index(path);
    let mut shard = shards()[idx].lock().unwrap_or_else(|e| e.into_inner());
    let entry = shard.map.get(path)?.clone();
    if entry.fingerprint == fp {
        Some(entry)
    } else {
        drop_entry(&mut shard, path);
        None
    }
}

fn drop_entry(shard: &mut Shard, path: &str) -> Option<Arc<CachedFile>> {
    let removed = shard.map.remove(path)?;
    shard.bytes = shard.bytes.saturating_sub(removed.bytes.len());
    Some(removed)
}

/// Insert (or replace) a cache entry. `fp` must come from the metadata
/// observed around the read; callers only pass stable fingerprints.
/// Returns the stored entry so the caller shares its lazy text/flags.
fn insert(path: &str, fp: Fingerprint, bytes: Arc<Vec<u8>>) -> Option<Arc<CachedFile>> {
    let cfg = config();
    if !cfg.enabled
        || bytes.is_empty()
        || bytes.len() > cfg.max_entry
        // Only a whole, stable snapshot is cacheable. Virtual files report
        // a size that does not match the bytes they yield (sysfs/procfs),
        // and a racing truncate/grow lands here too.
        || bytes.len() as u64 != fp.len
    {
        return None;
    }
    let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    let idx = shard_index(path);
    let mut shard = shards()[idx].lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = shard.map.get(path) {
        shard.bytes = shard.bytes.saturating_sub(old.bytes.len());
    }
    let size = bytes.len();
    let entry = Arc::new(CachedFile {
        fingerprint: fp,
        seq,
        bytes,
        text: OnceLock::new(),
        has_nul: OnceLock::new(),
        utf8_valid: OnceLock::new(),
    });
    shard.map.insert(path.into(), Arc::clone(&entry));
    shard.order.push_back(Slot {
        key: path.into(),
        seq,
    });
    shard.bytes += size;
    while shard.bytes > cfg.shard_budget || shard.map.len() > MAX_ENTRIES_PER_SHARD {
        let Some(slot) = shard.order.pop_front() else {
            break;
        };
        let Some(current) = shard.map.get(slot.key.as_ref()) else {
            continue;
        };
        if current.seq != slot.seq {
            continue;
        }
        // The just-inserted entry can be evicted only when it is the last
        // survivor and still over budget; keep it reachable for the caller
        // (its `Arc` lives on in the returned handle).
        drop_entry(&mut shard, slot.key.as_ref());
    }
    // Replacing one path repeatedly (generated sources, editor scratch
    // files) leaves one stale slot per insert. Compaction is rare and
    // proportional to the bounded live map, so `order` stays O(entries).
    if shard.order.len() > shard.map.len() * 2 + 64 {
        let live: std::collections::HashSet<u64> =
            shard.map.values().map(|entry| entry.seq).collect();
        shard.order.retain(|slot| live.contains(&slot.seq));
    }
    Some(entry)
}

/// Drop one path (called by every `std.fs` mutator).
pub(crate) fn invalidate(path: &str) {
    if config().enabled {
        remove(path);
    }
}

fn remove(path: &str) {
    let idx = shard_index(path);
    let mut shard = shards()[idx].lock().unwrap_or_else(|e| e.into_inner());
    drop_entry(&mut shard, path);
}

/// Drop every entry under `prefix` (recursive removals / moves).
pub(crate) fn invalidate_prefix(prefix: &str) {
    if !config().enabled || prefix.is_empty() {
        return;
    }
    let mut prefix = prefix.to_owned();
    while prefix.len() > 1 && (prefix.ends_with('/') || prefix.ends_with('\\')) {
        prefix.pop();
    }
    let rooted = prefix == "/";
    for shard in shards() {
        let mut shard = shard.lock().unwrap_or_else(|e| e.into_inner());
        let victims: Vec<String> = shard
            .map
            .keys()
            .filter(|k| {
                rooted
                    || k.as_ref() == prefix
                    || (k.starts_with(prefix.as_str())
                        && matches!(k.as_bytes().get(prefix.len()), Some(b'/') | Some(b'\\')))
            })
            .map(|k| k.to_string())
            .collect();
        for key in victims {
            drop_entry(&mut shard, &key);
        }
    }
}

/// Read a file through the cache. On a hit this is one `stat` plus an
/// `Arc` bump; on a miss it performs the read and populates the cache when
/// the file metadata is stable across the read.
#[cfg(test)]
pub(crate) fn read(path: &str) -> std::io::Result<Arc<CachedFile>> {
    if let Some(entry) = probe(path) {
        return Ok(entry);
    }
    read_uncached(path)
}

/// Miss path for callers that already probed (`probe_cached` returned
/// `None`): skips a second `stat` before opening the file.
pub(crate) fn read_uncached(path: &str) -> std::io::Result<Arc<CachedFile>> {
    let mut file = std::fs::File::open(path)?;
    let before = file.metadata()?;
    let fp = fingerprint(&before);
    let mut buf = Vec::with_capacity(before.len() as usize);
    file.read_to_end(&mut buf)?;
    // If the file changed while we read it, serve the bytes but do not
    // cache a torn snapshot: the next call will read again.
    let bytes = Arc::new(buf);
    if let (Some(fp), Ok(after)) = (fp, file.metadata()) {
        if fingerprint(&after) == Some(fp) {
            if let Some(entry) = insert(path, fp, Arc::clone(&bytes)) {
                return Ok(entry);
            }
        }
    }
    Ok(uncached(fp, bytes))
}

/// Standalone handle for a read that was not cached (or raced). Shares the
/// exact call's bytes; lazily computes text/flags like a cached entry.
fn uncached(fp: Option<Fingerprint>, bytes: Arc<Vec<u8>>) -> Arc<CachedFile> {
    Arc::new(CachedFile {
        fingerprint: fp.unwrap_or(Fingerprint {
            len: bytes.len() as u64,
            mtime_sec: 0,
            mtime_nsec: 0,
            #[cfg(unix)]
            ctime_sec: 0,
            #[cfg(unix)]
            ctime_nsec: 0,
            #[cfg(unix)]
            dev: 0,
            #[cfg(unix)]
            ino: 0,
        }),
        seq: 0,
        bytes,
        text: OnceLock::new(),
        has_nul: OnceLock::new(),
        utf8_valid: OnceLock::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The cache is process-wide; serialize these tests so one test's
    /// invalidation/eviction cannot make another's invariants flaky.
    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zz_fs_cache_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("f.txt")
    }

    #[test]
    fn hit_returns_shared_bytes() {
        let _guard = guard();
        let p = temp_path("hit");
        std::fs::write(&p, b"hello").unwrap();
        let a = read(p.to_str().unwrap()).unwrap();
        let b = read(p.to_str().unwrap()).unwrap();
        assert_eq!(a.bytes().as_slice(), b"hello");
        // Same backing store: the second read is a cache hit.
        assert!(Arc::ptr_eq(a.bytes(), b.bytes()));
        assert_eq!(a.text().unwrap().as_str(), "hello");
        assert!(!a.has_nul());
        assert!(a.is_utf8());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn external_rewrite_is_observed() {
        let _guard = guard();
        let p = temp_path("rewrite");
        std::fs::write(&p, b"aaaa").unwrap();
        let _ = read(p.to_str().unwrap()).unwrap();
        // Different length guarantees a distinct fingerprint even on
        // coarse-timestamp filesystems.
        std::fs::write(&p, b"bbbbbb").unwrap();
        let after = read(p.to_str().unwrap()).unwrap();
        assert_eq!(after.bytes().as_slice(), b"bbbbbb");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn invalidate_forces_reread() {
        let _guard = guard();
        let p = temp_path("invalidate");
        std::fs::write(&p, b"one").unwrap();
        assert_eq!(
            read(p.to_str().unwrap()).unwrap().bytes().as_slice(),
            b"one"
        );
        invalidate(p.to_str().unwrap());
        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_len(0).unwrap();
            f.write_all(b"two").unwrap();
        }
        assert_eq!(
            read(p.to_str().unwrap()).unwrap().bytes().as_slice(),
            b"two"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn prefix_invalidation_drops_children() {
        let _guard = guard();
        let dir = std::env::temp_dir().join(format!("zz_fs_cache_prefix_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let p = dir.join("sub").join("f.txt");
        std::fs::write(&p, b"abc").unwrap();
        let before = read(p.to_str().unwrap()).unwrap();
        invalidate_prefix(dir.to_str().unwrap());
        std::fs::write(&p, b"abcd").unwrap();
        let after = read(p.to_str().unwrap()).unwrap();
        assert_eq!(after.bytes().as_slice(), b"abcd");
        assert!(!Arc::ptr_eq(before.bytes(), after.bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_utf8_is_negative_cached() {
        let _guard = guard();
        let p = temp_path("non_utf8");
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        let a = read(p.to_str().unwrap()).unwrap();
        assert!(a.text().is_none());
        assert!(a.has_nul());
        assert!(!a.is_utf8());
        let b = read(p.to_str().unwrap()).unwrap();
        assert!(Arc::ptr_eq(a.bytes(), b.bytes()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_files_are_not_cached() {
        let _guard = guard();
        let p = temp_path("empty");
        std::fs::write(&p, b"").unwrap();
        let a = read(p.to_str().unwrap()).unwrap();
        let b = read(p.to_str().unwrap()).unwrap();
        assert!(!Arc::ptr_eq(a.bytes(), b.bytes()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn replacement_churn_keeps_order_bounded() {
        let _guard = guard();
        let p = temp_path("churn");
        let path = p.to_str().unwrap();
        for i in 0..1_000u32 {
            invalidate(path);
            std::fs::write(&p, i.to_string()).unwrap();
            let entry = read(path).unwrap();
            assert_eq!(entry.bytes().as_slice(), i.to_string().as_bytes());
        }
        let idx = shard_index(path);
        let shard = shards()[idx].lock().unwrap_or_else(|e| e.into_inner());
        assert!(shard.order.len() <= shard.map.len() * 2 + 64);
        drop(shard);
        invalidate(path);
        let _ = std::fs::remove_file(&p);
    }
}
