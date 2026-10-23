//! Dependency resolver for `zz pm`.
//!
//! Resolves `[dependencies]` from `zz.toml` into concrete versions/sources.
//! Git + path deps always work offline. Plain-version (registry) deps need
//! a [`ResolveOptions`] with a remote source; without one they error with
//! [`ResolveError::RegistryUnsupported`] (legacy behavior, kept so plain
//! `resolve()` never touches the network).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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
    /// Transitive requirements disagree on one package (#420): two parents
    /// need incompatible versions/sources of the same dependency.
    VersionConflict { name: String, detail: String },
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
            Self::VersionConflict { name, detail } => {
                write!(
                    f,
                    "version conflict for `{name}`: {detail}\n\
                     hint: add `{name}` to zz.toml to pin one version explicitly, \
                     or align the parents' requirements"
                )
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
    /// Human-readable source decisions (#266): entries where the manifest
    /// disagreed with a stale lock pin and the manifest won.
    pub notes: Vec<String>,
}

/// Source kind of one dependency, manifest side or lock side (#266).
/// Fine-grained URLs/versions do not matter here — only which provider
/// owns the name, since that decides where the bytes come from.
fn spec_source_kind(spec: &DepSpec) -> &'static str {
    match spec {
        DepSpec::Version(_) => "registry",
        DepSpec::Git(_) => "git",
        DepSpec::Path(_) => "path",
    }
}

/// Source kind of a lock entry. Unknown/legacy tags report as `"other"`
/// (never equal to a manifest kind, so they always re-resolve).
fn lock_source_kind(source: &str) -> &'static str {
    if source == "path" {
        "path"
    } else if source.starts_with("registry+") {
        "registry"
    } else if source.starts_with("git+") {
        "git"
    } else {
        "other"
    }
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
    let mut notes = Vec::new();

    // Phase 2: Resolve each dependency
    for (name, spec) in &manifest.dependencies {
        // #266 precedence rule: the manifest declaration always wins. A lock
        // pin from another source (e.g. registry, left over from before the
        // manifest flipped to a path dep) is stale by definition — record
        // the decision and resolve from the manifest, never from the pin.
        if let Some(pinned) = lockfile.and_then(|l| l.find(name)) {
            let want = spec_source_kind(spec);
            let have = lock_source_kind(&pinned.source);
            if want != have {
                notes.push(format!(
                    "{name}: manifest declares {want}, ignoring stale {have} pin"
                ));
            }
        }
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

    // Phase 3: transitive closure (#420) — every dependency's own
    // `zz.toml` contributes its requirements, so downstream projects get
    // the full tree (fetch + link) instead of only the direct layer.
    resolve_transitive(
        manifest,
        lockfile,
        project_dir,
        opts,
        &mut locked,
        &mut path_deps,
        &mut path_hashes,
        &mut notes,
    )?;

    Ok(ResolvedDeps {
        locked,
        path_deps,
        path_hashes,
        notes,
    })
}

// TRANSITIVE CLOSURE (#420)
// ---------------------------------------------------------------------------
// Direct dependencies' own `zz.toml` files contribute requirements, so the
// tree resolves fully: `faker → rand` lands in `zz.lock` + `vendor/` and
// `import rand` works without a manual `zz add rand`.
//
// Design notes:
//
// * Direct declarations always win: a transitive requirement on a name the
//   root manifest declares is checked (registry pins must satisfy the
//   parent req, surfaced as a `note`) but never re-resolved. This keeps
//   pre-#420 workarounds (manually added transitive deps) building.
// * Version selection is a fixpoint: manifests depend on chosen versions,
//   so the walk repeats (bounded) until every collected requirement is
//   satisfied by the choice. Genuine disagreements surface as
//   `ResolveError::VersionConflict` naming the parents — never silent.
// * Offline contract preserved: without a registry remote (plain
//   `resolve()`), subtrees whose content is unavailable are skipped and
//   prior lock pins carried over with a `note`, instead of failing or
//   silently dropping transitive entries.

/// One parent's requirement on a transitive package.
#[derive(Debug, Clone)]
struct ParentReq {
    parent: String,
    spec: DepSpec,
}

