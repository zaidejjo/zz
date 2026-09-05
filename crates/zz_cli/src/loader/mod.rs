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

use zz_checker::{check_program, FuncSig, StructSig, Type};
use zz_frontend::ast::{Expr, Program, Stmt};
use zz_frontend::diag::{error_at, RawDiag};
use zz_frontend::parse;
use zz_frontend::span::Span;
use zz_runtime::NativeEntry;
use zz_stdlib::{register_module_namespace, stdlib_funcs, stdlib_natives, STDLIB_MODULES};

mod rewrite;

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
    /// Native implementations (stdlib + namespaced copies), for the
    /// interpreter.
    pub natives: HashMap<String, NativeEntry>,
    pub errors: Vec<LoadError>,
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
    /// All items (pub + private) for the entry file / runtime.
    all_funcs: HashMap<String, FuncSig>,
    all_bindings: HashMap<String, Type>,
    all_structs: HashMap<String, StructSig>,
    natives: HashMap<String, NativeEntry>,
    /// Namespace → canonical path of the module (or `std:<module>` for the
    /// standard library) that owns it.
    namespaces: HashMap<String, PathBuf>,
    /// Canonical path → namespace it was registered under.
    ns_of: HashMap<PathBuf, String>,
    /// Canonical path of the entry file (for main() call validation).
    entry: PathBuf,
}

/// Load an entry file and all of its imports.
pub fn load_program(main_path: &Path) -> Result<LoadResult, String> {
    let entry = main_path.canonicalize().map_err(|e| {
        format!(
            "cannot read entry file `{}`: {e}\n\
                 hint: check that the file exists and the path is correct",
            main_path.display()
        )
    })?;
    let mut loader = Loader {
        sources: HashMap::new(),
        programs: HashMap::new(),
        order: Vec::new(),
        visiting: HashSet::new(),
        done: HashSet::new(),
        errors: Vec::new(),
        funcs: stdlib_funcs(),
        bindings: HashMap::new(),
        structs: HashMap::new(),
        all_funcs: HashMap::new(),
        all_bindings: HashMap::new(),
        all_structs: HashMap::new(),
        natives: stdlib_natives(),
        namespaces: HashMap::new(),
        ns_of: HashMap::new(),
        entry,
    };
    loader.load_file(main_path, None)?;
    Ok(loader.finish())
}

