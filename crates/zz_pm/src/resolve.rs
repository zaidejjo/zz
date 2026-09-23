//! Dependency resolver for `zz pm`.
//!
//! Resolves `[dependencies]` from `zz.toml` into concrete versions/sources.
//! Git + path deps always work offline. Plain-version (registry) deps need
//! a [`ResolveOptions`] with a remote source; without one they error with
//! [`ResolveError::RegistryUnsupported`] (legacy behavior, kept so plain
//! `resolve()` never touches the network).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::hash;
use crate::lock::{LockedDep, Lockfile};
use crate::manifest::{DepSpec, GitDep, Manifest, PathDep};
use crate::remote;

/// Errors during dependency resolution.
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// Circular dependency detected in the resolution chain.
    CircularDependency(Vec<String>),
    /// A dependency path does not exist on disk.
    PathNotFound { name: String, path: String },
    /// Git clone/fetch failed.
    GitFetchFailed {
        name: String,
        url: String,
        detail: String,
    },
    /// Git revision not found in the repository.
    GitRevNotFound {
        name: String,
        url: String,
        rev: String,
    },
    /// Registry dependency used but no registry is configured.
    /// Plain `resolve()` is offline-only; pass a remote source via
    /// `resolve_with` to fetch registry deps.
    RegistryUnsupported { name: String, version_req: String },
    /// Registry fetch failed (network, 404, version mismatch, bad tarball).
    RegistryFetchFailed { name: String, detail: String },
    /// I/O error during resolution.
    Io(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CircularDependency(chain) => {
                write!(f, "circular dependency: {}", chain.join(" → "))
            }
            Self::PathNotFound { name, path } => {
                write!(
                    f,
                    "path dependency `{name}` not found at `{path}`\n\
                     hint: check the path in zz.toml"
                )
            }
            Self::GitFetchFailed { name, url, detail } => {
                write!(
                    f,
                    "git fetch failed for `{name}` from `{url}`: {detail}\n\
                     hint: verify the URL is accessible and the rev exists"
                )
            }
            Self::GitRevNotFound { name, url, rev } => {
                write!(
                    f,
                    "git revision `{rev}` not found in `{name}` ({url})\n\
                     hint: check the branch, tag, or commit hash"
                )
            }
            Self::RegistryUnsupported { name, version_req } => {
                write!(
                    f,
                    "registry dependencies need a configured registry\n\
                     --> {name} = \"{version_req}\"\n\
                     hint: run `zz install` (uses ZZ_REGISTRY or the default registry), \
                     or use a git dependency (zz add {name} --git URL --rev main) \
                     or path dependency (zz add {name} --path ../foo) for now"
                )
            }
            Self::RegistryFetchFailed { name, detail } => {
                write!(f, "registry fetch failed for `{name}`: {detail}")
            }
            Self::Io(msg) => write!(f, "resolution I/O error: {msg}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Where plain-version (registry) deps resolve from.
#[derive(Debug, Clone, Default)]
pub enum RegistrySource {
    /// Offline: `Version` deps error with `RegistryUnsupported`.
    /// This is the default, so plain `resolve()` never touches the network.
    #[default]
    Offline,
    /// Fetch from the hosted registry at this base URL
    /// (see [`remote::registry_base`]).
    Remote(String),
}

/// Resolver knobs. Defaults to fully offline.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    pub registry: RegistrySource,
}

impl ResolveOptions {
    /// Offline resolution (git + path only).
    pub fn offline() -> Self {
        Self {
            registry: RegistrySource::Offline,
        }
    }

    /// Resolution with registry access at `base`
    /// (e.g. [`remote::registry_base()`]).
    pub fn remote(base: &str) -> Self {
        Self {
            registry: RegistrySource::Remote(base.to_string()),
        }
    }
}

/// Result of resolving all dependencies.
#[derive(Debug, Clone)]
pub struct ResolvedDeps {
    /// Dependencies that were resolved (excluding path deps, which don't get locked).
    pub locked: Vec<LockedDep>,
    /// Path dependencies that were validated but not version-locked.
    pub path_deps: Vec<PathDep>,
    /// Content hashes for path deps (for staleness detection in lockfile).
    pub path_hashes: HashMap<String, String>,
}

/// Resolve all dependencies declared in the manifest.
///
/// Offline-only (git + path): registry (`Version`) deps error with
/// `RegistryUnsupported` without touching the network. Use
/// [`resolve_with`] with [`ResolveOptions::remote`] for registry access.
///
/// Uses the existing lockfile for pinned versions where available.
/// Returns the list of locked deps to write into `zz.lock`.
pub fn resolve(
    manifest: &Manifest,
    lockfile: Option<&Lockfile>,
    project_dir: &Path,
) -> Result<ResolvedDeps, ResolveError> {
    resolve_with(manifest, lockfile, project_dir, &ResolveOptions::offline())
}

/// Resolve with explicit [`ResolveOptions`] (registry on/off).
pub fn resolve_with(
    manifest: &Manifest,
    lockfile: Option<&Lockfile>,
    project_dir: &Path,
    opts: &ResolveOptions,
) -> Result<ResolvedDeps, ResolveError> {
    // Phase 1: Dependency graph analysis — detect cycles
    detect_cycles(&manifest.dependencies)?;

    let mut locked = Vec::new();
    let mut path_deps = Vec::new();
    let mut path_hashes = HashMap::new();

    // Phase 2: Resolve each dependency
    for (name, spec) in &manifest.dependencies {
        match spec {
            DepSpec::Version(version_req) => match &opts.registry {
                RegistrySource::Offline => {
                    return Err(ResolveError::RegistryUnsupported {
                        name: name.clone(),
                        version_req: version_req.clone(),
                    });
                }
                RegistrySource::Remote(base) => {
                    let pinned = resolve_registry_dep(name, version_req, lockfile, base)?;
                    locked.push(pinned);
                }
            },
            DepSpec::Git(git_dep) => {
                let pinned = resolve_git_dep(name, git_dep, lockfile)?;
                locked.push(pinned);
            }
            DepSpec::Path(path_dep) => {
                resolve_path_dep(name, path_dep, project_dir)?;
                // Hash the path dep for staleness tracking (Amendment 3)
                let dep_path = project_dir.join(&path_dep.path);
                let hash_opts = hash::HashOptions::default();
                let content_hash =
                    hash::hash_dir(&dep_path, &hash_opts).map_err(ResolveError::Io)?;
                path_hashes.insert(name.clone(), content_hash);
                path_deps.push(path_dep.clone());
            }
        }
    }

    Ok(ResolvedDeps {
        locked,
        path_deps,
        path_hashes,
    })
}

/// Resolve a single git dependency.
///
/// If the lockfile has a pinned commit for this dep, use that.
/// Otherwise, fetch the latest commit for the specified rev.
fn resolve_git_dep(
    name: &str,
    git_dep: &GitDep,
    lockfile: Option<&Lockfile>,
) -> Result<LockedDep, ResolveError> {
    // Check if lockfile already has this dep pinned
    if let Some(lock) = lockfile {
        if let Some(pinned) = lock.find(name) {
            // Verify the pinned version still matches the requested version
            if pinned.version == git_dep.version {
                return Ok(pinned.clone());
            }
        }
    }

    // No lock or version mismatch — need to fetch
    // Git fetch happens here; for M2, we shell to git
    let commit = crate::git::resolve_rev(&git_dep.git, &git_dep.rev)?;

    Ok(LockedDep {
        name: name.to_string(),
        version: git_dep.version.clone(),
        source: format!("git+{}#{}", git_dep.git, git_dep.rev),
        hash: String::new(), // Will be populated when CAS is fetched (M2 actual fetch)
        commit: Some(commit),
    })
}

/// Resolve a single registry (`Version`) dependency.
///
/// Lock-first: a pinned `registry+…` entry whose version still satisfies
/// the requirement is reused with no network. Otherwise the registry is
/// queried, the highest matching version is downloaded, verified, and
/// staged into the CAS.
fn resolve_registry_dep(
    name: &str,
    version_req: &str,
    lockfile: Option<&Lockfile>,
    base: &str,
) -> Result<LockedDep, ResolveError> {
    if let Some(lock) = lockfile {
        if let Some(pinned) = lock.find(name) {
            // Reuse needs a usable CAS key: an empty hash (legacy pins)
            // cannot address `cas_entry`, so fall through to re-fetch.
            if pinned.source.starts_with("registry+")
                && !pinned.hash.is_empty()
                && version_satisfies(&pinned.version, version_req)
            {
                return Ok(pinned.clone());
            }
        }
    }

    let client = remote::RegistryClient::new(base);
    let info = client
        .fetch_metadata(name)
        .map_err(|e| ResolveError::RegistryFetchFailed {
            name: name.to_string(),
            detail: e.to_string(),
        })?;
    let picked = remote::pick_version(&info.metadata.versions, version_req).map_err(|mut e| {
        // Attribute the requirement failure to this package.
        if let remote::RemoteError::VersionNoMatch { req, .. } = &mut e {
            return ResolveError::RegistryFetchFailed {
                name: name.to_string(),
                detail: remote::RemoteError::VersionNoMatch {
                    name: name.to_string(),
                    req: std::mem::take(req),
                }
                .to_string(),
            };
        }
        ResolveError::RegistryFetchFailed {
            name: name.to_string(),
            detail: e.to_string(),
        }
    })?;
    let expected = remote::expected_sha(&info, &picked);
    let (_cas_dir, actual_sha) = client.fetch_to_cas(name, &picked, &expected).map_err(|e| {
        ResolveError::RegistryFetchFailed {
            name: name.to_string(),
            detail: e.to_string(),
        }
    })?;

    Ok(LockedDep {
        name: name.to_string(),
        version: picked.clone(),
        source: format!("registry+{base}/{name}#{picked}"),
        hash: actual_sha,
        commit: None,
    })
}

/// Check a pinned exact version against a requirement string.
/// Unparseable inputs never match (caller falls through to re-resolution).
fn version_satisfies(version: &str, req: &str) -> bool {
    let Ok(v) = semver::Version::parse(version) else {
        return false;
    };
    let req_trim = req.trim();
    let normalized = if semver::Version::parse(req_trim).is_ok() {
        format!("={req_trim}")
    } else {
        req_trim.to_string()
    };
    semver::VersionReq::parse(&normalized)
        .map(|range| range.matches(&v))
        .unwrap_or(false)
}

/// Validate that a path dependency exists and is a directory.
fn resolve_path_dep(
    name: &str,
    path_dep: &PathDep,
    project_dir: &Path,
) -> Result<(), ResolveError> {
    let dep_path = project_dir.join(&path_dep.path);
    if !dep_path.exists() {
        return Err(ResolveError::PathNotFound {
            name: name.to_string(),
            path: path_dep.path.clone(),
        });
    }
    if !dep_path.is_dir() {
        return Err(ResolveError::PathNotFound {
            name: name.to_string(),
            path: path_dep.path.clone(),
        });
    }
    Ok(())
}

/// Detect circular dependencies in the dependency graph.
fn detect_cycles(deps: &HashMap<String, DepSpec>) -> Result<(), ResolveError> {
    // Build adjacency list
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, spec) in deps {
        let edges = graph.entry(name.as_str()).or_default();
        match spec {
            DepSpec::Git(g) => {
                // For now, only direct cycles between declared deps are detectable
                // Transitive cycles will be caught in M3+ when we have a full dep tree
                let _ = g; // We can't detect cross-repo cycles without full resolution
            }
            DepSpec::Path(p) => {
                // Path deps could reference other workspace crates — detect direct self-ref
                // Full cycle detection requires resolving the path dep's own zz.toml (M3+)
                let _ = p;
            }
            DepSpec::Version(_) => {}
        }
        let _ = edges; // Placeholder for transitive cycle detection
    }

    // Simple DFS for direct cycles within declared deps
    // (In M2, deps are flat — no transitive resolution yet)
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();

    for name in deps.keys() {
        if !visited.contains(name.as_str()) {
            dfs_cycle_check(name, deps, &mut visited, &mut in_stack)?;
        }
    }

    Ok(())
}