/// Resolve the transitive closure over direct dependencies' manifests.
///
/// Appends transitive pins to `locked` (sorted by name for a stable
/// lockfile), transitive path content hashes to `path_hashes`, and human
/// notes to `notes`.
#[allow(clippy::too_many_arguments)]
fn resolve_transitive(
    manifest: &Manifest,
    lockfile: Option<&Lockfile>,
    project_dir: &Path,
    opts: &ResolveOptions,
    locked: &mut Vec<LockedDep>,
    path_deps: &mut Vec<PathDep>,
    path_hashes: &mut HashMap<String, String>,
    notes: &mut Vec<String>,
) -> Result<(), ResolveError> {
    if manifest.dependencies.is_empty() {
        return Ok(());
    }
    let direct: HashSet<String> = manifest.dependencies.keys().cloned().collect();
    // Current version choices, seeded with the direct pins.
    let mut choices: HashMap<String, LockedDep> =
        locked.iter().map(|l| (l.name.clone(), l.clone())).collect();
    // Canonical content dirs, filled during the walk (path entries need
    // their parent dir to resolve `rel` against).
    let mut dirs: HashMap<String, PathBuf> = HashMap::new();
    for (name, spec) in &manifest.dependencies {
        if let DepSpec::Path(p) = spec {
            let dir = project_dir.join(&p.path);
            let canon = std::fs::canonicalize(&dir).map_err(|_| ResolveError::PathNotFound {
                name: name.clone(),
                path: p.path.clone(),
            })?;
            dirs.insert(name.clone(), canon);
        }
    }
    // Registry metadata cache across fixpoint passes (one fetch per name).
    let mut meta_cache: HashMap<String, crate::remote::PackageInfo> = HashMap::new();

    // Fixpoint: manifests depend on chosen versions, so the walk repeats
    // (bounded) until every collected requirement is satisfied. Notes are
    // generated from the final pass only (per-pass notes would duplicate).
    let mut final_reqs: HashMap<String, Vec<ParentReq>> = HashMap::new();
    let mut stabilized = false;
    for _ in 0..10 {
        let mut reqs: HashMap<String, Vec<ParentReq>> = HashMap::new();
        let mut edges: HashMap<String, Vec<String>> = HashMap::new();
        walk_closure(
            opts,
            lockfile,
            &direct,
            &mut choices,
            &mut dirs,
            &mut meta_cache,
            &mut reqs,
            &mut edges,
        )?;
        check_closure_cycles(&edges)?;
        let mut changed = false;
        // Deterministic order so lock output doesn't shuffle.
        let mut names: Vec<&String> = reqs.keys().collect();
        names.sort();
        for name in names {
            let rs = &reqs[name];
            if direct.contains(name) {
                continue; // direct wins; audited from the final pass
            }
            if choice_satisfies(choices.get(name), rs, &dirs) {
                continue;
            }
            let pinned = resolve_transitive_dep(name, rs, lockfile, opts, &dirs, &mut meta_cache)?;
            // Record the content dir so later passes can read the pin's
            // own manifest (grandchildren): path targets verified during
            // reconcile, registry CAS just staged, git CAS fetched now.
            match kind_of_reqs(rs) {
                DepSpec::Path(_) => {
                    if let Ok(Some(d)) = transitive_path_target(rs, &dirs) {
                        dirs.insert(name.clone(), d);
                    }
                }
                DepSpec::Version(_) => {
                    let dir = crate::paths::cas_entry(&pinned.hash);
                    if dir.exists() {
                        dirs.insert(name.clone(), dir);
                    }
                }
                DepSpec::Git(_) => {
                    if let Some(commit) = &pinned.commit {
                        let dir = crate::paths::cas_entry(commit);
                        if dir.exists() {
                            dirs.insert(name.clone(), dir);
                        } else if matches!(opts.registry, RegistrySource::Remote(_)) {
                            let url = pinned
                                .source
                                .strip_prefix("git+")
                                .and_then(|s| s.split('#').next())
                                .unwrap_or_default();
                            if let Ok(d) = crate::git::fetch_to_cas(url, commit) {
                                dirs.insert(name.clone(), d);
                            }
                        }
                    }
                }
            }
            choices.insert(name.clone(), pinned);
            changed = true;
        }
        if !changed {
            final_reqs = reqs;
            stabilized = true;
            break;
        }
    }
    if !stabilized {
        return Err(ResolveError::VersionConflict {
            name: "<closure>".to_string(),
            detail: "transitive requirements did not stabilize after 10 passes".to_string(),
        });
    }
    let reqs = final_reqs;
    // Audit direct-vs-transitive decisions from the stable pass (per-pass
    // notes would duplicate across fixpoint iterations).
    {
        let mut names: Vec<&String> = reqs.keys().collect();
        names.sort();
        for name in names {
            if !direct.contains(name) {
                continue;
            }
            let rs = &reqs[name];
            // Path direct deps: warn when a parent points elsewhere
            // (imports inside that parent still resolve to the direct
            // target — the manifest always wins).
            if let Ok(Some(want)) = transitive_path_target(rs, &dirs) {
                if dirs.get(name).is_some_and(|d| d != &want) {
                    let via = rs
                        .iter()
                        .map(|r| format!("`{}`", r.parent))
                        .collect::<Vec<_>>()
                        .join(", ");
                    notes.push(format!(
                        "`{name}`: direct path wins over transitive requirement via {via}"
                    ));
                }
            }
            note_direct_violation(name, rs, &choices, notes, &dirs);
        }
    }
    // Stable: materialize transitive choices (direct pins already in
    // `locked`).
    {
        let mut new_names: Vec<&String> = choices.keys().filter(|n| !direct.contains(*n)).collect();
        new_names.sort();
        for name in new_names {
            let dep = choices[name].clone();
            if dep.source == "path" {
                // Path pins live in `path_hashes` (the caller writes
                // their lock entries from there, exactly like direct
                // path deps) — never in `locked`, to avoid duplicates.
                if let Some(dir) = dirs.get(name) {
                    let content_hash = hash::hash_dir(dir, &hash::HashOptions::default())
                        .map_err(ResolveError::Io)?;
                    path_hashes.insert(name.clone(), content_hash);
                    // `path_deps` is informational (see struct docs);
                    // transitive entries share it with direct ones.
                    if let Some(spec) = transitive_path_spec(name, &reqs) {
                        path_deps.push(spec);
                    }
                }
                // The audit note still applies to path entries.
                if let Some(via) = transitive_via(name, &reqs) {
                    notes.push(format!("transitive `{name}` {} via {via}", dep.version));
                }
                continue;
            }
            // One line per transitive dep keeps installs auditable
            // (mirrors the #266 manifest-wins notes).
            if let Some(via) = transitive_via(name, &reqs) {
                notes.push(format!("transitive `{name}` {} via {via}", dep.version));
            }
            locked.push(dep);
        }
    }
    carry_incomplete_pins(lockfile, &direct, &choices, locked, path_hashes, notes);
    Ok(())
}

/// The spec kind shared by a requirement set (callers guarantee non-mixed
/// sets; mixed sets are a [`ResolveError::VersionConflict`]).
fn kind_of_reqs(rs: &[ParentReq]) -> DepSpec {
    rs[0].spec.clone()
}

/// BFS over package manifests using current version choices for content
/// dirs. Collects per-name requirements and parent→child edges. Picks no
/// versions itself except recording first encounters (deferred to the
/// fixpoint reconcile); missing offline content marks `incomplete` instead
/// of failing so warm offline resolves keep working.
#[allow(clippy::too_many_arguments)]
fn walk_closure(
    opts: &ResolveOptions,
    lockfile: Option<&Lockfile>,
    direct: &HashSet<String>,
    choices: &mut HashMap<String, LockedDep>,
    dirs: &mut HashMap<String, PathBuf>,
    meta_cache: &mut HashMap<String, crate::remote::PackageInfo>,
    reqs: &mut HashMap<String, Vec<ParentReq>>,
    edges: &mut HashMap<String, Vec<String>>,
) -> Result<(), ResolveError> {
    let can_fetch = matches!(opts.registry, RegistrySource::Remote(_));
    // Seed content dirs for registry/git pins whose CAS is warm.
    // (Path dirs resolve against parent dirs, handled separately.)
    for dep in choices.values() {
        if dep.source == "path" || dirs.contains_key(&dep.name) {
            continue;
        }
        if let Some(dir) = choice_content_dir(dep, can_fetch, opts) {
            dirs.insert(dep.name.clone(), dir);
        }
    }
    let mut visited: HashSet<String> = HashSet::new();
    // Seed the queue with every direct dep that has a known dir.
    let mut queue: Vec<(String, PathBuf)> = Vec::new();
    let mut direct_names: Vec<String> = direct.iter().cloned().collect();
    direct_names.sort();
    for name in &direct_names {
        if let Some(dir) = dirs.get(name) {
            queue.push((name.clone(), dir.clone()));
        }
    }
    // Root edges drive cycle detection for direct self-references.
    edges.insert("ROOT".to_string(), direct_names);
    while let Some((pkg, dir)) = queue.pop() {
        if !visited.insert(pkg.clone()) {
            continue;
        }
        let Some(child_manifest) = load_child_manifest(&dir, &pkg)? else {
            continue;
        };
        let mut child_names: Vec<&String> = child_manifest.dependencies.keys().collect();
        child_names.sort();
        for child in child_names {
            let spec = child_manifest.dependencies[child].clone();
            edges.entry(pkg.clone()).or_default().push(child.clone());
            reqs.entry(child.clone()).or_default().push(ParentReq {
                parent: pkg.clone(),
                spec: spec.clone(),
            });
            if visited.contains(child) {
                continue;
            }
            if direct.contains(child) {
                continue;
            }
            if choices.contains_key(child) {
                // Known pin without (yet) known content: grandchildren
                // stay invisible this pass; the reconcile/fetch below (or
                // pin carryover) resolves it.
                if let Some(d) = dirs.get(child).cloned() {
                    queue.push((child.clone(), d));
                }
                continue;
            }
            // First encounter: place the requirement so grandchildren are
            // visible this pass (union reconciliation happens after).
            let (dep, dir) = place_transitive_dep(
                child,
                std::slice::from_ref(&ParentReq {
                    parent: pkg.clone(),
                    spec: spec.clone(),
                }),
                lockfile,
                opts,
                dirs,
                meta_cache,
            )?;
            if let Some(d) = dir {
                dirs.insert(child.clone(), d);
            }
            choices.insert(child.clone(), dep);
            if let Some(d) = dirs.get(child).cloned() {
                queue.push((child.clone(), d));
            }
        }
    }
    Ok(())
}

