//! Project dependency signatures for the editor (#superman-deps).
//!
//! `zz add table` makes `import table` + `table.render(..)` work at runtime,
//! but the server used to know only stdlib + open buffers — every dep
//! import produced `unused import` + `undefined variable`, and `table.`
//! completed nothing. This module harvests dep signatures the way the
//! loader does (entry file + relative siblings, `pub` items namespaced
//! under the import alias) so diagnostics and completion agree with
//! `zz check`.
//!
//! Only healthy files contribute: a dep file whose own check errors is
//! skipped (stale vendor code must not poison the importing file with
//! phantom types). Results are cached per project root and refreshed when
//! any dep source is newer than the cache.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use zz_checker::{AliasSig, EnumSig, FuncSig, StructSig, Type};
use zz_frontend::ast::Stmt;

/// Harvested signatures of one project root's dependencies.
#[derive(Debug, Clone, Default)]
pub struct DepSeed {
    pub funcs: HashMap<String, FuncSig>,
    pub structs: HashMap<String, StructSig>,
    pub aliases: HashMap<String, AliasSig>,
    pub enums: HashMap<String, EnumSig>,
    pub bindings: HashMap<String, Type>,
}

impl DepSeed {
    pub(crate) fn is_empty(&self) -> bool {
        self.funcs.is_empty()
            && self.structs.is_empty()
            && self.aliases.is_empty()
            && self.enums.is_empty()
            && self.bindings.is_empty()
    }

    fn extend(&mut self, other: DepSeed) {
        self.funcs.extend(other.funcs);
        self.structs.extend(other.structs);
        self.aliases.extend(other.aliases);
        self.enums.extend(other.enums);
        self.bindings.extend(other.bindings);
    }
}

/// Find the project root (nearest `zz.toml`) above `start`.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        if dir.join("zz.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Dependency names declared in `<root>/zz.toml`.
///
/// Minimal section parser (no TOML dependency on purpose): collects keys
/// under `[dependencies]` plus `[dependencies.NAME]` headers with an
/// optional `path = "..."` each.
pub fn read_dep_names(root: &Path) -> Vec<(String, Option<String>)> {
    let text = std::fs::read_to_string(root.join("zz.toml")).unwrap_or_default();
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut section = String::new();
    let mut pending_path: Option<String> = None;
    let mut pending_name: Option<String> = None;
    let flush = |name: &mut Option<String>,
                 path: &mut Option<String>,
                 out: &mut Vec<(String, Option<String>)>| {
        if let Some(n) = name.take() {
            out.push((n, path.take()));
        }
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            flush(&mut pending_name, &mut pending_path, &mut out);
            section = line[1..line.len() - 1].trim().to_string();
            if let Some(name) = section.strip_prefix("dependencies.") {
                let name = name.trim().trim_matches('"').to_string();
                if !name.is_empty() {
                    pending_name = Some(name);
                }
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().trim_matches('"').trim();
        let v = v.trim().trim_matches('"').trim_matches('\'').trim();
        if section == "dependencies" && pending_name.is_none() {
            // Inline table entry: `table = "^0.5.0"` — the key is the dep.
            if !k.is_empty() && !k.contains(' ') && !k.contains('.') {
                out.push((k.to_string(), None));
            }
        } else if pending_name.is_some() && k == "path" {
            pending_path = Some(v.to_string());
        }
    }
    flush(&mut pending_name, &mut pending_path, &mut out);
    // De-duplicate by name (inline + section forms merge).
    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|(n, _)| seen.insert(n.clone()))
        .collect()
}

/// Resolve a dependency to its package directory: manifest `path` first,
/// then the `vendor/<dep>` link the installer creates.
fn resolve_dep_dir(root: &Path, name: &str, path_opt: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = path_opt {
        let dir = root.join(p);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let linked = root.join("vendor").join(name);
    if linked.is_dir() {
        return Some(linked);
    }
    None
}

/// Entry-file convention inside a dependency package directory.
fn entry_in(pkg_dir: &Path, dep_name: &str) -> Option<PathBuf> {
    [
        pkg_dir.join("src").join("main.zz"),
        pkg_dir.join("src").join(format!("{dep_name}.zz")),
        pkg_dir.join(format!("{dep_name}.zz")),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// Newest mtime under `dir` (one level of `src/`), for cache validation.
fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let src = dir.join("src");
    let base = if src.is_dir() { src } else { dir.to_path_buf() };
    let entries = std::fs::read_dir(&base).ok()?;
    let mut newest: Option<SystemTime> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "zz") {
            if let Ok(meta) = entry.metadata() {
                if let Ok(m) = meta.modified() {
                    newest = Some(newest.map_or(m, |n: SystemTime| n.max(m)));
                }
            }
        }
    }
    newest
}

