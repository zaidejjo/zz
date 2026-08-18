//! Import resolution and program loading (Phase 2).
//!
//! Resolves `import` statements to files, parses and type-checks every
//! module in dependency order, and detects circular imports.
//!
//! Resolution rules:
//! - `import std.*` → standard library (no file; signatures are seeded).
//! - `import a.b`   → `<dir of importing file>/a/b.zz`.
//!
//! Imported modules are merged into the importing scope (include-style):
//! their top-level functions and bindings become visible to the importer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use zz_checker::{check_program, FuncSig, Type};
use zz_frontend::ast::{Program, Stmt};
use zz_frontend::diag::{error_at, RawDiag};
use zz_frontend::parse;
use zz_frontend::span::Span;
use zz_stdlib::{stdlib_funcs, STDLIB_MODULES};

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
    };
    loader.load_file(main_path)?;
    Ok(loader.finish())
}

impl Loader {
    /// Parse a file and recursively load its imports (DFS post-order, so
    /// dependencies land in `order` before their dependents).
    fn load_file(&mut self, path: &Path) -> Result<(), String> {
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

        let imports: Vec<Vec<String>> = parsed
            .program
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Import { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();

        for imp in imports {
            if imp.first().map(String::as_str) == Some("std") {
                if let Some(module) = imp.get(1) {
                    if !STDLIB_MODULES.contains(&module.as_str()) {
                        self.errors.push(LoadError {
                            name: path.display().to_string(),
                            source: source.clone(),
                            diags: vec![error_at(
                                format!("unknown standard library module `std.{module}`"),
                                Span::new(0, 0),
                            )],
                        });
                    }
                }
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
            }
            self.load_file(&rel)?;
        }

        self.visiting.remove(&canon);
        self.done.insert(canon.clone());
        self.sources.insert(canon.clone(), source);
        self.programs.insert(canon.clone(), parsed.program);
        self.order.push(canon);
        Ok(())
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
            let checked = check_program(&program, self.bindings.clone(), self.funcs.clone());
            if !checked.errors.is_empty() {
                self.errors.push(LoadError {
                    name,
                    source,
                    diags: checked.errors,
                });
            } else {
                self.bindings.extend(checked.bindings);
                self.funcs.extend(checked.funcs);
            }
            programs.push(program);
        }

        LoadResult {
            programs,
            files,
            funcs: self.funcs,
            bindings: self.bindings,
            errors: self.errors,
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
            ("main.zz", "import math.utils\nx := double(21)"),
            ("math/utils.zz", "func double(n: int) -> int { n * 2 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.programs.len(), 2);
        assert_eq!(result.bindings["x"], Type::Int);
    }

    #[test]
    fn imported_bindings_visible() {
        let dir = temp_project(&[
            ("main.zz", "import config\nx := base + 1"),
            ("config.zz", "base := 41"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.bindings["x"], Type::Int);
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
        use zz_stdlib::stdlib_natives;

        let dir = temp_project(&[
            ("main.zz", "import std.io\nimport std.str\nimport math.utils\nn := double(6)\nstd.str.length(\"abc\")"),
            ("math/utils.zz", "func double(n: int) -> int { n * 2 }"),
        ]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let mut interp = Interp::with_natives(stdlib_natives());
        let mut last = Value::Unit;
        for p in &result.programs {
            last = interp.run(p).unwrap();
        }
        assert_eq!(last, Value::Int(3));
        // The imported function ran: main's `n` binding must hold.
        assert_eq!(result.bindings["n"], zz_checker::Type::Int);
    }

    #[test]
    fn stdlib_import_needs_no_file() {
        let dir = temp_project(&[("main.zz", "import std.io\nimport std.str\n1")]);
        let result = load_program(&dir.join("main.zz")).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.programs.len(), 1);
        assert!(result.funcs.contains_key("std.io.println"));
    }
}