/// The namespace a module is bound to: its alias, or its file stem.
fn module_ns(alias: Option<&str>, path: &Path) -> String {
    alias.map(str::to_string).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

impl Loader {
    /// Parse a file and recursively load its imports (DFS post-order, so
    /// dependencies land in `order` before their dependents).
    fn load_file(&mut self, path: &Path, alias: Option<&str>) -> Result<(), String> {
        let canon = path.canonicalize().map_err(|e| {
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
        let parsed = parse(&source);
        if !parsed.errors.is_empty() {
            self.errors.push(LoadError {
                name: path.display().to_string(),
                source,
                diags: parsed.errors,
            });
            return Ok(());
        }

        self.visiting.insert(canon.clone());

        let imports: Vec<(Vec<String>, Option<String>)> = parsed
            .program
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Import { path, alias, .. } => Some((path.clone(), alias.clone())),
                _ => None,
            })
            .collect();

        for (imp, imp_alias) in imports {
            if imp.first().map(String::as_str) == Some("std") {
                let Some(module) = imp.get(1) else { continue };
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
                let ns = imp_alias.unwrap_or_else(|| module.clone());
                if let Err(msg) =
                    register_module_namespace(module, &ns, &mut self.funcs, &mut self.natives)
                {
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
                self.register_ns(&ns, &PathBuf::from(format!("std:{module}")), path, &source);
                continue;
            }
            let rel = canon
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(format!("{}.zz", imp.join("/")));
            // Report cycles at the import site, against the importer's source.
            if let Ok(rel_canon) = rel.canonicalize() {
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
                // A module already loaded under a different namespace cannot
                // be re-imported under another one.
                if let Some(existing) = self.ns_of.get(&rel_canon) {
                    let want = module_ns(imp_alias.as_deref(), &rel);
                    if existing != &want {
                        self.errors.push(LoadError {
                             name: path.display().to_string(),
                             source: source.clone(),
                             diags: vec![error_at(
                                 format!(
                                     "`{}` is imported under two namespaces: `{existing}` and `{want}`\n\
                                      hint: each file can only be imported under one namespace; \
                                      consider using an alias to avoid the conflict",
                                     rel.display()
                                 ),
                                 Span::new(0, 0),
                             )],
                         });
                        continue;
                    }
                }
            }
            self.load_file(&rel, imp_alias.as_deref())?;
        }

        self.visiting.remove(&canon);
        self.done.insert(canon.clone());
        self.sources.insert(canon.clone(), source);

        let ns = module_ns(alias, &canon);
        let src = self.sources[&canon].clone();
        if self.register_ns(&ns, &canon, path, &src) {
            let mut program = parsed.program;
            namespace_program(&mut program, &ns);
            self.programs.insert(canon.clone(), program);
            self.order.push(canon);
        }
        Ok(())
    }

    /// Register a namespace → module mapping, detecting collisions. Returns
    /// false (and records an error) when two different modules claim the same
    /// namespace, or one module is claimed by two namespaces.
    fn register_ns(&mut self, ns: &str, canon: &Path, display: &Path, source: &str) -> bool {
        if let Some(existing) = self.ns_of.get(canon) {
            if existing != ns {
                self.errors.push(LoadError {
                    name: display.display().to_string(),
                    source: source.to_string(),
                    diags: vec![error_at(
                        format!(
                            "module `{}` is imported under two namespaces: `{existing}` and `{ns}`\n\
                             hint: this can happen when the same file is imported via different paths",
                            display.display()
                        ),
                        Span::new(0, 0),
                    )],
                });
                return false;
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

    /// Type-check every module in dependency order, accumulating the checker
    /// seed. Modules with errors do not contribute their definitions.
    fn finish(mut self) -> LoadResult {
        let mut files = Vec::with_capacity(self.order.len());
        let mut programs = Vec::with_capacity(self.order.len());

        for path in &self.order {
            let name = path.display().to_string();
            let source = self.sources.remove(path).unwrap_or_default();
            files.push((name.clone(), source.clone()));

            let program = self.programs.remove(path).unwrap();

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

            let checked = check_program(
                &program,
                self.bindings.clone(),
                self.funcs.clone(),
                self.structs.clone(),
            );
            let has_errors = checked
                .errors
                .iter()
                .any(|e| e.severity == zz_frontend::diag::Severity::Error);
            if has_errors {
                self.errors.push(LoadError {
                    name: name.clone(),
                    source: source.clone(),
                    diags: checked.errors,
                });
            } else {
                // Propagate warnings (even if there are no hard errors)
                // so they can be displayed to the user.
                if !checked.errors.is_empty() {
                    self.errors.push(LoadError {
                        name: name.clone(),
                        source: source.clone(),
                        diags: checked.errors,
                    });
                }
                // Only propagate pub items to the cross-module seed.
                self.bindings.extend(checked.pub_bindings.clone());
                self.funcs.extend(checked.pub_funcs.clone());
                self.structs.extend(checked.pub_structs.clone());
                // Track all items for the entry file / runtime.
                self.all_bindings.extend(checked.bindings);
                self.all_funcs.extend(checked.funcs);
                self.all_structs.extend(checked.structs);

                // Handle `pub import` re-exports: for each `pub import ns` in
                // this module, copy the re-exported namespace's pub functions
                // and bindings into the current module's namespace in the
                // cross-module seed.
                // NOTE: struct re-exports are not yet supported because struct
                // types are identity-based (Type::Struct("a.X") ≠
                // Type::Struct("b.X")). Struct re-exports require type aliasing
                // support (future work).
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
                            // Copy items from seed `reexport_ns.*` to
                            // `module_ns.reexport_ns.*`
                            let prefix = format!("{}.", reexport_ns);
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

        LoadResult {
            programs,
            files,
            funcs: self.all_funcs,
            bindings: self.all_bindings,
            structs: self.all_structs,
            natives: self.natives,
            errors: self.errors,
        }
    }
}

/// Rewrite a module's AST so its top-level definitions are namespaced:
/// - top-level `func foo` becomes `func ns.foo`,
/// - top-level `x := ...` becomes `x := ...` with binding name `ns.x`,
/// - references to those names (`Expr::Ident`) become `Expr::Path([ns, name])`
///   unless shadowed by a local binding.
fn namespace_program(program: &mut Program, ns: &str) {
    let mut top = HashSet::new();
    for stmt in &program.stmts {
        match stmt {
            Stmt::Func { name, .. } | Stmt::Struct { name, .. } => {
                top.insert(name.join("."));
            }
            Stmt::Decl { name, .. } => {
                top.insert(name.name.clone());
            }
            _ => {}
        }
    }
    let mut rw = rewrite::Rewriter::new(ns, &top);
    for stmt in &mut program.stmts {
        rw.rewrite_stmt(stmt);
    }
}