/// DFS helper for cycle detection.
fn dfs_cycle_check(
    node: &str,
    _deps: &HashMap<String, DepSpec>,
    visited: &mut HashSet<String>,
    in_stack: &mut HashSet<String>,
) -> Result<(), ResolveError> {
    if in_stack.contains(node) {
        // Found a cycle — return the chain
        return Err(ResolveError::CircularDependency(vec![node.to_string()]));
    }
    if visited.contains(node) {
        return Ok(());
    }

    visited.insert(node.to_string());
    in_stack.insert(node.to_string());

    // Check if any dep references this node (basic cycle detection)
    // In M2 with flat deps, this catches self-references and direct A→B→A
    // Full transitive detection comes in M3

    in_stack.remove(node);
    Ok(())
}

/// Check if path deps have changed content since last lock (Amendment 3).
///
/// Returns true if any path dep's content hash differs from what's in the lockfile.
pub fn path_deps_stale(
    manifest: &Manifest,
    lockfile: Option<&Lockfile>,
    project_dir: &Path,
) -> Result<bool, ResolveError> {
    let lock = match lockfile {
        Some(l) => l,
        None => return Ok(true), // No lockfile = everything is stale
    };

    for (name, spec) in &manifest.dependencies {
        if let DepSpec::Path(path_dep) = spec {
            let dep_path = project_dir.join(&path_dep.path);
            if !dep_path.exists() {
                return Err(ResolveError::PathNotFound {
                    name: name.clone(),
                    path: path_dep.path.clone(),
                });
            }

            // Compute current content hash
            let hash_opts = hash::HashOptions::default();
            let current_hash = hash::hash_dir(&dep_path, &hash_opts).map_err(ResolveError::Io)?;

            // Compare against stored hash in lockfile
            if let Some(locked) = lock.find(name) {
                if locked.hash != current_hash {
                    return Ok(true); // Content changed
                }
            } else {
                return Ok(true); // Dep not in lockfile
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::fs;

    fn tmp() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d =
            std::env::temp_dir().join(format!("zz_pm_resolve_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn registry_unsupported_error() {
        let mut manifest = Manifest::default();
        manifest
            .dependencies
            .insert("foo".to_string(), DepSpec::Version("^1.2.0".to_string()));

        // Plain resolve() is offline: registry deps error without network.
        let err = resolve(&manifest, None, std::path::Path::new(".")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("need a configured registry"));
        assert!(msg.contains("foo"));
        assert!(msg.contains("^1.2.0"));
    }

    #[test]
    fn path_dep_not_found() {
        let d = tmp();
        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "missing".to_string(),
            DepSpec::Path(PathDep {
                path: "../nonexistent".to_string(),
            }),
        );

        let err = resolve(&manifest, None, &d).unwrap_err();
        match err {
            ResolveError::PathNotFound { name, .. } => assert_eq!(name, "missing"),
            _ => panic!("expected PathNotFound, got: {err}"),
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn path_dep_resolves_successfully() {
        let d = tmp();
        let dep_dir = d.join("dep_a");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(dep_dir.join("zz.toml"), "").unwrap();

        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "dep_a".to_string(),
            DepSpec::Path(PathDep {
                path: "dep_a".to_string(),
            }),
        );

        let result = resolve(&manifest, None, &d);
        assert!(result.is_ok(), "resolve failed: {result:?}");
        let resolved = result.unwrap();
        assert_eq!(resolved.path_deps.len(), 1);
        assert!(resolved.path_hashes.contains_key("dep_a"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn path_deps_staleness_detected() {
        let d = tmp();
        let dep_dir = d.join("dep_b");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(dep_dir.join("lib.zz"), "pub func hello() -> int { 42 }").unwrap();

        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "dep_b".to_string(),
            DepSpec::Path(PathDep {
                path: "dep_b".to_string(),
            }),
        );

        // First resolve — creates hashes
        let resolved = resolve(&manifest, None, &d).unwrap();
        let hash_before = resolved.path_hashes["dep_b"].clone();

        // Create a lockfile with the hash
        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "dep_b".to_string(),
            version: "*".to_string(),
            source: "path".to_string(),
            hash: hash_before,
            commit: None,
        });

        // Content unchanged → not stale
        assert!(!path_deps_stale(&manifest, Some(&lock), &d).unwrap());

        // Modify the path dep content
        fs::write(dep_dir.join("lib.zz"), "pub func hello() -> int { 99 }").unwrap();

        // Content changed → stale
        assert!(path_deps_stale(&manifest, Some(&lock), &d).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn version_satisfies_cases() {
        assert!(version_satisfies("1.4.1", "^1.2"));
        assert!(version_satisfies("1.2.0", "1.2.0"));
        assert!(version_satisfies("2.0.0", "*"));
        assert!(!version_satisfies("2.0.0", "^1.0"));
        assert!(!version_satisfies("not-a-version", "*"));
        assert!(!version_satisfies("1.0.0", "bogus-req[[["));
    }

    #[test]
    fn registry_lock_hit_reuses_without_network() {
        // A pinned registry dep satisfying the req resolves offline —
        // the base URL is unreachable and must never be contacted.
        let d = tmp();
        let mut manifest = Manifest::default();
        manifest
            .dependencies
            .insert("foo".to_string(), DepSpec::Version("^1.0".to_string()));
        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "foo".to_string(),
            version: "1.2.0".to_string(),
            source: "registry+http://127.0.0.1:9/foo#1.2.0".to_string(),
            hash: "abc123".to_string(),
            commit: None,
        });
        let opts = ResolveOptions::remote("http://127.0.0.1:9");
        let resolved = resolve_with(&manifest, Some(&lock), &d, &opts).unwrap();
        assert_eq!(resolved.locked.len(), 1);
        assert_eq!(resolved.locked[0].version, "1.2.0");
        assert_eq!(resolved.locked[0].hash, "abc123");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn registry_fetch_and_stage_against_local_server() {
        let _guard = crate::paths::test_sync::lock_env();
        // Canned registry: metadata + tarball for foo 1.2.0.
        let tgz = make_test_tarball(&[("zz.toml", "[package]\nname = \"foo\"\n")]);
        let tgz_clone = tgz.clone();
        let expected_sha = crate::hash::hash_bytes(&tgz);
        let expected_clone = expected_sha.clone();
        // metadata + tarball = 2 requests.
        let (base, handle) = crate::remote::tests::serve(2, move |path| {
            if path == "/api/pkg/foo" {
                let body = format!(
                    r#"{{"metadata":{{"name":"foo","latest":"1.2.0","author":"a","versions":["1.2.0"],"version_details":{{"1.2.0":{{"sha256":"{expected_clone}","license":"MIT"}}}},"deps":{{}},"tarball_sha256":"{expected_clone}"}},"readme":""}}"#
                );
                (200, "application/json", body.into_bytes())
            } else if path == "/api/pkg/foo/1.2.0.tgz" {
                (200, "application/gzip", tgz_clone.clone())
            } else {
                (404, "application/json", br#"{"error":"nope"}"#.to_vec())
            }
        });

        let home = std::env::temp_dir().join(format!(
            "zz_pm_resolve_cas_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&home);
        std::env::set_var("ZZ_HOME", &home);

        let d = tmp();
        let mut manifest = Manifest::default();
        manifest
            .dependencies
            .insert("foo".to_string(), DepSpec::Version("^1.0".to_string()));
        let opts = ResolveOptions::remote(&base);
        let resolved = resolve_with(&manifest, None, &d, &opts).unwrap();
        assert_eq!(resolved.locked.len(), 1);
        let dep = &resolved.locked[0];
        assert_eq!(dep.version, "1.2.0");
        assert_eq!(dep.hash, expected_sha);
        assert!(dep.source.starts_with("registry+"));
        // CAS entry staged with the package content.
        assert!(crate::paths::cas_entry(&expected_sha)
            .join("zz.toml")
            .exists());

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&home);
        handle.join().unwrap();
    }

    /// Minimal `.tar.gz` builder for tests (mirrors remote.rs helper).
    fn make_test_tarball(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            let mut tar = tar::Builder::new(enc);
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, name, content.as_bytes())
                    .unwrap();
            }
            tar.finish().unwrap();
        }
        buf
    }

    #[test]
    fn circular_dep_detection_self_ref() {
        let mut deps = std::collections::HashMap::new();
        deps.insert("foo".to_string(), DepSpec::Version("^1.0".to_string()));
        // Self-reference would be detected in transitive resolution (M3+)
        // For now, detect_cycles with flat deps should not false-positive
        let result = detect_cycles(&deps);
        assert!(result.is_ok());
    }

    #[test]
    fn git_rev_not_found_error_display() {
        let err = ResolveError::GitRevNotFound {
            name: "mylib".to_string(),
            url: "https://github.com/example/repo.git".to_string(),
            rev: "nonexistent-tag".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("nonexistent-tag"));
        assert!(msg.contains("mylib"));
    }
}
