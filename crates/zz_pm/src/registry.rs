//! Local package-name registry: personal aliases, not a server.
//!
//! `~/.zz/registry.toml` maps a package name to a local source — either a
//! filesystem path or a git URL. `zz add <name>` (no `--path`/`--git` flag)
//! consults this file before failing, so teams can share short names via
//! dotfiles without any hosted infrastructure.
//!
//! A real hosted registry (publishing, version resolution, auth) remains
//! explicitly out of scope — see the original zz pm spec. This file is a
//! local alias list, nothing more.
//!
//! Format:
//!
//! ```toml
//! [packages.zimg]
//! path = "/home/user/projects/zimg"
//!
//! [packages.foo]
//! git = "https://example.com/foo.git"
//! rev = "main"      # optional, defaults to "main"
//! version = "*"     # optional, defaults to "*"
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use crate::manifest::{DepSpec, GitDep, PathDep};

/// One registry alias: exactly one of `path` / `git` must be set.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct RegistryEntry {
    /// Local filesystem path to the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Git URL of the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    /// Git revision (branch, tag, commit). Defaults to `"main"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Version constraint for git deps. Defaults to `"*"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl RegistryEntry {
    /// Convert to a manifest dependency spec. Returns `None` when the
    /// entry names no usable source (neither `path` nor `git`).
    pub fn to_dep_spec(&self) -> Option<DepSpec> {
        if let Some(path) = &self.path {
            return Some(DepSpec::Path(PathDep { path: path.clone() }));
        }
        if let Some(git) = &self.git {
            return Some(DepSpec::Git(GitDep {
                version: self.version.clone().unwrap_or_else(|| "*".to_string()),
                git: git.clone(),
                rev: self.rev.clone().unwrap_or_else(|| "main".to_string()),
            }));
        }
        None
    }
}

/// The local registry file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct Registry {
    /// Package name → alias entry. Empty when the file does not exist.
    #[serde(default)]
    pub packages: HashMap<String, RegistryEntry>,
}

impl Registry {
    /// Path to the registry file: `~/.zz/registry.toml` ( honors `ZZ_HOME`).
    pub fn path() -> PathBuf {
        crate::paths::zz_home().join("registry.toml")
    }

    /// Load the registry. A missing file yields an empty registry; a
    /// corrupt file is an error (never silently ignored).
    pub fn load() -> Result<Self, String> {
        Self::load_from(&Self::path())
    }

    /// Load from an explicit path (same semantics as [`Registry::load`]).
    pub fn load_from(path: &std::path::Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&content).map_err(|e| format!("invalid {}: {e}", path.display()))
    }

    /// Persist the registry, creating `~/.zz/` as needed.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }

    /// Persist to an explicit path, creating parents as needed.
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("cannot encode registry: {e}"))?;
        std::fs::write(path, content)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(())
    }

    /// Resolve a package name to a dependency spec via its alias entry.
    /// Returns `None` for unknown names or entries with no usable source.
    pub fn resolve(&self, name: &str) -> Option<DepSpec> {
        self.packages.get(name)?.to_dep_spec()
    }

    /// Insert or replace an alias.
    pub fn add(&mut self, name: String, entry: RegistryEntry) {
        self.packages.insert(name, entry);
    }

    /// Remove an alias. Returns true when one existed.
    pub fn remove(&mut self, name: &str) -> bool {
        self.packages.remove(name).is_some()
    }

    /// Alias names in sorted order (deterministic `list` output).
    pub fn names(&self) -> Vec<&String> {
        let mut names: Vec<&String> = self.packages.keys().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("zz_registry_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("registry.toml")
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_file_loads_empty() {
        let f = tmp_file("missing");
        let reg = Registry::load_from(&f).unwrap();
        assert!(reg.packages.is_empty());
        assert!(reg.resolve("anything").is_none());
        cleanup(&f);
    }

    #[test]
    fn path_entry_round_trips_and_resolves() {
        let f = tmp_file("path");
        let mut reg = Registry::load_from(&f).unwrap();
        reg.add(
            "zimg".to_string(),
            RegistryEntry {
                path: Some("/home/user/projects/zimg".to_string()),
                ..Default::default()
            },
        );
        reg.save_to(&f).unwrap();

        let reloaded = Registry::load_from(&f).unwrap();
        let spec = reloaded.resolve("zimg").expect("must resolve");
        assert!(matches!(spec, DepSpec::Path(_)));
        cleanup(&f);
    }

    #[test]
    fn git_entry_defaults_rev_and_version() {
        let f = tmp_file("git");
        let mut reg = Registry::load_from(&f).unwrap();
        reg.add(
            "foo".to_string(),
            RegistryEntry {
                git: Some("https://example.com/foo.git".to_string()),
                ..Default::default()
            },
        );
        reg.save_to(&f).unwrap();

        let reloaded = Registry::load_from(&f).unwrap();
        match reloaded.resolve("foo").expect("must resolve") {
            DepSpec::Git(g) => {
                assert_eq!(g.rev, "main");
                assert_eq!(g.version, "*");
            }
            other => panic!("expected git spec, got {other:?}"),
        }
        cleanup(&f);
    }

    #[test]
    fn remove_and_empty_source_entry() {
        let f = tmp_file("remove");
        let mut reg = Registry::load_from(&f).unwrap();
        reg.add("a".to_string(), RegistryEntry::default());
        assert!(reg.resolve("a").is_none());
        reg.add(
            "b".to_string(),
            RegistryEntry {
                path: Some("/x".to_string()),
                ..Default::default()
            },
        );
        assert!(reg.remove("b"));
        assert!(!reg.remove("b"));
        assert_eq!(reg.names(), vec![&"a".to_string()]);
        cleanup(&f);
    }

    #[test]
    fn corrupt_file_errors_loudly() {
        let f = tmp_file("corrupt");
        std::fs::write(&f, "this is not [[[ valid").unwrap();
        assert!(Registry::load_from(&f).is_err());
        cleanup(&f);
    }
}