/// Content dir for an already-chosen registry/git pin. Returns `None`
/// when the CAS entry is missing and no fetch is possible (offline) —
/// the caller carries prior pins instead of failing.
fn choice_content_dir(dep: &LockedDep, can_fetch: bool, opts: &ResolveOptions) -> Option<PathBuf> {
    if dep.source == "path" {
        return None; // path dirs resolve against parent dirs, not the lock
    }
    if let Some(commit) = &dep.commit {
        let dir = crate::paths::cas_entry(commit);
        if dir.exists() {
            return Some(dir);
        }
        if !can_fetch {
            return None;
        }
        let url = dep.source.strip_prefix("git+")?.split('#').next()?;
        return crate::git::fetch_to_cas(url, commit).ok();
    }
    if dep.hash.is_empty() {
        return None;
    }
    let dir = crate::paths::cas_entry(&dep.hash);
    if dir.exists() {
        return Some(dir);
    }
    if !can_fetch {
        return None;
    }
    let (base, name, version) = crate::remote::parse_registry_source(&dep.source)?;
    let RegistrySource::Remote(_) = &opts.registry else {
        return None;
    };
    crate::remote::RegistryClient::new(&base)
        .fetch_to_cas(&name, &version, &dep.hash)
        .ok()
        .map(|(d, _)| d)
}

/// Load a dependency's manifest. Missing `zz.toml` means "leaf package"
/// (plain directory dep with no further requirements); unreadable or
/// invalid manifests fail with the parent attached.
fn load_child_manifest(dir: &Path, pkg: &str) -> Result<Option<Manifest>, ResolveError> {
    let toml = dir.join("zz.toml");
    if !toml.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&toml)
        .map_err(|e| ResolveError::Io(format!("cannot read {pkg}'s zz.toml: {e}")))?;
    Manifest::parse(&content)
        .map(Some)
        .map_err(|e| ResolveError::Io(format!("invalid zz.toml in dependency `{pkg}`: {e}")))
}

/// Place one transitive requirement set: lock-reuse when the pin
/// satisfies every known req, else fresh resolution. Returns the pin plus
/// its content dir (`None` dir = content unavailable offline → the walk
/// marks the subtree incomplete).
#[allow(clippy::too_many_arguments)]
fn place_transitive_dep(
    name: &str,
    rs: &[ParentReq],
    lockfile: Option<&Lockfile>,
    opts: &ResolveOptions,
    dirs: &HashMap<String, PathBuf>,
    meta_cache: &mut HashMap<String, crate::remote::PackageInfo>,
) -> Result<(LockedDep, Option<PathBuf>), ResolveError> {
    resolve_transitive_dep(name, rs, lockfile, opts, dirs, meta_cache).map(|d| {
        let dir = match kind_of_reqs(rs) {
            DepSpec::Path(_) => transitive_path_target(rs, dirs).ok().flatten(),
            _ if dep_source_is_git(&d) => {
                crate::paths::cas_entry(d.commit.as_deref().unwrap_or_default())
                    .exists()
                    .then(|| crate::paths::cas_entry(d.commit.as_deref().unwrap_or_default()))
            }
            _ => {
                let dir = crate::paths::cas_entry(&d.hash);
                dir.exists().then_some(dir)
            }
        };
        (d, dir)
    })
}

/// Whether a pin came from git (commit-bearing).
fn dep_source_is_git(dep: &LockedDep) -> bool {
    dep.commit.is_some()
}

