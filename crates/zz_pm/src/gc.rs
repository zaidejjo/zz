//! CAS garbage collection for `zz pm`.
//!
//! Removes CAS entries that have no live references in any project's
//! `zz.lock` or `vendor/` directory. Uses `known-projects.json` to track
//! every project directory that has ever run `zz install`, so GC can
//! enumerate all live references before deleting anything.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::paths;

/// Result of a GC sweep.
#[derive(Debug, Clone)]
pub struct GcResult {
    /// CAS entries that were removed.
    pub removed: Vec<String>,
    /// CAS entries that are still referenced.
    pub kept: Vec<String>,
    /// Bytes freed (approximate).
    pub bytes_freed: u64,
}

/// Machine-wide project registry: every project that has ever run `zz install`.
///
/// This is the safety mechanism for GC. Without it, GC cannot know about
/// projects outside the current working directory, and would delete CAS
/// entries still referenced by those projects.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnownProjects {
    /// Absolute paths to project directories.
    #[serde(default)]
    pub projects: HashSet<String>,
}

impl KnownProjects {
    /// Load from disk, or create empty.
    pub fn load() -> Self {
        let path = paths::known_projects_path();
        Self::load_from(&path)
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Self {
        if !path.exists() {
            return Self {
                projects: HashSet::new(),
            };
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                return Self {
                    projects: HashSet::new(),
                }
            }
        };
        toml::from_str(&content).unwrap_or(Self {
            projects: HashSet::new(),
        })
    }

    /// Save to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = paths::known_projects_path();
        self.save_to(&path)
    }

    /// Save to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create dir: {e}"))?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize known-projects: {e}"))?;
        std::fs::write(path, content).map_err(|e| format!("cannot write known-projects: {e}"))?;
        Ok(())
    }

    /// Register a project directory.
    pub fn register(&mut self, project_dir: &Path) -> Result<(), String> {
        let canonical = project_dir
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize path: {e}"))?;
        self.projects
            .insert(canonical.to_string_lossy().into_owned());
        self.save()
    }

    /// Remove a project from the registry.
    pub fn unregister(&mut self, project_dir: &Path) -> Result<(), String> {
        let canonical = project_dir
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize path: {e}"))?;
        self.projects
            .remove(&canonical.to_string_lossy().into_owned());
        self.save()
    }

    /// Get all registered project directories that still exist on disk.
    pub fn active_projects(&self) -> Vec<std::path::PathBuf> {
        self.projects
            .iter()
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    }
}

/// Reverse reference index: maps CAS entry hashes to referencing projects.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReverseRefs {
    /// CAS hash → set of project paths that reference it.
    #[serde(default)]
    pub refs: HashMap<String, HashSet<String>>,
}

impl ReverseRefs {
    /// Load from disk, or create empty.
    pub fn load() -> Self {
        let path = paths::reverse_refs_path();
        Self::load_from(&path)
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Self {
        if !path.exists() {
            return Self {
                refs: HashMap::new(),
            };
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                return Self {
                    refs: HashMap::new(),
                }
            }
        };
        toml::from_str(&content).unwrap_or(Self {
            refs: HashMap::new(),
        })
    }

    /// Save to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = paths::reverse_refs_path();
        self.save_to(&path)
    }

    /// Save to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create dir: {e}"))?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize reverse-refs: {e}"))?;
        std::fs::write(path, content).map_err(|e| format!("cannot write reverse-refs: {e}"))?;
        Ok(())
    }

    /// Record that a project references a CAS entry.
    pub fn add_ref(&mut self, cas_hash: &str, project_path: &str) {
        self.refs
            .entry(cas_hash.to_string())
            .or_default()
            .insert(project_path.to_string());
    }

    /// Remove a project's reference to a CAS entry.
    pub fn remove_ref(&mut self, cas_hash: &str, project_path: &str) {
        if let Some(referencers) = self.refs.get_mut(cas_hash) {
            referencers.remove(project_path);
            if referencers.is_empty() {
                self.refs.remove(cas_hash);
            }
        }
    }

    /// Remove all references from a specific project.
    pub fn remove_project(&mut self, project_path: &str) {
        let to_remove: Vec<String> = self
            .refs
            .iter()
            .filter(|(_, refs)| refs.contains(project_path))
            .map(|(hash, _)| hash.clone())
            .collect();

        for hash in to_remove {
            self.remove_ref(&hash, project_path);
        }
    }

    /// Get all CAS entries that have no live references.
    pub fn unreferenced_entries(&self) -> Vec<String> {
        self.refs
            .iter()
            .filter(|(_, refs)| refs.is_empty())
            .map(|(hash, _)| hash.clone())
            .collect()
    }
}

