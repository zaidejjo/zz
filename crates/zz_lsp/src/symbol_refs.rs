//! Cross-file symbol scope resolution, shared by rename, references,
//! and document-highlight applicants.
//!
//! The old handlers searched one file (rename) or every indexed file by
//! bare name (references — false positives across unrelated files, and
//! stale snapshots for dirty buffers). This module resolves *which files
//! can actually see the symbol* before searching:
//!
//! 1. The symbol at the cursor names `name`; its defining file is the
//!    file holding the definition — the current file when defined
//!    locally, otherwise the file reached through the current file's
//!    imports (selective `import m(x)` first, full-module membership
//!    second, current file as the safe fallback).
//! 2. Candidate files are the open documents (fresh buffers win over the
//!    workspace index) plus indexed files, kept only when they are the
//!    defining file, the current file, import the defining module — or
//!    always, when the definition is a project-global top-level binding.
//! 3. Inside non-defining files, definition-site hits are dropped: a
//!    local `func circle` there is its own symbol, not ours. Struct
//!    fields go further — only qualified (`p.x`) hits count elsewhere,
//!    since bare `x` in another file is almost never our field.

use std::collections::HashMap;

use tower_lsp::lsp_types::Url;
use zz_frontend::ast::{ImportItem, Program, Stmt};
use zz_frontend::span::Span;

use crate::lookup::{self, DefKind};
use crate::state::GlobalState;

/// One file that can observe the symbol: fresh program + source.
#[derive(Debug, Clone)]
pub struct ScopedFile {
    pub uri: Url,
    pub program: Program,
    pub source: String,
}

/// A resolved symbol: its name, where it is defined, and every file
/// that can observe it.
#[derive(Debug, Clone)]
pub struct SymbolScope {
    pub name: String,
    pub defining_uri: Url,
    /// Struct-field target when the symbol is a field: (struct, field).
    /// Drives type-aware reference finding (definition site, literal
    /// labels, receiver-resolved usages) instead of bare-name matching.
    pub field_target: Option<(String, String)>,
    /// True when the cursor sits on a struct-field definition/usage:
    /// cross-file hits must be qualified (`p.x`), never bare (`x`).
    pub field_only_qualified: bool,
    /// True when the definition is a top-level value binding
    /// (`shared := 1`): those ride the global seed and are visible in
    /// every file without imports, so every file stays in scope. Module
    /// members (funcs, structs) stay import-gated.
    pub global_binding: bool,
    pub files: Vec<ScopedFile>,
}

/// Resolve the symbol at `offset` in `uri` to its cross-file scope.
///
/// Returns `None` when there is no document, no program, or no symbol
/// at the cursor.
pub fn resolve_symbol_scope(state: &GlobalState, uri: &Url, offset: u32) -> Option<SymbolScope> {
    let doc: crate::state::DocumentState = state.documents.get(uri)?.value().clone();
    let program = doc.program.clone()?;
    let source = doc.source.clone();

    let node = lookup::find_node_at(&program, &source, offset);
    let name = node.name.clone()?;

    let defs = lookup::collect_definitions(&program, &source);
    let field_only_qualified = node
        .name_span
        .and_then(|s| defs.get(&s.start))
        .is_some_and(|d| d.kind == DefKind::Field);

    let defining_uri = if defs.values().any(|d| d.name == name) {
        uri.clone()
    } else {
        defining_uri_via_imports(state, &program, uri, &name).unwrap_or_else(|| uri.clone())
    };

    // Top-level value bindings ride the global seed: visible in every
    // file without imports. Look the definition up in the defining
    // file's own program (fresh buffer when open).
    let global_binding = is_top_level_binding(state, &defining_uri, &name);
    let field_target = field_target_in(state, &defining_uri, &name);

    // Fresh buffers first: open documents shadow index snapshots.
    let mut files: Vec<ScopedFile> = Vec::new();
    let mut seen: HashMap<Url, ()> = HashMap::new();
    for entry in state.documents.iter() {
        let doc = entry.value();
        if let Some(program) = doc.program.clone() {
            seen.insert(entry.key().clone(), ());
            files.push(ScopedFile {
                uri: entry.key().clone(),
                program,
                source: doc.source.clone(),
            });
        }
    }
    {
        let index = state.module_index.read().unwrap();
        for (entry_uri, entry) in &index.entries {
            if seen.contains_key(entry_uri) {
                continue;
            }
            let program = match &entry.program {
                Some(p) => p.clone(),
                None => zz_frontend::parse(&entry.source).program,
            };
            files.push(ScopedFile {
                uri: entry_uri.clone(),
                program,
                source: entry.source.clone(),
            });
        }
    }

    let index = state.module_index.read().unwrap();
    let root = state.root.read().unwrap().clone();
    files.retain(|f| {
        f.uri == defining_uri
            || f.uri == *uri
            || global_binding
            || imports_module(&f.program, &defining_uri, &index, root.as_deref(), uri)
    });

    Some(SymbolScope {
        name,
        defining_uri,
        field_only_qualified,
        global_binding,
        field_target,
        files,
    })
}