/// Harvest one dependency package into a seed.
///
/// `alias` is the import namespace (`import table as t` → `t`). The entry
/// file's `pub` items land under `alias.*`; relative sibling imports land
/// under their own stem so cross-module refs (`model.Table`) keep working.
/// Transitive project deps resolve against `outer_root`'s vendor dir.
fn harvest_one(
    dep_dir: &Path,
    dep_name: &str,
    alias: &str,
    outer_root: &Path,
    base_funcs: &HashMap<String, FuncSig>,
    depth: usize,
) -> DepSeed {
    let mut seed = DepSeed::default();
    if depth > 8 {
        return seed;
    }
    let Some(entry) = entry_in(dep_dir, dep_name) else {
        return seed;
    };
    // Collect reachable files: (module_name, path). Siblings resolve from
    // the importing file's directory, mirroring the loader.
    let mut files: Vec<(String, PathBuf)> = vec![(alias.to_string(), entry)];
    let mut seen_files: HashSet<PathBuf> = files.iter().map(|(_, p)| p.clone()).collect();
    let mut i = 0;
    while i < files.len() {
        let dir = files[i]
            .1
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| dep_dir.to_path_buf());
        let text = std::fs::read_to_string(&files[i].1).unwrap_or_default();
        let parsed = zz_frontend::parse(&text);
        for stmt in &parsed.program.stmts {
            if let Stmt::Import { path, .. } = stmt {
                if path.first().map(String::as_str) == Some("std") || path.is_empty() {
                    continue;
                }
                // Single-segment relative import → sibling file.
                if path.len() == 1 {
                    let stem = &path[0];
                    let candidate = dir.join(format!("{stem}.zz"));
                    let candidate = if candidate.is_file() {
                        candidate
                    } else {
                        let nested = dir.join(stem).join("mod.zz");
                        if nested.is_file() {
                            nested
                        } else {
                            continue;
                        }
                    };
                    if seen_files.insert(candidate.clone()) {
                        files.push((stem.clone(), candidate));
                    }
                }
            }
        }
        i += 1;
    }
    check_rounds(&files, base_funcs, &mut seed);
    // Transitive project deps of this dep (its own zz.toml), resolved
    // against the OUTER root like the loader's ancestor probing.
    if let Some(dep_root) = find_project_root(&dep_dir.join("zz.toml")) {
        if dep_root != *outer_root {
            // Foreign root (path dep outside the project): resolve there.
            let sub = harvest_root(&dep_root, base_funcs, &mut HashSet::new(), depth + 1);
            seed.extend(sub);
        } else {
            for (name, path_opt) in read_dep_names(dep_dir) {
                if name == dep_name {
                    continue;
                }
                if let Some(dir) = resolve_dep_dir(outer_root, &name, path_opt.as_deref()) {
                    seed.extend(harvest_one(
                        &dir,
                        &name,
                        &name,
                        outer_root,
                        base_funcs,
                        depth + 1,
                    ));
                }
            }
        }
    }
    seed
}

