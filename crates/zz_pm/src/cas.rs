//! Content-addressable storage with read-only enforcement.
//!
//! CAS layout: `~/.zz/packages/<algo>/<pfx2>/<full-hash>/`
//! - `manifest.json` — source URL, version, sub-hashes
//! - Extracted package source files
//!
//! Read-only enforcement:
//! - All files in CAS are chmod 0444 (Unix) / readonly attrib (Windows)
//! - Hardlink creation inherits read-only from source inode
//! - Symlink is a path (no permissions)
//! - Copy copies bytes but project-side copy is writable
//!
//! Link fallback chain: hardlink → symlink → copy (with warning)

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hash;
use crate::paths;

/// Options for storing content into CAS.
#[derive(Debug, Clone)]
pub struct CasStoreOptions {
    /// Original source URL or path (recorded in manifest.json).
    pub source_url: String,
    /// Package version.
    pub version: String,
    /// Algorithm label (default: "sha256").
    pub algorithm: String,
}

impl Default for CasStoreOptions {
    fn default() -> Self {
        Self {
            source_url: String::new(),
            version: String::new(),
            algorithm: "sha256".into(),
        }
    }
}

/// Manifest stored alongside CAS content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasManifest {
    pub source_url: String,
    pub version: String,
    pub algorithm: String,
    pub content_hash: String,
}

/// Place content into CAS. Returns the CAS path.
///
/// 1. Computes content hash of source_dir
/// 2. Creates `~/.zz/packages/<algo>/<pfx2>/<hash>/` directory
/// 3. Copies content into CAS dir
/// 4. Writes `manifest.json`
/// 5. **Sets chmod 0444 / readonly on all CAS files**
pub fn store_in_cas(source_dir: &Path, opts: &CasStoreOptions) -> Result<PathBuf, String> {
    let hash_opts = hash::HashOptions::default();
    let content_hash = hash::hash_dir(source_dir, &hash_opts)?;

    // CAS path: packages/<algo>/<first-2-chars>/<full-hash>/
    let cas_base = paths::packages_dir()
        .join(&opts.algorithm)
        .join(&content_hash[..2.min(content_hash.len())])
        .join(&content_hash);

    // If already exists, return it (content-addressed = idempotent)
    if cas_base.exists() {
        return Ok(cas_base);
    }

    std::fs::create_dir_all(&cas_base)
        .map_err(|e| format!("cannot create CAS dir {}: {e}", cas_base.display()))?;

    // Copy source files into CAS
    copy_dir_recursive(source_dir, &cas_base)?;

    // Write manifest
    let manifest = CasManifest {
        source_url: opts.source_url.clone(),
        version: opts.version.clone(),
        algorithm: opts.algorithm.clone(),
        content_hash: content_hash.clone(),
    };
    let manifest_path = cas_base.join("manifest.json");
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("cannot serialize manifest: {e}"))?;
    std::fs::write(&manifest_path, manifest_json)
        .map_err(|e| format!("cannot write manifest: {e}"))?;

    // Set all files to read-only
    set_readonly_recursive(&cas_base)?;

    Ok(cas_base)
}

/// Link CAS content into a project directory.
///
/// Fallback chain: hardlink → symlink → copy (with warning).
/// CAS files remain read-only; project-side files are writable.
pub fn link_into_project(
    cas_path: &Path,
    project_dir: &Path,
    pkg_name: &str,
) -> Result<PathBuf, String> {
    let dest = project_dir.join(pkg_name);
    std::fs::create_dir_all(&dest)
        .map_err(|e| format!("cannot create dir {}: {e}", dest.display()))?;

    // Link each file from CAS into the project
    let files = collect_files(cas_path)?;
    for file in &files {
        let rel = file
            .strip_prefix(cas_path)
            .map_err(|e| format!("cannot compute relative path: {e}"))?;
        let file_dest = dest.join(rel);
        if let Some(parent) = file_dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create dir {}: {e}", parent.display()))?;
        }

        // Try hardlink first
        match std::fs::hard_link(file, &file_dest) {
            Ok(()) => continue,
            Err(_) => {
                // Fallback: try symlink
                #[cfg(unix)]
                {
                    match std::os::unix::fs::symlink(file, &file_dest) {
                        Ok(()) => continue,
                        Err(_) => {
                            // Fallback: copy
                            eprintln!(
                                "warning: hardlink and symlink failed for {}; falling back to copy (dedup disabled for this file)",
                                rel.display()
                            );
                            std::fs::copy(file, &file_dest)
                                .map_err(|e| format!("cannot copy {}: {e}", file.display()))?;
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    // Windows without dev mode: copy
                    eprintln!(
                        "warning: hardlink failed for {}; falling back to copy (dedup disabled for this file)",
                        rel.display()
                    );
                    std::fs::copy(file, &file_dest)
                        .map_err(|e| format!("cannot copy {}: {e}", file.display()))?;
                }
            }
        }
    }

    Ok(dest)
}

/// Verify CAS integrity: hash matches directory name.
pub fn verify_cas(cas_path: &Path) -> Result<bool, String> {
    let dir_name = cas_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("invalid CAS path: {}", cas_path.display()))?;

    let hash_opts = hash::HashOptions::default();
    // Hash the CAS content (excluding manifest.json)
    let actual_hash = hash_dir_excluding(cas_path, &["manifest.json"], &hash_opts)?;

    Ok(actual_hash == dir_name)
}