/// Resolve one transitive requirement set to a pin (union of all parent
/// reqs). Mixed source kinds across parents are a conflict.
fn resolve_transitive_dep(
    name: &str,
    rs: &[ParentReq],
    lockfile: Option<&Lockfile>,
    opts: &ResolveOptions,
    dirs: &HashMap<String, PathBuf>,
    meta_cache: &mut HashMap<String, crate::remote::PackageInfo>,
) -> Result<LockedDep, ResolveError> {
    let kinds: HashSet<&str> = rs
        .iter()
        .map(|r| match &r.spec {
            DepSpec::Version(_) => "registry",
            DepSpec::Git(_) => "git",
            DepSpec::Path(_) => "path",
        })
        .collect();
    if kinds.len() > 1 {
        let detail = rs
            .iter()
            .map(|r| format!("`{}` wants {}", r.parent, dep_spec_desc(&r.spec)))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ResolveError::VersionConflict {
            name: name.to_string(),
            detail: format!("parents disagree on source kind: {detail}"),
        });
    }
    match &rs[0].spec {
        DepSpec::Version(_) => {
            let reqs: Vec<(String, String)> = rs
                .iter()
                .map(|r| {
                    let DepSpec::Version(v) = &r.spec else {
                        unreachable!()
                    };
                    (r.parent.clone(), v.clone())
                })
                .collect();
            // Lock-first: a usable pin satisfying every known req is
            // reused with no network (mirrors `resolve_registry_dep`).
            if let Some(lock) = lockfile {
                if let Some(pinned) = lock.find(name) {
                    if pinned.source.starts_with("registry+")
                        && !pinned.hash.is_empty()
                        && reqs
                            .iter()
                            .all(|(_, r)| version_satisfies(&pinned.version, r))
                    {
                        return Ok(pinned.clone());
                    }
                }
            }
            let RegistrySource::Remote(base) = &opts.registry else {
                let want = reqs
                    .iter()
                    .map(|(p, r)| format!("`{p}` wants `{r}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ResolveError::RegistryUnsupported {
                    name: name.to_string(),
                    version_req: format!("{want} (transitive, offline)"),
                });
            };
            let info = match meta_cache.get(name) {
                Some(info) => info.clone(),
                None => {
                    let client = crate::remote::RegistryClient::new(base);
                    let info = client.fetch_metadata(name).map_err(|e| {
                        ResolveError::RegistryFetchFailed {
                            name: name.to_string(),
                            detail: e.to_string(),
                        }
                    })?;
                    meta_cache.insert(name.to_string(), info.clone());
                    info
                }
            };
            let picked = pick_union_version(name, &info.metadata.versions, &reqs)?;
            let expected = crate::remote::expected_sha(&info, &picked);
            let client = crate::remote::RegistryClient::new(base);
            let (_dir, actual_sha) =
                client.fetch_to_cas(name, &picked, &expected).map_err(|e| {
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
                native: None,
            })
        }
        DepSpec::Git(_) => {
            let first = rs[0].spec.clone();
            let DepSpec::Git(first_git) = &first else {
                unreachable!()
            };
            for r in &rs[1..] {
                let DepSpec::Git(g) = &r.spec else {
                    unreachable!()
                };
                if g.git != first_git.git || g.rev != first_git.rev {
                    return Err(ResolveError::VersionConflict {
                        name: name.to_string(),
                        detail: format!(
                            "`{}` wants git {}#{}, `{}` wants git {}#{}",
                            rs[0].parent, first_git.git, first_git.rev, r.parent, g.git, g.rev
                        ),
                    });
                }
            }
            // Lock-first mirrors `resolve_git_dep` (#266 source check).
            if let Some(lock) = lockfile {
                if let Some(pinned) = lock.find(name) {
                    if pinned.source == format!("git+{}#{}", first_git.git, first_git.rev)
                        && pinned.version == first_git.version
                    {
                        return Ok(pinned.clone());
                    }
                }
            }
            let commit = crate::git::resolve_rev(&first_git.git, &first_git.rev)?;
            Ok(LockedDep {
                name: name.to_string(),
                version: first_git.version.clone(),
                source: format!("git+{}#{}", first_git.git, first_git.rev),
                hash: String::new(),
                commit: Some(commit),
                native: None,
            })
        }
        DepSpec::Path(_) => {
            // Validates the requirement set (unknown parents → Io,
            // disagreeing parents → conflict); the dir itself is
            // (re)derived at materialize time.
            match transitive_path_target(rs, dirs) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(ResolveError::Io(format!(
                        "cannot place path dependency `{name}`: parent content unavailable"
                    )));
                }
                Err(detail) => {
                    return Err(ResolveError::VersionConflict {
                        name: name.to_string(),
                        detail,
                    });
                }
            }
            Ok(LockedDep {
                name: name.to_string(),
                version: "*".to_string(),
                source: "path".to_string(),
                // Hash filled at materialize time (walk end); the pin
                // only needs identity here.
                hash: String::new(),
                commit: None,
                native: None,
            })
        }
    }
}

/// Canonical target dir for an all-path requirement set. All parents must
/// agree on the location, else it is a conflict (`Err`). Returns `None`
/// when a parent dir is unknown (offline-incomplete walk).
fn transitive_path_target(
    rs: &[ParentReq],
    dirs: &HashMap<String, PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let mut canon: Option<PathBuf> = None;
    let mut first_parent = String::new();
    for r in rs {
        let DepSpec::Path(p) = &r.spec else {
            return Ok(None);
        };
        let Some(parent_dir) = dirs.get(&r.parent) else {
            return Ok(None);
        };
        let target = std::fs::canonicalize(parent_dir.join(&p.path)).ok();
        let Some(target) = target else {
            return Ok(None);
        };
        match &canon {
            Some(c) if c != &target => {
                return Err(format!(
                    "parents disagree on location: `{first_parent}` and `{}` point at different dirs",
                    r.parent
                ));
            }
            Some(_) => {}
            None => {
                canon = Some(target);
                first_parent = r.parent.clone();
            }
        }
    }
    Ok(canon)
}

/// Human description of a dep spec for conflict messages.
fn dep_spec_desc(spec: &DepSpec) -> String {
    match spec {
        DepSpec::Version(v) => format!("registry `{v}`"),
        DepSpec::Git(g) => format!("git {}#{}", g.git, g.rev),
        DepSpec::Path(p) => format!("path `{}`", p.path),
    }
}

/// Pick the highest published version satisfying every requirement.
/// Disagreement surfaces as [`ResolveError::VersionConflict`] with each
/// parent's requirement attached.
fn pick_union_version(
    name: &str,
    versions: &[String],
    reqs: &[(String, String)],
) -> Result<String, ResolveError> {
    let mut ranges = Vec::new();
    for (parent, req) in reqs {
        let normalized = if semver::Version::parse(req.trim()).is_ok() {
            format!("={}", req.trim())
        } else {
            req.trim().to_string()
        };
        let range =
            semver::VersionReq::parse(&normalized).map_err(|_| ResolveError::VersionConflict {
                name: name.to_string(),
                detail: format!("`{parent}` declares an unparsable requirement `{req}`"),
            })?;
        ranges.push((parent.clone(), range));
    }
    let mut parsed: Vec<semver::Version> = versions
        .iter()
        .filter_map(|v| semver::Version::parse(v).ok())
        .collect();
    parsed.sort();
    parsed
        .iter()
        .rev()
        .find(|v| ranges.iter().all(|(_, r)| r.matches(v)))
        .map(|v| v.to_string())
        .ok_or_else(|| {
            let want = reqs
                .iter()
                .map(|(p, r)| format!("`{p}` wants `{r}`"))
                .collect::<Vec<_>>()
                .join(", ");
            ResolveError::VersionConflict {
                name: name.to_string(),
                detail: format!("no published version satisfies all of: {want}"),
            }
        })
}

/// Whether the current choice satisfies every collected requirement.
/// Path requirements compare canonical dirs (needs `dirs`).
fn choice_satisfies(
    choice: Option<&LockedDep>,
    rs: &[ParentReq],
    dirs: &HashMap<String, PathBuf>,
) -> bool {
    let Some(choice) = choice else {
        return false;
    };
    match &rs[0].spec {
        DepSpec::Version(_) => {
            if !choice.source.starts_with("registry+") || choice.hash.is_empty() {
                return false;
            }
            rs.iter().all(|r| {
                let DepSpec::Version(v) = &r.spec else {
                    return false;
                };
                version_satisfies(&choice.version, v)
            })
        }
        DepSpec::Git(_) => rs.iter().all(|r| {
            let DepSpec::Git(g) = &r.spec else {
                return false;
            };
            choice.source == format!("git+{}#{}", g.git, g.rev) && choice.version == g.version
        }),
        DepSpec::Path(_) => {
            let Some(choice_dir) = dirs.get(&choice.name) else {
                return false;
            };
            let Ok(Some(want)) = transitive_path_target(rs, dirs) else {
                return false;
            };
            choice_dir == &want
        }
    }
}

