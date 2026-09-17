//! Typed cache key for build artifact invalidation.
//!
//! The cache key captures everything that affects the compiled output:
//! - Source content hash of the main file
//! - Source content hashes of all path dependencies (computed live)
//! - Build options (optimize, strip, static, etc.)
//! - Target triple
//! - Runtime/compiler mtimes (codegen + native RT + stdlib)
//!
//! **Critical invariant (Amendment 2, Round 4):** Path dependency hashes
//! are computed from live disk content at key-generation time, NOT from
//! a value stored in `zz.lock` at a prior `zz install`. This means `zz build`
//! (without an intervening `zz install`) will detect path-dep content changes
//! and produce a cache miss.

use std::collections::HashMap;
use std::path::Path;

use crate::hash;
use crate::manifest::{DepSpec, Manifest};

/// A fully-resolved cache key for a build.
///
/// Two builds with identical `CacheKey` values will produce identical output.
/// Any change to source content, dependencies, build options, or compiler
/// version will produce a different key.
#[derive(Debug, Clone)]
pub struct CacheKey {
    /// SHA-256 hash of the main source file content.
    pub source_hash: String,
    /// SHA-256 hashes of path dependencies, keyed by dependency name.
    /// Empty if no path deps exist.
    pub dep_hashes: HashMap<String, String>,
    /// Build options fingerprint (optimize, strip, static, etc.).
    pub build_fingerprint: u64,
    /// Target triple (or "host" for native builds).
    pub target: String,
    /// Sum of runtime/compiler file mtimes for invalidation.
    pub runtime_mtime: Option<u64>,
}

impl CacheKey {
    /// Compute a cache key for the given source file and build options.
    ///
    /// This reads the manifest (if present) to discover path dependencies,
    /// then hashes each one from live disk content.
    pub fn compute(
        source_path: &Path,
        source_content: &str,
        build_fingerprint: u64,
        target: Option<&str>,
        runtime_mtime: Option<u64>,
    ) -> Result<Self, String> {
        let source_hash = hash::hash_bytes(source_content.as_bytes());

        // Look for zz.toml in the source file's directory or parent
        let project_dir = source_path.parent().unwrap_or(Path::new("."));
        let dep_hashes = compute_dep_hashes(project_dir)?;

        Ok(Self {
            source_hash,
            dep_hashes,
            build_fingerprint,
            target: target.unwrap_or("host").to_string(),
            runtime_mtime,
        })
    }

    /// Compute only the source hash (no deps) — used for quick mtime checks.
    pub fn source_only_hash(source_content: &str) -> String {
        hash::hash_bytes(source_content.as_bytes())
    }

    /// Serialize to a deterministic string for use as a cache directory name.
    ///
    /// The format is: `<source_hash_16>-<deps_hash_16>-<build_fp>-<target>`
    /// where each component is truncated to 16 hex chars for readability.
    pub fn to_slug(&self) -> String {
        let deps_slug = if self.dep_hashes.is_empty() {
            "nodeps".to_string()
        } else {
            // Hash all dep hashes together for a single fingerprint
            let mut names: Vec<&String> = self.dep_hashes.keys().collect();
            names.sort(); // Deterministic order
            let mut combined = String::new();
            for name in names {
                let h = &self.dep_hashes[name];
                combined.push_str(name);
                combined.push('\0');
                combined.push_str(h);
                combined.push('\0');
            }
            let full = hash::hash_bytes(combined.as_bytes());
            full[..16.min(full.len())].to_string()
        };

        let build_hex = format!("{:016x}", self.build_fingerprint);

        format!(
            "{}-{}-{}-{}",
            &self.source_hash[..16.min(self.source_hash.len())],
            deps_slug,
            &build_hex[..16.min(build_hex.len())],
            self.target
        )
    }
}

