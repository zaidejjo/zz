//! Per-module build artifact cache.
//!
//! Manages cached build outputs (compiled `.o` files, final binaries)
//! under `~/.zz/cache/objects/`. Each entry is keyed by the deterministic
//! slug from `CacheKey::to_slug()`.
//!
//! This replaces the old whole-file cache in `build.rs` with a proper
//! per-module system that correctly invalidates when path deps change.

use std::path::{Path, PathBuf};

use crate::cache_key::CacheKey;
use crate::paths;

/// A cached build artifact entry.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// The cache key slug (directory name under `objects/`).
    pub slug: String,
    /// Full path to the cached artifact (binary or object file).
    pub path: PathBuf,
    /// Whether the artifact exists and is usable.
    pub valid: bool,
}

/// The build artifact cache.
pub struct BuildCache {
    /// Base directory: `~/.zz/cache/objects/`
    base: PathBuf,
}

impl Default for BuildCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildCache {
    /// Create a handle to the build artifact cache.
    pub fn new() -> Self {
        Self {
            base: paths::cache_objects_dir(),
        }
    }

    /// Create a cache handle with a custom base (for testing).
    pub fn with_base(base: PathBuf) -> Self {
        Self { base }
    }

    /// Look up a cached artifact by cache key.
    ///
    /// Returns `Some(entry)` if a valid cached binary exists at the
    /// expected path. The artifact must be a non-empty file with
    /// execute permission (on unix).
    pub fn lookup(&self, key: &CacheKey) -> Option<CacheEntry> {
        let slug = key.to_slug();
        let path = self.base.join(&slug);

        if is_usable_binary(&path) {
            Some(CacheEntry {
                slug,
                path,
                valid: true,
            })
        } else {
            None
        }
    }

    /// Store a newly-built artifact into the cache.
    ///
    /// Returns the path where the artifact was stored.
    /// Creates parent directories as needed.
    pub fn store(&self, key: &CacheKey, artifact: &Path) -> Result<CacheEntry, String> {
        let slug = key.to_slug();
        let dest = self.base.join(&slug);

        std::fs::create_dir_all(&self.base).map_err(|e| format!("cannot create cache dir: {e}"))?;

        // Atomic write: write to temp, then rename
        let tmp = self.base.join(format!("{slug}.{}.tmp", std::process::id()));
        std::fs::copy(artifact, &tmp).map_err(|e| format!("cannot copy artifact to cache: {e}"))?;

        // Ensure exec bit on unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&tmp) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o111);
                let _ = std::fs::set_permissions(&tmp, perms);
            }
        }

        std::fs::rename(&tmp, &dest).map_err(|e| format!("cannot publish cache entry: {e}"))?;

        Ok(CacheEntry {
            slug,
            path: dest,
            valid: true,
        })
    }

    /// Invalidate (remove) a cached artifact.
    pub fn invalidate(&self, key: &CacheKey) -> Result<(), String> {
        let slug = key.to_slug();
        let path = self.base.join(&slug);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("cannot remove cache entry: {e}"))?;
        }
        Ok(())
    }

    /// List all cached entries (for gc/debugging).
    pub fn list_entries(&self) -> Vec<CacheEntry> {
        let mut entries = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(&self.base) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let slug = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let valid = is_usable_binary(&path);
                    entries.push(CacheEntry { slug, path, valid });
                }
            }
        }
        entries
    }

    /// Count of valid cache entries.
    pub fn count(&self) -> usize {
        self.list_entries().iter().filter(|e| e.valid).count()
    }
}

/// Check if a path points to a usable cached binary.
///
/// Must be a non-empty file with execute permission (on unix).
fn is_usable_binary(p: &Path) -> bool {
    let Ok(meta) = p.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_key::CacheKey;
    use std::fs;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "zz_pm_build_cache_test_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_key(src: &str) -> CacheKey {
        CacheKey {
            source_hash: crate::hash::hash_bytes(src.as_bytes()),
            dep_hashes: std::collections::HashMap::new(),
            build_fingerprint: 42,
            target: "host".to_string(),
            runtime_mtime: None,
            artifact_hash: String::new(),
            artifact_flags: String::new(),
        }
    }

    #[test]
    fn store_and_lookup() {
        let d = tmp();
        let cache = BuildCache::with_base(d.join("objects"));
        let key = make_key("func main() { }");

        // Create a fake artifact
        let artifact = d.join("artifact.bin");
        fs::write(&artifact, "binary content").unwrap();

        // Store
        let entry = cache.store(&key, &artifact).unwrap();
        assert!(entry.valid);
        assert!(entry.path.exists());

        // Lookup
        let found = cache.lookup(&key);
        assert!(found.is_some());
        assert_eq!(found.unwrap().path, entry.path);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn lookup_miss_for_different_key() {
        let d = tmp();
        let cache = BuildCache::with_base(d.join("objects"));
        let key1 = make_key("source A");
        let key2 = make_key("source B");

        let artifact = d.join("artifact.bin");
        fs::write(&artifact, "binary content").unwrap();
        cache.store(&key1, &artifact).unwrap();

        // Different key should miss
        assert!(cache.lookup(&key2).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn invalidate_removes_entry() {
        let d = tmp();
        let cache = BuildCache::with_base(d.join("objects"));
        let key = make_key("func main() { }");

        let artifact = d.join("artifact.bin");
        fs::write(&artifact, "binary content").unwrap();
        cache.store(&key, &artifact).unwrap();

        assert!(cache.lookup(&key).is_some());
        cache.invalidate(&key).unwrap();
        assert!(cache.lookup(&key).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn list_entries_shows_all() {
        let d = tmp();
        let cache = BuildCache::with_base(d.join("objects"));

        let artifact = d.join("artifact.bin");
        fs::write(&artifact, "binary content").unwrap();

        let key1 = make_key("source A");
        let key2 = make_key("source B");
        cache.store(&key1, &artifact).unwrap();
        cache.store(&key2, &artifact).unwrap();

        let entries = cache.list_entries();
        assert_eq!(entries.len(), 2);
        let _ = fs::remove_dir_all(&d);
    }
}
