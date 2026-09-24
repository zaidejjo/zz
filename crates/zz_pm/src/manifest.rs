//! zz.toml manifest parse/emit.
//!
//! Schema:
//! ```toml
//! [package]
//! name = "my_app"
//! version = "0.1.0"
//! authors = ["Alice"]
//! description = "Does things"
//! license = "MIT"
//! repository = "https://github.com/user/my_app"
//!
//! [dependencies]
//! foo = "^1.2.0"
//! bar = { version = "2.0", git = "https://github.com/user/repo", rev = "main" }
//! baz = { path = "../baz" }
//! ```
//!
//! The `authors`, `description`, `license`, and `repository` keys are
//! optional: manifests written before they existed still parse, and
//! `save()` omits them while empty so diffs stay minimal.

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
    /// Native build configuration for packages that provide C/Rust extensions.
    #[serde(default)]
    pub native: Option<NativeSpec>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            package: PackageSpec {
                name: "untitled".to_string(),
                version: "0.1.0".to_string(),
                authors: Vec::new(),
                description: None,
                license: None,
                repository: None,
                category: None,
                keywords: Vec::new(),
            },
            dependencies: HashMap::new(),
            native: None,
        }
    }
}

/// Package metadata.
///
/// `authors`, `description`, `license`, and `repository` are optional so
/// pre-enrichment manifests keep parsing; `save()` skips them while empty.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageSpec {
    pub name: String,
    pub version: String,
    /// e.g. `authors = ["Alice <alice@example.com>"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// Short human-readable summary (sent as `description` on publish).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// SPDX identifier (e.g. `"MIT"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Source URL (sent as `repo` on publish).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Fixed-vocabulary category (sent as `category` on publish).
    /// Canonical values: backend, cli, frameworks, math, gui, utilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Free-form discovery keywords (sent as `keywords` on publish).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

/// Options for scaffolding a new manifest (`zz init` / `zz new` flags).
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// `--author` (repeatable, comma-separated values also split).
    pub authors: Vec<String>,
    /// `--description`.
    pub description: Option<String>,
    /// `--license` (e.g. `MIT`).
    pub license: Option<String>,
    /// `--repo`.
    pub repository: Option<String>,
}