/// Direct declarations win over transitive requirements (#420 back-compat:
/// manually added transitive deps keep building). Warn when the direct pin
/// violates a parent requirement so the drift is visible.
fn note_direct_violation(
    name: &str,
    rs: &[ParentReq],
    choices: &HashMap<String, LockedDep>,
    notes: &mut Vec<String>,
    dirs: &HashMap<String, PathBuf>,
) {
    let Some(choice) = choices.get(name) else {
        return;
    };
    if choice_satisfies(Some(choice), rs, dirs) {
        return;
    }
    let want = rs
        .iter()
        .map(|r| format!("`{}` wants {}", r.parent, dep_spec_desc(&r.spec)))
        .collect::<Vec<_>>()
        .join(", ");
    notes.push(format!(
        "`{name}`: direct declaration wins over transitive requirement ({want})"
    ));
}

/// First parent chain for the install notes (`via `faker`` / `via a, b`).
fn transitive_via(name: &str, reqs: &HashMap<String, Vec<ParentReq>>) -> Option<String> {
    let rs = reqs.get(name)?;
    let mut parents: Vec<&str> = rs.iter().map(|r| r.parent.as_str()).collect();
    parents.sort();
    parents.dedup();
    let shown: Vec<String> = parents.iter().map(|p| format!("`{p}`")).collect();
    Some(shown.join(", "))
}

/// A representative path spec for informational `path_deps`.
fn transitive_path_spec(name: &str, reqs: &HashMap<String, Vec<ParentReq>>) -> Option<PathDep> {
    let rs = reqs.get(name)?;
    for r in rs {
        if let DepSpec::Path(p) = &r.spec {
            return Some(p.clone());
        }
    }
    None
}

/// Carry prior transitive pins for subtrees skipped offline: re-insert
/// old lock entries (non-direct names) absent from the fresh choices, so
/// warm offline resolves never shrink the lock.
fn carry_incomplete_pins(
    lockfile: Option<&Lockfile>,
    direct: &HashSet<String>,
    choices: &HashMap<String, LockedDep>,
    locked: &mut Vec<LockedDep>,
    path_hashes: &mut HashMap<String, String>,
    notes: &mut Vec<String>,
) {
    let Some(lock) = lockfile else {
        return;
    };
    let mut carried = 0;
    for dep in &lock.deps {
        if direct.contains(&dep.name) || choices.contains_key(&dep.name) {
            continue;
        }
        // Path pins travel via `path_hashes` (their lock entries are
        // written from there); every other kind joins `locked`.
        if dep.source == "path" {
            if !dep.hash.is_empty() {
                path_hashes.insert(dep.name.clone(), dep.hash.clone());
                carried += 1;
            }
            continue;
        }
        locked.push(dep.clone());
        carried += 1;
    }
    if carried > 0 {
        notes.push(format!(
            "{carried} transitive pin(s) carried over (content unavailable offline)"
        ));
    }
}

/// DFS cycle check over the full closure graph (ROOT → direct, plus every
/// parent→child edge). Reports the dependency chain.
fn check_closure_cycles(edges: &HashMap<String, Vec<String>>) -> Result<(), ResolveError> {
    let mut color: HashMap<String, u8> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();
    // Deterministic traversal.
    let mut nodes: Vec<&String> = edges.keys().collect();
    nodes.sort();
    for node in nodes {
        if !color.contains_key(node.as_str()) {
            dfs_visit(node, edges, &mut color, &mut stack)?;
        }
    }
    Ok(())
}

/// Recursive DFS helper for [`check_closure_cycles`].
fn dfs_visit(
    node: &str,
    edges: &HashMap<String, Vec<String>>,
    color: &mut HashMap<String, u8>,
    stack: &mut Vec<String>,
) -> Result<(), ResolveError> {
    color.insert(node.to_string(), 1);
    stack.push(node.to_string());
    if let Some(children) = edges.get(node) {
        let mut sorted = children.clone();
        sorted.sort();
        sorted.dedup();
        for child in sorted {
            if child == "ROOT" {
                continue;
            }
            match color.get(&child).copied().unwrap_or(0) {
                0 => dfs_visit(&child, edges, color, stack)?,
                1 => {
                    let pos = stack.iter().position(|n| n == &child).unwrap_or(0);
                    let mut chain: Vec<String> = stack[pos..].to_vec();
                    chain.push(child.clone());
                    // Hide the synthetic ROOT from user-facing chains.
                    let chain: Vec<String> = chain.into_iter().filter(|n| n != "ROOT").collect();
                    return Err(ResolveError::CircularDependency(chain));
                }
                _ => {}
            }
        }
    }
    stack.pop();
    color.insert(node.to_string(), 2);
    Ok(())
}

/// Offline closure walk for link/valid/stale checks (no network, no fetch):
/// every package name in the tree mapped to its content dir. Missing
/// content is an error — callers treat it as stale/unlinked and re-resolve.
pub fn closure(
    manifest: &Manifest,
    lockfile: &Lockfile,
    project_dir: &Path,
) -> Result<HashMap<String, PathBuf>, ResolveError> {
    let mut dirs: HashMap<String, PathBuf> = HashMap::new();
    // Direct layer first.
    for (name, spec) in &manifest.dependencies {
        match spec {
            DepSpec::Path(p) => {
                let dir = project_dir.join(&p.path);
                dirs.insert(
                    name.clone(),
                    std::fs::canonicalize(&dir).map_err(|_| ResolveError::PathNotFound {
                        name: name.clone(),
                        path: p.path.clone(),
                    })?,
                );
            }
            _ => {
                let locked = lockfile.find(name).ok_or_else(|| {
                    ResolveError::Io(format!("`{name}` is declared but not locked"))
                })?;
                dirs.insert(name.clone(), locked_content_dir(name, locked)?);
            }
        }
    }
    // Then transitive layers (BFS over manifests; cycle-guarded).
    let mut queue: Vec<String> = manifest.dependencies.keys().cloned().collect();
    let mut seen: HashSet<String> = queue.iter().cloned().collect();
    while let Some(pkg) = queue.pop() {
        let dir = dirs[&pkg].clone();
        let Some(child_manifest) = load_child_manifest(&dir, &pkg)? else {
            continue;
        };
        let mut child_names: Vec<&String> = child_manifest.dependencies.keys().collect();
        child_names.sort();
        for child in child_names {
            if seen.contains(child) {
                continue;
            }
            seen.insert(child.clone());
            match &child_manifest.dependencies[child] {
                DepSpec::Path(p) => {
                    let target = dir.join(&p.path);
                    dirs.insert(
                        child.clone(),
                        std::fs::canonicalize(&target).map_err(|_| ResolveError::PathNotFound {
                            name: child.clone(),
                            path: format!("{} (via `{pkg}`)", p.path),
                        })?,
                    );
                    queue.push(child.clone());
                }
                _ => {
                    // Transitive registry/git pins live in the lockfile.
                    if manifest.dependencies.contains_key(child) {
                        continue; // direct entry owns this name
                    }
                    let locked = lockfile.find(child).ok_or_else(|| {
                        ResolveError::Io(format!(
                            "`{child}` is required transitively via `{pkg}` but not locked"
                        ))
                    })?;
                    dirs.insert(child.clone(), locked_content_dir(child, locked)?);
                    queue.push(child.clone());
                }
            }
        }
    }
    Ok(dirs)
}

