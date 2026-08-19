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
use zz_frontend::ast::{Block, Expr, FmtPart, Pattern, Program, Stmt, Ty, TyKind};
use zz_frontend::diag::{error_at, RawDiag};
use zz_frontend::parse;
use zz_frontend::span::Span;
use zz_runtime::NativeEntry;
use zz_stdlib::{register_module_namespace, stdlib_funcs, stdlib_natives, STDLIB_MODULES};

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
    funcs: HashMap<String, FuncSig>,
    bindings: HashMap<String, Type>,
    structs: HashMap<String, StructSig>,
    natives: HashMap<String, NativeEntry>,
    /// Namespace → canonical path of the module (or `std:<module>` for the
    /// standard library) that owns it.
    namespaces: HashMap<String, PathBuf>,
    /// Canonical path → namespace it was registered under.
    ns_of: HashMap<PathBuf, String>,
}

/// Load an entry file and all of its imports.
pub fn load_program(main_path: &Path) -> Result<LoadResult, String> {
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
        natives: stdlib_natives(),
        namespaces: HashMap::new(),
        ns_of: HashMap::new(),
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
        let canon = path
            .canonicalize()
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;

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

        let source = std::fs::read_to_string(&canon)
            .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
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
                            format!("unknown standard library module `std.{module}`"),
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
                        diags: vec![error_at(msg, Span::new(0, 0))],
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
                                "circular import: `{}` imports `{}`, which (transitively) imports `{}`",
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
                                    "`{}` is imported under two namespaces: `{existing}` and `{want}`",
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
                            "module `{}` is imported under two namespaces: `{existing}` and `{ns}`",
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
                            "namespace `{ns}` is claimed by both `{}` and `{}`",
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
            let checked = check_program(
                &program,
                self.bindings.clone(),
                self.funcs.clone(),
                self.structs.clone(),
            );
            if !checked.errors.is_empty() {
                self.errors.push(LoadError {
                    name,
                    source,
                    diags: checked.errors,
                });
            } else {
                self.bindings.extend(checked.bindings);
                self.funcs.extend(checked.funcs);
                self.structs.extend(checked.structs);
            }
            programs.push(program);
        }

        LoadResult {
            programs,
            files,
            funcs: self.funcs,
            bindings: self.bindings,
            structs: self.structs,
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
            Stmt::Func { name, .. } | Stmt::Decl { name, .. } | Stmt::Struct { name, .. } => {
                top.insert(name.name.clone());
            }
            _ => {}
        }
    }
    let mut rw = Rewriter {
        ns,
        top: &top,
        scopes: vec![HashSet::new()],
    };
    for stmt in &mut program.stmts {
        rw.rewrite_stmt(stmt);
    }
}

struct Rewriter<'a> {
    ns: &'a str,
    top: &'a HashSet<String>,
    /// Stack of shadowing scopes; each holds names declared so far.
    scopes: Vec<HashSet<String>>,
}