/// Native build configuration for packages that provide C/Rust extensions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NativeSpec {
    /// Build hook script (relative to package root).
    pub build: String,
    /// Optional pkg-config dependency declaration.
    #[serde(default)]
    pub pkg_config: Option<String>,
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
        Self::create_init_opts(dir, name, &InitOptions::default())
    }

    /// Create an init manifest with optional enriched metadata.
    pub fn create_init_opts(dir: &Path, name: &str, opts: &InitOptions) -> Result<Self, String> {
        let manifest = Manifest {
            package: PackageSpec {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                authors: opts.authors.clone(),
                description: opts.description.clone(),
                license: opts.license.clone(),
                repository: opts.repository.clone(),
                category: None,
                keywords: Vec::new(),
            },
            dependencies: HashMap::new(),
            native: None,
        };
        let path = dir.join("zz.toml");
        manifest.save(&path)?;
        Self::ensure_gitignore(dir)?;
        Ok(manifest)
    }

    /// Entries every ZZ project gitignores: fetched deps, native build
    /// outputs, and compiled binaries. `zz.toml` and `zz.lock` are
    /// deliberately absent — both are committed (lockfile = reproducibility
    /// source of truth, same convention as Cargo).
    const GITIGNORE_ENTRIES: &[&str] = &["vendor/", "build/", "src/bin/"];

    /// Create `.gitignore` if absent, or append missing ZZ entries if
    /// present. Existing content is never removed or reordered.
    pub fn ensure_gitignore(dir: &Path) -> Result<(), String> {
        let path = dir.join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let present: std::collections::HashSet<&str> = existing.lines().map(str::trim).collect();
        let missing: Vec<&&str> = Self::GITIGNORE_ENTRIES
            .iter()
            .filter(|e| !present.contains(**e))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n# zz: fetched deps, native build outputs, compiled binaries\n");
        for entry in missing {
            out.push_str(entry);
            out.push('\n');
        }
        std::fs::write(&path, out).map_err(|e| format!("cannot write .gitignore: {e}"))?;
        Ok(())
    }

    /// Create a new project directory with manifest and starter code.
    pub fn create_new(
        parent_dir: &Path,
        name: &str,
        template: Option<&str>,
    ) -> Result<PathBuf, String> {
        Self::create_new_opts(parent_dir, name, template, &InitOptions::default())
    }

    /// Create a new project directory with enriched manifest metadata.
    pub fn create_new_opts(
        parent_dir: &Path,
        name: &str,
        template: Option<&str>,
        opts: &InitOptions,
    ) -> Result<PathBuf, String> {
        let project_dir = parent_dir.join(name);
        std::fs::create_dir_all(&project_dir)
            .map_err(|e| format!("cannot create {}: {e}", project_dir.display()))?;
        std::fs::create_dir_all(project_dir.join("src"))
            .map_err(|e| format!("cannot create src/: {e}"))?;

        // Write manifest
        Self::create_init_opts(&project_dir, name, opts)?;

        // Write starter source
        let main_content = match template {
            Some("lib") => {
                "/// Add one to a number.\npub func add_one(n: int) -> int {\n    n + 1\n}\n"
            }
            Some("web") => {
                "import std.http\n\nfunc main() {\n    s := http.server()\n    s2 := http.route_get(s, \"/\", |req| \"Hello, ZZ!\")\n    http.listen(s2, 8080) ?? println(\"failed to start server\")\n}\n"
            }
            _ => {
                "import std.http\n\nfunc main() {\n    println(\"Hello, ZZ!\")\n}\n"
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
                authors: Vec::new(),
                description: None,
                license: None,
                repository: None,
                category: None,
                keywords: Vec::new(),
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
            native: None,
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

    #[test]
    fn gitignore_created_when_absent() {
        let d = tmp_dir("gi_new");
        Manifest::ensure_gitignore(&d).unwrap();
        let content = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert!(content.contains("vendor/"));
        assert!(content.contains("build/"));
        assert!(content.contains("src/bin/"));
        assert!(!content.contains("zz.toml"));
        assert!(!content.contains("zz.lock"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn gitignore_appends_without_clobbering() {
        let d = tmp_dir("gi_append");
        fs::write(d.join(".gitignore"), "target/\nvendor/\n").unwrap();
        Manifest::ensure_gitignore(&d).unwrap();
        let content = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert!(content.contains("target/"));
        assert_eq!(content.matches("vendor/").count(), 1);
        assert!(content.contains("build/"));
        assert!(content.contains("src/bin/"));
        // Idempotent: second run changes nothing.
        Manifest::ensure_gitignore(&d).unwrap();
        let again = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert_eq!(content, again);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_init_writes_gitignore() {
        let d = tmp_dir("init_gi");
        Manifest::create_init(&d, "myapp").unwrap();
        assert!(d.join(".gitignore").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn metadata_round_trip() {
        let m = Manifest {
            package: PackageSpec {
                name: "enriched".into(),
                version: "2.3.4".into(),
                authors: vec!["Alice <alice@example.com>".into()],
                description: Some("Does things".into()),
                license: Some("MIT".into()),
                repository: Some("https://github.com/user/enriched".into()),
                category: Some("cli".into()),
                keywords: vec!["tool".into()],
            },
            dependencies: HashMap::new(),
            native: None,
        };

        let d = tmp_dir("meta_rt");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn legacy_manifest_without_metadata_parses() {
        // Manifests written before enrichment have no new keys.
        let m = Manifest::parse(
            r#"
[package]
name = "legacy"
version = "0.1.0"

[dependencies]
foo = "^1.0"
"#,
        )
        .unwrap();
        assert_eq!(m.package.name, "legacy");
        assert!(m.package.authors.is_empty());
        assert_eq!(m.package.description, None);
        assert_eq!(m.package.license, None);
        assert_eq!(m.package.repository, None);
    }

    #[test]
    fn save_omits_empty_metadata() {
        // Empty metadata stays out of the TOML so old diffs stay minimal.
        let m = Manifest::default();
        let d = tmp_dir("meta_omit");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("authors"));
        assert!(!content.contains("description"));
        assert!(!content.contains("license"));
        assert!(!content.contains("repository"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_init_opts_writes_metadata() {
        let d = tmp_dir("init_opts");
        let opts = InitOptions {
            authors: vec!["Bob".into()],
            description: Some("A tool".into()),
            license: Some("MIT".into()),
            repository: Some("https://example.com/bob/tool".into()),
        };
        let m = Manifest::create_init_opts(&d, "tool", &opts).unwrap();
        assert_eq!(m.package.authors, vec!["Bob".to_string()]);
        assert_eq!(m.package.description.as_deref(), Some("A tool"));
        let reloaded = Manifest::load(&d.join("zz.toml")).unwrap();
        assert_eq!(m, reloaded);
        let _ = fs::remove_dir_all(&d);
    }
}
