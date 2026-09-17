//! Project linking for `zz pm`.
//!
//! Manages symlinks from project `vendor/` into the global CAS.
//! Implements Amendment 2: verify existing symlinks resolve to expected targets.

use std::path::{Path, PathBuf};

use crate::lock::Lockfile;
use crate::manifest::Manifest;

/// Errors during linking.
#[derive(Debug)]
pub enum LinkError {
    /// CAS entry does not exist.
    CasEntryMissing { hash: String },
    /// Symlink creation failed.
    SymlinkFailed {
        target: PathBuf,
        link: PathBuf,
        detail: String,
    },
    /// I/O error.
    Io(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CasEntryMissing { hash } => {
                write!(
                    f,
                    "CAS entry missing for `{hash}`\n\
                     hint: run `zz install` to fetch dependencies"
                )
            }
            Self::SymlinkFailed {
                target,
                link,
                detail,
            } => {
                write!(
                    f,
                    "failed to create symlink: {} → {}\n{detail}",
                    link.display(),
                    target.display()
                )
            }
            Self::Io(msg) => write!(f, "link I/O error: {msg}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// Link strategy for creating vendor/ entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStrategy {
    /// Hardlink files (fast, same filesystem).
    Hardlink,
    /// Symlink to CAS directory (pnpm model).
    Symlink,
    /// Copy files (fallback, cross-filesystem).
    Copy,
}

/// Link all dependencies into the project's vendor/ directory.
///
/// For each dependency in the lockfile:
/// 1. Check if vendor/<name> already exists and points to the correct CAS entry
/// 2. If unchanged, skip (Amendment 2: verify symlink target, not just existence)
/// 3. If missing or wrong, create symlink from vendor/<name> → CAS/<hash>
pub fn link_project(
    project_dir: &Path,
    manifest: &Manifest,
    lockfile: &Lockfile,
    strategy: LinkStrategy,
) -> Result<Vec<PathBuf>, LinkError> {
    let vendor_dir = project_dir.join("vendor");
    std::fs::create_dir_all(&vendor_dir)
        .map_err(|e| LinkError::Io(format!("cannot create vendor/: {e}")))?;

    let mut linked = Vec::new();

    for (name, spec) in &manifest.dependencies {
        let locked = match lockfile.find(name) {
            Some(l) => l,
            None => continue, // Path deps don't appear in lockfile
        };

        // Path deps are handled differently — symlink to local path
        if let crate::manifest::DepSpec::Path(path_dep) = spec {
            let target = project_dir.join(&path_dep.path);
            let link_path = vendor_dir.join(name);

            if is_valid_symlink(&link_path, &target) {
                continue; // Amendment 2: symlink exists and points to correct target
            }

            create_link(strategy, &target, &link_path)?;
            linked.push(link_path);
            continue;
        }

        // Git/versioned deps — link from CAS
        let cas_path = if let Some(commit) = &locked.commit {
            crate::paths::cas_entry(commit)
        } else {
            crate::paths::cas_entry(&locked.hash)
        };

        if !cas_path.exists() {
            return Err(LinkError::CasEntryMissing {
                hash: locked.hash.clone(),
            });
        }

        let link_path = vendor_dir.join(name);

        if is_valid_symlink(&link_path, &cas_path) {
            continue; // Amendment 2: symlink exists and points to correct CAS entry
        }

        create_link(strategy, &cas_path, &link_path)?;
        linked.push(link_path);
    }

    Ok(linked)
}

/// Re-link a project (Amendment 2: verify existing symlinks).
///
/// Checks each existing symlink in vendor/ and recreates if the target
/// has changed (e.g., CAS was GC'd and re-populated).
pub fn re_link_project(
    project_dir: &Path,
    manifest: &Manifest,
    lockfile: &Lockfile,
    strategy: LinkStrategy,
) -> Result<Vec<PathBuf>, LinkError> {
    let vendor_dir = project_dir.join("vendor");

    // If vendor/ doesn't exist, do a fresh link
    if !vendor_dir.exists() {
        return link_project(project_dir, manifest, lockfile, strategy);
    }

    let mut relinked = Vec::new();

    for (name, spec) in &manifest.dependencies {
        let locked = match lockfile.find(name) {
            Some(l) => l,
            None => continue,
        };

        let expected_target = if let crate::manifest::DepSpec::Path(path_dep) = spec {
            project_dir.join(&path_dep.path)
        } else if let Some(commit) = &locked.commit {
            crate::paths::cas_entry(commit)
        } else {
            crate::paths::cas_entry(&locked.hash)
        };

        let link_path = vendor_dir.join(name);

        // Amendment 2: verify symlink still resolves to expected target
        if is_valid_symlink(&link_path, &expected_target) {
            continue;
        }

        // Symlink is missing or points to wrong target — recreate
        create_link(strategy, &expected_target, &link_path)?;
        relinked.push(link_path);
    }

    Ok(relinked)
}

/// Check if a symlink exists and resolves to the expected target.
///
/// Amendment 2: This is NOT just checking existence — we verify the symlink
/// target matches the expected CAS path. If the CAS was GC'd and re-populated
/// with different content at the same path, the symlink would still "exist"
/// but point to wrong content. We check both existence AND target match.
fn is_valid_symlink(link_path: &Path, expected_target: &Path) -> bool {
    // Must exist
    if !link_path.exists() && !link_path.symlink_metadata().is_ok() {
        return false;
    }

    // Must be a symlink (not a regular file or directory)
    let meta = match link_path.symlink_metadata() {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.file_type().is_symlink() {
        return false;
    }

    // Read symlink target
    let actual_target = match std::fs::read_link(link_path) {
        Ok(t) => t,
        Err(_) => return false,
    };

    // Compare — normalize both paths for comparison
    let expected = normalize_path(expected_target);
    let actual = normalize_path(&actual_target);

    // Also verify the target actually exists and is accessible
    if !link_path.exists() {
        return false;
    }

    actual == expected
}

/// Create a link (symlink, hardlink, or copy) from source to target.
fn create_link(strategy: LinkStrategy, source: &Path, link_path: &Path) -> Result<(), LinkError> {
    // Remove existing link/file if present
    if link_path.exists() || link_path.symlink_metadata().is_ok() {
        std::fs::remove_file(link_path)
            .map_err(|e| LinkError::Io(format!("cannot remove old link: {e}")))?;
    }

    match strategy {
        LinkStrategy::Symlink => {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(source, link_path).map_err(|e| {
                    LinkError::SymlinkFailed {
                        target: source.to_path_buf(),
                        link: link_path.to_path_buf(),
                        detail: e.to_string(),
                    }
                })?;
            }
            #[cfg(windows)]
            {
                // On Windows, use dir symlink for directories, file symlink for files
                if source.is_dir() {
                    std::os::windows::fs::symlink_dir(source, link_path).map_err(|e| {
                        LinkError::SymlinkFailed {
                            target: source.to_path_buf(),
                            link: link_path.to_path_buf(),
                            detail: e.to_string(),
                        }
                    })?;
                } else {
                    std::os::windows::fs::symlink_file(source, link_path).map_err(|e| {
                        LinkError::SymlinkFailed {
                            target: source.to_path_buf(),
                            link: link_path.to_path_buf(),
                            detail: e.to_string(),
                        }
                    })?;
                }
            }
        }
        LinkStrategy::Hardlink => {
            // Hardlinks only work for files on the same filesystem
            // For directories, fall back to symlink
            if source.is_dir() {
                return create_link(LinkStrategy::Symlink, source, link_path);
            }
            std::fs::hard_link(source, link_path)
                .map_err(|e| LinkError::Io(format!("hardlink failed: {e}; try --link=symlink")))?;
        }
        LinkStrategy::Copy => {
            if source.is_dir() {
                copy_dir_recursive(source, link_path)?;
            } else {
                std::fs::copy(source, link_path)
                    .map_err(|e| LinkError::Io(format!("copy failed: {e}")))?;
            }
        }
    }

    Ok(())
}

/// Normalize a path for comparison (resolve `.` and `..` components).
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Recursively copy a directory.
fn copy_dir_recursive(source: &Path, dest: &Path) -> Result<(), LinkError> {
    std::fs::create_dir_all(dest).map_err(|e| LinkError::Io(format!("cannot create dir: {e}")))?;

    for entry in
        std::fs::read_dir(source).map_err(|e| LinkError::Io(format!("cannot read dir: {e}")))?
    {
        let entry = entry.map_err(|e| LinkError::Io(format!("cannot read entry: {e}")))?;
        let file_type = entry
            .file_type()
            .map_err(|e| LinkError::Io(e.to_string()))?;
        let src = entry.path();
        let dst = dest.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| LinkError::Io(e.to_string()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockedDep;
    use crate::manifest::{DepSpec, Manifest};
    use std::fs;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("zz_pm_link_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn valid_symlink_detected() {
        let d = tmp();
        let target = d.join("target");
        let link = d.join("link");
        fs::create_dir_all(&target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(is_valid_symlink(&link, &target));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_symlink_not_valid() {
        let d = tmp();
        let link = d.join("nonexistent");
        let target = d.join("target");
        assert!(!is_valid_symlink(&link, &target));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn wrong_target_not_valid() {
        let d = tmp();
        let target1 = d.join("target1");
        let target2 = d.join("target2");
        let link = d.join("link");
        fs::create_dir_all(&target1).unwrap();
        fs::create_dir_all(&target2).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target1, &link).unwrap();

        // Points to target1, not target2
        assert!(!is_valid_symlink(&link, &target2));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn regular_file_not_valid_symlink() {
        let d = tmp();
        let file = d.join("file");
        let target = d.join("target");
        fs::write(&file, "content").unwrap();

        assert!(!is_valid_symlink(&file, &target));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn link_project_creates_symlinks() {
        let d = tmp();
        // Set ZZ_HOME so CAS paths point to our temp dir
        std::env::set_var("ZZ_HOME", &d);

        let project = d.join("project");
        let cas_dir = d.join("packages").join("abc123");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&cas_dir).unwrap();
        fs::write(cas_dir.join("lib.zz"), "pub func hello() {}").unwrap();

        let mut manifest = Manifest::default();
        manifest
            .dependencies
            .insert("mylib".to_string(), DepSpec::Version("^1.0".to_string()));

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "^1.0".to_string(),
            source: "git+https://example.com/repo.git#main".to_string(),
            hash: "abc123".to_string(),
            commit: Some("abc123".to_string()),
        });

        let linked = link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert_eq!(linked.len(), 1);
        assert!(project.join("vendor/mylib").exists());
        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn re_link_skips_unchanged_symlinks() {
        let d = tmp();
        // Set ZZ_HOME so CAS paths point to our temp dir
        std::env::set_var("ZZ_HOME", &d);

        let project = d.join("project");
        let cas_dir = d.join("packages").join("abc123");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&cas_dir).unwrap();

        let mut manifest = Manifest::default();
        manifest
            .dependencies
            .insert("mylib".to_string(), DepSpec::Version("^1.0".to_string()));

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "^1.0".to_string(),
            source: "git+https://example.com/repo.git#main".to_string(),
            hash: "abc123".to_string(),
            commit: Some("abc123".to_string()),
        });

        // First link
        let linked = link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert_eq!(linked.len(), 1);

        // Re-link — should skip unchanged
        let relinked = re_link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert_eq!(relinked.len(), 0); // No re-links needed
        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn normalize_path_removes_dots() {
        let p = PathBuf::from("/foo/bar/../baz/./qux");
        let n = normalize_path(&p);
        assert_eq!(n, PathBuf::from("/foo/baz/qux"));
    }
}
