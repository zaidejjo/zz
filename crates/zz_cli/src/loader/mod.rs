//! Import resolution and program loading (Phase 2, extended in Phase 2.5).
//!
//! Resolves `import` statements to files, parses and type-checks every
//! module in dependency order, and detects circular imports.
//!
//! Resolution rules:
//! - `import std.*` → standard library (no file; signatures are seeded).
//! - `import a.b`   → `<dir of importing file>/a/b.zz`.
//!
//! Namespacing (Phase 2.5): every module is bound to a namespace — the last
//! path component of its file stem, or an explicit alias
//! (`import math.utils as m`). The module's top-level functions and bindings
//! are registered under `ns.name`, and references to them inside the module
//! are rewritten to qualified paths. Imported modules are referenced by their
//! namespace: `utils.double(6)`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use zz_checker::{
    check_program_with_module_ns, AliasSig, CheckResult, EnumSig, FuncSig, StructSig, Type,
};
use zz_frontend::ast::{Block, Expr, ImportItem, Program, Stmt, Ty, TyKind};
use zz_frontend::diag::{error_at, RawDiag};
use zz_frontend::parse;
use zz_frontend::span::Span;
use zz_runtime::NativeEntry;
use zz_stdlib::{
    register_module_namespace, register_selective_namespace, register_wildcard_namespace,
    stdlib_funcs, stdlib_natives, STDLIB_MODULES,
};

pub(crate) mod cache;
pub(crate) mod rewrite;

#[cfg(test)]
mod tests;

/// A file that failed to parse or type-check.
#[derive(Debug)]
pub struct LoadError {
    /// Display name of the file (as given on the command line or import).
    pub name: String,
    /// Source text, for rendering diagnostics.
    pub source: String,
    pub diags: Vec<RawDiag>,
}

/// A selective stdlib import replayed for bare-name natives on scoped
/// runs: (module, [(name, alias)]). Alias for the nested vec (clippy
/// `type_complexity` would otherwise fire at every use site).
pub type StdlibSelectives = Vec<(String, Vec<(String, Option<String>)>)>;

/// A fully loaded program: all modules in dependency order (imports first,
/// the entry file last), plus the accumulated checker seed.
#[derive(Debug)]
pub struct LoadResult {
    /// Modules in execution order: dependencies before dependents.
    pub programs: Vec<Program>,
    /// Parallel to `programs`: (display name, source) for error rendering.
    pub files: Vec<(String, String)>,
    /// All functions (stdlib + modules), for seeding the checker.
    #[allow(dead_code)]
    pub funcs: HashMap<String, FuncSig>,
    /// Top-level bindings from all modules.
    #[allow(dead_code)]
    pub bindings: HashMap<String, Type>,
    /// Struct definitions from all modules.
    #[allow(dead_code)]
    pub structs: HashMap<String, StructSig>,
    /// Type alias definitions from all modules.
    #[allow(dead_code)]
    pub aliases: HashMap<String, AliasSig>,
    /// User enum definitions from all modules.
    #[allow(dead_code)]
    pub enums: HashMap<String, EnumSig>,
    /// Native implementations (stdlib + namespaced copies), for the
    /// interpreter.
    pub natives: HashMap<String, NativeEntry>,
    /// Static constant values resolved through selective imports (including
    /// aliases).  Keys are bare names like `"pi"`, values are the float.
    pub consts: HashMap<String, f64>,
    pub errors: Vec<LoadError>,
    /// `import std.X as alias` renames for stdlib modules — needed to mirror
    /// pure-ZZ Env bindings under the alias (e.g. `colors.red` → `cl.red`).
    pub stdlib_aliases: Vec<(String, String)>,
    /// Selective-import aliases: bare name → qualified `ns.sym`, collected
    /// from every `import m(x)` / `import m(x as y)` / `import m(*)` in all
    /// modules. Injected into the interpreter before execution so bare
    /// calls to *generic* functions resolve — generics have no value
    /// binding, and the VM compiler emits no code for import statements.
    /// First registration wins (mirrors the loader seed, where an earlier
    /// import shadows a later one for the same bare name).
    pub import_aliases: HashMap<String, String>,
    /// Check-pipeline counters for `zz check --stats`.
    pub stats: LoadStats,
    /// Sorted union of every stdlib module imported anywhere in the load
    /// closure (any import kind). Scoped runs execute only these modules'
    /// pure-ZZ programs plus their runtime deps (see
    /// `zz_stdlib::stdlib_program_closure`).
    pub stdlib_modules: Vec<String>,
    /// Selective imports replayed for bare-name natives on scoped runs.
    pub stdlib_selectives: StdlibSelectives,
    /// Wildcard imports replayed for bare-name natives on scoped runs.
    pub stdlib_wildcards: Vec<String>,
}

/// One discovered member of a `[collect.<name>]` registry.
#[derive(Debug, Clone)]
struct CollectionMember {
    /// Member name as used by `get("<name>")`.
    name: String,
    /// Internal module namespace (`__collect_<collection>_<member>`).
    ns: String,
    /// Canonical member file path.
    path: PathBuf,
    /// File name as written on disk, for diagnostics.
    file: String,
    /// Span of the exported function definition (0..0 when absent).
    run_span: Span,
    /// Source text, for member-local diagnostics.
    source: String,
}

/// A synthesized collection module, kept until the check pass so member
/// signatures can be validated against the real checked types before the
/// final AST is generated.
#[derive(Debug, Clone)]
struct CollectionModule {
    name: String,
    /// Namespace bound to the synthesized module (`commands`, or an alias).
    ns: String,
    /// Required exported function name (`run` by default).
    export: String,
    /// Parsed signature type (function type).
    sig_ty: Ty,
    /// Canonical source spelling of `sig` (`func([str]) -> int`).
    sig_source: String,
    members: Vec<CollectionMember>,
}

struct Loader {
    sources: HashMap<PathBuf, String>,
    programs: HashMap<PathBuf, Program>,
    order: Vec<PathBuf>,
    visiting: HashSet<PathBuf>,
    done: HashSet<PathBuf>,
    errors: Vec<LoadError>,
    /// Cross-module seed: only `pub` items from previously loaded modules.
    funcs: HashMap<String, FuncSig>,
    bindings: HashMap<String, Type>,
    structs: HashMap<String, StructSig>,
    /// Cross-module seed: `pub type` aliases from previously loaded modules.
    aliases: HashMap<String, AliasSig>,
    /// Cross-module seed: `pub enum` definitions from previously loaded modules.
    enums: HashMap<String, EnumSig>,
    /// Keys currently owned by the seed maps above (stdlib + plugin + prior
    /// modules' pubs). Lets each module's seed maps MOVE into the checker
    /// and come back via key-set restore instead of O(seed) clones per
    /// module (quadratic → linear across modules). Key maintenance is O(new
    /// pubs) total; values are never cloned on the seed path.
    seed_func_keys: HashSet<String>,
    seed_struct_keys: HashSet<String>,
    seed_alias_keys: HashSet<String>,
    seed_enum_keys: HashSet<String>,
    /// Whether the per-module check cache is active (S1 arena-scale).
    /// Only the `zz check` entry point enables it: `run`/`build` need
    /// span types and codegen inputs the cache does not store.
    use_cache: bool,
    /// Project manifests cached per project root (`None` = no `zz.toml`
    /// or unloadable manifest): backs implicit package mapping
    /// (`import app.x` → `<root>/src/x.zz`, issue #288) without
    /// re-parsing the manifest per import.
    pkg_cache: HashMap<PathBuf, Option<(String, PathBuf)>>,
    /// Synthesized collection modules, keyed by their synthetic path.
    collections: HashMap<PathBuf, CollectionModule>,
    /// Declared collections that failed manifest validation; the error was
    /// reported once up front, so a later import is skipped instead of
    /// repeating it.
    invalid_collections: HashSet<String>,
    /// Plugin function names, for the cache genesis hash.
    plugin_names: Vec<String>,
    /// File dependency edges (importer → imported canonical paths) for S1
    /// dep-aware cache keys. Recorded during discovery, which already
    /// resolves every import; key resolution follows true dependencies
    /// instead of load order.
    dep_edges: HashMap<PathBuf, Vec<PathBuf>>,
    /// Same-file namespace aliases (`(importer, requested)` → canonical):
    /// when one file is imported under several namespaces (explicit `as`
    /// aliases alongside bare or other-aliased imports, #380), the first
    /// binding wins as the canonical namespace and every other requested
    /// spelling maps to it — scoped per importing module. Importing
    /// modules rewrite references through the canonical spelling, so
    /// seeds, types, and runtime values keep a single identity instead
    /// of fracturing per alias. Populated during discovery; empty when no
    /// file is multi-bound (all substitution paths then no-op).
    ns_aliases: HashMap<(PathBuf, String), String>,
    /// Whether the seed tables have been contributed to `all_*` yet.
    /// `all_*` must contain the seeds (stdlib!) exactly once: the first
    /// succeeding module contributes them (prior modules all errored, so
    /// seeds are still just stdlib/plugins). One O(seed) clone total.
    contributed_seeds: bool,
    /// All items (pub + private) for the entry file / runtime.
    all_funcs: HashMap<String, FuncSig>,
    all_bindings: HashMap<String, Type>,
    all_structs: HashMap<String, StructSig>,
    all_aliases: HashMap<String, AliasSig>,
    all_enums: HashMap<String, EnumSig>,
    natives: HashMap<String, NativeEntry>,
    /// Namespace → canonical path of the module (or `std:<module>` for the
    /// standard library) that owns it.
    namespaces: HashMap<String, PathBuf>,
    /// Canonical path → namespace it was registered under.
    ns_of: HashMap<PathBuf, String>,
    /// Canonical path of the entry file (for main() call validation).
    entry: PathBuf,
    /// Project root of the entry file (nearest ancestor holding
    /// `zz.toml`; `None` for bare scripts). Files outside it (registry
    /// CAS, path deps) or under a `vendor/` dir are dependencies —
    /// their style warnings collapse to one note (#387).
    root: Option<PathBuf>,
    /// Selective/wildcard imports to process after all modules are loaded.
    /// Each entry: (importing_file_path, module_path, items, is_stdlib).
    selective_imports: Vec<(PathBuf, Vec<String>, Vec<ImportItem>, bool)>,
    /// Bare constant names (including aliases) to inject into the runtime env.
    /// Populated during selective import processing in finish().
    selected_consts: HashMap<String, f64>,
    /// Mirrors LoadResult::stdlib_aliases while loading.
    stdlib_aliases: Vec<(String, String)>,
    /// Every stdlib module touched by any import kind (full, selective,
    /// wildcard), across all loaded modules including vendor deps. Drives
    /// scoped pure-ZZ stdlib execution and scoped natives on run-cache
    /// hits — unimported stdlib code is unreachable by checked programs.
    stdlib_modules: std::collections::HashSet<String>,
    /// Selective imports to replay for runtime natives on scoped runs:
    /// (module, [(name, alias)]). Bare-name natives exist only through
    /// these (and wildcards); full-module registration is qualified-only.
    stdlib_selectives: StdlibSelectives,
    /// Wildcard-imported stdlib modules, replayed like selectives on hits.
    stdlib_wildcards: Vec<String>,
    /// S1 check-cache outcomes for `--stats` observability (#248).
    cache_hits: usize,
    cache_misses: usize,
}

/// Check-pipeline observability for `zz check --stats` (#248): per-load
/// module counts, cache outcomes, and seed size.
#[derive(Debug, Clone, Default)]
pub struct LoadStats {
    /// Modules in the load closure (dependencies + entry).
    pub modules: usize,
    /// Modules served from the S1 content-addressed cache.
    pub cache_hits: usize,
    /// Modules freshly checked (cache disabled, unresolvable key, or miss).
    pub cache_misses: usize,
    /// Seed function entries shared by every module (stdlib + plugins).
    pub seed_funcs: usize,
}

/// Load an entry file and all of its imports.
pub fn load_program(main_path: &Path) -> Result<LoadResult, String> {
    load_program_with_plugins(main_path, &[])
}

/// Load an entry file for `zz check`, with the per-module check cache
/// enabled (S1 arena-scale): unchanged modules skip re-checking via a
/// content-addressed on-disk cache. `run`/`build` must use `load_program`
/// (they need span types and codegen inputs the cache does not store).
pub fn load_program_check(main_path: &Path) -> Result<LoadResult, String> {
    load_program_impl(main_path, &[], true)
}

/// Load an entry file and all of its imports, merging additional function
/// signatures from plugin manifests.
///
/// `plugin_funcs` is a slice of `(package_name, FuncSig)` pairs loaded from
/// `.zzi` plugin manifests. These are merged into the checker's function table
/// alongside the stdlib, making them available for type-checking and codegen.
pub fn load_program_with_plugins(
    main_path: &Path,
    plugin_funcs: &[(String, FuncSig)],
) -> Result<LoadResult, String> {
    load_program_impl(main_path, plugin_funcs, false)
}

fn load_program_impl(
    main_path: &Path,
    plugin_funcs: &[(String, FuncSig)],
    use_cache: bool,
) -> Result<LoadResult, String> {
    let entry = cache::canon(main_path).map_err(|e| {
        format!(
            "cannot read entry file `{}`: {e}\n\
                 hint: check that the file exists and the path is correct",
            main_path.display()
        )
    })?;
    let std_funcs = stdlib_funcs();
    let seed_func_keys = std_funcs.keys().cloned().collect();
    let mut loader = Loader {
        sources: HashMap::new(),
        programs: HashMap::new(),
        order: Vec::new(),
        visiting: HashSet::new(),
        done: HashSet::new(),
        errors: Vec::new(),
        funcs: std_funcs,
        bindings: HashMap::new(),
        structs: HashMap::new(),
        aliases: HashMap::new(),
        enums: HashMap::new(),
        seed_func_keys,
        seed_struct_keys: HashSet::new(),
        seed_alias_keys: HashSet::new(),
        seed_enum_keys: HashSet::new(),
        contributed_seeds: false,
        use_cache,
        pkg_cache: HashMap::new(),
        collections: HashMap::new(),
        invalid_collections: HashSet::new(),
        plugin_names: Vec::new(),
        dep_edges: HashMap::new(),
        ns_aliases: HashMap::new(),
        all_funcs: HashMap::new(),
        all_bindings: HashMap::new(),
        all_structs: HashMap::new(),
        all_aliases: HashMap::new(),
        all_enums: HashMap::new(),
        natives: stdlib_natives(),
        namespaces: HashMap::new(),
        ns_of: HashMap::new(),
        entry: entry.clone(),
        root: find_project_root(&entry),
        selective_imports: Vec::new(),
        selected_consts: HashMap::new(),
        stdlib_aliases: Vec::new(),
        stdlib_modules: std::collections::HashSet::new(),
        stdlib_selectives: Vec::new(),
        stdlib_wildcards: Vec::new(),
        cache_hits: 0,
        cache_misses: 0,
    };
    // Merge plugin manifest function signatures into the checker's function table.
    for (name, sig) in plugin_funcs {
        loader.funcs.insert(name.clone(), sig.clone());
        loader.seed_func_keys.insert(name.clone());
        loader.plugin_names.push(name.clone());
    }
    loader.validate_declared_collections();
    loader.load_file(main_path, None)?;
    Ok(loader.finish())
}

/// Walk up from `start` (a file or directory) to the project root —
/// the nearest ancestor (or self) holding `zz.toml`.
pub(crate) fn find_project_root(start: &Path) -> Option<PathBuf> {
    // Canonicalize so relative starts (e.g. `src/main.zz` with cwd at
    // the project root) walk up through real ancestors. Both the
    // canonicalization and the walk memoize process-wide (#233): every
    // bare import otherwise re-walks per entry file.
    let canonical = cache::canon(start).unwrap_or_else(|_| start.to_path_buf());
    cache::project_root_memo(&canonical, || {
        let mut dir = if canonical.is_file() {
            canonical.parent()?.to_path_buf()
        } else {
            canonical.clone()
        };
        loop {
            if dir.join("zz.toml").exists() {
                return Some(dir);
            }
            if !dir.pop() {
                return None;
            }
        }
    })
}

/// Normalize a package name for import matching: `-` and `_` spellings
/// agree (`my-app` in `zz.toml` matches `import my_app.x`), since `-`
/// is not a valid identifier character in import paths.
fn normalize_pkg(name: &str) -> String {
    name.replace('-', "_")
}

/// Find a lock entry by package name under `-`/`_` agreement: exact
/// match first (no behavior change when spellings already agree), then
/// the normalized form (`#486`: `import my_app.x` with `my-app` locked).
fn find_locked<'a>(
    lock: &'a zz_pm::lock::Lockfile,
    name: &str,
) -> Option<&'a zz_pm::lock::LockedDep> {
    lock.deps.iter().find(|d| d.name == name).or_else(|| {
        let want = normalize_pkg(name);
        lock.deps.iter().find(|d| normalize_pkg(&d.name) == want)
    })
}

/// Find a manifest dependency under `-`/`_` agreement (same rule as
/// [`find_locked`]).
fn manifest_dep<'a>(
    manifest: &'a zz_pm::manifest::Manifest,
    name: &str,
) -> Option<&'a zz_pm::manifest::DepSpec> {
    manifest.dependencies.get(name).or_else(|| {
        let want = normalize_pkg(name);
        manifest
            .dependencies
            .iter()
            .find(|(n, _)| normalize_pkg(n) == want)
            .map(|(_, s)| s)
    })
}

/// True when `name` is a declared dependency under `-`/`_` agreement.
fn manifest_declares(manifest: &zz_pm::manifest::Manifest, name: &str) -> bool {
    manifest_dep(manifest, name).is_some()
}