/// Whether `name` is a top-level value binding (`x := …`) in the
/// defining file's program. Such bindings merge into the global seed
/// and are referenceable without imports.
fn is_top_level_binding(state: &GlobalState, defining_uri: &Url, name: &str) -> bool {
    let Some(program) = defining_program(state, defining_uri) else {
        return false;
    };
    program.stmts.iter().any(|stmt| match stmt {
        Stmt::Decl { name: n, .. } => n.name == name,
        _ => false,
    })
}

/// Resolve a struct-field target in the defining file: full
/// `Point.x` names match directly; bare `x` resolves when exactly one
/// struct declares it (ambiguity falls back to bare-name matching).
fn field_target_in(
    state: &GlobalState,
    defining_uri: &Url,
    name: &str,
) -> Option<(String, String)> {
    let program = defining_program(state, defining_uri)?;
    let mut structs: Vec<(String, Vec<String>)> = Vec::new();
    for stmt in &program.stmts {
        if let Stmt::Struct { name, fields, .. } = stmt {
            structs.push((
                name.join("."),
                fields.iter().map(|(f, _)| f.name.clone()).collect(),
            ));
        }
    }
    if let Some((head, short)) = name.rsplit_once('.') {
        if structs
            .iter()
            .any(|(s, fs)| s == head && fs.iter().any(|f| f == short))
        {
            return Some((head.to_string(), short.to_string()));
        }
        return None;
    }
    let mut hits = structs
        .iter()
        .filter(|(_, fs)| fs.iter().any(|f| f == name));
    let (owner, _) = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some((owner.clone(), name.to_string()))
}

/// The defining file's program: open buffer first, index second.
fn defining_program(state: &GlobalState, defining_uri: &Url) -> Option<Program> {
    if let Some(doc) = state.documents.get(defining_uri) {
        if let Some(program) = doc.program.clone() {
            return Some(program);
        }
    }
    state
        .module_index
        .read()
        .unwrap()
        .entries
        .get(defining_uri)
        .and_then(|e| e.program.clone())
}

/// Find the defining file of `name` through `program`'s imports:
/// selective imports exposing `name` win over full-module membership.
fn defining_uri_via_imports(
    state: &GlobalState,
    program: &Program,
    uri: &Url,
    name: &str,
) -> Option<Url> {
    let index = state.module_index.read().unwrap();
    let root = state.root.read().unwrap().clone();
    // Filesystem fallback when no workspace root is set: resolve
    // relative to the current file's directory.
    let fallback_root: Option<std::path::PathBuf> = uri
        .to_file_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let base = root.as_deref().or(fallback_root.as_deref());
    let mut weak: Option<Url> = None;
    for stmt in &program.stmts {
        let Stmt::Import { path, .. } = stmt else {
            continue;
        };
        let Some(base) = base else {
            break;
        };
        let Some(target) = index.resolve_import(path, base) else {
            continue;
        };
        if target == *uri {
            continue;
        }
        if selective_exposes(stmt, name) {
            return Some(target);
        }
        // Full import: `name` may be a member (`ns.name` usage). Weak
        // signal — kept only when nothing selective matches.
        if weak.is_none() {
            weak = Some(target);
        }
    }
    weak
}

/// Whether `import_stmt` selectively binds `name`
/// (`import m(name)` or `import m(other as name)`).
fn selective_exposes(import_stmt: &Stmt, name: &str) -> bool {
    let Stmt::Import { items, .. } = import_stmt else {
        return false;
    };
    items.iter().any(|item| match item {
        ImportItem::Named { name: n, alias, .. } => n == name || alias.as_deref() == Some(name),
        ImportItem::Wildcard { .. } => true,
    })
}

