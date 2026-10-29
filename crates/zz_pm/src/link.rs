//! Project linking for `zz pm`.
//!
//! Manages symlinks from project `vendor/` into the global CAS.
//! Implements Amendment 2: verify existing symlinks resolve to expected targets.

use std::path::{Path, PathBuf};

use crate::lock::Lockfile;
use crate::manifest::Manifest;
use crate::pathsafe::strip_verbatim;

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
///
/// Transitive pins (#420) link exactly like direct ones so
/// `import <transitive>` resolves; stale vendor entries outside the
/// closure are pruned.
pub fn link_project(
    project_dir: &Path,
    manifest: &Manifest,
    lockfile: &Lockfile,
    strategy: LinkStrategy,
) -> Result<Vec<PathBuf>, LinkError> {
    link_project_with_features(
        project_dir,
        manifest,
        lockfile,
        strategy,
        &crate::manifest::FeatureSelection::default(),
    )
}

/// [`link_project`] with an explicit feature selection (#488). Disabled
/// optional deps are not linked (and stale links for them are pruned).
pub fn link_project_with_features(
    project_dir: &Path,
    manifest: &Manifest,
    lockfile: &Lockfile,
    strategy: LinkStrategy,
    selection: &crate::manifest::FeatureSelection,
) -> Result<Vec<PathBuf>, LinkError> {
    let active = manifest
        .active_dependency_names(selection)
        .map_err(LinkError::Io)?;
    let vendor_dir = project_dir.join("vendor");
    std::fs::create_dir_all(&vendor_dir)
        .map_err(|e| LinkError::Io(format!("cannot create vendor/: {e}")))?;

    let mut linked = Vec::new();

    for (name, spec) in &manifest.dependencies {
        if !active.contains(name) {
            continue;
        }
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

    // Transitive layer (#420): lock pins outside the manifest link from
    // their content dirs (path pins) or CAS entries, exactly like direct
    // deps, so `import <transitive>` resolves without `zz add`.
    let closure_dirs =
        crate::resolve::closure_with_features(manifest, lockfile, project_dir, selection)
            .map_err(|e| LinkError::Io(e.to_string()))?;
    for dep in &lockfile.deps {
        if active.contains(&dep.name) {
            continue; // direct layer handled above
        }
        let Some(dir) = closure_dirs.get(&dep.name) else {
            continue; // not in the live closure (pruned below as orphan)
        };
        let target = if dep.source == "path" {
            dir.clone()
        } else if let Some(commit) = &dep.commit {
            let cas_path = crate::paths::cas_entry(commit);
            if !cas_path.exists() {
                return Err(LinkError::CasEntryMissing {
                    hash: dep.hash.clone(),
                });
            }
            cas_path
        } else {
            let cas_path = crate::paths::cas_entry(&dep.hash);
            if !cas_path.exists() {
                return Err(LinkError::CasEntryMissing {
                    hash: dep.hash.clone(),
                });
            }
            cas_path
        };
        let link_path = vendor_dir.join(&dep.name);
        if is_valid_symlink(&link_path, &target) {
            continue;
        }
        create_link(strategy, &target, &link_path)?;
        linked.push(link_path);
    }

    // Prune orphans: vendor entries outside the live closure (removed
    // transitive deps) stop resolving once unlinked. Only symlinks and
    // directories are managed; stray files are left alone.
    if let Ok(entries) = std::fs::read_dir(&vendor_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if closure_dirs.contains_key(&name) || active.contains(&name) {
                continue;
            }
            let path = entry.path();
            let managed = path
                .symlink_metadata()
                .map(|m| m.file_type())
                .map(|t| t.is_symlink() || t.is_dir())
                .unwrap_or(false);
            if managed {
                let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
            }
        }
    }

    Ok(linked)
}

/// Check that every manifest dependency is materialized without changing
/// anything (#323): each vendor/ link exists and points at the target the
/// manifest + lockfile imply (path deps at their local dir, others at their
/// CAS entry, which must itself exist). Lets `zz install` tell
/// resolved-but-unlinked (fresh clone, wiped vendor/) apart from fully
/// installed.
///
/// Transitive pins (#420) are covered too, plus orphan detection: a
/// vendor entry outside the live closure, or a lock pin the closure no
/// longer reaches, reads as not-valid so the next install re-resolves.
pub fn links_valid(project_dir: &Path, manifest: &Manifest, lockfile: &Lockfile) -> bool {
    links_valid_with_features(
        project_dir,
        manifest,
        lockfile,
        &crate::manifest::FeatureSelection::default(),
    )
}