/// Resolve `<pkg>.<rest...>` against a project root: `<rest...>` maps to
/// `<root>/src/<rest...>.zz` (dot-separated → path). A bare `<pkg>`
/// (empty `rest`) maps to `<root>/src/main.zz`.
fn package_target(root: &Path, rest: &[String]) -> PathBuf {
    if rest.is_empty() {
        root.join("src").join("main.zz")
    } else {
        let mut p = root.join("src");
        for seg in rest {
            p = p.join(seg);
        }
        p.with_extension("zz")
    }
}

/// Resolve a registry/git dependency's package directory for `import <dep>`.
/// Unlike [`resolve_plugin_pkg`], no `plugin.zzi` is required: plain-ZZ
/// packages resolve to their entry file (`src/main.zz`, `src/<dep>.zz`,
/// or `<dep>.zz`). Prefers the `vendor/<dep>` link when present, else the
/// CAS entry recorded in `zz.lock`. Returns the ENTRY FILE path.
fn resolve_registry_entry(project_root: &Path, dep_name: &str) -> Option<PathBuf> {
    // Manifest+lock parse once per root process-wide (#233); mtimes
    // revalidate so concurrent edits reload instead of serving stale deps.
    let (manifest, lock) = cache::manifest_lock(project_root)?;
    let locked = find_locked(&lock, dep_name)?;
    // Path deps resolve against the manifest path (same as plugins).
    // Transitive path pins (#420) have no manifest entry: resolve them
    // through the dependency-closure BFS instead.
    if locked.source == "path" {
        match manifest_dep(&manifest, dep_name) {
            Some(zz_pm::manifest::DepSpec::Path(p)) => {
                return entry_in(&project_root.join(&p.path), dep_name);
            }
            _ => {
                return resolve_transitive_path_dir(project_root, dep_name)
                    .and_then(|dir| entry_in(&dir, dep_name));
            }
        }
    }
    // Registry/git deps: the vendor link first, CAS entry as fallback.
    // The link lives under the locked (real-spelling) name, which may
    // differ from the import spelling under `-`/`_` agreement (#486);
    // entry files may also use either spelling (`src/my-app.zz` vs
    // `src/my_app.zz`), so both are tried with `src/main.zz` first.
    let entry_in_either = |dir: &Path| {
        entry_in(dir, dep_name).or_else(|| {
            (locked.name.as_str() != dep_name)
                .then(|| entry_in(dir, &locked.name))
                .flatten()
        })
    };
    for candidate in [dep_name, locked.name.as_str()] {
        let linked = project_root.join("vendor").join(candidate);
        if linked.exists() {
            if let Some(entry) = entry_in_either(&linked) {
                return Some(entry);
            }
        }
    }
    entry_in_either(&zz_pm::paths::cas_entry(&locked.hash))
}

/// Entry-file convention inside a dependency package directory.
/// Results memoize per (`pkg_dir`, `dep_name`) process-wide (#233):
/// every bare dep import otherwise re-stats the same 3 candidates.
fn entry_in(pkg_dir: &Path, dep_name: &str) -> Option<PathBuf> {
    cache::entry_in_memo(pkg_dir, dep_name, || {
        [
            pkg_dir.join("src").join("main.zz"),
            pkg_dir.join("src").join(format!("{dep_name}.zz")),
            pkg_dir.join(format!("{dep_name}.zz")),
        ]
        .into_iter()
        .find(|candidate| candidate.is_file())
    })
}

/// Submodule file inside a dependency package directory for dotted
/// imports (`import tui.screen`, #375): `<rest...>` maps to
/// `<depdir>/src/<rest...>.zz` (mirroring the self-package layout from
/// #288), with a root-level `<depdir>/<rest...>.zz` fallback. Returns the
/// file only when it positively exists — otherwise the importer falls
/// through to legacy relative resolution untouched.
fn submodule_in(pkg_dir: &Path, rest: &[String]) -> Option<PathBuf> {
    cache::submodule_in_memo(pkg_dir, rest, || {
        let mut under_src = PathBuf::from("src");
        let mut at_root = PathBuf::new();
        for seg in rest {
            under_src = under_src.join(seg);
            at_root = at_root.join(seg);
        }
        [
            pkg_dir.join(&under_src).with_extension("zz"),
            pkg_dir.join(&at_root).with_extension("zz"),
        ]
        .into_iter()
        .find(|candidate| candidate.is_file())
    })
}

/// Package directory of a registry/git/path dependency for dotted imports
/// (`import <dep>.<module>`, #375). Unlike [`resolve_registry_entry`],
/// no entry file is required (multi-module packages expose submodules
/// without a top-level entry): path deps resolve against the manifest
/// path, others prefer the `vendor/<dep>` link, then the lockfile CAS
/// entry. Plugin packages (shipping `plugin.zzi`) count too — their
/// `.zz` sources load as ordinary modules under the submodule
/// namespace (manifest signatures only merge on bare `import <dep>`).
fn resolve_dep_dir(project_root: &Path, dep_name: &str) -> Option<PathBuf> {
    // Manifest+lock parse once per root process-wide (#233), same as the
    // sibling resolvers.
    let (manifest, lock) = cache::manifest_lock(project_root)?;
    let locked = find_locked(&lock, dep_name)?;
    if locked.source == "path" {
        // Direct path deps resolve against the manifest path; transitive
        // path pins (#420) resolve through the dep-closure BFS below.
        if let Some(zz_pm::manifest::DepSpec::Path(p)) = manifest_dep(&manifest, dep_name) {
            return Some(project_root.join(&p.path));
        }
        return resolve_transitive_path_dir(project_root, dep_name);
    }
    // The vendor link lives under the locked (real-spelling) name (#486).
    for candidate in [dep_name, locked.name.as_str()] {
        let linked = project_root.join("vendor").join(candidate);
        if linked.exists() {
            return Some(linked);
        }
    }
    Some(zz_pm::paths::cas_entry(&locked.hash))
}

/// Content dir of a transitive path dependency (#420): BFS over dep
/// manifests starting from the root's direct deps. Path children resolve
/// against their parent dir; registry/git children via the root lock
/// (vendor link, then CAS). Depth-capped and canonical-dir-guarded so
/// dependency cycles terminate.
fn resolve_transitive_path_dir(project_root: &Path, dep_name: &str) -> Option<PathBuf> {
    fn cas_dir_of(lock: &zz_pm::lock::Lockfile, name: &str) -> Option<PathBuf> {
        let locked = lock.deps.iter().find(|d| d.name == name)?;
        let dir = if let Some(commit) = &locked.commit {
            zz_pm::paths::cas_entry(commit)
        } else if locked.hash.is_empty() {
            return None;
        } else {
            zz_pm::paths::cas_entry(&locked.hash)
        };
        dir.is_dir().then_some(dir)
    }
    let (manifest, lock) = cache::manifest_lock(project_root)?;
    // Seed with every direct dep's content dir.
    let mut queue: Vec<PathBuf> = Vec::new();
    for (direct, spec) in &manifest.dependencies {
        let dir = match spec {
            zz_pm::manifest::DepSpec::Path(p) => project_root.join(&p.path),
            _ => {
                let linked = project_root.join("vendor").join(direct);
                if linked.is_dir() {
                    linked
                } else {
                    match cas_dir_of(&lock, direct) {
                        Some(d) => d,
                        None => continue,
                    }
                }
            }
        };
        if dir.is_dir() {
            queue.push(dir);
        }
    }
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut depth = 0;
    while !queue.is_empty() && depth < 32 {
        depth += 1;
        let mut next: Vec<PathBuf> = Vec::new();
        for dir in queue {
            let canon = cache::canon(&dir).unwrap_or_else(|_| dir.clone());
            if !visited.insert(canon) {
                continue;
            }
            let Some(child_manifest) = cache::manifest_of(&dir) else {
                continue;
            };
            for (child, spec) in &child_manifest.dependencies {
                match spec {
                    zz_pm::manifest::DepSpec::Path(p) => {
                        let target = dir.join(&p.path);
                        if child == dep_name {
                            if target.is_dir() {
                                return Some(target);
                            }
                        } else if target.is_dir() {
                            next.push(target);
                        }
                    }
                    _ => {
                        if child == dep_name {
                            // Non-path transitive matches resolve via the
                            // lock + vendor link elsewhere, not here.
                            continue;
                        }
                        let linked = project_root.join("vendor").join(child);
                        if linked.is_dir() {
                            next.push(linked);
                        } else if let Some(cas) = cas_dir_of(&lock, child) {
                            next.push(cas);
                        }
                    }
                }
            }
        }
        queue = next;
    }
    None
}
/// Resolve a dotted dependency import (`import tui.screen`) to its
/// submodule file: `<depdir>/src/screen.zz` via [`resolve_dep_dir`] +
/// [`submodule_in`]. Every search root is tried (ancestors plus consumer
/// roots, see [`search_roots`]) so transitive imports — from inside
/// `vendor/` or a sibling path dep — resolve against the outer project
/// lock, the same discipline as bare registry imports.
fn resolve_dep_submodule(start: &Path, imp: &[String]) -> Option<(PathBuf, PathBuf)> {
    for root in search_roots(start) {
        if let Some(dir) = resolve_dep_dir(&root, &imp[0]) {
            if let Some(file) = submodule_in(&dir, &imp[1..]) {
                return Some((root, file));
            }
        }
    }
    None
}