/// Whether `program` imports the module at `defining_uri` (any form).
fn imports_module(
    program: &Program,
    defining_uri: &Url,
    index: &crate::cross_file::ModuleIndex,
    root: Option<&std::path::Path>,
    current_uri: &Url,
) -> bool {
    // Filesystem fallback when no workspace root is set (same rule as
    // `defining_uri_via_imports`).
    let fallback_root: Option<std::path::PathBuf> = current_uri
        .to_file_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let base_root: Option<&std::path::Path> = match root {
        Some(r) => Some(r),
        None => fallback_root.as_deref(),
    };
    for stmt in &program.stmts {
        let Stmt::Import { path, .. } = stmt else {
            continue;
        };
        let Some(base) = base_root else {
            break;
        };
        if index
            .resolve_import(path, base)
            .is_some_and(|u| &u == defining_uri)
        {
            return true;
        }
    }
    false
}

/// Receiver-to-struct resolver for field reference finding, backed by
/// one file's check bindings (`p` → `Point`). Files without a check
/// result resolve nothing (usages there are skipped, never guessed).
pub struct FieldResolver<'a> {
    bindings: Option<&'a HashMap<String, zz_checker::Type>>,
}

impl<'a> FieldResolver<'a> {
    pub fn new(bindings: Option<&'a HashMap<String, zz_checker::Type>>) -> Self {
        Self { bindings }
    }

    pub fn resolve(&self, obj: &str) -> Option<String> {
        match self.bindings?.get(obj)? {
            zz_checker::Type::Struct(name, _) => Some(name.clone()),
            _ => None,
        }
    }
}

/// Map a reference span to the exact range a rename must replace.
///
/// Path matches (`lib.circle`) span the whole path; only the last
/// segment is the symbol. The source slice decides: a slice ending in
/// the name is trimmed to it, anything else is passed through.
pub fn rename_span_for(source: &str, span: Span, name: &str) -> Span {
    let text = source
        .get(span.start as usize..span.end as usize)
        .unwrap_or("");
    if text != name && text.ends_with(name) {
        Span::new(span.end - name.len() as u32, span.end)
    } else {
        span
    }
}