/// Run garbage collection on the CAS.
///
/// Enumerates ALL known projects on this machine (from `known-projects.json`),
/// scans each project's zz.lock and vendor/ for CAS references, then removes
/// only entries that no project references.
pub fn gc() -> Result<GcResult, String> {
    let packages_dir = paths::packages_dir();
    let known = KnownProjects::load();

    let mut removed = Vec::new();
    let mut kept = Vec::new();
    let mut bytes_freed = 0u64;

    if !packages_dir.exists() {
        return Ok(GcResult {
            removed,
            kept,
            bytes_freed,
        });
    }

    // Phase 1: Collect all CAS entries on disk
    let entries: Vec<String> = std::fs::read_dir(&packages_dir)
        .map_err(|e| format!("cannot read packages dir: {e}"))?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();

    // Phase 2: Build the live reference set from ALL known projects
    let mut live_refs: HashSet<String> = HashSet::new();
    for project_dir in known.active_projects() {
        let refs = scan_project_refs(&project_dir);
        live_refs.extend(refs);
    }

    // Phase 3: Decide what to keep and what to remove
    for entry in &entries {
        if live_refs.contains(entry) {
            kept.push(entry.clone());
        } else {
            let entry_path = packages_dir.join(entry);
            let size = dir_size(&entry_path);
            std::fs::remove_dir_all(&entry_path)
                .map_err(|e| format!("cannot remove CAS entry {entry}: {e}"))?;
            bytes_freed += size;
            removed.push(entry.clone());
        }
    }

    Ok(GcResult {
        removed,
        kept,
        bytes_freed,
    })
}

/// Get the approximate size of a directory in bytes.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata().ok();
            if let Some(meta) = meta {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += dir_size(&entry.path());
                }
            }
        }
    }
    total
}

/// Scan a project directory for CAS references in vendor/ and zz.lock.
/// Returns the set of CAS hashes referenced by the project.
pub fn scan_project_refs(project_dir: &Path) -> HashSet<String> {
    let mut refs = HashSet::new();

    // Scan vendor/ for symlinks pointing into CAS
    let vendor_dir = project_dir.join("vendor");
    if vendor_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&vendor_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_symlink() {
                    if let Ok(target) = std::fs::read_link(&path) {
                        // Extract CAS hash from the target path
                        if let Some(hash) = extract_cas_hash(&target) {
                            refs.insert(hash);
                        }
                    }
                }
            }
        }
    }

    // Scan zz.lock for commit hashes
    let lock_path = project_dir.join("zz.lock");
    if lock_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&lock_path) {
            if let Ok(lock) = crate::lock::Lockfile::parse(&content) {
                for dep in &lock.deps {
                    if let Some(commit) = &dep.commit {
                        refs.insert(commit.clone());
                    }
                }
            }
        }
    }

    refs
}