/// [`links_valid`] with an explicit feature selection (#488).
pub fn links_valid_with_features(
    project_dir: &Path,
    manifest: &Manifest,
    lockfile: &Lockfile,
    selection: &crate::manifest::FeatureSelection,
) -> bool {
    let active = match manifest.active_dependency_names(selection) {
        Ok(a) => a,
        Err(_) => return false,
    };
    let vendor_dir = project_dir.join("vendor");
    for (name, spec) in &manifest.dependencies {
        if !active.contains(name) {
            continue;
        }
        let locked = match lockfile.find(name) {
            Some(l) => l,
            None => return false,
        };
        let target = match spec {
            crate::manifest::DepSpec::Path(path_dep) => project_dir.join(&path_dep.path),
            _ => {
                let cas_path = if let Some(commit) = &locked.commit {
                    crate::paths::cas_entry(commit)
                } else {
                    crate::paths::cas_entry(&locked.hash)
                };
                if !cas_path.exists() {
                    return false;
                }
                cas_path
            }
        };
        if !is_valid_symlink(&vendor_dir.join(name), &target) {
            return false;
        }
    }
    // Transitive layer: the closure must resolve (all content fetched),
    // every closure entry must be linked, and no managed vendor entry
    // may sit outside the closure.
    let dirs =
        match crate::resolve::closure_with_features(manifest, lockfile, project_dir, selection) {
            Ok(d) => d,
            Err(_) => return false,
        };
    for (name, dir) in &dirs {
        if active.contains(name) {
            continue; // checked above (non-canonical join targets)
        }
        let locked = match lockfile.find(name) {
            Some(l) => l,
            None => return false,
        };
        let target = if locked.source == "path" {
            dir.clone()
        } else if let Some(commit) = &locked.commit {
            let cas_path = crate::paths::cas_entry(commit);
            if !cas_path.exists() {
                return false;
            }
            cas_path
        } else {
            let cas_path = crate::paths::cas_entry(&locked.hash);
            if !cas_path.exists() {
                return false;
            }
            cas_path
        };
        if !is_valid_symlink(&vendor_dir.join(name), &target) {
            return false;
        }
    }
    // Orphans bust validity so removals prune on the next install.
    if let Ok(entries) = std::fs::read_dir(&vendor_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if dirs.contains_key(&name) {
                continue;
            }
            let managed = entry
                .path()
                .symlink_metadata()
                .map(|m| m.file_type())
                .map(|t| t.is_symlink() || t.is_dir())
                .unwrap_or(false);
            if managed {
                return false;
            }
        }
    }
    true
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

    // Direct layer plus transitive pins (#420): every lock entry gets a
    // verified link, wherever the requirement came from. The closure is
    // walked once for transitive path targets (missing content skips —
    // `link_project` is the repairing path).
    let closure_dirs = crate::resolve::closure(manifest, lockfile, project_dir).ok();
    let mut names: Vec<&String> = lockfile.deps.iter().map(|d| &d.name).collect();
    names.sort();
    names.dedup();
    for name in names {
        let locked = match lockfile.find(name) {
            Some(l) => l,
            None => continue,
        };

        let expected_target = match manifest.dependencies.get(name) {
            Some(crate::manifest::DepSpec::Path(path_dep)) => project_dir.join(&path_dep.path),
            Some(_) if locked.commit.is_some() => {
                crate::paths::cas_entry(locked.commit.as_deref().unwrap_or_default())
            }
            Some(_) => crate::paths::cas_entry(&locked.hash),
            // Transitive pin: path targets come from the closure walk,
            // registry/git from the lock entry's CAS address.
            None if locked.source == "path" => {
                match closure_dirs.as_ref().and_then(|d| d.get(name)) {
                    Some(dir) => dir.clone(),
                    None => continue,
                }
            }
            None if locked.commit.is_some() => {
                crate::paths::cas_entry(locked.commit.as_deref().unwrap_or_default())
            }
            None => crate::paths::cas_entry(&locked.hash),
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
    // Must exist (a dangling link fails here: `exists()` follows it).
    if !link_path.exists() {
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

    // A relative link target resolves against the link's own directory, not
    // the process cwd. Windows stores targets verbatim, so normalizing here
    // keeps both host spellings comparable.
    let actual_abs = if actual_target.is_absolute() {
        actual_target.clone()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&actual_target)
    };

    // Compare real identities first: canonicalization absorbs `..`,
    // symlinked path parents (`/var` → `/private/var` on macOS) and the
    // `\\?\` prefix Windows adds on one side only.
    if let (Ok(actual), Ok(expected)) = (
        std::fs::canonicalize(&actual_abs),
        std::fs::canonicalize(expected_target),
    ) {
        return strip_verbatim(&actual) == strip_verbatim(&expected);
    }

    // One side is gone (racing prune, missing CAS entry): fall back to a
    // lexical comparison of the normalized absolute forms.
    strip_verbatim(&normalize_path(&actual_abs)) == strip_verbatim(&normalize_path(expected_target))
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

    /// Serializes tests that mutate the process-wide `ZZ_HOME` env var.
    /// Without this, parallel test threads overwrite each other's `ZZ_HOME`
    /// mid-test (flaky `re_link_skips_unchanged_symlinks` failures).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Poison-tolerant guard: one failing test must not cascade into
    /// `PoisonError` failures for every test that runs after it.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create a directory symlink with the platform-appropriate call.
    fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no symlink support",
            ))
        }
    }

    /// Windows only allows symlinks in Developer Mode or for elevated
    /// processes. Tests that exist purely to exercise symlinks skip
    /// themselves (with a note) when the privilege is missing, rather than
    /// failing with an unrelated `os error 1314`.
    fn symlinks_supported() -> bool {
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SUPPORTED.get_or_init(|| {
            let probe =
                std::env::temp_dir().join(format!("zz_pm_symlink_probe_{}", std::process::id()));
            let target = probe.join("target");
            let link = probe.join("link");
            let _ = fs::remove_dir_all(&probe);
            if fs::create_dir_all(&target).is_err() {
                return false;
            }
            let ok = symlink_dir(&target, &link).is_ok();
            let _ = fs::remove_dir_all(&probe);
            ok
        })
    }

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
        if !symlinks_supported() {
            eprintln!("skipping valid_symlink_detected: symlinks unavailable");
            return;
        }
        let d = tmp();
        let target = d.join("target");
        let link = d.join("link");
        fs::create_dir_all(&target).unwrap();
        symlink_dir(&target, &link).unwrap();

        assert!(is_valid_symlink(&link, &target));
        // A verbatim-prefixed spelling (what Windows canonicalize returns)
        // must compare equal to the plain one.
        assert!(is_valid_symlink(&link, &strip_verbatim(&target)));
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
        if !symlinks_supported() {
            eprintln!("skipping wrong_target_not_valid: symlinks unavailable");
            return;
        }
        let d = tmp();
        let target1 = d.join("target1");
        let target2 = d.join("target2");
        let link = d.join("link");
        fs::create_dir_all(&target1).unwrap();
        fs::create_dir_all(&target2).unwrap();
        symlink_dir(&target1, &link).unwrap();

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
        let _guard = env_lock();
        if !symlinks_supported() {
            eprintln!("skipping link_project_creates_symlinks: symlinks unavailable");
            return;
        }
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
            .insert("mylib".to_string(), DepSpec::Version("^1.0".into()));

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "^1.0".to_string(),
            source: "git+https://example.com/repo.git#main".to_string(),
            hash: "abc123".to_string(),
            commit: Some("abc123".to_string()),
            native: None,
        });

        let linked = link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert_eq!(linked.len(), 1);
        assert!(project.join("vendor/mylib").exists());
        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn links_valid_after_link_project() {
        // #323: a linked project reads as materialized; a wiped vendor/
        // (fresh clone) reads as needing fetch + link.
        let _guard = env_lock();
        if !symlinks_supported() {
            eprintln!("skipping links_valid_after_link_project: symlinks unavailable");
            return;
        }
        let d = tmp();
        std::env::set_var("ZZ_HOME", &d);

        let project = d.join("project");
        let dep_dir = d.join("mylib");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(
            dep_dir.join("zz.toml"),
            "[package]\nname=\"mylib\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();

        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mylib".to_string(),
            DepSpec::Path(crate::manifest::PathDep::new("../mylib".to_string())),
        );

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "*".to_string(),
            source: "path".to_string(),
            hash: "deadbeef".to_string(),
            commit: None,
            native: None,
        });

        link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert!(links_valid(&project, &manifest, &lock));

        fs::remove_dir_all(project.join("vendor")).unwrap();
        assert!(!links_valid(&project, &manifest, &lock));

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn links_valid_false_without_cas_entry() {
        // #323: a registry pin with no CAS entry is not materialized —
        // install must fall through to fetch, not short-circuit.
        let _guard = env_lock();
        let d = tmp();
        std::env::set_var("ZZ_HOME", &d);

        let project = d.join("project");
        fs::create_dir_all(&project).unwrap();

        let mut manifest = Manifest::default();
        manifest
            .dependencies
            .insert("mylib".to_string(), DepSpec::Version("^1.0".into()));

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "1.0.0".to_string(),
            source: "registry+https://example.com/mylib#1.0.0".to_string(),
            hash: "abc123".to_string(),
            commit: None,
            native: None,
        });

        assert!(!links_valid(&project, &manifest, &lock));

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    /// Write a minimal parseable package manifest (the closure walk reads
    /// every manifest it reaches, so fixtures need name + version).
    fn write_pkg(dir: &Path, name: &str, deps_section: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("zz.toml"),
            format!("[package]\nname=\"{name}\"\nversion=\"0.1.0\"\n{deps_section}"),
        )
        .unwrap();
    }

    #[test]
    fn link_project_links_transitive_path_pins() {
        // root → mid (path) → leaf (path): the lock holds both path pins;
        // link creates vendor/leaf pointing at the mid-relative dir, and
        // the tree validates.
        let _guard = env_lock();
        if !symlinks_supported() {
            eprintln!("skipping link_project_links_transitive_path_pins: symlinks unavailable");
            return;
        }
        let d = tmp();
        std::env::set_var("ZZ_HOME", &d);

        let project = d.join("project");
        fs::create_dir_all(&project).unwrap();
        write_pkg(
            &d.join("mid"),
            "mid",
            "[dependencies]\nleaf = { path = \"../leaf\" }\n",
        );
        write_pkg(&d.join("leaf"), "leaf", "");

        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mid".to_string(),
            DepSpec::Path(crate::manifest::PathDep::new("../mid".to_string())),
        );

        let mut lock = Lockfile::new();
        for name in ["mid", "leaf"] {
            lock.upsert(LockedDep {
                name: name.to_string(),
                version: "*".to_string(),
                source: "path".to_string(),
                hash: format!("hash-{name}"),
                commit: None,
                native: None,
            });
        }

        let linked = link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert_eq!(linked.len(), 2);
        assert!(project.join("vendor/mid").exists());
        let leaf_link = project.join("vendor/leaf");
        assert!(leaf_link.exists(), "transitive leaf linked");
        assert_eq!(
            fs::canonicalize(fs::read_link(&leaf_link).unwrap()).unwrap(),
            fs::canonicalize(d.join("leaf")).unwrap()
        );
        assert!(links_valid(&project, &manifest, &lock));

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn link_project_prunes_orphan_vendor_entries() {
        // A stale vendor/ link outside the closure fails validity and is
        // pruned by the next link.
        let _guard = env_lock();
        if !symlinks_supported() {
            eprintln!("skipping link_project_prunes_orphan_vendor_entries: symlinks unavailable");
            return;
        }
        let d = tmp();
        std::env::set_var("ZZ_HOME", &d);

        let project = d.join("project");
        fs::create_dir_all(&project).unwrap();
        write_pkg(&d.join("mid"), "mid", "");

        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mid".to_string(),
            DepSpec::Path(crate::manifest::PathDep::new("../mid".to_string())),
        );
        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mid".to_string(),
            version: "*".to_string(),
            source: "path".to_string(),
            hash: "hash-mid".to_string(),
            commit: None,
            native: None,
        });

        link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert!(links_valid(&project, &manifest, &lock));

        // Simulate a removed transitive dep lingering in vendor/.
        symlink_dir(&d.join("mid"), &project.join("vendor/old")).unwrap();
        assert!(
            !links_valid(&project, &manifest, &lock),
            "orphan busts validity"
        );
        link_project(&project, &manifest, &lock, LinkStrategy::Symlink).unwrap();
        assert!(!project.join("vendor/old").exists(), "orphan pruned");
        assert!(links_valid(&project, &manifest, &lock));

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn re_link_skips_unchanged_symlinks() {
        let _guard = env_lock();
        if !symlinks_supported() {
            eprintln!("skipping re_link_skips_unchanged_symlinks: symlinks unavailable");
            return;
        }
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
            .insert("mylib".to_string(), DepSpec::Version("^1.0".into()));

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "^1.0".to_string(),
            source: "git+https://example.com/repo.git#main".to_string(),
            hash: "abc123".to_string(),
            commit: Some("abc123".to_string()),
            native: None,
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
