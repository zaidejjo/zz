//! Directory entry cache for `std.fs` read_dir / walk_dir.
//!
//! # Why
//!
//! Directory listings are expensive: one `openat`/`getdents` syscall per
//! directory plus `stat`/`file_type` per entry. Compiler-style workloads
//! list the same directories repeatedly (import resolution, walk_dir scans,
//! `ls`-style builds). Without a cache every listing pays the full cost.
//!
//! # Design
//!
//! Keyed by directory path. Each entry holds `(name, is_dir)` pairs so both
//! `read_dir` (names only) and `walk_dir` (names + dir detection) can reuse
//! the same cache. Validated against `len`/`mtime`/`ino`/`dev` (Unix) or
//! `len`/`mtime` (elsewhere) on **every** hit, so external writes are
//! observed. Sharded (32 `Mutex`s) for low-contention concurrency, FIFO
//! evicted per shard, 96 MiB total budget.
//!
//! # Freshness
//!
//! Fingerprint match = cache hit. Any change invalidates. `std.fs` mutators
//! (write/remove/mkdir/...) invalidate eagerly.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const SHARDS: usize = 32;
const DEFAULT_BUDGET_MB: usize = 96;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Fingerprint {
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
    max_entries_per_shard: usize,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[allow(dead_code)]
pub(crate) fn enabled() -> bool {
    config().enabled
}

fn config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let enabled = std::env::var_os("ZZ_DIR_CACHE")
            .map(|v| v != "0")
            .unwrap_or(true);
        let mb = std::env::var("ZZ_DIR_CACHE_MB")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(DEFAULT_BUDGET_MB as i64)
            .min(1_048_576);
        let budget = usize::try_from(mb).unwrap_or(0).saturating_mul(1024 * 1024);
        Config {
            enabled: enabled && budget > 0,
            shard_budget: (budget / SHARDS).max(64 * 1024),
            max_entries_per_shard: (budget / 4096).max(128),
        }
    })
}

/// Cached directory listing: (name, is_dir) pairs plus fingerprint.
pub(crate) struct CachedDir {
    fingerprint: Fingerprint,
    pub(crate) entries: Vec<(String, bool)>,
}

struct Slot {
    key: Box<str>,
    #[allow(dead_code)]
    seq: u64,
}

#[derive(Default)]
struct Shard {
    map: HashMap<Box<str>, Arc<CachedDir>>,
    order: VecDeque<Slot>,
    bytes: usize,
}

static SHARDS_DATA: OnceLock<[Mutex<Shard>; SHARDS]> = OnceLock::new();
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

fn shards() -> &'static [Mutex<Shard>; SHARDS] {
    SHARDS_DATA.get_or_init(|| std::array::from_fn(|_| Mutex::new(Shard::default())))
}

fn shard_index(path: &str) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h as usize) & (SHARDS - 1)
}

pub(crate) fn fingerprint(meta: &std::fs::Metadata) -> Option<Fingerprint> {
    if !meta.is_dir() {
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

/// Look up `path` and validate its fingerprint. `None` on miss or stale.
pub(crate) fn probe(path: &str) -> Option<Arc<CachedDir>> {
    if !config().enabled {
        return None;
    }
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
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

fn drop_entry(shard: &mut Shard, path: &str) -> Option<Arc<CachedDir>> {
    let removed = shard.map.remove(path)?;
    shard.bytes = shard.bytes.saturating_sub(removed.entries.len() * 128);
    Some(removed)
}

/// Insert (or replace) a directory listing.
pub(crate) fn insert(
    path: &str,
    fp: Fingerprint,
    entries: Vec<(String, bool)>,
) -> Option<Arc<CachedDir>> {
    let cfg = config();
    if !cfg.enabled {
        return None;
    }
    let size = entries.len() * 128;
    if size > cfg.shard_budget {
        return None;
    }
    let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    let idx = shard_index(path);
    let mut shard = shards()[idx].lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = shard.map.get(path) {
        shard.bytes = shard.bytes.saturating_sub(old.entries.len() * 128);
    }
    let entry = Arc::new(CachedDir {
        fingerprint: fp,
        entries,
    });
    shard.map.insert(path.into(), Arc::clone(&entry));
    shard.order.push_back(Slot {
        key: path.into(),
        seq,
    });
    shard.bytes += size;
    // Evict FIFO until under budget / entry cap.
    while shard.bytes > cfg.shard_budget || shard.map.len() > cfg.max_entries_per_shard {
        let Some(slot) = shard.order.pop_front() else {
            break;
        };
        let Some(current) = shard.map.get(slot.key.as_ref()) else {
            continue;
        };
        if std::ptr::eq(current.as_ref(), entry.as_ref()) && shard.map.len() == 1 {
            continue;
        }
        drop_entry(&mut shard, slot.key.as_ref());
    }
    if shard.order.len() > shard.map.len() * 2 + 64 {
        let live_keys: Vec<Box<str>> = shard.map.keys().cloned().collect();
        shard
            .order
            .retain(|slot| live_keys.iter().any(|k| k.as_ref() == slot.key.as_ref()));
    }
    Some(entry)
}

/// Drop one path (called by every std.fs mutator).
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

/// Drop every entry under `prefix`.
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
