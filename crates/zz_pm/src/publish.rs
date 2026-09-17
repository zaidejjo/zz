//! Package validation and packing for `zz pm`.
//!
//! Validates a package before publishing:
//! - No path dependencies in the published manifest
//! - Package has required fields (name, version)
//! - Runs `zz test` to ensure correctness
//! - Packs source + manifest into a tarball
//!
//! No real upload target in M2 — validation + packing only.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;

/// Errors during publish validation.
#[derive(Debug)]
pub enum PublishError {
    /// Package has path dependencies that cannot be published.
    PathDepsNotAllowed(Vec<String>),
    /// Missing required field.
    MissingField(String),
    /// Test failure.
    TestsFailed(String),
    /// I/O error.
    Io(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathDepsNotAllowed(names) => {
                write!(
                    f,
                    "cannot publish: path dependencies not allowed in published packages\n\
                     --> remove or replace: {}",
                    names.join(", ")
                )
            }
            Self::MissingField(field) => {
                write!(f, "cannot publish: missing required field `{field}`")
            }
            Self::TestsFailed(detail) => {
                write!(f, "cannot publish: tests failed\n{detail}")
            }
            Self::Io(msg) => write!(f, "publish I/O error: {msg}"),
        }
    }
}

impl std::error::Error for PublishError {}

/// Validate a manifest for publishing.
///
/// Checks that:
/// 1. Package name is set (not "untitled")
/// 2. Version is set
/// 3. No path dependencies exist
pub fn validate(manifest: &Manifest) -> Result<(), PublishError> {
    // Check package name
    if manifest.package.name == "untitled" || manifest.package.name.is_empty() {
        return Err(PublishError::MissingField("package.name".to_string()));
    }

    // Check version
    if manifest.package.version.is_empty() {
        return Err(PublishError::MissingField("package.version".to_string()));
    }

    // Check for path dependencies — these cannot be published
    let path_deps: Vec<String> = manifest
        .dependencies
        .iter()
        .filter(|(_, spec)| matches!(spec, crate::manifest::DepSpec::Path(_)))
        .map(|(name, _)| name.clone())
        .collect();

    if !path_deps.is_empty() {
        return Err(PublishError::PathDepsNotAllowed(path_deps));
    }

    Ok(())
}

/// Run `zz test` on the project to verify correctness before publishing.
pub fn run_tests(project_dir: &Path) -> Result<(), PublishError> {
    let status = std::process::Command::new("zz")
        .arg("test")
        .arg(".")
        .current_dir(project_dir)
        .output()
        .map_err(|e| PublishError::TestsFailed(format!("cannot run zz test: {e}")))?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(PublishError::TestsFailed(stderr.trim().to_string()));
    }

    Ok(())
}

/// Pack a validated package into a tarball.
///
/// Creates `<name>-<version>.tar.gz` in the project directory containing:
/// - `zz.toml`
/// - `src/` directory (all `.zz` files)
/// - `README.md` (if exists)
/// - `LICENSE` (if exists)
///
/// Returns the path to the created tarball.
pub fn pack(project_dir: &Path, manifest: &Manifest) -> Result<PathBuf, PublishError> {
    let pkg_name = &manifest.package.name;
    let pkg_version = &manifest.package.version;
    let tarball_name = format!("{pkg_name}-{pkg_version}.tar.gz");
    let tarball_path = project_dir.join(&tarball_name);

    // Build list of files to include
    let mut files_to_pack: Vec<PathBuf> = Vec::new();

    // Always include zz.toml
    let toml_path = project_dir.join("zz.toml");
    if toml_path.exists() {
        files_to_pack.push(toml_path);
    }

    // Include all .zz files in src/
    let src_dir = project_dir.join("src");
    if src_dir.exists() {
        collect_zz_files(&src_dir, project_dir, &mut files_to_pack);
    }

    // Include README.md if present
    let readme = project_dir.join("README.md");
    if readme.exists() {
        files_to_pack.push(readme);
    }

    // Include LICENSE if present
    let license = project_dir.join("LICENSE");
    if license.exists() {
        files_to_pack.push(license);
    }

    if files_to_pack.is_empty() {
        return Err(PublishError::Io(
            "no files to pack — is this a valid ZZ project?".to_string(),
        ));
    }

    // Create tarball
    let tar_gz = std::fs::File::create(&tarball_path)
        .map_err(|e| PublishError::Io(format!("cannot create tarball: {e}")))?;

    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);

    for file in &files_to_pack {
        let rel_path = file.strip_prefix(project_dir).unwrap_or(file);
        tar.append_path_with_name(file, rel_path).map_err(|e| {
            PublishError::Io(format!("cannot add {} to tarball: {e}", rel_path.display()))
        })?;
    }

    tar.finish()
        .map_err(|e| PublishError::Io(format!("cannot finish tarball: {e}")))?;

    Ok(tarball_path)
}

