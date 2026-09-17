//! zz.toml manifest parse/emit.
//!
//! Schema:
//! ```toml
//! [package]
//! name = "my_app"
//! version = "0.1.0"
//!
//! [dependencies]
//! foo = "^1.2.0"
//! bar = { version = "2.0", git = "https://github.com/user/repo", rev = "main" }
//! baz = { path = "../baz" }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hash;

/// Top-level manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub package: PackageSpec,
    #[serde(default)]
    pub dependencies: HashMap<String, DepSpec>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            package: PackageSpec {
                name: "untitled".to_string(),
                version: "0.1.0".to_string(),
            },
            dependencies: HashMap::new(),
        }
    }
}

/// Package metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageSpec {
    pub name: String,
    pub version: String,
}

/// Dependency specification — three variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DepSpec {
    /// Simple version range: `"^1.2.0"`.
    Version(String),
    /// Git dependency with version constraint.
    Git(GitDep),
    /// Path dependency (local filesystem).
    // TODO(workspace): cross-source version conflicts (same package name available
    // as both a registry version and a path dep, e.g. once [workspace] ships)
    // are currently undefined behavior — not resolved now, just flagged so it
    // isn't silently assumed away later.
    Path(PathDep),
}

/// Git dependency spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitDep {
    pub version: String,
    pub git: String,
    pub rev: String,
}

/// Path dependency spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathDep {
    pub path: String,
}

impl Manifest {
    /// Load a manifest from a `zz.toml` file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse a manifest from a string.
    pub fn parse(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| format!("invalid zz.toml: {e}"))
    }

    /// Save the manifest to a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("cannot serialize manifest: {e}"))?;
        std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Hash of the `[dependencies]` section — for lockfile short-circuit.
    /// Stable: same deps produce same hash regardless of TOML whitespace/order.
    pub fn deps_hash(&self) -> String {
        // Sort keys for determinism
        let mut sorted: Vec<_> = self.dependencies.iter().collect();
        sorted.sort_by_key(|(k, _)| (*k).clone());
        let serialized = serde_json::to_string(&sorted).unwrap_or_default();
        hash::hash_bytes(serialized.as_bytes())
    }

    /// Check if this manifest has any path dependencies.
    pub fn has_path_deps(&self) -> bool {
        self.dependencies
            .values()
            .any(|d| matches!(d, DepSpec::Path(_)))
    }

    /// Resolve a path dep relative to the manifest directory.
    pub fn resolve_path_dep(&self, manifest_dir: &Path, dep_name: &str) -> Option<PathBuf> {
        match self.dependencies.get(dep_name)? {
            DepSpec::Path(p) => Some(manifest_dir.join(&p.path)),
            _ => None,
        }
    }

    /// Create a minimal init manifest in the current directory.
    pub fn create_init(dir: &Path, name: &str) -> Result<Self, String> {
        let manifest = Manifest {
            package: PackageSpec {
                name: name.to_string(),
                version: "0.1.0".to_string(),
            },
            dependencies: HashMap::new(),
        };
        let path = dir.join("zz.toml");
        manifest.save(&path)?;
        Ok(manifest)
    }

    /// Create a new project directory with manifest and starter code.
    pub fn create_new(
        parent_dir: &Path,
        name: &str,
        template: Option<&str>,
    ) -> Result<PathBuf, String> {
        let project_dir = parent_dir.join(name);
        std::fs::create_dir_all(&project_dir)
            .map_err(|e| format!("cannot create {}: {e}", project_dir.display()))?;
        std::fs::create_dir_all(project_dir.join("src"))
            .map_err(|e| format!("cannot create src/: {e}"))?;

        // Write manifest
        Self::create_init(&project_dir, name)?;

        // Write starter source
        let main_content = match template {
            Some("lib") => {
                "/// Add one to a number.\npub func add_one(n: int) -> int {\n    n + 1\n}\n"
            }
            Some("web") => {
                "import std.http\n\nfunc main() {\n    s := http.server()\n    s2 := http.route_get(s, \"/\", |req| \"Hello, ZZ!\")\n    http.listen(s2, 8080) ?? println(\"failed to start server\")\n}\n"
            }
            _ => {
                "import std.io\n\nfunc main() {\n    io.println(\"Hello, ZZ!\")\n}\n"
            }
        };
        std::fs::write(project_dir.join("src/main.zz"), main_content)
            .map_err(|e| format!("cannot write src/main.zz: {e}"))?;

        Ok(project_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("zz_manifest_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let m = Manifest {
            package: PackageSpec {
                name: "test_pkg".into(),
                version: "1.0.0".into(),
            },
            dependencies: {
                let mut d = HashMap::new();
                d.insert("foo".into(), DepSpec::Version("^1.2.0".into()));
                d.insert(
                    "bar".into(),
                    DepSpec::Git(GitDep {
                        version: "2.0".into(),
                        git: "https://github.com/user/repo".into(),
                        rev: "main".into(),
                    }),
                );
                d.insert(
                    "baz".into(),
                    DepSpec::Path(PathDep {
                        path: "../baz".into(),
                    }),
                );
                d
            },
        };

        let d = tmp_dir("round_trip");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn deps_hash_stable() {
        let m1 = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
foo = "^1.0"
bar = "^2.0"
"#,
        )
        .unwrap();

        let m2 = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
bar = "^2.0"
foo = "^1.0"
"#,
        )
        .unwrap();

        // Different TOML ordering, same deps → same hash
        assert_eq!(m1.deps_hash(), m2.deps_hash());
    }

    #[test]
    fn deps_hash_empty() {
        let m = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"
"#,
        )
        .unwrap();
        let h = m.deps_hash();
        assert!(!h.is_empty());
    }

    #[test]
    fn has_path_deps_true() {
        let m = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
foo = { path = "../foo" }
"#,
        )
        .unwrap();
        assert!(m.has_path_deps());
    }

    #[test]
    fn has_path_deps_false() {
        let m = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
foo = "^1.0"
"#,
        )
        .unwrap();
        assert!(!m.has_path_deps());
    }

    #[test]
    fn create_new_cli_template() {
        let d = tmp_dir("new_cli");
        let project = Manifest::create_new(&d, "myapp", None).unwrap();
        assert!(project.join("zz.toml").exists());
        assert!(project.join("src/main.zz").exists());
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("Hello, ZZ!"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_new_lib_template() {
        let d = tmp_dir("new_lib");
        let project = Manifest::create_new(&d, "mylib", Some("lib")).unwrap();
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("add_one"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_new_web_template() {
        let d = tmp_dir("new_web");
        let project = Manifest::create_new(&d, "myweb", Some("web")).unwrap();
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("http.server"));
        let _ = fs::remove_dir_all(&d);
    }
}