/// Compute content hashes for all path dependencies in the project.
///
/// Looks for `zz.toml` in `project_dir`, parses it, and hashes each
/// path dependency from live disk content.
fn compute_dep_hashes(project_dir: &Path) -> Result<HashMap<String, String>, String> {
    let toml_path = project_dir.join("zz.toml");
    if !toml_path.exists() {
        return Ok(HashMap::new());
    }

    let manifest = Manifest::load(&toml_path)?;
    let mut hashes = HashMap::new();

    for (name, spec) in &manifest.dependencies {
        if let DepSpec::Path(path_dep) = spec {
            let dep_path = project_dir.join(&path_dep.path);
            if dep_path.exists() {
                let hash_opts = hash::HashOptions::default();
                let content_hash = hash::hash_dir(&dep_path, &hash_opts)?;
                hashes.insert(name.clone(), content_hash);
            }
        }
    }

    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "zz_pm_cache_key_test_{}_{}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cache_key_deterministic() {
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        let k1 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        let k2 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        assert_eq!(k1.source_hash, k2.source_hash);
        assert_eq!(k1.to_slug(), k2.to_slug());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_key_differs_on_source_change() {
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content1 = fs::read_to_string(&src).unwrap();

        fs::write(&src, "func main() { x := 1 }").unwrap();
        let content2 = fs::read_to_string(&src).unwrap();

        let k1 = CacheKey::compute(&src, &content1, 42, None, None).unwrap();
        let k2 = CacheKey::compute(&src, &content2, 42, None, None).unwrap();
        assert_ne!(k1.source_hash, k2.source_hash);
        assert_ne!(k1.to_slug(), k2.to_slug());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_key_differs_on_build_options() {
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        let k1 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        let k2 = CacheKey::compute(&src, &content, 99, None, None).unwrap();
        assert_eq!(k1.source_hash, k2.source_hash); // Same source
        assert_ne!(k1.build_fingerprint, k2.build_fingerprint);
        assert_ne!(k1.to_slug(), k2.to_slug());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_key_includes_path_deps() {
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        // Create a path dependency
        let dep_dir = d.join("dep_a");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(dep_dir.join("lib.zz"), "pub func helper() -> int { 42 }").unwrap();

        // Create zz.toml with path dep
        fs::write(
            d.join("zz.toml"),
            r#"
[package]
name = "test_project"
version = "0.1.0"

[dependencies]
dep_a = { path = "dep_a" }
"#,
        )
        .unwrap();

        let k = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        assert!(!k.dep_hashes.is_empty());
        assert!(k.dep_hashes.contains_key("dep_a"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn path_dep_change_produces_different_cache_key() {
        // This is the critical Amendment 2 test: modifying a path dependency's
        // content must produce a different CacheKey even without zz install.
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        // Create a path dependency
        let dep_dir = d.join("dep_a");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(dep_dir.join("lib.zz"), "pub func helper() -> int { 42 }").unwrap();

        // Create zz.toml
        fs::write(
            d.join("zz.toml"),
            r#"
[package]
name = "test_project"
version = "0.1.0"

[dependencies]
dep_a = { path = "dep_a" }
"#,
        )
        .unwrap();

        // Compute cache key with original dep content
        let k1 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        let deps_hash_1 = k1.dep_hashes.get("dep_a").cloned();

        // Modify the path dep content WITHOUT running zz install
        fs::write(dep_dir.join("lib.zz"), "pub func helper() -> int { 99 }").unwrap();

        // Compute cache key again — must detect the change
        let k2 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        let deps_hash_2 = k2.dep_hashes.get("dep_a").cloned();

        assert_ne!(
            deps_hash_1, deps_hash_2,
            "path dep content changed but cache key hash did not update"
        );
        assert_ne!(
            k1.to_slug(),
            k2.to_slug(),
            "full cache slug did not change after path dep edit"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn no_path_deps_empty_dep_hashes() {
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        // No zz.toml — should have empty dep_hashes
        let k = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        assert!(k.dep_hashes.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn slug_format() {
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        let k =
            CacheKey::compute(&src, &content, 42, Some("aarch64-unknown-linux-gnu"), None).unwrap();
        let slug = k.to_slug();
        // Should contain target triple
        assert!(slug.contains("aarch64-unknown-linux-gnu"));
        // Should have 4 dash-separated parts
        let parts: Vec<&str> = slug.split('-').collect();
        assert!(parts.len() >= 4);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_zz_toml_errors_not_silent() {
        // Amendment 2, Round 4: a corrupt zz.toml must NOT silently degrade
        // to a key that ignores path-dep content. The build must fail loudly.
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        // Write a corrupt zz.toml (invalid TOML syntax)
        fs::write(d.join("zz.toml"), "this is not valid [[[ toml {{{").unwrap();

        let result = CacheKey::compute(&src, &content, 42, None, None);
        assert!(
            result.is_err(),
            "corrupt zz.toml must produce an error, not a silent fallback key"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("invalid zz.toml") || err.contains("toml"),
            "error message should mention zz.toml parse failure, got: {err}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_zz_toml_ok_no_deps() {
        // No zz.toml at all is fine — just means no path deps to hash.
        // This is NOT an error case (unlike a corrupt zz.toml).
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        // No zz.toml exists
        let result = CacheKey::compute(&src, &content, 42, None, None);
        assert!(
            result.is_ok(),
            "missing zz.toml should be OK, got: {result:?}"
        );
        let key = result.unwrap();
        assert!(key.dep_hashes.is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