/// Content dir for a locked non-path pin. Missing CAS entries are errors
/// (callers re-resolve, which refetches).
fn locked_content_dir(name: &str, locked: &LockedDep) -> Result<PathBuf, ResolveError> {
    if locked.source == "path" {
        return Err(ResolveError::Io(format!(
            "`{name}` is a path pin without a recorded location"
        )));
    }
    let dir = if let Some(commit) = &locked.commit {
        crate::paths::cas_entry(commit)
    } else if locked.hash.is_empty() {
        return Err(ResolveError::Io(format!(
            "`{name}` is locked without content (empty hash)"
        )));
    } else {
        crate::paths::cas_entry(&locked.hash)
    };
    if dir.exists() {
        Ok(dir)
    } else {
        Err(ResolveError::Io(format!(
            "`{name}` is locked but its content is not fetched"
        )))
    }
}

/// Whether the transitive requirement closure still matches the lockfile:
/// requirement sets per package plus the absence of orphaned pins. Used by
/// `zz install`'s up-to-date short-circuit — upstream manifest changes
/// (new/removed transitive reqs) must bust the cache even when the root
/// `zz.toml` is untouched.
pub fn transitive_reqs_stale(
    manifest: &Manifest,
    lockfile: &Lockfile,
    project_dir: &Path,
) -> Result<bool, ResolveError> {
    let dirs = match closure(manifest, lockfile, project_dir) {
        Ok(d) => d,
        Err(_) => return Ok(true),
    };
    // Every closure package must resolve requirements against the lock.
    let mut queue: Vec<String> = manifest.dependencies.keys().cloned().collect();
    let mut seen: HashSet<String> = HashSet::new();
    // (name -> parents) for union checks.
    let mut reqs: HashMap<String, Vec<ParentReq>> = HashMap::new();
    while let Some(pkg) = queue.pop() {
        if !seen.insert(pkg.clone()) {
            continue;
        }
        let dir = match dirs.get(&pkg) {
            Some(d) => d.clone(),
            None => return Ok(true),
        };
        let Some(child_manifest) = load_child_manifest(&dir, &pkg)? else {
            continue;
        };
        for (child, spec) in &child_manifest.dependencies {
            reqs.entry(child.clone()).or_default().push(ParentReq {
                parent: pkg.clone(),
                spec: spec.clone(),
            });
            queue.push(child.clone());
        }
    }
    for (name, rs) in &reqs {
        if manifest.dependencies.contains_key(name) {
            continue; // direct pins own these names
        }
        let Some(pinned) = lockfile.find(name) else {
            return Ok(true); // new transitive requirement
        };
        // Compare against the recomputed canonical dirs for path reqs.
        let dir_view: HashMap<String, PathBuf> = dirs.clone();
        if !choice_satisfies(Some(pinned), rs, &dir_view) {
            return Ok(true);
        }
    }
    // Orphaned pins (lock entries outside the closure) also bust the
    // cache so removals prune `vendor/` + lock on the next install.
    let mut closure_names: HashSet<String> = seen;
    for name in manifest.dependencies.keys() {
        closure_names.insert(name.clone());
    }
    for dep in &lockfile.deps {
        if !closure_names.contains(&dep.name) {
            return Ok(true);
        }
    }
    Ok(false)
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
            // AND the pin came from git (#266: a registry pin with an equal
            // version string must not satisfy a git declaration — the
            // manifest source always wins).
            if pinned.source.starts_with("git+") && pinned.version == git_dep.version {
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
        native: None,
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
        native: None,
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

/// Detect circular dependencies in the direct dependency map.
///
/// This is a shallow pre-check (self-references); full-graph cycles
/// including transitive edges are caught by [`check_closure_cycles`]
/// after the closure walk, where manifests are available.
fn detect_cycles(deps: &HashMap<String, DepSpec>) -> Result<(), ResolveError> {
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

    // Direct-map cycles need manifest edges to detect; the shallow map
    // here only guards re-entry. Full detection (direct + transitive)
    // runs in `check_closure_cycles` once manifests are walked.

    in_stack.remove(node);
    Ok(())
}

/// Check if path deps have changed content since last lock (Amendment 3).
///
/// Covers transitive path dependencies too (#420): every path pin in the
/// lock (direct or transitive) is re-hashed against its closure dir.
/// Unresolvable closures count as stale (the re-resolve reports why).
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

    // Transitive path pins: re-hash each against its closure dir. A
    // closure that no longer resolves is itself stale (re-resolve
    // surfaces the reason — moved dirs, dropped pins, GC'd CAS).
    let dirs = match closure(manifest, lock, project_dir) {
        Ok(d) => d,
        Err(_) => return Ok(true),
    };
    for dep in &lock.deps {
        if dep.source != "path" || manifest.dependencies.contains_key(&dep.name) {
            continue; // direct layer handled above; non-path pins elsewhere
        }
        let Some(dir) = dirs.get(&dep.name) else {
            return Ok(true); // transitive pin vanished from the closure
        };
        let current_hash =
            hash::hash_dir(dir, &hash::HashOptions::default()).map_err(ResolveError::Io)?;
        if dep.hash != current_hash {
            return Ok(true);
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
        fs::write(
            dep_dir.join("zz.toml"),
            "[package]\nname = \"dep_a\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

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
        fs::write(
            dep_dir.join("lib.zz"),
            "pub func hello() -> int { return 42 }",
        )
        .unwrap();

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
            native: None,
        });

        // Content unchanged → not stale
        assert!(!path_deps_stale(&manifest, Some(&lock), &d).unwrap());

        // Modify the path dep content
        fs::write(
            dep_dir.join("lib.zz"),
            "pub func hello() -> int { return 99 }",
        )
        .unwrap();

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
    fn source_kinds_classify() {
        // #266: only the provider matters for staleness, not URLs/versions.
        assert_eq!(
            spec_source_kind(&DepSpec::Version("^1.0".to_string())),
            "registry"
        );
        assert_eq!(
            spec_source_kind(&DepSpec::Git(GitDep {
                version: "1.0.0".to_string(),
                git: "https://example.com/r.git".to_string(),
                rev: "main".to_string(),
            })),
            "git"
        );
        assert_eq!(
            spec_source_kind(&DepSpec::Path(PathDep {
                path: "../x".to_string(),
            })),
            "path"
        );
        assert_eq!(lock_source_kind("path"), "path");
        assert_eq!(lock_source_kind("registry+https://r/x#1.0.0"), "registry");
        assert_eq!(lock_source_kind("git+https://r.git#main"), "git");
        assert_eq!(lock_source_kind("registry"), "other");
        assert_eq!(lock_source_kind(""), "other");
    }

    #[test]
    fn manifest_path_wins_over_stale_registry_pin() {
        // #266: manifest declares path, lock still pins registry — resolve
        // from the manifest (offline) and record the decision.
        let d = tmp();
        let dep_dir = d.join("mylib");
        fs::create_dir_all(&dep_dir).unwrap();
        fs::write(
            dep_dir.join("zz.toml"),
            "[package]\nname=\"mylib\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();

        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mylib".to_string(),
            DepSpec::Path(PathDep {
                path: "mylib".to_string(),
            }),
        );

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "1.0.0".to_string(),
            source: "registry+https://example.com/mylib#1.0.0".to_string(),
            hash: "abc123".to_string(),
            commit: None,
            native: None,
        });

        let resolved = resolve(&manifest, Some(&lock), &d).unwrap();
        assert_eq!(resolved.path_deps.len(), 1);
        assert_eq!(
            resolved.notes,
            vec!["mylib: manifest declares path, ignoring stale registry pin".to_string()]
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn git_spec_rejects_registry_pin_with_equal_version() {
        // #266: a registry pin whose version string equals the git
        // declaration must not satisfy it — resolution falls through to
        // git (which fails fast offline here, proving non-reuse).
        let d = tmp();
        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mylib".to_string(),
            DepSpec::Git(GitDep {
                version: "1.0.0".to_string(),
                git: "file:///tmp/zz-pm-nonexistent-repo".to_string(),
                rev: "main".to_string(),
            }),
        );

        let mut lock = Lockfile::new();
        lock.upsert(LockedDep {
            name: "mylib".to_string(),
            version: "1.0.0".to_string(),
            source: "registry+https://example.com/mylib#1.0.0".to_string(),
            hash: "abc123".to_string(),
            commit: None,
            native: None,
        });

        let err = resolve(&manifest, Some(&lock), &d).unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::GitFetchFailed { .. } | ResolveError::GitRevNotFound { .. }
            ),
            "stale registry pin must not satisfy git decl, got: {err}"
        );
        let _ = fs::remove_dir_all(&d);
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
            native: None,
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
        let tgz = make_test_tarball(&[(
            "zz.toml",
            "[package]\nname = \"foo\"\nversion = \"1.2.0\"\n",
        )]);
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
        // The shallow pre-check never false-positives; real cycles
        // (including self-references through manifests) surface from the
        // closure walk's `check_closure_cycles`.
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

    // -- transitive closure (#420) ------------------------------------

    /// Write a package dir with a parseable manifest (`[package]` needs
    /// name + version; the closure walk reads every manifest it reaches).
    fn write_pkg(dir: &std::path::Path, name: &str, deps_section: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("zz.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{deps_section}"),
        )
        .unwrap();
    }

    fn single_path_manifest(dep: &str, rel: &str) -> Manifest {
        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            dep.to_string(),
            DepSpec::Path(PathDep {
                path: rel.to_string(),
            }),
        );
        manifest
    }

    #[test]
    fn transitive_path_chain_resolves_and_hashes() {
        // root → mid → leaf (all path deps): the closure lands leaf in
        // `path_hashes` + `path_deps`, never in `locked` (path pins live
        // with the direct ones — the caller writes lock entries from
        // `path_hashes`, exactly like direct path deps).
        let d = tmp();
        write_pkg(
            &d.join("mid"),
            "mid",
            "[dependencies]\nleaf = \"../leaf\"\n",
        );
        // NOTE: `leaf = "../leaf"` parses as a bare version string, not a
        // path spec — path deps need `{ path = ... }` tables. Rewrite with
        // the table form below.
        write_pkg(&d.join("leaf"), "leaf", "");
        fs::write(
            d.join("mid/zz.toml"),
            "[package]\nname = \"mid\"\nversion = \"0.1.0\"\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
        )
        .unwrap();
        let manifest = single_path_manifest("mid", "mid");

        let resolved = resolve(&manifest, None, &d).unwrap();
        assert!(
            resolved.path_hashes.contains_key("mid"),
            "direct path hashed"
        );
        assert!(
            resolved.path_hashes.contains_key("leaf"),
            "transitive path hashed: {:?}",
            resolved.path_hashes.keys().collect::<Vec<_>>()
        );
        assert!(
            resolved.locked.iter().all(|l| l.name != "leaf"),
            "path pins stay out of `locked`"
        );
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("leaf") && n.contains("mid")),
            "audit note names the provider: {:?}",
            resolved.notes
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn transitive_path_cycle_errors_with_chain() {
        // root → mid → leaf → mid: the closure walk reports the chain.
        let d = tmp();
        write_pkg(
            &d.join("mid"),
            "mid",
            "[dependencies]\nleaf = { path = \"../leaf\" }\n",
        );
        write_pkg(
            &d.join("leaf"),
            "leaf",
            "[dependencies]\nmid = { path = \"../mid\" }\n",
        );
        let manifest = single_path_manifest("mid", "mid");

        let err = resolve(&manifest, None, &d).unwrap_err();
        match err {
            ResolveError::CircularDependency(chain) => {
                assert!(chain.contains(&"mid".to_string()), "chain: {chain:?}");
                assert!(chain.contains(&"leaf".to_string()), "chain: {chain:?}");
            }
            other => panic!("expected CircularDependency, got: {other}"),
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn direct_path_wins_silently_when_targets_agree() {
        // root declares both mid and leaf; mid also requires the same
        // leaf dir: direct wins, no warning, single hash.
        let d = tmp();
        write_pkg(
            &d.join("mid"),
            "mid",
            "[dependencies]\nleaf = { path = \"../leaf\" }\n",
        );
        write_pkg(&d.join("leaf"), "leaf", "");
        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mid".to_string(),
            DepSpec::Path(PathDep {
                path: "mid".to_string(),
            }),
        );
        manifest.dependencies.insert(
            "leaf".to_string(),
            DepSpec::Path(PathDep {
                path: "leaf".to_string(),
            }),
        );

        let resolved = resolve(&manifest, None, &d).unwrap();
        assert!(resolved.path_hashes.contains_key("leaf"));
        assert!(
            !resolved.notes.iter().any(|n| n.contains("wins over")),
            "agreeing targets stay silent: {:?}",
            resolved.notes
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn direct_path_divergence_warns() {
        // root's leaf/ and mid's other-leaf/ differ: direct wins with a
        // note naming the divergence.
        let d = tmp();
        write_pkg(
            &d.join("mid"),
            "mid",
            "[dependencies]\nleaf = { path = \"../other-leaf\" }\n",
        );
        write_pkg(&d.join("leaf"), "leaf", "");
        write_pkg(&d.join("other-leaf"), "other-leaf", "");
        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "mid".to_string(),
            DepSpec::Path(PathDep {
                path: "mid".to_string(),
            }),
        );
        manifest.dependencies.insert(
            "leaf".to_string(),
            DepSpec::Path(PathDep {
                path: "leaf".to_string(),
            }),
        );

        let resolved = resolve(&manifest, None, &d).unwrap();
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("`leaf`") && n.contains("wins over")),
            "divergence noted: {:?}",
            resolved.notes
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn transitive_registry_behind_path_dep_uses_canned_server() {
        let _guard = crate::paths::test_sync::lock_env();
        // mid (path) requires rand ^1.0; the canned registry serves a
        // leaf rand 1.2.0 tarball.
        let rand_toml = "[package]\nname = \"rand\"\nversion = \"1.2.0\"\n";
        let tgz = make_test_tarball(&[("zz.toml", rand_toml)]);
        let tgz_clone = tgz.clone();
        let expected_sha = crate::hash::hash_bytes(&tgz);
        let expected_clone = expected_sha.clone();
        // metadata + tarball = 2 requests.
        let (base, handle) = crate::remote::tests::serve(2, move |path| {
            if path == "/api/pkg/rand" {
                let body = format!(
                    r#"{{"metadata":{{"name":"rand","latest":"1.2.0","author":"a","versions":["1.2.0"],"version_details":{{"1.2.0":{{"sha256":"{expected_clone}","license":"MIT"}}}},"deps":{{}},"tarball_sha256":"{expected_clone}"}},"readme":""}}"#
                );
                (200, "application/json", body.into_bytes())
            } else if path == "/api/pkg/rand/1.2.0.tgz" {
                (200, "application/gzip", tgz_clone.clone())
            } else {
                (404, "application/json", br#"{"error":"nope"}"#.to_vec())
            }
        });

        let home = std::env::temp_dir().join(format!(
            "zz_pm_resolve_transitive_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&home);
        std::env::set_var("ZZ_HOME", &home);

        let d = tmp();
        write_pkg(&d.join("mid"), "mid", "[dependencies]\nrand = \"^1.0\"\n");
        let manifest = single_path_manifest("mid", "mid");
        let opts = ResolveOptions::remote(&base);
        let resolved = resolve_with(&manifest, None, &d, &opts).unwrap();
        let rand = resolved
            .locked
            .iter()
            .find(|l| l.name == "rand")
            .expect("transitive rand locked");
        assert_eq!(rand.version, "1.2.0");
        assert_eq!(rand.hash, expected_sha);
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("transitive `rand`") && n.contains("`mid`")),
            "notes: {:?}",
            resolved.notes
        );

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&home);
        handle.join().unwrap();
    }

    #[test]
    fn transitive_version_conflict_names_parents() {
        let _guard = crate::paths::test_sync::lock_env();
        // a wants foo ^1.0, b wants foo ^2.0; both 1.5.0 and 2.1.0 exist.
        // metadata + one tarball: the first-encountered req places a
        // version (downloaded for its manifest) before the union fails.
        // Either tarball may be fetched (parent visit order is by name but
        // req content differs), so both are served.
        let tgz150 = make_test_tarball(&[(
            "zz.toml",
            "[package]\nname = \"foo\"\nversion = \"1.5.0\"\n",
        )]);
        let tgz210 = make_test_tarball(&[(
            "zz.toml",
            "[package]\nname = \"foo\"\nversion = \"2.1.0\"\n",
        )]);
        let (base, handle) = crate::remote::tests::serve(2, move |path| {
            if path == "/api/pkg/foo" {
                let body = r#"{"metadata":{"name":"foo","latest":"2.1.0","author":"a","versions":["1.5.0","2.1.0"],"version_details":{"1.5.0":{"sha256":"","license":"MIT"},"2.1.0":{"sha256":"","license":"MIT"}},"deps":{},"tarball_sha256":""},"readme":""}"#.to_string();
                (200, "application/json", body.into_bytes())
            } else if path == "/api/pkg/foo/1.5.0.tgz" {
                (200, "application/gzip", tgz150.clone())
            } else if path == "/api/pkg/foo/2.1.0.tgz" {
                (200, "application/gzip", tgz210.clone())
            } else {
                (404, "application/json", br#"{"error":"nope"}"#.to_vec())
            }
        });

        let home = std::env::temp_dir().join(format!(
            "zz_pm_resolve_conflict_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&home);
        std::env::set_var("ZZ_HOME", &home);

        let d = tmp();
        write_pkg(&d.join("a"), "a", "[dependencies]\nfoo = \"^1.0\"\n");
        write_pkg(&d.join("b"), "b", "[dependencies]\nfoo = \"^2.0\"\n");
        let mut manifest = Manifest::default();
        manifest.dependencies.insert(
            "a".to_string(),
            DepSpec::Path(PathDep {
                path: "a".to_string(),
            }),
        );
        manifest.dependencies.insert(
            "b".to_string(),
            DepSpec::Path(PathDep {
                path: "b".to_string(),
            }),
        );
        let opts = ResolveOptions::remote(&base);
        let err = resolve_with(&manifest, None, &d, &opts).unwrap_err();
        match err {
            ResolveError::VersionConflict { name, detail } => {
                assert_eq!(name, "foo");
                assert!(detail.contains('`'), "detail: {detail}");
            }
            other => panic!("expected VersionConflict, got: {other}"),
        }

        std::env::remove_var("ZZ_HOME");
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&home);
        handle.join().unwrap();
    }

    #[test]
    fn transitive_reqs_stale_detects_new_requirement() {
        // root → mid → leaf, all path: build a lock the install leg
        // would write, then grow mid's manifest and expect staleness.
        let d = tmp();
        write_pkg(
            &d.join("mid"),
            "mid",
            "[dependencies]\nleaf = { path = \"../leaf\" }\n",
        );
        write_pkg(&d.join("leaf"), "leaf", "");
        write_pkg(&d.join("extra"), "extra", "");
        let manifest = single_path_manifest("mid", "mid");

        let resolved = resolve(&manifest, None, &d).unwrap();
        let mut lock = Lockfile::new();
        for (name, hash) in &resolved.path_hashes {
            lock.upsert(LockedDep {
                name: name.clone(),
                version: "*".to_string(),
                source: "path".to_string(),
                hash: hash.clone(),
                commit: None,
                native: None,
            });
        }
        assert!(!transitive_reqs_stale(&manifest, &lock, &d).unwrap());
        assert!(!path_deps_stale(&manifest, Some(&lock), &d).unwrap());

        // New transitive requirement appears upstream.
        fs::write(
            d.join("mid/zz.toml"),
            "[package]\nname = \"mid\"\nversion = \"0.1.0\"\n[dependencies]\nleaf = { path = \"../leaf\" }\nextra = { path = \"../extra\" }\n",
        )
        .unwrap();
        assert!(transitive_reqs_stale(&manifest, &lock, &d).unwrap());

        // Content change inside a transitive path dep trips path staleness.
        fs::write(
            d.join("leaf/zz.toml"),
            "[package]\nname = \"leaf\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        assert!(path_deps_stale(&manifest, Some(&lock), &d).unwrap());
        let _ = fs::remove_dir_all(&d);
    }
}