/// Package dir holding an entry file: nearest ancestor (or self) with
/// `zz.toml`. Used to attribute a loaded dep file to its package for
/// consumer-root tracking.
fn pkg_dir_of_entry(entry: &Path) -> Option<PathBuf> {
    let canon = cache::canon(entry).unwrap_or_else(|_| entry.to_path_buf());
    let mut dir = if canon.is_file() {
        canon.parent()?.to_path_buf()
    } else {
        canon.clone()
    };
    loop {
        if dir.join("zz.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Project roots to try for an import from `start`: ancestor roots plus
/// the consumer roots of the enclosing package (#420). A sibling path
/// dep is not under its consumer on disk, so ancestors alone never reach
/// the consumer lock; the reverse edges recorded at each dep load close
/// the loop (parents load before their imports are processed).
fn search_roots(start: &Path) -> Vec<PathBuf> {
    let mut out = ancestor_roots(start);
    let mut seen: HashSet<PathBuf> = out.iter().cloned().collect();
    if let Some(pkg) = pkg_dir_of_entry(start) {
        for root in cache::dep_consumers(&pkg) {
            if seen.insert(root.clone()) {
                out.push(root);
            }
        }
    }
    out
}

/// All ancestor project roots (nearest first) for `import <dep>` lookup.
/// The nearest root of a vendored file is the dependency itself (which
/// has no lock); outer roots must also be tried for transitive deps.
fn ancestor_roots(start: &Path) -> Vec<PathBuf> {
    let canonical = cache::canon(start).unwrap_or_else(|_| start.to_path_buf());
    cache::ancestor_roots_memo(&canonical, || {
        let mut dir = if canonical.is_file() {
            match canonical.parent() {
                Some(p) => p.to_path_buf(),
                None => return Vec::new(),
            }
        } else {
            canonical.clone()
        };
        let mut roots = Vec::new();
        loop {
            if dir.join("zz.toml").exists() {
                roots.push(dir.clone());
            }
            if !dir.pop() {
                break;
            }
        }
        roots
    })
}
/// Resolve a plugin dependency's package directory for `import <dep>`.
/// Returns the package dir only when the dep exists in `zz.lock` and
/// ships a `plugin.zzi` manifest.
fn resolve_plugin_pkg(project_root: &Path, dep_name: &str) -> Option<PathBuf> {
    let (manifest, lock) = cache::manifest_lock(project_root)?;
    let locked = find_locked(&lock, dep_name)?;
    let pkg_dir = if locked.source == "path" {
        match manifest_dep(&manifest, dep_name) {
            Some(zz_pm::manifest::DepSpec::Path(p)) => project_root.join(&p.path),
            // Transitive path pins (#420) live under vendor/ like every
            // other locked dep (real-spelling link, #486).
            _ => {
                let mut found = None;
                for candidate in [dep_name, locked.name.as_str()] {
                    let linked = project_root.join("vendor").join(candidate);
                    if linked.exists() {
                        found = Some(linked);
                        break;
                    }
                }
                found?
            }
        }
    } else {
        zz_pm::paths::cas_entry(&locked.hash)
    };
    if pkg_dir.join("plugin.zzi").exists() {
        Some(pkg_dir)
    } else {
        None
    }
}

/// The namespace a module is bound to: its alias, or its file stem.
fn module_ns(alias: Option<&str>, path: &Path) -> String {
    alias.map(str::to_string).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// True when `path` is first-party source the consumer owns (#387):
/// under the entry's project root and outside any `vendor/` dir.
/// Registry (CAS), path, and vendored deps are someone else's code —
/// their style warnings are unactionable downstream. `None` root (bare
/// scripts, which cannot have deps) means everything is first-party.
fn is_first_party(root: Option<&Path>, path: &Path) -> bool {
    match root {
        None => true,
        Some(root) => match path.strip_prefix(root) {
            Ok(rel) => !rel.components().any(|c| c.as_os_str() == "vendor"),
            Err(_) => false,
        },
    }
}
/// If `msg` is `prefix``name`` (a backtick-quoted name immediately after a
/// known prefix), return `name`. Used to lift names out of checker messages
/// (`undefined variable `b.helper``) for private-item detection.
fn quoted_name_after<'a>(msg: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = msg.strip_prefix(prefix)?;
    let rest = rest.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(&rest[..end])
}

/// From `struct `T` has no field `f``, extract (`T`, `f`).
fn struct_no_field(msg: &str) -> Option<(&str, &str)> {
    let rest = msg.strip_prefix("struct `")?;
    let end = rest.find('`')?;
    let sname = &rest[..end];
    let rest = rest[end + 1..].strip_prefix(" has no field ")?;
    let rest = rest.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some((sname, &rest[..end]))
}

/// Push the standard "it's private — add `pub`" note onto a diagnostic.
fn push_private_note(d: &mut RawDiag, name: &str, template: &str) {
    let last = name.rsplit('.').next().unwrap_or(name);
    d.notes.push(template.replace("{}", last));
}

/// Resolve one module's S1 cache key over in-memory sources (no IO):
/// `H(genesis, namespace, source, sorted(dep path, dep key)...)`, memoized.
/// Returns `None` when unresolvable (missing source, cycle) — the caller
/// treats it as a cache miss and checks normally.
fn resolve_module_key(
    genesis: &str,
    sources: &HashMap<PathBuf, String>,
    edges: &HashMap<PathBuf, Vec<PathBuf>>,
    namespaces: &HashMap<PathBuf, String>,
    path: &Path,
    memo: &mut HashMap<PathBuf, String>,
    stack: &mut Vec<PathBuf>,
) -> Option<String> {
    if let Some(key) = memo.get(path) {
        return Some(key.clone());
    }
    if stack.contains(&path.to_path_buf()) {
        return None;
    }
    let source = sources.get(path)?;
    stack.push(path.to_path_buf());
    let mut deps = Vec::new();
    if let Some(edge_list) = edges.get(path) {
        for dep in edge_list {
            let dep_key =
                resolve_module_key(genesis, sources, edges, namespaces, dep, memo, stack)?;
            deps.push((dep.display().to_string(), dep_key));
        }
    }
    stack.pop();
    // #290: the importing namespace is part of the key — cached pubs are
    // stored already-namespaced, so sharing one entry across namespaces
    // restores the wrong qualifier set.
    let ns: &str = namespaces.get(path).map(String::as_str).unwrap_or("");
    let key = cache::module_key(genesis, source, &deps, ns);
    memo.insert(path.to_path_buf(), key.clone());
    Some(key)
}

/// True when `name` is usable as a ZZ identifier (and hence a C
/// symbol): leading letter/underscore, then alphanumerics/underscores.
/// Mirrors the codegen-side `c_ident_valid` for the alias gate.
pub(crate) fn is_zz_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Loader {
    /// Project name + root owning `canon` (nearest `zz.toml`), cached per
    /// root. `None` outside any project or when the manifest cannot load
    /// (callers fall back to legacy relative resolution, never hard-error).
    fn package_of(&mut self, canon: &Path) -> Option<(String, PathBuf)> {
        let root = find_project_root(canon)?;
        if let Some(cached) = self.pkg_cache.get(&root) {
            return cached.clone();
        }
        // Manifest parses once per root process-wide (#233); the
        // per-Loader pkg_cache keeps first-load ordering semantics.
        // Lockless projects (manifest, no lock) resolve too.
        let entry = cache::manifest_of(&root)
            .filter(|m| !m.package.name.is_empty())
            .map(|m| (m.package.name.clone(), root.clone()));
        self.pkg_cache.insert(root, entry.clone());
        entry
    }

    /// Namespace a dependency module's symbols live under in the seeds:
    /// its bound (first) namespace. Usually the import's own effective
    /// namespace, but a multi-bound file (#380) answers with its
    /// canonical spelling instead.
    fn seed_ns(&self, dep: &Path, fallback: String) -> String {
        self.ns_of.get(dep).cloned().unwrap_or(fallback)
    }

    /// Record a same-file namespace alias for later reference rewriting
    /// (#380): when this import requests `requested` for a file already
    /// bound under another (canonical) namespace, the importing module
    /// rewrites `requested.*` references through the canonical spelling.
    /// No-op for first bindings and repeat imports under one spelling.
    fn record_ns_alias(&mut self, importer: &Path, requested: &str, dep: &Path) {
        if let Some(canonical) = self.ns_of.get(dep).cloned() {
            if canonical != requested {
                self.ns_aliases
                    .insert((importer.to_path_buf(), requested.to_string()), canonical);
            }
        }
    }

    /// Selective-import path with the canonical seed namespace last (see
    /// [`Loader::seed_ns`]). `alias` is the import's own effective
    /// spelling (`as` alias or last segment); `dep_canon` is the resolved
    /// dependency file when known.
    fn canonical_sel_path(
        &self,
        imp: &[String],
        alias: Option<&str>,
        dep_canon: Option<&Path>,
    ) -> Vec<String> {
        let mut sel_path = imp.to_vec();
        if let Some(a) = alias {
            if let Some(last) = sel_path.last_mut() {
                *last = a.to_string();
            }
        }
        if let Some(dep) = dep_canon {
            let seed_ns = self.seed_ns(dep, sel_path.last().cloned().unwrap_or_default());
            if let Some(last) = sel_path.last_mut() {
                *last = seed_ns;
            }
        }
        sel_path
    }

    /// Record an importer → imported edge (deduped). Small vecs; linear
    /// scan is cheaper than a second map.
    fn record_edge(edges: &mut HashMap<PathBuf, Vec<PathBuf>>, importer: &Path, dep: &Path) {
        let entry = edges.entry(importer.to_path_buf()).or_default();
        if !entry.contains(&dep.to_path_buf()) {
            entry.push(dep.to_path_buf());
        }
    }

    /// Direct dependencies of `root` whose own manifests declare `name`
    /// (#420): `rand` via `faker` — present in the tree (vendor/path)
    /// but not importable without `zz add`. Reads are failure-path-only
    /// (a handful of small TOML parses on an otherwise fatal path).
    fn transitive_providers(root: &Path, name: &str) -> Vec<String> {
        let Some(manifest) = cache::manifest_of(root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (dep_name, spec) in &manifest.dependencies {
            let dir = match spec {
                zz_pm::manifest::DepSpec::Path(p) => root.join(&p.path),
                _ => root.join("vendor").join(dep_name),
            };
            if !dir.is_dir() {
                continue;
            }
            if let Some(dep_man) = cache::manifest_of(&dir) {
                if manifest_declares(&dep_man, name) {
                    out.push(dep_name.clone());
                }
            }
        }
        out.sort();
        out
    }

    /// Actionable diagnostic for a bare-name import with no resolvable
    /// file inside a project (#420). Returns `None` when neither case
    /// applies (caller keeps the relative-path error).
    fn missing_dep_hint(root: &Path, name: &str) -> Option<String> {
        let manifest = cache::manifest_of(root)?;
        // Declared but not installed (no vendor link / no lock entry).
        if manifest_declares(&manifest, name) {
            return Some(format!(
                "cannot resolve import `{name}`: declared in zz.toml but not installed\n\
                 hint: run `zz install`, then build again"
            ));
        }
        // Transitive: declared by a direct dependency's manifest.
        let providers = Self::transitive_providers(root, name);
        if providers.is_empty() {
            return None;
        }
        let via = providers
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "cannot resolve import `{name}`: not a direct dependency of this project\n\
             hint: `{name}` is available transitively via {via} — run `zz add {name}` to use it directly"
        ))
    }

    /// Parse a file and recursively load its imports (DFS post-order, so
    /// dependencies land in `order` before their dependents).
    fn load_file(&mut self, path: &Path, alias: Option<&str>) -> Result<(), String> {
        let canon = cache::canon(path).map_err(|e| {
            format!(
                "cannot read imported file `{}`: {e}\n\
                     hint: check that the file exists relative to the importing module",
                path.display()
            )
        })?;

        if self.done.contains(&canon) {
            return Ok(());
        }
        if self.visiting.contains(&canon) {
            self.errors.push(LoadError {
                name: path.display().to_string(),
                source: String::new(),
                diags: vec![error_at(
                    format!(
                        "circular import: `{}` is imported (directly or transitively) by itself",
                        path.display()
                    ),
                    Span::new(0, 0),
                )],
            });
            return Ok(());
        }

        let source = std::fs::read_to_string(&canon).map_err(|e| {
            format!(
                "cannot read imported file `{}`: {e}\n\
                      hint: check that the file exists and you have permission to read it",
                path.display()
            )
        })?;
        // Shared parse memo (#327): every entry file re-parses its whole
        // dependency closure; serve repeats from memory. Keyed by content
        // hash (edits, including `--fix` writes mid-run, miss soundly).
        // The memoized program is pre-namespace: each consumer clones it
        // and namespace-rewrites below with its own alias.
        let hash = cache::source_hash(&source);
        let program = match cache::parsed_get(&canon, &hash) {
            Some(program) => program,
            None => {
                let parsed = parse(&source);
                if !parsed.errors.is_empty() {
                    self.errors.push(LoadError {
                        name: path.display().to_string(),
                        source,
                        diags: parsed.errors,
                    });
                    return Ok(());
                }

                // Expand explicit decorators before import scanning and namespacing
                // so decorator references are ordinary call expressions when the
                // namespace rewriter runs.
                let (expanded_program, decorator_errors) =
                    zz_frontend::decorators::expand_program(&parsed.program);
                if !decorator_errors.is_empty() {
                    self.errors.push(LoadError {
                        name: path.display().to_string(),
                        source,
                        diags: decorator_errors,
                    });
                    return Ok(());
                }
                cache::parsed_insert(&canon, &hash, &expanded_program, source.len());
                expanded_program
            }
        };

        self.visiting.insert(canon.clone());

        let imports: Vec<(Vec<String>, Option<String>, Vec<ImportItem>)> = program
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Import {
                    path, alias, items, ..
                } => Some((path.clone(), alias.clone(), items.clone())),
                _ => None,
            })
            .collect();

        for (imp, imp_alias, imp_items) in imports {
            let is_selective = !imp_items.is_empty();
            if imp.first().map(String::as_str) == Some("std") {
                // Dotted module key: `std.sqlz` -> "sqlz",
                // `std.sqlz.postgres` -> "sqlz.postgres".
                if imp.len() < 2 {
                    self.errors.push(LoadError {
                        name: path.display().to_string(),
                        source: source.clone(),
                        diags: vec![error_at(
                            "`import std` names no module\n\
                             hint: import a concrete module, e.g. `import std.str`",
                            Span::new(0, 0),
                        )],
                    });
                    continue;
                }
                let module = imp[1..].join(".");
                // Removed `db` module: hard error with a rename hint.
                // Handle variables named `db` are unaffected — this fires
                // only on the import.
                if module == "db" {
                    self.errors.push(LoadError {
                        name: path.display().to_string(),
                        source: source.clone(),
                        diags: vec![error_at(
                            "no std module `db` — rename to `sqlz`",
                            Span::new(0, 0),
                        )
                        .with_note("hint: `import std.db` → `import std.sqlz`")],
                    });
                    continue;
                }
                if !STDLIB_MODULES.contains(&module.as_str()) {
                    self.errors.push(LoadError {
                        name: path.display().to_string(),
                        source: source.clone(),
                        diags: vec![error_at(
                            format!(
                                "unknown standard library module `std.{module}`\n\
                                     hint: available modules are: {}",
                                STDLIB_MODULES.to_vec().join(", ")
                            ),
                            Span::new(0, 0),
                        )],
                    });
                    continue;
                }
                if is_selective {
                    // Store for processing in finish() when all modules are loaded.
                    self.selective_imports
                        .push((canon.clone(), imp.clone(), imp_items, true));
                } else {
                    // Full module import: copy all symbols under namespace.
                    // Default namespace is the last component:
                    // `import std.sqlz.postgres` -> `postgres.*`.
                    let ns = imp_alias
                        .clone()
                        .unwrap_or_else(|| imp.last().cloned().unwrap_or_else(|| module.clone()));
                    // Same (module, ns) pair already copied by an earlier
                    // importer (#233): the std tables are immutable, so the
                    // entries are byte-identical — skip the re-copy. The
                    // namespace claim + alias/module bookkeeping below
                    // still run (idempotent for repeats, and they own
                    // collision diagnostics).
                    let std_canon = PathBuf::from(format!("std:{module}"));
                    let fresh_copy = self
                        .namespaces
                        .get(&ns)
                        .map(|owner| *owner != std_canon)
                        .unwrap_or(true);
                    if fresh_copy {
                        match register_module_namespace(
                            &module,
                            &ns,
                            &mut self.funcs,
                            &mut self.natives,
                        ) {
                            Err(msg) => {
                                self.errors.push(LoadError {
                                     name: path.display().to_string(),
                                     source: source.clone(),
                                     diags: vec![error_at(
                                         format!("{msg}\n\
                                                  hint: this may occur if the stdlib module exports a conflicting name"),
                                         Span::new(0, 0),
                                     )],
                                 });
                                continue;
                            }
                            // Seed-owned: the per-module check partition
                            // restores (rather than consumes) these keys,
                            // so every importer resolves identically (#214).
                            Ok(inserted) => self.seed_func_keys.extend(inserted),
                        }
                    }
                    self.register_std_ns(&ns, &module, path, &source);
                    self.stdlib_aliases.push((module.clone(), ns.clone()));
                    self.stdlib_modules.insert(module.clone());
                }
                continue;
            }
            // Declared collection (`import collect.commands`): synthesize
            // the registry module. Only the root package's own collections
            // are supported in v1 — a dependency importing its collection
            // gets a targeted diagnostic instead of silent misbehavior.
            if imp.first().map(String::as_str) == Some("collect") {
                let importer_root = find_project_root(&canon);
                let collection_name = imp.get(1).cloned().unwrap_or_default();
                if let Some(ir) = importer_root.as_deref() {
                    if self.root.as_deref() != Some(ir) {
                        let declares = cache::manifest_of(ir)
                            .map(|m| m.collections.contains_key(&collection_name))
                            .unwrap_or(false);
                        if declares {
                            self.errors.push(LoadError {
                                name: path.display().to_string(),
                                source: source.clone(),
                                diags: vec![error_at(
                                    format!(
                                        "collection `collect.{collection_name}` is declared in dependency `{}`, which is not supported yet\n\
                                         hint: collections are root-package-only in v1; declare `[collect.{collection_name}]` in the root package's zz.toml",
                                        ir.display()
                                    ),
                                    Span::new(0, 0),
                                )],
                            });
                            continue;
                        }
                    }
                }
                let root = self.root.clone();
                let root_manifest = root.as_deref().and_then(cache::manifest_of);
                let declared = root_manifest
                    .as_ref()
                    .and_then(|m| m.collections.get(&collection_name))
                    .cloned();
                if let Some(spec) = declared {
                    if imp.len() != 2 {
                        self.errors.push(LoadError {
                            name: path.display().to_string(),
                            source: source.clone(),
                            diags: vec![error_at(
                                format!(
                                    "`collect.{collection_name}` takes no submodules\n\
                                     hint: v1 collections are single-level; import `collect.{collection_name}`"
                                ),
                                Span::new(0, 0),
                            )],
                        });
                        continue;
                    }
                    if !zz_pm::manifest::is_identifier(&collection_name) {
                        self.errors.push(LoadError {
                            name: path.display().to_string(),
                            source: source.clone(),
                            diags: vec![error_at(
                                format!(
                                    "collection name `{collection_name}` is not a valid identifier"
                                ),
                                Span::new(0, 0),
                            )],
                        });
                        continue;
                    }
                    let ok = self.load_collection(
                        &collection_name,
                        &spec,
                        &canon,
                        &source,
                        imp_alias.as_deref(),
                    );
                    if ok {
                        let synth = PathBuf::from(format!("collect.{collection_name}"));
                        Self::record_edge(
                            &mut self.dep_edges,
                            &canon,
                            &cache::canon(&synth).unwrap_or_else(|_| synth.clone()),
                        );
                        if is_selective {
                            let sel_path =
                                self.canonical_sel_path(&imp, imp_alias.as_deref(), Some(&synth));
                            self.selective_imports.push((
                                canon.clone(),
                                sel_path,
                                imp_items.clone(),
                                false,
                            ));
                        }
                    }
                    continue;
                }
            }
            // Implicit package mapping (`import app.math`, issue #288):
            // the first segment matches `package.name` in the nearest
            // `zz.toml` → `<root>/src/...`. Precedence: std (above) >
            // self-package (here) > registry/plugin (below) > relative
            // file (bottom). No manifest or no name match → fall through
            // to the legacy branches untouched.
            if let Some((pkg_name, root)) = self.package_of(&canon) {
                if normalize_pkg(&pkg_name) == imp[0] {
                    let candidate = package_target(&root, &imp[1..]);
                    if !candidate.is_file() {
                        let want = if imp.len() == 1 {
                            "src/main.zz".to_string()
                        } else {
                            format!("src/{}.zz", imp[1..].join("/"))
                        };
                        self.errors.push(LoadError {
                            name: path.display().to_string(),
                            source: source.clone(),
                            diags: vec![error_at(
                                format!(
                                    "package `{}` resolved to `{}`: file not found\n\
                                     hint: `{}` maps to `{want}` under the project root; check the `src/` layout",
                                    imp[0],
                                    candidate.display(),
                                    imp.join("."),
                                ),
                                Span::new(0, 0),
                            )],
                        });
                        continue;
                    }
                    // Bare `import app` binds `src/main.zz` under the
                    // package name itself (not the `main` file stem) so
                    // selective imports (`import app(x)`) and qualified
                    // calls (`app.x()`) agree. Deeper paths bind under
                    // their file stem (or `as` alias), mirroring
                    // relative imports.
                    let effective_alias = imp_alias.clone().or_else(|| {
                        if imp.len() == 1 {
                            Some(imp[0].clone())
                        } else {
                            None
                        }
                    });
                    // Report cycles at the import site, against the
                    // importer's source (mirrors the relative branch).
                    if let Ok(dep) = cache::canon(&candidate) {
                        if self.visiting.contains(&dep) {
                            self.errors.push(LoadError {
                                 name: path.display().to_string(),
                                 source: source.clone(),
                                 diags: vec![error_at(
                                     format!(
                                         "circular import: `{}` imports `{}`, which (transitively) imports `{}`\n\
                                          hint: circular imports are not allowed; consider restructuring your code to break the cycle",
                                         path.display(),
                                         candidate.display(),
                                         path.display()
                                     ),
                                     Span::new(0, 0),
                                 )],
                             });
                            continue;
                        }
                        // Same-file re-imports under another namespace are
                        // allowed (#380): the alias maps to the canonical
                        // namespace in `register_ns`, and references
                        // rewrite through it. Nothing to check here.
                    }
                    self.load_file(&candidate, effective_alias.as_deref())?;
                    // S1 dep edge (package file). Re-canonicalize
                    // defensively (filesystem cache hot).
                    if let Ok(dep) = cache::canon(&candidate) {
                        Self::record_edge(&mut self.dep_edges, &canon, &dep);
                        // Same-file alias pair for reference rewriting (#380).
                        let requested = module_ns(effective_alias.as_deref(), &candidate);
                        self.record_ns_alias(&canon, &requested, &dep);
                    }
                    if is_selective {
                        // Seeds hold `ns.sym` under the canonical seed
                        // namespace (see `canonical_sel_path`).
                        let dep_canon = cache::canon(&candidate).ok();
                        let sel_path = self.canonical_sel_path(
                            &imp,
                            effective_alias.as_deref(),
                            dep_canon.as_deref(),
                        );
                        self.selective_imports
                            .push((canon.clone(), sel_path, imp_items, false));
                    }
                    continue;
                }
            }
            // Plugin dependency import (`import zimg`): the dep ships a
            // `plugin.zzi` manifest. Merge its signatures under their
            // declared ZZ names and register the namespace. Selective
            // (`import zimg(resize)` / `as rz` / `(*)`) registers bare
            // names first-wins, mirroring std selective imports (#229).
            if imp.len() == 1 {
                if let Some(root) = find_project_root(&canon) {
                    if let Some(pkg_dir) = resolve_plugin_pkg(&root, &imp[0]) {
                        // Reverse edge for nested imports (#420), same as
                        // the registry branches.
                        cache::record_dep_consumer(&pkg_dir, &root);
                        self.import_plugin(&imp[0], imp_alias.as_deref(), &pkg_dir, path, &source);
                        if !imp_items.is_empty() {
                            let ns = imp_alias.as_deref().unwrap_or(&imp[0]);
                            let prefix = format!("{ns}.");
                            for item in &imp_items {
                                match item {
                                    ImportItem::Named { name, alias, .. } => {
                                        let target = alias.as_ref().unwrap_or(name).clone();
                                        let qualified = format!("{prefix}{name}");
                                        if let Some(sig) = self.funcs.get(&qualified).cloned() {
                                            // First registration wins: two
                                            // plugins must never collide
                                            // silently on short names.
                                            self.funcs.entry(target.clone()).or_insert(sig.clone());
                                            self.all_funcs.entry(target).or_insert(sig);
                                        } else if let Some(sig) = self.funcs.get(name).cloned() {
                                            // Manifest declared a bare name
                                            // (no ns prefix); still honor it.
                                            self.funcs.entry(target.clone()).or_insert(sig.clone());
                                            self.all_funcs.entry(target).or_insert(sig);
                                        } else {
                                            self.errors.push(LoadError {
                                                name: path.display().to_string(),
                                                source: source.clone(),
                                                diags: vec![error_at(
                                                    format!(
                                                        "symbol `{name}` not found in plugin `{}`\n\
                                                         hint: check the plugin's `plugin.zzi` exports",
                                                        imp[0]
                                                    ),
                                                    Span::new(0, 0),
                                                )],
                                            });
                                        }
                                    }
                                    ImportItem::Wildcard { .. } => {
                                        let keys: Vec<String> = self
                                            .funcs
                                            .keys()
                                            .filter(|k| k.starts_with(&prefix))
                                            .cloned()
                                            .collect();
                                        for key in keys {
                                            if let Some(bare) = key.strip_prefix(&prefix) {
                                                if bare.is_empty() || bare.contains('.') {
                                                    continue;
                                                }
                                                if let Some(sig) = self.funcs.get(&key).cloned() {
                                                    self.funcs
                                                        .entry(bare.to_string())
                                                        .or_insert(sig.clone());
                                                    self.all_funcs
                                                        .entry(bare.to_string())
                                                        .or_insert(sig);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
                }
                // Registry/git dependency import (`import e2epkg`): plain-ZZ
                // package, entry file loaded under the dep namespace (or
                // alias). Every ancestor root is tried so transitive deps
                // (imported from inside vendor/) resolve against the outer
                // project lock.
                {
                    let mut resolved: Option<(PathBuf, PathBuf)> = None;
                    for root in search_roots(&canon) {
                        if let Some(entry) = resolve_registry_entry(&root, &imp[0]) {
                            resolved = Some((root, entry));
                            break;
                        }
                    }
                    if let Some((root, entry)) = resolved {
                        // Reverse edge for nested imports (#420): files
                        // inside this package resolve against `root` too.
                        if let Some(pkg_dir) = pkg_dir_of_entry(&entry) {
                            cache::record_dep_consumer(&pkg_dir, &root);
                        }
                        let alias = imp_alias.clone().unwrap_or_else(|| imp[0].clone());
                        self.load_file(&entry, Some(alias.as_str()))?;
                        // S1 dep edge (registry package file).
                        Self::record_edge(
                            &mut self.dep_edges,
                            &canon,
                            &cache::canon(&entry).unwrap_or(entry.clone()),
                        );
                        // Same-file alias pair for reference rewriting (#380).
                        if let Ok(dep) = cache::canon(&entry) {
                            self.record_ns_alias(&canon, &alias, &dep);
                        }
                        if is_selective {
                            // Deferred to finish(): seeds hold `ns.sym`
                            // under the canonical seed namespace (see
                            // `canonical_sel_path`).
                            let dep_canon = cache::canon(&entry).ok();
                            let sel_path = self.canonical_sel_path(
                                &imp,
                                Some(alias.as_str()),
                                dep_canon.as_deref(),
                            );
                            self.selective_imports
                                .push((entry, sel_path, imp_items, false));
                        }
                        continue;
                    }
                }
            }
            // Dotted dependency import (`import tui.screen`, #375): the
            // first segment names a registry/git/path/plugin dependency
            // and the rest maps to `<depdir>/src/<mod...>.zz`. Only fires
            // when the submodule file positively exists — anything else
            // falls through to legacy relative resolution untouched, so
            // local `a/b.zz` layouts never change meaning.
            if imp.len() > 1 {
                if let Some((root, candidate)) = resolve_dep_submodule(&canon, &imp) {
                    // Reverse edge for nested imports (#420), same as the
                    // bare-registry branch above.
                    if let Some(pkg_dir) = pkg_dir_of_entry(&candidate) {
                        cache::record_dep_consumer(&pkg_dir, &root);
                    }
                    // Effective namespace mirrors local imports: the `as`
                    // alias when present, else the last segment
                    // (`import tui.screen` binds `screen.*`, #368).
                    let eff_ns = imp_alias
                        .clone()
                        .unwrap_or_else(|| imp.last().cloned().unwrap_or_default());
                    // Cycle + double-namespace diagnostics mirror the
                    // local branch below (importer source, same hints).
                    // Same-file re-imports under another namespace are
                    // allowed (#380): the alias maps to the canonical
                    // namespace in `register_ns`, and references rewrite
                    // through it.
                    if let Ok(dep) = cache::canon(&candidate) {
                        if self.visiting.contains(&dep) {
                            self.errors.push(LoadError {
                                 name: path.display().to_string(),
                                 source: source.clone(),
                                 diags: vec![error_at(
                                     format!(
                                         "circular import: `{}` imports `{}`, which (transitively) imports `{}`\n\
                                          hint: circular imports are not allowed; consider restructuring your code to break the cycle",
                                         path.display(),
                                         candidate.display(),
                                         path.display()
                                     ),
                                     Span::new(0, 0),
                                 )],
                             });
                            continue;
                        }
                    }
                    self.load_file(&candidate, Some(&eff_ns))?;
                    // S1 dep edge (dependency submodule file).
                    if let Ok(dep) = cache::canon(&candidate) {
                        Self::record_edge(&mut self.dep_edges, &canon, &dep);
                        // Same-file alias pair for reference rewriting (#380).
                        self.record_ns_alias(&canon, &eff_ns, &dep);
                    }
                    if is_selective {
                        // Seeds hold `ns.sym` under the canonical seed
                        // namespace (see `canonical_sel_path`).
                        let dep_canon = cache::canon(&candidate).ok();
                        let sel_path =
                            self.canonical_sel_path(&imp, Some(&eff_ns), dep_canon.as_deref());
                        self.selective_imports
                            .push((canon.clone(), sel_path, imp_items, false));
                    }
                    continue;
                }
            }
            // Local file import.
            let rel = canon
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(format!("{}.zz", imp.join("/")));
            // Actionable missing-dependency hints (#420): the file is
            // missing and this is a bare-name import. Two cases the
            // relative-path error obscures:
            //   1. the name is declared in zz.toml but not installed
            //      (`zz install` materializes vendor/ + lock), and
            //   2. the name is a transitive dep (declared by a direct
            //      dependency's manifest) — usable after `zz add`.
            // Dotted paths, existing files, and project-less imports
            // keep their current messages (the no-project hint above).
            if imp.len() == 1 && cache::canon(&rel).is_err() {
                if let Some(root) = find_project_root(&canon) {
                    if let Some(diag) = Self::missing_dep_hint(&root, &imp[0]) {
                        self.errors.push(LoadError {
                            name: path.display().to_string(),
                            source: source.clone(),
                            diags: vec![error_at(diag, Span::new(0, 0))],
                        });
                        continue;
                    }
                }
            }
            // Standalone file with a bare-name import and no project
            // (#419): the name is almost certainly a registry package,
            // but registry resolution needs a project (zz.toml + lock).
            // Say so instead of falling through to the relative-path
            // error, which sends standalone-file users down the wrong
            // path. Dotted paths keep the relative error (they read as
            // local layouts), as do files inside a project (missing dep
            // or missing install — different problems with their own
            // messages elsewhere).
            if imp.len() == 1 && cache::canon(&rel).is_err() && find_project_root(&canon).is_none()
            {
                self.errors.push(LoadError {
                    name: path.display().to_string(),
                    source: source.clone(),
                    diags: vec![error_at(
                        format!(
                            "cannot resolve import `{}`: no project (zz.toml) found above `{}`\n\
                             hint: outside a project, `import {0}` resolves as a relative file; \
                             for a registry package, scaffold a project (`zz new <name>`) and `zz install` it first",
                            imp[0],
                            canon.display(),
                        ),
                        Span::new(0, 0),
                    )],
                });
                continue;
            }
            // Report cycles at the import site, against the importer's source.
            if let Ok(rel_canon) = cache::canon(&rel) {
                if self.visiting.contains(&rel_canon) {
                    self.errors.push(LoadError {
                         name: path.display().to_string(),
                         source: source.clone(),
                         diags: vec![error_at(
                             format!(
                                 "circular import: `{}` imports `{}`, which (transitively) imports `{}`\n\
                                  hint: circular imports are not allowed; consider restructuring your code to break the cycle",
                                 path.display(),
                                 rel.display(),
                                 path.display()
                             ),
                             Span::new(0, 0),
                         )],
                     });
                    continue;
                }
                // Same-file re-imports under another namespace are
                // allowed (#380): the alias maps to the canonical
                // namespace in `register_ns`, and references rewrite
                // through it. Nothing further to check here.
            }
            self.load_file(&rel, imp_alias.as_deref())?;
            // S1 dep edge (local file). `rel_canon` was verified above;
            // re-canonicalize defensively (filesystem cache hot).
            if let Ok(dep) = cache::canon(&rel) {
                Self::record_edge(&mut self.dep_edges, &canon, &dep);
                // Same-file alias pair for reference rewriting (#380).
                let requested = module_ns(imp_alias.as_deref(), &rel);
                self.record_ns_alias(&canon, &requested, &dep);
            }
            if is_selective {
                // Seeds hold `ns.sym` under the canonical seed namespace
                // (see `canonical_sel_path`).
                let dep_canon = cache::canon(&rel).ok();
                let sel_path =
                    self.canonical_sel_path(&imp, imp_alias.as_deref(), dep_canon.as_deref());
                self.selective_imports
                    .push((canon.clone(), sel_path, imp_items, false));
            }
        }

        self.visiting.remove(&canon);
        self.done.insert(canon.clone());
        self.sources.insert(canon.clone(), source);

        let ns = module_ns(alias, &canon);
        let src = self.sources[&canon].clone();
        if self.register_ns(&ns, &canon, path, &src) {
            let mut program = program;
            // Same-file alias heads requested by this module's own imports
            // rewrite through the canonical namespace (#380).
            let aliases: HashMap<String, String> = self
                .ns_aliases
                .iter()
                .filter(|((imp, _), _)| imp == &canon)
                .map(|((_, req), c)| (req.clone(), c.clone()))
                .collect();
            namespace_program(&mut program, &ns, &aliases);
            self.programs.insert(canon.clone(), program);
            self.order.push(canon.clone());
            // Plugin package entry (`<pkg>.zz` / `src/<pkg>.zz` next to
            // `plugin.zzi`): the package's own module sees its manifest
            // signatures, so a generated wrapper (`src/<pkg>.zz`, #235)
            // can call through by declared name. Same seed treatment as
            // importer-side merging (seed-owned, every module agrees).
            self.merge_own_plugin_sigs(&canon);
        }
        Ok(())
    }

    /// Validate every `[collect.*]` declared by the root manifest, even
    /// when nothing imports it yet. This keeps `zz check` honest about a
    /// broken registry and lets the import path skip a repeat diagnostic.
    fn validate_declared_collections(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let Some(manifest) = cache::manifest_of(&root) else {
            return;
        };
        if manifest.collections.is_empty() {
            return;
        }
        let toml = root.join("zz.toml");
        let source = std::fs::read_to_string(&toml).unwrap_or_default();
        for (name, spec) in &manifest.collections {
            let err = if !zz_pm::manifest::is_identifier(name) {
                Some(format!(
                    "collection name `{name}` is not a valid identifier"
                ))
            } else {
                spec.validate(&root)
                    .err()
                    .or_else(|| spec.discover(&root).err())
            };
            if let Some(err) = err {
                self.invalid_collections.insert(name.clone());
                self.errors.push(LoadError {
                    name: toml.display().to_string(),
                    source: source.clone(),
                    diags: vec![error_at(
                        format!("collection `{name}` in zz.toml is invalid: {err}"),
                        Span::new(0, 0),
                    )],
                });
            }
        }
    }

    /// Load a declared `[collect.<name>]` registry as a synthesized module.
    ///
    /// Members are loaded first (under internal namespaces) so their `run`
    /// signatures seed the checker before the generated module is checked.
    /// The final AST is built in [`Loader::prepare_collection`] during the
    /// check pass, once each member's real checked signature is available.
    fn load_collection(
        &mut self,
        name: &str,
        spec: &zz_pm::manifest::CollectionSpec,
        importer: &Path,
        source: &str,
        alias: Option<&str>,
    ) -> bool {
        let Some(root) = self.root.clone() else {
            self.errors.push(LoadError {
                name: importer.display().to_string(),
                source: source.to_string(),
                diags: vec![error_at(
                    format!(
                        "cannot import collection `collect.{name}`: no project (zz.toml) found\n\
                         hint: collections are declared in the root package's zz.toml"
                    ),
                    Span::new(0, 0),
                )],
            });
            return false;
        };
        let synth = PathBuf::from(format!("collect.{name}"));
        let ns = alias.unwrap_or(name).to_string();
        if !zz_pm::manifest::is_identifier(&ns) {
            self.errors.push(LoadError {
                name: importer.display().to_string(),
                source: source.to_string(),
                diags: vec![error_at(
                    format!("collection namespace `{ns}` is not a valid identifier"),
                    Span::new(0, 0),
                )],
            });
            return false;
        }
        // Manifest validation already reported this collection; don't
        // repeat the same error at every import site.
        if self.invalid_collections.contains(name) {
            return false;
        }
        if self.done.contains(&synth) {
            self.record_ns_alias(importer, &ns, &synth);
            return true;
        }
        if self.visiting.contains(&synth) {
            self.errors.push(LoadError {
                name: importer.display().to_string(),
                source: source.to_string(),
                diags: vec![error_at(
                    format!(
                        "circular collection import: `collect.{name}` is imported while the collection is still loading"
                    ),
                    Span::new(0, 0),
                )],
            });
            return false;
        }
        if let Err(e) = spec.validate(&root) {
            self.errors.push(LoadError {
                name: importer.display().to_string(),
                source: source.to_string(),
                diags: vec![error_at(
                    format!("collection `{name}` in zz.toml is invalid: {e}"),
                    Span::new(0, 0),
                )],
            });
            return false;
        }
        let members = match spec.discover(&root) {
            Ok(members) => members,
            Err(e) => {
                self.errors.push(LoadError {
                    name: importer.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!("collection `{name}`: {e}"),
                        Span::new(0, 0),
                    )],
                });
                return false;
            }
        };
        let (sig_ty, sig_source) = match spec.parsed_signature_with_source() {
            Ok(v) => v,
            Err(e) => {
                self.errors.push(LoadError {
                    name: importer.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!("collection `{name}` has an invalid signature: {e}"),
                        Span::new(0, 0),
                    )],
                });
                return false;
            }
        };

        self.visiting.insert(synth.clone());
        let mut loaded = Vec::new();
        for member in members {
            let member_ns = format!("__collect_{name}_{}", member.name);
            if let Err(e) = self.load_file(&member.path, Some(&member_ns)) {
                self.errors.push(LoadError {
                    name: importer.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!(
                            "cannot load collection member `{}`: {e}",
                            member.path.display()
                        ),
                        Span::new(0, 0),
                    )],
                });
                continue;
            }
            let canon = cache::canon(&member.path).unwrap_or_else(|_| member.path.clone());
            Self::record_edge(&mut self.dep_edges, &synth, &canon);
            let member_source = self.sources.get(&canon).cloned().unwrap_or_default();
            let run_span = self
                .programs
                .get(&canon)
                .and_then(|p| {
                    p.stmts.iter().find_map(|s| match s {
                        Stmt::Func {
                            name,
                            pub_: true,
                            span,
                            ..
                        } if name.last().map(String::as_str) == Some(spec.export.as_str()) => {
                            Some(*span)
                        }
                        _ => None,
                    })
                })
                .unwrap_or_else(|| Span::new(0, 0));
            loaded.push(CollectionMember {
                name: member.name,
                ns: member_ns,
                path: canon,
                file: member.file,
                run_span,
                source: member_source,
            });
        }
        self.visiting.remove(&synth);
        self.done.insert(synth.clone());

        if !self.register_ns(&ns, &synth, importer, source) {
            return false;
        }
        self.sources.insert(
            synth.clone(),
            format!(
                "// synthesized collection `collect.{name}`\n\
                 dir = \"{}\"\n\
                 sig = \"{}\"\n\
                 name = \"{}\"\n\
                 export = \"{}\"\n",
                spec.dir, spec.sig, spec.name, spec.export
            ),
        );
        self.programs.insert(
            synth.clone(),
            Program {
                stmts: Vec::new(),
                span: Span::new(0, 0),
            },
        );
        self.order.push(synth.clone());
        self.collections.insert(
            synth,
            CollectionModule {
                name: name.to_string(),
                ns,
                export: spec.export.clone(),
                sig_ty,
                sig_source,
                members: loaded,
            },
        );
        true
    }

    /// Validate members against the declared signature and synthesize the
    /// collection AST. Called during the check pass, after every member
    /// module has been checked and its `pub run` signature seeded.
    ///
    /// Returns the generated source (also stored in `files` for diagnostic
    /// rendering). Invalid members are reported at their own file/line and
    /// omitted from the generated `ALL` array; the collection then contains
    /// only valid members, exactly as the failing build would suggest.
    fn prepare_collection(&mut self, path: &Path, failed_ns: &HashSet<String>) -> String {
        let Some(info) = self.collections.get(path).cloned() else {
            return String::new();
        };
        let expected = self.collection_expected_type(&info.sig_ty);
        let mut valid = Vec::new();
        for member in &info.members {
            let key = format!("{}.{}", member.ns, info.export);
            let Some(sig) = self.funcs.get(&key).cloned() else {
                // A failed member module already reported its own error;
                // don't bury it under a collection cascade.
                if failed_ns.contains(&member.ns) {
                    continue;
                }
                let rel = member
                    .path
                    .strip_prefix(self.root.as_deref().unwrap_or(Path::new("")))
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| member.path.display().to_string());
                let expected_text = format_expected_sig(&info.export, &expected);
                self.errors.push(LoadError {
                    name: member.path.display().to_string(),
                    source: member.source.clone(),
                    diags: vec![error_at(
                        format!(
                            "{rel} does not match collection `{}`\n\
                             expected: {expected_text}\n\
                             found:    `pub {export}` is missing",
                            info.name,
                            export = info.export
                        ),
                        member.run_span,
                    )
                    .with_note(format!(
                        "hint: add `pub {}({}) -> {}` to `{}`",
                        info.export,
                        expected_param_list(&expected),
                        expected_ret(&expected),
                        member.file
                    ))],
                });
                continue;
            };
            let actual = Type::Func(
                sig.params.iter().map(|(_, t)| t.clone()).collect(),
                Box::new(sig.ret.clone()),
            );
            if actual == expected {
                valid.push(member.clone());
                continue;
            }
            let rel = member
                .path
                .strip_prefix(self.root.as_deref().unwrap_or(Path::new("")))
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| member.path.display().to_string());
            let expected_text = format_expected_sig(&info.export, &expected);
            let found_text = format_found_sig(&info.export, &sig);
            self.errors.push(LoadError {
                name: member.path.display().to_string(),
                source: member.source.clone(),
                diags: vec![error_at(
                    format!(
                        "{rel} does not match collection `{}`\n\
                         expected: {expected_text}\n\
                         found:    {found_text}",
                        info.name
                    ),
                    member.run_span,
                )
                .with_note(format!(
                    "hint: change `{}` in `{}` to `{expected_text}`",
                    info.export, member.file
                ))],
            });
        }
        let source = render_collection_source(&info, &valid);
        let parsed = parse(&source);
        if let Some(diag) = parsed
            .errors
            .iter()
            .find(|d| d.severity == zz_frontend::diag::Severity::Error)
            .cloned()
        {
            // A synthesized module must always parse; surface a bug loudly
            // rather than silently dropping the collection.
            self.errors.push(LoadError {
                name: path.display().to_string(),
                source: source.clone(),
                diags: vec![diag],
            });
            return source;
        }
        let mut program = parsed.program;
        let aliases = HashMap::new();
        namespace_program(&mut program, &info.ns, &aliases);
        self.programs.insert(path.to_path_buf(), program);
        source
    }

    /// Convert a parsed signature `Ty` to the checker's `Type`, resolving
    /// local aliases/structs/enums so member signatures compare like for
    /// like. Mirrors the checker's own table precedence.
    fn collection_expected_type(&self, ty: &Ty) -> Type {
        ty_to_checker_type(ty, &self.structs, &self.aliases, &self.enums)
    }

    /// Import a plugin dependency (`import zimg`): merge its manifest
    /// signatures under their declared ZZ names. With an alias
    /// (`import zimg as z`), names under the `<dep>.` prefix are also
    /// copied to `<alias>.`. Bare short names are never created.
    fn import_plugin(
        &mut self,
        dep: &str,
        alias: Option<&str>,
        pkg_dir: &Path,
        importer: &Path,
        source: &str,
    ) {
        let manifest = match zz_plugin::load_manifest(&pkg_dir.join("plugin.zzi")) {
            Ok(m) => m,
            Err(e) => {
                self.errors.push(LoadError {
                    name: importer.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!("cannot load plugin `{dep}` manifest: {e}"),
                        Span::new(0, 0),
                    )],
                });
                return;
            }
        };
        for (name, sig) in &manifest.funcs {
            self.funcs.insert(name.clone(), sig.clone());
            // Seed-owned: plugin deps shared by several modules must
            // resolve identically in each (same consumption bug as #214).
            self.seed_func_keys.insert(name.clone());
        }
        // C-symbol aliases (#235) resolve identically too.
        for (alias, sig) in Self::plugin_aliases(&manifest.funcs) {
            self.funcs.entry(alias.clone()).or_insert(sig.clone());
            self.all_funcs.entry(alias.clone()).or_insert(sig);
            self.seed_func_keys.insert(alias);
        }
        let ns = alias.unwrap_or(dep);
        if ns != dep {
            let prefix = format!("{dep}.");
            let aliased: Vec<(String, zz_checker::FuncSig)> = manifest
                .funcs
                .iter()
                .filter(|(n, _)| n.starts_with(&prefix))
                .map(|(n, s)| (format!("{ns}.{}", &n[prefix.len()..]), s.clone()))
                .collect();
            for (name, sig) in aliased {
                self.funcs.insert(name.clone(), sig);
                self.seed_func_keys.insert(name);
            }
        }
        // Ergonomic layer: a `<dep>.zz` entry file in the package root
        // (or `src/`, the conventional code dir) is loaded as a module
        // under the import namespace, so `import zimg` also brings in
        // `zimg.resize(...)` etc. Its relative imports resolve inside
        // the package. Manifest-only packages skip this and only record
        // the namespace.
        let entry = [format!("{dep}.zz"), format!("src/{dep}.zz")]
            .into_iter()
            .map(|rel| pkg_dir.join(rel))
            .find(|p| p.exists());
        if let Some(entry) = entry {
            // load_file records its own diagnostics; the entry exists.
            let _ = self.load_file(&entry, Some(ns));
        } else {
            // Namespace bookkeeping (re-import diagnostics stay consistent).
            self.register_ns(ns, &pkg_dir.join("plugin.zzi"), importer, source);
        }
    }

    /// Register a stdlib namespace without the one-namespace-per-module
    /// rule that governs user files: the same std module may serve
    /// several importers under different qualifiers (`import std.path
    /// as fspath` in one module, plain `import std.path` in another),
    /// since every qualifier gets its own copied entries (issue #216).
    /// A namespace claimed by two DIFFERENT modules stays an error.
    fn register_std_ns(&mut self, ns: &str, module: &str, display: &Path, source: &str) -> bool {
        let canon = PathBuf::from(format!("std:{module}"));
        if let Some(existing) = self.namespaces.get(ns) {
            if *existing != canon {
                self.errors.push(LoadError {
                    name: display.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!(
                            "namespace `{ns}` is claimed by both `{}` and `std.{module}`\n\
                             hint: use an alias for one of the imports",
                            existing.display()
                        ),
                        Span::new(0, 0),
                    )],
                });
                return false;
            }
            return true;
        }
        self.namespaces.insert(ns.to_string(), canon);
        true
    }

    /// Register a namespace → module mapping, detecting collisions. Returns
    /// false (and records an error) when two different modules claim the same
    /// namespace. One module under several namespaces is allowed (#380):
    /// the first binding wins as canonical; import sites record the
    /// requested→canonical pair (see `record_ns_alias`) so references
    /// rewrite through one spelling and keep a single value/type identity.
    fn register_ns(&mut self, ns: &str, canon: &Path, display: &Path, source: &str) -> bool {
        if let Some(existing) = self.ns_of.get(canon) {
            if existing != ns {
                // Same file, another namespace: alias, don't error.
                // (#380: `import tui.keys as tkeys` alongside a bare
                // `import tui.keys` elsewhere must compose.)
                if !self.namespaces.contains_key(ns) {
                    self.namespaces.insert(ns.to_string(), canon.to_path_buf());
                }
                return true;
            }
            return true;
        }
        if let Some(existing) = self.namespaces.get(ns) {
            if existing != canon {
                self.errors.push(LoadError {
                    name: display.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!(
                            "namespace `{ns}` is claimed by both `{}` and `{}`\n\
                             hint: two different files are trying to use the same namespace; \
                             consider using an alias for one of the imports",
                            existing.display(),
                            display.display()
                        ),
                        Span::new(0, 0),
                    )],
                });
                return false;
            }
            return true;
        }
        self.namespaces.insert(ns.to_string(), canon.to_path_buf());
        self.ns_of.insert(canon.to_path_buf(), ns.to_string());
        true
    }

    /// Merge a plugin package's own manifest signatures into its entry
    /// module (`<pkg>.zz` / `src/<pkg>.zz` next to `plugin.zzi`), so a
    /// generated wrapper (#235) can call its natives without dotted
    /// self-paths (which read as method calls). Registers the C-symbol
    /// alias spellings (`bytehash.sum` → `bytehash_sum`): bare names the
    /// defining module calls directly. Aliases never override real names
    /// (or_insert); an author-defined collision surfaces as a loud arity
    /// error at the call, never a silent reroute. No-op for non-plugin
    /// files.
    fn merge_own_plugin_sigs(&mut self, canon: &Path) {
        let Some((pkg_name, root)) = self.package_of(canon) else {
            return;
        };
        if canon != root.join(format!("{pkg_name}.zz"))
            && canon != root.join("src").join(format!("{pkg_name}.zz"))
        {
            return;
        }
        let zzi = root.join("plugin.zzi");
        if !zzi.exists() {
            return;
        }
        let manifest = match zz_plugin::load_manifest(&zzi) {
            Ok(m) => m,
            Err(_) => return,
        };
        for (alias, sig) in Self::plugin_aliases(&manifest.funcs) {
            self.funcs.entry(alias.clone()).or_insert(sig.clone());
            self.all_funcs.entry(alias.clone()).or_insert(sig);
            self.seed_func_keys.insert(alias);
        }
    }

    /// C-symbol alias spellings for plugin externs (`bytehash.sum` →
    /// `bytehash_sum`, #235). Only spellings that are valid ZZ
    /// identifiers, differ from the declared name, and collide with no
    /// other manifest entry qualify. Sorted for determinism.
    fn plugin_aliases(
        funcs: &HashMap<String, zz_checker::FuncSig>,
    ) -> Vec<(String, zz_checker::FuncSig)> {
        let mut out = Vec::new();
        for (name, sig) in funcs {
            let alias = sig.c_symbol(name);
            if alias != *name && !funcs.contains_key(&alias) && is_zz_ident(&alias) {
                out.push((alias, sig.clone()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Rewrite a diagnostic that references an item which exists but was not
    /// exported (it is private to its module) into a targeted error with a
    /// hint to add `pub`. This is a DX upgrade over the generic
    /// "undefined variable" / "unknown struct" / "no such field" messages.
    ///
    /// Returns the fully-qualified private name when the diagnostic was
    /// rewritten, so the caller can prune cascading noise for that namespace.
    fn enrich_private(&self, d: &mut RawDiag) -> Option<String> {
        let msg = d.message.clone();

        // `undefined variable `ns.item`` — private function/variable/struct.
        if let Some(name) = quoted_name_after(&msg, "undefined variable ") {
            if let Some(kind) = self.private_item_kind(name) {
                d.message = format!("{kind} `{name}` is private");
                push_private_note(
                    d,
                    name,
                    "add `pub` to `{}` to make it visible outside its module",
                );
                return Some(name.to_string());
            }
        }

        // `unknown struct `ns.name`` — private struct.
        if let Some(name) = quoted_name_after(&msg, "unknown struct ") {
            if let Some(kind) = self.private_item_kind(name) {
                d.message = format!("{kind} `{name}` is private");
                push_private_note(
                    d,
                    name,
                    "add `pub` to `{}` to make it visible outside its module",
                );
                return Some(name.to_string());
            }
        }

        // `struct `T` has no field `f`` where `T.f` is a private method from
        // an `impl` block.
        if let Some((sname, field)) = struct_no_field(&msg) {
            let key = format!("{sname}.{field}");
            if self.all_funcs.contains_key(&key) && !self.funcs.contains_key(&key) {
                d.message = format!("method `{field}` on struct `{sname}` is private");
                d.notes.push(format!(
                    "add `pub` to `{field}` inside `impl {sname}` to make it callable from other modules"
                ));
                return Some(key);
            }
        }
        None
    }

    /// If diagnostic message `msg` is cascade noise from a failed dependency
    /// (#274) — a reference to namespace `ns` whose module failed to check —
    /// return `ns`. The failed module contributed no pubs, so every use of
    /// `ns` or `ns.item` in an importer is a follow-on of the same root
    /// cause, reported once at the dependency itself (calls `ns.f()` and
    /// struct literals `ns.T{}` included). The importer's own
    /// namespace never counts (its references are genuine).
    fn failed_dep_ns(failed: &HashSet<String>, own_ns: &str, msg: &str) -> Option<String> {
        failed.iter().find_map(|ns| {
            if ns == own_ns {
                return None;
            }
            let bare = format!("undefined variable `{ns}`");
            let dotted = format!("undefined variable `{ns}.");
            let called = format!("unknown function `{ns}");
            let structured = format!("unknown struct `{ns}");
            let unused = format!("unused import `{ns}`");
            if msg == bare
                || msg.starts_with(&dotted)
                || msg.starts_with(&called)
                || msg.starts_with(&structured)
                || msg == unused
            {
                Some(ns.clone())
            } else {
                None
            }
        })
    }

    /// The item kind if `name` exists in the private universe (all items of
    /// already-loaded modules) but is missing from the pub export seed.
    fn private_item_kind(&self, name: &str) -> Option<&'static str> {
        if self.all_funcs.contains_key(name) && !self.funcs.contains_key(name) {
            return Some("function");
        }
        if self.all_structs.contains_key(name) && !self.structs.contains_key(name) {
            return Some("struct");
        }
        if self.all_bindings.contains_key(name) && !self.bindings.contains_key(name) {
            return Some("variable");
        }
        None
    }

    /// Type-check every module in dependency order, accumulating the checker
    /// seed. Modules with errors do not contribute their definitions.
    fn finish(mut self) -> LoadResult {
        let mut files = Vec::with_capacity(self.order.len());
        let mut programs = Vec::with_capacity(self.order.len());
        // S1 dep-aware check-cache keys, resolved up front (sources still
        // intact): each module's key covers its source plus its transitive
        // deps' keys. Only true dependents recheck on an edit; anything
        // unresolvable (deleted file, cycle) misses and checks normally.
        let genesis = cache::genesis_key(&self.plugin_names);
        let mut key_memo: HashMap<PathBuf, String> = HashMap::new();
        let mut module_keys: HashMap<PathBuf, String> = HashMap::new();
        for path in &self.order {
            let mut stack = Vec::new();
            if let Some(key) = resolve_module_key(
                &genesis,
                &self.sources,
                &self.dep_edges,
                &self.ns_of,
                path,
                &mut key_memo,
                &mut stack,
            ) {
                module_keys.insert(path.clone(), key);
            }
        }

        // Namespaces of modules that failed to check (#274): `order` is
        // dependency-first, so a failed dependency is recorded before its
        // importers are processed and their cascade noise can be collapsed
        // to one targeted error each.
        let mut failed_ns: HashSet<String> = HashSet::new();
        let order = self.order.clone();
        for path in &order {
            let name = path.display().to_string();
            let source = self.sources.remove(path).unwrap_or_default();
            files.push((name.clone(), source.clone()));

            // Collection modules are materialized here: member modules
            // have been checked (dependency-first order), so their real
            // signatures are available for validation and the generated
            // `ALL`/`get`/`all` AST can be built exactly once, before the
            // check that both engines consume.
            if self.collections.contains_key(path) {
                let generated = self.prepare_collection(path, &failed_ns);
                if let Some(last) = files.last_mut() {
                    last.1 = generated;
                }
            }

            // Process selective/wildcard imports for this module before
            // type-checking, so imported symbols are in the seed.
            let selective: Vec<_> = self
                .selective_imports
                .iter()
                .filter(|(p, _, _, _)| p == path)
                .cloned()
                .collect();
            for (_, imp_path, items, is_std) in selective {
                if is_std {
                    // Stdlib selective import (dotted key for nested
                    // modules: `std.sqlz.postgres` -> "sqlz.postgres").
                    if imp_path.len() < 2 {
                        continue;
                    }
                    let module = imp_path[1..].join(".");
                    let has_wildcard = items
                        .iter()
                        .any(|i| matches!(i, ImportItem::Wildcard { .. }));
                    if has_wildcard {
                        match register_wildcard_namespace(
                            module.as_str(),
                            &mut self.funcs,
                            &mut self.natives,
                        ) {
                            Err(msg) => {
                                self.errors.push(LoadError {
                                    name: name.clone(),
                                    source: source.clone(),
                                    diags: vec![error_at(
                                        format!("{msg}\n\
                                                 hint: this may occur if the stdlib module exports a conflicting name"),
                                        Span::new(0, 0),
                                    )],
                                });
                            }
                            // Seed-owned like full-module copies (#214).
                            Ok(inserted) => {
                                self.seed_func_keys.extend(inserted.clone());
                                // Mirror into the all-universe (the
                                // first-success snapshot below misses later
                                // modules' selectives): skip generic bare
                                // values like the local branch.
                                for target in &inserted {
                                    if let Some(sig) = self.funcs.get(target).cloned() {
                                        if sig.generics.is_empty() {
                                            self.all_funcs.entry(target.clone()).or_insert(sig);
                                        }
                                    }
                                }
                                self.stdlib_modules.insert(module.clone());
                                self.stdlib_wildcards.push(module.clone());
                            }
                        }
                    } else {
                        let name_aliases: Vec<(String, Option<String>)> = items
                            .iter()
                            .filter_map(|i| match i {
                                ImportItem::Named { name, alias, .. } => {
                                    Some((name.clone(), alias.clone()))
                                }
                                ImportItem::Wildcard { .. } => None,
                            })
                            .collect();
                        match register_selective_namespace(
                            module.as_str(),
                            &name_aliases,
                            &mut self.funcs,
                            &mut self.natives,
                        ) {
                            Ok((missing, inserted)) => {
                                // Seed-owned like full-module copies (#214).
                                self.seed_func_keys.extend(inserted.clone());
                                // Mirror into the all-universe (the
                                // first-success snapshot below misses later
                                // modules' selectives): skip generic bare
                                // values like the local branch.
                                for target in &inserted {
                                    if let Some(sig) = self.funcs.get(target).cloned() {
                                        if sig.generics.is_empty() {
                                            self.all_funcs.entry(target.clone()).or_insert(sig);
                                        }
                                    }
                                }
                                for sym in &missing {
                                    self.errors.push(LoadError {
                                        name: name.clone(),
                                        source: source.clone(),
                                        diags: vec![error_at(
                                            format!(
                                                "symbol `{sym}` not found in `std.{module}`\n\
                                                 hint: check the module's public exports"
                                            ),
                                            Span::new(0, 0),
                                        )],
                                    });
                                }
                            }
                            Err(msg) => {
                                self.errors.push(LoadError {
                                    name: name.clone(),
                                    source: source.clone(),
                                    diags: vec![error_at(
                                        format!("{msg}\n\
                                                 hint: this may occur if the stdlib module exports a conflicting name"),
                                        Span::new(0, 0),
                                    )],
                                });
                            }
                        }
                        // Collect constants (including aliases) for runtime injection.
                        let std_consts = zz_stdlib::stdlib_consts();
                        for (sym_name, alias) in &name_aliases {
                            let qualified = format!("std.{module}.{sym_name}");
                            if let Some(&val) = std_consts.get(&qualified) {
                                let target = alias.as_ref().unwrap_or(sym_name);
                                self.selected_consts.insert(target.clone(), val);
                            }
                        }
                        self.stdlib_modules.insert(module.clone());
                        self.stdlib_selectives.push((module.clone(), name_aliases));
                    }
                } else {
                    // Local file selective import — items are in seed as ns.name.
                    let ns = imp_path.last().map(String::as_str).unwrap_or("");
                    let prefix = format!("{ns}.");
                    for item in &items {
                        match item {
                            ImportItem::Named {
                                name: sym_name,
                                alias,
                                ..
                            } => {
                                let target = alias.as_ref().unwrap_or(sym_name);
                                let full = format!("{prefix}{sym_name}");
                                let mut found = false;
                                if let Some(sig) = self.funcs.get(&full).cloned() {
                                    // Generic functions have no value type:
                                    // skip the bare seed entry. Bare calls
                                    // resolve through the import-alias maps
                                    // (checker + runtimes) instead, and bare
                                    // *uses* stay errors.
                                    if sig.generics.is_empty() {
                                        self.funcs.insert(target.clone(), sig.clone());
                                        self.all_funcs.insert(target.clone(), sig);
                                    }
                                    found = true;
                                }
                                if let Some(entry) = self.natives.get(&full).cloned() {
                                    self.natives.insert(target.clone(), entry);
                                    found = true;
                                }
                                if let Some(ty) = self.bindings.get(&full).cloned() {
                                    self.bindings.insert(target.clone(), ty.clone());
                                    self.all_bindings.insert(target.clone(), ty);
                                    found = true;
                                }
                                if let Some(sig) = self.structs.get(&full).cloned() {
                                    self.structs.insert(target.clone(), sig.clone());
                                    self.all_structs.insert(target.clone(), sig);
                                    found = true;
                                }
                                if let Some(sig) = self.aliases.get(&full).cloned() {
                                    self.aliases.insert(target.clone(), sig.clone());
                                    self.all_aliases.insert(target.clone(), sig);
                                    found = true;
                                }
                                if let Some(sig) = self.enums.get(&full).cloned() {
                                    self.enums.insert(target.clone(), sig.clone());
                                    self.all_enums.insert(target.clone(), sig);
                                    found = true;
                                }
                                if !found {
                                    // #274: the symbol is missing because the
                                    // dependency itself failed — say so
                                    // instead of misdirecting toward `pub`.
                                    let missing = if failed_ns.contains(ns) {
                                        let dep_path = self
                                            .namespaces
                                            .get(ns)
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_else(|| ns.to_string());
                                        format!(
                                            "cannot use `{prefix}{sym_name}`: dependency `{dep_path}` failed to check\n\
                                             hint: fix the errors in `{dep_path}` first"
                                        )
                                    } else {
                                        format!(
                                            "symbol `{sym_name}` not found or not public in `{}`\n\
                                             hint: ensure the symbol is declared with `pub` in the imported module",
                                            imp_path.join(".")
                                        )
                                    };
                                    self.errors.push(LoadError {
                                        name: name.clone(),
                                        source: source.clone(),
                                        diags: vec![error_at(missing, Span::new(0, 0))],
                                    });
                                }
                            }
                            ImportItem::Wildcard { .. } => {
                                // Copy ALL pub items from ns.* → bare names.
                                let keys: Vec<String> = self
                                    .funcs
                                    .keys()
                                    .filter(|k| k.starts_with(&prefix))
                                    .cloned()
                                    .collect();
                                for key in keys {
                                    let bare = key[prefix.len()..].to_string();
                                    if let Some(sig) = self.funcs.get(&key).cloned() {
                                        // Generic functions have no value
                                        // type: no bare seed entry (calls
                                        // resolve via import aliases).
                                        if sig.generics.is_empty() {
                                            self.funcs.insert(bare.clone(), sig.clone());
                                            self.all_funcs.insert(bare, sig);
                                        }
                                    }
                                    if let Some(entry) = self.natives.get(&key).cloned() {
                                        self.natives.insert(key[prefix.len()..].to_string(), entry);
                                    }
                                }
                                let bkeys: Vec<String> = self
                                    .bindings
                                    .keys()
                                    .filter(|k| k.starts_with(&prefix))
                                    .cloned()
                                    .collect();
                                for key in bkeys {
                                    let bare = key[prefix.len()..].to_string();
                                    if let Some(ty) = self.bindings.get(&key).cloned() {
                                        self.bindings.insert(bare.clone(), ty.clone());
                                        self.all_bindings.insert(bare, ty);
                                    }
                                }
                                let skeys: Vec<String> = self
                                    .structs
                                    .keys()
                                    .filter(|k| k.starts_with(&prefix))
                                    .cloned()
                                    .collect();
                                for key in skeys {
                                    let bare = key[prefix.len()..].to_string();
                                    if let Some(sig) = self.structs.get(&key).cloned() {
                                        self.structs.insert(bare.clone(), sig.clone());
                                        self.all_structs.insert(bare, sig);
                                    }
                                }
                                let akeys: Vec<String> = self
                                    .aliases
                                    .keys()
                                    .filter(|k| k.starts_with(&prefix))
                                    .cloned()
                                    .collect();
                                for key in akeys {
                                    let bare = key[prefix.len()..].to_string();
                                    if let Some(sig) = self.aliases.get(&key).cloned() {
                                        self.aliases.insert(bare.clone(), sig.clone());
                                        self.all_aliases.insert(bare, sig);
                                    }
                                }
                                let ekeys: Vec<String> = self
                                    .enums
                                    .keys()
                                    .filter(|k| k.starts_with(&prefix))
                                    .cloned()
                                    .collect();
                                for key in ekeys {
                                    let bare = key[prefix.len()..].to_string();
                                    if let Some(sig) = self.enums.get(&key).cloned() {
                                        self.enums.insert(bare.clone(), sig.clone());
                                        self.all_enums.insert(bare, sig);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Remove processed selective imports for this module.
            // Replace selective import statements with synthetic Decl
            // assignments so the runtime (VM) creates the variable bindings.
            self.selective_imports.retain(|(p, _, _, _)| p != path);

            let mut program = self.programs.remove(path).unwrap();
            let mut new_stmts = Vec::new();
            for stmt in program.stmts.drain(..) {
                match &stmt {
                    Stmt::Import {
                        items,
                        path: imp_path,
                        alias: stmt_alias,
                        span,
                        ..
                    } if !items.is_empty() => {
                        // Selective imports: bare names are already in
                        // self.funcs / self.natives by the register_* helpers.
                        // The runtime resolves them as natives. For local
                        // files, emit synthetic Decls (below); for stdlib,
                        // KEEP the import statement (with items) so the AOT
                        // backend can resolve bare/aliased names to their
                        // canonical natives. It emits no code and the
                        // checker already tracks selective names, so no
                        // false "unused import" warning follows.
                        let is_stdlib = imp_path.first().map(String::as_str) == Some("std");

                        if is_stdlib {
                            // Stdlib: keep as-is for alias resolution.
                            // Constants are injected into the runtime env via
                            // LoadResult.consts (including aliased names).
                            new_stmts.push(stmt);
                        } else {
                            // Local file: keep the import (items intact) so
                            // every engine can build import-alias maps from
                            // it, then emit synthetic Decls below.
                            new_stmts.push(stmt.clone());
                            // resolve under the effective namespace: the
                            // statement alias when present (`import m as
                            // a(x)` binds `a.x`, not `m.x`). Canonicalized
                            // through same-file alias pairs (#380): an
                            // alias-spelled `tkeys` becomes the bound
                            // `keys` the seeds actually hold.
                            let stmt_ns = stmt_alias
                                .as_deref()
                                .or_else(|| imp_path.last().map(String::as_str))
                                .unwrap_or("");
                            let ns = self
                                .ns_aliases
                                .get(&(path.clone(), stmt_ns.to_string()))
                                .cloned()
                                .unwrap_or_else(|| stmt_ns.to_string());
                            let prefix = format!("{ns}.");
                            for item in items {
                                match item {
                                    ImportItem::Named {
                                        name: sym_name,
                                        alias,
                                        span: item_span,
                                    } => {
                                        let target = alias.as_ref().unwrap_or(sym_name);
                                        // Generic functions have no value
                                        // type: no value Decl. Bare calls
                                        // resolve via import aliases.
                                        let is_generic = self
                                            .funcs
                                            .get(&format!("{prefix}{sym_name}"))
                                            .map(|s| !s.generics.is_empty())
                                            .unwrap_or(false);
                                        if is_generic {
                                            continue;
                                        }
                                        // Structs are types, not values: the
                                        // bare name already resolves through
                                        // the structs seed (checker +
                                        // runtimes), and a `Name := ns.Name`
                                        // Decl would force a value lookup of
                                        // a type name ("undefined variable
                                        // `ns.Name`"). Skip the Decl unless
                                        // the name is also a value (func /
                                        // native / binding) that needs a
                                        // runtime binding.
                                        let qualified = format!("{prefix}{sym_name}");
                                        let is_struct = self.structs.contains_key(&qualified);
                                        // Aliases are types, not values: same
                                        // skip as structs, or `Name :=
                                        // ns.Name` forces a value lookup of
                                        // a type name ("undefined variable
                                        // `ns.Name`").
                                        let is_alias = self.aliases.contains_key(&qualified);
                                        // Enums are types, not values: same
                                        // skip (variant construction is a
                                        // call, resolved via the enum table).
                                        let is_enum = self.enums.contains_key(&qualified);
                                        let is_value = self
                                            .funcs
                                            .get(&qualified)
                                            .map(|s| s.generics.is_empty())
                                            .unwrap_or(false)
                                            || self.natives.contains_key(&qualified)
                                            || self.bindings.contains_key(&qualified);
                                        if (is_struct || is_alias || is_enum) && !is_value {
                                            continue;
                                        }
                                        let parts = vec![ns.to_string(), sym_name.clone()];
                                        new_stmts.push(Stmt::Decl {
                                            ty: None,
                                            name: zz_frontend::ast::Ident {
                                                name: target.clone(),
                                                span: *item_span,
                                            },
                                            value: Expr::Path {
                                                parts,
                                                span: *item_span,
                                            },
                                            span: *item_span,
                                            pub_: false,
                                            is_const: false,
                                        });
                                    }
                                    ImportItem::Wildcard { span: item_span } => {
                                        // Wildcard: emit a Decl for every pub
                                        // symbol in the imported module.
                                        // Generic functions get no value
                                        // Decl (no value type); bare calls
                                        // to them resolve via import aliases.
                                        let mut names: Vec<String> = Vec::new();
                                        for key in self.funcs.keys() {
                                            if key.starts_with(&prefix) {
                                                let bare = &key[prefix.len()..];
                                                if bare.is_empty() {
                                                    continue;
                                                }
                                                let is_generic = self
                                                    .funcs
                                                    .get(key)
                                                    .map(|s| !s.generics.is_empty())
                                                    .unwrap_or(false);
                                                if !is_generic {
                                                    names.push(bare.to_string());
                                                }
                                            }
                                        }
                                        for key in self.bindings.keys() {
                                            if key.starts_with(&prefix) {
                                                let bare = &key[prefix.len()..];
                                                if !bare.is_empty()
                                                    && !names.contains(&bare.to_string())
                                                {
                                                    names.push(bare.to_string());
                                                }
                                            }
                                        }
                                        for bare in &names {
                                            new_stmts.push(Stmt::Decl {
                                                ty: None,
                                                name: zz_frontend::ast::Ident {
                                                    name: bare.to_string(),
                                                    span: *item_span,
                                                },
                                                value: Expr::Path {
                                                    parts: vec![ns.to_string(), bare.to_string()],
                                                    span: *item_span,
                                                },
                                                span: *item_span,
                                                pub_: false,
                                                is_const: false,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => new_stmts.push(stmt),
                }
            }
            program.stmts = new_stmts;

            // Selective imports (`import std.path(join)`) call bare names
            // that resolve through a synthetic runtime binding with no
            // native equivalent — rewrite bare *call* callees to their
            // canonical paths (verified against the checker seed, so the
            // spelling always resolves on every engine). Value positions
            // keep the binding (first-class function values).
            let selective_map = rewrite::selective_rewrites(&program.stmts, &self.funcs);
            rewrite::rewrite_selective_calls(&mut program, &selective_map);

            // In the entry file, error if func main() and a top-level main()
            // call coexist — the auto-call would double-execute main().
            if *path == self.entry {
                let mut has_main_func = false;
                let mut has_main_call = false;
                for stmt in &program.stmts {
                    match stmt {
                        Stmt::Func { name, .. } => {
                            // After namespace rewriting, name may be
                            // ["ns.main"] or ["ns", "main"]. Check the
                            // joined form.
                            let joined = name.join(".");
                            if joined.ends_with(".main") || joined == "main" {
                                has_main_func = true;
                            }
                        }
                        Stmt::Expr(Expr::Call { callee, .. }) => {
                            if let Expr::Path { parts, .. } = callee.as_ref() {
                                if parts.last().map(|s| s.as_str()) == Some("main") {
                                    has_main_call = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if has_main_func && has_main_call {
                    self.errors.push(LoadError {
                        name: name.clone(),
                        source: source.clone(),
                        diags: vec![error_at(
                            "`main()` is auto-called; remove the explicit `main()` call\n\
                             hint: ZZ automatically calls main() if it exists; \
                             having both causes double execution",
                            Span::new(0, 0),
                        )],
                    });
                }
            }

            // Move (never clone) the seed maps into the checker: it stores
            // them in its tables untouched (seed entries are only read;
            // module items insert alongside) and they come back below via
            // key-set restore. Per-module seed cost drops from O(seed)
            // clones to zero (quadratic to linear across modules).
            // Bindings seeds stay cloned: the checker drops its seed env
            // (only new bindings are returned), so they are unrecoverable;
            // top-level bindings are rare, so the term is noise.
            //
            // S1 check-cache: on a hit the outcome is synthesized from
            // disk (seeds move out and back so the shared tail below sees
            // the exact same shapes as a fresh check); on a miss the fresh
            // outcome runs the shared tail and is stored afterwards from
            // own-only maps (never seeds — entries stay ~30KB, not 460KB).
            // Parse/selective/namespace work above always runs and feeds
            // both paths identically.
            // Own namespace for the module-namespace shadow lint (#376,
            // #361). Cloned up front: the check calls below take `&mut self`.
            let module_ns: String = self.ns_of.get(path).cloned().unwrap_or_default();
            let (mut checked, pending_key): (CheckResult, Option<String>) = if self.use_cache {
                // Dep-aware hit: the precomputed key covers source + true
                // transitive deps. Absent key (unresolvable) misses.
                let pending = module_keys.get(path).cloned();
                match pending {
                    Some(key) => match cache::read_cached(&key) {
                        Some(cached) => {
                            self.cache_hits += 1;
                            let mut funcs = std::mem::take(&mut self.funcs);
                            funcs.extend(cached.funcs.into_owned());
                            let mut structs = std::mem::take(&mut self.structs);
                            structs.extend(cached.structs.into_owned());
                            let mut aliases = std::mem::take(&mut self.aliases);
                            aliases.extend(cached.aliases.into_owned());
                            let mut enums = std::mem::take(&mut self.enums);
                            enums.extend(cached.enums.into_owned());
                            (
                                CheckResult {
                                    errors: cached.diags,
                                    bindings: cached.bindings.into_owned(),
                                    funcs,
                                    structs,
                                    aliases,
                                    enums,
                                    try_resolutions: HashMap::new(),
                                    try_converts: HashMap::new(),
                                    link_libs: Vec::new(),
                                    const_bindings: HashMap::new(),
                                    pub_bindings: cached.pub_bindings.into_owned(),
                                    pub_funcs: cached.pub_funcs.into_owned(),
                                    pub_structs: cached.pub_structs.into_owned(),
                                    pub_aliases: cached.pub_aliases.into_owned(),
                                    pub_enums: cached.pub_enums.into_owned(),
                                },
                                None,
                            )
                        }
                        None => {
                            self.cache_misses += 1;
                            (
                                check_program_with_module_ns(
                                    &program,
                                    self.bindings.clone(),
                                    std::mem::take(&mut self.funcs),
                                    std::mem::take(&mut self.structs),
                                    std::mem::take(&mut self.aliases),
                                    std::mem::take(&mut self.enums),
                                    &module_ns,
                                ),
                                Some(key),
                            )
                        }
                    },
                    // Unresolvable key (missing source, cycle): check without
                    // storing (nothing to key the entry by).
                    None => {
                        self.cache_misses += 1;
                        (
                            check_program_with_module_ns(
                                &program,
                                self.bindings.clone(),
                                std::mem::take(&mut self.funcs),
                                std::mem::take(&mut self.structs),
                                std::mem::take(&mut self.aliases),
                                std::mem::take(&mut self.enums),
                                &module_ns,
                            ),
                            None,
                        )
                    }
                }
            } else {
                self.cache_misses += 1;
                (
                    check_program_with_module_ns(
                        &program,
                        self.bindings.clone(),
                        std::mem::take(&mut self.funcs),
                        std::mem::take(&mut self.structs),
                        std::mem::take(&mut self.aliases),
                        std::mem::take(&mut self.enums),
                        &module_ns,
                    ),
                    None,
                )
            };
            let has_errors = checked
                .errors
                .iter()
                .any(|e| e.severity == zz_frontend::diag::Severity::Error);
            // Restore seeds BEFORE error enrichment (it reads the seeds).
            // Each checked table splits into seed-owned entries (keys known
            // before the check) and module-owned entries. Seeds stay in
            // place (`extract_if` pulls only the module-owned entries out),
            // so no rehash of the ~1k seed entries per module — the old
            // partition-into-two-fresh-maps rebuilt them every time.
            // On error the module contributes nothing, exactly as
            // before (seeds were never mutated: registration is insert-only
            // under namespace-qualified keys).
            let mut checked_funcs = std::mem::take(&mut checked.funcs);
            let own_funcs: HashMap<String, FuncSig> = checked_funcs
                .extract_if(|k, _| !self.seed_func_keys.contains(k))
                .collect();
            let seeded_funcs = checked_funcs;
            let mut checked_structs = std::mem::take(&mut checked.structs);
            let own_structs: HashMap<String, StructSig> = checked_structs
                .extract_if(|k, _| !self.seed_struct_keys.contains(k))
                .collect();
            let seeded_structs = checked_structs;
            let mut checked_aliases = std::mem::take(&mut checked.aliases);
            let own_aliases: HashMap<String, AliasSig> = checked_aliases
                .extract_if(|k, _| !self.seed_alias_keys.contains(k))
                .collect();
            let seeded_aliases = checked_aliases;
            let mut checked_enums = std::mem::take(&mut checked.enums);
            let own_enums: HashMap<String, EnumSig> = checked_enums
                .extract_if(|k, _| !self.seed_enum_keys.contains(k))
                .collect();
            let seeded_enums = checked_enums;
            self.funcs = seeded_funcs;
            self.structs = seeded_structs;
            self.aliases = seeded_aliases;
            self.enums = seeded_enums;
            // Store the miss (own-only maps + pubs + diags, all borrowed —
            // zero copy before the tail moves them below).
            if let Some(key) = pending_key.as_deref() {
                cache::write_cached(
                    key,
                    &cache::CachedModule {
                        funcs: std::borrow::Cow::Borrowed(&own_funcs),
                        pub_funcs: std::borrow::Cow::Borrowed(&checked.pub_funcs),
                        structs: std::borrow::Cow::Borrowed(&own_structs),
                        pub_structs: std::borrow::Cow::Borrowed(&checked.pub_structs),
                        aliases: std::borrow::Cow::Borrowed(&own_aliases),
                        pub_aliases: std::borrow::Cow::Borrowed(&checked.pub_aliases),
                        enums: std::borrow::Cow::Borrowed(&own_enums),
                        pub_enums: std::borrow::Cow::Borrowed(&checked.pub_enums),
                        bindings: std::borrow::Cow::Borrowed(&checked.bindings),
                        pub_bindings: std::borrow::Cow::Borrowed(&checked.pub_bindings),
                        diags: checked.errors.clone(),
                    },
                );
            }
            let mut diags = checked.errors;

            // Same-file alias imports (#380) always read as unused: references
            // through the alias head rewrite to the canonical spelling before
            // checking, but the import statement still loads the file and feeds
            // the alias mapping — removing it would break the load. Drop that
            // false positive (only when this module actually requested the
            // alias spelling).
            diags.retain(|d| {
                if d.severity != zz_frontend::diag::Severity::Warning {
                    return true;
                }
                let Some(alias) = d
                    .message
                    .strip_prefix("unused import `")
                    .and_then(|s| s.strip_suffix('`'))
                else {
                    return true;
                };
                !self
                    .ns_aliases
                    .contains_key(&(path.clone(), alias.to_string()))
            });
            // Dependency style-warning collapse (#387): a registry dep's
            // lints (shadow-namespace, unused-*, dead-mutation, ...) are
            // true positives the consumer can neither fix nor silence —
            // every downstream build would carry them forever, burying
            // first-party lints and breaking any future --deny-warnings
            // gate. Errors still propagate (they break the build);
            // warnings collapse to one `help` note keeping file
            // attribution. Runs after the cache write (cached diags stay
            // raw) and after the #380 false-positive filter above (only
            // genuine warnings collapse) so each consumer filters by its
            // own root — including cache-hit loads, which flow through
            // here too.
            if !is_first_party(self.root.as_deref(), path) {
                let warned: Vec<(String, Option<Span>)> = diags
                    .iter()
                    .filter(|d| d.severity == zz_frontend::diag::Severity::Warning)
                    .map(|d| (d.message.clone(), d.span))
                    .collect();
                if !warned.is_empty() {
                    diags.retain(|d| d.severity != zz_frontend::diag::Severity::Warning);
                    let ns = self.ns_of.get(path).cloned().unwrap_or_default();
                    let n = warned.len();
                    let (first_msg, first_span) = &warned[0];
                    diags.push(
                        zz_frontend::diag::warning_at(
                            format!(
                                "{n} style warning{} inside dependency `{ns}` hidden",
                                if n == 1 { "" } else { "s" }
                            ),
                            first_span.unwrap_or(Span::new(0, 0)),
                        )
                        .with_severity(zz_frontend::diag::Severity::Help)
                        .with_note(format!(
                            "check that package directly to see them (first: {first_msg})"
                        )),
                    );
                }
            }
            if has_errors {
                // Upgrade generic "undefined variable / unknown struct / no
                // field" errors into targeted "this item is private — add
                // `pub`" diagnostics whenever the reference hits an item that
                // exists but was not exported.
                let private_names: Vec<String> = diags
                    .iter_mut()
                    .filter_map(|d| self.enrich_private(d))
                    .collect();
                // When the root cause is a private item, prune cascading
                // noise: the `undefined variable `ns`` hop and the
                // "unused import" warning for the same namespace.
                if !private_names.is_empty() {
                    let prefixes: HashSet<String> = private_names
                        .iter()
                        .filter_map(|n| n.split('.').next().map(str::to_string))
                        .collect();
                    diags.retain(|d| {
                        !prefixes.iter().any(|ns| {
                            d.message == format!("undefined variable `{ns}`")
                                || d.message == format!("unused import `{ns}`")
                        })
                    });
                }
                // When the root cause is a failed dependency, collapse the
                // cascade: every `ns` / `ns.item` follow-on becomes one
                // targeted error naming the broken dependency (#274). The
                // dependency's own failure is already reported at its source.
                let own_ns: String = self.ns_of.get(path).cloned().unwrap_or_default();
                let mut collapsed: Vec<String> = Vec::new();
                diags.retain(
                    |d| match Self::failed_dep_ns(&failed_ns, &own_ns, &d.message) {
                        Some(ns) => {
                            if !collapsed.contains(&ns) {
                                collapsed.push(ns);
                            }
                            false
                        }
                        None => true,
                    },
                );
                collapsed.sort();
                for ns in &collapsed {
                    let dep_path = self
                        .namespaces
                        .get(ns)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| ns.clone());
                    diags.push(error_at(
                        format!(
                            "cannot use `{ns}`: dependency `{dep_path}` failed to check\n\
                             hint: fix the errors in `{dep_path}` first"
                        ),
                        Span::new(0, 0),
                    ));
                }
                self.errors.push(LoadError {
                    name: name.clone(),
                    source: source.clone(),
                    diags,
                });
                // Record the failure BEFORE the next module is processed:
                // importers come later in `order` and collapse references
                // to this namespace into one targeted error (#274).
                if let Some(own) = self.ns_of.get(path).cloned() {
                    failed_ns.insert(own);
                }
            } else {
                // Propagate warnings (even if there are no hard errors)
                // so they can be displayed to the user. Drop the `unused
                // import` warning for a failed dependency (#274): the import
                // is unused only because the dependency contributed nothing.
                if !failed_ns.is_empty() {
                    let own_ns: String = self.ns_of.get(path).cloned().unwrap_or_default();
                    diags
                        .retain(|d| Self::failed_dep_ns(&failed_ns, &own_ns, &d.message).is_none());
                }
                if !diags.is_empty() {
                    self.errors.push(LoadError {
                        name: name.clone(),
                        source: source.clone(),
                        diags,
                    });
                }
                // Only propagate pub items to the cross-module seed.
                // Resolved pub versions move over the seeds (was `.clone()`);
                // key sets grow by exactly the new pubs (O(pubs) total).
                self.seed_func_keys
                    .extend(checked.pub_funcs.keys().cloned());
                self.seed_struct_keys
                    .extend(checked.pub_structs.keys().cloned());
                self.seed_alias_keys
                    .extend(checked.pub_aliases.keys().cloned());
                self.seed_enum_keys
                    .extend(checked.pub_enums.keys().cloned());
                // Track all items for the entry file / runtime.
                // Seeds are contributed here once (first success only —
                // prior modules all errored, so seeds are still just
                // stdlib/plugins): one O(seed) clone total instead of one
                // per module. Runs BEFORE pubs extend the seeds below.
                if !self.contributed_seeds {
                    self.contributed_seeds = true;
                    self.all_funcs
                        .extend(self.funcs.iter().map(|(k, v)| (k.clone(), v.clone())));
                    self.all_structs
                        .extend(self.structs.iter().map(|(k, v)| (k.clone(), v.clone())));
                    self.all_aliases
                        .extend(self.aliases.iter().map(|(k, v)| (k.clone(), v.clone())));
                    self.all_enums
                        .extend(self.enums.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                self.bindings.extend(checked.pub_bindings);
                self.funcs.extend(checked.pub_funcs);
                self.structs.extend(checked.pub_structs);
                self.aliases.extend(checked.pub_aliases);
                self.enums.extend(checked.pub_enums);
                // `own_*` (module pubs + privates) move into `all_*` after
                // the seeds, so overlapping keys overwrite with the same
                // (unresolved) values as the old `checked.funcs` move.
                self.all_bindings.extend(checked.bindings);
                self.all_funcs.extend(own_funcs);
                self.all_structs.extend(own_structs);
                self.all_aliases.extend(own_aliases);
                self.all_enums.extend(own_enums);

                // Handle `pub import` re-exports: for each `pub import ns` in
                // this module, copy the re-exported namespace's pub functions
                // and bindings into the current module's namespace in the
                // cross-module seed.
                // Structs cannot be re-exported (identity-based types:
                // Type::Struct("a.X") != Type::Struct("b.X")). Fail loudly
                // at the re-export site instead of silently dropping (#230).
                if let Some(module_ns) = self.ns_of.get(path).cloned() {
                    for stmt in &program.stmts {
                        if let Stmt::Import {
                            path: imp_path,
                            alias,
                            pub_: true,
                            ..
                        } = stmt
                        {
                            let reexport_ns = alias
                                .as_deref()
                                .or_else(|| imp_path.last().map(|s| s.as_str()))
                                .unwrap_or("");
                            let prefix = format!("{}.", reexport_ns);
                            if let Some(offender) = self
                                .structs
                                .keys()
                                .chain(self.all_structs.keys())
                                .find(|k| k.starts_with(&prefix))
                                .map(|k| k[prefix.len()..].to_string())
                            {
                                self.errors.push(LoadError {
                                    name: name.clone(),
                                    source: source.clone(),
                                    diags: vec![error_at(
                                        format!(
                                            "`pub import {reexport_ns}` cannot re-export struct `{offender}` (struct re-exports unsupported)\n\
                                             hint: import `{reexport_ns}` directly instead of through `{module_ns}`"
                                        ),
                                        Span::new(0, 0),
                                    )],
                                });
                                continue;
                            }
                            let new_prefix = format!("{}.{reexport_ns}.", module_ns);
                            let seed_b = self.bindings.clone();
                            for (k, v) in &seed_b {
                                if k.starts_with(&prefix) {
                                    let new_key = format!("{}{}", new_prefix, &k[prefix.len()..]);
                                    self.bindings.insert(new_key.clone(), v.clone());
                                    self.all_bindings.insert(new_key, v.clone());
                                }
                            }
                            let seed_f = self.funcs.clone();
                            for (k, v) in &seed_f {
                                if k.starts_with(&prefix) {
                                    let new_key = format!("{}{}", new_prefix, &k[prefix.len()..]);
                                    self.funcs.insert(new_key.clone(), v.clone());
                                    self.all_funcs.insert(new_key, v.clone());
                                }
                            }
                        }
                    }
                }
            }
            programs.push(program);
        }

        let import_aliases =
            collect_import_aliases(&programs, &self.order, &self.ns_aliases, &self.funcs);
        // #503: bare references to pure-ZZ stdlib helpers need their module
        // in the compiled closure (see `collect_bare_purezz_modules`).
        for module in collect_bare_purezz_modules(&programs, &self.funcs, &self.natives) {
            self.stdlib_modules.insert(module);
        }
        LoadResult {
            programs,
            files,
            funcs: self.all_funcs,
            bindings: self.all_bindings,
            structs: self.all_structs,
            aliases: self.all_aliases,
            enums: self.all_enums,
            natives: self.natives,
            consts: self.selected_consts,
            errors: self.errors,
            stdlib_aliases: self.stdlib_aliases,
            import_aliases,
            stats: LoadStats {
                modules: self.order.len(),
                cache_hits: self.cache_hits,
                cache_misses: self.cache_misses,
                seed_funcs: self.seed_func_keys.len(),
            },
            stdlib_modules: {
                let mut v: Vec<String> = self.stdlib_modules.into_iter().collect();
                v.sort();
                v
            },
            stdlib_selectives: self.stdlib_selectives,
            stdlib_wildcards: self.stdlib_wildcards,
        }
    }
}

/// Convert an AST type annotation to a checker [`Type`], resolving aliases,
/// structs, and enums from the current seed tables. Used to compare a
/// collection signature against each member's checked `pub run` type.
fn ty_to_checker_type(
    ty: &Ty,
    structs: &HashMap<String, StructSig>,
    aliases: &HashMap<String, AliasSig>,
    enums: &HashMap<String, EnumSig>,
) -> Type {
    match &ty.kind {
        TyKind::Int => Type::Int,
        TyKind::Float => Type::Float,
        TyKind::Bool => Type::Bool,
        TyKind::Str => Type::Str,
        TyKind::Unit => Type::Unit,
        TyKind::Void => Type::Void,
        TyKind::Ptr { mutable, inner } => Type::Ptr {
            mutable: *mutable,
            inner: Box::new(ty_to_checker_type(inner, structs, aliases, enums)),
        },
        TyKind::Tuple(ts) => Type::Tuple(
            ts.iter()
                .map(|t| ty_to_checker_type(t, structs, aliases, enums))
                .collect(),
        ),
        TyKind::Option(t) => Type::Option(Box::new(ty_to_checker_type(t, structs, aliases, enums))),
        TyKind::Result(a, b) => Type::Result(
            Box::new(ty_to_checker_type(a, structs, aliases, enums)),
            Box::new(ty_to_checker_type(b, structs, aliases, enums)),
        ),
        TyKind::Func(ps, ret) => Type::Func(
            ps.iter()
                .map(|t| ty_to_checker_type(t, structs, aliases, enums))
                .collect(),
            Box::new(ty_to_checker_type(ret, structs, aliases, enums)),
        ),
        TyKind::Array(t) => Type::Array(Box::new(ty_to_checker_type(t, structs, aliases, enums))),
        TyKind::Dict(k, v) => Type::Dict(
            Box::new(ty_to_checker_type(k, structs, aliases, enums)),
            Box::new(ty_to_checker_type(v, structs, aliases, enums)),
        ),
        TyKind::Union(ts) => Type::Union(
            ts.iter()
                .map(|t| ty_to_checker_type(t, structs, aliases, enums))
                .collect(),
        ),
        TyKind::Named(name, args) => {
            let converted_args: Vec<Type> = args
                .iter()
                .map(|t| ty_to_checker_type(t, structs, aliases, enums))
                .collect();
            if name == "bytes" && args.is_empty() {
                return Type::Bytes;
            }
            if let Some(key) = find_named_key(aliases.keys(), name) {
                let alias = &aliases[&key];
                if alias.generics.len() == converted_args.len() {
                    let subs: HashMap<String, Type> =
                        alias.generics.iter().cloned().zip(converted_args).collect();
                    return zz_checker::subst_type(&alias.target, &subs);
                }
                return alias.target.clone();
            }
            if let Some(key) = find_named_key(structs.keys(), name) {
                return Type::Struct(key, converted_args);
            }
            if let Some(key) = find_named_key(enums.keys(), name) {
                return Type::Enum(key, converted_args);
            }
            Type::Named(name.clone())
        }
    }
}

/// Exact key, else a unique `*.name` suffix match (mirrors the checker's
/// canonical-name lookup for unqualified struct/enum/alias spellings).
fn find_named_key<'a>(keys: impl Iterator<Item = &'a String>, name: &str) -> Option<String> {
    let suffix = format!(".{name}");
    let mut unique: Option<String> = None;
    for key in keys {
        if key == name {
            return Some(key.clone());
        }
        if key.ends_with(&suffix) {
            if unique.is_some() {
                return None;
            }
            unique = Some(key.clone());
        }
    }
    unique
}

fn expected_param_list(ty: &Type) -> String {
    let Type::Func(params, _) = ty else {
        return ty.to_string();
    };
    let names: Vec<String> = if params.len() == 1 {
        vec!["args".to_string()]
    } else {
        (0..params.len()).map(|i| format!("a{i}")).collect()
    };
    params
        .iter()
        .zip(names)
        .map(|(t, n)| format!("{n}: {t}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn expected_ret(ty: &Type) -> String {
    match ty {
        Type::Func(_, ret) => ret.to_string(),
        other => other.to_string(),
    }
}

fn format_expected_sig(export: &str, ty: &Type) -> String {
    format!(
        "func {export}({}) -> {}",
        expected_param_list(ty),
        expected_ret(ty)
    )
}

fn format_found_sig(export: &str, sig: &FuncSig) -> String {
    let params = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("func {export}({params}) -> {}", sig.ret)
}

/// Render the synthesized collection module. Members are already sorted
/// by name and filtered to signature-matching entries.
fn render_collection_source(info: &CollectionModule, members: &[CollectionMember]) -> String {
    let export = &info.export;
    let mut out = String::new();
    out.push_str("// Generated from [collect.");
    out.push_str(&info.name);
    out.push_str("] in zz.toml. Do not edit.\n");
    out.push_str(&format!(
        "pub struct Entry {{\n    name: str,\n    {export}: {}\n}}\n\n",
        info.sig_source
    ));
    out.push_str("pub const ALL: [Entry] = [\n");
    for member in members {
        out.push_str(&format!(
            "    Entry {{ name: \"{}\", {export}: {}.{export} }},\n",
            member.name, member.ns
        ));
    }
    out.push_str("]\n\n");
    out.push_str("pub func all() -> [Entry] {\n    return ALL\n}\n\n");
    out.push_str("pub func get(name: str) -> Option<Entry> {\n");
    out.push_str("    for entry in ALL {\n");
    out.push_str("        if entry.name == name {\n");
    out.push_str("            return .some(entry)\n");
    out.push_str("        }\n    }\n    return .none\n}\n");
    out
}

/// Collect bare→qualified selective-import aliases from every module's
/// import statements: `import m(x)` → `x → m.x`,
/// `import m(x as y)` → `y → m.x`, `import m as a(x)` → `x → a.x`.
/// Wildcards (`import m(*)`) expand against the seed's `ns.*` keys.
/// First registration wins, mirroring seed shadowing. Only used as a
/// miss-only fallback for generic-function calls (generics have no
/// value binding); locals, seed entries and Decls take precedence.
///
/// Qualified spellings rewrite through same-file alias pairs (#380): an
/// `as`-spelled `tkeys.Foo` becomes the canonical `keys.Foo` the seeds
/// actually hold. `order[i]` is the importer of `programs[i]`.
fn collect_import_aliases(
    programs: &[Program],
    order: &[PathBuf],
    ns_aliases: &HashMap<(PathBuf, String), String>,
    seed_funcs: &HashMap<String, FuncSig>,
) -> HashMap<String, String> {
    // `programs[i]` was checked for `order[i]`; lengths stay in lockstep
    // by construction in `finish`.
    debug_assert_eq!(programs.len(), order.len());
    // Rewrite an alias-spelled qualified name through this importer's
    // same-file pairs; canonical and ordinary spellings pass through.
    let canonicalize = |importer: &PathBuf, qualified: String| -> String {
        match qualified.split_once('.') {
            Some((head, _)) => match ns_aliases.get(&(importer.clone(), head.to_string())) {
                Some(canon) => format!("{canon}.{}", &qualified[head.len() + 1..]),
                None => qualified,
            },
            None => qualified,
        }
    };
    let mut aliases = HashMap::new();
    for (program, importer) in programs.iter().zip(order.iter()) {
        for stmt in &program.stmts {
            let Stmt::Import {
                path, alias, items, ..
            } = stmt
            else {
                continue;
            };
            if items.is_empty() {
                continue;
            }
            let ns = alias
                .as_deref()
                .or_else(|| path.last().map(String::as_str))
                .unwrap_or("");
            if ns.is_empty() {
                continue;
            }
            for item in items {
                match item {
                    ImportItem::Named { name, alias, .. } => {
                        let target = alias.as_ref().unwrap_or(name);
                        aliases
                            .entry(target.clone())
                            .or_insert_with(|| canonicalize(importer, format!("{ns}.{name}")));
                    }
                    ImportItem::Wildcard { .. } => {
                        let prefix = format!("{ns}.");
                        for key in seed_funcs.keys() {
                            if let Some(bare) = key.strip_prefix(&prefix) {
                                if !bare.is_empty() && !bare.contains('.') {
                                    aliases.entry(bare.to_string()).or_insert(key.clone());
                                }
                            }
                        }
                        // Wildcard under an alias spelling: the seed holds
                        // the canonical namespace instead.
                        if let Some(canon) = ns_aliases.get(&(importer.clone(), ns.to_string())) {
                            let prefix = format!("{canon}.");
                            for key in seed_funcs.keys() {
                                if let Some(bare) = key.strip_prefix(&prefix) {
                                    if !bare.is_empty() && !bare.contains('.') {
                                        aliases.entry(bare.to_string()).or_insert(key.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    aliases
}

// Collect Ident/Path references inside a function (params included —
// conservative: a param shadowing a Decl name just blocks hoisting).
fn collect_refs_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Ident { name, .. } => {
            out.insert(name.clone());
        }
        Expr::Path { parts, .. } => {
            out.insert(parts.join("."));
            for p in parts {
                out.insert(p.clone());
            }
        }
        _ => {}
    }
    // Recurse through children generically via debug walk: enumerate
    // every nested Ident/Path by re-walking known containers.
    match e {
        Expr::Paren { expr: x, .. }
        | Expr::Unary { expr: x, .. }
        | Expr::Try { expr: x, .. }
        | Expr::Closure { body: x, .. } => collect_refs_expr(x, out),
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
            ..
        } => {
            collect_refs_expr(left, out);
            collect_refs_expr(right, out);
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_refs_expr(callee, out);
            for a in args {
                collect_refs_expr(a, out);
            }
            for (_, v) in named {
                collect_refs_expr(v, out);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_refs_expr(cond, out);
            collect_refs_block(then, out);
            if let Some(x) = els {
                collect_refs_expr(x, out);
            }
        }
        Expr::While { cond, body, .. } => {
            collect_refs_expr(cond, out);
            collect_refs_block(body, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_refs_expr(scrutinee, out);
            for a in arms {
                collect_refs_expr(&a.body, out);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            collect_refs_expr(value, out);
            collect_refs_block(then, out);
            if let Some(x) = els {
                collect_refs_expr(x, out);
            }
        }
        Expr::Block(b) => collect_refs_block(b, out),
        Expr::Array { elems, .. } | Expr::Tuple { items: elems, .. } => {
            for x in elems {
                collect_refs_expr(x, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_refs_expr(k, out);
                collect_refs_expr(v, out);
            }
        }
        Expr::Fmt { parts, .. } => {
            for p in parts {
                if let zz_frontend::ast::FmtPart::Expr(x, _) = p {
                    collect_refs_expr(x, out);
                }
            }
        }
        Expr::Field { obj, .. } => collect_refs_expr(obj, out),
        Expr::Index { obj, index, .. } => {
            collect_refs_expr(obj, out);
            collect_refs_expr(index, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_refs_expr(obj, out);
            if let Some(x) = start {
                collect_refs_expr(x, out);
            }
            if let Some(x) = end {
                collect_refs_expr(x, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_refs_expr(v, out);
            }
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_refs_expr(body, out);
            collect_refs_expr(iter, out);
            if let Some(f) = filter {
                collect_refs_expr(f, out);
            }
        }
        Expr::Variant { arg: Some(x), .. } => {
            collect_refs_expr(x, out);
        }
        _ => {}
    }
}
fn collect_refs_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        collect_refs_stmt(s, out);
    }
}
fn collect_refs_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Decl { value, .. } => collect_refs_expr(value, out),
        Stmt::Func { body, .. } => collect_refs_block(body, out),
        Stmt::Return { value: Some(x), .. } => {
            collect_refs_expr(x, out);
        }
        Stmt::Expr(x) => collect_refs_expr(x, out),
        Stmt::Defer { expr: x, .. } => collect_refs_expr(x, out),
        Stmt::Assign { target, value, .. } | Stmt::CompoundAssign { target, value, .. } => {
            collect_refs_expr(target, out);
            collect_refs_expr(value, out);
        }
        Stmt::Destructure { value, .. } => collect_refs_expr(value, out),
        Stmt::For { iter, body, .. } => {
            collect_refs_expr(iter, out);
            collect_refs_block(body, out);
        }
        Stmt::Impl { methods, .. } => {
            for m in methods {
                collect_refs_stmt(m, out);
            }
        }
        _ => {}
    }
}

/// #503: std namespaces a program references *bare* through a pure-ZZ stdlib
/// helper — `str.repeat("ab", 2)` with no `import std.str` — and which must
/// therefore join the compiled closure.
///
/// The seed exposes every canonical `std.*` signature under its bare spelling
/// too, so the checker resolves such a call and `zz check` says nothing. The
/// helper's *body*, however, only exists in the closure that the VM and both
/// codegen backends compile (`zz_stdlib::stdlib_program_closure`), which is
/// otherwise driven by imports alone. That left the engines disagreeing: the
/// VM failed at run time with `undefined method`, the HIR backend merged every
/// reachable stdlib program and silently succeeded, and the chunk backend could
/// not resolve the call at all.
///
/// Natives are always registered, so a bare native (`vec.push`) is left alone.
/// Imprecision is harmless here — naming a namespace costs a little startup
/// compile, never correctness — which is why a plain reference scan suffices
/// and no type information is needed.
fn collect_bare_purezz_modules(
    programs: &[Program],
    funcs: &HashMap<String, FuncSig>,
    natives: &HashMap<String, NativeEntry>,
) -> HashSet<String> {
    let mut out = HashSet::new();
    for program in programs {
        let mut refs = HashSet::new();
        for stmt in &program.stmts {
            collect_refs_stmt(stmt, &mut refs);
        }
        for name in refs {
            // `ns.func`: a real std namespace head and a non-dotted tail.
            let Some((head, tail)) = name.split_once('.') else {
                continue;
            };
            if tail.is_empty() || tail.contains('.') || !STDLIB_MODULES.contains(&head) {
                continue;
            }
            // The canonical signature must exist: a user function, or a
            // module named like a std namespace, is never seeded under `std.`.
            if !funcs.contains_key(&format!("std.{name}")) {
                continue;
            }
            // Natives (bare or canonical spelling) are registered
            // unconditionally; only a missing ZZ body needs the closure.
            if natives.contains_key(&name) || natives.contains_key(&format!("std.{name}")) {
                continue;
            }
            out.insert(head.to_string());
        }
    }
    out
}

/// Rewrite a module's AST so its top-level definitions are namespaced:
/// - top-level `func foo` becomes `func ns.foo`,
/// - top-level `x := ...` becomes `x := ...` with binding name `ns.x`,
/// - references to those names (`Expr::Ident`) become `Expr::Path([ns, name])`
///   unless shadowed by a local binding,
/// - same-file alias heads (`tkeys` bound alongside `keys`, #380) rewrite
///   through the canonical namespace from `ns_aliases`.
fn namespace_program(program: &mut Program, ns: &str, ns_aliases: &HashMap<String, String>) {
    let mut top = HashSet::new();
    let mut top_bindings = HashSet::new();
    for stmt in &program.stmts {
        match stmt {
            Stmt::Func { name, .. }
            | Stmt::Struct { name, .. }
            | Stmt::TypeAlias { name, .. }
            | Stmt::Enum { name, .. } => {
                top.insert(name.join("."));
            }
            Stmt::Decl { name, .. } => {
                top.insert(name.name.clone());
                top_bindings.insert(name.name.clone());
            }
            _ => {}
        }
    }
    let mut rw = rewrite::Rewriter::new(ns, &top, &top_bindings);
    rw.ns_aliases = ns_aliases.clone();
    // Imported namespace heads resolve on their own; the Call rewriter
    // must leave `alias.func(...)` paths untouched.
    for stmt in &program.stmts {
        if let Stmt::Import { path, alias, .. } = stmt {
            if let Some(a) = alias {
                rw.imports.insert(a.clone());
            } else if let Some(last) = path.last() {
                rw.imports.insert(last.clone());
            }
        }
    }
    for stmt in &mut program.stmts {
        rw.rewrite_stmt(stmt);
    }
    hoist_forward_callable_funcs(program, ns);
}

/// Hoist top-level `func` definitions that are safe to define early so a
/// top-level statement may call a function defined later in the same file
/// (`println(game_end(3))` before `func game_end`, #362). The checker
/// already resolves such calls (Pass 1c registers all signatures
/// upfront), but the VM executes statements in source order, so without
/// hoisting the call runs before `MakeFunc` defines `ns.func` and fails
/// with `undefined variable <ns>`.
///
/// Only hoisted when safe, and placed after imports. A function whose
/// body references a top-level binding (`Decl` or `Destructure`) must stay
/// in source order, since defining or checking it before the binding
/// breaks capture and check order. A function that transitively calls a
/// non-hoisted function also stays put. Hoisted functions land after all
/// imports, because the checker populates import aliases in statement
/// order, but before everything else. Pure functions of their params and
/// mutual recursion between hoisted functions move stably, while programs
/// hoisting cannot help keep exactly today's behavior.
fn hoist_forward_callable_funcs(program: &mut Program, ns: &str) {
    use zz_frontend::ast::Pattern;
    // Top-level binding names: `Decl`s post-rewrite ("ns.x") plus their
    // bare forms, and `Destructure` bindings (never namespaced — the
    // rewriter leaves patterns alone, and refs to them stay bare).
    let mut decl_names: HashSet<String> = HashSet::new();
    fn collect_pat_binds(p: &Pattern, out: &mut HashSet<String>) {
        match p {
            Pattern::Binding { name } => {
                out.insert(name.name.clone());
            }
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                for q in pats {
                    collect_pat_binds(q, out);
                }
            }
            Pattern::Variant { arg: Some(a), .. } => collect_pat_binds(a, out),
            _ => {}
        }
    }
    // Index of each top-level func in stmt order + its bare name.
    let mut funcs: Vec<(usize, String)> = Vec::new();
    for (i, stmt) in program.stmts.iter().enumerate() {
        match stmt {
            Stmt::Decl { name, .. } => {
                decl_names.insert(name.name.clone());
                if let Some(bare) = name.name.rsplit('.').next() {
                    decl_names.insert(bare.to_string());
                }
            }
            Stmt::Destructure { pat, .. } => collect_pat_binds(pat, &mut decl_names),
            Stmt::Func { name, .. } => {
                // Post-rewrite: ["ns.foo"] (dot inside one element).
                let full = name.join(".");
                let bare = full
                    .strip_prefix(&format!("{ns}."))
                    .unwrap_or(&full)
                    .to_string();
                funcs.push((i, bare));
            }
            _ => {}
        }
    }
    if funcs.is_empty() {
        return;
    }
    // Per-func refs (body + defaults).
    let mut refs: Vec<HashSet<String>> = Vec::with_capacity(funcs.len());
    for (stmt_idx, _) in &funcs {
        let mut out = HashSet::new();
        if let Stmt::Func { params, body, .. } = &program.stmts[*stmt_idx] {
            for p in params {
                if let Some(d) = &p.default {
                    collect_refs_expr(d, &mut out);
                }
            }
            collect_refs_block(body, &mut out);
        }
        refs.push(out);
    }
    // Fixpoint: hoistable = no Decl refs + only calls hoistable funcs.
    let mut hoistable: Vec<bool> = refs
        .iter()
        .map(|r| {
            !r.iter().any(|n| {
                decl_names.contains(n)
                    || n.rsplit('.')
                        .next()
                        .is_some_and(|b| decl_names.iter().any(|d| d == b))
            })
        })
        .collect();
    loop {
        let mut changed = false;
        for i in 0..funcs.len() {
            if !hoistable[i] {
                continue;
            }
            let calls_blocked = refs[i].iter().any(|n| {
                let bare = n.rsplit('.').next().unwrap_or(n);
                funcs
                    .iter()
                    .enumerate()
                    .any(|(j, (_, b))| b == bare && !hoistable[j])
            });
            if calls_blocked {
                hoistable[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    if !hoistable.iter().any(|b| *b) {
        return;
    }
    let hoist_idx: HashSet<usize> = funcs
        .iter()
        .enumerate()
        .filter(|(k, _)| hoistable[*k])
        .map(|(_, (si, _))| *si)
        .collect();
    // Imports stay first (the checker populates import aliases in
    // statement order), hoisted funcs follow, everything else keeps its
    // relative order.
    let mut imports = Vec::new();
    let mut hoisted = Vec::new();
    let mut rest = Vec::with_capacity(program.stmts.len());
    for (i, stmt) in program.stmts.drain(..).enumerate() {
        if matches!(stmt, Stmt::Import { .. }) {
            imports.push(stmt);
        } else if hoist_idx.contains(&i) {
            hoisted.push(stmt);
        } else {
            rest.push(stmt);
        }
    }
    program
        .stmts
        .extend(imports.into_iter().chain(hoisted).chain(rest));
}