/// Whether a reference hit counts for this scope in a non-defining file.
///
/// Definition sites are always foreign symbols; struct fields additionally
/// require qualified (`p.x`) usage elsewhere.
pub fn hit_counts_in_file(
    scope: &SymbolScope,
    file_uri: &Url,
    hit: &lookup::Reference,
    source: &str,
) -> bool {
    if file_uri == &scope.defining_uri {
        return true;
    }
    if hit.is_definition {
        return false;
    }
    if scope.field_only_qualified {
        let text = source
            .get(hit.span.start as usize..hit.span.end as usize)
            .unwrap_or("");
        if !text.contains('.') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> GlobalState {
        GlobalState::new()
    }

    fn open(state: &GlobalState, uri: &str, src: &str) -> Url {
        let uri: Url = uri.parse().unwrap();
        state.update_document(uri.clone(), 1, src.to_string());
        uri
    }

    fn index_file(state: &GlobalState, uri: &Url, module: &str, src: &str) {
        let entry =
            crate::cross_file::parse_file_entry(uri.clone(), src.to_string(), module.to_string());
        let mut index = state.module_index.write().unwrap();
        index.module_to_uri.insert(module.to_string(), uri.clone());
        index.uri_to_module.insert(uri.clone(), module.to_string());
        index.entries.insert(uri.clone(), entry);
    }

    fn offset_of(src: &str, needle: &str) -> u32 {
        src.find(needle).expect("needle") as u32 + 1
    }

    #[test]
    fn selective_import_resolves_defining_file() {
        let state = test_state();
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let tmp = std::env::temp_dir().join(format!("zz_lsp_sym_{}", std::process::id()));
        let lib = open(
            &state,
            Url::from_file_path(tmp.join("lib.zz")).unwrap().as_ref(),
            lib_src,
        );
        index_file(&state, &lib, "lib", lib_src);
        let main_src =
            "import lib(circle)\nfunc main() {\n    y := circle(1.0)\n    println(y)\n}\n";
        let main = open(
            &state,
            Url::from_file_path(tmp.join("main.zz")).unwrap().as_ref(),
            main_src,
        );

        let scope =
            resolve_symbol_scope(&state, &main, offset_of(main_src, "circle(1.0)")).expect("scope");
        assert_eq!(scope.name, "circle");
        assert_eq!(scope.defining_uri, lib);
        let uris: Vec<&Url> = scope.files.iter().map(|f| &f.uri).collect();
        assert!(uris.contains(&&lib), "defining file in scope");
        assert!(uris.contains(&&main), "current file in scope");
    }

    #[test]
    fn unrelated_same_name_file_excluded() {
        let state = test_state();
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let lib = open(&state, "file:///lib.zz", lib_src);
        index_file(&state, &lib, "lib", lib_src);
        let main_src =
            "import lib(circle)\nfunc main() {\n    y := circle(1.0)\n    println(y)\n}\n";
        let main = open(&state, "file:///main.zz", main_src);
        // Same name, no import: its own symbol, never ours.
        let other_src = "func circle() -> int {\n    return 1\n}\nc := circle()\n";
        let other = open(&state, "file:///other.zz", other_src);

        let scope =
            resolve_symbol_scope(&state, &main, offset_of(main_src, "circle(1.0)")).expect("scope");
        let uris: Vec<&Url> = scope.files.iter().map(|f| &f.uri).collect();
        assert!(!uris.contains(&&other), "unrelated file excluded");
    }

    #[test]
    fn local_symbol_stays_local() {
        let state = test_state();
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let lib = open(&state, "file:///lib.zz", lib_src);
        index_file(&state, &lib, "lib", lib_src);
        // No imports: `helper` is local to main.
        let main_src = "func helper(x: int) -> int {\n    return x\n}\nfunc main() {\n    y := helper(1)\n    println(y)\n}\n";
        let main = open(&state, "file:///main.zz", main_src);

        let scope =
            resolve_symbol_scope(&state, &main, offset_of(main_src, "helper(1)")).expect("scope");
        assert_eq!(scope.defining_uri, main);
        assert_eq!(scope.files.len(), 1, "only the current file");
    }

    #[test]
    fn top_level_binding_is_project_global() {
        // `shared := 1` rides the global seed: files without imports
        // still observe it (funcs/structs stay import-gated).
        let state = test_state();
        let a_src = "shared_val := 42\n";
        let a = open(&state, "file:///a.zz", a_src);
        let b_src = "x := shared_val\n";
        let b = open(&state, "file:///b.zz", b_src);

        let scope =
            resolve_symbol_scope(&state, &a, offset_of(a_src, "shared_val")).expect("scope");
        assert!(scope.global_binding, "top-level let is global");
        let uris: Vec<&Url> = scope.files.iter().map(|f| &f.uri).collect();
        assert!(uris.contains(&&b), "import-less file stays in scope");
    }

    #[test]
    fn rename_span_trims_qualifier() {
        let src = "y := lib.circle(1.0)";
        let whole = Span::new(5, 15);
        assert_eq!(src.get(5..15), Some("lib.circle"));
        let trimmed = rename_span_for(src, whole, "circle");
        assert_eq!((trimmed.start, trimmed.end), (9, 15));
        assert_eq!(
            src.get(trimmed.start as usize..trimmed.end as usize),
            Some("circle")
        );

        let bare = Span::new(0, 6);
        assert_eq!(rename_span_for("circle", bare, "circle"), bare);
    }

    #[test]
    fn foreign_definition_hits_dropped() {
        let state = test_state();
        let lib = open(
            &state,
            "file:///lib.zz",
            "pub func circle() -> int {\n    return 1\n}\n",
        );
        let scope = SymbolScope {
            name: "circle".to_string(),
            defining_uri: lib.clone(),
            field_target: None,
            field_only_qualified: false,
            global_binding: false,
            files: vec![],
        };
        let other_src = "func circle() -> int {\n    return 1\n}\n";
        let def_hit = lookup::Reference {
            span: Span::new(4, 10),
            is_definition: true,
        };
        assert!(!hit_counts_in_file(
            &scope,
            &"file:///other.zz".parse().unwrap(),
            &def_hit,
            other_src
        ));
        // Same call inside the defining file counts.
        assert!(hit_counts_in_file(&scope, &lib, &def_hit, other_src));
    }
}