impl Rewriter<'_> {
    fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string());
        }
    }

    fn rewrite_stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Decl {
                name, ty, value, ..
            } => {
                if self.top.contains(&name.name) {
                    name.name = format!("{}.{}", self.ns, name.name);
                }
                if let Some(t) = ty {
                    self.rewrite_ty(t);
                }
                self.rewrite_expr(value);
                self.declare(&name.name);
            }
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                body,
                ..
            } => {
                let is_top = self.top.contains(&name.name);
                if is_top {
                    name.name = format!("{}.{}", self.ns, name.name);
                }
                self.push_scope();
                if !is_top {
                    // A nested func shadows its own name within its body.
                    self.declare(&name.name);
                }
                for g in generics {
                    self.declare(&g.name);
                }
                for p in params {
                    self.declare(&p.name.name);
                    if let Some(t) = &mut p.ty {
                        self.rewrite_ty(t);
                    }
                }
                if let Some(t) = ret {
                    self.rewrite_ty(t);
                }
                self.rewrite_block(body);
                self.pop_scope();
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.rewrite_expr(v);
                }
            }
            Stmt::Struct { name, fields, .. } => {
                if self.top.contains(&name.name) {
                    name.name = format!("{}.{}", self.ns, name.name);
                }
                for (_, fty) in fields {
                    self.rewrite_ty(fty);
                }
            }
            Stmt::For {
                var, iter, body, ..
            } => {
                self.rewrite_expr(iter);
                self.push_scope();
                self.declare(&var.name);
                self.rewrite_block(body);
                self.pop_scope();
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Assign { target, value, .. } => {
                self.rewrite_expr(target);
                self.rewrite_expr(value);
            }
            Stmt::Expr(e) => self.rewrite_expr(e),
            Stmt::Import { .. } => {}
        }
    }

    fn rewrite_block(&mut self, block: &mut Block) {
        self.push_scope();
        for stmt in &mut block.stmts {
            self.rewrite_stmt(stmt);
        }
        self.pop_scope();
    }

    /// Rewrite type annotations: struct names defined in this module become
    /// namespaced (`Point` → `shapes.Point`).
    fn rewrite_ty(&mut self, ty: &mut Ty) {
        match &mut ty.kind {
            TyKind::Named(name, args) => {
                if self.top.contains(name) && !self.is_shadowed(name) {
                    *name = format!("{}.{}", self.ns, name);
                }
                for a in args {
                    self.rewrite_ty(a);
                }
            }
            TyKind::Tuple(ts) => {
                for t in ts {
                    self.rewrite_ty(t);
                }
            }
            TyKind::Option(t) | TyKind::Array(t) => self.rewrite_ty(t),
            TyKind::Result(a, b) => {
                self.rewrite_ty(a);
                self.rewrite_ty(b);
            }
            TyKind::Func(ps, r) => {
                for p in ps {
                    self.rewrite_ty(p);
                }
                self.rewrite_ty(r);
            }
            TyKind::Dict(k, v) => {
                self.rewrite_ty(k);
                self.rewrite_ty(v);
            }
            TyKind::Union(ts) => {
                for t in ts {
                    self.rewrite_ty(t);
                }
            }
            _ => {}
        }
    }

    fn rewrite_expr(&mut self, expr: &mut Expr) {
        match expr {
            Expr::Ident { name, span } => {
                if self.top.contains(name) && !self.is_shadowed(name) {
                    *expr = Expr::Path {
                        parts: vec![self.ns.to_string(), name.clone()],
                        span: *span,
                    };
                }
            }
            Expr::Paren { expr: inner, .. } => self.rewrite_expr(inner),
            Expr::Unary { expr: inner, .. } => self.rewrite_expr(inner),
            Expr::Binary { left, right, .. } => {
                self.rewrite_expr(left);
                self.rewrite_expr(right);
            }
            Expr::Call { callee, args, .. } => {
                // Method call: `p.dist()` — the method name is the last path
                // component; qualify it like a bare function reference so the
                // checker/runtime can resolve `ns.dist`.
                if let Expr::Path { parts, .. } = callee.as_mut() {
                    if parts.len() >= 2 {
                        if let Some(last) = parts.last_mut() {
                            if self.top.contains(last) && !self.is_shadowed(last) {
                                *last = format!("{}.{}", self.ns, last);
                            }
                        }
                    }
                }
                self.rewrite_expr(callee);
                for a in args {
                    self.rewrite_expr(a);
                }
            }
            Expr::Closure { params, body, .. } => {
                self.push_scope();
                for p in params {
                    self.declare(&p.name.name);
                    if let Some(t) = &mut p.ty {
                        self.rewrite_ty(t);
                    }
                }
                self.rewrite_expr(body);
                self.pop_scope();
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.rewrite_expr(cond);
                self.rewrite_block(then);
                if let Some(e) = els {
                    self.rewrite_expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.rewrite_expr(cond);
                self.rewrite_block(body);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.rewrite_expr(scrutinee);
                for arm in arms {
                    self.push_scope();
                    if let Pattern::Binding { name } = &arm.pat {
                        self.declare(&name.name);
                    }
                    self.rewrite_expr(&mut arm.body);
                    self.pop_scope();
                }
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.rewrite_expr(value);
                self.push_scope();
                if let Pattern::Binding { name } = pat {
                    self.declare(&name.name);
                }
                self.rewrite_block(then);
                if let Some(e) = els {
                    self.rewrite_expr(e);
                }
                self.pop_scope();
            }
            Expr::Try { expr: inner, .. } => self.rewrite_expr(inner),
            Expr::Block(b) => self.rewrite_block(b),
            Expr::Variant { arg, .. } => {
                if let Some(a) = arg {
                    self.rewrite_expr(a);
                }
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.rewrite_expr(e);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.rewrite_expr(k);
                    self.rewrite_expr(v);
                }
            }
            Expr::Fmt { parts, .. } => {
                for part in parts {
                    if let FmtPart::Expr(e) = part {
                        self.rewrite_expr(e);
                    }
                }
            }
            Expr::Field { obj, .. } => self.rewrite_expr(obj),
            Expr::Range { start, end, .. } => {
                self.rewrite_expr(start);
                self.rewrite_expr(end);
            }
            Expr::Index { obj, index, .. } => {
                self.rewrite_expr(obj);
                self.rewrite_expr(index);
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.rewrite_expr(obj);
                if let Some(s) = start {
                    self.rewrite_expr(s);
                }
                if let Some(e) = end {
                    self.rewrite_expr(e);
                }
            }
            Expr::StructInit { name, fields, .. } => {
                // A struct defined in this module is referenced by its
                // namespaced name; imported structs are already qualified.
                if self.top.contains(name) && !self.is_shadowed(name) {
                    *name = format!("{}.{}", self.ns, name);
                }
                for (_, v) in fields {
                    self.rewrite_expr(v);
                }
            }
            Expr::Path { parts, .. } => {
                // `p.x` on a top-level binding: qualify the root so the
                // struct-field walk finds `ns.p`.
                if let Some(first) = parts.first_mut() {
                    if self.top.contains(first) && !self.is_shadowed(first) {
                        *first = format!("{}.{}", self.ns, first);
                    }
                }
            }
            Expr::Int { .. } | Expr::Float { .. } | Expr::Str { .. } | Expr::Bool { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a unique temp dir with the given (relative path → contents).
    fn temp_project(files: &[(&str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("zz_loader_test_{}", n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (rel, contents) in files {
            let p = dir.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, contents).unwrap();
        }
        dir
    }

    #[test]
    fn loads_relative_import() {
        let dir = temp_project(&[
            ("main.zz", "import math.utils\nx := utils.double(21)"),
            ("math/utils.zz", "func double(n: int) -> int { n * 2 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.programs.len(), 2);
        assert_eq!(result.bindings["main.x"], Type::Int);
    }

    #[test]
    fn imported_bindings_visible() {
        let dir = temp_project(&[
            ("main.zz", "import config\nx := config.base + 1"),
            ("config.zz", "base := 41"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.x"], Type::Int);
    }

    #[test]
    fn import_alias_works() {
        let dir = temp_project(&[
            ("main.zz", "import math.utils as m\nx := m.double(21)"),
            ("math/utils.zz", "func double(n: int) -> int { n * 2 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.x"], Type::Int);
    }

    #[test]
    fn module_internal_calls_rewritten() {
        // `double` calls `twice` internally; both are top-level in utils.zz.
        let dir = temp_project(&[
            ("main.zz", "import math.utils\nx := utils.double(21)"),
            (
                "math/utils.zz",
                "func twice(n: int) -> int { n * 2 }\nfunc double(n: int) -> int { twice(n) }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.x"], Type::Int);
    }

    #[test]
    fn shadowing_respected() {
        // Local `n` shadows the top-level `n` inside the func body.
        let dir = temp_project(&[
            ("main.zz", "import math.utils\nx := utils.double(21)"),
            (
                "math/utils.zz",
                "n := 100\nfunc double(n: int) -> int { n * 2 }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.x"], Type::Int);
    }

    #[test]
    fn namespace_collision_detected() {
        // Two modules with the same stem claim the same namespace.
        let dir = temp_project(&[
            ("main.zz", "import a.utils\nimport b.utils\n1"),
            ("a/utils.zz", "func f() -> int { 1 }"),
            ("b/utils.zz", "func g() -> int { 2 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(
            result.errors.iter().any(|e| e
                .diags
                .iter()
                .any(|d| d.message.contains("claimed by both"))),
            "expected namespace collision error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn two_namespaces_for_one_module_detected() {
        let dir = temp_project(&[
            ("main.zz", "import utils\nimport utils as u\n1"),
            ("utils.zz", "func f() -> int { 1 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.diags.iter().any(|d| d.message.contains("two namespaces"))),
            "expected two-namespace error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn circular_import_detected() {
        let dir = temp_project(&[("a.zz", "import b"), ("b.zz", "import a")]);
        let result = load_program(&dir.join("a.zz")).unwrap();
        assert!(
            result.errors.iter().any(|e| e
                .diags
                .iter()
                .any(|d| d.message.contains("circular import"))),
            "expected circular import error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn transitive_cycle_detected() {
        let dir = temp_project(&[
            ("a.zz", "import b"),
            ("b.zz", "import c"),
            ("c.zz", "import a"),
        ]);
        let result = load_program(&dir.join("a.zz")).unwrap();
        assert!(
            result.errors.iter().any(|e| e
                .diags
                .iter()
                .any(|d| d.message.contains("circular import"))),
            "expected circular import error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn diamond_import_is_fine() {
        // a imports b and c; both import d. No cycle.
        let dir = temp_project(&[
            ("a.zz", "import b\nimport c"),
            ("b.zz", "import d"),
            ("c.zz", "import d"),
            ("d.zz", "func f() -> int { 1 }"),
        ]);
        let result = load_program(&dir.join("a.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        // d loaded once, before b and c.
        assert_eq!(result.programs.len(), 4);
    }

    #[test]
    fn missing_module_errors() {
        let dir = temp_project(&[("main.zz", "import nope.missing\n1")]);
        let result = load_program(&dir.join("main.zz")).unwrap_err();
        assert!(result.contains("cannot read"), "{result}");
    }

    #[test]
    fn unknown_stdlib_module_errors() {
        let dir = temp_project(&[("main.zz", "import std.nope\n1")]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(
            result.errors.iter().any(|e| e
                .diags
                .iter()
                .any(|d| d.message.contains("unknown standard library module"))),
            "expected unknown stdlib error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn parse_error_in_module_reported() {
        let dir = temp_project(&[("main.zz", "import broken\n1"), ("broken.zz", "x :=")]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(
            result.errors.iter().any(|e| e.name.contains("broken.zz")),
            "expected error for broken.zz, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn type_error_in_module_reported() {
        let dir = temp_project(&[
            ("main.zz", "import broken\n1"),
            ("broken.zz", "x := 1 + \"a\""),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.diags.iter().any(|d| d.message.contains("cannot apply"))),
            "expected type error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn full_program_runs_with_imports() {
        use zz_runtime::{Interp, Value};

        let dir = temp_project(&[
            ("main.zz", "import std.io\nimport std.str\nimport math.utils\nn := utils.double(6)\nstr.length(\"abc\")"),
            ("math/utils.zz", "func double(n: int) -> int { n * 2 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let mut interp = Interp::with_natives(result.natives.clone());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Int(3));
        // The imported function ran: main's `n` binding must hold.
        assert_eq!(result.bindings["main.n"], zz_checker::Type::Int);
    }

    #[test]
    fn stdlib_import_needs_no_file() {
        let dir = temp_project(&[("main.zz", "import std.io\nimport std.str\n1")]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.programs.len(), 1);
        assert!(result.funcs.contains_key("std.io.println"));
        // Namespaced copies are registered too.
        assert!(result.funcs.contains_key("io.println"));
        assert!(result.natives.contains_key("io.println"));
    }

    #[test]
    fn struct_in_module_namespaced() {
        let dir = temp_project(&[
            (
                "main.zz",
                "import shapes\np := shapes.Point{ x: 1, y: 2 }\nz := shapes.dist(p)",
            ),
            (
                "shapes.zz",
                "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.z"], Type::Int);
        assert!(result.structs.contains_key("shapes.Point"));
    }

    #[test]
    fn struct_field_access_in_module() {
        use zz_runtime::{Interp, Value};

        // `p.x` inside the module must resolve through the namespaced
        // binding `shapes.p`.
        let dir = temp_project(&[
            ("main.zz", "import shapes\nz := shapes.get_x()"),
            (
                "shapes.zz",
                "struct Point { x: int, y: int }\np := Point{ x: 42, y: 0 }\nfunc get_x() -> int { p.x }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.z"], Type::Int);

        let mut interp = Interp::with_natives(result.natives.clone());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Int(42));
    }

    #[test]
    fn indexing_and_slicing_in_module() {
        use zz_runtime::{Interp, Value};

        // Index/slice expressions inside a module must be rewritten too
        // (they reference a namespaced binding as their object).
        let dir = temp_project(&[
            ("main.zz", "import stats\nz := stats.first()\nw := stats.mid()"),
            (
                "stats.zz",
                "scores := [10, 20, 30]\nfunc first() -> int { scores[0] }\nfunc mid() -> [int] { scores[1:3] }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.z"], Type::Int);
        assert_eq!(result.bindings["main.w"], Type::Array(Box::new(Type::Int)));

        let mut interp = Interp::with_natives(result.natives.clone());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Array(vec![Value::Int(20), Value::Int(30)]));
    }

    #[test]
    fn pipeline_in_module() {
        use zz_runtime::{Interp, Value};

        // Piped calls in a module rewrite the callee path correctly.
        let dir = temp_project(&[
            ("main.zz", "import math\nz := math.apply()"),
            (
                "math.zz",
                "func inc(n: int) -> int { n + 1 }\nfunc apply() -> int { 5 |> inc }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let mut interp = Interp::with_natives(result.natives.clone());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Int(6));
    }

    #[test]
    fn method_call_in_module() {
        use zz_runtime::{Interp, Value};

        // `p.dist()` inside the defining module: the method name is
        // qualified to `shapes.dist`.
        let dir = temp_project(&[
            ("main.zz", "import shapes\nz := shapes.apply()"),
            (
                "shapes.zz",
                "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\nfunc apply() -> int { p := Point { x: 3, y: 4 }\np.dist() }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let mut interp = Interp::with_natives(result.natives.clone());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Int(7));
    }

    #[test]
    fn method_call_cross_module() {
        use zz_runtime::{Interp, Value};

        // `p.dist()` from another module: the method resolves through the
        // receiver's struct type (`shapes.Point` → `shapes.dist`).
        let dir = temp_project(&[
            (
                "main.zz",
                "import shapes\np := shapes.Point { x: 3, y: 4 }\nz := p.dist()",
            ),
            (
                "shapes.zz",
                "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }",
            ),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["main.z"], Type::Int);

        let mut interp = Interp::with_natives(result.natives.clone());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Int(7));
    }
}