/// Find CAS entry by hash.
pub fn find_in_cas(content_hash: &str) -> Result<Option<PathBuf>, String> {
    let cas_path = paths::packages_dir()
        .join("sha256")
        .join(&content_hash[..2.min(content_hash.len())])
        .join(content_hash);

    if cas_path.exists() {
        Ok(Some(cas_path))
    } else {
        Ok(None)
    }
}

/// Set a file to read-only (0444 on Unix, readonly on Windows).
#[cfg(unix)]
fn set_readonly(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
        .map_err(|e| format!("cannot set read-only on {}: {e}", path.display()))
}

/// Set a file to read-only (readonly attrib on Windows).
#[cfg(windows)]
fn set_readonly(path: &Path) -> Result<(), String> {
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
        .permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms)
        .map_err(|e| format!("cannot set read-only on {}: {e}", path.display()))
}

/// Set read-only on all files in a directory recursively.
fn set_readonly_recursive(dir: &Path) -> Result<(), String> {
    let files = collect_files(dir)?;
    for file in &files {
        set_readonly(file)?;
    }
    Ok(())
}

/// Copy a directory recursively.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    let files = collect_files(src)?;
    for file in &files {
        let rel = file
            .strip_prefix(src)
            .map_err(|e| format!("cannot compute relative path: {e}"))?;
        let dest = dst.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create dir {}: {e}", parent.display()))?;
        }
        std::fs::copy(file, &dest).map_err(|e| format!("cannot copy {}: {e}", file.display()))?;
    }
    Ok(())
}

