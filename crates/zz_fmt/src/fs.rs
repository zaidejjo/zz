//! File-system discovery (M4).
//!
//! Uses the `ignore` crate's `WalkBuilder` to recursively discover `.zz`
//! source files. Automatically respects `.gitignore`, `.ignore`, and an
//! optional `.zzignore` file. Always skips common build/vendor directories
//! (`target/`, `node_modules/`, `.git/`) regardless of ignore configuration.

use crate::error::FmtError;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Name of the ZZ-specific ignore file.
const ZZIGNORE_FILENAME: &str = ".zzignore";

/// Default directories that are always skipped, even if not gitignored.
const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git"];

/// Discover all `.zz` files at the given paths.
///
/// For directories, walks recursively using `ignore::WalkBuilder` which
/// automatically honors `.gitignore`, `.ignore`, and the optional
/// `.zzignore` file. Files that are not under a repository root fall back
/// to filesystem-only ignore rules.
///
/// Returns results sorted for deterministic output.
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

fn walk_dir(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), FmtError> {
    let mut builder = WalkBuilder::new(root);

    // Honor .gitignore (default), .ignore, and our custom .zzignore.
    builder
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .add_custom_ignore_filename(ZZIGNORE_FILENAME);

    // Standard hidden-file filter (skips `.git/` etc.).
    builder.hidden(true);

    // Filter entries: skip always-skipped dirs and non-.zz files.
    for result in builder.build() {
        let entry = result.map_err(|e| FmtError::Walk(e.to_string()))?;
        let path = entry.path();

        // Skip the always-excluded directories.
        if should_skip(path) {
            continue;
        }

        if entry.file_type().is_some_and(|ft| ft.is_file())
            && path.extension().and_then(|s| s.to_str()) == Some("zz")
        {
            out.push(path.to_path_buf());
        }
    }

    Ok(())
}

/// Returns `true` if any path component is in the hard-coded skip list.
fn should_skip(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(s) = name.to_str() {
                if SKIP_DIRS.contains(&s) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp directory tree and run `discover` on it.
    fn setup_tree(base: &Path) {
        // Source files.
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(base.join("src/main.zz"), "fn main() {}").unwrap();
        fs::write(base.join("src/util.zz"), "fn util() {}").unwrap();

        // Ignored directory — should be skipped by .gitignore.
        fs::create_dir_all(base.join("target/debug")).unwrap();
        fs::write(base.join("target/debug/out.zz"), "fn ignored() {}").unwrap();

        // node_modules — always skipped.
        fs::create_dir_all(base.join("node_modules/pkg")).unwrap();
        fs::write(base.join("node_modules/pkg/index.zz"), "fn nope() {}").unwrap();

        // .git — always skipped.
        fs::create_dir_all(base.join(".git/objects")).unwrap();
        fs::write(base.join(".git/objects/data.zz"), "fn nope() {}").unwrap();

        // Non-zz file — should be skipped by extension filter.
        fs::write(base.join("README.md"), "# Hello").unwrap();

        // .gitignore: ignore target/
        fs::write(base.join(".gitignore"), "target/\n").unwrap();
    }

    #[test]
    fn discover_skips_ignored_and_non_zz() {
        let tmp = std::env::temp_dir().join("zz_fmt_test_fs");
        let _ = fs::remove_dir_all(&tmp);
        setup_tree(&tmp);

        let mut found = discover(std::slice::from_ref(&tmp)).unwrap();
        // Strip the temp prefix for assertion clarity.
        found.sort();

        // Should find exactly src/main.zz and src/util.zz.
        let names: Vec<String> = found
            .iter()
            .map(|p| p.strip_prefix(&tmp).unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(
            names.contains(&"src/main.zz".to_string()),
            "expected src/main.zz in {names:?}"
        );
        assert!(
            names.contains(&"src/util.zz".to_string()),
            "expected src/util.zz in {names:?}"
        );
        assert_eq!(
            names.len(),
            2,
            "should skip target/, node_modules/, .git/, and non-zz files: {names:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_respects_zzignore() {
        let tmp = std::env::temp_dir().join("zz_fmt_test_fs_zzignore");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::write(tmp.join("src/a.zz"), "").unwrap();
        fs::write(tmp.join("src/b.zz"), "").unwrap();
        fs::write(tmp.join(".zzignore"), "src/b.zz\n").unwrap();

        let found = discover(std::slice::from_ref(&tmp)).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.strip_prefix(&tmp).unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, vec!["src/a.zz"]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_missing_path_errors() {
        let result = discover(&[PathBuf::from("/nonexistent/path/zz_fmt_test_404")]);
        assert!(result.is_err());
    }

    #[test]
    fn discover_single_file() {
        let tmp = std::env::temp_dir().join("zz_fmt_test_fs_single");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("hello.zz"), "").unwrap();

        let found = discover(&[tmp.join("hello.zz")]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file_name().unwrap(), "hello.zz");

        let _ = fs::remove_dir_all(&tmp);
    }
}