/// Recursively collect `.zz` files in a directory.
fn collect_zz_files(dir: &Path, _base: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_zz_files(&path, _base, files);
            } else if path.extension().is_some_and(|e| e == "zz") {
                files.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DepSpec, PathDep};
    use std::fs;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("zz_pm_publish_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn validate_good_manifest() {
        let mut manifest = Manifest::default();
        manifest.package.name = "my_cool_lib".to_string();
        manifest.package.version = "1.0.0".to_string();

        assert!(validate(&manifest).is_ok());
    }

    #[test]
    fn validate_rejects_path_deps() {
        let mut manifest = Manifest::default();
        manifest.package.name = "my_lib".to_string();
        manifest.package.version = "1.0.0".to_string();
        manifest.dependencies.insert(
            "local_dep".to_string(),
            DepSpec::Path(PathDep {
                path: "../local_dep".to_string(),
            }),
        );

        let err = validate(&manifest).unwrap_err();
        match err {
            PublishError::PathDepsNotAllowed(names) => {
                assert!(names.contains(&"local_dep".to_string()));
            }
            _ => panic!("expected PathDepsNotAllowed"),
        }
    }

    #[test]
    fn validate_rejects_untitled_name() {
        let manifest = Manifest::default(); // name = "untitled"
        let err = validate(&manifest).unwrap_err();
        match err {
            PublishError::MissingField(field) => assert_eq!(field, "package.name"),
            _ => panic!("expected MissingField"),
        }
    }

    #[test]
    fn validate_rejects_empty_version() {
        let mut manifest = Manifest::default();
        manifest.package.name = "my_lib".to_string();
        manifest.package.version = "".to_string();
        let err = validate(&manifest).unwrap_err();
        match err {
            PublishError::MissingField(field) => assert_eq!(field, "package.version"),
            _ => panic!("expected MissingField"),
        }
    }

    #[test]
    fn pack_creates_tarball() {
        let d = tmp();
        let mut manifest = Manifest::default();
        manifest.package.name = "test_pkg".to_string();
        manifest.package.version = "0.1.0".to_string();

        // Create minimal project structure
        fs::write(d.join("zz.toml"), "").unwrap();
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src/main.zz"), "func main() { }").unwrap();

        let result = pack(&d, &manifest);
        assert!(result.is_ok(), "pack failed: {result:?}");

        let tarball = result.unwrap();
        assert!(tarball.exists());
        assert!(tarball.to_string_lossy().contains("test_pkg-0.1.0.tar.gz"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn validate_accepts_git_deps() {
        let mut manifest = Manifest::default();
        manifest.package.name = "my_lib".to_string();
        manifest.package.version = "1.0.0".to_string();
        manifest.dependencies.insert(
            "some_dep".to_string(),
            DepSpec::Git(crate::manifest::GitDep {
                version: "^1.0".to_string(),
                git: "https://github.com/example/repo.git".to_string(),
                rev: "main".to_string(),
            }),
        );

        // Git deps are fine for publishing
        assert!(validate(&manifest).is_ok());
    }
}