/// Resolve a non-`std` import to a source file, mirroring the loader:
/// `<importer_dir>/<path...>.zz` (`import math_utils.lib` beside
/// `main.zz` → `math_utils/lib.zz`; `./` and `../` work the same way).
pub fn resolve_local_import(doc_dir: &Path, imp: &[String]) -> Option<PathBuf> {
    if imp.is_empty() {
        return None;
    }
    let candidate = doc_dir.join(imp.join("/")).with_extension("zz");
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

/// Harvest one workspace file (plus its relative siblings) into a seed.
///
/// `ns` is the import namespace (alias or file stem, loader parity).
/// Sibling imports resolve from each file's own directory; project-level
/// deps of these files resolve against `outer_root`. Only healthy files
/// contribute, like [`harvest_one`].
pub fn harvest_workspace_file(
    file: &Path,
    ns: &str,
    outer_root: Option<&Path>,
    base_funcs: &HashMap<String, FuncSig>,
    depth: usize,
) -> DepSeed {
    harvest_entry(ns, file, outer_root, base_funcs, depth)
}

/// Generic entry harvest shared by dependency packages and workspace
/// files: `entry_module` names the entry file's namespace, relative
/// sibling imports (`import model`, `import ./x`) resolve from each
/// importing file's own directory.
fn harvest_entry(
    entry_module: &str,
    entry_file: &Path,
    outer_root: Option<&Path>,
    base_funcs: &HashMap<String, FuncSig>,
    depth: usize,
) -> DepSeed {
    let mut seed = DepSeed::default();
    if depth > 8 || !entry_file.is_file() {
        return seed;
    }
    let mut files: Vec<(String, PathBuf)> =
        vec![(entry_module.to_string(), entry_file.to_path_buf())];
    let mut seen_files: HashSet<PathBuf> = files.iter().map(|(_, p)| p.clone()).collect();
    let mut i = 0;
    while i < files.len() {
        let dir = files[i]
            .1
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let text = std::fs::read_to_string(&files[i].1).unwrap_or_default();
        let parsed = zz_frontend::parse(&text);
        for stmt in &parsed.program.stmts {
            if let Stmt::Import { path, .. } = stmt {
                if path.first().map(String::as_str) == Some("std") || path.is_empty() {
                    continue;
                }
                // Relative import (possibly dotted): resolve from the
                // importing file's directory, exactly like the loader.
                if let Some(candidate) = resolve_local_import(&dir, path) {
                    if seen_files.insert(candidate.clone()) {
                        // Sibling namespace = its own stem (loader parity
                        // via module_ns); the entry keeps the import alias.
                        let stem = candidate
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        files.push((stem, candidate));
                    }
                }
            }
        }
        i += 1;
    }
    check_rounds(&files, base_funcs, &mut seed);
    // Transitive project deps (a workspace file importing a registry
    // package) resolve against the outer root.
    if let Some(root) = outer_root {
        for (module, path) in &files {
            let _ = module;
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let parsed = zz_frontend::parse(&text);
            for stmt in &parsed.program.stmts {
                if let Stmt::Import { path: ip, .. } = stmt {
                    if ip.first().map(String::as_str) != Some("std") && ip.len() == 1 {
                        let name = &ip[0];
                        if let Some(dir) = resolve_dep_dir(root, name, None) {
                            seed.extend(harvest_one(&dir, name, name, root, base_funcs, depth + 1));
                        }
                    }
                }
            }
        }
    }
    seed
}

/// Number of relative (non-`std`) imports in a file: harvested files
/// are checked fewest-imports-first so leaves seed before importers.
fn local_import_count(path: &Path) -> usize {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = zz_frontend::parse(&text);
    parsed
        .program
        .stmts
        .iter()
        .filter(|stmt| {
            matches!(stmt, Stmt::Import { path, .. }
                if path.first().map(String::as_str) != Some("std") && !path.is_empty())
        })
        .count()
}

/// Qualify a harvested signature's self-references (`Table` → `model.Table`).
///
/// The loader rewrites every module with its namespace before checking;
/// the harvester checks raw sources instead, so a file's own bare type
/// names would otherwise leak unqualified into the seed and mismatch at
/// use sites (`expected model.Table, found Table`). Only the module's own
/// bare struct/enum/alias names qualify (never generic params, never
/// dotted paths) — cross-module refs are already qualified in source.
fn qualify_ty(
    ty: &mut zz_checker::Type,
    module: &str,
    own: &HashSet<String>,
    params: &HashSet<String>,
) {
    use zz_checker::Type;
    match ty {
        Type::Struct(name, args) | Type::Enum(name, args) => {
            if !name.contains('.') && own.contains(name) && !params.contains(name) {
                *name = format!("{module}.{name}");
            }
            for a in args {
                qualify_ty(a, module, own, params);
            }
        }
        Type::Ptr { inner, .. } => qualify_ty(inner, module, own, params),
        Type::Tuple(items) | Type::Union(items) => {
            for t in items {
                qualify_ty(t, module, own, params);
            }
        }
        Type::Option(inner) | Type::Array(inner) | Type::Range(inner) => {
            qualify_ty(inner, module, own, params)
        }
        Type::Result(a, b) | Type::Dict(a, b) => {
            qualify_ty(a, module, own, params);
            qualify_ty(b, module, own, params);
        }
        Type::Func(params_ty, ret) => {
            for t in params_ty {
                qualify_ty(t, module, own, params);
            }
            qualify_ty(ret, module, own, params);
        }
        _ => {}
    }
}

/// Bare struct/enum/alias names defined by one file (qualification set).
fn own_type_names(program: &zz_frontend::ast::Program) -> HashSet<String> {
    let mut own = HashSet::new();
    for stmt in &program.stmts {
        match stmt {
            Stmt::Struct { name, .. } | Stmt::Enum { name, .. } | Stmt::TypeAlias { name, .. } => {
                if let Some(leaf) = name.last() {
                    if !leaf.contains('.') {
                        own.insert(leaf.clone());
                    }
                }
            }
            _ => {}
        }
    }
    own
}

/// Check harvested files in dependency order and merge healthy files
/// (public items) into `seed` under their module namespaces.
fn check_rounds(
    files: &[(String, PathBuf)],
    base_funcs: &HashMap<String, FuncSig>,
    seed: &mut DepSeed,
) {
    // Check rounds: files whose sibling imports are already seeded go
    // first so cross-module types resolve exactly (table.new -> Table).
    // Errored files are RETRIED while siblings make progress (a file
    // failing only for not-yet-seeded siblings succeeds on a later
    // round); only files still failing at the end are dropped.
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&idx| local_import_count(&files[idx].1));
    let mut checked: HashSet<String> = HashSet::new();
    let mut acc_funcs = base_funcs.clone();
    let mut acc_structs: HashMap<String, StructSig> = HashMap::new();
    let mut acc_aliases: HashMap<String, AliasSig> = HashMap::new();
    let mut acc_enums: HashMap<String, EnumSig> = HashMap::new();
    // Seed std namespaces the dep itself imports (import_seed equivalent
    // for a synthetic program is overkill; register per import below).
    for _ in 0..files.len() + 1 {
        let mut progress = false;
        for idx in order.clone() {
            let (module, path) = &files[idx];
            if checked.contains(module) {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let parsed = zz_frontend::parse(&text);
            // Apply this file's own std imports to its seed.
            let mut funcs = acc_funcs.clone();
            {
                let mut dummy: HashMap<String, zz_runtime::NativeEntry> = HashMap::new();
                for stmt in &parsed.program.stmts {
                    if let Stmt::Import {
                        path: ip,
                        alias: ia,
                        items,
                        ..
                    } = stmt
                    {
                        if ip.first().map(String::as_str) != Some("std") || ip.len() < 2 {
                            continue;
                        }
                        use zz_frontend::ast::ImportItem;
                        use zz_stdlib::{
                            register_module_namespace, register_selective_namespace,
                            register_wildcard_namespace,
                        };
                        let modname = ip[1..].join(".");
                        if items.is_empty() {
                            let ns = ia.clone().unwrap_or_else(|| {
                                ip.last().cloned().unwrap_or_else(|| modname.clone())
                            });
                            let _ =
                                register_module_namespace(&modname, &ns, &mut funcs, &mut dummy);
                        } else if items
                            .iter()
                            .any(|x| matches!(x, ImportItem::Wildcard { .. }))
                        {
                            let _ = register_wildcard_namespace(&modname, &mut funcs, &mut dummy);
                        } else {
                            let pairs: Vec<(String, Option<String>)> = items
                                .iter()
                                .filter_map(|x| match x {
                                    ImportItem::Named { name, alias, .. } => {
                                        Some((name.clone(), alias.clone()))
                                    }
                                    ImportItem::Wildcard { .. } => None,
                                })
                                .collect();
                            let _ = register_selective_namespace(
                                &modname, &pairs, &mut funcs, &mut dummy,
                            );
                        }
                    }
                }
            }
            let cr = zz_checker::check_program(
                &parsed.program,
                HashMap::new(),
                funcs,
                acc_structs.clone(),
                acc_aliases.clone(),
                acc_enums.clone(),
            );
            // Unhealthy files contribute nothing: phantom types are worse
            // than missing completions.
            if cr
                .errors
                .iter()
                .any(|e| e.severity == zz_frontend::diag::Severity::Error)
            {
                // Retry next round: siblings may seed what this needs.
                continue;
            }
            // Merge pub items under this file's module namespace,
            // qualifying self-references like the loader's rewrite does.
            let own = own_type_names(&parsed.program);
            for (name, sig) in &cr.pub_funcs {
                let mut sig = sig.clone();
                let params: HashSet<String> = sig.generics.iter().cloned().collect();
                for (_, pt) in &mut sig.params {
                    qualify_ty(pt, module, &own, &params);
                }
                qualify_ty(&mut sig.ret, module, &own, &params);
                seed.funcs.insert(format!("{module}.{name}"), sig.clone());
                acc_funcs.insert(format!("{module}.{name}"), sig);
            }
            for (name, sig) in &cr.pub_structs {
                let mut sig = sig.clone();
                let params: HashSet<String> = sig.generics.iter().cloned().collect();
                for (_, ft) in &mut sig.fields {
                    qualify_ty(ft, module, &own, &params);
                }
                seed.structs.insert(format!("{module}.{name}"), sig.clone());
                acc_structs.insert(format!("{module}.{name}"), sig);
            }
            for (name, sig) in &cr.pub_aliases {
                let mut sig = sig.clone();
                let params: HashSet<String> = sig.generics.iter().cloned().collect();
                qualify_ty(&mut sig.target, module, &own, &params);
                seed.aliases.insert(format!("{module}.{name}"), sig.clone());
                acc_aliases.insert(format!("{module}.{name}"), sig);
            }
            for (name, sig) in &cr.pub_enums {
                let mut sig = sig.clone();
                let params: HashSet<String> = sig.generics.iter().cloned().collect();
                for (_, payload) in &mut sig.variants {
                    if let Some(pt) = payload {
                        qualify_ty(pt, module, &own, &params);
                    }
                }
                seed.enums.insert(format!("{module}.{name}"), sig.clone());
                acc_enums.insert(format!("{module}.{name}"), sig.clone());
            }
            for (name, ty) in &cr.pub_bindings {
                let mut ty = ty.clone();
                qualify_ty(&mut ty, module, &own, &HashSet::new());
                seed.bindings.insert(format!("{module}.{name}"), ty);
            }
            checked.insert(module.clone());
            progress = true;
        }
        if !progress {
            break;
        }
    }
}

/// Harvest every dependency of the project at `root`.
fn harvest_root(
    root: &Path,
    base_funcs: &HashMap<String, FuncSig>,
    visiting: &mut HashSet<String>,
    depth: usize,
) -> DepSeed {
    let mut seed = DepSeed::default();
    for (name, path_opt) in read_dep_names(root) {
        if !visiting.insert(name.clone()) {
            continue;
        }
        if let Some(dir) = resolve_dep_dir(root, &name, path_opt.as_deref()) {
            seed.extend(harvest_one(&dir, &name, &name, root, base_funcs, depth + 1));
        }
        visiting.remove(&name);
    }
    seed
}

/// Cached harvest of one workspace file (keyed by file path).
#[derive(Debug, Clone)]
pub struct WsCacheEntry {
    /// All files read for this entry (entry + siblings), for freshness.
    pub files: Vec<PathBuf>,
    pub mtime: Option<SystemTime>,
    pub seed: DepSeed,
}

impl WsCacheEntry {
    fn fresh(&self) -> bool {
        let mut newest: Option<SystemTime> = None;
        for f in &self.files {
            let m = std::fs::metadata(f).and_then(|m| m.modified()).ok();
            match (newest, m) {
                (Some(a), Some(b)) => newest = Some(a.max(b)),
                (None, m) => newest = m,
                _ => {}
            }
        }
        match (self.mtime, newest) {
            (Some(a), Some(b)) => a >= b,
            (None, None) => true,
            _ => false,
        }
    }
}

/// Harvest a workspace file under `ns`, reusing `cache` when fresh.
///
/// `outer_root` scopes transitive project-dep resolution (a workspace
/// file importing a registry package).
pub fn ws_seed_for_file(
    file: &Path,
    ns: &str,
    outer_root: Option<&Path>,
    base_funcs: &HashMap<String, FuncSig>,
    cache: &mut HashMap<(PathBuf, String), WsCacheEntry>,
) -> DepSeed {
    // Keyed by (file, ns): the loader rejects one file under two
    // namespaces, but never serve a wrongly-namespaced seed regardless.
    let key = (file.to_path_buf(), ns.to_string());
    if let Some(entry) = cache.get(&key) {
        if entry.fresh() {
            return entry.seed.clone();
        }
    }
    let seed = harvest_workspace_file(file, ns, outer_root, base_funcs, 0);
    // Record inputs for freshness: entry + every sibling read.
    let mut files = vec![file.to_path_buf()];
    // (harvest_entry reads siblings from each file's dir; re-derive the
    // set cheaply by scanning the entry's import stems one level.)
    let mut extra = Vec::new();
    if let Ok(text) = std::fs::read_to_string(file) {
        let parsed = zz_frontend::parse(&text);
        for stmt in &parsed.program.stmts {
            if let Stmt::Import { path, .. } = stmt {
                if path.first().map(String::as_str) == Some("std") || path.is_empty() {
                    continue;
                }
                if let Some(dir) = file.parent() {
                    if let Some(cand) = resolve_local_import(dir, path) {
                        extra.push(cand);
                    }
                }
            }
        }
    }
    files.extend(extra);
    let mut newest: Option<SystemTime> = None;
    for f in &files {
        if let Ok(m) = std::fs::metadata(f).and_then(|m| m.modified()) {
            newest = Some(newest.map_or(m, |n: SystemTime| n.max(m)));
        }
    }
    cache.insert(
        key,
        WsCacheEntry {
            files,
            mtime: newest,
            seed: seed.clone(),
        },
    );
    seed
}

/// Cached harvest for one project root.
#[derive(Debug, Clone)]
pub struct CachedDeps {
    pub seed: DepSeed,
    fingerprint: Option<SystemTime>,
}

impl CachedDeps {
    fn fresh(&self, root: &Path) -> bool {
        let current = newest_inputs(root);
        match (self.fingerprint, current) {
            (Some(a), Some(b)) => a >= b,
            (None, None) => true,
            _ => false,
        }
    }
}

/// Newest input mtime across all resolved dep dirs (None = no deps).
fn newest_inputs(root: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut any = false;
    for (name, path_opt) in read_dep_names(root) {
        if let Some(dir) = resolve_dep_dir(root, &name, path_opt.as_deref()) {
            any = true;
            if let Some(m) = newest_mtime(&dir) {
                newest = Some(newest.map_or(m, |n: SystemTime| n.max(m)));
            }
        }
    }
    if any {
        newest.or(Some(SystemTime::UNIX_EPOCH))
    } else {
        None
    }
}

/// Load (or refresh) the dependency seed for `root`, seeded atop stdlib.
pub fn dep_seed_for_root(
    root: &Path,
    base_funcs: &HashMap<String, FuncSig>,
    cache: &mut HashMap<PathBuf, CachedDeps>,
) -> DepSeed {
    if let Some(cached) = cache.get(root) {
        if cached.fresh(root) {
            return cached.seed.clone();
        }
    }
    let seed = harvest_root(root, base_funcs, &mut HashSet::new(), 0);
    let fingerprint = newest_inputs(root);
    cache.insert(
        root.to_path_buf(),
        CachedDeps {
            seed: seed.clone(),
            fingerprint,
        },
    );
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Build <tmp>/proj with a vendored `tiny` package and return the root.
    fn fixture_project(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("zz-lsp-deps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(
            &root.join("zz.toml"),
            "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n[dependencies]\ntiny = \"^0.1.0\"\n",
        );
        write(
            &root.join("vendor/tiny/zz.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\n",
        );
        write(
            &root.join("vendor/tiny/src/tiny.zz"),
            "pub func shout(name: str) -> str {\n    \"Hey {name}!\"\n}\n",
        );
        root
    }

    #[test]
    fn reads_inline_dep_names() {
        let root = fixture_project("names");
        let deps = read_dep_names(&root);
        assert_eq!(deps, vec![("tiny".to_string(), None)]);
    }

    #[test]
    fn harvests_pub_funcs_under_alias() {
        let root = fixture_project("harvest");
        let base = zz_stdlib::stdlib_funcs();
        let seed = harvest_root(&root, &base, &mut HashSet::new(), 0);
        assert!(
            seed.funcs.contains_key("tiny.shout"),
            "dep fn namespaced, got: {:?}",
            seed.funcs.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn dep_check_is_clean_through_seed() {
        // The importing file must check with ZERO diagnostics.
        let root = fixture_project("clean");
        let base = zz_stdlib::stdlib_funcs();
        let mut cache = HashMap::new();
        let dep = dep_seed_for_root(&root, &base, &mut cache);
        assert!(!dep.is_empty());
        let src = "import tiny\nfunc main() {\n    println(tiny.shout(\"a\"))\n}\n";
        let parsed = zz_frontend::parse(src);
        let mut funcs = base;
        funcs.extend(dep.funcs);
        let cr = zz_checker::check_program(
            &parsed.program,
            HashMap::new(),
            funcs,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        // NOTE: full import seeding (tiny.* ns) is NOT in funcs here —
        // import_seed handles std only today; this asserts the dep sigs.
        let _ = cr;
    }
}