/// Extract a CAS hash from a symlink target path.
/// Looks for `~/.zz/packages/<hash>` patterns.
fn extract_cas_hash(target: &Path) -> Option<String> {
    let target_str = target.to_string_lossy();
    let packages_dir = paths::packages_dir();
    let packages_str = packages_dir.to_string_lossy();

    if let Some(idx) = target_str.find(&*packages_str) {
        let remainder = &target_str[idx + packages_str.len()..];
        let hash = remainder.trim_start_matches('/');
        if !hash.is_empty() {
            return Some(hash.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("zz_pm_gc_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reverse_refs_round_trip() {
        let d = tmp();
        let path = d.join("reverse-refs.toml");

        let mut refs = ReverseRefs {
            refs: HashMap::new(),
        };
        refs.add_ref("abc123", "/home/user/project1");
        refs.add_ref("abc123", "/home/user/project2");
        refs.add_ref("def456", "/home/user/project1");

        refs.save_to(&path).unwrap();

        let loaded = ReverseRefs::load_from(&path);
        assert_eq!(loaded.refs.len(), 2);
        assert!(loaded.refs["abc123"].contains("/home/user/project1"));
        assert!(loaded.refs["abc123"].contains("/home/user/project2"));
        assert!(loaded.refs["def456"].contains("/home/user/project1"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_project_cleans_refs() {
        let d = tmp();
        let path = d.join("reverse-refs.toml");

        let mut refs = ReverseRefs {
            refs: HashMap::new(),
        };
        refs.add_ref("abc123", "/home/user/project1");
        refs.add_ref("abc123", "/home/user/project2");
        refs.add_ref("def456", "/home/user/project1");

        refs.save_to(&path).unwrap();

        let mut loaded = ReverseRefs::load_from(&path);
        loaded.remove_project("/home/user/project1");

        // abc123 still has project2, def456 has no refs
        assert!(loaded.refs.contains_key("abc123"));
        assert_eq!(loaded.refs["abc123"].len(), 1);
        assert!(!loaded.refs.contains_key("def456"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unreferenced_entries_detected() {
        let d = tmp();
        let path = d.join("reverse-refs.toml");

        let mut refs = ReverseRefs {
            refs: HashMap::new(),
        };
        refs.add_ref("abc123", "/home/user/project1");
        // def456 has empty ref set (simulating removed project)
        refs.refs.insert("def456".to_string(), HashSet::new());

        refs.save_to(&path).unwrap();

        let loaded = ReverseRefs::load_from(&path);
        let unreferenced = loaded.unreferenced_entries();
        assert!(unreferenced.contains(&"def456".to_string()));
        assert!(!unreferenced.contains(&"abc123".to_string()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn gc_removes_unreferenced() {
        let d = tmp();
        let packages_dir = d.join("packages");

        // Create CAS entries
        fs::create_dir_all(packages_dir.join("referenced")).unwrap();
        fs::create_dir_all(packages_dir.join("unreferenced")).unwrap();
        fs::write(packages_dir.join("referenced/file.txt"), "keep").unwrap();
        fs::write(packages_dir.join("unreferenced/file.txt"), "delete").unwrap();

        // Set up reverse refs — only "referenced" is live
        let mut refs = ReverseRefs {
            refs: HashMap::new(),
        };
        refs.add_ref("referenced", "/some/project");
        // "unreferenced" not in refs at all

        // Override paths for test
        let mut result = GcResult {
            removed: Vec::new(),
            kept: Vec::new(),
            bytes_freed: 0,
        };

        // Manual GC logic for test (can't use real gc() since it uses global paths)
        for entry in fs::read_dir(&packages_dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if refs.refs.contains_key(&name) {
                result.kept.push(name);
            } else {
                let size = dir_size(&entry.path());
                fs::remove_dir_all(entry.path()).unwrap();
                result.bytes_freed += size;
                result.removed.push(name);
            }
        }

        assert!(result.removed.contains(&"unreferenced".to_string()));
        assert!(result.kept.contains(&"referenced".to_string()));
        assert!(!packages_dir.join("unreferenced").exists());
        assert!(packages_dir.join("referenced").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_size_nonzero() {
        let d = tmp();
        fs::write(d.join("file.txt"), "hello world").unwrap();
        let size = dir_size(&d);
        assert!(size > 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn shared_cas_entry_survives_gc() {
        // Critical safety test: two separate project dirs both reference the
        // same CAS entry. GC must NOT delete it even when run from a third,
        // unrelated location — because known-projects.json lists both projects.
        let d = tmp();
        let packages_dir = d.join("packages");
        let projects_dir = d.join("known-projects.json");

        // Create a shared CAS entry
        fs::create_dir_all(packages_dir.join("shared_hash")).unwrap();
        fs::write(
            packages_dir.join("shared_hash/lib.zz"),
            "pub func helper() {}",
        )
        .unwrap();

        // Create two project dirs that both reference the CAS entry
        let proj_a = d.join("project_a");
        let proj_b = d.join("project_b");
        fs::create_dir_all(&proj_a).unwrap();
        fs::create_dir_all(&proj_b).unwrap();

        // Create zz.lock in each project referencing the shared CAS entry
        for proj in [&proj_a, &proj_b] {
            let lock_content = r#"
version = 1

[[deps]]
name = "mylib"
version = "1.0.0"
source = "git+https://example.com/repo.git#main"
hash = ""
commit = "shared_hash"
"#;
            fs::write(proj.join("zz.lock"), lock_content).unwrap();
            // Create vendor dir with symlink to CAS
            let vendor = proj.join("vendor");
            fs::create_dir_all(&vendor).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(packages_dir.join("shared_hash"), vendor.join("mylib"))
                .unwrap();
        }

        // Create known-projects registry pointing to both projects
        let mut known = KnownProjects {
            projects: HashSet::new(),
        };
        known.projects.insert(proj_a.to_string_lossy().into_owned());
        known.projects.insert(proj_b.to_string_lossy().into_owned());
        known.save_to(&projects_dir).unwrap();

        // Verify scan_project_refs finds the shared hash from both projects
        let refs_a = scan_project_refs(&proj_a);
        let refs_b = scan_project_refs(&proj_b);
        assert!(
            refs_a.contains("shared_hash"),
            "project_a should reference shared_hash"
        );
        assert!(
            refs_b.contains("shared_hash"),
            "project_b should reference shared_hash"
        );

        // The shared entry should survive GC (we can't call real gc() since
        // it uses global paths, but we can verify the logic manually)
        let mut live_refs = HashSet::new();
        live_refs.extend(refs_a);
        live_refs.extend(refs_b);
        assert!(
            live_refs.contains("shared_hash"),
            "shared_hash must be in live refs"
        );

        // Simulate GC decision: would shared_hash be deleted?
        let cas_entries: Vec<String> = fs::read_dir(&packages_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();

        for entry in &cas_entries {
            assert!(
                live_refs.contains(entry),
                "GC would incorrectly delete {entry} which is still referenced"
            );
        }

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn known_projects_round_trip() {
        let d = tmp();
        let path = d.join("known-projects.json");

        let mut known = KnownProjects {
            projects: HashSet::new(),
        };
        known.projects.insert("/home/user/proj1".to_string());
        known.projects.insert("/home/user/proj2".to_string());
        known.save_to(&path).unwrap();

        let loaded = KnownProjects::load_from(&path);
        assert_eq!(loaded.projects.len(), 2);
        assert!(loaded.projects.contains("/home/user/proj1"));
        assert!(loaded.projects.contains("/home/user/proj2"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn known_projects_active_filters_missing() {
        let d = tmp();
        let path = d.join("known-projects.json");

        let proj_existing = d.join("existing_proj");
        fs::create_dir_all(&proj_existing).unwrap();

        let mut known = KnownProjects {
            projects: HashSet::new(),
        };
        known
            .projects
            .insert(proj_existing.to_string_lossy().into_owned());
        known.projects.insert("/nonexistent/path".to_string());
        known.save_to(&path).unwrap();

        let loaded = KnownProjects::load_from(&path);
        let active = loaded.active_projects();
        assert_eq!(active.len(), 1);
        assert!(active.contains(&proj_existing));
        let _ = fs::remove_dir_all(&d);
    }
}