/// Collect all files in a directory recursively, sorted.
fn collect_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read dir {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// Hash a directory, excluding certain filenames.
fn hash_dir_excluding(
    dir: &Path,
    exclude: &[&str],
    opts: &hash::HashOptions,
) -> Result<String, String> {
    let all_files = collect_files(dir)?;
    let files: Vec<_> = all_files
        .iter()
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !exclude.contains(&n))
                .unwrap_or(true)
        })
        .collect();

    use sha2::{Digest, Sha256};
    let mut combined = Sha256::new();
    for file in &files {
        let data =
            std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let rel = file.strip_prefix(dir).unwrap_or(file).to_string_lossy();
        // Normalize text extensions
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let normalized = if opts.text_extensions.iter().any(|e| e == &ext) {
            let text = String::from_utf8_lossy(&data);
            text.replace("\r\n", "\n")
        } else {
            String::from_utf8_lossy(&data).into_owned()
        };
        let prefixed = format!("{}\0{}", rel, normalized);
        combined.update(prefixed.as_bytes());
    }
    Ok(hex::encode(combined.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zz_cas_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn store_and_find() {
        let src = tmp_dir("store_src");
        fs::write(src.join("hello.zz"), "x := 1\n").unwrap();

        let opts = CasStoreOptions {
            source_url: "test://example".into(),
            version: "1.0.0".into(),
            ..Default::default()
        };
        let cas_path = store_in_cas(&src, &opts).unwrap();

        // CAS dir exists
        assert!(cas_path.exists());
        assert!(cas_path.join("manifest.json").exists());
        assert!(cas_path.join("hello.zz").exists());

        // Find by hash
        let hash = cas_path.file_name().unwrap().to_str().unwrap().to_string();
        let found = find_in_cas(&hash).unwrap();
        assert_eq!(found, Some(cas_path.clone()));

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&cas_path);
    }

    #[test]
    fn readonly_enforced() {
        let src = tmp_dir("readonly_src");
        fs::write(src.join("test.txt"), "content\n").unwrap();

        let opts = CasStoreOptions::default();
        let cas_path = store_in_cas(&src, &opts).unwrap();

        // Check file is read-only
        let file = cas_path.join("test.txt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::metadata(&file).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o444, "CAS file should be 0444");
        }
        #[cfg(windows)]
        {
            let perms = fs::metadata(&file).unwrap().permissions();
            assert!(perms.readonly(), "CAS file should be readonly");
        }

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&cas_path);
    }

    #[test]
    fn hardlink_write_fails() {
        let src = tmp_dir("hl_write_src");
        fs::write(src.join("file.txt"), "original\n").unwrap();

        let opts = CasStoreOptions::default();
        let cas_path = store_in_cas(&src, &opts).unwrap();

        // Create project dir under same CAS root to avoid cross-device link
        let project = cas_path.join("_test_project");
        fs::create_dir_all(&project).unwrap();
        let dest = project.join("linked.txt");
        fs::hard_link(cas_path.join("file.txt"), &dest).unwrap();

        // Attempt to write through the hardlink — should fail on Unix
        #[cfg(unix)]
        {
            let result = fs::write(&dest, "tampered\n");
            assert!(
                result.is_err(),
                "write through read-only hardlink should fail"
            );
        }

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn hardlink_removal_after_readonly() {
        // Amendment 1: verify hardlink removal works when target is read-only CAS
        let src = tmp_dir("hl_rm_src");
        fs::write(src.join("data.zz"), "x := 42\n").unwrap();

        let opts = CasStoreOptions::default();
        let cas_path = store_in_cas(&src, &opts).unwrap();
        let cas_file = cas_path.join("data.zz");

        // Create project dir under same CAS root to avoid cross-device link
        let project = cas_path.join("_test_project_rm");
        fs::create_dir_all(&project).unwrap();
        let link_path = project.join("data.zz");
        fs::hard_link(&cas_file, &link_path).unwrap();
        assert!(link_path.exists());

        // (a) Remove the project-side hardlink
        fs::remove_file(&link_path).unwrap();
        assert!(!link_path.exists());

        // (b) CAS original is untouched and still 0444
        assert!(cas_file.exists(), "CAS file should still exist");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::metadata(&cas_file).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o444, "CAS file should still be 0444");
        }
        #[cfg(windows)]
        {
            let perms = fs::metadata(&cas_file).unwrap().permissions();
            assert!(perms.readonly(), "CAS file should still be readonly");
        }

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn symlink_removal_after_readonly() {
        // Verify symlink removal works when target is read-only CAS
        let src = tmp_dir("sl_rm_src");
        fs::write(src.join("data.zz"), "x := 42\n").unwrap();

        let opts = CasStoreOptions::default();
        let cas_path = store_in_cas(&src, &opts).unwrap();
        let cas_file = cas_path.join("data.zz");

        // Create project dir and symlink
        let project = tmp_dir("sl_rm_project");
        let link_path = project.join("data.zz");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&cas_file, &link_path).unwrap();
            assert!(link_path.exists());

            // Remove symlink — should succeed
            fs::remove_file(&link_path).unwrap();
            assert!(!link_path.exists());

            // CAS original still exists
            assert!(cas_file.exists());
        }

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&cas_path);
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn verify_integrity() {
        let src = tmp_dir("verify_src");
        fs::write(src.join("file.zz"), "test\n").unwrap();

        let opts = CasStoreOptions::default();
        let cas_path = store_in_cas(&src, &opts).unwrap();

        // Integrity check should pass
        assert!(verify_cas(&cas_path).unwrap());

        // Tamper the file — integrity should fail
        // Need to temporarily make it writable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                cas_path.join("file.zz"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        #[cfg(windows)]
        {
            let mut perms = fs::metadata(cas_path.join("file.zz"))
                .unwrap()
                .permissions();
            perms.set_readonly(false);
            fs::set_permissions(cas_path.join("file.zz"), perms).unwrap();
        }
        fs::write(cas_path.join("file.zz"), "TAMPERED\n").unwrap();

        assert!(
            !verify_cas(&cas_path).unwrap(),
            "tampered CAS should fail integrity check"
        );

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&cas_path);
    }

    #[test]
    fn idempotent_store() {
        let src = tmp_dir("idempotent_src");
        fs::write(src.join("file.zz"), "x\n").unwrap();

        let opts = CasStoreOptions::default();
        let p1 = store_in_cas(&src, &opts).unwrap();
        let p2 = store_in_cas(&src, &opts).unwrap();
        assert_eq!(p1, p2, "storing same content twice should return same path");

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&p1);
    }

    #[test]
    fn link_into_project_hardlink() {
        let src = tmp_dir("link_src");
        fs::write(src.join("file.zz"), "x := 1\n").unwrap();

        let opts = CasStoreOptions::default();
        let cas_path = store_in_cas(&src, &opts).unwrap();

        let project = tmp_dir("link_project");
        let linked = link_into_project(&cas_path, &project, "mylib").unwrap();
        assert!(linked.exists());
        assert!(linked.join("file.zz").exists());

        // Verify it's a hardlink (same inode on Unix)
        #[cfg(unix)]
        {
            let cas_meta = fs::metadata(cas_path.join("file.zz")).unwrap();
            let link_meta = fs::metadata(linked.join("file.zz")).unwrap();
            assert_eq!(cas_meta.ino(), link_meta.ino());
        }

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&cas_path);
        let _ = fs::remove_dir_all(&project);
    }
}
