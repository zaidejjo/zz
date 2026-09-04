//! File-system discovery (M4).

use crate::error::FmtError;
use std::path::{Path, PathBuf};

/// Discover all `.zz` files at the given paths. For directories, walks
/// recursively, respecting `.gitignore` and other ignore files. This is
/// the M4 milestone; the current implementation is a simple recursive
/// walker so M0/M1 tests can run.
pub fn discover(paths: &[PathBuf]) -> Result<Vec<PathBuf>, FmtError> {
    let mut out = Vec::new();
    for p in paths {
        collect(p, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) -> Result<(), FmtError> {
    if !p.exists() {
        return Err(FmtError::Walk(format!("path not found: {}", p.display())));
    }
    if p.is_file() {
        if p.extension().and_then(|s| s.to_str()) == Some("zz") {
            out.push(p.to_path_buf());
        }
        return Ok(());
    }
    walk_dir(p, out)
}

fn walk_dir(p: &Path, out: &mut Vec<PathBuf>) -> Result<(), FmtError> {
    use walkdir::WalkDir;
    for entry in WalkDir::new(p)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.path()))
    {
        let entry = entry.map_err(|e| FmtError::Walk(e.to_string()))?;
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|s| s.to_str()) == Some("zz")
        {
            out.push(entry.into_path());
        }
    }
    Ok(())
}

fn is_skipped_dir(p: &Path) -> bool {
    if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
        matches!(name, "target" | "node_modules" | ".git")
    } else {
        false
    }
}
