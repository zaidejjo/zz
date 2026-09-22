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
    /// Content hash of linked plugin binaries (loose `.o` plus original
    /// `.a` archives — see [`artifact_sig`]). Content, not mtime: build
    /// hooks re-copy artifacts on every run, so mtimes churn even when
    /// bytes are identical and would perma-bust the cache. Empty for
    /// plugin-free projects (stable slug).
    pub artifact_hash: String,
    /// Content hash of plugin flags files (`ldflags.txt`/`cflags.txt`).
    /// Hashed by content, not mtime, for the same unconditional-rewrite
    /// reason. Empty when no plugin flags exist.
    pub artifact_flags: String,
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

        // Look for zz.toml starting at the source file's directory and
        // walking up (entry files often live in `src/`, one level below
        // the project root that holds `zz.toml`). Canonicalize first so
        // relative starts walk through real ancestors. Falls back to the
        // source directory when no manifest is found.
        let start_dir = source_path.parent().unwrap_or(Path::new("."));
        let canonical = start_dir
            .canonicalize()
            .unwrap_or_else(|_| start_dir.to_path_buf());
        let mut project_dir = canonical.clone();
        let mut found = false;
        loop {
            if project_dir.join("zz.toml").exists() {
                found = true;
                break;
            }
            if !project_dir.pop() {
                break;
            }
        }
        if !found {
            project_dir = start_dir.to_path_buf();
        }
        let dep_hashes = compute_dep_hashes(&project_dir)?;

        Ok(Self {
            source_hash,
            dep_hashes,
            build_fingerprint,
            target: target.unwrap_or("host").to_string(),
            runtime_mtime,
            artifact_hash: String::new(),
            artifact_flags: String::new(),
        })
    }

    /// Compute only the source hash (no deps) — used for quick mtime checks.
    pub fn source_only_hash(source_content: &str) -> String {
        hash::hash_bytes(source_content.as_bytes())
    }

    /// Serialize to a deterministic string for use as a cache directory name.
    ///
    /// The format is: `<source_hash_16>-<deps_hash_16>-<build_fp>-<target>-<rt_16>-<art_16>-<flags_8>`
    /// where each component is truncated for readability. The trailing
    /// `rt` segment carries the runtime/compiler mtime sum (`none` when
    /// unavailable): without it, a compiler change reuses binaries built
    /// by the older compiler. The `art`/`flags` segments carry the plugin
    /// native-artifact signature (see [`artifact_sig`]): without them, a
    /// plugin `.so`/`.a` rebuild is invisible and stale binaries are
    /// served. Both cache consumers (`zz_cli::build` native cache,
    /// `BuildCache`) treat the slug as opaque, so extending it only
    /// orphans pre-fix entries (safe: cold rebuild once, old entries age
    /// out via gc).
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
        let rt_slug = match self.runtime_mtime {
            Some(m) => format!("{m:016x}"),
            None => "none".to_string(),
        };
        let art_slug = if self.artifact_hash.is_empty() {
            "none".to_string()
        } else {
            self.artifact_hash[..16.min(self.artifact_hash.len())].to_string()
        };
        let flags_slug = if self.artifact_flags.is_empty() {
            "none".to_string()
        } else {
            self.artifact_flags[..8.min(self.artifact_flags.len())].to_string()
        };

        format!(
            "{}-{}-{}-{}-{}-{}-{}",
            &self.source_hash[..16.min(self.source_hash.len())],
            deps_slug,
            &build_hex[..16.min(build_hex.len())],
            self.target,
            rt_slug,
            art_slug,
            flags_slug,
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

/// Signature over linked plugin artifacts for cache invalidation.
///
/// Returns `(artifact_hash, artifact_flags)` for [`CacheKey`]. Both are
/// content hashes (empty when nothing is present):
/// - `artifact_hash`: over the bytes of loose `.o` files plus the
///   **original** `.a` archives. Thin copies (`*.zz-thin.a`, regenerated
///   by `ar d` whenever the archive is newer) resolve back to their
///   original: `ar` member headers carry mtimes, so re-thinned bytes
///   differ even for identical content and would perma-bust. The thin
///   copy is a pure function of the archive, so the original's bytes
///   are exactly as precise for link-output identity. Only filenames
///   (not full paths) join the hash so the key stays
///   machine-independent.
/// - `artifact_flags`: over `ldflags.txt`/`cflags.txt` contents found
///   alongside the artifacts.
///
/// Content, not mtime, throughout: build hooks re-copy artifacts on
/// every consumer build, so mtimes churn while bytes are identical —
/// an mtime signal here was measured to bust on every no-change build.
/// Hashing costs ~0.3s for a 40MB archive and runs only for projects
/// that actually link plugins.
pub fn artifact_sig(artifacts: &[std::path::PathBuf]) -> (String, String) {
    // Resolve thin copies back to originals; dedupe; sort by filename
    // for a deterministic, machine-independent order.
    let mut bins: Vec<std::path::PathBuf> = Vec::new();
    for a in artifacts {
        let s = a.to_string_lossy();
        let orig = match s.strip_suffix(".zz-thin.a") {
            Some(prefix) => std::path::PathBuf::from(prefix),
            None => a.clone(),
        };
        if !bins.contains(&orig) {
            bins.push(orig);
        }
    }
    bins.sort_by(|x, y| {
        x.file_name()
            .cmp(&y.file_name())
            .then_with(|| x.to_string_lossy().cmp(&y.to_string_lossy()))
    });
    let mut combined = String::new();
    for b in &bins {
        let Ok(bytes) = std::fs::read(b) else {
            continue;
        };
        combined.push_str(
            &b.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        combined.push('\0');
        combined.push_str(&hash::hash_bytes(&bytes));
        combined.push('\0');
    }
    let bin_hash = if combined.is_empty() {
        String::new()
    } else {
        hash::hash_bytes(combined.as_bytes())
    };
    // Flags files live next to the artifacts in each plugin build dir.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for a in artifacts {
        if let Some(d) = a.parent() {
            let d = d.to_path_buf();
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs.sort();
    let mut fcombined = String::new();
    for d in &dirs {
        for name in ["ldflags.txt", "cflags.txt"] {
            if let Ok(content) = std::fs::read(d.join(name)) {
                fcombined.push_str(name);
                fcombined.push('\0');
                fcombined.push_str(&hash::hash_bytes(&content));
                fcombined.push('\0');
            }
        }
    }
    let flags = if fcombined.is_empty() {
        String::new()
    } else {
        hash::hash_bytes(fcombined.as_bytes())
    };
    (bin_hash, flags)
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
    fn cache_key_slug_differs_on_compiler_mtime() {
        // The native cache once served binaries built by an older compiler:
        // `runtime_mtime` was computed but dropped by `to_slug()`. A
        // compiler change (different mtime sum) must change the slug.
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        let k1 = CacheKey::compute(&src, &content, 42, None, Some(1000)).unwrap();
        let k2 = CacheKey::compute(&src, &content, 42, None, Some(2000)).unwrap();
        assert_eq!(k1.source_hash, k2.source_hash); // Same source
        assert_ne!(k1.to_slug(), k2.to_slug());

        // Unknown compiler state must also differ from a known one.
        let k3 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        assert_ne!(k1.to_slug(), k3.to_slug());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn artifact_sig_empty_for_no_plugins() {
        // Plugin-free projects keep a stable, artifact-free signature.
        let (h, f) = artifact_sig(&[]);
        assert!(h.is_empty());
        assert!(f.is_empty());
    }

    #[test]
    fn artifact_sig_tracks_artifacts_and_flags() {
        let d = tmp();
        let build = d.join("build");
        fs::create_dir_all(&build).unwrap();
        let a = build.join("libx.a");
        fs::write(&a, "archive-bytes").unwrap();
        fs::write(build.join("ldflags.txt"), "-lfoo").unwrap();

        let (h1, f1) = artifact_sig(std::slice::from_ref(&a));
        assert!(!h1.is_empty());
        assert!(!f1.is_empty());

        // Stable across calls when nothing changes (hits preserved —
        // hooks re-copy files every run, so mtimes would churn here).
        let (h2, f2) = artifact_sig(std::slice::from_ref(&a));
        assert_eq!((h1.clone(), f1.clone()), (h2, f2));

        // Artifact content change busts.
        fs::write(&a, "archive-bytes-v2").unwrap();
        let (h3, _) = artifact_sig(std::slice::from_ref(&a));
        assert_ne!(h1, h3);

        // Flags content change busts with identical artifacts (hooks
        // rewrite flags files unconditionally, hence content hashing).
        fs::write(build.join("ldflags.txt"), "-lbar").unwrap();
        let (_, f3) = artifact_sig(std::slice::from_ref(&a));
        assert_ne!(f1, f3);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn artifact_sig_resolves_thin_copies_to_originals() {
        // `ar d` member headers carry mtimes, so re-thinned bytes differ
        // even for identical content. The signature must read the
        // original archive, or every build perma-busts.
        let d = tmp();
        let build = d.join("build");
        fs::create_dir_all(&build).unwrap();
        let orig = build.join("libx.a");
        fs::write(&orig, "archive-bytes").unwrap();
        let thin = build.join("libx.a.zz-thin.a");
        fs::write(&thin, "thinned-different-bytes").unwrap();

        let (h_thin, _) = artifact_sig(std::slice::from_ref(&thin));
        let (h_orig, _) = artifact_sig(std::slice::from_ref(&orig));
        assert_eq!(h_thin, h_orig);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_key_slug_differs_on_artifact_rebuild() {
        // Plugin .so/.a rebuilds were invisible: dep hashes exclude
        // build/ outputs, so stale binaries were served. The artifact
        // signature must participate in the slug.
        let d = tmp();
        let src = d.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        let mut k1 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        k1.artifact_hash = "abc".to_string();
        k1.artifact_flags = "abc".to_string();
        let mut k2 = CacheKey::compute(&src, &content, 42, None, None).unwrap();
        k2.artifact_hash = "def".to_string();
        k2.artifact_flags = "abc".to_string();
        assert_ne!(k1.to_slug(), k2.to_slug());

        // Flags-only change busts too.
        k2.artifact_hash = "abc".to_string();
        k2.artifact_flags = "def".to_string();
        assert_ne!(k1.to_slug(), k2.to_slug());
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
    fn src_layout_finds_project_manifest() {
        // Entry files usually live in `src/` while `zz.toml` sits at the
        // project root: the walk-up must still hash path deps.
        let d = tmp();
        let src_dir = d.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("main.zz");
        fs::write(&src, "func main() { }").unwrap();
        let content = fs::read_to_string(&src).unwrap();

        let dep_dir = d.join("dep_a");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(dep_dir.join("lib.zz"), "pub func helper() -> int { 42 }").unwrap();

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
        assert!(
            k.dep_hashes.contains_key("dep_a"),
            "src/ layout must still hash path deps, got: {:?}",
            k.dep_hashes
        );
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
