//! Git operations for `zz pm`.
//!
//! All git invocations shell out to the `git` binary.
//! **CRITICAL:** Never use `std::env::set_current_dir` — all `current_dir` calls
//! use an explicit unique temp dir per operation so parallel rayon workers
//! don't cross-contaminate checkout state.

use std::path::PathBuf;
use std::process::Command;

use crate::resolve::ResolveError;

/// Resolve a git revision (branch, tag, or commit hash) to a full commit SHA.
///
/// Clones the repo into a unique temp dir, then rev-parse's the rev.
/// Returns the full 40-char SHA-1 commit hash.
pub fn resolve_rev(url: &str, rev: &str) -> Result<String, ResolveError> {
    let clone_dir = unique_clone_dir(url)?;

    // Clone (shallow, single branch)
    let output = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--single-branch",
            url,
            &clone_dir.to_string_lossy(),
        ])
        .output()
        .map_err(|e| ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: format!("failed to spawn git: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Clean up failed clone
        let _ = std::fs::remove_dir_all(&clone_dir);
        return Err(ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: stderr.trim().to_string(),
        });
    }

    // Rev-parse the requested revision
    let output = Command::new("git")
        .current_dir(&clone_dir)
        .args(["rev-parse", rev])
        .output()
        .map_err(|e| ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: format!("failed to spawn git rev-parse: {e}"),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !output.status.success() || !is_valid_sha(&stdout) {
        let _stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&clone_dir);
        return Err(ResolveError::GitRevNotFound {
            name: String::new(),
            url: url.to_string(),
            rev: rev.to_string(),
        });
    }

    // Clean up — we only needed the rev, not the full checkout
    // (CAS will do its own clone when actually fetching content)
    let _ = std::fs::remove_dir_all(&clone_dir);

    Ok(stdout)
}

/// Fetch the content of a specific commit into the CAS.
///
/// Returns the path to the fetched content directory.
pub fn fetch_to_cas(url: &str, commit: &str) -> Result<PathBuf, ResolveError> {
    let cas_dir = crate::paths::cas_entry(commit);

    if cas_dir.exists() {
        return Ok(cas_dir);
    }

    // Create parent dirs
    if let Some(parent) = cas_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ResolveError::Io(format!("cannot create CAS parent: {e}")))?;
    }

    // Clone into a scratch sibling of the CAS entry (same filesystem —
    // the system temp dir is often tmpfs while ~/.zz lives on disk),
    // then stage it.
    let tmp_dir = crate::cas::scratch_sibling(&cas_dir, &format!("git-{commit}"));
    if let Some(parent) = tmp_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ResolveError::Io(format!("cannot create CAS parent: {e}")))?;
    }

    let output = Command::new("git")
        .args(["clone", "--depth", "1", url, &tmp_dir.to_string_lossy()])
        .output()
        .map_err(|e| ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: format!("failed to spawn git: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: stderr.trim().to_string(),
        });
    }

    // Checkout the specific commit
    let output = Command::new("git")
        .current_dir(&tmp_dir)
        .args(["checkout", commit])
        .output()
        .map_err(|e| ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: format!("failed to checkout {commit}: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(ResolveError::GitFetchFailed {
            name: String::new(),
            url: url.to_string(),
            detail: format!("checkout {commit} failed: {stderr}"),
        });
    }

    // Remove .git dir — we only need the source content in CAS
    let _ = std::fs::remove_dir_all(tmp_dir.join(".git"));

    // Move to CAS (rename, EXDEV-safe copy fallback, race-tolerant).
    crate::cas::stage_dir(&tmp_dir, &cas_dir)
        .map_err(|e| ResolveError::Io(format!("cannot move clone to CAS: {e}")))?;

    Ok(cas_dir)
}

/// Generate a unique temp directory path for cloning.
///
/// Uses the system temp dir + a hash of the URL + PID + timestamp
/// to ensure uniqueness across parallel workers.
fn unique_clone_dir(key: &str) -> Result<PathBuf, ResolveError> {
    let base = std::env::temp_dir();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let hash = crate::hash::hash_bytes(format!("{key}:{pid}:{timestamp}").as_bytes());
    let short_hash = &hash[..16.min(hash.len())];
    Ok(base.join(format!("zz_pm_git_{short_hash}")))
}

/// Check if a string looks like a valid hex SHA.
fn is_valid_sha(s: &str) -> bool {
    s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_clone_dir_is_unique() {
        let d1 = unique_clone_dir("test1").unwrap();
        let d2 = unique_clone_dir("test2").unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn unique_clone_dir_not_empty() {
        let d = unique_clone_dir("test").unwrap();
        assert!(d.to_string_lossy().contains("zz_pm_git_"));
    }

    #[test]
    fn is_valid_sha_true() {
        assert!(is_valid_sha("abc1234"));
        assert!(is_valid_sha("ABCDEF0123456789abcdef0123456789abcdef01"));
    }

    #[test]
    fn is_valid_sha_false() {
        assert!(!is_valid_sha(""));
        assert!(!is_valid_sha("abc"));
        assert!(!is_valid_sha("xyz1234"));
    }

    #[test]
    fn resolve_rev_error_display() {
        let err = ResolveError::GitRevNotFound {
            name: "test".to_string(),
            url: "https://example.com/repo.git".to_string(),
            rev: "v1.0".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("v1.0"));
        assert!(msg.contains("test"));
    }
}
